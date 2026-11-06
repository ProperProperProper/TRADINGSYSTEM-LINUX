//! Out-of-sample study on the candles already in the bot's store (no new candle pulls, no orders).
//!
//! Rolling walk-forward, exactly as paper runs it: before each traded session the full search
//! picks settings on that interval's fixed window ENDING before the session (15m: the last 14
//! sessions for every interval, with only indicator warm-up candles before that window), champion/challenger against
//! the settings in use (`choose_live`, recorded best settings re-tested), and ONE continuous
//! portfolio trades the unseen session. Several seeds; every variant runs on the same data and seeds.
//! The one-session interval cannot roll (its 69 warm-up + 21 test sessions use the whole store), so
//! it is tested inside its final window like the service's walk-forward, with more unseen sessions.
//!
//!   cargo run --release --example oos_study -- <interval key> <variant[,variant..]> [seeds] [unseen sessions]
use anyhow::{Context, Result};
use moomoo_bot::engine::walkforward::{self, SearchSpace};
use moomoo_bot::engine::xs::{self, XsParams, XsPortfolio};
use moomoo_bot::engine::{scores, Interval, BAR_MS, DAY_BARS, REFERENCE};
use std::time::{Duration, Instant};

const RECORD_CANDIDATES: usize = 300;

#[path = "common/mod.rs"]
mod common;
use common::{big_market, fmt, load_data, sessions, Big};

/// A research variant: the search space, plus anything else it changes.
#[derive(Clone)]
struct Variant {
    name: String,
    space: SearchSpace,
}

/// Variant names are '+'-joined tokens, e.g. `keep+top13`.
fn variant(name: &str, interval: Interval) -> Result<Variant> {
    use std::sync::atomic::Ordering::Relaxed;
    let k = interval.bars();
    let mut space = walkforward::space_for(interval);
    for tok in name.split('+') {
        match tok {
            "baseline" => {}
            "keep" => xs::research::KEEP_ON_REBALANCE.store(true, Relaxed),
            "feegate" => xs::research::FEE_GATE.store(true, Relaxed),
            "top13" => space.top = (1, 3),
            "top15" => space.top = (1, 5),
            "top12" => space.top = (1, 2),
            // Hold up to 5 sessions (live: the selected latest-window setting).
            "hold5" => {
                space.hold_bars = (
                    walkforward::per_candle(DAY_BARS, k),
                    walkforward::per_candle(5 * DAY_BARS, k),
                )
            }
            // Hold 2-5 sessions.
            "hold25" => {
                space.hold_bars = (
                    walkforward::per_candle(2 * DAY_BARS, k),
                    walkforward::per_candle(5 * DAY_BARS, k),
                )
            }
            // Trend margin band from the median up (never below 0: no entries under the average).
            "tband50" => {
                space.margin_pctl = (50.0, 99.9);
                space.min_margin = Some(0.0);
            }
            "tband0" => {
                space.margin_pctl = (0.0, 99.9);
                space.min_margin = Some(0.0);
            }
            // Trend average over 1-5 sessions (live: 0.5-2.5).
            "tlong" => space.trend_bars = (DAY_BARS, 5 * DAY_BARS),
            "tlong10" => space.trend_bars = (2 * DAY_BARS, 10 * DAY_BARS),
            other => anyhow::bail!("unknown variant token {other}"),
        }
    }
    Ok(Variant {
        name: name.into(),
        space,
    })
}

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
struct RunResult {
    variant: String,
    seed: u64,
    ret: f64,
    ret_wo_best: f64,
    trades: usize,
    wins: usize,
    max_dd: f64,
    fees: f64,
    sessions: usize,
    sessions_with_params: usize,
    switches: usize,
    days: Vec<f64>,
    /// Settings in use per unseen session (short form), and the closed trades.
    used: Vec<String>,
    trade_list: Vec<String>,
}

/// Rolling walk-forward of `v` with `seed` over the last `unseen` sessions of `big`.
fn run(big: &Big, v: &Variant, seed: u64, unseen: usize, in_window: bool) -> Result<RunResult> {
    let m = &big.m;
    let i = big.interval;
    let n = m.ts.len();
    let win = i.test_candles() + i.warmup_candles();
    let warm = i.warmup_candles();
    let k0 = big.opens.len() - unseen;
    let start = common::capital();
    let mut pf = XsPortfolio::new(start);
    let mut current: Option<XsParams> = None;
    let mut record: Vec<(f64, XsParams)> = Vec::new();
    let mut res = RunResult {
        variant: v.name.clone(),
        seed,
        sessions: unseen,
        ..Default::default()
    };
    // The one-session interval: its final window (warm-up + test) is fixed and choices use the
    // sessions before each unseen one inside it.
    let fixed_lo = n - win;
    for k in k0..big.opens.len() {
        let a = big.opens[k];
        let b = big.opens.get(k + 1).copied().unwrap_or(n);
        let lo = if in_window {
            fixed_lo
        } else {
            a.checked_sub(win)
                .context("not enough history for the window")?
        };
        let sel = m.slice(lo..a);
        let sc = scores::compute(&sel, walkforward::UNIVERSE);
        let eq = pf.equity + pf.unrealized();
        let space = SearchSpace {
            warmup_bars: if i == Interval::M15 { 0 } else { warm },
            ..v.space
        };
        let recorded: Vec<XsParams> = record.iter().map(|r| r.1.clone()).collect();
        let rep = walkforward::search_random(
            &sel,
            &sc,
            eq,
            Instant::now() + Duration::from_secs(3600),
            seed.wrapping_mul(1_000_003).wrapping_add(k as u64),
            &recorded,
            space,
        )?;
        for r in &rep.top {
            if let Some(x) = record.iter_mut().find(|x| x.1 == r.params) {
                x.0 = x.0.max(r.robust_score);
            } else {
                record.push((r.robust_score, r.params.clone()));
            }
        }
        record.sort_by(|x, y| y.0.total_cmp(&x.0));
        record.truncate(RECORD_CANDIDATES);
        let (chosen, _choice) = walkforward::choose_live(
            &sel,
            &sc,
            eq,
            rep.params.as_ref(),
            current.as_ref(),
            space.warmup_bars..sel.ts.len(),
        );
        if chosen.is_some() && chosen != current {
            res.switches += 1;
        }
        if chosen.is_some() {
            current = chosen;
        }
        let before = pf.equity + pf.unrealized();
        res.used.push(current.as_ref().map_or("-".into(), |p| {
            format!(
                "{:?}{} top{} hold{} tp{:?}/{:?} stop{:?} trend{:?}@{:?}",
                p.signal,
                if p.flip { "+" } else { "-" },
                p.top,
                p.hold,
                p.risk.take_profit_pct,
                p.risk.atr_tp_mult,
                p.stop_pct,
                p.risk.trend_bars,
                p.risk.trend_margin_pct
            )
        }));
        if let Some(p) = current.as_ref() {
            res.sessions_with_params += 1;
            let tr = m.slice(lo..b);
            let sct = scores::compute(&tr, walkforward::UNIVERSE);
            for t in (a - lo)..(b - lo) {
                pf.step(&tr, &sct, t, p);
            }
            if k + 1 == big.opens.len() {
                let last = tr.ts.len() - 1;
                pf.close_all(&tr, tr.ts[last] + BAR_MS, "End of test");
            }
        }
        let after = pf.equity + pf.unrealized();
        res.days.push((after / before - 1.0) * 100.0);
    }
    pf.validate_state()?;
    anyhow::ensure!(
        pf.execution_error.is_none(),
        "execution error: {:?}",
        pf.execution_error
    );
    let mt = pf.metrics();
    res.ret = (pf.equity / start - 1.0) * 100.0;
    let best = pf.trades.iter().map(|t| t.pnl).fold(0.0_f64, f64::max);
    res.ret_wo_best = ((pf.equity - best) / start - 1.0) * 100.0;
    res.trades = mt.trades;
    res.wins = mt.wins;
    res.max_dd = mt.max_drawdown_pct;
    res.fees = pf.trades.iter().map(|t| t.fees).sum();
    res.trade_list = pf
        .trades
        .iter()
        .map(|t| {
            format!(
                "{} {} {:+.2} ({})",
                fmt(m.wall[m.ts.partition_point(|&x| x < t.entry_ts).min(n - 1)]),
                t.symbol,
                t.pnl,
                t.reason
            )
        })
        .collect();
    Ok(res)
}

/// Median of `v` (sorts it in place; NaN when empty).
fn med(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The aggregate table of JSON result lines (one per variant and seed), variants in first-seen order.
fn table(lines: &str) -> Result<()> {
    let mut by: Vec<(String, Vec<RunResult>)> = Vec::new();
    for l in lines.lines().filter(|l| l.starts_with('{')) {
        let r: RunResult = serde_json::from_str(l)?;
        match by.iter_mut().find(|x| x.0 == r.variant) {
            Some(x) => x.1.push(r),
            None => by.push((r.variant.clone(), vec![r])),
        }
    }
    println!("variant                 seeds  median ret  [min .. max]      w/o best  trades  win%   maxDD  fees USD  days+/-   switches  per-seed");
    for (name, mut rs) in by {
        rs.sort_by_key(|r| r.seed);
        let col = |f: &dyn Fn(&RunResult) -> f64| -> f64 {
            med(&mut rs.iter().map(f).collect::<Vec<_>>())
        };
        let rets: Vec<f64> = rs.iter().map(|r| r.ret).collect();
        let lo = rets.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = rets.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let per: Vec<String> = rets.iter().map(|r| format!("{r:+.2}")).collect();
        println!(
            "{:<23} {:>5}  {:>+9.2}%  [{:+.2} .. {:+.2}]  {:>+8.2}%  {:>6.0}  {:>4.0}  {:>5.1}%  {:>8.2}  {:>3.0}/{:<3.0}  {:>8.0}  {}",
            name, rs.len(), col(&|r| r.ret), lo, hi, col(&|r| r.ret_wo_best), col(&|r| r.trades as f64),
            col(&|r| if r.trades > 0 { r.wins as f64 / r.trades as f64 * 100.0 } else { 0.0 }),
            col(&|r| r.max_dd), col(&|r| r.fees),
            col(&|r| r.days.iter().filter(|x| **x > 0.0).count() as f64), col(&|r| r.days.iter().filter(|x| **x < 0.0).count() as f64),
            col(&|r| r.switches as f64), per.join(" ")
        );
    }
    Ok(())
}

/// Out-of-sample study: runs one variant on unseen sessions (`run`) or prints a summary table of collected results (`table`).
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("table") {
        let f = args.get(2).context("table <results file>")?;
        return table(&std::fs::read_to_string(f)?);
    }
    // run <interval> <variant> <seed> [unseen]
    let interval = Interval::from_key(args.get(2).map_or("15m", |s| s.as_str()))
        .context("interval key: 15m, 1h, 4h, 1d")?;
    let name = args.get(3).map_or("baseline", |s| s.as_str());
    let seed: u64 = args.get(4).map_or(Ok(1), |s| s.parse())?;
    let cal = sessions().await?;
    let data = load_data()?;
    let big = big_market(&cal, &data, interval)?;
    let win_sessions =
        interval.test_sessions() + interval.warmup_candles().div_ceil(interval.per_session());
    // Rolling needs a full window (warm-up + test) before each unseen session; without 10 such
    // sessions (one session on the store's candles alone) the test runs inside the final window.
    let rolling = big.opens.len().saturating_sub(win_sessions + 1);
    let in_window = rolling < 10;
    let max_unseen = if in_window {
        interval.test_sessions() - 8
    } else {
        rolling
    };
    let unseen: usize = args
        .get(5)
        .map_or(Ok(max_unseen), |s| s.parse())?
        .min(max_unseen);
    let r = big
        .m
        .symbols
        .iter()
        .position(|s| s == REFERENCE)
        .context("SPY")?;
    let a = big.opens[big.opens.len() - unseen];
    let qqq = (big.m.bars[r][big.m.ts.len() - 1].unwrap().close / big.m.bars[r][a].unwrap().open
        - 1.0)
        * 100.0;
    eprintln!(
        "{} | {} unseen sessions {} .. {} ({}) | SPY buy&hold {:+.2}%",
        interval.label(),
        unseen,
        fmt(big.m.wall[a]),
        fmt(big.m.wall[big.m.ts.len() - 1]),
        if in_window {
            "inside the final window"
        } else {
            "rolling window"
        },
        qqq
    );
    let v = variant(name, interval)?;
    let res = run(&big, &v, seed, unseen, in_window)?;
    println!("{}", serde_json::to_string(&res)?);
    Ok(())
}

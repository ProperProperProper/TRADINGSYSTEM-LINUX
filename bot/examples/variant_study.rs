//! Which search-space VARIANT wins out of sample? For every 14-session look-forward block the bot's own search
//! (with the variant's ranges: hold, basket size, trend band, signal direction, fee gate, keep-on-rebalance) picks
//! settings on the 14 sessions before; those settings, fixed, trade the next 14. Settings still come from the search;
//! only the ranges differ. Stored candles only.
//!
//!   STUDY_EQUITY=5000 cargo run --release --example variant_study -- <interval key> <variant,variant,...> [offsets=0,5,10]
use anyhow::{Context, Result};
use moomoo_bot::engine::live_options;
use moomoo_bot::engine::walkforward::{self, SearchSpace};
use moomoo_bot::engine::xs;
use moomoo_bot::engine::{scores, Interval, Market, DAY_BARS, REFERENCE};
use std::time::{Duration, Instant};

#[path = "common/mod.rs"]
mod common;
use common::{big_market, jitter, load_data, sessions};

/// Noise stress: price jitter per bar (open and close, ~10 bps) and costs x1.5, on two independent noisy tapes.
const NOISE_SIGMA: f64 = 0.0010;
const NOISE_COSTS: f64 = 1.5;
const NOISE_TAPES: u64 = 2;

const BLOCK: usize = 14;

/// One forward block's result: (return %, noisy-tape return %, closed trades, winning trades).
type BlockOut = (f64, f64, usize, usize);

/// How many of the most liquid names (trailing dollar volume, causal) may enter, from a `univ<N>` token (live: 20).
fn variant_universe(name: &str) -> usize {
    name.split('+')
        .find_map(|t| t.strip_prefix("univ").and_then(|n| n.parse().ok()))
        .unwrap_or(walkforward::UNIVERSE)
}

/// The search space of variant `name` (a '+'-joined list of tokens) on `interval`, and the research switches it turns on.
fn variant(name: &str, interval: Interval) -> Result<SearchSpace> {
    use std::sync::atomic::Ordering::Relaxed;
    xs::research::KEEP_ON_REBALANCE.store(false, Relaxed);
    xs::research::FEE_GATE.store(false, Relaxed);
    xs::research::FLAT_ENTER.store(false, Relaxed);
    xs::research::ML_SIGNAL.store(false, Relaxed);
    xs::research::ML_GATE_PCTL.store(-1, Relaxed);
    moomoo_bot::engine::rules::LIQUIDITY_MODEL.store(common::liquidity_default(), Relaxed);
    common::apply_order_cap();
    let k = interval.bars();
    // the current live strategy first (options promoted by the lab, engine/live_options.rs; STUDY_NO_LIVE=1 for the plain one),
    // so "baseline" is what trades now and every variant complements it
    let plain = live_options::LiveStrategy {
        options: &[],
        ..live_options::LIVE
    };
    let live = if std::env::var("STUDY_NO_LIVE").is_ok() {
        plain
    } else {
        live_options::LIVE
    };
    let (mut space, _) = live_options::space(interval, &live);
    for tok in name.split('+') {
        match tok {
            "baseline" => {}
            t if t.starts_with("univ") => {} // handled by `variant_universe`
            "keep" => xs::research::KEEP_ON_REBALANCE.store(true, Relaxed),
            "feegate" => xs::research::FEE_GATE.store(true, Relaxed),
            "hold5" => {
                space.hold_bars = (
                    walkforward::per_candle(DAY_BARS, k),
                    walkforward::per_candle(5 * DAY_BARS, k),
                )
            }
            "hold25" => {
                space.hold_bars = (
                    walkforward::per_candle(2 * DAY_BARS, k),
                    walkforward::per_candle(5 * DAY_BARS, k),
                )
            }
            "hold510" => {
                space.hold_bars = (
                    walkforward::per_candle(5 * DAY_BARS, k),
                    walkforward::per_candle(10 * DAY_BARS, k),
                )
            }
            "tband50" => {
                space.margin_pctl = (50.0, 99.9);
                space.min_margin = Some(0.0);
            }
            "tlong" => space.trend_bars = (DAY_BARS, 5 * DAY_BARS),
            // Entry options: the engine's shared mapping (the live strategy uses the same one: engine/live_options.rs)
            t if live_options::is_live_token(t) => live_options::apply_token(&mut space, t)?,
            "liq" => moomoo_bot::engine::rules::LIQUIDITY_MODEL.store(true, Relaxed),
            "noliq" => moomoo_bot::engine::rules::LIQUIDITY_MODEL.store(false, Relaxed),
            "flatenter" => xs::research::FLAT_ENTER.store(true, Relaxed),
            "sig:Return" => space.sig = Some("Return"),
            "sig:TrendScore" => space.sig = Some("TrendScore"),
            "sig:PriceAction" => space.sig = Some("PriceAction"),
            "sig:Volatility" => space.sig = Some("Volatility"),
            "sig:Rsi" => space.sig = Some("Rsi"),
            "sig:Pulse" => space.sig = Some("Pulse"),
            "sig:CalmDip" => space.sig = Some("CalmDip"),
            "sig:Ml" => {
                xs::research::ML_SIGNAL.store(true, Relaxed);
                space.sig = Some("Ml")
            }
            "mlgate" => {
                xs::research::ML_SCORES_ON.store(true, Relaxed);
                space.gates.ml_gate = true
            }
            t if t.starts_with("mlgate") => xs::research::ML_GATE_PCTL.store(
                t["mlgate".len()..]
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!("mlgate<percentile>"))?,
                Relaxed,
            ),
            "follow" => space.direction = Some(true),
            "contra" => space.direction = Some(false),
            other => anyhow::bail!("unknown variant token {other}"),
        }
    }
    Ok(space)
}

struct Block {
    sel: Market,
    sc: Vec<Vec<Option<scores::Score>>>,
    tr: Market,
    sct: Vec<Vec<Option<scores::Score>>>,
    ta: usize,
    tb: usize,
    qqq: f64,
    seed: u64,
    /// Which block alignment (offset index) it belongs to: the first offset's blocks are consecutive and do not overlap.
    chain: bool,
    /// The same forward block on noisy tapes (settings are still chosen on the clean tape).
    noisy: Vec<(Market, Vec<Vec<Option<scores::Score>>>)>,
}

/// Variant study: scores each variant on 14-session look-forward blocks (both halves, noisy tape, compounded `chained` return).
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let i = Interval::from_key(args.get(1).map_or("4h", |s| s.as_str())).context("interval key")?;
    let names: Vec<String> = args
        .get(2)
        .map_or("baseline", |s| s.as_str())
        .split(',')
        .map(String::from)
        .collect();
    let offsets: Vec<usize> = args
        .get(3)
        .map_or("0,5,10", |s| s.as_str())
        .split(',')
        .filter_map(|x| x.parse().ok())
        .collect();
    if names.iter().any(|n| {
        n.split('+')
            .any(|t| t == "sig:Ml" || t.starts_with("mlgate"))
    }) {
        xs::research::ML_SCORES_ON.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let equity: f64 = common::capital();
    let cal = sessions().await?;
    let data = load_data()?;
    let big = big_market(&cal, &data, i)?;
    let (m, n) = (&big.m, big.m.ts.len());
    let warm = i.warmup_candles();
    let win = BLOCK * i.per_session() + warm;
    let first = big
        .opens
        .iter()
        .position(|&a| a >= win)
        .context("not enough history")?;
    let qqq = m
        .symbols
        .iter()
        .position(|s| s == REFERENCE)
        .context("SPY")?;
    let mut blocks: Vec<Block> = Vec::new();
    for &offset in &offsets {
        let mut k = first + offset;
        while k + BLOCK <= big.opens.len() {
            let a = big.opens[k];
            let b = big.opens.get(k + BLOCK).copied().unwrap_or(n);
            let lo = a - win;
            let sel = m.slice(lo..a);
            let tr = m.slice(lo..b);
            let q = match (
                m.bars[qqq][a].map(|x| x.open),
                m.bars[qqq][b - 1].map(|x| x.close),
            ) {
                (Some(x), Some(y)) => (y / x - 1.0) * 100.0,
                _ => 0.0,
            };
            let seed = (k * 31 + offset) as u64;
            let noisy: Vec<_> = (0..NOISE_TAPES)
                .map(|j| {
                    let nm = jitter(&tr, NOISE_SIGMA, seed * 7 + j + 1);
                    let ns = scores::compute(&nm, walkforward::UNIVERSE);
                    (nm, ns)
                })
                .collect();
            blocks.push(Block {
                chain: offset == offsets[0],
                sc: scores::compute(&sel, walkforward::UNIVERSE),
                sct: scores::compute(&tr, walkforward::UNIVERSE),
                sel,
                tr,
                ta: a - lo,
                tb: b - lo,
                qqq: q,
                seed,
                noisy,
            });
            k += BLOCK;
        }
    }
    // chronological order: the first half of the blocks vs the second half (settings never see either)
    blocks.sort_by_key(|b| b.tr.wall[b.ta]);
    let mean_q = blocks.iter().map(|b| b.qqq).sum::<f64>() / blocks.len() as f64;
    println!(
        "{} start capital {equity}: {} blocks, SPY buy&hold mean {mean_q:+.2}%",
        i.label(),
        blocks.len()
    );
    println!(
        "{:<26} {:>8} {:>8} {:>7} {:>8} {:>9} {:>9} {:>8} {:>9} {:>9} {:>8} {:>9} {:>6} {:>9}",
        "variant",
        "mean %",
        "median %",
        "blocks+",
        "worst %",
        "1st half",
        "2nd half",
        "trades",
        "noisy 1st",
        "noisy 2nd",
        "noisy+",
        "chained",
        "win 1st",
        "win 2nd"
    );
    let live_now = if std::env::var("STUDY_NO_LIVE").is_ok() {
        Vec::new()
    } else {
        live_options::space(i, &live_options::LIVE).1
    };
    println!(
        "live {}",
        if live_now.is_empty() {
            "none".to_string()
        } else {
            live_now.join("+")
        }
    );
    let mut universe = walkforward::UNIVERSE;
    for name in &names {
        // The entry universe size is part of the scores, so a different `univ<N>` rescoring every block (and noisy tape).
        let want = variant_universe(name);
        if want != universe {
            for b in &mut blocks {
                b.sc = scores::compute(&b.sel, want);
                b.sct = scores::compute(&b.tr, want);
                for (nm, ns) in b.noisy.iter_mut() {
                    *ns = scores::compute(nm, want);
                }
            }
            universe = want;
        }
        let space = SearchSpace {
            warmup_bars: if i == Interval::M15 { 0 } else { warm },
            ..variant(name, i)?
        };
        let mut fwd: Vec<f64> = Vec::new();
        let mut nfwd: Vec<f64> = Vec::new();
        let mut chained = 1.0f64;
        let mut trades = 0usize;
        // closed trades and winning trades per forward block (win rate = share of closed trades with positive net PnL), reported
        // per half like the returns so the lab can show the first half to its model and judge the second
        let mut block_wins: Vec<(usize, usize)> = Vec::new();
        // Blocks are independent: they run on `STUDY_THREADS` threads (default: all cores), results kept in block order.
        let threads = std::env::var("STUDY_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<std::sync::Mutex<Option<Result<BlockOut>>>> = (0..blocks.len())
            .map(|_| std::sync::Mutex::new(None))
            .collect();
        std::thread::scope(|sc| {
            for _ in 0..threads.min(blocks.len()).max(1) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(b) = blocks.get(k) else { break };
                    let one = (|| -> Result<BlockOut> {
                        let rep = walkforward::search_random(
                            &b.sel,
                            &b.sc,
                            equity,
                            Instant::now() + Duration::from_secs(3600),
                            b.seed,
                            &[],
                            space,
                        )?;
                        Ok(match rep.params.as_ref() {
                            Some(p) => {
                                let pf = xs::backtest(&b.tr, &b.sct, b.ta..b.tb, p, equity);
                                // the same settings on noisy tapes with higher costs (average over the tapes)
                                let nr = b
                                    .noisy
                                    .iter()
                                    .map(|(nm, ns)| {
                                        xs::backtest_costs(
                                            nm,
                                            ns,
                                            b.ta..b.tb,
                                            p,
                                            equity,
                                            NOISE_COSTS,
                                        )
                                        .metrics()
                                        .return_pct()
                                    })
                                    .sum::<f64>()
                                    / b.noisy.len() as f64;
                                (
                                    pf.metrics().return_pct(),
                                    nr,
                                    pf.metrics().trades,
                                    pf.metrics().wins,
                                )
                            }
                            None => (0.0, 0.0, 0, 0), // no safe settings found: stay in cash
                        })
                    })();
                    *results[k].lock().unwrap() = Some(one);
                });
            }
        });
        for (b, slot) in blocks.iter().zip(results) {
            let (r, nr, n, w) = slot.into_inner().unwrap().expect("every block ran")?;
            trades += n;
            block_wins.push((n, w));
            fwd.push(r);
            nfwd.push(nr);
            if b.chain {
                chained *= 1.0 + r / 100.0;
            }
        }
        let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
        let mut s = fwd.clone();
        s.sort_by(f64::total_cmp);
        let h = fwd.len() / 2;
        let win = |w: &[(usize, usize)]| {
            let (n, k) = w.iter().fold((0, 0), |a, x| (a.0 + x.0, a.1 + x.1));
            if n > 0 {
                k as f64 / n as f64 * 100.0
            } else {
                0.0
            }
        };
        println!(
            "{:<26} {:>+8.2} {:>+8.2} {:>6.0}% {:>+8.2} {:>+9.2} {:>+9.2} {:>8.1} {:>+9.2} {:>+9.2} {:>7.0}% {:>+8.1}% {:>5.0}% {:>8.0}%",
            name, mean(&fwd), s[s.len() / 2], fwd.iter().filter(|x| **x > 0.0).count() as f64 / fwd.len() as f64 * 100.0, s[0], mean(&fwd[..h]), mean(&fwd[h..]), trades as f64 / fwd.len() as f64,
            mean(&nfwd[..h]), mean(&nfwd[h..]), nfwd.iter().filter(|x| **x > 0.0).count() as f64 / nfwd.len() as f64 * 100.0, (chained - 1.0) * 100.0,
            win(&block_wins[..h]), win(&block_wins[h..])
        );
        // every block's result for charts (the lab stores them; parsers that expect the table skip this short line):
        // blocks <variant> <start unix s>/<return %>/<trades>/<wins>/<chain>;... in block order (first half = training, second =
        // holdout); chain = 1 for the first offset's consecutive, non-overlapping blocks (the only ones that may be compounded)
        let per: Vec<String> = blocks
            .iter()
            .zip(fwd.iter())
            .zip(block_wins.iter())
            .map(|((b, r), (n, w))| {
                format!(
                    "{}/{:.3}/{}/{}/{}",
                    b.tr.wall[b.ta] / 1000,
                    r,
                    n,
                    w,
                    u8::from(b.chain)
                )
            })
            .collect();
        println!("blocks {name} {}", per.join(";"));
    }
    Ok(())
}

//! Does ANY way of picking settings from the backtest carry forward? For every 14-session look-forward block
//! (search window = the 14 sessions before, forward = the next 14, as in lab_export), draw N random settings,
//! backtest each in-sample and forward, then score selection RULES by the forward return of the setting they pick.
//! Also reports the rank correlation between in-sample and forward return across the draws, and the bot's own
//! search winner on the same blocks. Stored candles only; no orders.
//!
//!   cargo run --release --example selection_study -- <interval key> [draws=400] [offsets=0,5,10]
use anyhow::{Context, Result};
use moomoo_bot::engine::walkforward::{self, MarginBand, SearchSpace};
use moomoo_bot::engine::xs::{self, XsParams};
use moomoo_bot::engine::{scores, Interval, REFERENCE};
use std::time::{Duration, Instant};

#[path = "common/mod.rs"]
mod common;
use common::{big_market, load_data, sessions};

const BLOCK: usize = 14;

#[derive(Clone, Copy)]
struct M {
    ret: f64,
    dd: f64,
    trades: usize,
    pf: f64,
    h1: f64,
    h2: f64,
    fwd: f64,
}

/// Ranks of `v` (0 = smallest), for rank correlation.
fn rank(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut r = vec![0.0; v.len()];
    for (k, &i) in idx.iter().enumerate() {
        r[i] = k as f64;
    }
    r
}

/// Spearman rank correlation between `a` and `b` (0 when either has no spread).
fn spearman(a: &[f64], b: &[f64]) -> f64 {
    let (ra, rb) = (rank(a), rank(b));
    let n = a.len() as f64;
    let (ma, mb) = (ra.iter().sum::<f64>() / n, rb.iter().sum::<f64>() / n);
    let (mut cov, mut va, mut vb) = (0.0, 0.0, 0.0);
    for i in 0..a.len() {
        cov += (ra[i] - ma) * (rb[i] - mb);
        va += (ra[i] - ma).powi(2);
        vb += (rb[i] - mb).powi(2);
    }
    if va > 0.0 && vb > 0.0 {
        cov / (va * vb).sqrt()
    } else {
        0.0
    }
}

/// The forward return of the candidate with the highest `score` (None when no candidate has a score).
fn pick<F: Fn(&M) -> Option<f64>>(ms: &[M], score: F) -> Option<f64> {
    ms.iter()
        .filter_map(|m| score(m).map(|s| (s, m.fwd)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| x.1)
}

/// Selection study: tests whether any in-sample selection rule predicts the next 14 sessions' return, on look-forward blocks.
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let i =
        Interval::from_key(args.get(1).map_or("15m", |s| s.as_str())).context("interval key")?;
    let draws: usize = args.get(2).map_or(Ok(400), |s| s.parse())?;
    let offsets: Vec<usize> = args
        .get(3)
        .map_or("0,5,10", |s| s.as_str())
        .split(',')
        .filter_map(|x| x.parse().ok())
        .collect();
    // STUDY_EQUITY (start capital, default START_CAPITAL) and STUDY_COST (fee/slippage multiplier, default 1) separate the signal from the costs.
    let equity: f64 = common::capital();
    let cost: f64 = std::env::var("STUDY_COST")
        .ok()
        .and_then(|x| x.parse().ok())
        .unwrap_or(1.0);
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
    let space = SearchSpace {
        warmup_bars: if i == Interval::M15 { 0 } else { warm },
        ..walkforward::space_for(i)
    };
    let qqq = m
        .symbols
        .iter()
        .position(|s| s == REFERENCE)
        .context("SPY")?;
    // rule name -> forward returns of its picks, one per block
    let names = [
        "random pick (all draws)",
        "bot's search winner",
        "max in-sample return",
        "max return, >=5 trades",
        "max profit factor, >=8 trades",
        "max return/drawdown, >=5 trades",
        "positive in BOTH halves, max min-half",
        "median of top-5% by return",
        "cash (no trading)",
    ];
    let mut picks: Vec<Vec<f64>> = vec![Vec::new(); names.len()];
    let (mut ics, mut qqq_fwd) = (Vec::new(), Vec::new());
    for &offset in &offsets {
        let mut k = first + offset;
        while k + BLOCK <= big.opens.len() {
            let a = big.opens[k];
            let b = big.opens.get(k + BLOCK).copied().unwrap_or(n);
            let lo = a - win;
            let sel = m.slice(lo..a);
            let sc = scores::compute(&sel, walkforward::UNIVERSE);
            let tr = m.slice(lo..b);
            let sct = scores::compute(&tr, walkforward::UNIVERSE);
            let (ta, tb) = (a - lo, b - lo);
            let w0 = space.warmup_bars;
            let mid = w0 + (sel.ts.len() - w0) / 2;
            let seed = (k * 31 + offset) as u64;
            let band = MarginBand::new(&sel)?;
            let ps: Vec<XsParams> =
                walkforward::sample_space(seed ^ 0x9E37_79B9, draws, &band, space);
            let ms: Vec<M> = ps
                .iter()
                .map(|p| {
                    let ins = xs::backtest_costs(&sel, &sc, w0..sel.ts.len(), p, equity, cost);
                    let mt = ins.metrics();
                    let h1 = xs::backtest_costs(&sel, &sc, w0..mid, p, equity, cost)
                        .metrics()
                        .return_pct();
                    let h2 = xs::backtest_costs(&sel, &sc, mid..sel.ts.len(), p, equity, cost)
                        .metrics()
                        .return_pct();
                    let fwd = xs::backtest_costs(&tr, &sct, ta..tb, p, equity, cost)
                        .metrics()
                        .return_pct();
                    M {
                        ret: mt.return_pct(),
                        dd: mt.max_drawdown_pct,
                        trades: mt.trades,
                        pf: mt.profit_factor(),
                        h1,
                        h2,
                        fwd,
                    }
                })
                .collect();
            let fw: Vec<f64> = ms.iter().map(|x| x.fwd).collect();
            let ins: Vec<f64> = ms.iter().map(|x| x.ret).collect();
            ics.push(spearman(&ins, &fw));
            picks[0].push(fw.iter().sum::<f64>() / fw.len() as f64);
            // the bot's own search on the same window
            let rep = walkforward::search_random(
                &sel,
                &sc,
                equity,
                Instant::now() + Duration::from_secs(3600),
                seed,
                &[],
                space,
            )?;
            picks[1].push(rep.params.as_ref().map_or(0.0, |p| {
                xs::backtest_costs(&tr, &sct, ta..tb, p, equity, cost)
                    .metrics()
                    .return_pct()
            }));
            picks[2].push(pick(&ms, |x| Some(x.ret)).unwrap_or(0.0));
            picks[3].push(pick(&ms, |x| (x.trades >= 5).then_some(x.ret)).unwrap_or(0.0));
            picks[4].push(
                pick(&ms, |x| (x.trades >= 8 && x.pf.is_finite()).then_some(x.pf)).unwrap_or(0.0),
            );
            picks[5]
                .push(pick(&ms, |x| (x.trades >= 5).then(|| x.ret / x.dd.max(0.5))).unwrap_or(0.0));
            picks[6].push(
                pick(&ms, |x| (x.h1 > 0.0 && x.h2 > 0.0).then(|| x.h1.min(x.h2))).unwrap_or(0.0),
            );
            let mut sorted: Vec<&M> = ms.iter().collect();
            sorted.sort_by(|x, y| y.ret.total_cmp(&x.ret));
            let top: Vec<f64> = sorted
                .iter()
                .take((ms.len() / 20).max(1))
                .map(|x| x.fwd)
                .collect();
            let mut t = top.clone();
            t.sort_by(f64::total_cmp);
            picks[7].push(t[t.len() / 2]);
            picks[8].push(0.0);
            if let (Some(x), Some(y)) = (
                m.bars[qqq][ta + lo].map(|b| b.open),
                m.bars[qqq][(tb - 1) + lo].map(|b| b.close),
            ) {
                qqq_fwd.push((y / x - 1.0) * 100.0);
            }
            eprintln!("  {} offset {offset} block {k}: IC {:+.3}, all-draws fwd {:+.2}%, top-return pick {:+.2}%", i.label(), ics[ics.len() - 1], picks[0][picks[0].len() - 1], picks[2][picks[2].len() - 1]);
            k += BLOCK;
        }
    }
    let nb = ics.len();
    println!("\n{}: {nb} look-forward blocks of {BLOCK} sessions, {draws} random settings each, start capital {equity}, costs x{cost}", i.label());
    println!(
        "rank correlation, in-sample return vs forward return (per block, averaged): {:+.3}",
        ics.iter().sum::<f64>() / nb as f64
    );
    println!(
        "{:<40} {:>10} {:>10} {:>10}",
        "selection rule", "mean fwd %", "blocks +", "worst %"
    );
    for (nm, v) in names.iter().zip(&picks) {
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        println!(
            "{:<40} {:>+10.2} {:>9.0}% {:>+10.2}",
            nm,
            mean,
            v.iter().filter(|x| **x > 0.0).count() as f64 / v.len() as f64 * 100.0,
            v.iter().cloned().fold(f64::INFINITY, f64::min)
        );
    }
    println!(
        "{:<40} {:>+10.2}",
        format!("{REFERENCE} buy&hold (reference)"),
        qqq_fwd.iter().sum::<f64>() / qqq_fwd.len().max(1) as f64
    );
    Ok(())
}

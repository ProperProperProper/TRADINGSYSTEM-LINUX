//! Walk-forward validation, repeated over several folds, on real data only. For every fold a setting is chosen using ONLY candles before the fold's test
//! block (the search, and the selection among its winners, never see the block) and is then run on the block, from flat. The out-of-sample blocks are
//! stitched into one return. Two ways of choosing are compared on exactly the same folds: the bot's own robust winner, and the winner that holds up best in
//! both halves of the training window. Nothing is tuned on a test block, and no score or indicator uses a price after its own candle. Buy-and-hold of the
//! reference symbol over the same blocks is the yardstick.
use anyhow::Result;
use moomoo_bot::engine::Interval;
use moomoo_bot::engine::scores;
use moomoo_bot::engine::walkforward::{self, SearchSpace};
use moomoo_bot::engine::xs::XsParams;
use rusqlite::Connection;
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::market::Built;
use crate::search::{PeriodMetrics, metrics_of, par_map};

/// The search space for one candle size: hold at most `hold_minutes` (whole candles of that size, at least one), up to `top` positions at once, trend filter on or
/// off. `None` keeps the live holding range (1-2 sessions). Everything else is the engine's live space.
pub fn space_for(
    interval: Interval,
    samples: usize,
    min_trades: usize,
    hold_minutes: Option<usize>,
    top: usize,
    trend_filter: bool,
) -> SearchSpace {
    let live = walkforward::space_for(interval);
    let hold_bars =
        hold_minutes.map_or(live.hold_bars, |m| (1, (m / (interval.bars() * 15)).max(1)));
    SearchSpace {
        samples,
        min_trades,
        hold_bars,
        top: (1, top.max(1)),
        trend_filter,
        ..walkforward::space_for(interval)
    }
}

/// How many of the search's best winners the stable-halves rule re-tests.
pub const STABLE_CANDIDATES: usize = 2000;

/// One fold: the candles the setting may be chosen from, and the block it is judged on right after.
#[derive(Debug, Clone, PartialEq)]
pub struct Fold {
    pub train: Range<usize>,
    pub test: Range<usize>,
}

/// Expanding-window folds over the scored span: the first training window is `min_train` sessions, every fold adds `block` sessions of training and tests the next
/// `block` sessions. `first` is the first scored candle, `total` the number of candles, `cps` candles per session.
pub fn folds(first: usize, total: usize, cps: usize, min_train: usize, block: usize) -> Vec<Fold> {
    if cps == 0 || min_train == 0 || block == 0 {
        return Vec::new();
    }
    let first = first.div_ceil(cps) * cps;
    let mut out = Vec::new();
    let mut k = 0;
    while first + (min_train + (k + 1) * block) * cps <= total {
        let train_end = first + (min_train + k * block) * cps;
        out.push(Fold {
            train: first..train_end,
            test: train_end..train_end + block * cps,
        });
        k += 1;
    }
    out
}

/// How the setting of a fold is chosen from its training window alone.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    /// The bot's own robust winner of the search.
    Robust,
    /// Among the search's usable winners, the one whose worse half of the training window is best (usable in both halves).
    StableHalves,
    /// The bot's robust winner of a search on ONLY the most recent chunk-sized window before the block: signals are tied to the regime they were found in,
    /// so this forgets older history.
    Rolling,
}

impl Rule {
    pub fn name(self) -> &'static str {
        match self {
            Rule::Robust => "robust",
            Rule::StableHalves => "stable-halves",
            Rule::Rolling => "rolling",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FoldResult {
    pub rule: Rule,
    pub fold: usize,
    pub train: Range<usize>,
    pub test: Range<usize>,
    /// None when the search found no usable setting: the fold stays in cash.
    pub params: Option<XsParams>,
    pub test_metrics: Option<PeriodMetrics>,
    pub reference_return_pct: Option<f64>,
}

impl FoldResult {
    pub fn return_pct(&self) -> f64 {
        self.test_metrics.map_or(0.0, |m| m.return_pct)
    }
}

/// Compounds the out-of-sample blocks into one percentage.
pub fn stitched(returns_pct: impl IntoIterator<Item = f64>) -> f64 {
    (returns_pct
        .into_iter()
        .fold(1.0, |acc, r| acc * (1.0 + r / 100.0))
        - 1.0)
        * 100.0
}

/// Picks the setting among `winners` that is usable in both halves of `train` and best in its worse half.
fn candles_per_session(b: &Built, at: usize) -> usize {
    let Some(&wall) = b.market.wall.get(at) else {
        return 1;
    };
    b.sessions
        .list()
        .iter()
        .find(|&&(open, close)| wall >= open && wall < close)
        .map(|&(open, close)| {
            b.market
                .wall
                .iter()
                .filter(|&&w| w >= open && w < close)
                .count()
                .max(1)
        })
        .unwrap_or(1)
}

fn stable_pick(
    b: &Built,
    sc: &[Vec<Option<scores::Score>>],
    winners: &[XsParams],
    train: &Range<usize>,
    equity: f64,
    min_trades: usize,
    cps: usize,
) -> Option<XsParams> {
    let cps = cps.max(1);
    let mid = train.start + (train.len() / cps) / 2 * cps;
    let halves = [train.start..mid, mid..train.end];
    // a window this short can only trade a few times: half of the usual floor
    let floor = min_trades.div_ceil(2).max(1);
    let scored: Vec<(f64, usize)> = par_map(winners, |_, p| {
        let ms: Vec<PeriodMetrics> = halves
            .iter()
            .map(|h| metrics_of(b, sc, p, h.clone(), equity, floor))
            .collect();
        (
            if ms.iter().all(|m| m.usable) {
                ms.iter()
                    .map(PeriodMetrics::objective)
                    .fold(f64::INFINITY, f64::min)
            } else {
                f64::NEG_INFINITY
            },
            0,
        )
    });
    let (best, _) = scored
        .iter()
        .enumerate()
        .max_by(|a, c| a.1.0.total_cmp(&c.1.0))
        .map(|(i, s)| (i, s.0))?;
    (scored[best].0 > f64::NEG_INFINITY).then(|| winners[best].clone())
}

/// Runs every fold with all three rules. `samples` random settings are drawn per fold (the bot's live search draws 200,000).
#[allow(clippy::too_many_arguments)]
pub fn run(
    b: &Built,
    sc: &[Vec<Option<scores::Score>>],
    folds: &[Fold],
    space: SearchSpace,
    equity: f64,
    seed: u64,
) -> Result<Vec<FoldResult>> {
    run_rules(
        b,
        sc,
        folds,
        space,
        equity,
        seed,
        &[Rule::Robust, Rule::StableHalves, Rule::Rolling],
    )
}

/// The same for the given selection rules only (a search is run only for what a rule needs).
pub fn run_rules(
    b: &Built,
    sc: &[Vec<Option<scores::Score>>],
    folds: &[Fold],
    space: SearchSpace,
    equity: f64,
    seed: u64,
    rules: &[Rule],
) -> Result<Vec<FoldResult>> {
    let min_trades = space.min_trades;
    // the rolling rule looks back as far as the first (shortest) training window
    let roll = folds.first().map_or(0, |f| f.train.len());
    walkforward::KEEP_ALL_USABLE.store(true, std::sync::atomic::Ordering::Relaxed);
    moomoo_bot::engine::governor::UNTHROTTLED.store(true, std::sync::atomic::Ordering::Relaxed);
    let spy = b.market.symbols.iter().position(|s| s == "SPY");
    // the searches see the training slice only
    let searched: Vec<Result<walkforward::SearchReport>> = par_map(folds, |k, f| {
        let m = b.market.slice(f.train.clone());
        let rows: Vec<Vec<Option<scores::Score>>> =
            sc.iter().map(|r| r[f.train.clone()].to_vec()).collect();
        walkforward::search_random(
            &m,
            &rows,
            equity,
            Instant::now() + Duration::from_secs(3600),
            seed + k as u64,
            &[],
            space,
        )
    });
    let want_rolling = rules.contains(&Rule::Rolling);
    let rolled: Vec<Result<walkforward::SearchReport>> =
        par_map(if want_rolling { folds } else { &[] }, |k, f| {
            let window = f.train.end.saturating_sub(roll).max(f.train.start)..f.train.end;
            let m = b.market.slice(window.clone());
            let rows: Vec<Vec<Option<scores::Score>>> =
                sc.iter().map(|r| r[window.clone()].to_vec()).collect();
            walkforward::search_random(
                &m,
                &rows,
                equity,
                Instant::now() + Duration::from_secs(3600),
                seed + 1000 + k as u64,
                &[],
                space,
            )
        });
    let mut out = Vec::new();
    let mut rolled = rolled.into_iter();
    for (k, (f, rep)) in folds.iter().zip(searched).enumerate() {
        let rep = rep?;
        let rolling = if want_rolling {
            Some(rolled.next().expect("one rolling search per fold")?)
        } else {
            None
        };
        // with a low trade floor almost every setting is usable: only the best-scoring ones (by the search's own robust score, a training-window number) are re-tested
        let mut ranked: Vec<&_> = rep.usable_all.iter().collect();
        ranked.sort_by(|a, c| c.robust_score.total_cmp(&a.robust_score));
        let winners: Vec<XsParams> = ranked
            .iter()
            .take(STABLE_CANDIDATES)
            .map(|w| w.params.clone())
            .collect();
        let reference = spy.and_then(|s| {
            Some(
                (b.market.bars[s][f.test.end - 1]?.close / b.market.bars[s][f.test.start]?.open
                    - 1.0)
                    * 100.0,
            )
        });
        for &rule in rules {
            let params = match rule {
                Rule::Robust => rep.params.clone(),
                Rule::Rolling => rolling.as_ref().and_then(|r| r.params.clone()),
                Rule::StableHalves => stable_pick(
                    b,
                    sc,
                    &winners,
                    &f.train,
                    equity,
                    min_trades,
                    candles_per_session(b, f.train.start),
                )
                .or_else(|| rep.params.clone()),
            };
            let test_metrics = params
                .as_ref()
                .map(|p| metrics_of(b, sc, p, f.test.clone(), equity, 0));
            out.push(FoldResult {
                rule,
                fold: k,
                train: f.train.clone(),
                test: f.test.clone(),
                params,
                test_metrics,
                reference_return_pct: reference,
            });
        }
    }
    Ok(out)
}

/// Walk-forward where each fold also chooses its UNIVERSE (which symbols the strategy may buy) from its training window only, then searches a setting on
/// that pool (scores recomputed for it) and runs the setting on the pool over the unseen block. Rule: the bot's robust winner.
pub fn run_universe(
    b: &Built,
    folds: &[Fold],
    space: SearchSpace,
    equity: f64,
    seed: u64,
    pick: crate::universe::Pick,
) -> Result<(Vec<FoldResult>, Vec<usize>)> {
    walkforward::KEEP_ALL_USABLE.store(true, std::sync::atomic::Ordering::Relaxed);
    moomoo_bot::engine::governor::UNTHROTTLED.store(true, std::sync::atomic::Ordering::Relaxed);
    let done: Vec<Result<(FoldResult, usize)>> = par_map(folds, |k, f| {
        let keep = crate::universe::choose(&b.market, f.train.clone(), pick);
        let sub = crate::universe::subset(b, &keep);
        let sc = scores::compute(&sub.market, sub.market.symbols.len());
        let m = sub.market.slice(f.train.clone());
        let rows: Vec<Vec<Option<scores::Score>>> =
            sc.iter().map(|r| r[f.train.clone()].to_vec()).collect();
        let rep = walkforward::search_random(
            &m,
            &rows,
            equity,
            Instant::now() + Duration::from_secs(3600),
            seed + k as u64,
            &[],
            space,
        )?;
        let spy = sub.market.symbols.iter().position(|s| s == "SPY");
        let reference = spy.and_then(|s| {
            Some(
                (sub.market.bars[s][f.test.end - 1]?.close
                    / sub.market.bars[s][f.test.start]?.open
                    - 1.0)
                    * 100.0,
            )
        });
        let params = rep.params.clone();
        let test_metrics = params
            .as_ref()
            .map(|p| metrics_of(&sub, &sc, p, f.test.clone(), equity, 0));
        Ok((
            FoldResult {
                rule: Rule::Robust,
                fold: k,
                train: f.train.clone(),
                test: f.test.clone(),
                params,
                test_metrics,
                reference_return_pct: reference,
            },
            keep.len(),
        ))
    });
    let mut out = Vec::new();
    let mut sizes = Vec::new();
    for d in done {
        let (r, n) = d?;
        out.push(r);
        sizes.push(n);
    }
    Ok((out, sizes))
}

/// Stores one row per fold and rule (`layout` also names the candle size, for example "1h 8x3").
#[allow(clippy::too_many_arguments)]
pub fn record(
    conn: &Connection,
    run_ms: i64,
    layout: &str,
    b: &Built,
    results: &[FoldResult],
    samples: usize,
    min_trades: usize,
    seed: u64,
) -> Result<()> {
    for r in results {
        let json = r.params.as_ref().map(serde_json::to_string).transpose()?;
        conn.execute(
            "INSERT INTO walkforward_runs(run_ms, layout, rule, fold, train_from_ms, train_to_ms, test_from_ms, test_to_ms, return_pct, max_drawdown_pct, trades, wins, ref_return_pct, params_json, min_trades, samples, seed) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            rusqlite::params![
                run_ms,
                layout,
                r.rule.name(),
                r.fold as i64,
                b.market.wall[r.train.start],
                b.market.wall[r.train.end - 1],
                b.market.wall[r.test.start],
                b.market.wall[r.test.end - 1],
                r.return_pct(),
                r.test_metrics.map_or(0.0, |m| m.max_drawdown_pct),
                r.test_metrics.map_or(0, |m| m.trades) as i64,
                r.test_metrics.map_or(0, |m| m.wins) as i64,
                r.reference_return_pct,
                json,
                min_trades as i64,
                samples as i64,
                seed as i64
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_expand_the_training_window_and_each_test_block_starts_where_its_training_ends() {
        // 20 scored sessions of 2 candles from candle 40: train 8, test blocks of 3
        let f = folds(40, 80, 2, 8, 3);
        assert_eq!(f.len(), 4);
        assert_eq!(
            f[0],
            Fold {
                train: 40..56,
                test: 56..62
            }
        );
        assert_eq!(f[3].test, 74..80);
        for w in f.windows(2) {
            assert_eq!(
                w[1].train.end, w[0].test.end,
                "the next fold trains on everything up to the end of the previous test block"
            );
            assert_eq!(w[1].train.start, w[0].train.start);
        }
        for fold in &f {
            assert_eq!(
                fold.train.end, fold.test.start,
                "no gap and no overlap: the test block never touches its training window"
            );
            assert!(fold.test.end <= 80);
        }
        assert!(folds(40, 50, 2, 8, 3).is_empty(), "too short for one fold");
        assert!(folds(0, 10, 0, 1, 1).is_empty());
        assert!(folds(0, 10, 1, 0, 1).is_empty());
        assert_eq!(
            folds(41, 80, 2, 8, 3)[0].train.start,
            42,
            "the first scored candle is rounded up to a session boundary"
        );
    }

    #[test]
    fn stitched_compounds_the_blocks() {
        assert!((stitched([10.0, -10.0]) - (-1.0)).abs() < 1e-9);
        assert!((stitched([]) - 0.0).abs() < 1e-12);
        assert!((stitched([5.0, 5.0]) - 10.25).abs() < 1e-9);
    }

    #[test]
    fn the_hold_limit_is_whole_candles_within_the_minutes_given() {
        use moomoo_bot::engine::Interval::*;
        let hold = |i, m| space_for(i, 100, 1, Some(m), 5, true).hold_bars;
        assert_eq!(
            hold(H1, 255),
            (1, 4),
            "4h15 on 1-hour candles: at most 4 candles"
        );
        assert_eq!(hold(M1, 255), (1, 17));
        assert_eq!(hold(M30, 240), (1, 8));
        assert_eq!(hold(M90, 255), (1, 2));
        assert_eq!(hold(H3, 255), (1, 1));
        let s = space_for(M30, 100, 1, Some(255), 5, false);
        assert_eq!(
            space_for(H5, 100, 1, None, 1, true).hold_bars,
            walkforward::space_for(H5).hold_bars,
            "live holding range"
        );
        assert_eq!(
            (s.top, s.trend_filter, s.min_trades, s.samples),
            ((1, 5), false, 1, 100)
        );
    }
}

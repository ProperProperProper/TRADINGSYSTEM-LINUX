//! History export for the research lab (the research lab): rolling walk-forward backtests over every stored
//! session, written to `backtest_archive.sqlite3` with each trade's entry-state features. No new
//! candle pulls, no orders; the live service's rows are left alone (runs are keyed by settings,
//! source, window and start, so re-running adds only what is new).
//!
//! Training data is cut into 14-session look-forward blocks (never one parameter set over a longer window, as the
//! live windows of the longer intervals use). For every block, with settings chosen on the 14 sessions before it:
//! - `oos`: the full search picks settings on the window ENDING before the block, champion/
//!   challenger against the previous block's settings, and those settings, held fixed, trade the
//!   block's 14 sessions from the run's start capital (`LAB_CAPITALS`) (unselected: kept whatever the result);
//! - `random`: RANDOM settings drawn from the same space trade the same block from the same start capital (the unselected baseline that measures the selection effect);
//! - `chosen`: the search winner on its own selection window (in-sample; trades kept only when it
//!   did well, the selection-biased population).
//!
//! Caveat, labelled in the cost-model string: the universe is TODAY's pool of liquid stocks applied
//! to past sessions, so it carries survivorship/selection bias the live runs do not have.
//!
//!   cargo run --release --example lab_export -- <interval key|all> [seeds=1] [random per session=8] [random-only]
//!
//! `random-only` skips the searches and only adds more unselected random-settings baselines (the lab
//! needs enough unique entries per week; this strategy trades rarely).
use anyhow::{Context, Result};
use moomoo_bot::engine::btarchive::{entry_features, params_value, wall_trade, BtArchive, BtRun};
use moomoo_bot::engine::metrics::Trade;
use moomoo_bot::engine::walkforward::{self, MarginBand, SearchSpace};
use moomoo_bot::engine::xs::{self, XsParams};
use moomoo_bot::engine::{scores, Interval, Market};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[path = "common/mod.rs"]
mod common;
use common::{big_market, fmt, load_data, sessions, Big};

const HISTORY_ENGINE: &str = concat!("moomoo-bot-history-", env!("CARGO_PKG_VERSION"));
const HISTORY_COST_MODEL: &str = "moomoo-measured-fees-1+today-universe+liq-sqrt";
const RECORD_CANDIDATES: usize = 300;

/// Trades with their entry features, times converted to real UTC; trades that cannot be mapped are left out.
fn with_features(
    m: &Market,
    closes: &[i64],
    sc: &[Vec<Option<scores::Score>>],
    p: &XsParams,
    trades: &[Trade],
) -> Vec<(Trade, serde_json::Value)> {
    let mut cache = HashMap::new();
    trades
        .iter()
        .filter_map(|t| {
            Some((
                wall_trade(m, closes, t)?,
                entry_features(m, closes, sc, p, t, &mut cache),
            ))
        })
        .collect()
}

/// Profit factor of a trade list: gross profit over gross loss (infinite with no loss, 0 with no trades).
fn profit_factor(trades: &[(Trade, serde_json::Value)]) -> f64 {
    let (gp, gl) = trades.iter().fold((0.0, 0.0), |(gp, gl), (t, _)| {
        if t.pnl > 0.0 {
            (gp + t.pnl, gl)
        } else {
            (gp, gl - t.pnl)
        }
    });
    if gl > 0.0 {
        gp / gl
    } else if gp > 0.0 {
        f64::INFINITY
    } else {
        0.0
    }
}

/// Sessions of data one set of settings is judged on, in training data. The live windows of the longer
/// intervals use the same latest 14-session window; training runs never use one parameter set across longer windows:
/// split into look-forward blocks of this many sessions, each with its own search.
const BLOCK_SESSIONS: usize = 14;

/// Rolling look-forward blocks for one interval. For every block start (sessions `offset`, `offset + 14`, ...):
/// - `oos`: the full search picks settings on the 14 sessions BEFORE the block (champion/challenger against the
///   settings of the previous block), and those settings, held fixed, trade the next 14 sessions from the run's start capital;
/// - `random`: random settings from the same space trade the same 14 sessions (the unselected baseline);
/// - `chosen`: the search winner on its own 14-session selection window (in-sample, selection-biased).
///
/// Several `offsets` stagger the blocks so every part of the history is covered by differently-aligned blocks.
#[allow(clippy::too_many_arguments)]
fn export(
    arch: &BtArchive,
    big: &Big,
    seed: u64,
    random_n: usize,
    random_only: bool,
    offsets: &[usize],
    start: f64,
    suffix: &str,
) -> Result<(usize, usize)> {
    let (m, i) = (&big.m, big.interval);
    let n = m.ts.len();
    let warm = i.warmup_candles();
    // 14 sessions of selection data (plus warm-up candles that only feed the indicators).
    let win = BLOCK_SESSIONS * i.per_session() + warm;
    let first = big
        .opens
        .iter()
        .position(|&a| a >= win)
        .context("not enough history for one window")?;
    let space = SearchSpace {
        warmup_bars: if i == Interval::M15 { 0 } else { warm },
        ..walkforward::space_for(i)
    };
    let key = i.key();
    let (runs, fresh) = (std::cell::Cell::new(0usize), std::cell::Cell::new(0usize));
    let put = |run: BtRun| -> Result<()> {
        runs.set(runs.get() + 1);
        if arch.record(0, common::now_ms(), &run)? {
            fresh.set(fresh.get() + 1);
        }
        Ok(())
    };
    for &offset in offsets {
        let mut current: Option<XsParams> = None;
        let mut record: Vec<(f64, XsParams)> = Vec::new();
        let mut k = first + offset;
        // Only full 14-session blocks: runs never mix lengths.
        while k + BLOCK_SESSIONS <= big.opens.len() {
            let a = big.opens[k];
            let b = big.opens.get(k + BLOCK_SESSIONS).copied().unwrap_or(n);
            let lo = a - win;
            let sel = m.slice(lo..a);
            let sc = scores::compute(&sel, walkforward::UNIVERSE);
            let block_seed = seed
                .wrapping_mul(1_000_003)
                .wrapping_add((k * 31 + offset) as u64);
            let rep = if random_only {
                None
            } else {
                let recorded: Vec<XsParams> = record.iter().map(|r| r.1.clone()).collect();
                Some(walkforward::search_random(
                    &sel,
                    &sc,
                    start,
                    Instant::now() + Duration::from_secs(3600),
                    block_seed,
                    &recorded,
                    space,
                )?)
            };
            if let Some(rep) = rep.as_ref() {
                for r in &rep.top {
                    match record.iter_mut().find(|x| x.1 == r.params) {
                        Some(x) => x.0 = x.0.max(r.robust_score),
                        None => record.push((r.robust_score, r.params.clone())),
                    }
                }
                record.sort_by(|x, y| y.0.total_cmp(&x.0));
                record.truncate(RECORD_CANDIDATES);
                let (chosen, _) = walkforward::choose_live(
                    &sel,
                    &sc,
                    start,
                    rep.params.as_ref(),
                    current.as_ref(),
                    space.warmup_bars..sel.ts.len(),
                );
                if chosen.is_some() {
                    current = chosen;
                }
            }
            // The block's market: the selection window plus the 14 forward sessions.
            let tr = m.slice(lo..b);
            let sct = scores::compute(&tr, walkforward::UNIVERSE);
            let closes = &big.closes[lo..b];
            let (ta, tb) = (a - lo, b - lo);

            // chosen: the winner on its own selection window.
            if let Some((p, rep)) = rep.as_ref().and_then(|r| r.params.as_ref().map(|p| (p, r))) {
                let range = space.warmup_bars..sel.ts.len();
                let ipf = xs::backtest(&sel, &sc, range.clone(), p, start);
                let mt = ipf.metrics();
                let ok = mt.return_pct() > 0.0 && mt.liquidations == 0;
                put(BtRun {
                    source: "chosen",
                    window_label: format!("{key}-fwd14-chosen{suffix}"),
                    first_bar_ts: sel.wall[range.start],
                    last_bar_ts: big.closes[a - 1],
                    start_equity: start,
                    params: params_value(p),
                    seed: Some(block_seed as i64),
                    robust_score: rep.robust_score,
                    return_pct: mt.return_pct(),
                    max_drawdown_pct: mt.max_drawdown_pct,
                    profit_factor: mt.profit_factor(),
                    successful: ok,
                    trades: if ok {
                        with_features(&sel, &closes[..ta], &sc, p, &ipf.trades)
                    } else {
                        Vec::new()
                    },
                    daily: Vec::new(),
                })?;
            }

            // oos: the chosen settings, fixed, trade the next 14 sessions.
            if let Some(p) = current.as_ref().filter(|_| !random_only) {
                let opf = xs::backtest(&tr, &sct, ta..tb, p, start);
                let mt = opf.metrics();
                let trades = with_features(&tr, closes, &sct, p, &opf.trades);
                put(BtRun {
                    source: "oos",
                    window_label: format!("{key}-fwd14-oos{suffix}"),
                    first_bar_ts: m.wall[a],
                    last_bar_ts: big.closes[b - 1],
                    start_equity: start,
                    params: params_value(p),
                    seed: Some(seed as i64),
                    robust_score: None,
                    return_pct: mt.return_pct(),
                    max_drawdown_pct: mt.max_drawdown_pct,
                    profit_factor: profit_factor(&trades),
                    successful: mt.return_pct() > 0.0 && mt.liquidations == 0,
                    trades,
                    daily: Vec::new(),
                })?;
            }

            // random: unselected settings from the same space trade the same 14 sessions.
            let band = MarginBand::new(&sel)?;
            for (j, p) in
                walkforward::sample_space(block_seed ^ 0x9E37_79B9, random_n, &band, space)
                    .iter()
                    .enumerate()
            {
                let rpf = xs::backtest(&tr, &sct, ta..tb, p, start);
                let mt = rpf.metrics();
                put(BtRun {
                    source: "random",
                    window_label: format!("{key}-fwd14-random{suffix}"),
                    first_bar_ts: m.wall[a],
                    last_bar_ts: big.closes[b - 1],
                    start_equity: start,
                    params: params_value(p),
                    seed: Some((block_seed ^ j as u64) as i64),
                    robust_score: None,
                    return_pct: mt.return_pct(),
                    max_drawdown_pct: mt.max_drawdown_pct,
                    profit_factor: mt.profit_factor(),
                    successful: mt.return_pct() > 0.0 && mt.liquidations == 0,
                    trades: with_features(&tr, closes, &sct, p, &rpf.trades),
                    daily: Vec::new(),
                })?;
            }
            eprintln!(
                "  {} seed {seed} offset {offset}: block {} .. {} done ({} runs, {} new)",
                i.label(),
                fmt(m.wall[a]),
                fmt(big.closes[b - 1]),
                runs.get(),
                fresh.get()
            );
            k += BLOCK_SESSIONS;
        }
    }
    Ok((runs.get(), fresh.get()))
}

/// Writes the research archive: rolling 14-session look-forward blocks (oos, random and chosen runs with entry features) for the lab to learn from.
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let which = args.get(1).map_or("all", |s| s.as_str());
    let seeds: u64 = args.get(2).map_or(Ok(1), |s| s.parse())?;
    let random_n: usize = args.get(3).map_or(Ok(8), |s| s.parse())?;
    let random_only = args.get(4).map(String::as_str) == Some("random-only");
    // Block alignments (session offsets): staggering them covers every part of the history by differently-cut blocks.
    let offsets: Vec<usize> = std::env::var("LAB_OFFSETS")
        .unwrap_or_else(|_| "0,5,10".into())
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    let intervals: Vec<Interval> = if which == "all" {
        Interval::ALL.to_vec()
    } else {
        vec![Interval::from_key(which).context("interval key: 15m, 1h, 4h, 1d or all")?]
    };
    moomoo_bot::engine::rules::LIQUIDITY_MODEL.store(
        common::liquidity_default(),
        std::sync::atomic::Ordering::Relaxed,
    );
    common::apply_order_cap();
    let arch = BtArchive::open(
        &moomoo_bot::platform::bot_backtest_archive_path(),
        HISTORY_ENGINE,
    )?
    .with_cost_model(HISTORY_COST_MODEL);
    let cal = sessions().await?;
    let data = load_data()?;
    for i in intervals {
        let big = match big_market(&cal, &data, i) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: skipped ({e:#})", i.label());
                continue;
            }
        };
        let win = BLOCK_SESSIONS * i.per_session() + i.warmup_candles();
        let rolling = big.opens.iter().filter(|&&a| a >= win).count();
        if rolling < BLOCK_SESSIONS + 10 {
            eprintln!("{}: only {rolling} sessions have a full window before them in the store; skipped (the stored history is too short for a rolling test)", i.label());
            continue;
        }
        eprintln!(
            "{}: {rolling} unseen sessions, {} .. {}",
            i.label(),
            fmt(big.m.wall[big.opens[big.opens.len() - rolling]]),
            fmt(big.m.wall[big.m.ts.len() - 1])
        );
        // LAB_SEED_BASE rotates the seeds between scheduled runs, so each run adds new settings instead of repeating itself.
        let base: u64 = std::env::var("LAB_SEED_BASE")
            .ok()
            .and_then(|x| x.parse().ok())
            .unwrap_or(0);
        for seed in base + 1..=base + seeds {
            let t0 = Instant::now();
            // LAB_CAPITALS: comma list of start capitals, `auto` = the bot's own (a START_CAPITAL account, capped by the available cash). Runs at a
            // non-default capital carry a `-c<amount>` label suffix so the lab never mixes order-size regimes.
            for spec in std::env::var("LAB_CAPITALS")
                .unwrap_or_else(|_| "auto".into())
                .split(',')
            {
                let (start, suffix) = match spec.trim().parse::<f64>() {
                    Ok(c) => (c, format!("-c{}", c as i64)),
                    Err(_) => (common::capital(), String::new()),
                };
                let (runs, fresh) = export(
                    &arch,
                    &big,
                    seed,
                    random_n,
                    random_only,
                    &offsets,
                    start,
                    &suffix,
                )?;
                println!(
                    "{} seed {seed} capital {start:.0}: {runs} runs, {fresh} new, {:.0} s",
                    i.label(),
                    t0.elapsed().as_secs_f64()
                );
            }
        }
    }
    Ok(())
}

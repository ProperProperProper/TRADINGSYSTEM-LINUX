//! Phase 2 search: overlapping 14-session periods laid on a fixed grid across the scored part of the 40-session window, a new period starting every 3
//! sessions, so every 3 new sessions of data add another one (owner 2026-10-18). Each new period is searched once with the bot's own
//! `walkforward::search_random` and stored (`search_periods`); every candidate setting is re-run on all current periods, and the one that holds up in all
//! of them is preferred. Periods overlap, so that is a stability check, NOT out-of-sample evidence (the bot says of its live search that it "reduces
//! selection noise, it does not prove an edge"). Genuine forward evidence appears as data grows: a stored winner is tested on any later period that starts
//! after its own period ended. Nothing here re-implements the engine.
use anyhow::{Result, ensure};
use moomoo_bot::engine::walkforward::{self, MAX_DRAWDOWN_PCT, SearchSpace};
use moomoo_bot::engine::xs::{self, XsParams};
use moomoo_bot::engine::{Interval, scores};
use rusqlite::{Connection, OptionalExtension};
use std::ops::Range;
use std::time::{Duration, Instant};
use thomas_core::config::{PERIOD_SESSIONS, PERIOD_STEP_SESSIONS, grid_starts};

use crate::market::Built;

/// Scores with EVERY symbol allowed into the universe (owner: all symbols are tested), not the bot's top `walkforward::UNIVERSE` by turnover.
pub fn compute_scores_all(b: &Built) -> Vec<Vec<Option<scores::Score>>> {
    scores::compute_with(&b.market, b.market.symbols.len(), false)
}

/// The market as every research command sees it: loaded from the stores, cut to the 40-session window, built on 1-minute candles, scored with every symbol
/// allowed in. One place, so `search`, `learn`, `improve` and `walkforward` cannot drift apart.
pub struct Prepared {
    pub interval: Interval,
    /// Candles per session at this interval.
    pub per_session: usize,
    pub loaded: crate::market::Loaded,
    pub built: Built,
    pub scores: Vec<Vec<Option<scores::Score>>>,
    /// Candle index of the first scored candle.
    pub first_scored: usize,
}

/// The candle size every trading and learning step of this system runs on: the real 1-minute candles themselves, no aggregation (owner 2026-10-20; it was 5-hour
/// candles built from them). The paper trader, `search`, `learn` and the other commands that take no `--interval` all use this one constant.
pub const ENGINE_INTERVAL: Interval = Interval::M1;

pub fn prepare(
    snapshot: &std::path::Path,
    store: &std::path::Path,
    holdings: &std::path::Path,
) -> Result<Prepared> {
    prepare_for(snapshot, store, holdings, ENGINE_INTERVAL)
}

/// The same for any candle size the engine knows (all aggregated from the real 1-minute bars); the warm-up is 20 sessions at that size.
pub fn prepare_for(
    snapshot: &std::path::Path,
    store: &std::path::Path,
    holdings: &std::path::Path,
    interval: Interval,
) -> Result<Prepared> {
    let mut loaded = crate::market::load_symbol_data(snapshot, store, holdings)?;
    crate::market::apply_window(&mut loaded, thomas_core::config::WINDOW_SESSIONS);
    let per_session = interval.per_session();
    crate::replay::use_warmup_sessions(thomas_core::config::WARMUP_SESSIONS, per_session);
    let built = crate::market::build_on(&loaded, interval)?;
    let scores = compute_scores_all(&built);
    let first_scored = (0..built.market.ts.len())
        .find(|&t| scores.iter().any(|r| r.get(t).and_then(|s| s.as_ref()).is_some()))
        .unwrap_or(0);
    Ok(Prepared {
        interval,
        per_session,
        loaded,
        built,
        scores,
        first_scored,
    })
}

/// The DEEP market: only the symbols with a long real history (the bot's roughly 95 moomoo symbols), over all of it, so a walk-forward has many unseen blocks. It needs
/// the reference symbol (SPY, the trend filter) to have the same history; until its real 1-minute pull is possible (moomoo quota) this refuses, saying why.
pub fn prepare_deep(
    snapshot: &std::path::Path,
    store: &std::path::Path,
    holdings: &std::path::Path,
    interval: Interval,
    min_sessions: usize,
) -> Result<Prepared> {
    use moomoo_bot::engine::REFERENCE;
    let mut loaded = crate::market::load_symbol_data(snapshot, store, holdings)?;
    // a full session is 26 real 1-minute bars; a symbol may miss a tenth of them
    let need = min_sessions * 26 * 9 / 10;
    let ref_bars = loaded
        .data
        .iter()
        .find(|d| d.symbol == REFERENCE)
        .map_or(0, |d| d.trades.len());
    anyhow::ensure!(
        ref_bars >= need,
        "the reference symbol {REFERENCE} has {ref_bars} real 1-minute bars (about {} sessions); a deep walk-forward needs {min_sessions} sessions. Its history is quota-gated: pull it with the bot's `ref_fetch {REFERENCE}` (slots free about 2026-10-21), then `thomas snapshot && thomas merge`",
        ref_bars / 26
    );
    loaded.data.retain(|d| d.trades.len() >= need);
    let per_session = interval.per_session();
    crate::replay::use_warmup_sessions(thomas_core::config::WARMUP_SESSIONS, per_session);
    let built = crate::market::build_on(&loaded, interval)?;
    let scores = compute_scores_all(&built);
    let first_scored = (0..built.market.ts.len())
        .find(|&t| scores.iter().any(|r| r[t].is_some()))
        .ok_or_else(|| anyhow::anyhow!("no symbol is scored"))?;
    Ok(Prepared {
        interval,
        per_session,
        loaded,
        built,
        scores,
        first_scored,
    })
}

/// Threads for parallel work: 84% of the cores, since the owner keeps 16% of the Mac free (8 of 10 here).
pub fn max_threads() -> usize {
    let n = std::thread::available_parallelism().map_or(1, |n| n.get());
    ((n as f64 * 0.84) as usize).max(1)
}

/// `f` over `items` on up to `max_threads()` threads, results in the order of `items`.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(usize, &T) -> R + Sync) -> Vec<R> {
    let mut out = Vec::with_capacity(items.len());
    let width = max_threads();
    for (c, chunk) in items.chunks(width).enumerate() {
        std::thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    let f = &f;
                    s.spawn(move || f(c * width + i, x))
                })
                .collect();
            out.extend(
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))),
            );
        });
    }
    out
}

/// Settings of one run.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// Candle index of the first scored candle.
    pub first_scored: usize,
    pub candles_per_session: usize,
    /// The candle size of the market being searched (the search space is the interval's own).
    pub interval: Interval,
    pub equity: f64,
    /// Combinations drawn per new period (the bot's live search draws 200,000 per window).
    pub samples: usize,
    /// Closed trades needed to count as usable in a period (the bot's own `walkforward::MIN_TRADES` is 8).
    pub min_trades: usize,
    pub seed: u64,
}

/// One setting's result over one period.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodMetrics {
    pub return_pct: f64,
    pub max_drawdown_pct: f64,
    pub trades: usize,
    pub wins: usize,
    pub usable: bool,
}

impl PeriodMetrics {
    /// The bot's objective: return % minus half the max drawdown %.
    pub fn objective(&self) -> f64 {
        self.return_pct - 0.5 * self.max_drawdown_pct
    }
}

/// A candidate setting and its result in every current period.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub params: XsParams,
    pub per_period: Vec<PeriodMetrics>,
}

impl Candidate {
    pub fn periods_usable(&self) -> usize {
        self.per_period.iter().filter(|m| m.usable).count()
    }
    /// The worst period's objective: a setting is only as good as its weakest period.
    pub fn worst_objective(&self) -> f64 {
        self.per_period
            .iter()
            .map(PeriodMetrics::objective)
            .fold(f64::INFINITY, f64::min)
    }
}

/// A period of the grid inside the current window.
#[derive(Debug, Clone)]
pub struct Period {
    pub start_ms: i64,
    pub end_ms: i64,
    pub range: Range<usize>,
    /// Searched in this run (false: its search was stored by an earlier run and is not repeated).
    pub new: bool,
    pub evaluated: usize,
    pub usable: usize,
    pub seconds: f64,
    pub winner: Option<XsParams>,
    pub robust_score: Option<f64>,
    pub raw_best_score: Option<f64>,
}

/// A stored winner tested on a later period that starts after its own period ended: out-of-sample.
#[derive(Debug, Clone)]
pub struct Forward {
    pub winner_period_start_ms: i64,
    pub on_period_start_ms: i64,
    pub params: XsParams,
    pub metrics: PeriodMetrics,
}

/// The whole run.
#[derive(Debug, Clone)]
pub struct Report {
    pub periods: Vec<Period>,
    /// Every candidate evaluated on every current period, best first (usable in the most periods, then by worst-period objective).
    pub candidates: Vec<Candidate>,
    pub forward: Vec<Forward>,
    /// The reference symbol's own return over each period, for context.
    pub reference_return_pct: Vec<Option<f64>>,
}

impl Report {
    /// The preferred setting: usable in every current period (return > 0, no liquidation, drawdown within the bot's limit, enough trades), best worst-period objective.
    pub fn best(&self) -> Option<&Candidate> {
        self.candidates
            .first()
            .filter(|c| c.periods_usable() == c.per_period.len())
    }
    pub fn new_periods(&self) -> usize {
        self.periods.iter().filter(|p| p.new).count()
    }
}

pub(crate) fn metrics_of(
    b: &Built,
    sc: &[Vec<Option<scores::Score>>],
    p: &XsParams,
    range: Range<usize>,
    equity: f64,
    min_trades: usize,
) -> PeriodMetrics {
    let m = xs::backtest(&b.market, sc, range, p, equity).metrics();
    PeriodMetrics {
        return_pct: m.return_pct(),
        max_drawdown_pct: m.max_drawdown_pct,
        trades: m.trades,
        wins: m.wins,
        usable: m.execution_error.is_none()
            && m.return_pct().is_finite()
            && m.return_pct() > 0.0
            && m.liquidations == 0
            && m.max_drawdown_pct <= MAX_DRAWDOWN_PCT
            && m.trades >= min_trades,
    }
}

/// A stored period's search result: winner JSON, usable count, robust score, best single score.
/// The calendar index just after the newest session the calendar knows. The newest session may still be running (or not yet in the calendar, which is derived from complete
/// sessions): the span then ends at the last session before it instead of the whole search failing.
pub fn last_known_session_end(
    wall: &[i64],
    opens: &[i64],
    cps: usize,
    first_candle: usize,
) -> Option<usize> {
    // candles of a session that has only just begun (a part of `cps`) do not make a session: drop them first, then any whole sessions the calendar does not know
    let mut end = first_candle + wall.len().saturating_sub(first_candle) / cps * cps;
    while end >= cps && end > first_candle && opens.binary_search(&wall[end - cps]).is_err() {
        end -= cps;
    }
    let last = end.checked_sub(cps)?.max(first_candle);
    opens.binary_search(&wall[last]).ok().map(|i| i + 1)
}

type StoredPeriod = (Option<String>, i64, Option<f64>, Option<f64>);

/// Searches every period of the grid that is not stored yet, stores it, then cross-evaluates all candidates on all current periods and
/// tests stored winners forward on later, non-overlapping periods.
pub fn run_periods(
    conn: &Connection,
    b: &Built,
    sc: &[Vec<Option<scores::Score>>],
    calendar: &[(i64, i64)],
    cfg: Settings,
    now_ms: i64,
) -> Result<Report> {
    let cps = cfg.candles_per_session;
    ensure!(cps > 0, "candles_per_session must be positive");
    ensure!(
        cfg.first_scored < b.market.ts.len(),
        "first_scored is outside the market"
    );
    ensure!(
        b.market.ts.len() >= cps,
        "market is shorter than one session"
    );
    let opens: Vec<i64> = calendar.iter().map(|s| s.0).collect();
    let session_of = |ms: i64| opens.binary_search(&ms).ok();
    let first_candle = cfg.first_scored.div_ceil(cps) * cps;
    let span_first = session_of(b.market.wall[first_candle])
        .ok_or_else(|| anyhow::anyhow!("scored span does not start on a calendar session"))?;
    let span_end = last_known_session_end(&b.market.wall, &opens, cps, first_candle)
        .ok_or_else(|| anyhow::anyhow!("market does not end on a calendar session"))?;
    let anchor = conn
        .query_row(
            "SELECT min(period_start_ms) FROM search_periods WHERE min_trades=?",
            [cfg.min_trades as i64],
            |r| r.get::<_, Option<i64>>(0),
        )?
        .and_then(session_of)
        .unwrap_or(span_first);
    let starts = grid_starts(
        anchor,
        span_first,
        span_end,
        PERIOD_SESSIONS,
        PERIOD_STEP_SESSIONS,
    );
    ensure!(
        !starts.is_empty(),
        "the scored span of {} sessions is shorter than one period of {PERIOD_SESSIONS}",
        span_end - span_first
    );
    moomoo_bot::engine::governor::UNTHROTTLED.store(true, std::sync::atomic::Ordering::Relaxed);
    walkforward::KEEP_ALL_USABLE.store(true, std::sync::atomic::Ordering::Relaxed);
    let space = SearchSpace {
        samples: cfg.samples,
        min_trades: cfg.min_trades,
        ..walkforward::space_for(cfg.interval)
    };

    // pass 1: which periods of the grid are stored already
    struct Pre {
        start_ms: i64,
        end_ms: i64,
        range: Range<usize>,
        stored: Option<StoredPeriod>,
    }
    let mut pre = Vec::new();
    for &s in &starts {
        let (start_ms, end_ms) = (opens[s], calendar[s + PERIOD_SESSIONS - 1].1);
        let first = b
            .market
            .wall
            .binary_search(&start_ms)
            .map_err(|_| anyhow::anyhow!("period start is not a candle of the market"))?;
        let range = first..first + PERIOD_SESSIONS * cps;
        ensure!(
            range.end <= b.market.ts.len(),
            "period extends beyond the market"
        );
        let stored: Option<StoredPeriod> = conn
            .query_row("SELECT params_json, usable, robust_score, raw_best_score FROM search_periods WHERE period_start_ms=? AND min_trades=?", rusqlite::params![start_ms, cfg.min_trades as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .optional()?;
        pre.push(Pre {
            start_ms,
            end_ms,
            range,
            stored,
        });
    }
    // pass 2: search the new periods side by side (each has its own seed, so the result is the same as one after the other)
    let todo: Vec<usize> = (0..pre.len())
        .filter(|&k| pre[k].stored.is_none())
        .collect();
    let searched: Vec<Result<(walkforward::SearchReport, f64)>> = par_map(&todo, |_, &k| {
        let range = pre[k].range.clone();
        let m = b.market.slice(range.clone());
        let rows: Vec<Vec<Option<scores::Score>>> =
            sc.iter().map(|row| row[range.clone()].to_vec()).collect();
        let t = Instant::now();
        let rep = walkforward::search_random(
            &m,
            &rows,
            cfg.equity,
            Instant::now() + Duration::from_secs(3600),
            cfg.seed + k as u64,
            &[],
            space,
        )?;
        Ok((rep, t.elapsed().as_secs_f64()))
    });
    let mut searched: std::collections::VecDeque<_> = searched.into_iter().collect();
    // pass 3: store the new ones and build the periods in grid order
    let mut periods = Vec::new();
    let mut pool: Vec<XsParams> = Vec::new();
    for (k, p) in pre.into_iter().enumerate() {
        let Pre {
            start_ms,
            end_ms,
            range,
            stored,
        } = p;
        let period = if let Some((json, usable, robust, raw)) = stored {
            let winner = json.map(|j| serde_json::from_str(&j)).transpose()?;
            Period {
                start_ms,
                end_ms,
                range,
                new: false,
                evaluated: 0,
                usable: usable as usize,
                seconds: 0.0,
                winner,
                robust_score: robust,
                raw_best_score: raw,
            }
        } else {
            let (rep, seconds) = searched.pop_front().expect("one result per new period")?;
            {
                // keep EVERY winning setting of this period for good
                let tx = conn.unchecked_transaction()?;
                for w in &rep.usable_all {
                    let json = serde_json::to_string(&w.params)?;
                    let hash = hash_of(&json);
                    tx.execute("INSERT OR IGNORE INTO winning_settings(period_start_ms, min_trades, params_hash, params_json, objective) VALUES(?,?,?,?,?)", rusqlite::params![start_ms, cfg.min_trades as i64, hash, json, w.robust_score])?;
                }
                tx.commit()?;
            }
            let winner_json = rep.params.as_ref().map(serde_json::to_string).transpose()?;
            conn.execute(
                "INSERT INTO search_periods(period_start_ms, min_trades, period_end_ms, searched_ms, samples, seed, usable, params_json, robust_score, raw_best_score) VALUES(?,?,?,?,?,?,?,?,?,?)",
                rusqlite::params![start_ms, cfg.min_trades as i64, end_ms, now_ms, cfg.samples as i64, (cfg.seed + k as u64) as i64, rep.usable as i64, winner_json, rep.robust_score, rep.raw_best_score],
            )?;
            Period {
                start_ms,
                end_ms,
                range,
                new: true,
                evaluated: rep.evaluated,
                usable: rep.usable,
                seconds,
                winner: rep.params,
                robust_score: rep.robust_score,
                raw_best_score: rep.raw_best_score,
            }
        };
        pool.extend(period.winner.iter().cloned());
        periods.push(period);
    }
    // every winner ever stored (also those of periods that have scrolled out of the window) is a candidate and gives forward evidence
    let mut earlier: Vec<(i64, XsParams)> = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT period_start_ms, params_json FROM winning_settings WHERE min_trades=?",
        )?;
        for row in st.query_map([cfg.min_trades as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })? {
            let (ms, json) = row?;
            if let Ok(p) = serde_json::from_str::<XsParams>(&json) {
                pool.push(p.clone());
                earlier.push((ms, p));
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    pool.retain(|p| seen.insert(serde_json::to_string(p).unwrap_or_default()));
    let mut candidates: Vec<Candidate> = pool
        .into_iter()
        .map(|params| {
            let per_period = periods
                .iter()
                .map(|p| metrics_of(b, sc, &params, p.range.clone(), cfg.equity, cfg.min_trades))
                .collect();
            Candidate { params, per_period }
        })
        .collect();
    candidates.sort_by(|a, c| {
        c.periods_usable()
            .cmp(&a.periods_usable())
            .then(c.worst_objective().total_cmp(&a.worst_objective()))
    });

    // forward evidence: a stored winner on a current period that starts after the winner's own period ended
    let mut forward = Vec::new();
    for (ws_ms, params) in &earlier {
        let Some(ws) = session_of(*ws_ms) else {
            continue;
        };
        for (p, &s) in periods.iter().zip(&starts) {
            if s >= ws + PERIOD_SESSIONS {
                forward.push(Forward {
                    winner_period_start_ms: *ws_ms,
                    on_period_start_ms: p.start_ms,
                    params: params.clone(),
                    metrics: metrics_of(b, sc, params, p.range.clone(), cfg.equity, cfg.min_trades),
                });
            }
        }
    }
    let spy = b.market.symbols.iter().position(|s| s == "SPY");
    let reference_return_pct = periods
        .iter()
        .map(|p| {
            spy.and_then(|s| {
                Some(
                    (b.market.bars[s][p.range.end - 1]?.close
                        / b.market.bars[s][p.range.start]?.open
                        - 1.0)
                        * 100.0,
                )
            })
        })
        .collect();
    Ok(Report {
        periods,
        candidates,
        forward,
        reference_return_pct,
    })
}

/// What every stored run of one `record` call shares.
struct RunCtx {
    sid: i64,
    equity: f64,
    seed: i64,
    now_ms: i64,
}

/// One result row to store.
struct RunRow<'a> {
    params_json: &'a str,
    label: String,
    first_ts: i64,
    last_ts: i64,
    metrics: &'a PeriodMetrics,
    robust: Option<f64>,
}

fn hash_of(json: &str) -> String {
    format!(
        "{:016x}",
        json.bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |h, x| (h ^ u64::from(x))
                .wrapping_mul(0x0100_0000_01b3))
    )
}

fn insert_run(conn: &Connection, c: &RunCtx, r: RunRow) -> Result<()> {
    let hash = format!(
        "{:016x}",
        r.params_json
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |h, x| (h ^ u64::from(x))
                .wrapping_mul(0x0100_0000_01b3))
    );
    let m = r.metrics;
    conn.execute(
        "INSERT OR REPLACE INTO backtest_runs(ts_ms, strategy_id, params_hash, params_json, window_label, first_bar_ts, last_bar_ts, start_equity, return_pct, max_drawdown_pct, trades, wins, profit_factor, fees, robust_score, seed, engine_version) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![c.now_ms, c.sid, hash, r.params_json, r.label, r.first_ts, r.last_ts, c.equity, m.return_pct, m.max_drawdown_pct, m.trades as i64, m.wins as i64, 0.0, 0.0, r.robust, c.seed, crate::bot_commit()],
    )?;
    Ok(())
}

/// Stores the run: the strategy row (notes say what was searched and what held up), the preferred setting's result in each current period, and every
/// forward test.
pub fn record(
    conn: &Connection,
    b: &Built,
    rep: &Report,
    cfg: Settings,
    now_ms: i64,
) -> Result<i64> {
    let name = "bot-xs-random-search (overlapping 14-session periods every 3 sessions, in-sample)";
    let usable_all = rep
        .candidates
        .iter()
        .filter(|c| c.periods_usable() == c.per_period.len())
        .count();
    let stored: i64 = conn.query_row(
        "SELECT count(*) FROM search_periods WHERE min_trades=?",
        [cfg.min_trades as i64],
        |r| r.get(0),
    )?;
    let note = format!(
        "{} periods in the window ({} searched now, {stored} stored in total), {} draws per new period, {} candidates, {usable_all} usable in every current period, {} forward tests, {}",
        rep.periods.len(),
        rep.new_periods(),
        cfg.samples,
        rep.candidates.len(),
        rep.forward.len(),
        if rep.best().is_some() {
            "preferred setting stored in backtest_runs"
        } else {
            "NO setting held up in every period"
        }
    );
    conn.execute(
        "INSERT INTO strategies(name, version, git_commit, status, created_ms, notes) VALUES(?, 1, ?, 'challenger', ?, ?) ON CONFLICT(name, version) DO UPDATE SET notes = excluded.notes, git_commit = excluded.git_commit",
        (name, crate::bot_commit(), now_ms, &note),
    )?;
    let sid: i64 = conn.query_row(
        "SELECT id FROM strategies WHERE name=? AND version=1",
        [name],
        |r| r.get(0),
    )?;
    conn.execute(
        "DELETE FROM backtest_runs WHERE strategy_id=? AND window_label LIKE 'preferred-%'",
        [sid],
    )?;
    let ctx = RunCtx {
        sid,
        equity: cfg.equity,
        seed: cfg.seed as i64,
        now_ms,
    };
    if let Some(c) = rep.best() {
        let json = serde_json::to_string(&c.params)?;
        for (m, p) in c.per_period.iter().zip(&rep.periods) {
            let row = RunRow {
                params_json: &json,
                label: format!("preferred-period-{}-in-sample", p.start_ms),
                first_ts: b.market.wall[p.range.start],
                last_ts: b.market.wall[p.range.end - 1],
                metrics: m,
                robust: p.robust_score,
            };
            insert_run(conn, &ctx, row)?;
        }
    }
    for f in &rep.forward {
        if let Some(p) = rep
            .periods
            .iter()
            .find(|p| p.start_ms == f.on_period_start_ms)
        {
            let json = serde_json::to_string(&f.params)?;
            let row = RunRow {
                params_json: &json,
                label: format!(
                    "forward-winner-of-{}-on-{}",
                    f.winner_period_start_ms, f.on_period_start_ms
                ),
                first_ts: b.market.wall[p.range.start],
                last_ts: b.market.wall[p.range.end - 1],
                metrics: &f.metrics,
                robust: None,
            };
            insert_run(conn, &ctx, row)?;
        }
    }
    Ok(sid)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_span_ends_at_the_last_session_the_calendar_knows_when_a_newer_one_is_still_running() {
        // sessions of 2 candles opening at 0, 10, 20, 30; the calendar knows the first three, the market already holds the first candle of the fourth
        let wall = [0, 1, 10, 11, 20, 21, 30];
        let opens = [0, 10, 20];
        assert_eq!(
            last_known_session_end(&wall, &opens, 2, 0),
            Some(3),
            "the first candle of a fourth session is only a part of one: it is dropped"
        );
        let wall_whole = [0, 1, 10, 11, 20, 21];
        assert_eq!(
            last_known_session_end(&wall_whole, &opens, 2, 0),
            Some(3),
            "ends on the third session"
        );
        let wall_new = [0, 1, 10, 11, 20, 21, 30, 31];
        assert_eq!(
            last_known_session_end(&wall_new, &opens, 2, 0),
            Some(3),
            "the fourth session is not in the calendar yet: the span stops before it"
        );
        assert_eq!(last_known_session_end(&[], &opens, 2, 0), None);
    }

    use super::*;

    #[test]
    fn par_map_keeps_the_order_and_uses_every_item_once() {
        let items: Vec<usize> = (0..37).collect();
        let out = par_map(&items, |i, x| (i, x * 2));
        assert_eq!(out, (0..37).map(|i| (i, i * 2)).collect::<Vec<_>>());
        assert!(
            max_threads() >= 1
                && max_threads() <= std::thread::available_parallelism().map_or(1, |n| n.get())
        );
    }

    #[test]
    fn the_period_length_is_the_bots_own_search_window() {
        assert_eq!(PERIOD_SESSIONS, moomoo_bot::engine::TEST_SESSIONS);
    }
}

//! Per-symbol parameter search for the momentum rule, tested on existing real data only.
//!
//! A symbol's own parameters are the strategy's own numbers: the momentum lookback (completed
//! sessions summed) and the minimum summed return that counts as a signal. Each refit uses only
//! sessions before the day it is used: the parameters are chosen on the earlier two thirds of those
//! sessions and must then earn a positive net return on the later third (which never chose them)
//! before they are accepted. Fits are stored in `symbol_params`; a later fit tries the stored
//! parameters first (they are only replaced by something strictly better on the train sessions) and
//! then a seeded random order of the rest of the grid, up to `trials`.
use crate::momentum_backtest::{Report, load_days, load_universe, record, render};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const LOOKBACK_MAX: usize = 30;
pub const TUNED_STRATEGY: &str = "per_symbol_tuned_momentum_1m";
const THRESHOLD_STEPS: usize = 21; // 0.0% .. 2.0% in 0.1% steps
const START_SESSIONS: usize = 30;
const REFIT_EVERY: usize = 5;
const MIN_TRAIN: usize = 8;
const MIN_VALID: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub lookback: usize,
    pub threshold: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct Fit {
    pub params: Params,
    pub train_mean: f64,
    pub train_n: usize,
    pub valid_mean: f64,
    pub valid_n: usize,
    pub trials: usize,
    pub accepted: bool,
}

#[derive(Default)]
struct Series {
    ret: Vec<f64>,
    pre: Vec<f64>, // pre[i] = sum of ret[..i]
}

impl Series {
    fn push(&mut self, r: f64) {
        if self.pre.is_empty() {
            self.pre.push(0.0);
        }
        self.pre.push(self.pre[self.ret.len()] + r);
        self.ret.push(r);
    }
    /// Signal for the next session using only the sessions already pushed.
    fn signal(&self, p: Params) -> bool {
        let n = self.ret.len();
        n >= p.lookback && self.pre[n] - self.pre[n - p.lookback] > p.threshold
    }
}

fn mean_net(s: &Series, p: Params, from: usize, to: usize, cost: f64) -> (f64, usize) {
    let (mut sum, mut n) = (0.0, 0usize);
    for i in from.max(p.lookback)..to {
        if s.pre[i] - s.pre[i - p.lookback] > p.threshold {
            sum += s.ret[i] - cost;
            n += 1;
        }
    }
    (if n > 0 { sum / n as f64 } else { 0.0 }, n)
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Search the grid using only the first `n` sessions of `s`. The incumbent (previous best) goes first.
fn fit(
    s: &Series,
    n: usize,
    incumbent: Option<Params>,
    trials: usize,
    seed: u64,
    cost: f64,
) -> Option<Fit> {
    if n < START_SESSIONS {
        return None;
    }
    let train_end = n * 2 / 3;
    let mut grid: Vec<Params> = (1..=LOOKBACK_MAX)
        .flat_map(|lookback| {
            (0..THRESHOLD_STEPS).map(move |t| Params {
                lookback,
                threshold: t as f64 * 0.001,
            })
        })
        .collect();
    let mut state = seed;
    for i in (1..grid.len()).rev() {
        grid.swap(i, (splitmix(&mut state) % (i as u64 + 1)) as usize);
    }
    if let Some(inc) = incumbent {
        grid.retain(|p| *p != inc);
        grid.insert(0, inc);
    }
    grid.truncate(trials);
    let mut best: Option<(Params, f64, usize)> = None;
    for p in &grid {
        let (m, k) = mean_net(s, *p, 0, train_end, cost);
        if k >= MIN_TRAIN && best.is_none_or(|b| m > b.1) {
            best = Some((*p, m, k));
        }
    }
    let (params, train_mean, train_n) = best?;
    let (valid_mean, valid_n) = mean_net(s, params, train_end, n, cost);
    Some(Fit {
        params,
        train_mean,
        train_n,
        valid_mean,
        valid_n,
        trials: grid.len(),
        accepted: train_mean > 0.0 && valid_n >= MIN_VALID && valid_mean > 0.0,
    })
}

fn schema(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS symbol_params(
           symbol TEXT NOT NULL, refit_day INTEGER NOT NULL, run_ms INTEGER NOT NULL,
           lookback INTEGER NOT NULL, threshold REAL NOT NULL,
           train_mean REAL NOT NULL, train_n INTEGER NOT NULL,
           valid_mean REAL NOT NULL, valid_n INTEGER NOT NULL,
           trials INTEGER NOT NULL, accepted INTEGER NOT NULL,
           PRIMARY KEY(symbol, refit_day));",
    )?;
    Ok(())
}

fn incumbent(c: &Connection, symbol: &str, before_day: i64) -> Option<Params> {
    c.query_row(
        "SELECT lookback, threshold FROM symbol_params WHERE symbol=? AND refit_day<? ORDER BY refit_day DESC LIMIT 1",
        rusqlite::params![symbol, before_day],
        |r| Ok(Params { lookback: r.get::<_, i64>(0)? as usize, threshold: r.get(1)? }),
    )
    .ok()
}

fn save(c: &Connection, symbol: &str, day: i64, f: &Fit, now_ms: i64) -> Result<()> {
    c.execute(
        "INSERT OR REPLACE INTO symbol_params VALUES(?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            symbol,
            day,
            now_ms,
            f.params.lookback as i64,
            f.params.threshold,
            f.train_mean,
            f.train_n as i64,
            f.valid_mean,
            f.valid_n as i64,
            f.trials as i64,
            i64::from(f.accepted)
        ],
    )?;
    Ok(())
}

fn seed_for(symbol: &str, n: usize) -> u64 {
    symbol.bytes().fold(0xCBF2_9CE4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01B3)
    }) ^ n as u64
}

#[allow(clippy::too_many_arguments)]
/// Walk-forward backtest with per-symbol tuned parameters, then a final fit for the next session.
pub fn run(
    db: &str,
    universe_path: &Path,
    store_path: &Path,
    trials: usize,
    slots: usize,
    cost_bps: f64,
    capital: f64,
    now_ms: i64,
) -> Result<String> {
    anyhow::ensure!(
        trials > 0 && slots > 0 && capital > 0.0,
        "invalid tune settings"
    );
    let universe = load_universe(universe_path)?;
    let mut days = load_days(db)?;
    for rows in days.values_mut() {
        rows.retain(|(s, _)| universe.contains(s.strip_prefix("US.").unwrap_or(s)));
    }
    days.retain(|_, rows| !rows.is_empty());
    let first_day = *days.keys().next().context("no sessions")?;
    let last_day = *days.keys().next_back().context("no sessions")?;
    let cost = cost_bps / 10_000.0;
    let store = Connection::open(store_path)?;
    store.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=10000;")?;
    schema(&store)?;
    let tx = store.unchecked_transaction()?;
    let mut series: HashMap<String, Series> = HashMap::new();
    let mut fits: BTreeMap<String, Fit> = BTreeMap::new();
    let (mut equity, mut peak, mut max_dd) = (capital, capital, 0.0f64);
    let (mut trades, mut wins) = (0usize, 0usize);
    let mut daily = Vec::new();
    for (g, (day, rows)) in days.into_iter().enumerate() {
        if g >= START_SESSIONS && g % REFIT_EVERY == 0 {
            for (sym, s) in &series {
                let n = s.ret.len();
                let inc = incumbent(&tx, sym, day);
                if let Some(f) = fit(s, n, inc, trials, seed_for(sym, n), cost) {
                    save(&tx, sym, day, &f, now_ms)?;
                    fits.insert(sym.clone(), f);
                }
            }
        }
        let mut picks: Vec<(f64, &String)> = fits
            .iter()
            .filter(|(sym, f)| f.accepted && series.get(*sym).is_some_and(|s| s.signal(f.params)))
            .map(|(sym, f)| (f.valid_mean, sym))
            .collect();
        picks.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        picks.truncate(slots);
        let today: HashMap<_, _> = rows.iter().cloned().collect();
        let before = equity;
        let alloc = equity / slots as f64;
        for (_, sym) in &picks {
            if let Some(r) = today.get(*sym) {
                let net = *r - cost;
                equity += alloc * net;
                trades += 1;
                wins += usize::from(net > 0.0);
            }
        }
        for (sym, r) in rows {
            if r.is_finite() {
                series.entry(sym).or_default().push(r);
            }
        }
        daily.push(equity / before - 1.0);
        peak = peak.max(equity);
        max_dd = max_dd.max((peak - equity) / peak);
    }
    // Final fit on every completed session: the parameters and candidates for the next session.
    let next_day = last_day + 1;
    let mut next = Vec::new();
    let mut accepted = 0usize;
    for (sym, s) in &series {
        let n = s.ret.len();
        let inc = incumbent(&tx, sym, next_day);
        if let Some(f) = fit(s, n, inc, trials, seed_for(sym, n), cost) {
            save(&tx, sym, next_day, &f, now_ms)?;
            if f.accepted {
                accepted += 1;
                if s.signal(f.params) {
                    next.push((f.valid_mean, sym.clone(), f));
                }
            }
        }
    }
    tx.commit()?;
    next.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut blocks = Vec::new();
    for i in 0..5 {
        let (a, b) = (i * daily.len() / 5, (i + 1) * daily.len() / 5);
        blocks.push(daily[a..b].iter().fold(1.0, |v, r| v * (1.0 + r)) - 1.0);
    }
    let report = Report {
        symbols: series.len(),
        universe: "nasdaq, per-symbol tuned params".into(),
        first_day,
        last_day,
        sessions: daily.len(),
        trades,
        wins,
        pnl: equity - capital,
        return_pct: (equity / capital - 1.0) * 100.0,
        max_drawdown_pct: max_dd * 100.0,
        blocks,
    };
    record(&store, &report, 0, slots, cost_bps, capital, now_ms)?;
    let mut out = render(&report, 0, slots, cost_bps, capital);
    out.push_str(&format!(
        "tuned: {} symbols, {} accepted on the final fit, {} trials each (grid {})\nnext-session candidates with a live signal ({}):\n",
        series.len(),
        accepted,
        trials.min(LOOKBACK_MAX * THRESHOLD_STEPS),
        LOOKBACK_MAX * THRESHOLD_STEPS,
        next.len()
    ));
    for (_, sym, f) in next.iter().take(slots) {
        out.push_str(&format!(
            "  {sym} lookback={} threshold={:.3} train={:+.3}%/{} valid={:+.3}%/{}\n",
            f.params.lookback,
            f.params.threshold,
            f.train_mean * 100.0,
            f.train_n,
            f.valid_mean * 100.0,
            f.valid_n
        ));
    }
    Ok(out)
}

/// Freeze the next session's candidates: fit every Nasdaq symbol on all completed sessions, store the fit,
/// and emit every accepted symbol whose own signal is on, ranked by held-out mean net return.
pub fn select_next(
    db: &str,
    universe_path: &Path,
    store_path: &Path,
    trials: usize,
    limit: usize,
    cost_bps: f64,
    now_ms: i64,
) -> Result<String> {
    anyhow::ensure!(trials > 0 && limit > 0, "invalid tune settings");
    let universe = load_universe(universe_path)?;
    let days = load_days(db)?;
    let last_day = *days.keys().next_back().context("no sessions")?;
    let mut series: HashMap<String, Series> = HashMap::new();
    for rows in days.into_values() {
        for (sym, r) in rows {
            if r.is_finite() && universe.contains(sym.strip_prefix("US.").unwrap_or(&sym)) {
                series.entry(sym).or_default().push(r);
            }
        }
    }
    let store = Connection::open(store_path)?;
    store.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=10000;")?;
    schema(&store)?;
    let tx = store.unchecked_transaction()?;
    let cost = cost_bps / 10_000.0;
    let next_day = last_day + 1;
    let mut rows = Vec::new();
    for (sym, s) in &series {
        let n = s.ret.len();
        let inc = incumbent(&tx, sym, next_day);
        if let Some(f) = fit(s, n, inc, trials, seed_for(sym, n), cost) {
            save(&tx, sym, next_day, &f, now_ms)?;
            if f.accepted && s.signal(f.params) {
                rows.push((sym.clone(), f));
            }
        }
    }
    tx.commit()?;
    rows.sort_by(|a, b| {
        b.1.valid_mean
            .total_cmp(&a.1.valid_mean)
            .then_with(|| a.0.cmp(&b.0))
    });
    rows.truncate(limit);
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "schema": 1,
        "strategy": TUNED_STRATEGY,
        "data_cutoff_day": last_day,
        "lookback_sessions": 0,
        "candidates": rows.iter().map(|(sym, f)| serde_json::json!({
            "symbol": sym,
            "score": f.valid_mean,
            "observations": f.params.lookback,
            "lookback": f.params.lookback,
            "threshold": f.params.threshold,
            "train_mean": f.train_mean,
            "train_n": f.train_n,
            "valid_mean": f.valid_mean,
            "valid_n": f.valid_n,
        })).collect::<Vec<_>>(),
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(rets: &[f64]) -> Series {
        let mut s = Series::default();
        for r in rets {
            s.push(*r);
        }
        s
    }

    #[test]
    fn no_fit_before_enough_sessions_and_incumbent_is_tried_first() {
        let s = series(&vec![0.01; 29]);
        assert!(fit(&s, 29, None, 100, 1, 0.001).is_none());
        let long: Vec<f64> = (0..90)
            .map(|i| if i % 2 == 0 { 0.02 } else { -0.01 })
            .collect();
        let s = series(&long);
        let inc = Params {
            lookback: 1,
            threshold: 0.0,
        };
        let f = fit(&s, 90, Some(inc), 1, 7, 0.001).unwrap();
        assert_eq!(
            f.params, inc,
            "with one trial only the incumbent is evaluated"
        );
        assert_eq!(f.trials, 1);
    }

    #[test]
    fn fit_ignores_sessions_after_n() {
        let mut a: Vec<f64> = (0..90)
            .map(|i| ((i * 7) % 5) as f64 / 100.0 - 0.02)
            .collect();
        let before = fit(&series(&a), 75, None, 1000, 3, 0.001).map(|f| (f.params, f.valid_mean));
        a[80] = 9.0;
        let after = fit(&series(&a), 75, None, 1000, 3, 0.001).map(|f| (f.params, f.valid_mean));
        assert_eq!(before, after, "later sessions cannot change an earlier fit");
    }

    #[test]
    fn stored_parameters_round_trip_and_are_causal() {
        let c = Connection::open_in_memory().unwrap();
        schema(&c).unwrap();
        let f = Fit {
            params: Params {
                lookback: 7,
                threshold: 0.004,
            },
            train_mean: 0.01,
            train_n: 9,
            valid_mean: 0.005,
            valid_n: 6,
            trials: 630,
            accepted: true,
        };
        save(&c, "AAA", 100, &f, 1).unwrap();
        assert_eq!(
            incumbent(&c, "AAA", 100),
            None,
            "same-day fit is not usable that day"
        );
        assert_eq!(incumbent(&c, "AAA", 101), Some(f.params));
    }
}

//! Backtest archive for the research lab (Qwen Lab): every backtest run the service makes, with its
//! individual trades, in `backtest_archive.sqlite3` beside the main store. The lab reads it strictly
//! read-only and learns from it; the bot never reads it back.
//!
//! Tables: `bt_runs` (one row per run), `bt_trades` (the closed trades of a run), `bt_daily`
//! (equity per day). Selection rule the lab relies on: runs of source `oos` (walk-forward days on
//! data the settings never saw) and `random` (settings drawn at random) keep their trades whether or
//! not the run did well, so they are an UNSELECTED sample; runs of source `chosen` (the search's
//! winners, in-sample) keep trades only when the run passed the success filter. Timestamps are
//! milliseconds. Stock bot: long only, cash only, so leverage is 1 and funding is 0.

use super::metrics::Trade;
use super::scores::Score;
use super::xs::{self, XsParams};
use super::{Market, BAR_MS};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

/// Bumped when the simulation's costs or fills change, so the lab never mixes the regimes.
pub const COST_MODEL: &str = "moomoo-measured-fees-1";

/// One backtest run to archive.
pub struct BtRun {
    /// `oos`, `random` or `chosen` (see the module note).
    pub source: &'static str,
    pub window_label: String,
    pub first_bar_ts: i64,
    pub last_bar_ts: i64,
    pub start_equity: f64,
    pub params: serde_json::Value,
    pub seed: Option<i64>,
    pub robust_score: Option<f64>,
    pub return_pct: f64,
    pub max_drawdown_pct: f64,
    pub profit_factor: f64,
    /// Passed the success filter (positive return, no liquidation).
    pub successful: bool,
    /// Each closed trade with the market and stock state at the bar that decided its entry (`entry_features`).
    pub trades: Vec<(Trade, serde_json::Value)>,
    pub daily: Vec<Daily>,
}

/// One day of a run: equity at its end, its pnl and the trades closed that day.
pub struct Daily {
    pub day_ts: i64,
    pub equity: f64,
    pub pnl: f64,
    pub trades: usize,
    pub wins: usize,
    /// Symbols traded that day with their pnl, best first.
    pub tokens: Vec<(String, f64)>,
}

pub struct BtArchive {
    conn: Mutex<Connection>,
    engine: String,
    cost_model: String,
}

const DDL: &str = "
CREATE TABLE IF NOT EXISTS bt_runs(
  id INTEGER PRIMARY KEY, service_run INTEGER NOT NULL, ts_ms INTEGER NOT NULL, source TEXT NOT NULL, window_label TEXT NOT NULL,
  first_bar_ts INTEGER NOT NULL, last_bar_ts INTEGER NOT NULL, start_equity REAL NOT NULL, params_hash TEXT NOT NULL, params_json TEXT NOT NULL,
  rank INTEGER, robust_score REAL, seed INTEGER, return_pct REAL NOT NULL, max_drawdown_pct REAL NOT NULL, trades INTEGER NOT NULL, wins INTEGER NOT NULL,
  profit_factor REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL, liquidations INTEGER NOT NULL, successful INTEGER NOT NULL,
  engine_commit TEXT, cost_model_version TEXT, tweak_id TEXT,
  UNIQUE(params_hash, source, window_label, first_bar_ts));
CREATE TABLE IF NOT EXISTS bt_trades(
  id INTEGER PRIMARY KEY, bt_id INTEGER NOT NULL REFERENCES bt_runs(id), symbol TEXT NOT NULL, entry_ts INTEGER NOT NULL, exit_ts INTEGER NOT NULL,
  entry REAL NOT NULL, exit REAL NOT NULL, qty REAL NOT NULL, leverage REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL, pnl REAL NOT NULL,
  ret_margin REAL NOT NULL, reason TEXT NOT NULL, features TEXT, decided_ts INTEGER);
CREATE INDEX IF NOT EXISTS bt_trades_run ON bt_trades(bt_id);
CREATE TABLE IF NOT EXISTS bt_daily(
  bt_id INTEGER NOT NULL REFERENCES bt_runs(id), day_ts INTEGER NOT NULL, equity REAL NOT NULL, pnl REAL NOT NULL,
  trades INTEGER NOT NULL DEFAULT 0, wins INTEGER NOT NULL DEFAULT 0, tokens TEXT NOT NULL DEFAULT '[]', PRIMARY KEY(bt_id, day_ts));";

impl BtArchive {
    /// Opens (or creates) the backtest archive database in WAL mode, labelled with the engine commit and cost model.
    pub fn open(path: &Path, engine_commit: &str) -> Result<BtArchive> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(DDL)?;
        Ok(BtArchive {
            conn: Mutex::new(conn),
            engine: engine_commit.chars().take(40).collect(),
            cost_model: COST_MODEL.into(),
        })
    }

    /// Label this archive's runs with another cost-model string, so the lab never mixes regimes
    /// (history exports built on today's stock pool use their own).
    pub fn with_cost_model(mut self, label: &str) -> BtArchive {
        self.cost_model = label.chars().take(200).collect();
        self
    }

    /// Store one run with its trades and daily rows in one transaction. A run already stored
    /// (same settings, source, window and start) is left alone. Returns whether it was new.
    pub fn record(&self, service_run: i64, now_ms: i64, run: &BtRun) -> Result<bool> {
        let params_json = serde_json::to_string(&run.params)?;
        let hash = {
            use sha1::{Digest, Sha1};
            Sha1::digest(params_json.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        let wins = run.trades.iter().filter(|(t, _)| t.pnl > 0.0).count();
        let fees: f64 = run.trades.iter().map(|(t, _)| t.fees).sum();
        let liquidations = 0;
        // Selection rule: only unselected sources keep trades of runs that did badly.
        let keep_trades = matches!(run.source, "oos" | "random") || run.successful;
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction()?;
        let n = tx.execute(
            "INSERT OR IGNORE INTO bt_runs(service_run, ts_ms, source, window_label, first_bar_ts, last_bar_ts, start_equity, params_hash, params_json, rank, robust_score, seed, \
             return_pct, max_drawdown_pct, trades, wins, profit_factor, fees, funding, liquidations, successful, engine_commit, cost_model_version) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,?10,?11,?12,?13,?14,?15,?16,?17,0,?18,?19,?20,?21)",
            params![service_run, now_ms, run.source, run.window_label, run.first_bar_ts, run.last_bar_ts, run.start_equity, hash, params_json, run.robust_score, run.seed,
                run.return_pct, run.max_drawdown_pct, run.trades.len() as i64, wins as i64, if run.profit_factor.is_finite() { run.profit_factor } else { 999.0 }, fees,
                liquidations, run.successful as i64, self.engine, self.cost_model],
        )?;
        if n == 0 {
            return Ok(false);
        }
        let id = tx.last_insert_rowid();
        if keep_trades {
            let mut s = tx.prepare(
                "INSERT INTO bt_trades(bt_id, symbol, entry_ts, exit_ts, entry, exit, qty, leverage, fees, funding, pnl, ret_margin, reason, features, decided_ts) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,1.0,?8,0.0,?9,?10,?11,?12,?13)",
            )?;
            for (t, features) in run.trades.iter().filter(|(t, _)| {
                t.exit_ts > t.entry_ts && t.entry > 0.0 && t.exit > 0.0 && t.pnl.is_finite()
            }) {
                let features = (!features.is_null()).then(|| features.to_string());
                s.execute(params![
                    id,
                    t.symbol,
                    t.entry_ts,
                    t.exit_ts,
                    t.entry,
                    t.exit,
                    t.qty,
                    t.fees,
                    t.pnl,
                    t.r,
                    t.reason,
                    features,
                    t.decided_ts
                ])?;
            }
        }
        for d in &run.daily {
            tx.execute(
                "INSERT OR IGNORE INTO bt_daily(bt_id, day_ts, equity, pnl, trades, wins, tokens) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![id, d.day_ts, d.equity, d.pnl, d.trades as i64, d.wins as i64, serde_json::to_string(&d.tokens)?],
            )?;
        }
        tx.commit()?;
        Ok(true)
    }
}

/// The market and stock state at the bar whose close DECIDED this entry (closed bars only), as the
/// research lab's features: the stock's own signal, returns, distance from its 24h average, ATR and
/// volume; the reference ETF's trend test; market-wide dispersion and breadth. Uses the decision bar
/// the engine recorded on the trade (a wick-guard delay does not move it); a trade without one falls
/// back to its entry bar minus two.
pub fn entry_features(
    m: &Market,
    closes: &[i64],
    sc: &[Vec<Option<Score>>],
    p: &XsParams,
    t: &Trade,
    cache: &mut std::collections::HashMap<usize, serde_json::Value>,
) -> serde_json::Value {
    let decision = t
        .decided_ts
        .and_then(|ts| m.ts.binary_search(&ts).ok())
        .or_else(|| {
            m.ts.binary_search(&t.entry_ts)
                .ok()
                .and_then(|e| e.checked_sub(2))
        });
    let Some(d) = decision else {
        return serde_json::Value::Null;
    };
    let ctx = cache
        .entry(d)
        .or_insert_with(|| xs::market_context(m, sc, d, p));
    let coin = ctx["coins"]
        .as_array()
        .and_then(|c| c.iter().find(|x| x["symbol"] == t.symbol.as_str()).cloned())
        .unwrap_or(serde_json::Value::Null);
    // Time of the deciding close in US Eastern time (the session shifts with daylight saving in UTC).
    let et = closes
        .get(d)
        .and_then(|c| chrono::DateTime::from_timestamp_millis(*c))
        .map(|u| u.with_timezone(&chrono_tz::America::New_York));
    serde_json::json!({"decision_bar_close_ms": closes.get(d), "coin": coin,
        "et_minute_of_day": et.map(|e| chrono::Timelike::hour(&e) * 60 + chrono::Timelike::minute(&e)),
        "et_weekday": et.map(|e| chrono::Datelike::weekday(&e).num_days_from_sunday()),
        "trend": ctx["trend"], "zones": ctx["zones"], "breadth_pct": ctx["breadth_pct_above_own_24h_avg"]})
}

/// A trading-clock time (`Market::ts`) as real UTC ms: the open of the bar it names, or the close
/// of the last bar when it is the end-of-test marker one bar past the data.
pub fn clock_wall(m: &Market, closes: &[i64], c: i64) -> Option<i64> {
    match m.ts.binary_search(&c) {
        Ok(i) => m.wall.get(i).copied(),
        Err(_) => {
            m.ts.binary_search(&(c - BAR_MS))
                .ok()
                .and_then(|i| closes.get(i).copied())
        }
    }
}

/// The trade with its clock times (entry, exit, deciding bar) converted to real UTC ms, for the
/// research archive; None when a time cannot be mapped (such a trade is left out, never guessed).
pub fn wall_trade(m: &Market, closes: &[i64], t: &Trade) -> Option<Trade> {
    let mut w = t.clone();
    w.entry_ts = clock_wall(m, closes, t.entry_ts)?;
    w.exit_ts = clock_wall(m, closes, t.exit_ts)?;
    w.decided_ts = match t.decided_ts {
        Some(d) => Some(clock_wall(m, closes, d)?),
        None => None,
    };
    Some(w)
}

/// Symbols traded with their summed pnl, best first.
pub fn day_tokens(trades: &[(Trade, serde_json::Value)]) -> Vec<(String, f64)> {
    let mut by: BTreeMap<String, f64> = BTreeMap::new();
    for (t, _) in trades {
        *by.entry(t.symbol.clone()).or_default() += t.pnl;
    }
    let mut v: Vec<(String, f64)> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

/// A run's parameters as JSON for the archive.
pub fn params_value<P: Serialize>(p: &P) -> serde_json::Value {
    serde_json::to_value(p).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Side;

    /// Test fixture: one closed long AAPL trade with net PnL `pnl`.
    fn trade(pnl: f64) -> Trade {
        Trade {
            symbol: "AAPL".into(),
            side: Side::Long,
            entry_ts: 1_000,
            exit_ts: 2_000,
            entry: 100.0,
            exit: 100.0 + pnl,
            qty: 1.0,
            leverage: 1.0,
            fees: 1.0,
            funding: 0.0,
            pnl,
            r: pnl / 100.0,
            reason: "Take profit".into(),
            decided_ts: Some(900),
        }
    }

    /// Test fixture: a backtest run of `source` with return `ret`, successful or not, holding one trade.
    fn run(source: &'static str, ret: f64, successful: bool) -> BtRun {
        BtRun {
            source,
            window_label: "1h-21s".into(),
            first_bar_ts: 10,
            last_bar_ts: 20,
            start_equity: 1000.0,
            params: serde_json::json!({"hold": 2}),
            seed: Some(1),
            robust_score: None,
            return_pct: ret,
            max_drawdown_pct: 1.0,
            profit_factor: f64::INFINITY,
            successful,
            trades: vec![(
                trade(ret),
                serde_json::json!({"coin": {"ret_24h_pct": -3.0}}),
            )],
            daily: vec![Daily {
                day_ts: 10,
                equity: 1000.0 + ret,
                pnl: ret,
                trades: 1,
                wins: usize::from(ret > 0.0),
                tokens: vec![("AAPL".into(), ret)],
            }],
        }
    }

    /// Test fixture: a fresh archive in a temporary folder named after the test.
    fn archive(name: &str) -> (BtArchive, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("moomoo-btarchive-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (
            BtArchive::open(&dir.join("bt.sqlite3"), "test").unwrap(),
            dir,
        )
    }

    /// Test helper: the single number returned by `sql` on the archive.
    fn count(a: &BtArchive, sql: &str) -> i64 {
        a.conn
            .lock()
            .unwrap()
            .query_row(sql, [], |r| r.get(0))
            .unwrap()
    }

    /// Test: unselected sources keep trades of losing runs and chosen only when successful.
    #[test]
    fn unselected_sources_keep_trades_of_losing_runs_and_chosen_only_when_successful() {
        let (a, dir) = archive("selection");
        assert!(a.record(1, 5, &run("oos", -2.0, false)).unwrap());
        assert!(a
            .record(
                1,
                5,
                &BtRun {
                    first_bar_ts: 11,
                    ..run("random", -3.0, false)
                }
            )
            .unwrap());
        assert!(a
            .record(
                1,
                5,
                &BtRun {
                    first_bar_ts: 12,
                    ..run("chosen", -1.0, false)
                }
            )
            .unwrap());
        assert!(a
            .record(
                1,
                5,
                &BtRun {
                    first_bar_ts: 13,
                    ..run("chosen", 4.0, true)
                }
            )
            .unwrap());
        assert_eq!(count(&a, "SELECT count(*) FROM bt_runs"), 4);
        // oos + random (both losing) + the successful chosen run keep trades; the failed chosen run is header-only.
        assert_eq!(count(&a, "SELECT count(*) FROM bt_trades"), 3);
        assert_eq!(count(&a, "SELECT count(*) FROM bt_trades WHERE features LIKE '%ret_24h_pct%' AND decided_ts = 900"), 3);
        assert_eq!(count(&a, "SELECT count(*) FROM bt_trades t JOIN bt_runs r ON r.id = t.bt_id WHERE r.source = 'chosen' AND NOT r.successful"), 0);
        assert_eq!(count(&a, "SELECT count(*) FROM bt_daily"), 4);
        // Stock bot: no leverage, no funding; an infinite profit factor is stored as a finite number.
        assert_eq!(
            count(
                &a,
                "SELECT count(*) FROM bt_trades WHERE leverage != 1.0 OR funding != 0.0"
            ),
            0
        );
        assert_eq!(
            count(
                &a,
                "SELECT count(*) FROM bt_runs WHERE profit_factor > 1000"
            ),
            0
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Test: the same settings source window and start are stored once.
    #[test]
    fn the_same_settings_source_window_and_start_are_stored_once() {
        let (a, dir) = archive("dedupe");
        assert!(a.record(1, 5, &run("oos", 1.0, true)).unwrap());
        assert!(!a.record(2, 9, &run("oos", 1.0, true)).unwrap());
        assert_eq!(count(&a, "SELECT count(*) FROM bt_trades"), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! Forward record from the moomoo US-stock bot (paper account, USD, long only, whole shares, no funding).
//!
//! The moomoo bot keeps one SQLite store (`moomoo.sqlite3`). This reads it strictly read-only and copies its paper
//! tables into the same lab tables the crypto importer fills, so everything downstream (sufficiency ladder, prompt
//! block, search-decay check, dashboard) is shared. Mapping: leverage is 1, funding is 0, and the return per trade is
//! `pnl / (entry * qty)` (the stock bot has no margin). Timestamps are milliseconds, as in the lab.

use crate::config::Config;
use crate::paper::{clean, counter, finish, pct, scrub_json, set_counter, PaperReport};
use crate::util::sha256_str;
use anyhow::Result;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::collections::HashMap;

/// What the stock bot's store must contain for the import (extra columns are ignored).
const REQUIRED: &[(&str, &[&str])] = &[
    ("runs", &["run_id", "started_ms", "note"]),
    ("paper", &["run_id", "wall_ms", "equity", "start_equity", "realized", "open_pnl", "max_drawdown_pct", "open_positions", "closed_trades", "wins", "params", "mode"]),
    ("paper_trades", &["run_id", "symbol", "side", "entry_ms", "exit_ms", "entry", "exit", "qty", "pnl", "fees", "reason"]),
    ("searches", &["run_id", "finished_ms", "json"]),
];

/// Column names of `table`.
fn columns(c: &Connection, table: &str) -> Result<Vec<String>> {
    let mut s = c.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let v = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// Checks the moomoo store has every required table and column; returns what is missing.
fn check_schema(c: &Connection) -> std::result::Result<(), String> {
    for (table, cols) in REQUIRED {
        let have = columns(c, table).map_err(|e| e.to_string())?;
        if have.is_empty() { return Err(format!("table `{table}` is missing")); }
        for col in *cols {
            if !have.iter().any(|h| h == col) { return Err(format!("`{table}.{col}` is missing")); }
        }
    }
    Ok(())
}

/// Stable positive id for a parameter set: the first 6 bytes of its hash (the stock store has no params table).
fn params_id(json: &str) -> i64 {
    i64::from_str_radix(&sha256_str(json)[..12], 16).unwrap_or(0)
}

/// Imports the moomoo bot's paper record (equity, trades, searches) read-only into the lab's paper tables.
pub fn import(cfg: &Config, mem: &Connection, data: &Connection, now_ms: i64, mut rep: PaperReport) -> Result<PaperReport> {
    let path = &cfg.paper.archive_path;
    if !path.exists() {
        rep.source = "absent".into();
        return finish(cfg, data, rep, now_ms);
    }
    let src = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) {
        Ok(c) => c,
        Err(e) => { rep.source = "locked".into(); rep.note = format!("cannot open archive: {e}"); return finish(cfg, data, rep, now_ms); }
    };
    src.busy_timeout(std::time::Duration::from_secs(5))?;
    if let Err(e) = src.execute_batch("BEGIN") { rep.source = "locked".into(); rep.note = format!("cannot read archive: {e}"); return finish(cfg, data, rep, now_ms); }
    if let Err(why) = check_schema(&src) { rep.source = "schema_mismatch".into(); rep.note = why; return finish(cfg, data, rep, now_ms); }
    rep.source = "ok".into();

    let first_started: Option<i64> = src.query_row("SELECT min(started_ms) FROM runs", [], |r| r.get(0))?;
    let max_trade: i64 = src.query_row("SELECT coalesce(max(rowid),0) FROM paper_trades", [], |r| r.get(0))?;
    let max_search: i64 = src.query_row("SELECT coalesce(max(rowid),0) FROM searches", [], |r| r.get(0))?;
    let max_eq: i64 = src.query_row("SELECT coalesce(max(rowid),0) FROM paper", [], |r| r.get(0))?;
    let (wm_t, wm_s, wm_e) = (counter(mem, "paper_wm_trades")?.unwrap_or(0), counter(mem, "paper_wm_searches")?.unwrap_or(0), counter(mem, "paper_wm_equity")?.unwrap_or(0));
    let stored_first = counter(mem, "paper_first_started")?;
    let reset = (stored_first.is_some() && first_started.is_some() && stored_first != first_started) || max_trade < wm_t || max_search < wm_s || max_eq < wm_e;
    let (epoch, wm_t, wm_s, wm_e) = if reset { rep.epoch_reset = true; (rep.epoch + 1, 0, 0, 0) } else { (rep.epoch, wm_t, wm_s, wm_e) };
    rep.epoch = epoch;

    let mut start_eq: HashMap<i64, f64> = HashMap::new();
    {
        let mut s = src.prepare("SELECT run_id, start_equity FROM paper p WHERE wall_ms = (SELECT min(wall_ms) FROM paper WHERE run_id = p.run_id)")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))? { let (id, v) = r?; start_eq.insert(id, v); }
    }

    let tx = data.unchecked_transaction()?;
    {
        let mut s = src.prepare("SELECT run_id, started_ms, coalesce(note,'') FROM runs")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))? {
            let (id, st, note) = r?;
            tx.execute("INSERT OR IGNORE INTO paper_runs(epoch, run_id, started_ms, note) VALUES (?1,?2,?3,?4)", params![epoch, id, st, clean(&note)?])?;
        }

        // Equity snapshots (one per bar) and the settings in force at each.
        let mut s = src.prepare("SELECT rowid, run_id, wall_ms, equity, start_equity, realized, open_pnl, max_drawdown_pct, open_positions, closed_trades, wins, params, coalesce(mode,'') FROM paper WHERE rowid > ?1 ORDER BY rowid")?;
        let mut rows = s.query([wm_e])?;
        while let Some(r) = rows.next()? {
            let (eq, base, realized, open): (f64, f64, f64, f64) = (r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
            let (pjson, wall): (Option<String>, i64) = (r.get(11)?, r.get(2)?);
            let pid = match pjson.filter(|j| !j.is_empty() && j != "null") {
                Some(j) => {
                    scrub_json(&j)?;
                    let id = params_id(&j);
                    tx.execute("INSERT OR IGNORE INTO paper_params(epoch, id, params_hash, json, first_used_ms) VALUES (?1,?2,?3,?4,?5)", params![epoch, id, sha256_str(&j), j, wall])?;
                    Some(id)
                }
                None => None,
            };
            let b = Some(base);
            tx.execute(
                "INSERT OR IGNORE INTO paper_equity(epoch, run_id, ts_ms, equity, start_equity, realized, open_pnl, equity_index, realized_pct, open_pnl_pct, max_drawdown_pct, open_positions, closed_trades, wins, params_id, mode) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![epoch, r.get::<_, i64>(1)?, wall, eq, base, realized, open, pct(eq, b), pct(realized, b), pct(open, b),
                    r.get::<_, f64>(7)?, r.get::<_, i64>(8)?, r.get::<_, i64>(9)?, r.get::<_, i64>(10)?, pid, clean(&r.get::<_, String>(12)?)?],
            )?;
            rep.new_equity += 1;
        }

        // Closed trades. Long only, cash only: leverage 1, no funding.
        let mut s = src.prepare("SELECT rowid, run_id, symbol, side, entry_ms, exit_ms, entry, exit, qty, pnl, fees, coalesce(reason,''), json FROM paper_trades WHERE rowid > ?1 ORDER BY rowid")?;
        let mut rows = s.query([wm_t])?;
        while let Some(r) = rows.next()? {
            let run: i64 = r.get(1)?;
            let base = start_eq.get(&run).copied();
            let (entry, qty, pnl, fees): (f64, f64, f64, f64) = (r.get(6)?, r.get(8)?, r.get(9)?, r.get(10)?);
            if !(entry > 0.0 && qty > 0.0 && [pnl, fees].iter().all(|x| x.is_finite())) { continue; }
            let ret = pnl / (entry * qty);
            // Entry-state features and the deciding bar, when the bot recorded them in the trade's json.
            let extra: Option<Value> = r.get::<_, Option<String>>(12)?.and_then(|j| serde_json::from_str(&j).ok());
            let features_text: Option<String> = match extra.as_ref().and_then(|e| e.get("features")).filter(|f| !f.is_null()) {
                Some(f) => { let t = f.to_string(); scrub_json(&t)?; Some(t.chars().take(20_000).collect()) }
                None => None,
            };
            let decided_ts = extra.as_ref().and_then(|e| e.get("decided_wall_ms")).and_then(|x| x.as_i64()).map(|d| d.div_euclid(1000));
            tx.execute(
                "INSERT OR IGNORE INTO paper_trades(epoch, id, run_id, symbol, side, entry_ts, exit_ts, entry, exit, qty, leverage, pnl, fees, funding, pnl_pct, fees_pct, funding_pct, ret_margin, reason, decided_ts, features_json) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1.0,?11,?12,0.0,?13,?14,0.0,?15,?16,?17,?18)",
                params![epoch, r.get::<_, i64>(0)?, run, clean(&r.get::<_, String>(2)?)?, clean(&r.get::<_, String>(3)?)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?,
                    entry, r.get::<_, f64>(7)?, qty, pnl, fees, pct(pnl, base), pct(fees, base), ret, clean(&r.get::<_, String>(11)?)?, decided_ts, features_text],
            )?;
            rep.new_trades += 1;
        }

        // Search results: the stock bot stores one walk-forward summary per interval and search.
        let mut s = src.prepare("SELECT rowid, run_id, finished_ms, json FROM searches WHERE rowid > ?1 ORDER BY rowid")?;
        let mut rows = s.query([wm_s])?;
        while let Some(r) = rows.next()? {
            let raw: String = r.get(3)?;
            let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
            let Some(w) = v.get("walk_forward") else { continue };
            let last = w.get("segments").and_then(|s| s.as_array()).and_then(|a| a.last());
            let chosen = last.and_then(|s| s.get("params")).cloned().unwrap_or(Value::Null);
            let settings = serde_json::to_string(&chosen)?;
            scrub_json(&settings)?;
            let f = |k: &str| w.get(k).and_then(|x| x.as_f64());
            tx.execute(
                "INSERT OR IGNORE INTO paper_searches(epoch, id, run_id, ts_ms, choice, signal, hold, top, lookback, start_equity, end_equity, return_pct, max_drawdown_pct, trades, wins, liquidations, settings_json) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,0,?16)",
                params![epoch, r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                    w.get("interval_label").and_then(|x| x.as_str()).map(|s| clean(s).unwrap_or_default()),
                    chosen.get("signal").and_then(|x| x.as_str()).map(|s| clean(s).unwrap_or_default()),
                    chosen.get("hold").and_then(|x| x.as_i64()), chosen.get("top").and_then(|x| x.as_i64()), chosen.get("lookback").and_then(|x| x.as_i64()),
                    f("start_equity"), f("end_equity"), f("return_pct"), f("max_drawdown_pct"), f("trades").map(|x| x as i64), f("wins").map(|x| x as i64), settings],
            )?;
            rep.new_searches += 1;
        }
    }
    tx.commit()?;
    src.execute_batch("COMMIT")?;
    set_counter(mem, "paper_epoch", epoch)?;
    set_counter(mem, "paper_wm_trades", max_trade)?;
    set_counter(mem, "paper_wm_searches", max_search)?;
    set_counter(mem, "paper_wm_equity", max_eq)?;
    if let Some(f) = first_started { set_counter(mem, "paper_first_started", f)?; }
    finish(cfg, data, rep, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    const DDL: &str = "
        CREATE TABLE runs(run_id INTEGER PRIMARY KEY, started_ms INTEGER, note TEXT);
        CREATE TABLE paper(run_id INTEGER NOT NULL, wall_ms INTEGER NOT NULL, equity REAL, start_equity REAL, realized REAL, open_pnl REAL,
          max_drawdown_pct REAL, open_positions INTEGER, closed_trades INTEGER, wins INTEGER, params TEXT, mode TEXT, positions TEXT, PRIMARY KEY(run_id, wall_ms));
        CREATE TABLE paper_trades(run_id INTEGER NOT NULL, symbol TEXT, side TEXT, entry_ms INTEGER, exit_ms INTEGER, entry REAL, exit REAL, qty REAL,
          pnl REAL, fees REAL, reason TEXT, json TEXT, PRIMARY KEY(run_id, symbol, entry_ms, exit_ms));
        CREATE TABLE searches(run_id INTEGER, finished_ms INTEGER, json TEXT);";

    /// Test: imports stock paper record in usd without leverage or funding and is incremental.
    #[test]
    fn imports_stock_paper_record_in_usd_without_leverage_or_funding_and_is_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_root(dir.path(), "real");
        cfg.paper.source = "moomoo".into();
        cfg.paper.currency = "USD".into();
        cfg.paper.archive_path = dir.path().join("moomoo.sqlite3");
        let mem = db::open(&cfg.memory_db(), true).unwrap();
        let data = db::open(&cfg.data_db(), false).unwrap();
        db::migrate_memory(&mem).unwrap();
        db::migrate_data(&data).unwrap();
        let a = Connection::open(&cfg.paper.archive_path).unwrap();
        a.pragma_update(None, "journal_mode", "WAL").unwrap();
        a.execute_batch(DDL).unwrap();
        a.execute("INSERT INTO runs VALUES (7, 1000, 'service start')", []).unwrap();
        a.execute("INSERT INTO paper VALUES (7, 2000, 1010.0, 1000.0, 10.0, 0.0, 0.5, 0, 1, 1, '{\"signal\":\"Volatility\",\"hold\":2,\"top\":4}', 'TRADING', '[]')", []).unwrap();
        a.execute("INSERT INTO paper_trades VALUES (7, 'AAPL', 'long', 1500, 1900, 100.0, 110.0, 1.0, 10.0, 1.99, 'Take profit', NULL)", []).unwrap();
        a.execute("INSERT INTO searches VALUES (7, 2000, '{\"walk_forward\":{\"interval_label\":\"1h\",\"start_equity\":1000.0,\"end_equity\":1010.0,\"return_pct\":1.0,\"max_drawdown_pct\":0.5,\"trades\":1,\"wins\":1,\"segments\":[{\"params\":{\"signal\":\"CalmDip\",\"hold\":2,\"top\":3}}]}}')", []).unwrap();

        let rep = crate::paper::import(&cfg, &mem, &data, 3000).unwrap();
        assert_eq!((rep.source.as_str(), rep.new_trades, rep.new_equity, rep.new_searches), ("ok", 1, 1, 1));
        let (lev, funding, ret, pct_): (f64, f64, f64, f64) = data
            .query_row("SELECT leverage, funding, ret_margin, pnl_pct FROM paper_trades", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        assert_eq!((lev, funding), (1.0, 0.0));
        assert!((ret - 0.1).abs() < 1e-9 && (pct_ - 1.0).abs() < 1e-9, "return {ret}, pct {pct_}");
        let (signal, top): (String, i64) = data.query_row("SELECT signal, top FROM paper_searches", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((signal.as_str(), top), ("CalmDip", 3));
        assert!(crate::paper::prompt_block(&cfg, &data, &rep).unwrap().contains("1010.00 USD"));

        // A second import copies nothing again; a new trade is picked up alone.
        let again = crate::paper::import(&cfg, &mem, &data, 3000).unwrap();
        assert_eq!((again.new_trades, again.new_equity, again.new_searches), (0, 0, 0));
        a.execute("INSERT INTO paper_trades VALUES (7, 'MSFT', 'long', 2100, 2200, 50.0, 49.0, 2.0, -2.0, 1.99, 'Stop', NULL)", []).unwrap();
        assert_eq!(crate::paper::import(&cfg, &mem, &data, 3000).unwrap().new_trades, 1);
    }
}

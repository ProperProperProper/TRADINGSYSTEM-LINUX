//! The two SQLite databases: `memory.sqlite3` (small, permanent) and `data.sqlite3` (large, culled).

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Block size (seconds) used to cut independent windows; fixed per lab at the first archive import.
pub fn block_secs(c: &Connection) -> i64 {
    c.query_row("SELECT value FROM lab_meta WHERE key = 'block_secs'", [], |r| r.get::<_, String>(0)).ok().and_then(|v| v.parse().ok()).unwrap_or(7 * 86_400)
}

/// A feature name is `[a-z][a-z0-9_]{0,40}`: it becomes a column name (`f_<name>`), so it is validated before any SQL.
pub fn valid_feature_name(n: &str) -> bool {
    let mut ch = n.chars();
    n.len() <= 41 && ch.next().is_some_and(|c| c.is_ascii_lowercase()) && ch.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// All registered feature names (dynamic: new features appear when the bot starts exporting them).
pub fn registry(c: &Connection) -> Result<Vec<String>> {
    let mut s = c.prepare("SELECT name FROM feature_registry ORDER BY name")?;
    let v = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// Register a feature and add its column the first time it is seen. Existing lessons keep pointing at the old name.
pub fn ensure_feature(c: &Connection, name: &str, source: &str) -> Result<()> {
    anyhow::ensure!(valid_feature_name(name), "invalid feature name `{name}`");
    let known: bool = c.query_row("SELECT count(*) FROM feature_registry WHERE name = ?1", [name], |r| r.get::<_, i64>(0)).map(|n| n > 0)?;
    if known { return Ok(()); }
    let has_col: bool = c.query_row("SELECT count(*) FROM pragma_table_info('trade_features') WHERE name = ?1", [format!("f_{name}")], |r| r.get::<_, i64>(0)).map(|n| n > 0)?;
    if !has_col { c.execute_batch(&format!("ALTER TABLE trade_features ADD COLUMN f_{name} REAL"))?; }
    c.execute("INSERT OR IGNORE INTO feature_registry(name, first_seen, source) VALUES (?1, strftime('%s','now'), ?2)", (name, source))?;
    Ok(())
}

/// Opens (or creates) a SQLite database in WAL mode with foreign keys and a 30 s busy timeout.
pub fn open(path: &Path, synchronous_full: bool) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", if synchronous_full { "FULL" } else { "NORMAL" })?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    Ok(conn)
}

/// Creates or upgrades the memory database schema.
pub fn migrate_memory(conn: &Connection) -> Result<()> {
    conn.execute_batch(MEMORY_DDL)?;
    set_version(conn, "memory", "1")
}

/// Column names of `table`.
fn columns_of(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut s = conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let v = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// Adds `column` to `table` if the table exists and lacks it.
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let cols = columns_of(conn, table)?;
    if !cols.is_empty() && !cols.iter().any(|c| c == column) {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
    }
    Ok(())
}

/// Create or upgrade `data.sqlite3`. Returns true when the paper copy tables had an older layout and were rebuilt (they are
/// only a copy of the bot's archive, so the caller resets the import watermarks and the next import repopulates them).
pub fn migrate_data(conn: &Connection) -> Result<bool> {
    // The paper tables hold a COPY of the bot's archive: if their layout is older than the current one, rebuild them.
    let mut rebuilt = false;
    for (table, needle) in [("paper_trades", "qty"), ("paper_equity", "start_equity"), ("paper_searches", "start_equity")] {
        let cols = columns_of(conn, table)?;
        if !cols.is_empty() && !cols.iter().any(|c| c == needle) { rebuilt = true; }
    }
    if rebuilt { conn.execute_batch("DROP TABLE IF EXISTS paper_runs; DROP TABLE IF EXISTS paper_params; DROP TABLE IF EXISTS paper_trades; DROP TABLE IF EXISTS paper_equity; DROP TABLE IF EXISTS paper_searches;")?; }
    conn.execute_batch(DATA_DDL)?;
    // Additive upgrades of tables created by earlier versions.
    add_column_if_missing(conn, "trades", "window", "TEXT NOT NULL DEFAULT ''")?;
    add_column_if_missing(conn, "runs", "selection", "TEXT NOT NULL DEFAULT 'none'")?;
    add_column_if_missing(conn, "runs", "source_label", "TEXT")?;
    add_column_if_missing(conn, "runs", "run_key", "TEXT")?;
    add_column_if_missing(conn, "runs", "tweak_id", "TEXT")?;
    add_column_if_missing(conn, "trades", "decided_ts", "INTEGER")?;
    add_column_if_missing(conn, "paper_trades", "decided_ts", "INTEGER")?;
    add_column_if_missing(conn, "paper_trades", "features_json", "TEXT")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS trades_window ON trades(window);")?;
    // Feature columns are added on demand by `ensure_feature`.
    conn.execute_batch("CREATE TABLE IF NOT EXISTS trade_features (run_id TEXT NOT NULL, idx INTEGER NOT NULL, PRIMARY KEY (run_id, idx));")?;
    set_version(conn, "data", "2")?;
    Ok(rebuilt)
}

/// Records the schema version of a component.
fn set_version(conn: &Connection, component: &str, version: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO schema_versions(component, version, applied) VALUES (?1, ?2, strftime('%s','now')) \
         ON CONFLICT(component) DO UPDATE SET version = excluded.version",
        (component, version),
    )?;
    Ok(())
}

/// Database size in bytes including the WAL.
pub fn size_bytes(path: &Path) -> u64 {
    let len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let wal = path.with_extension("sqlite3-wal");
    len(path) + len(&wal)
}

const MEMORY_DDL: &str = "
CREATE TABLE IF NOT EXISTS schema_versions (component TEXT PRIMARY KEY, version TEXT NOT NULL, applied INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS aggregates (id INTEGER PRIMARY KEY, batch_id INTEGER, spec_json TEXT NOT NULL,
  content_json TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ingest_log (sha256 TEXT PRIMARY KEY, file TEXT NOT NULL, run_id TEXT, ingested_at INTEGER NOT NULL,
  status TEXT NOT NULL, reason TEXT);
CREATE TABLE IF NOT EXISTS cull_log (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, bytes_before INTEGER, bytes_after INTEGER,
  batches_removed INTEGER, rows_removed INTEGER, tiers TEXT, dry_run INTEGER NOT NULL, duration_ms INTEGER);
";

const DATA_DDL: &str = "
CREATE TABLE IF NOT EXISTS schema_versions (component TEXT PRIMARY KEY, version TEXT NOT NULL, applied INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS lab_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS feature_registry (name TEXT PRIMARY KEY, first_seen INTEGER NOT NULL, source TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS ingest_batches (id INTEGER PRIMARY KEY, ingested_at INTEGER NOT NULL, files INTEGER NOT NULL,
  bytes INTEGER NOT NULL, runs INTEGER NOT NULL, distilled INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS runs (
  run_id TEXT PRIMARY KEY, batch INTEGER NOT NULL REFERENCES ingest_batches(id), engine_commit TEXT NOT NULL,
  window_label TEXT NOT NULL, window_start INTEGER NOT NULL, window_end INTEGER NOT NULL, data_fingerprint TEXT NOT NULL,
  cost_model_version TEXT NOT NULL, role TEXT NOT NULL, rank INTEGER, params_hash TEXT NOT NULL, params_json TEXT NOT NULL,
  metrics_json TEXT NOT NULL, return_pct REAL, degraded INTEGER NOT NULL DEFAULT 0, seed INTEGER, created INTEGER NOT NULL,
  trades_dropped INTEGER NOT NULL DEFAULT 0, features_dropped INTEGER NOT NULL DEFAULT 0,
  selection TEXT NOT NULL DEFAULT 'none', source_label TEXT, run_key TEXT, tweak_id TEXT);
CREATE INDEX IF NOT EXISTS runs_window ON runs(window_label);
CREATE INDEX IF NOT EXISTS runs_batch ON runs(batch);
CREATE TABLE IF NOT EXISTS trades (
  run_id TEXT NOT NULL REFERENCES runs(run_id), idx INTEGER NOT NULL, symbol TEXT NOT NULL, side TEXT NOT NULL,
  entry_ts INTEGER NOT NULL, exit_ts INTEGER NOT NULL, entry_px REAL NOT NULL, exit_px REAL NOT NULL, qty REAL NOT NULL,
  margin REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL, pnl REAL NOT NULL, ret_margin REAL NOT NULL,
  reason TEXT NOT NULL, day INTEGER NOT NULL, window TEXT NOT NULL DEFAULT '', decided_ts INTEGER, PRIMARY KEY (run_id, idx));
CREATE INDEX IF NOT EXISTS trades_day ON trades(day);
CREATE TABLE IF NOT EXISTS daily (run_id TEXT NOT NULL REFERENCES runs(run_id), day INTEGER NOT NULL, equity REAL NOT NULL,
  pnl REAL NOT NULL, PRIMARY KEY (run_id, day));
-- Forward record copied from the bot's paper archive (spec 4.3). Both the absolute paper amounts (USDT, as the bot
-- recorded them; user instruction 2026-10-05) and their percentages of the run's starting equity are stored.
-- Credentials are never stored. Protected from the cull.
CREATE TABLE IF NOT EXISTS paper_runs (epoch INTEGER NOT NULL, run_id INTEGER NOT NULL, started_ms INTEGER NOT NULL, note TEXT NOT NULL,
  PRIMARY KEY (epoch, run_id));
CREATE TABLE IF NOT EXISTS paper_params (epoch INTEGER NOT NULL, id INTEGER NOT NULL, params_hash TEXT NOT NULL, json TEXT NOT NULL,
  first_used_ms INTEGER NOT NULL, PRIMARY KEY (epoch, id));
CREATE TABLE IF NOT EXISTS paper_trades (epoch INTEGER NOT NULL, id INTEGER NOT NULL, run_id INTEGER NOT NULL, symbol TEXT NOT NULL,
  side TEXT NOT NULL, entry_ts INTEGER NOT NULL, exit_ts INTEGER NOT NULL, entry REAL NOT NULL, exit REAL NOT NULL, qty REAL NOT NULL,
  leverage REAL NOT NULL, pnl REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL, pnl_pct REAL, fees_pct REAL, funding_pct REAL, ret_margin REAL NOT NULL, reason TEXT NOT NULL, params_id INTEGER,
  decided_ts INTEGER, features_json TEXT, PRIMARY KEY (epoch, id));
CREATE TABLE IF NOT EXISTS paper_equity (epoch INTEGER NOT NULL, run_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL, equity REAL NOT NULL,
  start_equity REAL NOT NULL, realized REAL NOT NULL, open_pnl REAL NOT NULL, equity_index REAL,
  realized_pct REAL, open_pnl_pct REAL, max_drawdown_pct REAL NOT NULL, open_positions INTEGER NOT NULL, closed_trades INTEGER NOT NULL,
  wins INTEGER NOT NULL, params_id INTEGER, mode TEXT NOT NULL, PRIMARY KEY (epoch, run_id, ts_ms));
CREATE TABLE IF NOT EXISTS paper_searches (epoch INTEGER NOT NULL, id INTEGER NOT NULL, run_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL,
  choice TEXT, signal TEXT, hold INTEGER, top INTEGER, lookback INTEGER, seed TEXT, evaluated INTEGER, raw_best_score REAL,
  robust_score REAL, start_equity REAL, end_equity REAL, return_pct REAL, max_drawdown_pct REAL, trades INTEGER, wins INTEGER, liquidations INTEGER, settings_json TEXT,
  PRIMARY KEY (epoch, id));
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: an older database is upgraded in place without losing rows.
    #[test]
    fn an_older_database_is_upgraded_in_place_without_losing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let d = open(&dir.path().join("d.sqlite3"), false).unwrap();
        // the layout written by the first version of the lab
        d.execute_batch("
            CREATE TABLE ingest_batches (id INTEGER PRIMARY KEY, ingested_at INTEGER NOT NULL, files INTEGER NOT NULL, bytes INTEGER NOT NULL, runs INTEGER NOT NULL, distilled INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE runs (run_id TEXT PRIMARY KEY, batch INTEGER NOT NULL, engine_commit TEXT NOT NULL, window_label TEXT NOT NULL, window_start INTEGER NOT NULL, window_end INTEGER NOT NULL, data_fingerprint TEXT NOT NULL, cost_model_version TEXT NOT NULL, role TEXT NOT NULL, rank INTEGER, params_hash TEXT NOT NULL, params_json TEXT NOT NULL, metrics_json TEXT NOT NULL, return_pct REAL, degraded INTEGER NOT NULL DEFAULT 0, seed INTEGER, created INTEGER NOT NULL, trades_dropped INTEGER NOT NULL DEFAULT 0, features_dropped INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE trades (run_id TEXT NOT NULL, idx INTEGER NOT NULL, symbol TEXT NOT NULL, side TEXT NOT NULL, entry_ts INTEGER NOT NULL, exit_ts INTEGER NOT NULL, entry_px REAL NOT NULL, exit_px REAL NOT NULL, qty REAL NOT NULL, margin REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL, pnl REAL NOT NULL, ret_margin REAL NOT NULL, reason TEXT NOT NULL, day INTEGER NOT NULL, PRIMARY KEY (run_id, idx));
            CREATE TABLE paper_equity (epoch INTEGER NOT NULL, run_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL, equity_index REAL, realized_pct REAL, open_pnl_pct REAL, max_drawdown_pct REAL NOT NULL, open_positions INTEGER NOT NULL, closed_trades INTEGER NOT NULL, wins INTEGER NOT NULL, params_id INTEGER, mode TEXT NOT NULL, PRIMARY KEY (epoch, run_id, ts_ms));
            INSERT INTO ingest_batches VALUES (1, 0, 0, 0, 1, 0, 0);
            INSERT INTO runs(run_id, batch, engine_commit, window_label, window_start, window_end, data_fingerprint, cost_model_version, role, params_hash, params_json, metrics_json, created) VALUES ('r1',1,'c','w1',0,1,'f','m','study','h','{}','{}',0);
            INSERT INTO trades VALUES ('r1',0,'AAA','long',10,20,1,1,1,1,0,0,0,0,'x',0);
        ").unwrap();
        let rebuilt = migrate_data(&d).unwrap();
        assert!(rebuilt, "the old percent-only paper layout must be rebuilt");
        assert_eq!(d.query_row::<i64, _, _>("SELECT count(*) FROM trades", [], |r| r.get(0)).unwrap(), 1, "real rows survive");
        let (w, sel): (String, String) = d.query_row("SELECT (SELECT window FROM trades), (SELECT selection FROM runs)", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((w.as_str(), sel.as_str()), ("", "none"));
        assert!(columns_of(&d, "paper_equity").unwrap().iter().any(|c| c == "start_equity"));
        assert!(!migrate_data(&d).unwrap(), "a second migration changes nothing");
    }

    /// Test: migrations are idempotent.
    #[test]
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(&dir.path().join("m.sqlite3"), true).unwrap();
        let d = open(&dir.path().join("d.sqlite3"), false).unwrap();
        for _ in 0..2 {
            migrate_memory(&m).unwrap();
            migrate_data(&d).unwrap();
        }
        let n = || d.query_row("SELECT count(*) FROM pragma_table_info('trade_features')", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n(), 2);
        ensure_feature(&d, "c_ret_24h_pct", "test").unwrap();
        ensure_feature(&d, "c_ret_24h_pct", "test").unwrap(); // idempotent
        ensure_feature(&d, "new_thing", "test").unwrap();
        assert_eq!(n(), 4);
        assert_eq!(registry(&d).unwrap(), vec!["c_ret_24h_pct".to_string(), "new_thing".to_string()]);
        assert!(ensure_feature(&d, "x; DROP TABLE trades", "test").is_err());
        assert!(ensure_feature(&d, "Upper", "test").is_err());
        assert!(!valid_feature_name("") && !valid_feature_name("1abc") && valid_feature_name("a_1"));
    }
}

//! The bot's backtest archive as the main real input (spec 4.4). Read-only, incremental, idempotent import of
//! `bt_runs`, `bt_trades` (with the market state at each entry decision) and `bt_daily` into the lab's `runs`,
//! `trades`, `trade_features` and `daily` tables. Features are flattened into the DYNAMIC feature registry, so fields the
//! bot adds later appear automatically. Weekly blocks cut by entry time become the lab's independent windows.
//!
//! The archive stores trades only for runs that passed the bot's success filter, and the search runs are the best of
//! 100,000 in-sample. Every imported trade-level run is therefore flagged (`selection`), which caps confidence and
//! requires the out-of-sample (`oos`) check for a lesson (rules.rs).

use crate::config::Config;
use crate::contract;
use crate::db;
use crate::util::now_ts;
use anyhow::Result;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct BtReport {
    /// absent | locked | schema_mismatch | ok | disabled | error
    pub source: String,
    pub epoch: i64,
    pub epoch_reset: bool,
    pub new_runs: usize,
    pub new_trades: usize,
    pub new_daily: usize,
    pub skipped_trades: usize,
    pub runs_total: i64,
    pub runs_with_trades: i64,
    pub oos_runs: i64,
    pub trades_total: i64,
    pub note: String,
}

const REQUIRED: &[(&str, &[&str])] = &[
    ("bt_runs", &["id", "ts_ms", "source", "window_label", "first_bar_ts", "last_bar_ts", "start_equity", "params_hash", "params_json", "return_pct", "max_drawdown_pct", "trades", "wins", "profit_factor", "fees", "funding", "liquidations", "successful"]),
    ("bt_trades", &["id", "bt_id", "symbol", "entry_ts", "exit_ts", "entry", "exit", "qty", "leverage", "fees", "funding", "pnl", "ret_margin", "reason", "features"]),
    ("bt_daily", &["bt_id", "day_ts", "equity", "pnl"]),
];

/// Reads a named counter from the memory database (None if never set).
fn counter(mem: &Connection, name: &str) -> Result<Option<i64>> {
    Ok(mem.query_row("SELECT value FROM counters WHERE name = ?1", [name], |r| r.get(0)).optional()?)
}

/// Sets a named counter in the memory database.
fn set_counter(mem: &Connection, name: &str, v: i64) -> Result<()> {
    mem.execute("INSERT INTO counters(name, value) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET value = excluded.value", params![name, v])?;
    Ok(())
}

/// Checks the bot's archive has every required table and column; returns what is missing.
fn check_schema(c: &Connection) -> std::result::Result<(), String> {
    for (table, cols) in REQUIRED {
        let mut s = c.prepare(&format!("SELECT name FROM pragma_table_info('{table}')")).map_err(|e| e.to_string())?;
        let have: Vec<String> = s.query_map([], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?.filter_map(|r| r.ok()).collect();
        if have.is_empty() { return Err(format!("table `{table}` is missing")); }
        for col in *cols { if !have.iter().any(|h| h == col) { return Err(format!("`{table}.{col}` is missing")); } }
    }
    Ok(())
}

/// Whether `table` has a column named `col`.
fn has_col(c: &Connection, table: &str, col: &str) -> bool {
    c.query_row(&format!("SELECT count(*) FROM pragma_table_info('{table}') WHERE name = ?1"), [col], |r| r.get::<_, i64>(0)).map(|n| n > 0).unwrap_or(false)
}

/// User rule (2026-10-09): the lab learns from 2026 data and later only (2026-01-01 00:00 UTC, ms); older archive runs are skipped.
pub const RESEARCH_START_MS: i64 = 1_767_225_600_000;

/// Block label of a time (e.g. "b2931"): which block of `block_secs` seconds it falls in.
pub fn block_label(ts_secs: i64, block_secs: i64) -> String { format!("b{}", ts_secs.div_euclid(block_secs)) }

/// Turn a JSON key into a valid feature-name fragment.
fn snake(key: &str) -> String {
    let mut out = String::new();
    for c in key.chars() {
        if c.is_ascii_alphanumeric() { out.push(c.to_ascii_lowercase()) } else if !out.ends_with('_') { out.push('_') }
    }
    out.trim_matches('_').chars().take(30).collect()
}

/// A JSON number (or boolean as 1/0) as a finite f64.
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Flatten the bot's per-trade `features` JSON into named numbers. Known fields get stable names; any other numeric leaf
/// under `coin`, `trend` or `zones` becomes `c_*` / `m_*` automatically. Missing or null values are simply absent.
pub fn flatten_features(v: &Value) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let mut put = |name: &str, x: Option<f64>| { if let Some(x) = x { if db::valid_feature_name(name) { out.insert(name.to_string(), x); } } };
    let known_coin: &[(&str, &str)] = &[
        ("signal", "c_signal"), ("ret_1h_pct", "c_ret_1h_pct"), ("ret_4h_pct", "c_ret_4h_pct"), ("ret_24h_pct", "c_ret_24h_pct"),
        ("dist_from_24h_avg_pct", "c_dist_24h_avg_pct"), ("atr14_pct", "c_atr14_pct"), ("volume_ratio_1h_vs_prior_24h", "c_volume_ratio_1h"),
        ("turnover_24h", "c_turnover_24h"),
    ];
    if let Some(coin) = v.get("coin").and_then(|c| c.as_object()) {
        for (k, val) in coin {
            if k == "symbol" || k == "close" { continue; }
            let name = known_coin.iter().find(|(a, _)| a == k).map(|(_, b)| b.to_string()).unwrap_or_else(|| format!("c_{}", snake(k)));
            put(&name, num(val));
        }
    }
    let known_trend: &[(&str, &str)] = &[("dist_pct", "m_trend_dist_pct"), ("passes", "m_trend_passes")];
    if let Some(t) = v.get("trend").and_then(|c| c.as_object()) {
        for (k, val) in t {
            if matches!(k.as_str(), "btc_close" | "btc_line" | "ref_close" | "ref_line" | "window_bars" | "margin_pct" | "needed_pct") { continue; }
            let name = known_trend.iter().find(|(a, _)| a == k).map(|(_, b)| b.to_string()).unwrap_or_else(|| format!("m_trend_{}", snake(k)));
            put(&name, num(val));
        }
    }
    let known_zone: &[(&str, &str)] = &[
        ("dispersion_24h_pct", "m_dispersion_24h_pct"), ("median_return_24h_pct", "m_median_return_24h_pct"),
        ("btc_return_4h_pct", "m_btc_return_4h_pct"), ("btc_avg_slope_pct_vs_6h", "m_btc_slope_pct"),
        ("ref_return_4h_pct", "m_ref_return_4h_pct"), ("ref_avg_slope_pct_vs_6h", "m_ref_slope_pct"),
    ];
    if let Some(z) = v.get("zones").and_then(|c| c.as_object()) {
        for (k, val) in z {
            let name = known_zone.iter().find(|(a, _)| a == k).map(|(_, b)| b.to_string()).unwrap_or_else(|| format!("m_{}", snake(k)));
            put(&name, num(val));
        }
    }
    put("m_breadth_pct", v.get("breadth_pct").and_then(num));
    // Stock bot: the session clock in US Eastern time (UTC hours shift with daylight saving), minutes since midnight.
    let et = (v.get("et_minute_of_day").and_then(num), v.get("et_weekday").and_then(num));
    if let (Some(minute), Some(weekday)) = et {
        put("et_minute", Some(minute));
        put("weekday", Some(weekday));
    } else if let Some(ms) = v.get("decision_bar_close_ms").and_then(|x| x.as_i64()) {
        let secs = ms.div_euclid(1000);
        put("hour", Some(((secs / 3600) % 24) as f64));
        put("weekday", Some(((secs / 86_400 + 4) % 7) as f64)); // 1970-01-01 was a Thursday; 0 = Sunday
    }
    if let (Some(a), Some(b)) = (out.get("c_ret_24h_pct").copied(), out.get("m_median_return_24h_pct").copied()) {
        out.insert("c_ret_24h_vs_median".into(), a - b);
    }
    out
}

/// Imports new runs, trades and daily rows from the bot's backtest archive (read-only) into the lab's data database.
pub fn import(cfg: &Config, mem: &Connection, data: &Connection) -> Result<BtReport> {
    let mut rep = BtReport { epoch: counter(mem, "bt_epoch")?.unwrap_or(1), ..Default::default() };
    if cfg.mode != "real" { rep.source = "disabled".into(); return finish(data, rep); }
    let path = &cfg.backtest_archive.path;
    if !path.exists() { rep.source = "absent".into(); return finish(data, rep); }
    let src = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) {
        Ok(c) => c,
        Err(e) => { rep.source = "locked".into(); rep.note = format!("cannot open archive: {e}"); return finish(data, rep); }
    };
    src.busy_timeout(std::time::Duration::from_secs(5))?;
    if let Err(e) = src.execute_batch("BEGIN") { rep.source = "locked".into(); rep.note = format!("cannot read archive: {e}"); return finish(data, rep); }
    if let Err(why) = check_schema(&src) { rep.source = "schema_mismatch".into(); rep.note = why; return finish(data, rep); }
    rep.source = "ok".into();

    let first_ts: Option<i64> = src.query_row("SELECT min(ts_ms) FROM bt_runs", [], |r| r.get(0))?;
    let max_run: i64 = src.query_row("SELECT coalesce(max(id),0) FROM bt_runs", [], |r| r.get(0))?;
    let max_trade: i64 = src.query_row("SELECT coalesce(max(id),0) FROM bt_trades", [], |r| r.get(0))?;
    let (wm_r, wm_t) = (counter(mem, "bt_wm_runs")?.unwrap_or(0), counter(mem, "bt_wm_trades")?.unwrap_or(0));
    let stored_first = counter(mem, "bt_first_ts")?;
    // A recreated archive restarts its ids (max id below the watermark) or gains runs older than any seen before (earliest time moved
    // EARLIER). Deleting old rows only moves the earliest time later: that is a cleanup, not a new archive, so nothing is re-imported.
    let reset = matches!((stored_first, first_ts), (Some(s), Some(f)) if f < s) || max_run < wm_r || max_trade < wm_t;
    let (epoch, wm_r, wm_t) = if reset { rep.epoch_reset = true; (rep.epoch + 1, 0, 0) } else { (rep.epoch, wm_r, wm_t) };
    rep.epoch = epoch;

    // The block length is fixed per lab at the first import so labels stay consistent.
    let wanted = cfg.evidence.block_days.max(1) * 86_400;
    let block = match data.query_row("SELECT value FROM lab_meta WHERE key = 'block_secs'", [], |r| r.get::<_, String>(0)).ok().and_then(|v| v.parse::<i64>().ok()) {
        Some(b) => { if b != wanted { rep.note = format!("block length is fixed at {} days for this lab (lab.toml asks for {}); a re-import is needed to change it", b / 86_400, wanted / 86_400); } b }
        None => { data.execute("INSERT INTO lab_meta(key, value) VALUES ('block_secs', ?1)", [wanted.to_string()])?; wanted }
    };
    let tx = data.unchecked_transaction()?;
    let mut names_seen: std::collections::BTreeSet<String> = Default::default();
    let mut batch: Option<i64> = None;
    let mut new_run_ids: Vec<(i64, String)> = Vec::new();
    {
        // Columns the bot added later are optional: an older archive without them still imports.
        let opt = |col: &str| if has_col(&src, "bt_runs", col) { col.to_string() } else { format!("NULL AS {col}") };
        let mut s = src.prepare(&format!("SELECT id, ts_ms, source, window_label, first_bar_ts, last_bar_ts, start_equity, params_hash, params_json, rank, robust_score, seed, return_pct, max_drawdown_pct, trades, wins, profit_factor, fees, funding, liquidations, successful, {}, {}, {} FROM bt_runs WHERE id > ?1 AND first_bar_ts >= {RESEARCH_START_MS} ORDER BY id", opt("engine_commit"), opt("cost_model_version"), opt("tweak_id")))?;
        let mut rows = s.query([wm_r])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let source: String = r.get(2)?;
            let (first_bar, last_bar): (i64, i64) = (r.get::<_, i64>(4)?.div_euclid(1000), r.get::<_, i64>(5)?.div_euclid(1000));
            let params_json: String = r.get(8)?;
            contract::scrub(&params_json)?;
            let successful = r.get::<_, i64>(20)? != 0;
            let run_id = format!("bt{epoch}-{id}");
            let stored_trades: i64 = src.query_row("SELECT count(*) FROM bt_trades WHERE bt_id = ?1", [id], |r| r.get(0))?;
            // Selection (spec 4.4): trades of out-of-sample and random runs are stored whether or not the run succeeded, so they
            // are an UNSELECTED sample; trades of top-ranked and chosen runs exist only because the run passed the success filter.
            let unselected_source = matches!(source.as_str(), "oos" | "random");
            let selection = if stored_trades == 0 { "header_only" } else if unselected_source { "none" } else { "successful_only" };
            let engine_commit: String = r.get::<_, Option<String>>(21)?.map(|c| c.chars().take(40).collect()).unwrap_or_else(|| "bot-archive".into());
            let cost_model: String = r.get::<_, Option<String>>(22)?.map(|c| c.chars().take(200).collect()).unwrap_or_else(|| "bot-archive-1".into());
            let tweak_id: Option<String> = r.get::<_, Option<String>>(23)?.map(|t| t.chars().take(40).collect());
            let metrics = serde_json::json!({
                "return_pct": r.get::<_, f64>(12)?, "max_drawdown_pct": r.get::<_, f64>(13)?, "trades": r.get::<_, i64>(14)?, "wins": r.get::<_, i64>(15)?,
                "liquidations": r.get::<_, i64>(19)?, "profit_factor": r.get::<_, f64>(16)?, "fees": r.get::<_, f64>(17)?, "funding": r.get::<_, f64>(18)?,
                "start_equity": r.get::<_, f64>(6)?, "successful": successful, "robust_score": r.get::<_, Option<f64>>(10)?,
            });
            let b = match batch {
                Some(b) => b,
                None => {
                    tx.execute("INSERT INTO ingest_batches(ingested_at, files, bytes, runs) VALUES (?1, 0, 0, 0)", [now_ts()])?;
                    let b = tx.last_insert_rowid();
                    batch = Some(b);
                    b
                }
            };
            tx.execute(
                "INSERT OR IGNORE INTO runs(run_id, batch, engine_commit, window_label, window_start, window_end, data_fingerprint, cost_model_version, role, rank, params_hash, params_json, metrics_json, return_pct, degraded, seed, created, selection, source_label, run_key, tweak_id) \
                 VALUES (?1,?2,?18,?3,?4,?5,?6,?19,?7,?8,?9,?10,?11,?12,0,?13,?14,?15,?16,?17,?20)",
                params![run_id, b, block_label(last_bar, block), first_bar, last_bar, format!("archive:{first_bar}:{last_bar}"), source, r.get::<_, Option<i64>>(9)?,
                    r.get::<_, String>(7)?, params_json, metrics.to_string(), r.get::<_, f64>(12)?, r.get::<_, Option<i64>>(11)?, now_ts(),
                    selection, r.get::<_, String>(3)?, format!("bt:{epoch}:{id}"), engine_commit, cost_model, tweak_id],
            )?;
            new_run_ids.push((id, run_id));
            rep.new_runs += 1;
        }
    }
    let run_of: HashMap<i64, String> = new_run_ids.iter().cloned().collect();
    {
        let mut next_idx: HashMap<String, i64> = HashMap::new();
        let decided = if has_col(&src, "bt_trades", "decided_ts") { "decided_ts" } else { "NULL AS decided_ts" };
        let mut s = src.prepare(&format!("SELECT id, bt_id, symbol, entry_ts, exit_ts, entry, exit, qty, leverage, fees, funding, pnl, ret_margin, reason, features, {decided} FROM bt_trades WHERE id > ?1 ORDER BY id"))?;
        let mut rows = s.query([wm_t])?;
        while let Some(r) = rows.next()? {
            let bt_id: i64 = r.get(1)?;
            let run_id = run_of.get(&bt_id).cloned().unwrap_or_else(|| format!("bt{epoch}-{bt_id}"));
            let exists: i64 = tx.query_row("SELECT count(*) FROM runs WHERE run_id = ?1", [&run_id], |r| r.get(0))?;
            if exists == 0 { rep.skipped_trades += 1; continue; }
            let idx = match next_idx.get(&run_id) {
                Some(i) => *i,
                None => tx.query_row("SELECT coalesce(max(idx) + 1, 0) FROM trades WHERE run_id = ?1", [&run_id], |r| r.get::<_, i64>(0))?,
            };
            next_idx.insert(run_id.clone(), idx + 1);
            let (entry_ms, exit_ms): (i64, i64) = (r.get(3)?, r.get(4)?);
            let (entry_ts, exit_ts) = (entry_ms.div_euclid(1000), exit_ms.div_euclid(1000));
            let (entry, exit, qty, lev): (f64, f64, f64, f64) = (r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?);
            let (fees, funding, pnl, ret): (f64, f64, f64, f64) = (r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?);
            if ![entry, exit, qty, lev, fees, funding, pnl, ret].iter().all(|x| x.is_finite()) || exit_ts <= entry_ts || entry <= 0.0 || exit <= 0.0 { rep.skipped_trades += 1; continue; }
            let margin = if ret.abs() > 1e-12 { (pnl / ret).abs() } else { qty * entry / lev.max(1.0) };
            let reason: String = r.get(13)?;
            tx.execute(
                "INSERT OR IGNORE INTO trades(run_id, idx, symbol, side, entry_ts, exit_ts, entry_px, exit_px, qty, margin, fees, funding, pnl, ret_margin, reason, day, window, decided_ts) VALUES (?1,?2,?3,'long',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
                params![run_id, idx, r.get::<_, String>(2)?, entry_ts, exit_ts, entry, exit, qty, margin, fees, funding, pnl, ret, reason.chars().take(80).collect::<String>(), entry_ts / 86_400, block_label(entry_ts, block), r.get::<_, Option<i64>>(15)?.map(|d| d.div_euclid(1000))],
            )?;
            if let Some(fj) = r.get::<_, Option<String>>(14)? {
                if let Ok(v) = serde_json::from_str::<Value>(&fj) {
                    let feats = flatten_features(&v);
                    for n in feats.keys() { if names_seen.insert(n.clone()) { db::ensure_feature(&tx, n, "bot-backtest-archive")?; } }
                    let cols: String = feats.keys().map(|f| format!(", f_{f}")).collect();
                    let marks: String = (0..feats.len()).map(|k| format!(", ?{}", k + 3)).collect();
                    let mut vals: Vec<rusqlite::types::Value> = vec![run_id.clone().into(), idx.into()];
                    vals.extend(feats.values().map(|v| rusqlite::types::Value::Real(*v)));
                    tx.execute(&format!("INSERT OR IGNORE INTO trade_features(run_id, idx{cols}) VALUES (?1, ?2{marks})"), rusqlite::params_from_iter(vals))?;
                }
            }
            rep.new_trades += 1;
        }
    }
    {
        let mut s = src.prepare("SELECT day_ts, equity, pnl FROM bt_daily WHERE bt_id = ?1 ORDER BY day_ts")?;
        for (bt_id, run_id) in &new_run_ids {
            let mut rows = s.query([bt_id])?;
            while let Some(r) = rows.next()? {
                let day = r.get::<_, i64>(0)?.div_euclid(86_400_000);
                rep.new_daily += tx.execute("INSERT OR IGNORE INTO daily(run_id, day, equity, pnl) VALUES (?1,?2,?3,?4)", params![run_id, day, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?])?;
            }
        }
    }
    if let Some(b) = batch { tx.execute("UPDATE ingest_batches SET runs = ?1 WHERE id = ?2", params![rep.new_runs as i64, b])?; }
    reconcile(mem, &src, &tx, epoch, wm_t.max(counter(mem, "bt_wm_trades")?.unwrap_or(0)).min(max_trade))?;
    tx.commit()?;
    src.execute_batch("COMMIT")?;
    set_counter(mem, "bt_epoch", epoch)?;
    set_counter(mem, "bt_wm_runs", max_run)?;
    set_counter(mem, "bt_wm_trades", max_trade)?;
    if let Some(f) = first_ts { set_counter(mem, "bt_first_ts", f)?; }
    finish(data, rep)
}

/// Fill in details for rows imported earlier (for example by an older build of the lab) that the bot has since added to the same
/// archive rows: commit and cost model, tweak tag, the true selection status of out-of-sample/random runs, and decision times.
/// Never overwrites a value that is already set, and never touches rows the archive does not know.
fn reconcile(mem: &Connection, src: &Connection, tx: &Connection, epoch: i64, imported_trades_upto: i64) -> Result<()> {
    if has_col(src, "bt_runs", "engine_commit") {
        let mut s = tx.prepare("SELECT run_id, run_key FROM runs WHERE run_key LIKE ?1 AND (engine_commit = 'bot-archive' OR cost_model_version = 'bot-archive-1')")?;
        let todo: Vec<(String, String)> = s.query_map([format!("bt:{epoch}:%")], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        for (run_id, key) in todo {
            let Some(id) = key.rsplit(':').next().and_then(|x| x.parse::<i64>().ok()) else { continue };
            let row: Option<(Option<String>, Option<String>, Option<String>)> = src.query_row(
                "SELECT engine_commit, cost_model_version, tweak_id FROM bt_runs WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
            if let Some((c, m, t)) = row {
                if let Some(c) = c { tx.execute("UPDATE runs SET engine_commit = ?1 WHERE run_id = ?2", params![c.chars().take(40).collect::<String>(), run_id])?; }
                if let Some(m) = m { tx.execute("UPDATE runs SET cost_model_version = ?1 WHERE run_id = ?2", params![m.chars().take(200).collect::<String>(), run_id])?; }
                if let Some(t) = t { tx.execute("UPDATE runs SET tweak_id = ?1 WHERE run_id = ?2", params![t.chars().take(40).collect::<String>(), run_id])?; }
            }
        }
    }
    // Out-of-sample and random runs keep their trades whether or not they succeeded: with trades they are an unselected sample.
    {
        let mut s = tx.prepare("SELECT r.run_id, r.run_key, r.selection FROM runs r WHERE r.run_key LIKE ?1 AND r.role IN ('oos','random') AND r.selection != 'none'")?;
        let todo: Vec<(String, String, String)> = s.query_map([format!("bt:{epoch}:%")], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        for (run_id, key, _sel) in todo {
            let Some(id) = key.rsplit(':').next().and_then(|x| x.parse::<i64>().ok()) else { continue };
            let in_src: i64 = src.query_row("SELECT count(*) FROM bt_trades WHERE bt_id = ?1", [id], |r| r.get(0))?;
            let in_lab: i64 = tx.query_row("SELECT count(*) FROM trades WHERE run_id = ?1", [&run_id], |r| r.get(0))?;
            if in_src > 0 && in_lab > 0 { tx.execute("UPDATE runs SET selection = 'none' WHERE run_id = ?1", [&run_id])?; }
        }
    }
    if has_col(src, "bt_trades", "decided_ts") {
        let done = counter(mem, "bt_decided_upto")?.unwrap_or(0);
        let mut s = src.prepare("SELECT id, bt_id, symbol, entry_ts, decided_ts FROM bt_trades WHERE id > ?1 AND id <= ?2 AND decided_ts IS NOT NULL ORDER BY id")?;
        let mut rows = s.query(params![done, imported_trades_upto])?;
        let mut last = done;
        while let Some(r) = rows.next()? {
            let (tid, bt, sym, entry_ms, dec_ms): (i64, i64, String, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?);
            tx.execute("UPDATE trades SET decided_ts = ?1 WHERE run_id = ?2 AND symbol = ?3 AND entry_ts = ?4 AND decided_ts IS NULL",
                params![dec_ms.div_euclid(1000), format!("bt{epoch}-{bt}"), sym, entry_ms.div_euclid(1000)])?;
            last = tid;
        }
        if last > done { set_counter(mem, "bt_decided_upto", last)?; }
    }
    Ok(())
}

/// Report of what the lab already holds from the archive, without importing.
pub fn current(data: &Connection) -> Result<BtReport> {
    finish(data, BtReport { source: "cached".into(), ..Default::default() })
}

/// Fills the report's totals (runs, runs with trades, oos runs, trades) from the data database.
fn finish(data: &Connection, mut rep: BtReport) -> Result<BtReport> {
    rep.runs_total = data.query_row("SELECT count(*) FROM runs WHERE run_key IS NOT NULL", [], |r| r.get(0))?;
    rep.runs_with_trades = data.query_row("SELECT count(DISTINCT t.run_id) FROM trades t JOIN runs r ON r.run_id = t.run_id WHERE r.run_key IS NOT NULL", [], |r| r.get(0))?;
    rep.oos_runs = data.query_row("SELECT count(*) FROM runs WHERE role = 'oos'", [], |r| r.get(0))?;
    rep.trades_total = data.query_row("SELECT count(*) FROM trades t JOIN runs r ON r.run_id = t.run_id WHERE r.run_key IS NOT NULL", [], |r| r.get(0))?;
    Ok(rep)
}

/// Import that never fails: an error becomes a report with source "error" and the message.
pub fn import_best_effort(cfg: &Config, mem: &Connection, data: &Connection) -> BtReport {
    import(cfg, mem, data).unwrap_or_else(|e| BtReport { source: "error".into(), note: format!("{e:#}"), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DDL: &str = "
        CREATE TABLE bt_runs(id INTEGER PRIMARY KEY, service_run INTEGER NOT NULL, ts_ms INTEGER NOT NULL, source TEXT NOT NULL, window_label TEXT NOT NULL,
          first_bar_ts INTEGER NOT NULL, last_bar_ts INTEGER NOT NULL, start_equity REAL NOT NULL, params_hash TEXT NOT NULL, params_json TEXT NOT NULL,
          rank INTEGER, robust_score REAL, seed INTEGER, return_pct REAL NOT NULL, max_drawdown_pct REAL NOT NULL, trades INTEGER NOT NULL, wins INTEGER NOT NULL,
          profit_factor REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL, liquidations INTEGER NOT NULL, successful INTEGER NOT NULL,
          UNIQUE(params_hash, source, window_label, first_bar_ts));
        CREATE TABLE bt_trades(id INTEGER PRIMARY KEY, bt_id INTEGER NOT NULL REFERENCES bt_runs(id), symbol TEXT NOT NULL, entry_ts INTEGER NOT NULL,
          exit_ts INTEGER NOT NULL, entry REAL NOT NULL, exit REAL NOT NULL, qty REAL NOT NULL, leverage REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL,
          pnl REAL NOT NULL, ret_margin REAL NOT NULL, reason TEXT NOT NULL, features TEXT);
        CREATE TABLE bt_daily(bt_id INTEGER NOT NULL REFERENCES bt_runs(id), day_ts INTEGER NOT NULL, equity REAL NOT NULL, pnl REAL NOT NULL,
          trades INTEGER NOT NULL, wins INTEGER NOT NULL, tokens TEXT NOT NULL, PRIMARY KEY(bt_id, day_ts));
        CREATE TABLE candles(symbol TEXT NOT NULL, ts INTEGER NOT NULL, open REAL, PRIMARY KEY(symbol, ts));";

    const WEEK_MS: i64 = 7 * 86_400_000;
    const T0: i64 = 1_790_000_000_000 / WEEK_MS * WEEK_MS; // a block boundary

    struct Fx { _dir: tempfile::TempDir, cfg: Config, mem: Connection, data: Connection }

    /// Test fixture: a real-mode lab in a temporary folder with an archive path set.
    fn fixture() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_root(dir.path(), "real");
        cfg.backtest_archive.path = dir.path().join("arch").join("backtest_archive.sqlite3");
        let mem = db::open(&cfg.memory_db(), true).unwrap();
        let data = db::open(&cfg.data_db(), false).unwrap();
        db::migrate_memory(&mem).unwrap();
        db::migrate_data(&data).unwrap();
        Fx { _dir: dir, cfg, mem, data }
    }

    /// Test fixture: creates the bot-side backtest archive schema at `path`.
    fn archive(path: &std::path::Path) -> Connection {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch(DDL).unwrap();
        c
    }

    /// Test fixture: a trade's feature JSON for day `day` with a 24h return of `strong`.
    fn features(day: i64, strong: f64) -> String {
        json!({"decision_bar_close_ms": T0 + day * 86_400_000 + 3_600_000 * 5,
            "coin": {"symbol": "AAAUSDT", "signal": -1.5, "close": 1.0, "ret_1h_pct": 0.2, "ret_4h_pct": -0.5, "ret_24h_pct": strong, "dist_from_24h_avg_pct": -2.0,
                     "atr14_pct": 1.1, "volume_ratio_1h_vs_prior_24h": 1.7, "turnover_24h": 5.0e6, "brand_new_field": 4.5},
            "trend": {"regime": "BtcTrend", "dist_pct": 0.8, "passes": true, "btc_close": 100.0, "needed_pct": 1.0},
            "zones": {"dispersion_24h_pct": 2.5, "median_return_24h_pct": 0.5, "btc_return_4h_pct": 0.3, "btc_avg_slope_pct_vs_6h": 0.1},
            "breadth_pct": 61.0}).to_string()
    }

    /// One run: `n_days` days of trades starting at day `d0`, two trades a day.
    fn add_run(c: &Connection, id: i64, source: &str, successful: bool, d0: i64, n_days: i64) {
        let first = T0 + d0 * 86_400_000;
        let last = first + n_days * 86_400_000;
        c.execute("INSERT INTO bt_runs(id, service_run, ts_ms, source, window_label, first_bar_ts, last_bar_ts, start_equity, params_hash, params_json, rank, robust_score, seed, return_pct, max_drawdown_pct, trades, wins, profit_factor, fees, funding, liquidations, successful) VALUES (?1,1,?2,?3,'lbl',?4,?5,109.48,?6,'{\"signal\":\"CalmDip\",\"hold\":99}',?7,-3.5,42,?8,2.0,?9,?10,1.5,3.0,0.1,0,?11)",
            params![id, last, source, first, last, format!("h{id}"), if source == "search_top" { Some(id) } else { None }, if successful { 5.0 } else { -2.0 },
                n_days * 2, n_days, successful as i64]).unwrap();
        if !successful { return; }
        for d in 0..n_days {
            for k in 0..2 {
                let entry = first + d * 86_400_000 + k * 900_000;
                let ret = 0.02 * if (d + k) % 3 == 0 { -1.0 } else { 1.0 };
                c.execute("INSERT INTO bt_trades(bt_id, symbol, entry_ts, exit_ts, entry, exit, qty, leverage, fees, funding, pnl, ret_margin, reason, features) VALUES (?1,'AAAUSDT',?2,?3,1.0,1.01,55.5,11.0,0.05,0.0,?4,?5,'Take profit',?6)",
                    params![id, entry, entry + 3_600_000, 109.48 * ret * 0.1, ret, features(d0 + d, d as f64)]).unwrap();
            }
            c.execute("INSERT INTO bt_daily VALUES (?1,?2,?3,?4,2,1,'[]')", params![id, first + d * 86_400_000, 109.48 + d as f64, 1.0]).unwrap();
        }
    }

    /// Test: flattening maps known fields derives relative ones and adopts unknown numbers.
    #[test]
    fn flattening_maps_known_fields_derives_relative_ones_and_adopts_unknown_numbers() {
        let f = flatten_features(&serde_json::from_str(&features(0, 3.0)).unwrap());
        assert_eq!(f["c_ret_24h_pct"], 3.0);
        assert_eq!(f["c_volume_ratio_1h"], 1.7);
        assert_eq!(f["m_trend_dist_pct"], 0.8);
        assert_eq!(f["m_trend_passes"], 1.0);
        assert_eq!(f["m_btc_slope_pct"], 0.1);
        assert_eq!(f["m_breadth_pct"], 61.0);
        assert_eq!(f["c_ret_24h_vs_median"], 2.5);
        assert_eq!(f["c_brand_new_field"], 4.5, "unknown numeric fields become features automatically");
        assert_eq!(f["hour"], 5.0);
        assert!(!f.contains_key("c_symbol") && !f.contains_key("c_close"));
        assert!(flatten_features(&json!({"coin": null, "trend": 5})).is_empty());
    }

    /// Test: missing archive and schema drift are reported not fatal.
    #[test]
    fn missing_archive_and_schema_drift_are_reported_not_fatal() {
        let f = fixture();
        assert_eq!(import(&f.cfg, &f.mem, &f.data).unwrap().source, "absent");
        let c = archive(&f.cfg.backtest_archive.path);
        c.execute_batch("DROP TABLE bt_daily; CREATE TABLE bt_daily(x INTEGER);").unwrap();
        drop(c);
        let r = import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!(r.source, "schema_mismatch");
        assert!(r.note.contains("bt_daily"), "{}", r.note);
    }

    /// Test: import maps roles selection blocks features and amounts and is idempotent.
    #[test]
    fn import_maps_roles_selection_blocks_features_and_amounts_and_is_idempotent() {
        let f = fixture();
        let a = archive(&f.cfg.backtest_archive.path);
        add_run(&a, 1, "search_top", true, 0, 6);
        add_run(&a, 2, "chosen", false, 0, 6); // header only
        add_run(&a, 3, "oos", true, 7, 6);
        let r = import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!((r.source.as_str(), r.new_runs, r.new_trades, r.new_daily), ("ok", 3, 24, 12));
        assert_eq!((r.runs_total, r.runs_with_trades, r.oos_runs), (3, 2, 1));
        let (role, sel): (String, String) = f.data.query_row("SELECT role, selection FROM runs WHERE run_key = 'bt:1:2'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((role.as_str(), sel.as_str()), ("chosen", "header_only"));
        let (role, sel): (String, String) = f.data.query_row("SELECT role, selection FROM runs WHERE run_key = 'bt:1:3'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((role.as_str(), sel.as_str()), ("oos", "none"), "out-of-sample trades are an unselected sample");
        // the failed run has a header and no trades
        assert_eq!(f.data.query_row::<i64, _, _>("SELECT count(*) FROM trades WHERE run_id = 'bt1-2'", [], |r| r.get(0)).unwrap(), 0);
        // weekly blocks by entry time; times are stored in seconds
        let blocks: Vec<String> = { let mut s = f.data.prepare("SELECT DISTINCT window FROM trades ORDER BY window").unwrap(); s.query_map([], |r| r.get(0)).unwrap().map(|x| x.unwrap()).collect() };
        assert_eq!(blocks, vec![block_label(T0 / 1000, 7 * 86_400), block_label(T0 / 1000 + 7 * 86_400, 7 * 86_400)]);
        // features landed in the dynamic registry, amounts are kept as the bot recorded them
        let reg = db::registry(&f.data).unwrap();
        assert!(reg.contains(&"c_ret_24h_pct".to_string()) && reg.contains(&"c_brand_new_field".to_string()) && reg.contains(&"m_breadth_pct".to_string()), "{reg:?}");
        let v: f64 = f.data.query_row("SELECT f_c_atr14_pct FROM trade_features LIMIT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 1.1);
        let m: String = f.data.query_row("SELECT metrics_json FROM runs WHERE run_id = 'bt1-1'", [], |r| r.get(0)).unwrap();
        assert!(m.contains("109.48"), "absolute start equity must be kept: {m}");
        let (pnl, margin): (f64, f64) = f.data.query_row("SELECT pnl, margin FROM trades LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!((margin - 109.48 * 0.1).abs() < 1e-9 && pnl.abs() > 0.0);
        // idempotent, and incremental for new rows
        let again = import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!((again.new_runs, again.new_trades, again.new_daily), (0, 0, 0));
        add_run(&a, 4, "search_top", true, 14, 3);
        let more = import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!((more.new_runs, more.new_trades), (1, 6));
        assert_eq!(more.runs_total, 4);
    }

    /// Test: recreated archive starts a new epoch and the original is never modified.
    #[test]
    fn recreated_archive_starts_a_new_epoch_and_the_original_is_never_modified() {
        let f = fixture();
        {
            let a = archive(&f.cfg.backtest_archive.path);
            add_run(&a, 1, "search_top", true, 0, 4);
            a.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        }
        let before = (crate::util::sha256_file(&f.cfg.backtest_archive.path).unwrap(), std::fs::metadata(&f.cfg.backtest_archive.path).unwrap().modified().unwrap());
        import(&f.cfg, &f.mem, &f.data).unwrap();
        import(&f.cfg, &f.mem, &f.data).unwrap();
        let after = (crate::util::sha256_file(&f.cfg.backtest_archive.path).unwrap(), std::fs::metadata(&f.cfg.backtest_archive.path).unwrap().modified().unwrap());
        assert_eq!(before, after);
        std::fs::remove_file(&f.cfg.backtest_archive.path).unwrap();
        for ext in ["sqlite3-wal", "sqlite3-shm"] { std::fs::remove_file(f.cfg.backtest_archive.path.with_extension(ext)).ok(); }
        {
            let a = archive(&f.cfg.backtest_archive.path);
            add_run(&a, 1, "oos", true, 30, 3); // ids restart, a different first timestamp
        }
        let r = import(&f.cfg, &f.mem, &f.data).unwrap();
        assert!(r.epoch_reset && r.epoch == 2, "{r:?}");
        assert_eq!(r.runs_total, 2); // the first epoch's run is kept, not overwritten
    }

    /// Test: poisoned settings are rejected and the import rolls back.
    #[test]
    fn poisoned_settings_are_rejected_and_the_import_rolls_back() {
        let f = fixture();
        let a = archive(&f.cfg.backtest_archive.path);
        add_run(&a, 1, "search_top", true, 0, 2);
        a.execute("UPDATE bt_runs SET params_json = '{\"api_key\":\"x\"}'", []).unwrap();
        assert!(import(&f.cfg, &f.mem, &f.data).is_err());
        assert_eq!(f.data.query_row::<i64, _, _>("SELECT count(*) FROM runs", [], |r| r.get(0)).unwrap(), 0);
    }

    /// Test: newer archive columns unselected runs and the random population are understood.
    #[test]
    fn newer_archive_columns_unselected_runs_and_the_random_population_are_understood() {
        let f = fixture();
        let a = archive(&f.cfg.backtest_archive.path);
        // the bot later added these columns (additive migration)
        a.execute_batch("ALTER TABLE bt_runs ADD COLUMN engine_commit TEXT; ALTER TABLE bt_runs ADD COLUMN cost_model_version TEXT; ALTER TABLE bt_runs ADD COLUMN tweak_id TEXT; ALTER TABLE bt_trades ADD COLUMN decided_ts INTEGER;").unwrap();
        add_run(&a, 1, "search_top", true, 0, 6);
        // an out-of-sample run that FAILED the success filter but kept its trades, and a random-parameter run likewise
        add_run(&a, 2, "oos", true, 7, 6);
        add_run(&a, 3, "random", true, 7, 6);
        a.execute_batch("UPDATE bt_runs SET successful = 0 WHERE id IN (2, 3);").unwrap();
        a.execute("UPDATE bt_runs SET engine_commit = 'f67c7b4c0ef5', cost_model_version = 'v5 2026-10-05: measured fees', tweak_id = 'tweak-9' WHERE id = 3", []).unwrap();
        a.execute("UPDATE bt_trades SET decided_ts = entry_ts - 1800000", []).unwrap();
        let r = import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!((r.source.as_str(), r.new_runs), ("ok", 3), "{r:?}");
        let sel = |id: &str| -> (String, String, String, Option<String>) {
            f.data.query_row("SELECT selection, role, engine_commit, tweak_id FROM runs WHERE run_key = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap()
        };
        assert_eq!(sel("bt:1:1").0, "successful_only", "top-ranked trades exist only because the run passed the filter");
        assert_eq!(sel("bt:1:2").0, "none", "an out-of-sample run keeps its trades whether or not it succeeded: an unselected sample");
        let (selection, role, commit, tweak) = sel("bt:1:3");
        assert_eq!((selection.as_str(), role.as_str(), commit.as_str(), tweak.as_deref()), ("none", "random", "f67c7b4c0ef5", Some("tweak-9")));
        let cost: String = f.data.query_row("SELECT cost_model_version FROM runs WHERE run_key = 'bt:1:3'", [], |r| r.get(0)).unwrap();
        assert!(cost.starts_with("v5 2026-10-05"));
        // the older rows without commit info still get a placeholder, never an invented hash
        assert_eq!(sel("bt:1:1").2, "bot-archive");
        let d: (i64, i64) = f.data.query_row("SELECT decided_ts, entry_ts FROM trades WHERE run_id = 'bt1-3' LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(d.1 - d.0, 1800, "decided_ts is stored in seconds");
        assert_eq!(r.runs_with_trades, 3);
    }

    /// Test: rows imported before the bot added details are completed without overwriting.
    #[test]
    fn rows_imported_before_the_bot_added_details_are_completed_without_overwriting() {
        let f = fixture();
        let a = archive(&f.cfg.backtest_archive.path);
        add_run(&a, 1, "oos", true, 0, 6);
        add_run(&a, 2, "random", true, 0, 6);
        a.execute_batch("UPDATE bt_runs SET successful = 0;").unwrap();
        // 1) an older build imports the rows when the archive has no extra columns
        let first = import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!(first.new_runs, 2);
        let commit = |id: &str| -> String { f.data.query_row("SELECT engine_commit FROM runs WHERE run_key = ?1", [id], |r| r.get(0)).unwrap() };
        assert_eq!(commit("bt:1:1"), "bot-archive");
        // 2) the bot then adds the columns and fills them in for existing rows
        a.execute_batch("ALTER TABLE bt_runs ADD COLUMN engine_commit TEXT; ALTER TABLE bt_runs ADD COLUMN cost_model_version TEXT; ALTER TABLE bt_runs ADD COLUMN tweak_id TEXT; ALTER TABLE bt_trades ADD COLUMN decided_ts INTEGER;
                         UPDATE bt_runs SET engine_commit = 'abc123', cost_model_version = 'v5'; UPDATE bt_trades SET decided_ts = entry_ts - 1800000 WHERE id % 2 = 0;").unwrap();
        import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!((commit("bt:1:1"), commit("bt:1:2")), ("abc123".to_string(), "abc123".to_string()));
        let with_dec: i64 = f.data.query_row("SELECT count(*) FROM trades WHERE decided_ts IS NOT NULL", [], |r| r.get(0)).unwrap();
        let total: i64 = f.data.query_row("SELECT count(*) FROM trades", [], |r| r.get(0)).unwrap();
        assert!(with_dec > 0 && with_dec < total, "{with_dec} of {total}");
        // 3) values already set are never overwritten by a later change in the archive
        a.execute("UPDATE bt_runs SET engine_commit = 'zzz999' WHERE id = 1", []).unwrap();
        import(&f.cfg, &f.mem, &f.data).unwrap();
        assert_eq!(commit("bt:1:1"), "abc123");
        // 4) out-of-sample and random runs that kept their trades are an unselected sample
        let sel: String = f.data.query_row("SELECT selection FROM runs WHERE run_key = 'bt:1:1'", [], |r| r.get(0)).unwrap();
        assert_eq!(sel, "none");
    }

    /// Test: only complete blocks with enough days become windows.
    #[test]
    fn only_complete_blocks_with_enough_days_become_windows() {
        let f = fixture();
        let a = archive(&f.cfg.backtest_archive.path);
        for id in 1..=5 { add_run(&a, id, "search_top", true, 0, 14); } // two full weeks, 70 trades each week (enough to count)
        add_run(&a, 6, "search_top", true, 14, 2); // a barely started third block (2 days, 4 trades: too thin)
        import(&f.cfg, &f.mem, &f.data).unwrap();
        let w = crate::context::windows(&f.data).unwrap();
        assert_eq!(w.len(), 2, "{w:?}"); // the third block is incomplete and thin
        assert_eq!(w[0], block_label(T0 / 1000, 7 * 86_400));
    }

    /// Test: archive trades are flagged as selected so the oos rule applies.
    #[test]
    fn archive_trades_are_flagged_as_selected_so_the_oos_rule_applies() {
        let f = fixture();
        let a = archive(&f.cfg.backtest_archive.path);
        add_run(&a, 1, "search_top", true, 0, 14);
        import(&f.cfg, &f.mem, &f.data).unwrap();
        let n: i64 = f.data.query_row("SELECT count(*) FROM runs WHERE selection != 'none'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    /// Test: stock session time is taken in eastern time not utc hours.
    #[test]
    fn stock_session_time_is_taken_in_eastern_time_not_utc_hours() {
        let f = flatten_features(&json!({"decision_bar_close_ms": 1_790_000_000_000_i64, "et_minute_of_day": 570, "et_weekday": 3, "coin": {"ret_24h_pct": 1.0}}));
        assert_eq!((f["et_minute"], f["weekday"]), (570.0, 3.0));
        assert!(!f.contains_key("hour"), "UTC hour must not be used when Eastern time is given");
        let old = flatten_features(&json!({"decision_bar_close_ms": 1_790_000_000_000_i64}));
        assert!(old.contains_key("hour") && !old.contains_key("et_minute"));
    }
}

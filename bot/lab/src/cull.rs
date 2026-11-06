//! The data-size cull (spec section 6). `data.sqlite3` is bounded: at the limit, whole runs of the lowest-value
//! ingest batches are deleted, oldest first, but only after their statistics have been distilled into
//! `memory.sqlite3` and read back. Every cull is logged; dry-run changes nothing.

use crate::config::Config;
use crate::db;
use crate::stats;
use crate::util::now_ts;
use anyhow::{ensure, Result};
use rusqlite::{params, Connection};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Default, Clone)]
pub struct CullReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub batches_touched: usize,
    pub runs_removed: usize,
    pub trade_rows_removed: usize,
    pub tiers: Vec<String>,
    pub dry_run: bool,
    pub backpressure: bool,
}

/// Free space inside the data database (free pages times page size).
fn free_bytes(data: &Connection) -> Result<u64> {
    let free: i64 = data.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    let ps: i64 = data.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok((free * ps) as u64)
}

/// Statistics preserved before raw rows go (spec section 6): per-feature quintile tables on de-duplicated entries.
fn distil_batch(mem: &Connection, data: &Connection, batch: i64) -> Result<()> {
    let names = db::registry(data)?;
    let cols: String = names.iter().map(|f| format!(", f.f_{f}")).collect();
    let sql = format!(
        "SELECT r.window_label, t.symbol, t.side, t.entry_ts, t.ret_margin{cols} FROM trades t \
         JOIN trade_features f ON f.run_id = t.run_id AND f.idx = t.idx JOIN runs r ON r.run_id = t.run_id WHERE r.batch = ?1 ORDER BY t.run_id, t.idx"
    );
    let mut stmt = data.prepare(&sql)?;
    let mut rows = stmt.query([batch])?;
    let mut seen = HashSet::new();
    let (mut ys, mut xs): (Vec<f64>, Vec<Vec<f64>>) = (Vec::new(), vec![Vec::new(); names.len()]);
    let mut windows: BTreeMap<String, usize> = BTreeMap::new();
    while let Some(r) = rows.next()? {
        if !seen.insert((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)) { continue; }
        *windows.entry(r.get::<_, String>(0)?).or_default() += 1;
        ys.push(r.get(4)?);
        for j in 0..names.len() { xs[j].push(r.get::<_, Option<f64>>(5 + j)?.unwrap_or(f64::NAN)); }
    }
    let mut feats = serde_json::Map::new();
    for (j, name) in names.iter().enumerate() {
        let cuts: Vec<f64> = (1..5).map(|k| stats::quantile(&xs[j], k as f64 / 5.0)).collect();
        let mut sums = [0.0f64; 5];
        let mut ns = [0usize; 5];
        for (x, y) in xs[j].iter().zip(&ys) {
            if !x.is_finite() { continue; }
            let z = cuts.iter().filter(|c| x > c).count();
            sums[z] += y;
            ns[z] += 1;
        }
        let means: Vec<f64> = (0..5).map(|z| if ns[z] > 0 { sums[z] / ns[z] as f64 } else { f64::NAN }).collect();
        feats.insert(name.to_string(), json!({"cut_points": cuts, "mean_outcome": means, "n": ns}));
    }
    let content = json!({"batch": batch, "unique_entries": ys.len(), "entries_by_window": windows, "outcome_mean": stats::mean(&ys), "features": feats});
    mem.execute("INSERT INTO aggregates(batch_id, spec_json, content_json, created) VALUES (?1, ?2, ?3, ?4)",
        params![batch, "{\"kind\":\"batch_quintiles\",\"version\":1}", serde_json::to_string(&content)?, now_ts()])?;
    // Read back and verify before anything is deleted.
    let back: String = mem.query_row("SELECT content_json FROM aggregates WHERE batch_id = ?1 ORDER BY id DESC LIMIT 1", [batch], |r| r.get(0))?;
    let parsed: serde_json::Value = serde_json::from_str(&back)?;
    ensure!(parsed["features"].as_object().map_or(0, |m| m.len()) == names.len(), "distillation read-back failed for batch {batch}");
    Ok(())
}

/// Whether the data database has reached its size limit.
pub fn needs_cull(cfg: &Config) -> bool { db::size_bytes(&cfg.data_db()) >= cfg.limits.data_db_limit_bytes }

/// Deletes the oldest raw batches (keeping the newest batches) until the data database is under its target, after distilling their statistics.
pub fn cull(cfg: &Config, mem: &Connection, data: &Connection, dry_run: bool, force: bool) -> Result<CullReport> {
    let started = std::time::Instant::now();
    let mut rep = CullReport { dry_run, bytes_before: db::size_bytes(&cfg.data_db()), ..Default::default() };
    rep.bytes_after = rep.bytes_before;
    if !force && rep.bytes_before < cfg.limits.data_db_limit_bytes { return Ok(rep); }
    let target = cfg.limits.data_db_target_bytes;

    let batches: Vec<i64> = {
        let mut s = data.prepare("SELECT id FROM ingest_batches WHERE deleted = 0 ORDER BY id")?;
        let v = s.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?;
        v
    };
    let deletable = batches.len().saturating_sub(cfg.limits.newest_batches_protected);
    let mut est = rep.bytes_before;
    let tx = data.unchecked_transaction()?;
    for &b in &batches[..deletable] {
        if est <= target { break; }
        // Tier A: runs that were never a top-20 search result or recorded top set.
        let victims: Vec<String> = {
            let mut s = tx.prepare("SELECT run_id FROM runs WHERE batch = ?1 AND NOT (role = 'recorded_top' OR (role = 'search_winner' AND rank IS NOT NULL AND rank <= 20))")?;
            let v = s.query_map([b], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
            v
        };
        if victims.is_empty() { continue; }
        if !dry_run { distil_batch(mem, &tx, b)?; tx.execute("UPDATE ingest_batches SET distilled = 1 WHERE id = ?1", [b])?; }
        for rid in &victims {
            rep.trade_rows_removed += tx.execute("DELETE FROM trade_features WHERE run_id = ?1", [rid])?;
            tx.execute("DELETE FROM trades WHERE run_id = ?1", [rid])?;
            tx.execute("DELETE FROM daily WHERE run_id = ?1", [rid])?;
            tx.execute("DELETE FROM runs WHERE run_id = ?1", [rid])?;
        }
        rep.runs_removed += victims.len();
        rep.batches_touched += 1;
        let left: i64 = tx.query_row("SELECT count(*) FROM runs WHERE batch = ?1", [b], |r| r.get(0))?;
        if left == 0 { tx.execute("UPDATE ingest_batches SET deleted = 1 WHERE id = ?1", [b])?; }
        est = rep.bytes_before.saturating_sub(free_bytes(&tx)?);
    }
    if rep.runs_removed > 0 { rep.tiers.push("A".into()); }

    rep.bytes_after = est;
    if dry_run {
        tx.rollback()?;
    } else {
        tx.commit()?;
        data.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
        rep.bytes_after = db::size_bytes(&cfg.data_db());
        rep.backpressure = rep.bytes_after > cfg.limits.data_db_limit_bytes;
    }
    mem.execute(
        "INSERT INTO cull_log(ts, bytes_before, bytes_after, batches_removed, rows_removed, tiers, dry_run, duration_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![now_ts(), rep.bytes_before as i64, rep.bytes_after as i64, rep.batches_touched as i64, rep.trade_rows_removed as i64,
            rep.tiers.join(","), dry_run as i64, started.elapsed().as_millis() as i64],
    )?;
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{self, SynthSpec};
    use crate::ingest;

    /// Five ingest batches (one per window), 30 runs each: 20 protected top runs and 10 deletable study runs per window.
    fn build() -> (tempfile::TempDir, Config, Connection, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::for_root(dir.path(), "synthetic");
        let mem = db::open(&cfg.memory_db(), true).unwrap();
        let mut data = db::open(&cfg.data_db(), false).unwrap();
        db::migrate_memory(&mem).unwrap();
        db::migrate_data(&data).unwrap();
        std::fs::create_dir_all(cfg.exports_dir()).unwrap();
        for w in 1..=5 {
            synth::generate(&cfg.exports_dir(), &SynthSpec { windows: 1, first_window: w, runs_per_window: 30, entries_per_day: 10, seed: 5, ..Default::default() }).unwrap();
            let r = ingest::ingest(&cfg, &mem, &mut data).unwrap();
            assert_eq!(r.ingested, 30);
        }
        data.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        (dir, cfg, mem, data)
    }

    /// Test helper: the single number returned by `sql`.
    fn count(c: &Connection, sql: &str) -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() }

    /// Test: under the limit nothing happens.
    #[test]
    fn under_the_limit_nothing_happens() {
        let (_d, cfg, mem, data) = build();
        let r = cull(&cfg, &mem, &data, false, false).unwrap();
        assert_eq!((r.runs_removed, r.batches_touched), (0, 0));
        assert_eq!(count(&data, "SELECT count(*) FROM runs"), 150);
    }

    /// Test: dry run reports but changes nothing.
    #[test]
    fn dry_run_reports_but_changes_nothing() {
        let (_d, mut cfg, mem, data) = build();
        let size = db::size_bytes(&cfg.data_db());
        cfg.limits.data_db_limit_bytes = size / 10 * 9;
        cfg.limits.data_db_target_bytes = size / 10 * 8;
        let r = cull(&cfg, &mem, &data, true, false).unwrap();
        assert!(r.runs_removed > 0 && r.dry_run);
        assert_eq!(count(&data, "SELECT count(*) FROM runs"), 150);
        assert_eq!(count(&mem, "SELECT count(*) FROM aggregates"), 0);
        assert_eq!(count(&mem, "SELECT count(*) FROM cull_log WHERE dry_run = 1"), 1);
    }

    /// Test: cull reaches target and never touches protected data.
    #[test]
    fn cull_reaches_target_and_never_touches_protected_data() {
        let (_d, mut cfg, mem, data) = build();
        let size = db::size_bytes(&cfg.data_db());
        cfg.limits.data_db_limit_bytes = size / 10 * 9;
        cfg.limits.data_db_target_bytes = size / 10 * 8;
        let r = cull(&cfg, &mem, &data, false, false).unwrap();
        assert!(r.runs_removed > 0, "{r:?}");
        assert!(r.bytes_after < size, "{} !< {size}", r.bytes_after);
        assert!(!r.backpressure);
        // protected: all top-20 search winners and the newest 3 batches (windows 3..5) are intact
        assert_eq!(count(&data, "SELECT count(*) FROM runs WHERE role = 'search_winner'"), 100);
        assert_eq!(count(&data, "SELECT count(*) FROM runs WHERE batch >= 3"), 90);
        // deleted runs were distilled first, with a verified aggregates row per touched batch
        let touched = count(&data, "SELECT count(*) FROM ingest_batches WHERE distilled = 1");
        assert_eq!(count(&mem, "SELECT count(DISTINCT batch_id) FROM aggregates"), touched);
        assert!(touched >= 1);
        // no orphans
        assert_eq!(count(&data, "SELECT count(*) FROM trades WHERE run_id NOT IN (SELECT run_id FROM runs)"), 0);
        assert_eq!(count(&data, "SELECT count(*) FROM trade_features WHERE run_id NOT IN (SELECT run_id FROM runs)"), 0);
        assert_eq!(count(&mem, "SELECT count(*) FROM cull_log WHERE dry_run = 0"), 1);
    }

    /// Test: distilled aggregates reproduce a headline number.
    #[test]
    fn distilled_aggregates_reproduce_a_headline_number() {
        let (_d, mut cfg, mem, data) = build();
        cfg.limits.data_db_limit_bytes = 1;
        cfg.limits.data_db_target_bytes = 0;
        // Headline number from the raw rows (first occurrence of each de-duplicated entry, as the distiller sees them)...
        let raw: f64 = data.query_row(
            "SELECT avg(ret_margin) FROM trades WHERE rowid IN (SELECT min(t.rowid) FROM trades t JOIN runs r ON r.run_id = t.run_id \
             WHERE r.batch = 1 GROUP BY r.window_label, t.symbol, t.side, t.entry_ts)", [], |r| r.get(0)).unwrap();
        cull(&cfg, &mem, &data, false, true).unwrap();
        let agg: String = mem.query_row("SELECT content_json FROM aggregates WHERE batch_id = 1", [], |r| r.get(0)).unwrap();
        let v: serde_json::Value = serde_json::from_str(&agg).unwrap();
        assert!(v["unique_entries"].as_u64().unwrap() > 100);
        // ...must be reproducible from the distilled aggregates after the raw rows are gone.
        assert!((v["outcome_mean"].as_f64().unwrap() - raw).abs() < 1e-12, "{} vs {raw}", v["outcome_mean"]);
        assert_eq!(v["features"]["vol_ratio"]["n"].as_array().unwrap().len(), 5);
    }
}

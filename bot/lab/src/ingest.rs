//! Ingest: manifest-driven, hash-verified, idempotent loading of export files into `data.sqlite3`.
//! Bad runs are quarantined with a reason; nothing is ever overwritten silently.

use crate::config::Config;
use crate::contract::{self, ManifestEntry, RunHeader};
use crate::db;
use crate::util::{canonical, now_ts, sha256_file, sha256_str};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct IngestReport {
    pub ingested: usize,
    pub skipped_known: usize,
    pub quarantined: usize,
    pub incomplete: usize,
    pub trades: usize,
}

#[derive(Default)]
struct RunFiles {
    header: Option<ManifestEntry>,
    trades: Option<ManifestEntry>,
    daily: Option<ManifestEntry>,
}

/// Imports every complete run in the exports folder (header, trades, daily), checking hashes and the privacy scrub; bad runs are quarantined.
pub fn ingest(cfg: &Config, mem: &Connection, data: &mut Connection) -> Result<IngestReport> {
    let dir = cfg.exports_dir();
    let mut report = IngestReport::default();
    let manifest_path = dir.join("MANIFEST.jsonl");
    if !manifest_path.exists() { return Ok(report); }
    let mut groups: BTreeMap<String, RunFiles> = BTreeMap::new();
    for (n, line) in std::fs::read_to_string(&manifest_path)?.lines().enumerate() {
        if line.trim().is_empty() { continue; }
        let e: ManifestEntry = match serde_json::from_str(line) { Ok(e) => e, Err(_) => { eprintln!("manifest line {} unreadable, ignored", n + 1); continue; } };
        let Some(run_id) = run_id_of(&e.file) else { continue };
        let g = groups.entry(run_id).or_default();
        match e.kind.as_str() { "header" => g.header = Some(e), "trades" => g.trades = Some(e), "daily" => g.daily = Some(e), _ => {} }
    }
    let batch_ts = now_ts();
    let mut batch: Option<i64> = None;
    let mut files_in_batch = 0usize;
    let mut bytes_in_batch = 0u64;
    for (run_id, g) in groups {
        let (Some(h), Some(t), Some(d)) = (g.header, g.trades, g.daily) else { report.incomplete += 1; continue };
        // Already handled (ingested or quarantined)? The header hash identifies the run content.
        let known: Option<String> = mem.query_row("SELECT status FROM ingest_log WHERE sha256 = ?1", [&h.sha256], |r| r.get(0)).ok();
        if known.is_some() { report.skipped_known += 1; continue; }
        let paths = [dir.join(&h.file), dir.join(&t.file), dir.join(&d.file)];
        if paths.iter().any(|p| !p.exists()) { report.incomplete += 1; continue; }
        match load_run(cfg, data, &run_id, &h, &t, &d, &dir, &mut batch, batch_ts) {
            Ok(n) => {
                report.ingested += 1;
                report.trades += n;
                files_in_batch += 3;
                bytes_in_batch += h.bytes + t.bytes + d.bytes;
                for e in [&h, &t, &d] { log(mem, &e.sha256, &e.file, &run_id, "ok", None)?; }
            }
            Err(err) => {
                report.quarantined += 1;
                let reason = format!("{err:#}");
                quarantine(cfg, &dir, &run_id, &[&h, &t, &d], &reason)?;
                for e in [&h, &t, &d] { log(mem, &e.sha256, &e.file, &run_id, "quarantined", Some(&reason))?; }
            }
        }
    }
    if let Some(b) = batch {
        data.execute("UPDATE ingest_batches SET files = ?1, bytes = ?2, runs = ?3 WHERE id = ?4", params![files_in_batch as i64, bytes_in_batch as i64, report.ingested as i64, b])?;
    }
    Ok(report)
}

/// Run id from an export file name (run_<id>_trades.csv, _daily.csv or .json).
fn run_id_of(file: &str) -> Option<String> {
    let stem = file.strip_prefix("run_")?;
    let stem = stem.strip_suffix("_trades.csv").or_else(|| stem.strip_suffix("_daily.csv")).or_else(|| stem.strip_suffix(".json"))?;
    Some(stem.to_string())
}

/// Records one file's ingest outcome (status and reason) in the memory log.
fn log(mem: &Connection, sha: &str, file: &str, run_id: &str, status: &str, reason: Option<&str>) -> Result<()> {
    mem.execute(
        "INSERT OR IGNORE INTO ingest_log(sha256, file, run_id, ingested_at, status, reason) VALUES (?1,?2,?3,?4,?5,?6)",
        params![sha, file, run_id, now_ts(), status, reason],
    )?;
    Ok(())
}

/// Moves a run's files into quarantine/<run_id> with the reason.
fn quarantine(cfg: &Config, dir: &Path, run_id: &str, files: &[&ManifestEntry], reason: &str) -> Result<()> {
    let q = cfg.quarantine_dir().join(run_id);
    std::fs::create_dir_all(&q)?;
    for f in files {
        let from = dir.join(&f.file);
        if from.exists() { std::fs::rename(&from, q.join(&f.file)).or_else(|_| std::fs::copy(&from, q.join(&f.file)).map(|_| ()))?; }
    }
    std::fs::write(q.join("reason.txt"), reason)?;
    Ok(())
}

/// Loads one run into the data database after checking every file's sha256 and scrubbing for forbidden fields.
#[allow(clippy::too_many_arguments)]
fn load_run(cfg: &Config, data: &mut Connection, run_id: &str, h: &ManifestEntry, t: &ManifestEntry, d: &ManifestEntry, dir: &Path, batch: &mut Option<i64>, batch_ts: i64) -> Result<usize> {
    for e in [h, t, d] {
        let actual = sha256_file(&dir.join(&e.file))?;
        anyhow::ensure!(actual == e.sha256, "sha256 mismatch for {}", e.file);
    }
    let header_text = std::fs::read_to_string(dir.join(&h.file))?;
    let trades_text = std::fs::read_to_string(dir.join(&t.file))?;
    let daily_text = std::fs::read_to_string(dir.join(&d.file))?;
    contract::scrub(&header_text).context("header")?;
    contract::scrub(&trades_text).context("trades")?;
    let header: RunHeader = serde_json::from_str(&header_text).context("header json")?;
    anyhow::ensure!(header.run_id == run_id, "run_id in header does not match file name");
    anyhow::ensure!(header.synthetic == (cfg.mode == "synthetic"), "this lab is in {} mode; the file is {}", cfg.mode, if header.synthetic { "synthetic" } else { "real" });
    let (trades, degraded) = contract::parse_trades(&trades_text).context("trades")?;
    let daily = contract::parse_daily(&daily_text).context("daily")?;
    contract::validate(&header, &trades, &daily)?;
    let exists: Option<String> = data.query_row("SELECT data_fingerprint FROM runs WHERE run_id = ?1", [run_id], |r| r.get(0)).ok();
    anyhow::ensure!(exists.is_none(), "run_id {run_id} already present with different content (conflict; never overwritten)");

    let tx = data.transaction()?;
    let b = match *batch {
        Some(b) => b,
        None => {
            tx.execute("INSERT INTO ingest_batches(ingested_at, files, bytes, runs) VALUES (?1, 0, 0, 0)", [batch_ts])?;
            let id = tx.last_insert_rowid();
            *batch = Some(id);
            id
        }
    };
    let params_json = canonical(&header.params);
    tx.execute(
        "INSERT INTO runs(run_id, batch, engine_commit, window_label, window_start, window_end, data_fingerprint, cost_model_version, role, rank, params_hash, params_json, metrics_json, return_pct, degraded, seed, created) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![run_id, b, header.engine_commit, header.window_label, header.window_start, header.window_end, header.data_fingerprint,
            header.cost_model_version, header.role, header.rank, sha256_str(&params_json), params_json, serde_json::to_string(&header.metrics)?,
            header.metrics.return_pct, degraded as i64, header.seed.map(|s| s as i64), now_ts()],
    )?;
    {
        let mut ins = tx.prepare("INSERT INTO trades(run_id, idx, symbol, side, entry_ts, exit_ts, entry_px, exit_px, qty, margin, fees, funding, pnl, ret_margin, reason, day, window) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)")?;
        let names: std::collections::BTreeSet<&String> = trades.iter().flat_map(|t| t.features.keys()).collect();
        for n in &names { db::ensure_feature(&tx, n, "export-file")?; }
        for (i, tr) in trades.iter().enumerate() {
            ins.execute(params![run_id, i as i64, tr.symbol, tr.side, tr.entry_ts, tr.exit_ts, tr.entry_px, tr.exit_px, tr.qty, tr.margin, tr.fees, tr.funding, tr.pnl, tr.ret_margin, tr.reason, tr.entry_ts / 86_400, header.window_label])?;
            // Only the features present on this row are written; absent ones stay NULL.
            let cols: String = tr.features.keys().map(|f| format!(", f_{f}")).collect();
            let marks: String = (0..tr.features.len()).map(|k| format!(", ?{}", k + 3)).collect();
            let mut vals: Vec<rusqlite::types::Value> = vec![run_id.to_string().into(), (i as i64).into()];
            vals.extend(tr.features.values().map(|v| rusqlite::types::Value::Real(*v)));
            tx.execute(&format!("INSERT INTO trade_features(run_id, idx{cols}) VALUES (?1, ?2{marks})"), rusqlite::params_from_iter(vals))?;
        }
        let mut insd = tx.prepare("INSERT INTO daily(run_id, day, equity, pnl) VALUES (?1,?2,?3,?4)")?;
        for r in &daily { insd.execute(params![run_id, r.day, r.equity, r.pnl])?; }
    }
    tx.commit()?;
    Ok(trades.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db, synth};

    /// Test fixture: an empty lab in `mode` with both databases migrated.
    fn setup(mode: &str) -> (tempfile::TempDir, Config, Connection, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::for_root(dir.path(), mode);
        let mem = db::open(&cfg.memory_db(), true).unwrap();
        let data = db::open(&cfg.data_db(), false).unwrap();
        db::migrate_memory(&mem).unwrap();
        db::migrate_data(&data).unwrap();
        std::fs::create_dir_all(cfg.exports_dir()).unwrap();
        (dir, cfg, mem, data)
    }

    /// Test fixture: a small one-window synthetic spec.
    fn small() -> synth::SynthSpec {
        synth::SynthSpec { windows: 1, days_per_window: 3, entries_per_day: 4, runs_per_window: 3, ..Default::default() }
    }

    /// Test: ingest is idempotent.
    #[test]
    fn ingest_is_idempotent() {
        let (_d, cfg, mem, mut data) = setup("synthetic");
        synth::generate(&cfg.exports_dir(), &small()).unwrap();
        let first = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!(first.ingested, 3);
        assert!(first.trades > 0);
        let count = |c: &Connection| c.query_row("SELECT count(*) FROM trades", [], |r| r.get::<_, i64>(0)).unwrap();
        let before = count(&data);
        let second = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!((second.ingested, second.skipped_known), (0, 3));
        assert_eq!(count(&data), before);
        let feats: i64 = data.query_row("SELECT count(*) FROM trade_features", [], |r| r.get(0)).unwrap();
        assert_eq!(feats, before);
    }

    /// Test: corrupted file is quarantined and others still load.
    #[test]
    fn corrupted_file_is_quarantined_and_others_still_load() {
        let (_d, cfg, mem, mut data) = setup("synthetic");
        synth::generate(&cfg.exports_dir(), &small()).unwrap();
        let victim = cfg.exports_dir().join("run_w1-r1_trades.csv");
        let mut text = std::fs::read_to_string(&victim).unwrap();
        text.push_str("tampered\n");
        std::fs::write(&victim, text).unwrap();
        let r = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!((r.ingested, r.quarantined), (2, 1));
        assert!(cfg.quarantine_dir().join("w1-r1").join("reason.txt").exists());
        let reason = std::fs::read_to_string(cfg.quarantine_dir().join("w1-r1").join("reason.txt")).unwrap();
        assert!(reason.contains("sha256 mismatch"), "{reason}");
        let again = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!((again.ingested, again.quarantined), (0, 0)); // quarantined runs are not retried forever
    }

    /// Test: partial run waits until its manifest lines arrive.
    #[test]
    fn partial_run_waits_until_its_manifest_lines_arrive() {
        let (_d, cfg, mem, mut data) = setup("synthetic");
        synth::generate(&cfg.exports_dir(), &small()).unwrap();
        let mp = cfg.exports_dir().join("MANIFEST.jsonl");
        let lines: Vec<String> = std::fs::read_to_string(&mp).unwrap().lines().map(String::from).collect();
        std::fs::write(&mp, lines[..lines.len() - 1].join("\n") + "\n").unwrap(); // last run lacks its daily line
        let r = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!((r.ingested, r.incomplete), (2, 1));
        std::fs::write(&mp, lines.join("\n") + "\n").unwrap();
        let r2 = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!(r2.ingested, 1);
    }

    /// Test: real mode lab refuses synthetic files.
    #[test]
    fn real_mode_lab_refuses_synthetic_files() {
        let (_d, cfg, mem, mut data) = setup("real");
        // write synthetic files into the *real* exports folder
        synth::generate(&cfg.exports_dir(), &small()).unwrap();
        let r = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!((r.ingested, r.quarantined), (0, 3));
        let reason = std::fs::read_to_string(cfg.quarantine_dir().join("w1-r0").join("reason.txt")).unwrap();
        assert!(reason.contains("real mode"), "{reason}");
    }

    /// Test: credentials in a file are rejected.
    #[test]
    fn credentials_in_a_file_are_rejected() {
        let (_d, cfg, mem, mut data) = setup("synthetic");
        synth::generate(&cfg.exports_dir(), &small()).unwrap();
        let p = cfg.exports_dir().join("run_w1-r0.json");
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        v["api_key"] = "abc".into();
        std::fs::write(&p, serde_json::to_vec(&v).unwrap()).unwrap();
        // re-hash so only the scrub (not the checksum) can reject it
        let mp = cfg.exports_dir().join("MANIFEST.jsonl");
        let fixed: Vec<String> = std::fs::read_to_string(&mp).unwrap().lines().map(|l| {
            let mut e: ManifestEntry = serde_json::from_str(l).unwrap();
            if e.file == "run_w1-r0.json" { e.sha256 = sha256_file(&p).unwrap(); }
            serde_json::to_string(&e).unwrap()
        }).collect();
        std::fs::write(&mp, fixed.join("\n") + "\n").unwrap();
        let r = ingest(&cfg, &mem, &mut data).unwrap();
        assert_eq!((r.ingested, r.quarantined), (2, 1));
        let reason = std::fs::read_to_string(cfg.quarantine_dir().join("w1-r0").join("reason.txt")).unwrap();
        assert!(reason.contains("privacy scrub"), "{reason}");
    }
}

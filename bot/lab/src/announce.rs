//! Tells the owner (Discord, via the already-approved `qwen-notify`) when the lab finds BETTER parameters -- in the backtest search or in the
//! AI learning (owner 2026-10-13: "it should update and post when better params are found, whether in backtest or ai learning").
//!
//! Four detectors, each remembered in the memory database table `announced` so nothing is posted twice and a restart never re-posts:
//!   1. a new best validated parameter set (holdout return up by at least `MIN_GAIN` points, or a new leader)
//!   2. a variant that newly reaches `validated` (passes the holdout on cut A and the alternate cut B)
//!   3. a new winning FORWARD run (unseen data) at or above the highlight threshold, with the lab's own explanation and its settings
//!   4. a GRPO training run whose adapter beats the base model (or was promoted)
//!
//! The first scan only RECORDS what already exists (plus one summary post), so switching this on does not flood the channel.
//! Numbers come straight from the lab's records; nothing is estimated here. Account details cannot appear: the messages hold strategy
//! settings and percentages only, and `qwen-notify` redacts account-like tokens anyway.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// A new best validated set must beat the announced one by this many holdout points (percent per 14-session block).
pub const MIN_GAIN: f64 = 0.25;
const MAX_PER_SCAN: usize = 3;

pub fn init(mem: &Connection) -> Result<()> {
    mem.execute_batch("CREATE TABLE IF NOT EXISTS announced (key TEXT PRIMARY KEY, value REAL NOT NULL DEFAULT 0, ts INTEGER NOT NULL);")?;
    Ok(())
}

fn get(mem: &Connection, key: &str) -> Result<Option<f64>> {
    Ok(mem.query_row("SELECT value FROM announced WHERE key = ?1", [key], |r| r.get(0)).optional()?)
}
fn put(mem: &Connection, key: &str, value: f64) -> Result<()> {
    mem.execute("INSERT INTO announced(key, value, ts) VALUES (?1, ?2, ?3) ON CONFLICT(key) DO UPDATE SET value = ?2, ts = ?3", params![key, value, crate::util::now_ts()])?;
    Ok(())
}

/// True when `now` is a real improvement over the last announced value (or there is none yet).
pub fn improved(prev: Option<f64>, now: f64) -> bool { prev.is_none_or(|p| now >= p + MIN_GAIN) }

fn f(v: &Value, k: &str) -> f64 { v[k].as_f64().unwrap_or(0.0) }
fn pct(v: &Value, k: &str) -> String { v[k].as_f64().map_or("?".into(), |x| format!("{x:+.2}%")) }

/// One leaderboard row as an announcement body (the "parameters" are the option tokens, e.g. `kind:kama+univ60`).
pub fn describe(row: &Value) -> String {
    let a = &row["a"]; let b = &row["b"]; let base = &row["baseline_a"];
    format!(
        "{}|{} @ {:.0} USD\nstatus: {}\nmean {} per 14-session block (training {}, holdout {})\nalternate cut: holdout {}\n{:.0}% of blocks positive, worst block {:+.1}%, {:.1} trades per block\nbaseline holdout: {}",
        row["interval"].as_str().unwrap_or("?"), row["tokens"].as_str().unwrap_or("?"), f(row, "capital"), row["status"].as_str().unwrap_or("?"),
        pct(a, "mean"), pct(a, "h1"), pct(a, "h2"), pct(b, "h2"), f(a, "pos"), f(a, "worst"), f(a, "trades"), pct(base, "h2"))
}

fn post(title: &str, body: &str) { crate::ops::discord(title, body); }

/// Runs all detectors; returns what was announced (also printed by the loop).
pub fn scan(mem: &Connection) -> Result<Vec<String>> {
    init(mem)?;
    let first = get(mem, "_seeded")?.is_none();
    let mut said = Vec::new();
    let lb = crate::strategy::leaderboard(mem)?;

    // 1. new best validated parameter set (leaderboard order: validated first, best holdout first)
    if let Some(best) = lb.iter().find(|r| r["status"] == "validated") {
        let h2 = f(&best["a"], "h2");
        let key = format!("best:{}@{:.0}", best["interval"].as_str().unwrap_or(""), f(best, "capital"));
        let ident = format!("{}|{}", best["interval"].as_str().unwrap_or(""), best["tokens"].as_str().unwrap_or(""));
        let prev = get(mem, &key)?;
        if first { put(mem, &key, h2)?; put(mem, &format!("{key}#id:{ident}"), 1.0)?; said.push(format!("recorded current best: {ident}")); post("Lab recording started: current best validated set", &describe(best)); }
        else if improved(prev, h2) {
            put(mem, &key, h2)?; put(mem, &format!("{key}#id:{ident}"), 1.0)?;
            let msg = format!("{}\n(previous best holdout: {})", describe(best), prev.map_or("none".into(), |p| format!("{p:+.2}%")));
            post("BETTER PARAMETERS: new best validated set (backtest search)", &msg); said.push(format!("new best {ident} holdout {h2:+.2}%"));
        }
    }

    // 2. variants that newly reach `validated`
    let mut n = 0usize; let mut extra = 0usize;
    for r in lb.iter().filter(|r| r["status"] == "validated") {
        let key = format!("validated:{}|{}@{:.0}", r["interval"].as_str().unwrap_or(""), r["tokens"].as_str().unwrap_or(""), f(r, "capital"));
        if get(mem, &key)?.is_some() { continue; }
        put(mem, &key, f(&r["a"], "h2"))?;
        if first { continue; }
        if n < MAX_PER_SCAN { post("Variant validated (passes holdout on both cuts)", &describe(r)); said.push(format!("validated {}", r["tokens"].as_str().unwrap_or(""))); n += 1; } else { extra += 1; }
    }
    if extra > 0 { post("More variants validated", &format!("{extra} further variants newly validated; see the dashboard / HIGHLIGHTS.md")); }

    // 3. new winning forward runs (unseen data) -- from the highlights table
    let mut st = mem.prepare("SELECT run_id, label, return_pct, why, params_json FROM highlights WHERE forward = 1 ORDER BY return_pct DESC")?;
    let rows: Vec<(String, String, f64, String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.collect::<Result<_, _>>()?;
    let mut n = 0usize;
    for (run_id, label, ret, why, params_json) in rows {
        let key = format!("hl:{run_id}");
        if get(mem, &key)?.is_some() { continue; }
        put(mem, &key, ret)?;
        if first || n >= MAX_PER_SCAN { continue; }
        let settings: String = params_json.chars().take(700).collect();
        post(&format!("Winning forward run: {ret:.1}% over 14 sessions"), &format!("{label}\n{why}\nsettings (found by the search): {settings}"));
        said.push(format!("forward run {ret:.1}% {label}")); n += 1;
    }

    // 4. AI learning: a training run whose adapter beats the base model, or one that was promoted
    let has_legacy_training: bool = mem.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'training_runs')", [], |r| r.get(0))?;
    if has_legacy_training {
        let mut st = mem.prepare("SELECT id, status, base_score, adapter_score, promoted, note FROM training_runs WHERE finished IS NOT NULL ORDER BY id")?;
        let runs: Vec<(i64, String, Option<f64>, Option<f64>, i64, Option<String>)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
        for (id, status, base, adapter, promoted, note) in runs {
            let key = format!("train:{id}");
            if get(mem, &key)?.is_some() { continue; }
            put(mem, &key, adapter.unwrap_or(0.0))?;
            if first { continue; }
            let better = matches!((base, adapter), (Some(b), Some(a)) if a > b);
            if better || promoted == 1 {
                post(if promoted == 1 { "AI learning: new model PROMOTED" } else { "AI learning: adapter beats the base model" },
                    &format!("training run {id} ({status})\nbase score {} -> adapter score {}\n{}", base.map_or("?".into(), |x| format!("{x:.3}")), adapter.map_or("?".into(), |x| format!("{x:.3}")), note.unwrap_or_default()));
                said.push(format!("training run {id} improved"));
            }
        }
    }
    if first { put(mem, "_seeded", 1.0)?; }
    Ok(said)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn improvement_needs_the_minimum_gain() {
        assert!(improved(None, -3.0), "the first value always counts");
        assert!(improved(Some(4.0), 4.25));
        assert!(!improved(Some(4.0), 4.24), "a hair above the old best is noise");
        assert!(!improved(Some(4.0), 3.0));
    }

    #[test]
    fn announcement_text_has_the_parameters_and_numbers() {
        let row = json!({"interval": "5h", "tokens": "kind:kama+univ60", "capital": 5000.0, "status": "validated",
            "a": {"mean": 7.82, "h1": 7.53, "h2": 8.10, "pos": 66.0, "worst": -15.9, "trades": 13.0}, "b": {"h2": 5.72}, "baseline_a": {"h2": 1.2}});
        let t = describe(&row);
        for want in ["5h|kind:kama+univ60 @ 5000 USD", "validated", "+7.82%", "holdout +8.10%", "66% of blocks positive", "-15.9%", "baseline holdout: +1.20%"] { assert!(t.contains(want), "missing {want} in {t}"); }
    }

    #[test]
    fn announced_state_survives_and_blocks_repeats() {
        let m = Connection::open_in_memory().unwrap();
        init(&m).unwrap();
        assert_eq!(get(&m, "k").unwrap(), None);
        put(&m, "k", 1.5).unwrap(); put(&m, "k", 2.5).unwrap();
        assert_eq!(get(&m, "k").unwrap(), Some(2.5));
    }

    #[test]
    fn scan_on_an_empty_lab_is_quiet() {
        let m = Connection::open_in_memory().unwrap();
        crate::highlights::init(&m).unwrap();
        assert!(scan(&m).unwrap().is_empty());
        assert_eq!(get(&m, "_seeded").unwrap(), Some(1.0), "the first scan seeds, so a later one announces only new things");
    }
}

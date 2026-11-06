//! Promotion requests (owner 2026-10-10: "if it passes always go live with the strat"; "claude makes code changes, not anything
//! else"; "make sure it doesn't disrupt paper trading live"). Code, no model; runs in every hourly tick (`qwen-lab promote`).
//!
//! A variant qualifies when it is "considerably better" on cuts A and B (`strategy::considerably_better`: validated, same candles
//! as the current strategy, unseen-block return >= +1.0 point per block on both cuts, win rate not lower, noisy tape positive)
//! AND passes the full validation (`strategy::full_validation`: all five block cuts measured, at most one failing), on the live
//! interval, with entry options only. Whoever proposed it (code or a Hermes agent): the owner said not to wait for approval.
//!
//! The lab never changes the bot. It writes ONE request at a time to `<lab>/promotions/pending/<variant>.json` with the evidence;
//! the bot repo's hourly promotion job (ops/promote.sh) runs a Claude Code session only while the US market is closed, which
//! changes the live strategy constant in code with comments (what differs, the numbers, why), tests, deploys and pushes, then
//! reports back (`qwen-lab promotion-status`). Requests and their outcome are kept in the `promotions` table.

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// The live interval (the bot trades 5h candles only).
pub const LIVE_INTERVAL: &str = "5h";

/// Entry options the bot's live strategy accepts (mirror of moomoo-bot `engine::live_options::is_live_token`).
pub fn is_live_token(tok: &str) -> bool {
    matches!(tok, "entrywide" | "entry0" | "cmf" | "vwap" | "obv" | "breadth" | "er" | "adx" | "crash" | "knife" | "tstep" | "losspause" | "rollstop"
        | "kind:ema" | "kind:hma" | "kind:kama" | "kind:donchian" | "kind:any"
        | "gate:ret" | "gate:dist" | "gate:vol" | "gate:atr" | "gate:slope" | "gate:spyvol" | "gate:rs" | "gate:any"
        | "grade:rank" | "grade:trend" | "grade:both" | "grade:risk" | "grade:dd" | "grade:riskdd"
        | "dca:off" | "dca:on" | "dca:tight" | "dca:wide" | "top12" | "top13" | "top1" | "top2")
        || tok.starts_with("tune:margin=") || tok.starts_with("tune:dca=")
}

/// Creates the promotions table if missing.
pub fn init(mem: &Connection) -> Result<()> {
    mem.execute_batch(
        "CREATE TABLE IF NOT EXISTS promotions (variant TEXT NOT NULL, regime TEXT NOT NULL, ts INTEGER NOT NULL, source TEXT NOT NULL,
            status TEXT NOT NULL, detail TEXT NOT NULL, evidence TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY(variant, regime));",
    )?;
    Ok(())
}

/// Variants that qualify for the live strategy now (best first), with the reason any considerably-better variant does not.
pub fn qualifying(mem: &Connection) -> Result<(Vec<Value>, Vec<String>)> {
    let (better, _) = crate::strategy::considerably_better(mem)?;
    let mut ok = Vec::new();
    let mut not = Vec::new();
    for b in better {
        let v = format!("{}|{}", b["interval"].as_str().unwrap_or(""), b["tokens"].as_str().unwrap_or(""));
        let fv = &b["full"];
        if b["interval"].as_str() != Some(LIVE_INTERVAL) {
            not.push(format!("{v}: not the live interval ({LIVE_INTERVAL}); a candle-size change is the owner's decision"));
        } else if !b["tokens"].as_str().unwrap_or("").split('+').all(is_live_token) {
            not.push(format!("{v}: uses an option that cannot go live by itself (universe or research switch): needs a code change review"));
        } else if !fv["complete"].as_bool().unwrap_or(false) {
            not.push(format!("{v}: full validation still running ({} of 5 block cuts measured)", fv["measured"]));
        } else if !fv["robust"].as_bool().unwrap_or(false) {
            not.push(format!("{v}: failed the full validation (passes {} of 5 block cuts; at least 4 needed)", fv["passed"]));
        } else {
            ok.push(b);
        }
    }
    // the smallest unseen-block gain over the two cuts decides which goes first
    let min_gain = |b: &Value| b["cuts"].as_array().map(|c| c.iter().filter_map(|x| x["gain"].as_f64()).fold(f64::MAX, f64::min)).unwrap_or(0.0);
    ok.sort_by(|a, b| min_gain(b).total_cmp(&min_gain(a)));
    Ok((ok, not))
}

/// One promotion pass: writes a request for the best qualifying variant unless a request is already pending. Returns notes.
pub fn run(mem: &Connection, lab_root: &std::path::Path) -> Result<Vec<String>> {
    init(mem)?;
    let pending_dir = lab_root.join("promotions").join("pending");
    std::fs::create_dir_all(&pending_dir)?;
    std::fs::create_dir_all(lab_root.join("promotions").join("done"))?;
    let (ok, not) = qualifying(mem)?;
    let mut notes: Vec<String> = not;
    let open: Vec<String> = std::fs::read_dir(&pending_dir)?.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string())).filter(|f| f.ends_with(".json")).collect();
    if !open.is_empty() {
        notes.push(format!("a promotion request is already pending ({}): one at a time", open.join(", ")));
        return Ok(notes);
    }
    let regime = crate::strategy::current_regime(mem);
    for b in ok {
        let iv = b["interval"].as_str().unwrap_or("");
        let toks = b["tokens"].as_str().unwrap_or("");
        let variant = format!("{iv}|{toks}");
        let done: Option<String> = mem.query_row("SELECT status FROM promotions WHERE variant = ?1 AND regime = ?2", params![variant, regime], |r| r.get(0)).ok();
        if done.is_some() {
            continue;
        }
        let source: String = mem.query_row("SELECT source FROM variant_origin WHERE interval = ?1 AND tokens = ?2", params![iv, toks], |r| r.get(0)).unwrap_or_else(|_| "not recorded".into());
        let current: Vec<String> = regime.split('+').filter(|x| !x.is_empty()).map(String::from).collect();
        let mut new_options = current.clone();
        for t in toks.split('+') {
            if !new_options.iter().any(|x| x == t) { new_options.push(t.to_string()); }
        }
        let req = json!({
            "variant": variant, "interval": iv, "add_options": toks.split('+').collect::<Vec<_>>(), "current_options": current,
            "new_options": new_options, "source": source, "requested_ts": crate::util::now_ts(),
            "evidence": {"cuts_a_b": b["cuts"], "full_validation": b["full"]},
            "rule": "considerably better on cuts A and B (>= +1.0 pt/block unseen, win rate not lower, noisy positive) and passes the full validation (>= 4 of 5 block cuts)",
        });
        let file = pending_dir.join(format!("{}.json", variant.replace(['|', '+', ':', '='], "_")));
        std::fs::write(&file, serde_json::to_string_pretty(&req)?)?;
        mem.execute("INSERT OR REPLACE INTO promotions(variant, regime, ts, source, status, detail, evidence, updated) VALUES (?1,?2,?3,?4,'requested',?5,?6,?3)",
            params![variant, regime, crate::util::now_ts(), source, format!("request {}", file.display()), req["evidence"].to_string()])?;
        notes.push(format!("PROMOTION REQUESTED: {variant} (found by {source}): live options {:?} -> {:?}; the bot's promotion job applies it while the market is closed", current, new_options));
        break;
    }
    Ok(notes)
}

/// Records the outcome reported by the bot's promotion job ("live" with the commit, or "failed" with the reason), and moves
/// the request out of pending.
pub fn set_status(mem: &Connection, lab_root: &std::path::Path, variant: &str, status: &str, detail: &str) -> Result<()> {
    init(mem)?;
    anyhow::ensure!(matches!(status, "live" | "failed" | "reverted"), "status must be live, failed or reverted");
    let n = mem.execute("UPDATE promotions SET status = ?1, detail = ?2, updated = ?3 WHERE variant = ?4 AND status IN ('requested', 'live')",
        params![status, detail.chars().take(500).collect::<String>(), crate::util::now_ts(), variant])?;
    anyhow::ensure!(n > 0, "no open promotion for {variant}");
    let name = format!("{}.json", variant.replace(['|', '+', ':', '='], "_"));
    let from = lab_root.join("promotions").join("pending").join(&name);
    if from.exists() {
        std::fs::rename(&from, lab_root.join("promotions").join("done").join(format!("{}-{status}-{name}", crate::util::now_ts()))).context("move request")?;
    }
    Ok(())
}

/// Every promotion (newest first), for the dashboard.
pub fn list(mem: &Connection, limit: usize) -> Vec<Value> {
    mem.prepare("SELECT variant, regime, ts, source, status, detail, updated FROM promotions ORDER BY updated DESC LIMIT ?1")
        .and_then(|mut st| st.query_map([limit as i64], |r| Ok(json!({"variant": r.get::<_, String>(0)?, "regime": r.get::<_, String>(1)?, "ts": r.get::<_, i64>(2)?,
            "source": r.get::<_, String>(3)?, "status": r.get::<_, String>(4)?, "detail": r.get::<_, String>(5)?, "updated": r.get::<_, i64>(6)?})))?.collect::<Result<Vec<_>, _>>())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn study(lines: &[(&str, &str)]) -> String {
        let mut s = String::from("5h start capital 5000: 28 blocks, SPY buy&hold mean +2.49%\nvariant mean % median % blocks+ worst % 1st half 2nd half trades noisy 1st noisy 2nd noisy+ chained win1 win2\nlive none\n");
        for (n, row) in lines {
            s.push_str(&format!("{n} {row}\n"));
        }
        s
    }

    /// Test: a variant needs both cuts considerably better AND the full validation; then exactly one request is written, the
    /// status comes back from the bot's job, and a promoted variant is not requested twice.
    #[test]
    fn only_fully_validated_variants_are_requested_once() {
        let dir = tempfile::tempdir().unwrap();
        let m = Connection::open_in_memory().unwrap();
        crate::strategy::init(&m).unwrap();
        let base = "+2.00 +1.00 60% -6.00 +2.00 +2.00 9.0 +1.50 +1.50 55% +20.0 46% 46%";
        let good = "+3.50 +2.00 60% -5.00 +3.00 +4.00 8.0 +2.00 +2.50 55% +40.0 50% 49%";
        let bad = "+1.00 +0.00 40% -9.00 +1.00 +0.50 8.0 +0.50 +0.20 40% +5.0 44% 40%";
        for cut in ["A", "B"] {
            crate::strategy::import_output_cut(&m, &study(&[("baseline", base), ("obv", good)]), cut).unwrap();
        }
        let (ok, not) = qualifying(&m).unwrap();
        assert!(ok.is_empty() && not.iter().any(|n| n.contains("still running")), "{not:?}");
        // cuts C, D, E: obv fails two of them -> not robust
        crate::strategy::import_output_cut(&m, &study(&[("baseline", base), ("obv", good)]), "C").unwrap();
        crate::strategy::import_output_cut(&m, &study(&[("baseline", base), ("obv", bad)]), "D").unwrap();
        crate::strategy::import_output_cut(&m, &study(&[("baseline", base), ("obv", bad)]), "E").unwrap();
        assert!(qualifying(&m).unwrap().1.iter().any(|n| n.contains("failed the full validation")));
        crate::strategy::import_output_cut(&m, &study(&[("baseline", base), ("obv", good)]), "D").unwrap();
        let notes = run(&m, dir.path()).unwrap();
        assert!(notes.iter().any(|n| n.contains("PROMOTION REQUESTED: 5h|obv")), "{notes:?}");
        assert!(run(&m, dir.path()).unwrap().iter().any(|n| n.contains("already pending")));
        set_status(&m, dir.path(), "5h|obv", "live", "commit abc").unwrap();
        assert!(!run(&m, dir.path()).unwrap().iter().any(|n| n.contains("PROMOTION REQUESTED")), "a promoted variant is not requested again");
        assert_eq!(list(&m, 5)[0]["status"], "live");
        assert_eq!(std::fs::read_dir(dir.path().join("promotions/done")).unwrap().count(), 1);
    }
}

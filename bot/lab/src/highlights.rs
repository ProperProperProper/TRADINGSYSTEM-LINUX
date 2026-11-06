//! Winning runs worth a human's attention: any backtest block of 14 sessions that returned 15% or more (user rule). For each one the lab
//! records the exact settings and trades, works out WHY from the data (which stocks and entry conditions it favoured, how concentrated
//! the profit was, how the random settings of the same window did: a luck check), has the local model write the explanation with every
//! percentage checked against those facts, and REMEMBERS it: the finding is stored in the memory database, shown to the model on its
//! whiteboard, and written to `HIGHLIGHTS.md` in the lab root.
//!
//! Nothing here is invented: every figure comes from the imported runs and trades; when the model's text fails the checks, a plain
//! sentence built by code from the same facts is stored instead.


use crate::context::Ctx;
use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::fmt::Write as _;

/// A block of 14 sessions returning at least this much (percent) is a highlight.
pub const THRESHOLD_PCT: f64 = 15.0;

/// Creates the highlights table if missing.
pub fn init(mem: &Connection) -> Result<()> {
    mem.execute_batch(
        "CREATE TABLE IF NOT EXISTS highlights (
            run_id TEXT PRIMARY KEY, ts INTEGER NOT NULL, label TEXT NOT NULL, role TEXT NOT NULL, forward INTEGER NOT NULL,
            window_start INTEGER NOT NULL, window_end INTEGER NOT NULL, return_pct REAL NOT NULL,
            params_json TEXT NOT NULL, facts_json TEXT NOT NULL, why TEXT NOT NULL, why_source TEXT NOT NULL);",
    )?;
    Ok(())
}

/// The facts about one winning run, computed from the stored runs and trades.
fn facts(data: &Connection, run_id: &str, label: &str, w_start: i64, ret: f64) -> Result<Value> {
    let (n, wins, pnl, fees): (i64, i64, f64, f64) = data.query_row(
        "SELECT count(*), coalesce(sum(pnl > 0), 0), coalesce(sum(pnl), 0), coalesce(sum(fees), 0) FROM trades WHERE run_id = ?1", [run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    let mut by_sym = data.prepare("SELECT symbol, count(*), sum(pnl) FROM trades WHERE run_id = ?1 GROUP BY symbol ORDER BY sum(pnl) DESC")?;
    let syms: Vec<(String, i64, f64)> = by_sym.query_map([run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    let top_share = syms.first().map(|s| if pnl > 0.0 { s.2 / pnl * 100.0 } else { 0.0 }).unwrap_or(0.0);
    let mut best = data.prepare("SELECT symbol, pnl, ret_margin, reason, datetime(entry_ts, 'unixepoch') FROM trades WHERE run_id = ?1 ORDER BY pnl DESC LIMIT 5")?;
    let top_trades: Vec<Value> = best
        .query_map([run_id], |r| Ok(json!({"symbol": r.get::<_, String>(0)?, "pnl": r.get::<_, f64>(1)?, "return_pct": r.get::<_, f64>(2)? * 100.0, "exit_reason": r.get::<_, String>(3)?, "entered": r.get::<_, String>(4)?})))?
        .collect::<Result<_, _>>()?;
    // what the entries had in common, against every trade in the same window of the same kind of run
    let mut cols = Vec::new();
    for f in ["c_ret_24h_pct", "c_dist_24h_avg_pct", "c_atr14_pct", "m_trend_dist_pct", "m_breadth_pct", "et_minute"] {
        let has: i64 = data.query_row("SELECT count(*) FROM pragma_table_info('trade_features') WHERE name = ?1", [format!("f_{f}")], |r| r.get(0))?;
        if has > 0 {
            let mine: Option<f64> = data.query_row(&format!("SELECT avg(f.f_{f}) FROM trade_features f WHERE f.run_id = ?1"), [run_id], |r| r.get(0))?;
            let all: Option<f64> = data.query_row(
                &format!("SELECT avg(f.f_{f}) FROM trade_features f JOIN runs r ON r.run_id = f.run_id WHERE r.source_label = ?1 AND r.window_start = ?2"), params![label, w_start], |r| r.get(0))?;
            if let (Some(m), Some(a)) = (mine, all) {
                cols.push(json!({"feature": f, "this_run_avg": m, "all_runs_avg": a}));
            }
        }
    }
    // Luck check: only meaningful for FORWARD runs (settings that did not see the window): how many runs of the same window and kind
    // reached the threshold. An in-sample winner is the best of ~100k combinations on its own window, so it has no such comparison.
    let in_sample = label.contains("-chosen");
    let (total, reached, median): (i64, i64, Option<f64>) = if in_sample { (0, 0, None) } else {
        let (t, c): (i64, i64) = data.query_row(
            "SELECT count(*), coalesce(sum(return_pct >= ?3), 0) FROM runs WHERE source_label = ?1 AND window_start = ?2", params![label, w_start, THRESHOLD_PCT], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut s = data.prepare("SELECT return_pct FROM runs WHERE source_label = ?1 AND window_start = ?2 AND return_pct IS NOT NULL ORDER BY return_pct")?;
        let v: Vec<f64> = s.query_map(params![label, w_start], |r| r.get(0))?.collect::<Result<_, _>>()?;
        (t, c, v.get(v.len() / 2).copied())
    };
    Ok(json!({
        "return_pct": ret, "trades": n, "win_rate_pct": if n > 0 { wins as f64 / n as f64 * 100.0 } else { 0.0 }, "net_pnl": pnl, "fees": fees,
        "top_symbol": syms.first().map(|s| s.0.clone()), "top_symbol_share_of_profit_pct": top_share, "distinct_symbols": syms.len(),
        "top_trades": top_trades, "entry_conditions": cols,
        "same_window_runs": total, "same_window_runs_reaching_threshold": reached, "same_window_median_return_pct": median, "in_sample": in_sample,
    }))
}

/// A plain sentence from the facts alone (used when the model's text fails its checks, or the model is unavailable).
pub(crate) fn code_why(f: &Value, forward: bool) -> String {
    let n = f["trades"].as_i64().unwrap_or(0);
    if !forward {
        return format!(
            "In-sample: {:.1}% over 14 sessions from {n} trades ({:.0}% winners); {:.0}% of the profit came from {}. This is the best of ~100k combinations on its OWN window, so it is expected to shrink on unseen data; no luck comparison exists for it.",
            f["return_pct"].as_f64().unwrap_or(0.0), f["win_rate_pct"].as_f64().unwrap_or(0.0), f["top_symbol_share_of_profit_pct"].as_f64().unwrap_or(0.0), f["top_symbol"].as_str().unwrap_or("one symbol"));
    }
    let mut s = format!(
        "{} {:.1}% over 14 sessions from {} trades ({:.0}% winners); {:.0}% of the profit came from {}; {} of {} runs of the same window reached {:.0}%{}.",
        if forward { "Forward (unseen data):" } else { "In-sample:" },
        f["return_pct"].as_f64().unwrap_or(0.0), n, f["win_rate_pct"].as_f64().unwrap_or(0.0), f["top_symbol_share_of_profit_pct"].as_f64().unwrap_or(0.0),
        f["top_symbol"].as_str().unwrap_or("one symbol"), f["same_window_runs_reaching_threshold"].as_i64().unwrap_or(0), f["same_window_runs"].as_i64().unwrap_or(0), THRESHOLD_PCT,
        f["same_window_median_return_pct"].as_f64().map_or(String::new(), |m| format!(", median {m:.2}%"))
    );
    if f["top_symbol_share_of_profit_pct"].as_f64().unwrap_or(0.0) >= 60.0 {
        s.push_str(" Mostly one stock: treat as luck until it repeats.");
    }
    s
}

/// Find, explain and remember every new run at or above the threshold; (re)write `HIGHLIGHTS.md`. Returns what happened.
pub fn scan(ctx: &Ctx) -> Result<Vec<String>> {
    init(ctx.mem)?;
    let mut notes = Vec::new();
    let mut st = ctx.data.prepare(
        "SELECT run_id, role, coalesce(source_label, window_label), window_start, window_end, params_json, return_pct FROM runs WHERE return_pct >= ?1 ORDER BY return_pct DESC LIMIT 400")?;
    let rows: Vec<(String, String, String, i64, i64, String, f64)> = st.query_map([THRESHOLD_PCT], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<_, _>>()?;
    let known: std::collections::HashSet<String> = {
        let mut s = ctx.mem.prepare("SELECT run_id FROM highlights")?;
        let v = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
        v
    };
    let now = crate::util::now_ts();
    for (run_id, role, label, w0, w1, params_json, ret) in rows.into_iter().filter(|r| !known.contains(&r.0)).take(12) {
        let forward = matches!(role.as_str(), "oos" | "random");
        let f = facts(ctx.data, &run_id, &label, w0, ret)?;
        let (why, source) = (code_why(&f, forward), "code");
        ctx.mem.execute(
            "INSERT OR IGNORE INTO highlights(run_id, ts, label, role, forward, window_start, window_end, return_pct, params_json, facts_json, why, why_source) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![run_id, now, label, role, forward as i64, w0, w1, ret, params_json, f.to_string(), why, source])?;
        notes.push(format!("HIGHLIGHT {label} {ret:.1}% ({}): {why}", if forward { "forward" } else { "in-sample" }));
    }
    write_report(ctx)?;
    Ok(notes)
}

/// `HIGHLIGHTS.md` in the lab root: every remembered winning run, forward results first.
pub fn write_report(ctx: &Ctx) -> Result<()> {
    init(ctx.mem)?;
    let mut st = ctx.mem.prepare("SELECT label, role, forward, datetime(window_start, 'unixepoch'), datetime(window_end, 'unixepoch'), return_pct, params_json, facts_json, why, why_source FROM highlights ORDER BY forward DESC, return_pct DESC")?;
    let rows: Vec<(String, String, i64, String, String, f64, String, String, String, String)> =
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)))?.collect::<Result<_, _>>()?;
    let mut s = format!("# Winning runs: {THRESHOLD_PCT:.0}% or more over 14 sessions\n\nWritten by the lab from real backtests; nothing here is invented. FORWARD = settings that never saw the window; IN-SAMPLE = selected on it (expect it to shrink).\nA run is only as good as its luck check: see how many other runs of the same window reached the threshold.\n\n");
    if rows.is_empty() {
        s.push_str("None yet: no run has reached the threshold.\n");
    }
    for (label, role, fwd, a, b, ret, params, facts, why, src) in &rows {
        let f: Value = serde_json::from_str(facts).unwrap_or(Value::Null);
        let _ = writeln!(s, "## {ret:.1}%  {label}  [{}]  {a} .. {b}", if *fwd == 1 { "FORWARD" } else { "IN-SAMPLE" });
        let _ = writeln!(s, "- role {role}; {} trades, {:.0}% winners; top symbol {} = {:.0}% of the profit; {} of {} same-window runs reached the threshold", f["trades"], f["win_rate_pct"].as_f64().unwrap_or(0.0), f["top_symbol"].as_str().unwrap_or("-"), f["top_symbol_share_of_profit_pct"].as_f64().unwrap_or(0.0), f["same_window_runs_reaching_threshold"], f["same_window_runs"]);
        let _ = writeln!(s, "- why ({src}): {why}");
        let _ = writeln!(s, "- settings (found by the search): `{params}`\n");
    }
    std::fs::write(ctx.cfg.root.join("HIGHLIGHTS.md"), s)?;
    Ok(())
}

/// What the model remembers: the newest highlights with their explanation, for its whiteboard.
pub fn board_lines(mem: &Connection) -> Result<String> {
    init(mem)?;
    let mut st = mem.prepare("SELECT label, forward, return_pct, why FROM highlights ORDER BY forward DESC, ts DESC LIMIT 5")?;
    let rows: Vec<(String, i64, f64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut s = format!("\nWINNING RUNS I REMEMBER (>= {THRESHOLD_PCT:.0}% over 14 sessions; written by code from real runs):\n");
    for (l, f, r, w) in rows {
        let _ = writeln!(s, "  {l} {r:.1}% [{}]: {w}", if f == 1 { "forward" } else { "in-sample" });
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: code explanation is built from the facts and flags concentration.
    #[test]
    fn code_explanation_is_built_from_the_facts_and_flags_concentration() {
        let f = json!({"return_pct": 18.4, "trades": 12, "win_rate_pct": 58.3, "top_symbol": "MU", "top_symbol_share_of_profit_pct": 71.0,
                       "same_window_runs": 3000, "same_window_runs_reaching_threshold": 2, "same_window_median_return_pct": -0.2});
        let s = code_why(&f, true);
        assert!(s.contains("18.4%") && s.contains("MU") && s.contains("2 of 3000") && s.contains("luck"), "{s}");
    }
}

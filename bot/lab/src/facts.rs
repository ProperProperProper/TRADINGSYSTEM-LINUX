//! HARD RULE (owner 2026-10-11): the lab never invents data or results, ever.
//!
//! Enforced by code, not by asking nicely:
//! - [`known`] is every number the lab's tools can show the brain (results, win rates, trades, the win/lose analysis, tuning facts, options,
//!   highlights' facts, the research search results it was given, stored in `research_cache`);
//! - [`gate`] refuses any text the brain wants to store or send (notes, test reasons, research ideas, whiteboard edits) that contains a
//!   decimal or percentage number found in none of them (rounding a real number is fine; anything else is refused, costs [`PENALTY`]
//!   points, and nothing is stored);
//! - [`scrub_stored`] removes such numbers from text already stored and shown on the dashboard (hourly, in the tick).
//!
//! Plain integers (counts, dates, candle sizes like 5h) are not checked; decimals and percentages are what results are made of.

use crate::context::Ctx;
use anyhow::{bail, Result};
use rusqlite::{params, Connection};
use serde_json::json;

/// Points lost each time a role is caught inventing (owner: "punish when it gets caught"): as large as a validated result's reward.
pub const PENALTY: f64 = -10.0;

/// Creates the research cache (the search results the brain was shown, kept 14 days, so numbers they quote from a paper can
/// be verified).
pub fn init(mem: &Connection) -> Result<()> {
    mem.execute_batch("CREATE TABLE IF NOT EXISTS research_cache (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, text TEXT NOT NULL);")?;
    Ok(())
}

/// Stores search results the brain was shown.
pub fn remember_search(mem: &Connection, text: &str) -> Result<()> {
    init(mem)?;
    let now = crate::util::now_ts();
    mem.execute("INSERT INTO research_cache(ts, text) VALUES (?1, ?2)", params![now, text])?;
    mem.execute("DELETE FROM research_cache WHERE ts < ?1", [now - 14 * 86_400])?;
    Ok(())
}

/// The decimal and percentage numbers in `text`: (absolute value, decimals written).
pub fn numbers(text: &str) -> Vec<(f64, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // a digit right after a letter, '_', '.', ':', '=', '|' or after "<digit>-" (a range like 0.40-0.70, a date) is part of a token
        let range_tail = i >= 2 && b[i - 1] == b'-' && b[i - 2].is_ascii_digit();
        if b[i].is_ascii_digit() && !range_tail && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.' || b[i - 1] == b':' || b[i - 1] == b'=' || b[i - 1] == b'|')) {
            let s = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            let num = text[s..i].trim_end_matches('.');
            let pct = i < b.len() && b[i] == b'%';
            // part of an identifier or a token (e.g. tune:margin=0-10, 2026-10-10, 5h|obv): not a quoted result
            let glued = i < b.len() && (b[i].is_ascii_alphabetic() || b[i] == b'-' || b[i] == b'_' || b[i] == b'|');
            if (num.contains('.') || pct) && !glued {
                if let Ok(v) = num.parse::<f64>() {
                    out.push((v.abs(), num.split_once('.').map_or(0, |x| x.1.len())));
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// True for an error that means the lab could not be read (so the known numbers would be incomplete).
fn is_read_failure(e: &str) -> bool {
    let e = e.to_lowercase();
    ["locked", "busy", "i/o", "unable to open", "malformed", "not a database", "readonly", "out of memory"].iter().any(|k| e.contains(k))
}

/// Every number the lab's tools can show the brain (absolute values), see the module docs. An error when any source cannot be
/// read: an incomplete list would make real numbers look invented (2026-10-11 a restore removed real numbers such as the
/// current strategy's -0.71%/block on 1d), so every caller then changes nothing and penalises no one.
pub fn known(ctx: &Ctx) -> Result<Vec<f64>> {
    init(ctx.mem)?;
    let mut texts: Vec<String> = Vec::new();
    for (tool, args) in [("lab_experiments", json!({"n": 1000})), ("lab_winrate", json!({})), ("lab_tuning_facts", json!({})), ("lab_options", json!({})), ("lab_rules", json!({})), ("lab_scoreboard", json!({})), ("lab_quant", json!({})), ("lab_entry_gating", json!({})), ("lab_untested", json!({"n": 60}))] {
        match crate::labtools::call(ctx, tool, &args) {
            Ok(t) => texts.push(t),
            // a failure to READ (database locked or busy, I/O, corruption) stops the check: the list would be incomplete. A tool
            // saying it has nothing to show yet ("no such table", "too few runs ...") shows no numbers, so it adds none.
            Err(e) if is_read_failure(&format!("{e:#}")) => bail!("known numbers incomplete: {tool} failed: {e:#}"),
            Err(_) => {}
        }
    }
    texts.push(crate::agents::call_tool(ctx, "lab_best", &json!({"n": 200}), false).map_err(|e| anyhow::anyhow!("known numbers incomplete: lab_best failed: {e:#}"))?);
    texts.push(std::fs::read_to_string(ctx.cfg.root.join("HIGHLIGHTS.md")).unwrap_or_default()); // absent until the first highlight
    crate::highlights::init(ctx.mem)?;
    texts.extend(ctx.mem.prepare("SELECT facts_json FROM highlights")?.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?);
    texts.extend(ctx.mem.prepare("SELECT text FROM research_cache")?.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?);
    let mut out: Vec<f64> = texts.iter().flat_map(|t| numbers(t).into_iter().map(|x| x.0)).collect();
    // the brain training-view numbers straight from the results (training half only: h1, noisy h1, win rate, trades, mean)
    crate::strategy::init(ctx.mem)?;
    let rows = ctx.mem.prepare("SELECT h1, nh1, win1, trades, mean FROM strategy_runs WHERE cut = 'A'")?
        .query_map([], |r| Ok([r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, Option<f64>>(3)?, r.get::<_, Option<f64>>(4)?]))?
        .collect::<Result<Vec<_>, _>>()?;
    out.extend(rows.into_iter().flat_map(|r| r.into_iter().flatten().map(f64::abs)));
    out.sort_by(f64::total_cmp);
    out.dedup();
    // a number the lab showed once stays known: results are re-measured (new candles overwrite a row), and a real number an agent
    // copied yesterday is not an invention today (it may be out of date; the claim check judges statuses against the records)
    ctx.mem.execute_batch("CREATE TABLE IF NOT EXISTS known_numbers (value REAL PRIMARY KEY, first_seen INTEGER NOT NULL);")?;
    let now = crate::util::now_ts();
    for v in &out {
        ctx.mem.execute("INSERT OR IGNORE INTO known_numbers(value, first_seen) VALUES (?1, ?2)", params![v, now])?;
    }
    let mut all: Vec<f64> = ctx.mem.prepare("SELECT value FROM known_numbers")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    all.sort_by(f64::total_cmp);
    Ok(all)
}

/// The numbers in `text` that match no known number (a known number rounded to the decimals written counts as a match).
pub fn unverified(known: &[f64], text: &str) -> Vec<String> {
    let mut bad = Vec::new();
    for (v, d) in numbers(text) {
        let scale = 10f64.powi(d as i32);
        let ok = known.iter().any(|k| ((k * scale).round() - (v * scale).round()).abs() < 0.5 || (k - v).abs() < 1e-9);
        if !ok {
            let s = if d == 0 { format!("{v}") } else { format!("{v:.d$}") };
            if !bad.contains(&s) { bad.push(s); }
        }
    }
    bad
}

/// Refuses `text` from `author` when it contains a number the lab never showed (costs 10 points; nothing is stored).
pub fn gate(ctx: &Ctx, author: &str, what: &str, text: &str) -> Result<()> {
    if crate::guard::is_trusted(author) {
        return Ok(());
    }
    if numbers(text).is_empty() {
        return Ok(());
    }
    // the list of known numbers could not be built (the lab busy): nothing is judged, nothing stored, no penalty
    let k = known(ctx).map_err(|e| anyhow::anyhow!("could not verify the numbers right now ({e:#}); nothing was stored and there is no penalty: try again in a moment"))?;
    let bad = unverified(&k, text);
    if bad.is_empty() {
        return Ok(());
    }
    crate::labtools::init(ctx.mem)?;
    ctx.mem.execute("INSERT INTO reward_events(ts, author, kind, points, detail) VALUES (?1, ?2, 'invented numbers', ?3, ?4)",
        params![crate::util::now_ts(), author, PENALTY, format!("{what}: {}", bad.join(", "))])?;
    bail!("REFUSED (hard rule: never invent data or results): {} not found in any lab data you were shown. You lost 10 points. Copy numbers exactly as the tools print them, or leave them out.", bad.join(", "))
}

/// Replaces every unverified number in `text` with "[unverified number removed]" (None when nothing changes).
pub fn redact(known: &[f64], text: &str) -> Option<String> {
    let bad = unverified(known, text);
    if bad.is_empty() {
        return None;
    }
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let mut last = 0;
    while i < b.len() {
        let range_tail = i >= 2 && b[i - 1] == b'-' && b[i - 2].is_ascii_digit();
        if b[i].is_ascii_digit() && !range_tail && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'.' || b[i - 1] == b':' || b[i - 1] == b'=' || b[i - 1] == b'|')) {
            let s = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            let num = text[s..i].trim_end_matches('.');
            let end = s + num.len() + usize::from(i < b.len() && b[i] == b'%');
            if !unverified(known, &text[s..end]).is_empty() {
                out.push_str(&text[last..s]);
                out.push_str("[unverified number removed]");
                last = end;
                i = end;
            }
        } else {
            i += 1;
        }
    }
    out.push_str(&text[last..]);
    Some(out)
}

/// Owner (2026-10-11): no fake or manipulated data or results may reach the final strategy or the dashboard. Strategy results,
/// validation and promotion are written by code only (the brain has no tool that writes them); this scrubs the brain-written
/// text the dashboard shows (notes, whiteboard, research ideas): every number the lab never
/// showed is replaced in place (the text is kept, never deleted). Text from claude-code is not touched.
/// Returns what was changed.
pub fn scrub_stored(ctx: &Ctx) -> Result<Vec<String>> {
    let k = known(ctx)?; // incomplete list: change nothing (see known)
    let mut changed = Vec::new();
    let targets: [(&str, &str, &str, &str); 4] = [
        ("agent_messages", "id", "text", "sender NOT IN ('claude-code')"),
        ("whiteboard", "id", "text", "1"),
        ("whiteboard", "id", "evidence", "1"),
        ("research_ideas", "url", "idea", "1"),
    ];
    for (table, key, col, filter) in targets {
        let Ok(mut st) = ctx.mem.prepare(&format!("SELECT CAST({key} AS TEXT), {col} FROM {table} WHERE {filter} AND {col} IS NOT NULL")) else { continue };
        let rows: Vec<(String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
        for (id, text) in rows {
            if let Some(new) = redact(&k, &text) {
                ctx.mem.execute(&format!("UPDATE {table} SET {col} = ?1 WHERE CAST({key} AS TEXT) = ?2"), params![new, id])?;
                changed.push(format!("{table}.{col} {id}: {}", unverified(&k, &text).join(", ")));
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: a failure to read the lab stops the number check (an incomplete list would make real numbers look invented); a
    /// tool with no data yet does not.
    #[test]
    fn read_failures_stop_the_check_but_no_data_does_not() {
        assert!(is_read_failure("database is locked") && is_read_failure("disk I/O error") && is_read_failure("file is not a database"));
        assert!(!is_read_failure("no such table: runs") && !is_read_failure("too few runs for tuning facts"));
    }

    /// Test: only decimals and percentages are checked; identifiers and dates are not; rounding a known number is allowed.
    #[test]
    fn numbers_are_checked_against_known_ones() {
        assert_eq!(numbers("5h|obv +1.59%/block, win 52%, 178 tests on 2026-10-10, tune:margin=0.40-0.70, 9.0 trades"),
            vec![(1.59, 2), (52.0, 0), (9.0, 1)]);
        let k = [1.587, 52.0, 9.0, 3.12];
        assert!(unverified(&k, "obv +1.59%/block, 52% win, 9.0 trades").is_empty(), "rounding a real number is fine");
        assert!(unverified(&k, "obv +1.6%/block").is_empty());
        assert_eq!(unverified(&k, "obv +2.40%/block and 72% win"), vec!["2.40".to_string(), "72".to_string()]);
        assert_eq!(redact(&k, "obv +2.40%/block, 52% win, 5h|obv").unwrap(), "obv +[unverified number removed]/block, 52% win, 5h|obv");
        assert!(redact(&k, "obv +1.59%/block").is_none());
    }
}

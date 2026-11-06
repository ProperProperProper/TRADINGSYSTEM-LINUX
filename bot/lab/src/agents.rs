//! The read-only tools the lab's MCP server (`qwen-lab mcp`, used by Claude Code) and the brain share. The restricted view (`human = false`)
//! never contains the HOLDOUT half of any result (only the training half); the human-facing view shows everything.

use crate::context::Ctx;
use crate::strategy;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

/// A tool both the agents and the MCP server offer: (name, description, input schema, may an agent call it).
pub fn tool_defs() -> Vec<(&'static str, &'static str, Value, bool)> {
    let none = json!({"type": "object", "properties": {}});
    vec![
        ("lab_best", "The best strategy-search results so far (interval | options @capital: mean % per 14-session block, training / noisy; the human view adds the holdout and the verdict).", json!({"type": "object", "properties": {"n": {"type": "integer", "description": "how many (default 8)"}}}), true),
        ("lab_whiteboard", "The local model's whiteboard: rules, verified facts, its beliefs and plan, and the winning runs it remembers.", none.clone(), true),
        ("lab_highlights", "Runs that returned 15% or more over 14 sessions, with settings, the data-based reason and the luck check (HIGHLIGHTS.md).", none.clone(), true),
        ("bot_status", "The trading bot's health in a few lines (OpenD, service, paper equity, interval winners).", json!({"type": "object", "properties": {"minutes": {"type": "integer"}}}), true),
        ("lab_status", "STATUS.md written by the 15-minute monitor: jobs, loop progress, faults, fixes.", none.clone(), false),
        ("lab_log", "The last lines of the always-on learning loop's log.", json!({"type": "object", "properties": {"lines": {"type": "integer"}}}), false),
        ("lab_agents", "The brain's twelve roles (the lab's own models).", none.clone(), false),
        ("lab_entry_gating", "Entry gating verdict: which gate options have a validated variant that beats the current strategy (UNSEEN half). Human/Claude view only.", none.clone(), false),
        ("lab_quant", "The quant review of the 5h variants on the UNSEEN half (paired per-block evidence vs the baseline, selection bar, risk) plus the overfitting decay per option. Human/Claude view only.", none, false),
    ]
}

/// The best results, one per line. `human` adds the holdout half and the verdict; an agent never sees the holdout.
pub fn best_lines(mem: &Connection, n: usize, human: bool) -> Result<String> {
    strategy::init(mem)?;
    let mut s = mem.prepare("SELECT interval, tokens, equity, cut, mean, h1, h2, nh1, nh2, pos, worst, trades FROM strategy_runs WHERE tokens != 'baseline' ORDER BY (CASE WHEN ?1 = 1 THEN mean ELSE h1 END) DESC LIMIT ?2")?;
    let rows: Vec<(String, String, f64, String, f64, f64, f64, Option<f64>, Option<f64>, f64, f64, f64)> = s
        .query_map(rusqlite::params![human as i64, n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?)))?
        .collect::<Result<_, _>>()?;
    if rows.is_empty() {
        return Ok("no results yet".into());
    }
    let mut out = Vec::new();
    for (iv, t, eq, cut, mean, h1, h2, nh1, nh2, pos, worst, trades) in rows {
        out.push(if human {
            format!("{iv}|{t} @{eq:.0} [{cut}]: mean {mean:+.2}% (train {h1:+.2}, holdout {h2:+.2}), noisy {}/{}, {pos:.0}% blocks+, worst {worst:+.1}%, {trades:.1} trades",
                nh1.map_or("?".into(), |x| format!("{x:+.2}")), nh2.map_or("?".into(), |x| format!("{x:+.2}")))
        } else {
            format!("{iv}|{t} @{eq:.0}: train {h1:+.2}%, noisy {}%, {pos:.0}% blocks+, {trades:.1} trades", nh1.map_or("?".into(), |x| format!("{x:+.2}")))
        });
    }
    Ok(out.join("\n"))
}

/// Runs the bot's health check (`bot-ops check`, or `check.sh` before it is built) for the last `minutes` of logs, for the bot_status tool.
fn bot_status(minutes: i64) -> String {
    crate::botctl::check(minutes.clamp(1, 1440) as u32).trim().to_string()
}

/// Run a tool. `human` is the MCP/Claude view; an agent (`human = false`) gets the restricted view and only the tools marked for it.
pub fn call_tool(ctx: &Ctx, name: &str, args: &Value, human: bool) -> Result<String> {
    let allowed = tool_defs().into_iter().find(|t| t.0 == name).with_context(|| format!("unknown tool {name}"))?.3;
    if !human && !allowed {
        bail!("tool {name} is not available to agents");
    }
    match name {
        "lab_best" => best_lines(ctx.mem, args["n"].as_u64().unwrap_or(8).clamp(1, 30) as usize, human),
        // the Hermes view leaves out the sections other tools already return (lab_rules, lab_winrate, lab_research_*,
        // lab_highlights): measured 2026-10-11, ~16k characters per read halved, and every turn of the local model re-reads it
        "lab_whiteboard" if !human => Ok(strategy::render_board(ctx.mem)?.split("\n\n")
            .filter(|sec| !["RULES (", "WHY RUNS WIN", "IDEAS FROM PUBLISHED", "WINNING RUNS I REMEMBER"].iter().any(|p| sec.trim_start().starts_with(p)))
            .collect::<Vec<_>>().join("\n\n") + "\n\n(rules: lab_rules; why runs win: lab_winrate; research ideas: lab_inbox and lab_research_search; winning runs: lab_highlights)"),
        "lab_whiteboard" => strategy::render_board(ctx.mem),
        "lab_highlights" => {
            let f = ctx.cfg.root.join("HIGHLIGHTS.md");
            Ok(std::fs::read_to_string(&f).map(|t| t.chars().take(6000).collect()).unwrap_or_else(|_| "no highlights yet".into()))
        }
        "bot_status" => Ok(bot_status(args["minutes"].as_i64().unwrap_or(15))),
        "lab_status" => Ok(std::fs::read_to_string(ctx.cfg.root.join("STATUS.md")).unwrap_or_else(|_| "no STATUS.md yet (the monitor writes it every 15 minutes)".into())),
        "lab_log" => {
            let t = std::fs::read_to_string(ctx.cfg.root.join("logs/improve.log")).unwrap_or_default();
            let n = args["lines"].as_u64().unwrap_or(30).clamp(1, 200) as usize;
            let lines: Vec<&str> = t.lines().collect();
            Ok(lines[lines.len().saturating_sub(n)..].join("\n"))
        }
        "lab_entry_gating" => Ok(strategy::entry_gating(ctx.mem, crate::quant::Half::Unseen)?.0),
        "lab_agents" => Ok(crate::brain_roles::ROLES.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n")),
        "lab_quant" => {
            let mut t = crate::quant::review(ctx.mem, crate::quant::Half::Unseen)?.0;
            t.push_str("\nOverfitting decay per option (mean paired gain: training half -> unseen half):\n");
            for (o, n, a, b) in crate::quant::decay(ctx.mem)? { t.push_str(&format!("  {o} ({n} variants): {a:+.2} -> {b:+.2}\n")); }
            Ok(t)
        }
        other => bail!("unknown tool {other}"),
    }
}

/// Creates the agents' message table if missing.
pub fn init_messages(mem: &rusqlite::Connection) -> Result<()> {
    mem.execute_batch("CREATE TABLE IF NOT EXISTS agent_messages (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, sender TEXT NOT NULL, recipient TEXT NOT NULL, text TEXT NOT NULL, read INTEGER NOT NULL DEFAULT 0);")?;
    Ok(())
}

/// The newest notes between agents, for the dashboard: (time, from, to, text, read).
pub fn recent_messages(mem: &rusqlite::Connection, limit: usize) -> Result<Vec<Value>> {
    init_messages(mem)?;
    let mut st = mem.prepare("SELECT ts, sender, recipient, text, read FROM agent_messages WHERE (sender LIKE 'brain-%' OR sender = 'claude-code') AND recipient LIKE 'brain-%' ORDER BY id DESC LIMIT ?1")?;
    let v = st.query_map([limit as i64], |r| Ok(json!({"ts": r.get::<_, i64>(0)?, "from": r.get::<_, String>(1)?, "to": r.get::<_, String>(2)?, "text": r.get::<_, String>(3)?, "read": r.get::<_, bool>(4)?})))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// The MCP server (JSON-RPC over stdio). `human = false` is the restricted view: only the tools marked for it, never a holdout number.
pub fn serve_mcp_view(ctx: &Ctx, human: bool) -> Result<()> {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = req.get("id").cloned() else { continue };
        let result = match req["method"].as_str().unwrap_or("") {
            "initialize" => json!({"protocolVersion": req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"), "capabilities": {"tools": {}}, "serverInfo": {"name": "qwen-lab", "version": env!("CARGO_PKG_VERSION")}}),
            "tools/list" => {
                let tools: Vec<Value> = tool_defs().into_iter().filter(|t| human || t.3).map(|t| json!({"name": t.0, "description": t.1, "inputSchema": t.2})).collect();
                json!({"tools": tools})
            }
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let res = call_tool(ctx, name, &req["params"]["arguments"], human);
                let (text, is_err) = match res {
                    Ok(t) => (t, false),
                    Err(e) => (format!("{e:#}"), true),
                };
                json!({"content": [{"type": "text", "text": text}], "isError": is_err})
            }
            "ping" => json!({}),
            m => {
                let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {m}")}}));
                let _ = out.flush();
                continue;
            }
        };
        let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let _ = out.flush();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: agents never get the holdout and only the tools marked for them.
    #[test]
    fn agents_never_get_the_holdout_and_only_the_tools_marked_for_them() {
        let m = Connection::open_in_memory().unwrap();
        strategy::init(&m).unwrap();
        strategy::import_output(&m, "4h start capital 10000: 28 blocks, SPY buy&hold mean +2.49%\nvariant mean % median % blocks+ worst % 1st half 2nd half trades noisy 1st noisy 2nd noisy+\nbaseline +0.35 +0.50 54% -12.14 +0.53 +0.17 19.1 +0.20 -0.10 40%\nadx +1.28 +0.00 43% -6.00 +1.43 +1.14 8.7 +0.90 +0.80 45%\n").unwrap();
        let agent_view = best_lines(&m, 5, false).unwrap();
        let human_view = best_lines(&m, 5, true).unwrap();
        assert!(agent_view.contains("+1.43") && !agent_view.contains("1.14") && !agent_view.contains("holdout"), "{agent_view}");
        assert!(human_view.contains("holdout +1.14"), "{human_view}");
        let defs = tool_defs();
        for human_only in ["lab_status", "lab_log", "lab_agents", "lab_quant"] {
            assert!(!defs.iter().find(|t| t.0 == human_only).unwrap().3, "{human_only} must not be callable by an agent");
        }
    }
}

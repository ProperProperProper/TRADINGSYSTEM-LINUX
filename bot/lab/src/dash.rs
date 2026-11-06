//! The monitoring dashboard: a small READ-ONLY web page served on 127.0.0.1, separate from the bot. It shows what the lab is doing,
//! what real data it has, how close it is to a lesson, and whether anything needs attention. It opens the lab databases read-only,
//! has no endpoint that changes anything, accepts only GET, and answers only to localhost host names. The page is fully
//! self-contained (no external scripts, fonts or images).

use crate::config::Config;
use crate::{btarchive, db, context, paper, schedule, sysinfo};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PAGE: &str = include_str!("dash.html");

/// Opens a database read-only with a 5 s busy timeout.
fn open_ro(path: &std::path::Path) -> Result<Connection> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    c.busy_timeout(Duration::from_secs(5))?;
    Ok(c)
}

/// First column of the first row of `sql`, if any.
fn q1<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> Option<T> { c.query_row(sql, [], |r| r.get::<_, T>(0)).ok() }

/// A named counter from the memory database, if set.
fn counter(mem: &Connection, name: &str) -> Option<i64> {
    mem.query_row("SELECT value FROM counters WHERE name = ?1", [name], |r| r.get(0)).ok()
}

/// Recent tick logs, newest first: when each ran, light or full, and what each step reported.
fn ticks(cfg: &Config, limit: usize) -> Vec<Value> {
    let Ok(rd) = std::fs::read_dir(cfg.logs_dir()) else { return vec![] };
    let mut files: Vec<(i64, std::path::PathBuf)> = rd.filter_map(|e| e.ok()).filter_map(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        let ts = name.strip_prefix("tick-")?.strip_suffix(".log")?.parse::<i64>().ok()?;
        Some((ts, e.path()))
    }).collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.truncate(limit);
    files.into_iter().filter_map(|(ts, p)| {
        let text = std::fs::read_to_string(p).ok()?;
        let mut lines = text.lines();
        let mode = if lines.next().unwrap_or("").contains("full") { "full" } else { "light" };
        let steps: Vec<Value> = lines.filter_map(|l| {
            let l = l.trim();
            let status_end = l.find(']')?;
            let status = l.get(1..status_end)?;
            let rest = l.get(status_end + 1..)?.trim();
            let (name, detail) = rest.split_once(':')?;
            Some(json!({"name": name.trim(), "status": status, "detail": detail.trim().chars().take(240).collect::<String>()}))
        }).collect();
        Some(json!({"ts": ts, "mode": mode, "steps": steps}))
    }).collect::<Vec<Value>>()
    .into_iter()
    // owner 2026-10-11: "nothing stale to ever be presented": failures are current state only in the newest tick of each mode
    // (light / full). In an older tick a failure is history; a failure cleared by any newer tick (the same step ran ok later) is
    // hidden too.
    .scan((std::collections::HashSet::<String>::new(), std::collections::HashSet::<String>::new()), |(ok_later, modes), mut t| {
        let older = !modes.insert(t["mode"].as_str().unwrap_or("").to_string());
        if let Some(steps) = t["steps"].as_array_mut() {
            steps.retain(|s| !(s["status"] == "failed" && (older || s["name"].as_str().is_some_and(|n| ok_later.contains(n)))));
            for s in steps.iter().filter(|s| s["status"] == "ok") {
                if let Some(n) = s["name"].as_str() { ok_later.insert(n.to_string()); }
            }
        }
        Some(t)
    })
    .collect()
}

/// Creates every lab table that is missing (CREATE TABLE IF NOT EXISTS; never changes data).
pub fn ensure_schema(cfg: &Config) -> Result<()> {
    let mem = crate::db::open(&cfg.memory_db(), true)?;
    crate::strategy::init(&mem)?;
    crate::labtools::init(&mem)?;
    crate::scout::init(&mem)?;
    crate::highlights::init(&mem)?;
    crate::guard::init(&mem)?;
    crate::promote::init(&mem)?;
    crate::agents::init_messages(&mem)?;
    Ok(())
}

/// Identifies the dashboard page this server ships (a hash of the page); an open browser tab reloads itself when it changes,
/// so a dashboard update shows without a manual refresh.
fn page_id() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    PAGE.hash(&mut h);
    format!("{:x}", h.finish())
}

/// Commit time (unix seconds) of the lab repo's newest commit, None when it cannot be read.
fn code_age_secs() -> Option<i64> {
    let o = std::process::Command::new("git").args(["-C", &crate::platform::lab_repo().to_string_lossy(), "log", "-1", "--format=%ct"]).output().ok()?;
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

/// Everything the dashboard shows, as one JSON snapshot (inputs, PnL, hypotheses, lessons, health, alerts).
pub fn snapshot(cfg: &Config) -> Result<Value> {
    let now = crate::util::now_ts();
    let mem = open_ro(&cfg.memory_db())?;
    let data = open_ro(&cfg.data_db())?;
    let mut alerts: Vec<String> = Vec::new();

    // ---- inputs: the bot's backtest archive as the lab holds it
    let bt = btarchive::current(&data)?;
    let by_population: Vec<Value> = {
        let mut st = data.prepare(
            "SELECT r.role, r.selection, json_extract(r.metrics_json, '$.start_equity'), count(DISTINCT r.run_id), count(DISTINCT t.run_id), count(t.run_id), avg(t.ret_margin), avg(t.ret_margin > 0), \
                    (SELECT avg(x.return_pct) FROM runs x WHERE x.run_key IS NOT NULL AND x.role = r.role AND x.selection = r.selection AND json_extract(x.metrics_json, '$.start_equity') IS json_extract(r.metrics_json, '$.start_equity')), sum(t.pnl) \
             FROM runs r LEFT JOIN trades t ON t.run_id = r.run_id WHERE r.run_key IS NOT NULL GROUP BY r.role, r.selection, 3 ORDER BY r.role, r.selection, 3")?;
        let v = st.query_map([], |r| Ok(json!({"role": r.get::<_, String>(0)?, "selection": r.get::<_, String>(1)?, "capital": r.get::<_, Option<f64>>(2)?, "runs": r.get::<_, i64>(3)?, "with_trades": r.get::<_, i64>(4)?,
            "trades": r.get::<_, i64>(5)?, "mean_ret": r.get::<_, Option<f64>>(6)?, "win_rate": r.get::<_, Option<f64>>(7)?, "mean_run_return_pct": r.get::<_, Option<f64>>(8)?, "pnl": r.get::<_, Option<f64>>(9)?})))?.collect::<Result<_, _>>()?;
        v
    };
    let info = context::windows_info(&data)?;
    let windows: Vec<Value> = info.iter().map(|(w, complete)| {
        let (n, u, d, pnl, win): (i64, i64, i64, Option<f64>, Option<f64>) = data.query_row(
            "SELECT count(*), (SELECT count(*) FROM (SELECT DISTINCT symbol, side, entry_ts FROM trades WHERE window = ?1)), count(DISTINCT day), sum(pnl), avg(pnl > 0) FROM trades WHERE window = ?1",
            [w], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap_or((0, 0, 0, None, None));
        json!({"label": w, "complete": complete, "trades": n, "unique_entries": u, "days": d, "pnl": pnl, "win_rate": win})
    }).collect();
    // Daily PnL of the BOT'S STRATEGY on unseen sessions, as ONE account: the `oos` runs (settings searched on earlier sessions, then
    // traded forward unchanged) at one start capital, the capital of the newest such run (the daily export runs at the bot's
    // capital). A trade's PnL counts on the day it CLOSED. Several oos runs cover the same day (block cuts, seeds), so the day's
    // return is their average: % = 100 * (sum of PnL closed that day) / (capital * runs whose block covers that day), and
    // USD = % of one account. A failed lookup is an error, never a silently different population.
    let pnl_capital: Option<f64> = match data.query_row(
        "SELECT json_extract(metrics_json, '$.start_equity') FROM runs WHERE run_key IS NOT NULL AND role = 'oos' AND json_extract(metrics_json, '$.start_equity') IS NOT NULL \
         ORDER BY created DESC, rowid DESC LIMIT 1",
        [],
        |r| r.get::<_, Option<f64>>(0),
    ) {
        Ok(c) => c,
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    let per_day: Vec<Value> = match pnl_capital {
        None => Vec::new(),
        Some(cap) => {
            // Closed-trade PnL per exit day and the oos blocks (window start/end, seconds) at this capital.
            let mut st = data.prepare(
                "WITH oos AS (SELECT run_id FROM runs WHERE run_key IS NOT NULL AND role = 'oos' AND abs(json_extract(metrics_json, '$.start_equity') - ?1) < 0.5) \
                 SELECT t.exit_ts / 86400, count(*), sum(t.pnl), sum(t.pnl > 0), sum(t.pnl < 0) FROM trades t JOIN oos ON oos.run_id = t.run_id GROUP BY 1")?;
            let closed: std::collections::BTreeMap<i64, (i64, f64, i64, i64)> =
                st.query_map([cap], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))))?.collect::<Result<_, _>>()?;
            let mut st = data.prepare("SELECT window_start, window_end FROM runs WHERE run_key IS NOT NULL AND role = 'oos' AND abs(json_extract(metrics_json, '$.start_equity') - ?1) < 0.5")?;
            let blocks: Vec<(i64, i64)> = st.query_map([cap], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            // Trading days: SPY's real candles in the bot's store (read-only); without the store, the days any archived run traded.
            let mut market: std::collections::BTreeSet<i64> = open_ro(&cfg.strategy.store_path)
                .ok()
                .and_then(|c| {
                    let mut q = c.prepare("SELECT DISTINCT wall_ms / 86400000 FROM candles WHERE symbol = 'SPY'").ok()?;
                    let v: Result<std::collections::BTreeSet<i64>, _> = q.query_map([], |r| r.get::<_, i64>(0)).ok()?.collect();
                    v.ok()
                })
                .unwrap_or_default();
            if market.is_empty() {
                let mut q = data.prepare("SELECT DISTINCT entry_ts / 86400 FROM trades UNION SELECT DISTINCT exit_ts / 86400 FROM trades")?;
                market = q.query_map([], |r| r.get::<_, i64>(0))?.collect::<Result<_, _>>()?;
            }
            market.extend(closed.keys().copied());
            let last = closed.keys().next_back().copied().unwrap_or(0);
            let v: Vec<Value> = market
                .range(last - 29..=last)
                .filter_map(|&d| {
                    let runs = blocks.iter().filter(|(ws, we)| *ws < (d + 1) * 86_400 && *we >= d * 86_400).count() as i64;
                    if runs == 0 {
                        return None; // no oos block covers this day: nothing to show
                    }
                    let (n, pnl, w, l) = closed.get(&d).copied().unwrap_or((0, 0.0, 0, 0));
                    let pct = 100.0 * pnl / (cap * runs as f64);
                    let day: String = data.query_row("SELECT date(?1 * 86400, 'unixepoch')", [d], |r| r.get(0)).unwrap_or_default();
                    Some(json!({"day": day, "trades": n, "wins": w, "losses": l, "runs": runs, "no_trades": n == 0, "pct": pct, "pnl": pct * cap / 100.0}))
                })
                .collect();
            v
        }
    };
    let archive_trades: Option<i64> = open_ro(&cfg.backtest_archive.path).ok().and_then(|c| q1::<i64>(&c, "SELECT coalesce(max(id), 0) FROM bt_trades"));

    // ---- strategy tests (14-session look-forward blocks, bot-side variant_study): newest first, with PnL in USD
    let strategy_tests: Vec<Value> = mem.prepare(
        "SELECT ts, interval, tokens, equity, cut, blocks, mean, worst, pos, h1, h2, nh1, nh2, chained, win1, win2 FROM strategy_runs ORDER BY ts DESC, id DESC LIMIT 40")
        .and_then(|mut st| st.query_map([], |r| {
            let eq: f64 = r.get(3)?;
            let mean: f64 = r.get(6)?;
            let chained: Option<f64> = r.get(13).unwrap_or(None);
            Ok(json!({"ts": r.get::<_, i64>(0)?, "interval": r.get::<_, String>(1)?, "tokens": r.get::<_, String>(2)?, "capital": eq, "cut": r.get::<_, String>(4)?,
                "blocks": r.get::<_, i64>(5)?, "mean_pct": mean, "mean_pnl": eq * mean / 100.0, "worst_pct": r.get::<_, f64>(7)?, "pos": r.get::<_, f64>(8)?,
                "h1": r.get::<_, f64>(9)?, "h2": r.get::<_, f64>(10)?, "nh1": r.get::<_, Option<f64>>(11)?, "nh2": r.get::<_, Option<f64>>(12)?,
                "chained_pct": chained, "chained_pnl": chained.map(|c| eq * c / 100.0),
                "win1": r.get::<_, Option<f64>>(14).unwrap_or(None), "win2": r.get::<_, Option<f64>>(15).unwrap_or(None)}))
        })?.collect::<Result<Vec<_>, _>>())
        .unwrap_or_default();

    // ---- every tested strategy variant (shared by the winners report and the research-ideas report)
    let leaderboard: Vec<Value> = crate::strategy::leaderboard(&mem).unwrap_or_default();

    // ---- the current strategy (baseline) on every candle size, and the tuning team's ranges against it
    let base_rows: Vec<Value> = mem.prepare("SELECT interval, cut, mean, h2, chained, worst, pos, win2, trades, ts FROM strategy_runs WHERE tokens = 'baseline' AND regime = (SELECT regime FROM strategy_runs WHERE tokens = 'baseline' ORDER BY ts DESC LIMIT 1) ORDER BY interval, cut")
        .and_then(|mut st| st.query_map([], |r| Ok(json!({"interval": r.get::<_, String>(0)?, "cut": r.get::<_, String>(1)?, "mean": r.get::<_, f64>(2)?, "h2": r.get::<_, f64>(3)?,
            "chained": r.get::<_, Option<f64>>(4)?, "worst": r.get::<_, f64>(5)?, "pos": r.get::<_, f64>(6)?, "win2": r.get::<_, Option<f64>>(7)?, "trades": r.get::<_, f64>(8)?, "ts": r.get::<_, i64>(9)?})))?.collect::<Result<Vec<_>, _>>())
        .unwrap_or_default();
    let mut interval_table: Vec<Value> = Vec::new();
    for iv in &cfg.strategy.intervals {
        let a = base_rows.iter().find(|r| r["interval"] == iv.as_str() && r["cut"] == "A").cloned();
        let b = base_rows.iter().find(|r| r["interval"] == iv.as_str() && r["cut"] == "B").cloned();
        interval_table.push(json!({"interval": iv, "live": iv == "5h", "a": a, "b": b}));
    }
    interval_table.sort_by(|x, y| y["a"]["h2"].as_f64().unwrap_or(f64::NEG_INFINITY).total_cmp(&x["a"]["h2"].as_f64().unwrap_or(f64::NEG_INFINITY)));
    // "considerably better strategy" alert (code-decided, see strategy::considerably_better) and the Hermes agents' work
    let (better, better_pending) = crate::strategy::considerably_better(&mem).unwrap_or_default();
    let test_requests = crate::labtools::queue_with_results(&mem, &leaderboard, 20).unwrap_or_default();
    let scoreboard = crate::labtools::scoreboard(&mem).unwrap_or_default();
    let agent_results = crate::strategy::agent_results(&mem, cfg.strategy.equities.first().copied().unwrap_or(5000.0), 40).unwrap_or_default();
    let security = crate::guard::recent(&mem, 20);
    let promotions = crate::promote::list(&mem, 20);
    let promotion_status = crate::promote::qualifying(&mem).map(|(ok, not)| json!({"qualifying": ok.iter().map(|b| format!("{}|{}", b["interval"].as_str().unwrap_or(""), b["tokens"].as_str().unwrap_or(""))).collect::<Vec<_>>(), "not": not})).unwrap_or_default();
    let regime = crate::strategy::current_regime(&mem);
    let cap0 = cfg.strategy.equities.first().copied().unwrap_or(5000.0);
    let best_ai = crate::strategy::best_ai_runs(&mem, cap0).unwrap_or_default();
    let tuning_table: Vec<Value> = leaderboard.iter().filter(|l| l["tokens"].as_str().is_some_and(|t| t.contains("tune:"))).cloned().collect();

    // ---- why runs win (>=60% of trades profitable) or lose, from crate::winrate
    let _ = crate::winrate::init(&mem);
    let winrate_summary: Option<Value> = q1::<String>(&mem, "SELECT json FROM winrate_summary ORDER BY ts DESC LIMIT 1").and_then(|j| serde_json::from_str(&j).ok());
    let winrate: Vec<Value> = mem.prepare("SELECT driver, kind, direction, split, train_auc, test_auc, test_win_good, test_win_bad, delta, ci_low, ci_high, q, confirmed FROM winrate_findings ORDER BY confirmed DESC, delta DESC LIMIT 14")
        .and_then(|mut st| st.query_map([], |r| Ok(json!({"driver": r.get::<_, String>(0)?, "kind": r.get::<_, String>(1)?, "direction": r.get::<_, i64>(2)?, "split": r.get::<_, f64>(3)?,
            "train_auc": r.get::<_, f64>(4)?, "test_auc": r.get::<_, f64>(5)?, "good": r.get::<_, f64>(6)?, "bad": r.get::<_, f64>(7)?, "delta": r.get::<_, f64>(8)?,
            "ci_low": r.get::<_, f64>(9)?, "ci_high": r.get::<_, f64>(10)?, "q": r.get::<_, f64>(11)?, "confirmed": r.get::<_, bool>(12)?})))?.collect::<Result<Vec<_>, _>>())
        .unwrap_or_default();

    // ---- inputs: paper record
    let pr = paper::current(cfg, &data, now * 1000)?;
    let equity = data.query_row("SELECT equity, start_equity, equity_index FROM paper_equity ORDER BY epoch DESC, ts_ms DESC LIMIT 1", [], |r| Ok(json!({"equity": r.get::<_, f64>(0)?, "start": r.get::<_, f64>(1)?, "index": r.get::<_, Option<f64>>(2)?}))).ok();
    // the bot's newest decision and why (its archive, read-only): "0 trades" alone does not say whether the strategy chose not to
    // trade (e.g. SPY below its trend) or something is wrong
    let last_decision: Option<Value> = open_ro(&cfg.paper.archive_path).ok().and_then(|a| a.query_row(
        "SELECT wall_ms, detail FROM events WHERE kind = 'decision' ORDER BY wall_ms DESC, rowid DESC LIMIT 1", [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).ok()).and_then(|(ts, d)| {
            let v: Value = serde_json::from_str(&d).ok()?;
            Some(json!({"ts": ts / 1000, "note": v["allocation_note"], "funded": v["funded_top"], "requested": v["requested_top"], "gates": v["gates"]}))
        });
    let latest_search: Option<String> = data.query_row("SELECT signal, hold, top, robust_score FROM paper_searches ORDER BY epoch DESC, id DESC LIMIT 1", [],
        |r| Ok(format!("{} (hold {}, top {}), robust score {}", r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<i64>>(1)?.unwrap_or(0), r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            r.get::<_, Option<f64>>(3)?.map_or("n/a".into(), |v| format!("{v:.1}"))))).ok();

    // ---- health and alerts
    let last_tick = counter(&mem, "last_tick");
    let last_full = counter(&mem, "last_full_tick");
    let ticks = ticks(cfg, 24);
    let schedule_loaded = schedule::status().starts_with("loaded");
    let stop = cfg.stop_file().exists();
    if stop { alerts.push("the kill switch (STOP file) is engaged: no learning happens".into()); }
    match last_tick { Some(t) if now - t > 150 * 60 => alerts.push(format!("the last tick was {} minutes ago (expected hourly)", (now - t) / 60)), None => alerts.push("no tick has run yet".into()), _ => {} }
    if !schedule_loaded { alerts.push("the hourly job is not loaded in launchd".into()); }
    if let Some(t) = ticks.first() {
        for s in t["steps"].as_array().into_iter().flatten() {
            if s["status"] == "failed" { alerts.push(format!("last tick step `{}` failed: {}", s["name"].as_str().unwrap_or(""), s["detail"].as_str().unwrap_or(""))); }
        }
    }
    if pr.stale { alerts.push("the paper record is stale (the bot may be stopped)".into()); }
    let data_bytes = db::size_bytes(&cfg.data_db());
    if data_bytes * 10 >= cfg.limits.data_db_limit_bytes * 8 { alerts.push("the data database is above 80% of its size limit".into()); }
    let newest_bar: Option<i64> = q1(&data, "SELECT max(window_end) FROM runs");
    // owner 2026-10-13: "dash must always be up to date": running code older than the newest commit is shown at once (the monitor redeploys within 15 minutes)
    if let (Some(head), Some(bin)) = (code_age_secs(), std::env::current_exe().ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64)) {
        if head > bin + 600 { alerts.push("this dashboard runs code older than the newest commit of the lab repo: a redeploy is due (the monitor does it every 15 minutes; or run ops/redeploy_lab.sh)".into()); }
    }
    // The archive holds complete 14-session look-forward blocks only, so its newest bar lags the market by up to 14 sessions (about
    // 20 calendar days) by design; only a longer lag means the daily export stopped.
    if let Some(nb) = newest_bar { if now - nb > 24 * 86_400 { alerts.push(format!("the newest market bar in the lab is {} days old (blocks lag by up to ~20 days by design): is the daily archive export still running?", (now - nb) / 86_400)); } }
    // owner 2026-10-12: "make sure dash always shows current stats": every source the page shows, with its age and how often it
    // must be refreshed; an overdue source raises an alert (nothing stale is presented as current)
    if let Some((_, code)) = supervisor_sessions(&cfg.logs_dir().join("supervisor.log")).last() {
        if *code != 0 {
            let report = std::fs::read_to_string(cfg.root.join("supervisor/bundles/last_report.md")).unwrap_or_default();
            let reason = if report.contains("weekly limit") { " (Claude weekly limit)" } else { "" };
            alerts.push(format!("scrum master last session failed (exit {code}){reason}"));
        }
    }
    let fresh = freshness(cfg, &mem, now);
    for f in fresh.iter().filter(|f| f["overdue"] == true) {
        alerts.push(format!("{} is out of date: last update {} ago (expected at least every {})", f["name"].as_str().unwrap_or(""),
            f["age"].as_i64().map_or("never".into(), human_age), human_age(f["max"].as_i64().unwrap_or(0))));
    }
    let level = if stop { "stopped" } else if alerts.is_empty() { "ok" } else { "attention" };

    Ok(json!({
        "generated": now, "version": crate::util::CODE_VERSION, "mode": cfg.mode, "page": page_id(),
        "team": team(cfg, now),
        "freshness": fresh,
        "status": {"level": level},
        "alerts": alerts,
        "health": {
            "last_tick": last_tick, "last_full_tick": last_full, "schedule_loaded": schedule_loaded, "stop": stop,
            "load1": sysinfo::load1(), "free_gb": sysinfo::free_gb(), "data_mb": data_bytes as f64 / 1e6, "memory_mb": db::size_bytes(&cfg.memory_db()) as f64 / 1e6,
            "newest_bar": newest_bar, "last_import": q1::<i64>(&data, "SELECT max(ingested_at) FROM ingest_batches"),
        },
        "inputs": { "ai": ai_inputs(cfg, &mem), "block_compare": block_compare(&mem), "security_counts": crate::guard::daily_counts(&mem, 30),
            "backtest": {"runs": bt.runs_total, "runs_with_trades": bt.runs_with_trades, "oos_runs": bt.oos_runs, "trades": bt.trades_total, "archive_trades": archive_trades,
                         "by_population": by_population, "windows": windows, "per_day": per_day, "pnl_capital": pnl_capital}, "strategy_tests": strategy_tests, "intervals": interval_table, "tuning": tuning_table, "leaderboard": leaderboard.iter().take(25).cloned().collect::<Vec<_>>(), "research_ideas": crate::scout::ideas_with_results(&mem, &leaderboard, 30).unwrap_or_default(), "agent_messages": crate::agents::recent_messages(&mem, 30).unwrap_or_default(), "better": better, "better_pending": better_pending, "test_requests": test_requests, "role_reports": crate::labtools::reports(&cfg.root, 4), "scoreboard": scoreboard, "agent_results": agent_results, "security": security, "promotions": promotions, "promotion_status": promotion_status, "live_regime": regime, "best_ai": best_ai, "entry_gating": crate::strategy::entry_gating(&mem, crate::quant::Half::Unseen).map(|x| x.0).unwrap_or_default(), "quant": crate::quant::review(&mem, crate::quant::Half::Unseen).map(|x| x.0).unwrap_or_default(), "quant_decay": crate::quant::decay(&mem).unwrap_or_default().iter().map(|d| json!({"option": d.0, "variants": d.1, "train": d.2, "unseen": d.3})).collect::<Vec<_>>(), "winrate": winrate, "winrate_summary": winrate_summary,
            "paper": {"currency": cfg.paper.currency, "state": pr.state, "trades": pr.trades_total,
                "wins": q1::<i64>(&data, "SELECT count(*) FROM paper_trades WHERE pnl > 0"), "days": pr.days, "runs": pr.runs, "stale": pr.stale, "last_ts": pr.last_ts_ms.map(|t| t / 1000), "equity": equity, "latest_search": latest_search, "last_decision": last_decision},
        },
        "ticks": ticks,
    }))
}

struct Cache { at: Option<Instant>, body: String }

/// Writes an HTTP response with security headers and no caching.
fn respond(stream: &mut TcpStream, code: u16, reason: &str, ctype: &str, body: &[u8], head_only: bool) {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes());
    if !head_only { let _ = stream.write_all(body); }
}

/// Handles one HTTP request: serves the page, the JSON snapshot (cached briefly) or 404; GET and HEAD only.
fn handle(cfg: &Config, port: u16, cache: &Mutex<Cache>, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let mut buf = vec![0u8; 8192];
    let mut n = 0;
    while n < buf.len() {
        match stream.read(&mut buf[n..]) { Ok(0) | Err(_) => break, Ok(k) => { n += k; if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") { break; } } }
    }
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let host = lines.filter_map(|l| l.split_once(':')).find(|(k, _)| k.eq_ignore_ascii_case("host")).map(|(_, v)| v.trim().to_ascii_lowercase()).unwrap_or_default();
    // Only localhost names are served (protects against DNS-rebinding attempts from a web page in a browser).
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        respond(&mut stream, 421, "Misdirected Request", "text/plain; charset=utf-8", b"host not allowed", false);
        return;
    }
    if method != "GET" && method != "HEAD" {
        respond(&mut stream, 405, "Method Not Allowed", "text/plain; charset=utf-8", b"read-only", false);
        return;
    }
    let head_only = method == "HEAD";
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    match path {
        "/" | "/index.html" => respond(&mut stream, 200, "OK", "text/html; charset=utf-8", PAGE.as_bytes(), head_only),
        "/healthz" => respond(&mut stream, 200, "OK", "text/plain; charset=utf-8", b"ok", head_only),
        "/api/snapshot" => {
            let body = {
                let mut c = cache.lock().unwrap();
                if c.at.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
                    c.body = match snapshot(cfg) { Ok(v) => v.to_string(), Err(e) => json!({"error": format!("{e:#}")}).to_string() };
                    c.at = Some(Instant::now());
                }
                c.body.clone()
            };
            let ok = !body.starts_with("{\"error\"");
            respond(&mut stream, if ok { 200 } else { 500 }, if ok { "OK" } else { "Internal Server Error" }, "application/json", body.as_bytes(), head_only);
        }
        "/api/logs" => {
            // live logs (owner 2026-10-11: "logs should be live"): the newest lines of one whitelisted log, read on every request
            let q = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).unwrap_or("");
            let n: usize = q("n").parse().unwrap_or(80).clamp(10, 500);
            let body = match live_log(cfg, q("name")) {
                Some(f) => json!({"file": f.display().to_string(), "lines": tail(&f, n)}),
                None => json!({"error": "unknown log", "logs": LIVE_LOGS.iter().map(|l| l.0).collect::<Vec<_>>()}),
            };
            respond(&mut stream, 200, "OK", "application/json", body.to_string().as_bytes(), head_only);
        }
        _ => respond(&mut stream, 404, "Not Found", "text/plain; charset=utf-8", b"not found", false),
    }
}

/// The org chart's live part: when each Claude job and service last
/// wrote its log. Ages are seconds before `now`; a missing log is null (never a made-up time).
fn team(cfg: &Config, now: i64) -> Value {
    let bot_logs = crate::platform::bot_logs_dir();
    let lab_logs = cfg.root.join("logs");
    let age = |p: std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| now - d.as_secs() as i64);
    json!({
        "claude": {
            "supervisor": supervisor_sessions(&lab_logs.join("supervisor.log")).into_iter().rev().find(|(_, code)| *code == 0).map(|(ts, _)| now - ts),
            "logdoctor": age(bot_logs.join("log_doctor.log")), "fixreport": age(bot_logs.join("fix_report.log")),
            "promote": age(bot_logs.join("promote.launchd.log")),
        },
    })
}

/// Per-block returns of the current strategy and of the validated variants (cuts A and B), for the backtest charts: each series is the blocks in time order (start, return %, trades, wins, chain flag).
fn block_compare(mem: &Connection) -> Value {
    let lb = crate::strategy::leaderboard(mem).unwrap_or_default();
    let mut names: Vec<String> = vec!["baseline".into()];
    let mut good: Vec<&Value> = lb.iter().filter(|l| l["interval"] == "5h" && l["status"] == "validated").collect();
    good.sort_by(|a, b| b["a"]["h2"].as_f64().unwrap_or(0.0).total_cmp(&a["a"]["h2"].as_f64().unwrap_or(0.0)));
    names.extend(good.iter().take(3).filter_map(|l| l["tokens"].as_str().map(String::from)));
    json!(names.iter().flat_map(|n| ["A", "B"].map(|cut| {
        let b: Vec<Value> = crate::strategy::blocks_of(mem, "5h", n, cut).into_iter().map(|(s, r, t, w, c)| json!({"start": s, "ret": r, "trades": t, "wins": w, "chain": c})).collect();
        json!({"variant": n, "cut": cut, "blocks": b})
    })).filter(|v| !v["blocks"].as_array().is_none_or(|a| a.is_empty())).collect::<Vec<_>>())
}

/// "45 min", "3 h", "2 d".
fn human_age(s: i64) -> String {
    if s < 5400 { format!("{} min", (s / 60).max(1)) } else if s < 172_800 { format!("{} h", s / 3600) } else { format!("{} d", s / 86_400) }
}

/// A log write means the supervisor tried to run, not that its Claude session
/// succeeded. Only a zero exit code counts as a fresh scrum-master update.
fn supervisor_sessions(log: &std::path::Path) -> Vec<(i64, i32)> {
    std::fs::read_to_string(log).unwrap_or_default().lines().filter_map(|line| {
        let (stamp, rest) = line.split_once(" session exit ")?;
        let ts = chrono::DateTime::parse_from_rfc3339(stamp).ok()?.timestamp();
        let code = rest.split_whitespace().next()?.parse().ok()?;
        Some((ts, code))
    }).collect()
}

/// Each data source the dashboard shows: newest update (unix time, from the source itself), the longest gap allowed, and whether it
/// is overdue. Market-hours sources (the bot's paper account and decisions) allow a weekend.
fn freshness(cfg: &Config, mem: &Connection, now: i64) -> Vec<Value> {
    let q = |sql: &str| -> Option<i64> { mem.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).ok().flatten() };
    let mtime = |p: std::path::PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
    let archive = Connection::open_with_flags(&cfg.paper.archive_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
    let aq = |sql: &str| -> Option<i64> { archive.as_ref().and_then(|a| a.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).ok().flatten()).map(|ms| ms / 1000) };
    let logs = cfg.root.join("logs");
    let rows: Vec<(&str, Option<i64>, i64)> = vec![
        ("backtests (improve loop)", q("SELECT max(ts) FROM strategy_runs"), 4 * 3600),
        ("hourly lab tick", q("SELECT value FROM counters WHERE name = 'last_tick'"), 150 * 60),
        // the brain and the trainer run 24x7
        ("the brain (a role every 5 minutes)", mtime(logs.join("brain.log")), 30 * 60),
        ("deep-learning trainer", mtime(logs.join("trainer.log")), 45 * 60),
        ("scrum master", supervisor_sessions(&logs.join("supervisor.log")).into_iter().rev().find(|(_, code)| *code == 0).map(|(ts, _)| ts), 2 * 3600),
        ("bot paper account", aq("SELECT max(wall_ms) FROM paper"), 80 * 3600),
        ("bot decisions", aq("SELECT max(wall_ms) FROM events WHERE kind = 'decision'"), 80 * 3600),
    ];
    rows.into_iter().map(|(name, ts, max)| {
        let age = ts.map(|t| (now - t).max(0));
        json!({"name": name, "ts": ts, "age": age, "max": max, "overdue": age.is_none_or(|a| a > max)})
    }).collect()
}

/// Everything the dashboard shows about the lab's own models and the paper account, built from files and the databases only (nothing is computed here):
/// the packaged model (versions, calibration, the steward's reading, the live ranking and decision, the scoreboard), the deep-learning trainer's search, the trial ledger,
/// each brain role's latest run, and the paper account with its positions, trades and the bot's latest decision.
fn ai_inputs(cfg: &Config, mem: &Connection) -> Value {
    let root = &cfg.root;
    let fact = |role: &str, kind: &str| -> Value { mem.query_row("SELECT text, ts FROM brain_facts WHERE role = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1", [role, kind], |r| Ok(json!({"text": r.get::<_, String>(0)?, "ts": r.get::<_, i64>(1)?}))).unwrap_or(Value::Null) };
    let read = |p: std::path::PathBuf, n: usize| std::fs::read_to_string(p).unwrap_or_default().chars().take(n).collect::<String>();
    // the packaged model
    let monitor: Value = serde_json::from_str(&read(root.join("model").join("monitor.json"), 20_000)).unwrap_or(Value::Null);
    let versions: Vec<Value> = crate::aimodel::versions(root).iter().rev().take(8).filter_map(|v| crate::aimodel::load(root, *v).ok().map(|a| {
        let live = &monitor[format!("v{}", a.version)];
        json!({"version": a.version, "created": a.created, "trained_through_ms": a.trained_through_ms, "samples": a.samples, "heldout_ic": a.heldout_ic, "live_ic": live["summary"][0], "live_se": live["summary"][1], "live_sessions": live["sessions"]})
    })).collect();
    let current = crate::aimodel::current(root).and_then(|v| crate::aimodel::load(root, v).ok()).map(|a| json!({"version": a.version, "scheme": a.scheme, "features": a.features.len(), "heldout_ic": a.heldout_ic, "calibration": a.calibration}));
    // the trainer's search and the trial ledger, read-only
    let cdb = Connection::open_with_flags(cfg.candles_db(), OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
    let objective = cdb.as_ref().map(|c| crate::automl::objective_for(crate::mlscores::Scheme::from_ledger(c))).unwrap_or_default();
    let rows = |sql: &str, objective: Option<&str>| -> Vec<Value> { cdb.as_ref().and_then(|c| c.prepare(sql).ok().map(|mut st| st.query_map(rusqlite::params_from_iter(objective), |r| Ok(json!({"a": r.get::<_, Option<String>>(0).ok().flatten(), "b": r.get::<_, Option<f64>>(1).ok().flatten(), "c": r.get::<_, Option<f64>>(2).ok().flatten(), "d": r.get::<_, Option<f64>>(3).ok().flatten(), "e": r.get::<_, Option<f64>>(4).ok().flatten(), "f": r.get::<_, Option<f64>>(5).ok().flatten(), "g": r.get::<_, Option<f64>>(6).ok().flatten(), "h": r.get::<_, Option<i64>>(7).ok().flatten()}))).map(|it| it.flatten().collect()).unwrap_or_default())).unwrap_or_default() };
    // every evaluated network in the order it was tried (fitness over generations): paired gain over the ridge with its interval, and its kind
    let fitness: Vec<Value> = cdb.as_ref().and_then(|c| c.prepare("SELECT id, label, ic, d, d_lo, d_hi, champion, parent FROM automl WHERE status = 'done' AND objective = ?1 ORDER BY id").ok().map(|mut st| st.query_map([&objective], |r| Ok(json!({"id": r.get::<_, i64>(0)?, "kind": r.get::<_, String>(1)?.split_whitespace().next().unwrap_or("?").to_string(), "ic": r.get::<_, Option<f64>>(2)?, "d": r.get::<_, Option<f64>>(3)?, "d_lo": r.get::<_, Option<f64>>(4)?, "d_hi": r.get::<_, Option<f64>>(5)?, "champion": r.get::<_, i64>(6)? == 1, "child": r.get::<_, Option<i64>>(7)?.is_some()}))).map(|it| it.flatten().collect()).unwrap_or_default())).unwrap_or_default();
    // the model's live record over time: per day, the realised excess of its top quarter and bottom quarter of calls (bps), every call scored against the real outcome
    let scorehist: Vec<Value> = cdb.as_ref().and_then(|c| c.prepare("SELECT ts_ms / 86400000, avg(CASE WHEN pct >= 75 THEN excess END), avg(CASE WHEN pct < 25 THEN excess END), count(*) FROM ml_predictions WHERE excess IS NOT NULL GROUP BY ts_ms / 86400000 ORDER BY 1").ok().map(|mut st| st.query_map([], |r| Ok(json!({"t": r.get::<_, i64>(0)? * 86_400, "top": r.get::<_, Option<f64>>(1)?, "bottom": r.get::<_, Option<f64>>(2)?, "n": r.get::<_, i64>(3)?}))).map(|it| it.flatten().collect()).unwrap_or_default())).unwrap_or_default();
    let vol_track: Value = serde_json::from_str(&read(root.join("model").join("vol_track.json"), 2_000_000)).unwrap_or(Value::Array(vec![]));
    let genomes = rows("SELECT label, ic, ic_lo, ic_hi, d, d_lo, d_hi, champion FROM automl WHERE status = 'done' AND objective = ?1 ORDER BY d DESC LIMIT 12", Some(&objective));
    let trainer_counts = rows("SELECT status, count(*), 0, 0, 0, 0, 0, 0 FROM automl WHERE objective = ?1 GROUP BY status", Some(&objective));
    let ledger = rows("SELECT family, count(*), sum(adopted), 0, 0, 0, 0, 0 FROM ml_trials GROUP BY family ORDER BY count(*) DESC", None);
    let ledger_adopted = rows("SELECT family || ': ' || name, value, lo, hi, 0, 0, 0, 0 FROM ml_trials WHERE adopted = 1 ORDER BY ts DESC LIMIT 10", None);
    let tlog: Vec<String> = read(cfg.logs_dir().join("trainer.log"), 400_000).lines().rev().take(8).map(|l| l.chars().take(260).collect()).collect();
    // the latest run of each brain role
    let blog = read(cfg.logs_dir().join("brain.log"), 400_000);
    let roles: Vec<Value> = crate::brain_roles::ROLES.iter().map(|r| {
        let line = blog.lines().rev().find(|l| l.contains(&format!(" {r} ok ")) || l.contains(&format!(" {r} FAILED")));
        json!({"role": r, "line": line.map(|l| l.chars().take(300).collect::<String>())})
    }).collect();
    // the paper account (the bot's archive, read-only)
    let paper = open_ro(&cfg.paper.archive_path).ok().map(|a| {
        let acct = a.query_row("SELECT wall_ms, equity, start_equity, realized, open_pnl, max_drawdown_pct, open_positions, closed_trades, wins, positions FROM paper ORDER BY wall_ms DESC LIMIT 1", [], |r| Ok(json!({
            "ts_ms": r.get::<_, i64>(0)?, "equity": r.get::<_, f64>(1)?, "start": r.get::<_, f64>(2)?, "realized": r.get::<_, f64>(3)?, "open_pnl": r.get::<_, f64>(4)?, "max_drawdown_pct": r.get::<_, f64>(5)?,
            "open_positions": r.get::<_, i64>(6)?, "closed_trades": r.get::<_, i64>(7)?, "wins": r.get::<_, i64>(8)?, "positions": serde_json::from_str::<Value>(&r.get::<_, String>(9)?).unwrap_or(Value::Null)}))).unwrap_or(Value::Null);
        let curve: Vec<Value> = a.prepare("SELECT wall_ms, equity FROM paper WHERE start_equity = (SELECT start_equity FROM paper ORDER BY wall_ms DESC LIMIT 1) ORDER BY wall_ms DESC LIMIT 400").ok()
            .map(|mut st| st.query_map([], |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, f64>(1)?]))).map(|it| it.flatten().collect()).unwrap_or_default()).unwrap_or_default();
        let trades: Vec<Value> = a.prepare("SELECT symbol, side, entry_ms, exit_ms, entry, exit, qty, pnl, fees, reason FROM paper_trades ORDER BY entry_ms DESC LIMIT 40").ok()
            .map(|mut st| st.query_map([], |r| Ok(json!({"symbol": r.get::<_, String>(0)?, "side": r.get::<_, String>(1)?, "entry_ms": r.get::<_, i64>(2)?, "exit_ms": r.get::<_, i64>(3)?, "entry": r.get::<_, f64>(4)?, "exit": r.get::<_, f64>(5)?, "qty": r.get::<_, f64>(6)?, "pnl": r.get::<_, f64>(7)?, "fees": r.get::<_, f64>(8)?, "reason": r.get::<_, String>(9)?}))).map(|it| it.flatten().collect()).unwrap_or_default()).unwrap_or_default();
        let decision = a.query_row("SELECT wall_ms, detail FROM events WHERE kind = 'decision' ORDER BY wall_ms DESC, rowid DESC LIMIT 1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).ok().and_then(|(ms, d)| {
            let v: Value = serde_json::from_str(&d).ok()?;
            let cands = v["candidates"].as_array()?;
            let picked: Vec<Value> = cands.iter().filter(|c| c["outcome"].as_str().is_some_and(|o| !o.starts_with("rejected"))).take(8).map(|c| json!({"symbol": c["symbol"], "rank": c["rank"], "signal": c["signal"], "outcome": c["outcome"]})).collect();
            let rejected = cands.iter().filter(|c| c["outcome"].as_str().is_some_and(|o| o.starts_with("rejected"))).count();
            Some(json!({"ts_ms": ms, "trigger": v["trigger"], "gates": v["gates"], "slot_budget": v["slot_budget"], "candidates": picked, "rejected": rejected, "note": v["allocation_note"]}))
        });
        let pending: Vec<Value> = cfg.paper.archive_path.parent().and_then(|p| p.parent()).map(|p| p.join("ClaudeMoomooBot/paper_xs.json")).and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v["portfolio"]["pending_targets"].as_array().cloned()).unwrap_or_default().iter().filter_map(|t| Some(json!({"symbol": t.get(1)?, "side": t.get(2)?}))).collect();
        json!({"account": acct, "curve": curve, "trades": trades, "decision": decision, "pending": pending})
    });
    json!({
        "model": current, "versions": versions, "model_card": fact("steward", "model-card"), "ic_reading": fact("steward", "ic-reading"), "scoreboard": fact("steward", "scoreboard"),
        "live_ranking": fact("steward", "live-ranking"), "briefing": read(cfg.logs_dir().join("briefing.txt"), 6000),
        "candle_ml": read(cfg.logs_dir().join("candle-ml.txt"), 7000), "candle_improve": read(cfg.logs_dir().join("candle-improve.txt"), 5000), "genomes": genomes, "fitness": fitness, "scorehist": scorehist, "vol_track": vol_track, "trainer_counts": trainer_counts,
        "trainer_log": tlog, "bag": crate::automl::bag_size(root), "ledger": ledger, "ledger_adopted": ledger_adopted, "roles": roles, "paper": paper,
        "gate_off": root.join("ML_GATE_OFF").exists(),
    })
}


/// The logs the dashboard shows live: (name, file under the lab's logs folder; "tick" = the newest tick log). The bot's own logs
/// stay on the bot console.
pub const LIVE_LOGS: &[(&str, &str)] = &[("improve", "improve.log"), ("brain", "brain.log"), ("trainer", "trainer.log"), ("tick", ""), ("supervisor", "supervisor.log"), ("monitor", "monitor.log")];

fn live_log(cfg: &Config, name: &str) -> Option<std::path::PathBuf> {
    let (_, file) = LIVE_LOGS.iter().find(|l| l.0 == name)?;
    let dir = cfg.root.join("logs");
    if file.is_empty() {
        return std::fs::read_dir(&dir).ok()?.filter_map(Result::ok).map(|e| e.path())
            .filter(|p| p.file_name().and_then(|f| f.to_str()).is_some_and(|f| f.starts_with("tick-") && f.ends_with(".log")))
            .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    }
    Some(dir.join(file))
}

/// The last `n` lines of `f` (reads at most the last 256 KB).
fn tail(f: &std::path::Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut fh) = std::fs::File::open(f) else { return vec![] };
    let len = fh.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = fh.seek(SeekFrom::Start(len.saturating_sub(256 * 1024)));
    let mut buf = Vec::new();
    let _ = fh.read_to_end(&mut buf);
    let t = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = t.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| l.to_string()).collect()
}

/// Serve until `max_requests` connections have been handled (None = forever). The listener is bound to localhost by the caller.
pub fn serve_listener(cfg: Arc<Config>, listener: TcpListener, max_requests: Option<usize>) -> Result<()> {
    let port = listener.local_addr()?.port();
    let cache = Arc::new(Mutex::new(Cache { at: None, body: String::new() }));
    let mut handled = 0usize;
    let mut threads = Vec::new();
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let (cfg, cache) = (cfg.clone(), cache.clone());
        threads.push(std::thread::spawn(move || handle(&cfg, port, &cache, stream)));
        handled += 1;
        if max_requests.is_some_and(|m| handled >= m) { break; }
    }
    for t in threads { let _ = t.join(); }
    Ok(())
}

/// Serves the dashboard on 127.0.0.1 at the configured port until stopped.
pub fn serve(cfg: Config) -> Result<()> {
    // schema only: create any table a newer lab version added (no data is changed), so the read-only snapshot never fails on a
    // table the strategy loop has not created yet
    ensure_schema(&cfg)?;
    let port = cfg.dashboard.port;
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| anyhow::anyhow!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    eprintln!("qwen-lab monitor on http://127.0.0.1:{port}/ (read-only)");
    serve_listener(Arc::new(cfg), listener, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::SynthSpec;
    use crate::testutil::{lab, Lab};

    /// Test fixture: a lab with seven windows of synthetic data.
    fn busy_lab() -> Lab {
        lab(&SynthSpec { windows: 7, days_per_window: 28, entries_per_day: 8, runs_per_window: 6, planted: Some(("vol_ratio".into(), 0.02)), seed: 5, ..Default::default() })
    }

    /// Test: snapshot reports real state without changing anything.
    #[test]
    fn snapshot_reports_real_state_without_changing_anything() {
        let l = busy_lab();
        let before = (l.mem.query_row::<i64, _, _>("SELECT count(*) FROM counters", [], |r| r.get(0)).unwrap(), l.data.query_row::<i64, _, _>("SELECT count(*) FROM runs", [], |r| r.get(0)).unwrap());
        let s = snapshot(&l.cfg).unwrap();
        assert!(s["inputs"]["backtest"]["windows"].as_array().unwrap().len() == 7);
        // daily strategy PnL comes only from bot-archive oos runs with a recorded capital: none here, so no chart rather than a wrong one
        assert!(s["inputs"]["block_compare"].is_array() && s["inputs"]["security_counts"].is_array(), "the chart data is in the snapshot");
        assert!(s["inputs"]["ai"]["fitness"].is_array() && s["inputs"]["ai"]["scorehist"].is_array() && s["inputs"]["ai"]["vol_track"].is_array());
        assert!(s["inputs"]["backtest"]["pnl_capital"].is_null());
        assert!(s["inputs"]["backtest"]["per_day"].as_array().unwrap().is_empty());
        assert!(s["alerts"].as_array().unwrap().iter().any(|a| a.as_str().unwrap().contains("no tick has run")), "{}", s["alerts"]);
        let after = (l.mem.query_row::<i64, _, _>("SELECT count(*) FROM counters", [], |r| r.get(0)).unwrap(), l.data.query_row::<i64, _, _>("SELECT count(*) FROM runs", [], |r| r.get(0)).unwrap());
        assert_eq!(before, after, "reading the dashboard must change nothing");
    }

    /// Test: an empty lab shows blockers not errors.
    #[test]
    fn an_empty_lab_shows_blockers_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_root(dir.path(), "real");
        cfg.paper.archive_path = dir.path().join("none.sqlite3");
        cfg.backtest_archive.path = dir.path().join("none-bt.sqlite3");
        let mem = db::open(&cfg.memory_db(), true).unwrap();
        let data = db::open(&cfg.data_db(), false).unwrap();
        db::migrate_memory(&mem).unwrap();
        db::migrate_data(&data).unwrap();
        drop((mem, data));
        let s = snapshot(&cfg).unwrap();
        assert_eq!(s["inputs"]["paper"]["state"], "empty");
        assert_eq!(s["status"]["level"], "attention");
    }

    /// Test: tick logs are parsed into steps.
    #[test]
    fn tick_logs_are_parsed_into_steps() {
        let l = busy_lab();
        std::fs::create_dir_all(l.cfg.logs_dir()).unwrap();
        std::fs::write(l.cfg.logs_dir().join("tick-1800000000.log"), "tick (full)\n  [ok] guards: load 1.0 (cap 8.5)\n  [skipped] winrate: too few runs\n  [failed] cull: boom\n").unwrap();
        std::fs::write(l.cfg.logs_dir().join("tick-1800003600.log"), "tick (light)\n  [ok] report: /x\n").unwrap();
        let t = ticks(&l.cfg, 10);
        assert_eq!((t[0]["ts"].as_i64().unwrap(), t[0]["mode"].as_str().unwrap()), (1_800_003_600, "light"));
        assert_eq!(t[1]["steps"][2]["status"], "failed");
        assert_eq!(t[1]["steps"][1]["name"], "winrate");
        let s = snapshot(&l.cfg).unwrap();
        assert!(s["alerts"].as_array().unwrap().iter().all(|a| !a.as_str().unwrap().contains("`cull` failed")), "only the NEWEST tick's failures raise an alert");
        // nothing stale is shown: a failure that a newer tick cleared disappears; one not cleared yet stays
        std::fs::write(l.cfg.logs_dir().join("tick-1800007200.log"), "tick (full)\n  [ok] cull: 3 rows\n").unwrap();
        let t = ticks(&l.cfg, 10);
        assert!(t.iter().flat_map(|x| x["steps"].as_array().unwrap()).all(|st| !(st["name"] == "cull" && st["status"] == "failed")), "{t:?}");
        assert_eq!(t[2]["steps"].as_array().unwrap().len(), 2, "guards and winrate stay (nothing newer replaced them)");
        // a failure in an older tick of the same mode is history even when no newer tick ran that step
        std::fs::write(l.cfg.logs_dir().join("tick-1800010800.log"), "tick (light)\n  [failed] promote: old boom\n").unwrap();
        std::fs::write(l.cfg.logs_dir().join("tick-1800014400.log"), "tick (light)\n  [ok] report: /y\n").unwrap();
        let t = ticks(&l.cfg, 10);
        assert!(t.iter().flat_map(|x| x["steps"].as_array().unwrap()).all(|st| st["status"] != "failed"), "{t:?}");
    }

    #[test]
    fn the_page_renders_a_snapshot_with_the_team_first_and_details_folded() {
        let Ok(node) = which_node() else { eprintln!("node not installed: render smoke test skipped"); return };
        let l = busy_lab();
        let dir = tempfile::tempdir().unwrap();
        let script = PAGE.split("<script>").nth(1).unwrap().split("</script>").next().unwrap();
        std::fs::write(dir.path().join("page.js"), script).unwrap();
        std::fs::write(dir.path().join("snap.json"), snapshot(&l.cfg).unwrap().to_string()).unwrap();
        std::fs::write(dir.path().join("smoke.js"), include_str!("dash_smoke.js")).unwrap();
        let o = std::process::Command::new(node).arg(dir.path().join("smoke.js")).arg(dir.path().join("page.js")).arg(dir.path().join("snap.json")).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(o.status.success(), "{}{}", out, String::from_utf8_lossy(&o.stderr));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("The team"), "{out}");
        assert!(lines.iter().any(|l| l.starts_with("[fold] The brain")), "{out}");
        assert!(lines.iter().position(|l| l.starts_with("Live strategy")) < lines.iter().position(|l| l.starts_with("[fold]")), "{out}");
    }

    fn which_node() -> Result<String> {
        crate::platform::which("node").map(|p| p.to_string_lossy().into_owned()).ok_or_else(|| anyhow::anyhow!("no node"))
    }

    #[test]
    fn every_source_reports_its_age_and_an_overdue_one_is_flagged() {
        let l = busy_lab();
        l.mem.execute("INSERT INTO counters(name, value) VALUES ('last_tick', 1000) ON CONFLICT(name) DO UPDATE SET value = 1000", []).unwrap();
        let f = freshness(&l.cfg, &l.mem, 1000 + 60);
        let tick = f.iter().find(|x| x["name"] == "hourly lab tick").unwrap();
        assert_eq!((tick["age"].as_i64(), tick["overdue"].as_bool()), (Some(60), Some(false)));
        let f = freshness(&l.cfg, &l.mem, 1000 + 151 * 60);
        assert_eq!(f.iter().find(|x| x["name"] == "hourly lab tick").unwrap()["overdue"], true);
        assert_eq!(f.iter().find(|x| x["name"] == "scrum master").unwrap()["overdue"], true, "a source never written is overdue, not current");
        assert_eq!(human_age(45 * 60), "45 min");
    }

    #[test]
    fn failed_supervisor_attempts_never_count_as_fresh_updates() {
        let l = busy_lab();
        let log = l.cfg.logs_dir().join("supervisor.log");
        std::fs::create_dir_all(l.cfg.logs_dir()).unwrap();
        std::fs::write(&log, "2026-10-17T01:00:00Z session exit 1 -> failed.md\n2026-10-17T01:00:00Z done\n").unwrap();
        let snap = snapshot(&l.cfg).unwrap();
        assert!(snap["alerts"].as_array().unwrap().iter().any(|a| a.as_str().unwrap().contains("scrum master last session failed (exit 1)")));
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-17T01:30:00Z").unwrap().timestamp();
        let scrum = |now| freshness(&l.cfg, &l.mem, now).into_iter().find(|f| f["name"] == "scrum master").unwrap();
        assert_eq!(scrum(now)["overdue"], true);
        std::fs::write(&log, "2026-10-17T01:00:00Z session exit 1 -> failed.md\n2026-10-17T01:10:00Z session exit 0 -> ok.md\n2026-10-17T01:20:00Z session exit 1 -> failed-again.md\n").unwrap();
        let success = chrono::DateTime::parse_from_rfc3339("2026-10-17T01:10:00Z").unwrap().timestamp();
        assert_eq!(scrum(now)["ts"], success);
        assert_eq!(supervisor_sessions(&log).last().unwrap().1, 1);
    }

    /// Test helper: sends a raw HTTP request to the dashboard and returns the response.
    fn get(addr: &str, req: &str) -> String {
        let mut s = TcpStream::connect(addr).unwrap();
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    /// Test: the server is read only localhost only and self contained.
    #[test]
    fn the_server_is_read_only_localhost_only_and_self_contained() {
        let l = busy_lab();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let cfg = Arc::new(l.cfg.clone());
        let t = std::thread::spawn(move || serve_listener(cfg, listener, Some(9)).unwrap());
        let ok = |path: &str| get(&addr, &format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n"));
        let page = ok("/");
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("SPY Bot Lab monitor") && page.contains("Content-Security-Policy"));
        let body = page.split("\r\n\r\n").nth(1).unwrap();
        assert!(!body.contains("https://") && !body.contains("src=\"http"), "the page must not load anything from the internet");
        assert!(body.contains("textContent") && !body.contains("innerHTML"), "untrusted text must be inserted as text, never as HTML");
        let api = ok("/api/snapshot");
        assert!(api.starts_with("HTTP/1.1 200") && api.contains("application/json"));
        let v: Value = serde_json::from_str(api.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert!(v["inputs"]["backtest"]["windows"].as_array().is_some());
        assert!(ok("/healthz").ends_with("ok"));
        // live logs: only whitelisted names, newest lines last
        std::fs::create_dir_all(l.cfg.logs_dir()).unwrap();
        std::fs::write(l.cfg.logs_dir().join("improve.log"), (1..=300).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
        let v: Value = serde_json::from_str(ok("/api/logs?name=improve&n=20").split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!((v["lines"].as_array().unwrap().len(), v["lines"][19].as_str()), (20, Some("line 300")));
        assert!(ok("/api/logs?name=../../etc/passwd").contains("unknown log"));
        assert!(ok("/nope").starts_with("HTTP/1.1 404"));
        // writes are refused, and a foreign Host header (DNS rebinding) is refused
        assert!(get(&addr, &format!("POST /api/snapshot HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\n\r\n")).starts_with("HTTP/1.1 405"));
        assert!(get(&addr, "GET /api/snapshot HTTP/1.1\r\nHost: evil.example.com\r\n\r\n").starts_with("HTTP/1.1 421"));
        assert!(get(&addr, &format!("HEAD / HTTP/1.1\r\nHost: {addr}\r\n\r\n")).starts_with("HTTP/1.1 200"));
        t.join().unwrap();
    }
}

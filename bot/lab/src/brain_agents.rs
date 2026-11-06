//! Six agents that DO things (owner 2026-10-13: "the local model needs agents which do things ... build it all ... make sure it learns 24x7").
//! They join the six reporting roles in `brain_roles` and take turns in the same always-on process (a role every 5 minutes, so each runs hourly, day and night).
//! Every action goes through code that checks it, is logged as a knowledge-store fact, and can be undone by deleting a plain file. None touches the live strategy,
//! an account, an order or an exit.
//!
//!   data          keeps the candle store current and clean; watches gaps, stale symbols and moomoo's candle quota (it never spends the quota)
//!   steward       owns the ML model: re-scores when new bars arrive, watches the last sessions' out-of-sample IC, switches the model off (file ML_GATE_OFF) when
//!                 it is significantly negative and back on when it recovers
//!   drift         compares the paper bot's real fills with the candles: observed slippage against the cost table, and how the model rated the entries it took
//!   risk          watches the paper account against the owner's rules (drawdown cap, worst-block rule, positions) and alerts early
//!   promotion     prepares an evidence packet for every variant that passes the lab's full validation, and lists the ones that need a code review
//!   experimenter  finds where a lead loses (block by block against the baseline) and keeps the standing ML experiments running when no new data arrives

use crate::brain_roles::{recent, remember, DAY};
use crate::candles;
use crate::context::Ctx;
use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const AGENT_ROLES: [&str; 6] = ["data", "steward", "drift", "risk", "promotion", "experimenter"];

fn bot_db() -> PathBuf { crate::platform::bot_store_path() }
pub fn scores_path() -> PathBuf { crate::platform::bot_scores_path() }
fn ro(p: &std::path::Path) -> rusqlite::Result<Connection> { Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) }

pub fn run(ctx: &Ctx, role: &str) -> Result<Vec<String>> {
    match role {
        "data" => data(ctx), "steward" => steward(ctx), "drift" => drift(ctx), "risk" => risk(ctx), "promotion" => promotion(ctx), "experimenter" => experimenter(ctx),
        other => Err(anyhow::anyhow!("unknown agent {other}")),
    }
}

fn alert(ctx: &Ctx, role: &str, subject: &str, title: &str, text: &str) {
    if !recent(ctx.mem, role, "alert", subject, DAY).unwrap_or(true) {
        let _ = remember(ctx.mem, role, "alert", subject, text, "agent");
        crate::ops::discord(title, text);
    }
}

// ------------------------------------------------------------------ data
/// Sessions are the New York dates of the bars; coverage = bars present / bars expected (26 a session) over the last `n` sessions of the reference symbol.
pub fn coverage(series: &BTreeMap<String, Vec<candles::Bar>>, symbol: &str, n: usize) -> Option<f64> {
    let day = |ts: i64| (ts + crate::mlcandle::et_offset_ms(ts)).div_euclid(86_400_000);
    let spy = series.get("SPY")?;
    let mut days: Vec<i64> = spy.iter().map(|b| day(b.ts)).collect(); days.dedup();
    let last: std::collections::BTreeSet<i64> = days.iter().rev().take(n).copied().collect();
    let bars = series.get(symbol)?.iter().filter(|b| last.contains(&day(b.ts))).count();
    Some(bars as f64 / (last.len() * 26) as f64)
}

pub fn data(ctx: &Ctx) -> Result<Vec<String>> {
    let db = Connection::open(ctx.cfg.candles_db())?;
    let hist = crate::platform::bot_history_json();
    let (read, written, dropped) = candles::import(Some(&hist), Some(&bot_db()), &db)?;
    let n_univ = candles::import_universe(&crate::platform::bot_holdings_csv(), &db).unwrap_or(0);
    let series = candles::load(&db)?;
    let uni = candles::load_universe(&db);
    let latest = uni.iter().filter_map(|s| series.get(s).and_then(|b| b.last()).map(|b| b.ts)).max().unwrap_or(0);
    let stale: Vec<&String> = uni.iter().filter(|s| series.get(*s).is_some_and(|b| b.last().is_none_or(|x| latest - x.ts > 4 * 86_400_000))).collect();
    let have = uni.iter().filter(|s| series.contains_key(*s)).count();
    let mut thin: Vec<(String, f64)> = uni.iter().filter_map(|s| coverage(&series, s, 20).map(|c| (s.clone(), c))).filter(|x| x.1 < 0.9).collect();
    thin.sort_by(|a, b| a.1.total_cmp(&b.1));
    let quota = ro(&bot_db()).ok().and_then(|c| c.query_row("SELECT used, remain FROM quota ORDER BY ts_ms DESC LIMIT 1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).ok());
    let line = format!("{written} bars stored ({read} read this pass, {dropped} invalid dropped); {have} of {n_univ} SPY holdings have candles ({} not pulled: moomoo allows 100 symbols per 7 days{}; the lab never spends that quota); newest bar {} ({:.1} days old); {} symbols stale for over 4 days; {} thin over the last 20 sessions{}",
        n_univ.saturating_sub(have), quota.map_or(String::new(), |(u, r)| format!(", {u} used and {r} left")), crate::util::utc_minute(latest / 1000), (crate::util::now_ts() * 1000 - latest) as f64 / 86_400_000.0, stale.len(), thin.len(),
        thin.first().map_or(String::new(), |t| format!(" (worst {} at {:.0}% of its bars)", t.0, t.1 * 100.0)));
    if !recent(ctx.mem, "data", "state", "pass", 6 * 3600)? { remember(ctx.mem, "data", "state", "pass", &line, "candles::import")?; }
    let lag_days = (crate::util::now_ts() * 1000 - latest) as f64 / 86_400_000.0;
    if lag_days > 3.0 {
        alert(ctx, "data", "lag", "Data agent: candles are behind", &format!("the newest bar is {lag_days:.1} days old; the bot's candle pull or the import may have stopped"));
    }
    if !stale.is_empty() {
        // a symbol without bars for days while others have them: a halted or delisted name, or a failed pull
        alert(ctx, "data", &format!("stale:{}", stale.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>().join(",")), "Data agent: stale candles",
            &format!("{} SPY holdings have no bars for over 4 days (first: {})", stale.len(), stale.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
    }
    Ok(vec![format!("data: {line}")])
}

// ------------------------------------------------------------------ steward
/// Mean and standard error of the daily rank IC of `table` (scores at bars) against the realised excess returns of the last `days` sessions with complete labels.
pub fn recent_ic(series: &BTreeMap<String, Vec<candles::Bar>>, table: &BTreeMap<String, Vec<(i64, f64)>>, days: usize) -> Option<(f64, f64, usize)> {
    let names: Vec<&String> = series.keys().collect();
    let samples = crate::mlcandle::build(series, 4);
    let mut by: BTreeMap<i64, Vec<(f64, f64)>> = BTreeMap::new();
    for s in &samples {
        let Some(v) = table.get(names[s.sym]) else { continue };
        if let Ok(i) = v.binary_search_by_key(&s.ts, |x| x.0) { by.entry(s.ts).or_default().push((v[i].1, s.excess)); }
    }
    let mut per_day: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for (ts, v) in &by {
        if v.len() < crate::mlcandle::MIN_CROSS { continue; }
        let (p, e): (Vec<f64>, Vec<f64>) = v.iter().copied().unzip();
        per_day.entry((ts + crate::mlcandle::et_offset_ms(*ts)).div_euclid(86_400_000)).or_default().push(crate::brain::spearman(&p, &e));
    }
    let daily: Vec<f64> = per_day.values().rev().take(days).map(|v| v.iter().sum::<f64>() / v.len() as f64).collect();
    if daily.len() < 8 { return None; }
    let n = daily.len() as f64;
    let mean = daily.iter().sum::<f64>() / n;
    let sd = (daily.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    Some((mean, sd / n.sqrt(), daily.len()))
}

/// The switch decision with hysteresis: off when the mean is significantly negative (95%), back on once it is above zero. None = keep the state.
pub fn gate_decision(currently_off: bool, mean: f64, se: f64, n: usize) -> Option<bool> {
    if n < 10 { return None; }
    if !currently_off && mean + 1.96 * se < 0.0 { return Some(true); }
    if currently_off && mean > 0.0 { return Some(false); }
    None
}

pub fn steward(ctx: &Ctx) -> Result<Vec<String>> {
    let db = Connection::open(ctx.cfg.candles_db())?;
    let newest: i64 = db.query_row("SELECT coalesce(max(ts_ms), 0) FROM candles15", [], |r| r.get(0))?;
    let through: Option<String> = ctx.mem.query_row("SELECT text FROM brain_facts WHERE role = 'steward' AND kind = 'scored-through' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
    let off_file = ctx.cfg.root.join("ML_GATE_OFF");
    let path = scores_path();
    let has_reading = latest_fact(ctx.mem, "steward", "ic-reading").is_some();
    if has_reading && through.as_deref().and_then(|t| t.parse::<i64>().ok()) == Some(newest) && path.exists() {
        return Ok(vec![format!("model unchanged: no bars newer than {} since the last scoring", crate::util::utc_minute(newest / 1000))]);
    }
    let scheme = crate::mlscores::Scheme::from_ledger(&db);
    let series = candles::load_prepared(&db)?;
    let table = crate::mlscores::table(&series, scheme);
    let mut out = vec![];
    let ic = recent_ic(&series, &table, 15);
    let was_off = off_file.exists();
    let mut off = was_off;
    if let Some((mean, se, n)) = ic {
        let reading = format!("out-of-sample IC of the last {n} sessions: {mean:+.4} (standard error {se:.4})");
        remember(ctx.mem, "steward", "ic-reading", "r", &reading, "recent_ic")?;
        out.push(reading);
        match gate_decision(was_off, mean, se, n) {
            Some(true) => { std::fs::write(&off_file, format!("switched off by the steward: IC {mean:+.4} +- {se:.4} over {n} sessions\n"))?; off = true;
                crate::ops::discord("ML model SWITCHED OFF by the steward", &format!("the last {n} sessions' out-of-sample IC is {mean:+.4} (standard error {se:.4}), significantly below zero; the bot's backtests ignore the model until it recovers (delete ML_GATE_OFF to override)")); }
            Some(false) => { let _ = std::fs::remove_file(&off_file); off = false;
                crate::ops::discord("ML model switched back ON by the steward", &format!("the last {n} sessions' out-of-sample IC is {mean:+.4}, above zero again")); }
            None => {}
        }
    } else {
        out.push("too few completed sessions to judge the recent IC".into());
    }
    // the file the bot's backtester reads: the scores, or an empty table while the switch is on (no scores = no gating, no ML signal)
    let tmp = path.with_extension("json.tmp");
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    if off { std::fs::write(&tmp, b"{}")?; } else {
        let rounded: BTreeMap<&String, Vec<(i64, f64)>> = table.iter().map(|(k, v)| (k, v.iter().map(|(a, b)| (*a, (*b * 1e7).round() / 1e7)).collect())).collect();
        std::fs::write(&tmp, serde_json::to_vec(&rounded)?)?;
    }
    std::fs::rename(&tmp, &path)?;
    remember(ctx.mem, "steward", "scored-through", "t", &newest.to_string(), "mlscores::table")?;
    // the packaged model: a new version every 14 sessions, every version's live out-of-sample record, rollback, and today's live ranking
    match crate::aimodel::maintain(&ctx.cfg.root, &series, scheme) {
        Ok(lines) => { if !lines.is_empty() { remember(ctx.mem, "steward", "model-version", "v", &lines.join(" | "), "aimodel::maintain")?; } out.extend(lines); }
        Err(e) => out.push(format!("model lifecycle failed: {e:#}")),
    }
    if let Some(cur) = crate::aimodel::current(&ctx.cfg.root).and_then(|v| crate::aimodel::load(&ctx.cfg.root, v).ok()) {
        if let Some((ts, picks)) = crate::aimodel::predict_live(&series, &cur, &crate::automl::load_champions(&ctx.cfg.root)) {
            let f = |p: &crate::aimodel::Pick| format!("{} {:+.0} bps", p.symbol, p.expected_bps);
            let line = format!("live ranking (version {}, bar {}): best {} | worst {}", cur.version, crate::util::utc_minute(ts / 1000), picks.iter().take(3).map(f).collect::<Vec<_>>().join(", "), picks.iter().rev().take(3).map(f).collect::<Vec<_>>().join(", "));
            remember(ctx.mem, "steward", "live-ranking", "r", &line, "aimodel::predict_live")?;
            out.push(line);
            // constant testing: store this call, resolve every call whose outcome has arrived, and keep the model's real scoreboard
            crate::aimodel::record_predictions(&db, cur.version, ts, &picks)?;
            crate::aimodel::resolve_predictions(&db, &series)?;
            let sb = crate::aimodel::scoreboard(&db, None)?.line();
            remember(ctx.mem, "steward", "scoreboard", "s", &sb, "aimodel::scoreboard")?;
            out.push(format!("live scoreboard: {sb}"));
        }
    }
    let card = format!("ML model card: ridge with market-regime interactions on the bot's score components, top-20 universe; training {}; {} symbols scored, {} scores; ledger trials {}; gate {}",
        match (scheme.window_days, scheme.half_life_days) { (Some(d), _) => format!("rolling {d} days"), (_, Some(h)) => format!("half-life {h:.0} days"), _ => "all older history".to_string() }, table.len(), table.values().map(|v| v.len()).sum::<usize>(), crate::mlimprove::count(&db), if off { "OFF (switch file present)" } else { "on" });
    remember(ctx.mem, "steward", "model-card", "card", &card, "steward")?;
    out.push(card);
    // the daily trading briefing: regime, risk forecast, the ranking after costs, the decision and the confidence
    {
        match crate::intel::briefing(&ctx.cfg.root, &db, &series) {
            Ok(mut b) => {
                if let Ok(data) = rusqlite::Connection::open_with_flags(ctx.cfg.data_db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) { if let Ok(g) = crate::intel::gate_on_trades(&db, &data) { b += &g; } }
                let _ = std::fs::write(ctx.cfg.root.join("logs").join("briefing.txt"), &b);
                let key: String = b.lines().filter(|l| l.starts_with("RISK") || l.starts_with("DIRECTION") || l.starts_with("STANCE") || l.starts_with("DECISION take") || l.contains("skip the lowest 35%")).map(|l| l.chars().take(130).collect::<String>()).collect::<Vec<_>>().join("\n");
                remember(ctx.mem, "steward", "briefing", "latest", &key, "intel::briefing")?; out.push(format!("trading briefing: {}", b.lines().find(|l| l.starts_with("STANCE")).unwrap_or("written"))); }
            Err(e) => out.push(format!("briefing failed: {e:#}")),
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------ drift
pub fn drift(ctx: &Ctx) -> Result<Vec<String>> {
    let Ok(bot) = ro(&bot_db()) else { return Ok(vec!["the bot's store is not readable".into()]) };
    let mut st = bot.prepare("SELECT symbol, side, entry_ms, exit_ms, entry, exit FROM paper_trades ORDER BY entry_ms DESC LIMIT 400")?;
    let trades: Vec<(String, String, i64, i64, f64, f64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.flatten().collect();
    if trades.is_empty() { return Ok(vec!["no paper trades yet: nothing to compare with the candles".into()]); }
    let db = Connection::open(ctx.cfg.candles_db())?;
    let open_at = |sym: &str, ms: i64| -> Option<(i64, f64)> { db.query_row("SELECT ts_ms, open FROM candles15 WHERE symbol = ?1 AND ts_ms <= ?2 ORDER BY ts_ms DESC LIMIT 1", rusqlite::params![sym, ms], |r| Ok((r.get(0)?, r.get(1)?))).ok() };
    let (mut slip, mut n) = (0.0, 0usize);
    for t in &trades {
        if let Some((_, o)) = open_at(&t.0, t.2) { slip += (t.4 / o - 1.0) * if t.1.to_lowercase().contains("sell") || t.1 == "short" { -1.0 } else { 1.0 }; n += 1; }
        if t.3 > 0 { if let Some((_, o)) = open_at(&t.0, t.3) { slip += (1.0 - t.5 / o) * if t.1 == "short" { -1.0 } else { 1.0 }; n += 1; } }
    }
    let costs = candles::load_costs(&db);
    let observed_bps = if n > 0 { slip / n as f64 * 1e4 } else { 0.0 };
    let modeled_bps = costs.default_half * 1e4;
    // how the model rated the entries the bot actually took (decision bar = the bar before the fill)
    let view = std::fs::read(scores_path()).ok().and_then(|b| serde_json::from_slice::<BTreeMap<String, Vec<(i64, f64)>>>(&b).ok());
    let mut rated = (0usize, 0usize);
    if let Some(table) = &view {
        for t in trades.iter().take(100) {
            let Some((ts, _)) = open_at(&t.0, t.2 - 900_000) else { continue };
            let at: Vec<f64> = table.values().filter_map(|v| v.binary_search_by_key(&ts, |x| x.0).ok().map(|i| v[i].1)).collect();
            let Some(mine) = table.get(&t.0).and_then(|v| v.binary_search_by_key(&ts, |x| x.0).ok().map(|i| v[i].1)) else { continue };
            if at.len() >= 10 { rated.0 += 1; if (at.iter().filter(|x| **x < mine).count() as f64 / at.len() as f64) < 0.35 { rated.1 += 1; } }
        }
    }
    let line = format!("paper fills vs candles: {n} fills compared, average cost against the bar open {observed_bps:+.1} bps (the lab's cost table assumes a {modeled_bps:.1} bps half-spread); {} of {} rated entries were in the model's bottom 35%", rated.1, rated.0);
    if !recent(ctx.mem, "drift", "state", "pass", 6 * 3600)? { remember(ctx.mem, "drift", "state", "pass", &line, "paper_trades")?; }
    if n >= 20 && observed_bps > 2.0 * modeled_bps.max(1.0) {
        alert(ctx, "drift", "costs", "Drift agent: real costs above the lab's model", &format!("{line}; the backtests may be too kind"));
    }
    Ok(vec![line])
}

// ------------------------------------------------------------------ risk
pub fn risk(ctx: &Ctx) -> Result<Vec<String>> {
    let Ok(bot) = ro(&bot_db()) else { return Ok(vec!["the bot's store is not readable".into()]) };
    let rows: Vec<(i64, f64, f64, f64, i64)> = bot.prepare("SELECT wall_ms, equity, coalesce(start_equity, equity), coalesce(max_drawdown_pct, 0), coalesce(open_positions, 0) FROM paper ORDER BY wall_ms")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?.flatten().collect();
    let Some(newest) = rows.last().copied() else { return Ok(vec!["no paper account history yet".into()]) };
    // only the current account size: earlier rows come from test runs with other capital (one had 999,991), which would fake a -99% window
    let rows: Vec<(i64, f64, f64, f64, i64)> = rows.into_iter().filter(|r| (r.2 - newest.2).abs() < 1.0).collect();
    let Some(last) = rows.last() else { return Ok(vec!["no paper account history yet".into()]) };
    let days = (last.0 - rows[0].0) / 86_400_000;
    let ret = (last.1 / last.2 - 1.0) * 100.0;
    // worst trailing return over any window of about 14 sessions that the history covers
    let win = 14 * 86_400_000 * 7 / 5;
    let mut worst = 0.0f64;
    for (i, a) in rows.iter().enumerate() { if let Some(b) = rows[i..].iter().rev().find(|b| b.0 - a.0 <= win) { worst = worst.min((b.1 / a.1 - 1.0) * 100.0); } }
    let mut flags = vec![];
    if last.3 >= 20.0 { flags.push(format!("drawdown {:.1}% is within 5 points of the 25% cap", last.3)); }
    if last.4 > 2 { flags.push(format!("{} open positions, the rule is 2", last.4)); }
    if days >= 10 && worst <= -6.0 { flags.push(format!("worst 14-session stretch {worst:.1}% is within 2 points of the -8% rule")); }
    let line = format!("paper account over {days} days: equity {:.0} ({ret:+.2}% since start), max drawdown {:.1}%, {} open positions, worst 14-session window {worst:+.1}%{}", last.1, last.3, last.4, if flags.is_empty() { String::new() } else { format!(" -- WATCH: {}", flags.join("; ")) });
    if !recent(ctx.mem, "risk", "state", "pass", 6 * 3600)? { remember(ctx.mem, "risk", "state", "pass", &line, "paper")?; }
    if !flags.is_empty() { alert(ctx, "risk", &flags.join("|"), "Risk agent: paper account near a rule", &line); }
    Ok(vec![line])
}

// ------------------------------------------------------------------ promotion
pub fn promotion(ctx: &Ctx) -> Result<Vec<String>> {
    let (ok, not) = crate::promote::qualifying(ctx.mem)?;
    let mut out = vec![];
    for b in &ok {
        let key = format!("{}|{}", b["interval"].as_str().unwrap_or(""), b["tokens"].as_str().unwrap_or(""));
        if recent(ctx.mem, "promotion", "packet", &key, 7 * DAY)? { continue; }
        let dir = ctx.cfg.root.join("brain/reports");
        std::fs::create_dir_all(&dir)?;
        let file = dir.join(format!("promotion-{}.md", key.replace(['|', '+', ':'], "_")));
        std::fs::write(&file, format!("# Promotion evidence: {key}\n\nPassed the lab's full validation (all block cuts, noisy tape, holdout). Numbers below are the lab's own, unchanged.\n\n```json\n{}\n```\n\n## Rollback\nThe promotion job writes a request; the previous live options are restored by removing this variant from src/engine/live_options.rs LIVE in the bot repo and redeploying (git history has the earlier file).\n",
            serde_json::to_string_pretty(b)?))?;
        remember(ctx.mem, "promotion", "packet", &key, &format!("evidence packet written: {}", file.display()), "promote::qualifying")?;
        crate::ops::discord("Promotion agent: a variant passed the full validation", &format!("{key} qualifies; evidence packet {}", file.display()));
        out.push(format!("evidence packet for {key}"));
    }
    for line in not.iter().filter(|l| l.contains("research switch") || l.contains("code change")) {
        let key: String = line.chars().take(60).collect();
        if recent(ctx.mem, "promotion", "needs-review", &key, 7 * DAY)? { continue; }
        remember(ctx.mem, "promotion", "needs-review", &key, line, "promote::qualifying")?;
        out.push(format!("needs a code review before it could go live: {line}"));
    }
    if out.is_empty() { out.push(format!("{} variants qualify, {} are being validated or blocked; no new packet", ok.len(), not.len())); }
    Ok(out)
}

// ------------------------------------------------------------------ experimenter
/// Blocks (cut A, 5h) where `tokens` lost to the baseline by more than 2 points: (block start s, variant ret, baseline ret).
pub fn lost_blocks(mem: &Connection, tokens: &str) -> Result<(usize, Vec<(i64, f64, f64)>)> {
    let mut st = mem.prepare("SELECT v.start, v.ret, b.ret FROM strategy_blocks v JOIN strategy_blocks b ON b.interval = v.interval AND b.equity = v.equity AND b.cut = v.cut AND b.idx = v.idx AND b.tokens = 'baseline'
        WHERE v.tokens = ?1 AND v.interval = '5h' AND v.cut = 'A' ORDER BY v.start")?;
    let rows: Vec<(i64, f64, f64)> = st.query_map([tokens], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.flatten().collect();
    let n = rows.len();
    Ok((n, rows.into_iter().filter(|r| r.1 - r.2 < -2.0).collect()))
}

pub fn experimenter(ctx: &Ctx) -> Result<Vec<String>> {
    let mut out = vec![];
    // 1. where do the leads lose? (the ML gate and every candidate/validated variant), block by block against the baseline
    let leads: Vec<String> = {
        let mut st = ctx.mem.prepare("SELECT DISTINCT tokens FROM strategy_blocks WHERE interval = '5h' AND (tokens LIKE '%mlgate%' OR tokens IN (SELECT tokens FROM strategy_runs WHERE interval = '5h' AND cut = 'A' ORDER BY h2 DESC LIMIT 3)) AND tokens != 'baseline'")?;
        let v = st.query_map([], |r| r.get::<_, String>(0))?.flatten().collect();
        v
    };
    for t in leads.iter().take(3) {
        if recent(ctx.mem, "experimenter", "bisect", t, DAY)? { continue; }
        let (n, lost) = lost_blocks(ctx.mem, t)?;
        if n == 0 { continue; }
        let dates: Vec<String> = lost.iter().take(6).map(|l| crate::util::utc_minute(l.0)[..10].to_string()).collect();
        let line = format!("{t} lost to the baseline by more than 2 points in {} of {n} blocks (cut A){}", lost.len(), if dates.is_empty() { String::new() } else { format!(", starting {}", dates.join(", ")) });
        remember(ctx.mem, "experimenter", "bisect", t, &line, "strategy_blocks")?;
        out.push(line);
    }
    // 2. the standing experiments (24x7): one per turn, whatever the market is doing
    // the trainer runs these between genomes; this turn only does it when the trainer has been silent for half an hour (so the two never run the same experiment at once)
    let trainer_alive = std::fs::metadata(ctx.cfg.logs_dir().join("trainer.log")).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_secs() < 1800);
    if trainer_alive { out.push("standing experiments: the deep-learning trainer is running them".into()); } else { match crate::mlround::explore_next(&ctx.cfg.candles_db(), &ctx.cfg.data_db()) {
        Ok(l) => {
            if l.contains("ADOPTED") { crate::ops::discord("ML experiment ADOPTED (paired, multiple-testing adjusted)", &l); }
            remember(ctx.mem, "experimenter", "explore", &crate::util::utc_minute(crate::util::now_ts()), &l, "mlround::explore_next")?;
            out.push(l);
        }
        Err(e) => out.push(format!("explore failed: {e:#}")),
    } }
    Ok(out)
}

// ------------------------------------------------------------------ the daily digest
fn latest_fact(mem: &Connection, role: &str, kind: &str) -> Option<String> {
    mem.query_row("SELECT text FROM brain_facts WHERE role = ?1 AND kind = ?2 ORDER BY id DESC LIMIT 1", [role, kind], |r| r.get(0)).ok()
}

fn last_commit(repo: &str) -> String {
    std::process::Command::new("git").args(["-C", &crate::platform::projects_dir().join(repo).to_string_lossy(), "log", "-1", "--format=%h %s"]).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().chars().take(90).collect()).unwrap_or_default()
}

/// One short status for the owner's Discord every day, built only from the knowledge store and git (no new computation): the model and its recent out-of-sample IC,
/// the daily ML verdicts, the standing experiments, the data and the paper account, and each repo's latest commit. Returns the posted text.
pub fn digest(ctx: &Ctx) -> Result<Option<String>> {
    if recent(ctx.mem, "statistician", "digest", "daily", 22 * 3600)? { return Ok(None); }
    let get = |role: &str, kind: &str| latest_fact(ctx.mem, role, kind).unwrap_or_else(|| "not run yet".into());
    let clip = |s: String, n: usize| -> String { s.chars().take(n).collect() };
    let text = format!(
        "MODEL: {}\nRECENT IC: {}\nCANDLE VERDICT: {}\nIMPROVEMENT ROUND: {}\nLATEST EXPERIMENT: {}\nDATA: {}\nMODEL VERSIONS: {}\nLIVE RANKING: {}\nDECISION: {}\nLIVE SCOREBOARD: {}\nPAPER ACCOUNT: {}\nLEAD: {}\nREPOS: qwen-lab {} | bot {}",
        clip(get("steward", "model-card"), 230), clip(get("steward", "ic-reading"), 120), clip(get("statistician", "candle-ml"), 200), clip(get("statistician", "candle-improve"), 110),
        clip(get("experimenter", "explore"), 170), clip(get("data", "state"), 170), clip(get("steward", "briefing").lines().filter(|l| l.starts_with("STANCE") || l.starts_with("DECISION") || l.starts_with("RISK")).collect::<Vec<_>>().join(" | "), 420), clip(get("steward", "scoreboard"), 230), clip(get("risk", "state"), 170), clip(get("steward", "model-version"), 200), clip(get("steward", "live-ranking"), 170), clip(get("experimenter", "bisect"), 150),
        last_commit("qwen-lab"), last_commit("moomoo-bot"));
    remember(ctx.mem, "statistician", "digest", "daily", &text, "digest")?;
    crate::ops::discord("Daily digest: the local model and the ML", &text);
    Ok(Some("daily digest posted to Discord".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64) -> candles::Bar { candles::Bar { ts, open: 1.0, high: 1.0, low: 1.0, close: 1.0, volume: 1.0 } }

    #[test]
    fn the_switch_has_hysteresis_and_needs_enough_sessions() {
        assert_eq!(gate_decision(false, -0.05, 0.01, 9), None, "too few sessions");
        assert_eq!(gate_decision(false, -0.05, 0.01, 15), Some(true), "significantly negative: off");
        assert_eq!(gate_decision(false, -0.02, 0.02, 15), None, "negative but inside the noise: stays on");
        assert_eq!(gate_decision(true, -0.01, 0.01, 15), None, "still below zero: stays off");
        assert_eq!(gate_decision(true, 0.01, 0.02, 15), Some(false), "above zero: back on");
        assert_eq!(gate_decision(false, 0.05, 0.01, 15), None);
    }

    #[test]
    fn coverage_counts_the_bars_of_the_last_sessions() {
        let day = 86_400_000i64;
        let t0 = 1_775_000_000_000i64 / day * day + 14 * 3_600_000;
        let spy: Vec<candles::Bar> = (0..3).flat_map(|d| (0..26).map(move |i| bar(t0 + d * day + i * 900_000))).collect();
        let half: Vec<candles::Bar> = (0..3).flat_map(|d| (0..13).map(move |i| bar(t0 + d * day + i * 900_000))).collect();
        let mut m = BTreeMap::new();
        m.insert("SPY".to_string(), spy);
        m.insert("HALF".to_string(), half);
        assert_eq!(coverage(&m, "SPY", 3), Some(1.0));
        assert_eq!(coverage(&m, "HALF", 3), Some(0.5));
        assert_eq!(coverage(&m, "NONE", 3), None);
    }

    #[test]
    fn lost_blocks_compares_a_variant_with_the_baseline_block_by_block() {
        let mem = Connection::open_in_memory().unwrap();
        crate::strategy::init(&mem).unwrap();
        for (idx, v, b) in [(0, 3.0, 1.0), (1, -4.0, 1.0), (2, 0.5, 1.5), (3, -9.0, -2.0)] {
            for (tok, ret) in [("mlgate", v), ("baseline", b)] {
                mem.execute("INSERT INTO strategy_blocks(interval, tokens, equity, cut, idx, start, ret, trades, wins, ts, chain) VALUES ('5h', ?1, 5000, 'A', ?2, ?3, ?4, 5, 3, 0, 1)", rusqlite::params![tok, idx, 1_775_000_000 + idx * 86_400 * 20, ret]).unwrap();
            }
        }
        let (n, lost) = lost_blocks(&mem, "mlgate").unwrap();
        assert_eq!(n, 4);
        assert_eq!(lost.iter().map(|l| l.1).collect::<Vec<_>>(), vec![-4.0, -9.0], "lost by more than 2 points: blocks 1 (-5) and 3 (-7), not block 2 (-1)");
    }

    #[test]
    fn the_recent_ic_follows_a_planted_score_and_needs_enough_sessions() {
        // 30 symbols, 40 sessions; the score is the realised next-session excess return, so the IC must be high
        let day = 86_400_000i64;
        let t0 = 1_775_000_000_000i64 / day * day + 13 * 3_600_000 + 30 * 60_000;
        let mut st = 5u64;
        let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let mut series = BTreeMap::new();
        for s in 0..30 {
            let mut p = 50.0 + s as f64;
            let bars: Vec<candles::Bar> = (0..40 * 26).map(|i| { let o = p; p *= 1.0 + rnd() * 0.004; let d = i / 26; candles::Bar { ts: t0 + ((d / 5) * 7 + d % 5) as i64 * day + (i % 26) as i64 * 900_000, open: o, high: o.max(p) * 1.001, low: o.min(p) * 0.999, close: p, volume: 1000.0 } }).collect();
            series.insert(format!("S{s:02}"), bars);
        }
        let names: Vec<String> = series.keys().cloned().collect();
        let samples = crate::mlcandle::build(&series, 4);
        let mut table: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
        for s in &samples { table.entry(names[s.sym].clone()).or_default().push((s.ts, s.excess)); }
        let (mean, se, n) = recent_ic(&series, &table, 15).unwrap();
        assert!(mean > 0.95 && se < 0.05 && n == 15, "{mean} {se} {n}");
        let flipped: BTreeMap<String, Vec<(i64, f64)>> = table.iter().map(|(k, v)| (k.clone(), v.iter().map(|(a, b)| (*a, -b)).collect())).collect();
        let (m2, _, _) = recent_ic(&series, &flipped, 15).unwrap();
        assert!(m2 < -0.95);
        assert!(recent_ic(&series, &BTreeMap::new(), 15).is_none());
    }
}

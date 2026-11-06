//! The brain's team: twelve roles done by code and the small models in `brain.rs` and `brain_agents.rs` (owner 2026-10-13: a narrow Rust model for
//! this bot; no language model). Every role works through the lab's tools (`lab_queue_test`, `lab_note`, `lab_new_results`, `lab_record_idea`,
//! `lab_whiteboard_edit`, src/labtools.rs), as author `brain-<role>`, so the lab's validation, honesty gate, scoreboard and dashboards judge every
//! role by what code measures. Nothing a role writes contains a number it did not copy from a tool.
//!
//! | role | what it does |
//! |---|---|
//! | strategist | queues untested variants under rotating policies (`brain::cycle`), keeps the shared plan true, reads its inbox |
//! | tuner | queues tuned search ranges (`tune:margin`, `tune:dca`) from where the most profitable training runs set them (`strategy::tuning_ranges`) |
//! | researcher | rotates through market-research topics, searches the literature (guarded), records ideas mapped to the options that implement them |
//! | auditor | reviews new results for fragility / too few trades and notes the strategist |
//! | statistician | passes on the quant verdict, flags validated/candidate results whose gain is not consistent across blocks |
//! | execution | reads the bot's real paper decisions, reports what blocks entries (quoting the reason exactly) and queues one test that addresses it |
//!
//! `run_role` also writes a report to `brain/reports/` and a line to `logs/brain.log`; `serve` runs the roles in rotation as an always-on process.
//! Knowledge (every note, idea, finding, plan) is kept in the `brain_facts` table, which also prevents repeating a note within a day.

use crate::brain::PREFIX;
use crate::context::Ctx;
use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::io::Write;

pub const ROLES: [&str; 12] = ["strategist", "tuner", "researcher", "auditor", "execution", "statistician", "data", "steward", "drift", "risk", "promotion", "experimenter"];
pub(crate) const DAY: i64 = 86_400;

pub fn agent(role: &str) -> String { format!("{PREFIX}{role}") }

// ------------------------------------------------------------------ knowledge store (everything on disk)
pub fn init_store(mem: &Connection) -> Result<()> {
    mem.execute_batch("CREATE TABLE IF NOT EXISTS brain_facts (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, role TEXT NOT NULL, kind TEXT NOT NULL, subject TEXT NOT NULL, text TEXT NOT NULL, source TEXT);
        CREATE INDEX IF NOT EXISTS brain_facts_idx ON brain_facts(role, kind, subject, ts);")?;
    Ok(())
}

/// Has this role already recorded this kind of fact about this subject within the last `secs` seconds?
pub fn recent(mem: &Connection, role: &str, kind: &str, subject: &str, secs: i64) -> Result<bool> {
    Ok(mem.query_row("SELECT EXISTS (SELECT 1 FROM brain_facts WHERE role = ?1 AND kind = ?2 AND subject = ?3 AND ts > ?4)", params![role, kind, subject, crate::util::now_ts() - secs], |r| r.get(0))?)
}

pub fn remember(mem: &Connection, role: &str, kind: &str, subject: &str, text: &str, source: &str) -> Result<()> {
    mem.execute("INSERT INTO brain_facts(ts, role, kind, subject, text, source) VALUES (?1,?2,?3,?4,?5,?6)", params![crate::util::now_ts(), role, kind, subject, text.chars().take(600).collect::<String>(), source])?;
    Ok(())
}

/// Stored lessons and skills whose text contains all the given words (case-insensitive), newest first.
pub fn lessons(mem: &Connection, words: &[String], limit: usize) -> Result<Vec<String>> {
    init_store(mem)?;
    let mut sql = "SELECT role, kind, text FROM brain_facts WHERE kind IN ('lesson', 'skill')".to_string();
    for _ in words { sql.push_str(" AND lower(text) LIKE ?"); }
    sql.push_str(" ORDER BY id DESC LIMIT ");
    sql.push_str(&limit.to_string());
    let pats: Vec<String> = words.iter().map(|w| format!("%{}%", w.to_lowercase())).collect();
    let mut st = mem.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(pats.iter()), |r| Ok(format!("[{} {}] {}", r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?.replace('\n', " ").chars().take(260).collect::<String>())))?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn tool(ctx: &Ctx, name: &str, args: Value) -> Result<String> { crate::labtools::call(ctx, name, &args) }

/// Leaves a note through `lab_note` (at most once per `subject` per day). `plain` is the same finding without numbers, used when the honesty gate cannot
/// verify a number right now. Returns a line for the report, or None when it was a repeat.
pub(crate) fn note(ctx: &Ctx, from: &str, to: &str, subject: &str, text: &str, plain: &str) -> Result<Option<String>> {
    if recent(ctx.mem, from, "note", subject, DAY)? { return Ok(None); }
    let send = |t: &str| tool(ctx, "lab_note", json!({"to": agent(to), "text": t, "author": agent(from)}));
    let (sent, used) = match send(text) { Ok(_) => (true, text), Err(e) if format!("{e:#}").contains("REFUSED") || format!("{e:#}").contains("could not verify") => (send(plain).is_ok(), plain), Err(e) => return Ok(Some(format!("note to {to} failed: {e:#}"))) };
    if !sent { return Ok(Some(format!("note to {to} refused: {plain}"))); }
    remember(ctx.mem, from, "note", subject, used, "lab_note")?;
    Ok(Some(format!("note to {to}: {used}")))
}

// ------------------------------------------------------------------ parsers (tested on the lab's real tool output)
#[derive(Debug, Clone, PartialEq)]
pub struct NewResult { pub status: String, pub interval: String, pub key: String, pub training: f64, pub noisy: Option<f64>, pub win: Option<f64>, pub trades: f64 }

fn num_after(s: &str, marker: &str) -> Option<f64> {
    let rest = s.split(marker).nth(1)?;
    let t: String = rest.trim_start().chars().take_while(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.')).collect();
    t.parse().ok()
}

/// Parses `lab_new_results`: `  [status] 5h|key: training +2.31%/block, noisy +1.20%, win rate 46%, 9.0 trades/block`.
pub fn parse_new_results(text: &str) -> Vec<NewResult> {
    text.lines().filter_map(|l| {
        let l = l.trim();
        let (status, rest) = l.strip_prefix('[')?.split_once("] ")?;
        let (name, stats) = rest.split_once(": training ")?;
        let (interval, key) = name.split_once('|')?;
        let training = num_after(&format!("training {stats}"), "training ")?;
        let noisy = stats.split("noisy ").nth(1).and_then(|s| s.split(',').next()).and_then(|s| s.trim().trim_end_matches('%').parse().ok());
        let win = stats.split("win rate ").nth(1).and_then(|s| s.split(',').next()).and_then(|s| s.trim().strip_suffix('%')?.parse().ok());
        let trades = stats.rsplit(", ").next().and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok())?;
        Some(NewResult { status: status.to_string(), interval: interval.to_string(), key: key.to_string(), training, noisy, win, trades })
    }).collect()
}

/// Red flags the auditor reports for one result: (text with the lab's own numbers, the same without numbers).
pub fn red_flags(r: &NewResult) -> Vec<(String, String)> {
    let mut v = vec![];
    if let (true, Some(n)) = (r.training > 0.0, r.noisy.filter(|n| *n < 0.0)) {
        v.push((format!("{}|{}: training {:+.2}%/block but noisy {:+.2}%: fragile under jittered prices and 1.5x costs", r.interval, r.key, r.training, n),
                format!("{}|{}: positive on the clean tape but negative on the noisy tape: fragile", r.interval, r.key)));
    }
    if r.trades < 3.0 && r.status != "current strategy" {
        v.push((format!("{}|{}: only {:.1} trades per block: too few to measure an edge", r.interval, r.key, r.trades), format!("{}|{}: too few trades per block to measure an edge", r.interval, r.key)));
    }
    v
}

#[derive(Debug, Clone, PartialEq)]
pub struct Decision { pub head: String, pub funded: Option<(u32, u32)>, pub rejections: Vec<(String, String)> }

/// Parses `lab_paper_decisions`: `  2026-10-13: <reason> | funded 2 of 2 | gates {...} | ABBV rejected: why; ACN rejected: why`.
pub fn parse_decisions(text: &str) -> Vec<Decision> {
    text.lines().filter(|l| l.starts_with("  20")).map(|l| {
        let parts: Vec<&str> = l.trim().split(" | ").collect();
        let funded = parts.iter().find_map(|p| { let p = p.strip_prefix("funded ")?; let (a, b) = p.split_once(" of ")?; Some((a.trim().parse().ok()?, b.trim().parse().ok()?)) });
        let rejections = parts.last().filter(|p| p.contains(" rejected: ")).map(|p| p.split("; ").filter_map(|x| { let (sym, why) = x.split_once(" rejected: ")?; Some((sym.trim().to_string(), why.trim().to_string())) }).collect()).unwrap_or_default();
        Decision { head: parts.first().copied().unwrap_or("").to_string(), funded, rejections }
    }).collect()
}

/// The most common rejection reason in a decision and how many symbols it covers.
pub fn dominant_rejection(d: &Decision) -> Option<(String, usize)> {
    let mut counts: Vec<(String, usize)> = vec![];
    for (_, why) in &d.rejections { match counts.iter_mut().find(|c| &c.0 == why) { Some(c) => c.1 += 1, None => counts.push((why.clone(), 1)) } }
    counts.into_iter().max_by_key(|c| c.1)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Paper { pub title: String, pub published: String, pub url: String, pub summary: String }

/// Parses `lab_research_search`: `- Title (published) https://...` (withheld results are skipped).
pub fn parse_papers(text: &str) -> Vec<Paper> {
    let lines: Vec<&str> = text.lines().collect();
    lines.iter().enumerate().filter_map(|(i, l)| {
        let l = l.strip_prefix("- ")?;
        if l.starts_with("[WITHHELD") { return None; }
        let (head, url) = l.rsplit_once(' ')?;
        if !url.starts_with("http") { return None; }
        let head = head.trim_end();
        let (title, published) = match head.rsplit_once(" (") { Some((t, p)) if p.ends_with(')') => (t.trim(), p.trim_end_matches(')')), _ => (head, "") };
        // the summary is the indented lines under the entry, up to the next entry
        let summary = lines[i + 1..].iter().take_while(|n| !n.starts_with("- ")).map(|n| n.trim()).collect::<Vec<_>>().join(" ");
        Some(Paper { title: title.to_string(), published: published.to_string(), url: url.to_string(), summary })
    }).collect()
}

/// Does the paper itself mention the topic? (a search hit that never uses the topic's own terms is not evidence about the option)
pub fn mentions(p: &Paper, keywords: &[&str]) -> bool {
    let hay = format!("{} {}", p.title, p.summary).to_lowercase();
    keywords.iter().any(|k| hay.contains(&k.to_lowercase()))
}

/// Market-research topics: (search query, short name, options that implement it, words a paper must use to count as being about it).
pub const TOPICS: &[(&str, &str, &[&str], &[&str])] = &[
    ("Chaikin money flow accumulation distribution stock returns", "money flow", &["cmf"], &["money flow", "chaikin", "accumulation/distribution", "accumulation distribution"]),
    ("volume weighted average price trend stock returns", "volume-weighted price", &["vwap"], &["vwap", "volume-weighted", "volume weighted"]),
    ("on-balance volume predicts stock returns", "on-balance volume", &["obv"], &["on-balance volume", "on balance volume", "obv"]),
    ("Donchian channel breakout trading profitability stocks", "Donchian breakout", &["kind:donchian"], &["donchian", "channel breakout", "breakout"]),
    ("Kaufman adaptive moving average trading stocks", "adaptive moving average", &["kind:kama"], &["adaptive moving average", "kaufman", "kama"]),
    ("Hull moving average trading strategy stocks", "Hull moving average", &["kind:hma"], &["hull moving average", "moving average"]),
    ("market breadth filter momentum strategy equities", "market breadth", &["breadth"], &["market breadth", "breadth"]),
    ("volatility filter entry timing momentum equities", "volatility filter", &["gate:vol"], &["volatility filter", "volatility-managed", "volatility managed", "volatility timing"]),
    ("average true range volatility position sizing stocks", "average true range", &["gate:atr"], &["average true range", "atr"]),
    ("average directional index trend strength stock trading", "trend strength", &["adx"], &["directional index", "adx", "trend strength"]),
    ("Kaufman efficiency ratio trend noise stocks", "efficiency ratio", &["er"], &["efficiency ratio"]),
    ("crash protection momentum strategies drawdown equities", "crash protection", &["crash"], &["momentum crash", "crash"]),
    ("catching falling knives short term reversal stocks", "falling knives", &["knife"], &["falling knife", "short-term reversal", "short term reversal"]),
    ("averaging down position scaling stock trading evidence", "averaging down", &["dca:on"], &["averaging down", "dollar cost averaging", "dollar-cost averaging", "scaling in"]),
    ("cross-sectional momentum ranking stocks", "relative-strength ranking", &["grade:rank"], &["cross-sectional momentum", "relative strength", "momentum ranking"]),
    ("risk adjusted momentum ranking volatility scaled stocks", "risk-adjusted ranking", &["grade:risk"], &["risk-adjusted momentum", "risk adjusted momentum", "volatility-scaled", "sharpe momentum"]),
    ("market regime filter trend following index moving average", "market regime", &["gate:spyvol"], &["market regime", "regime", "trend following"]),
    ("momentum strategy universe size large cap stocks", "universe size", &["univ60"], &["universe", "large-cap", "large cap"]),
    ("transaction cost aware trading entry filter turnover", "transaction costs", &["feegate"], &["transaction cost", "turnover", "trading cost"]),
    ("pausing trading after losses drawdown control", "loss pause", &["losspause"], &["after losses", "loss streak", "drawdown control", "stop trading"]),
];

pub(crate) fn queue(ctx: &Ctx, role: &str, options: &[String], why: &str) -> (bool, String) {
    match tool(ctx, "lab_queue_test", json!({"interval": "5h", "options": options, "why": why, "author": agent(role)})) {
        Ok(t) => (t.starts_with("queued"), t.lines().next().unwrap_or("").to_string()),
        Err(e) => (false, format!("refused: {e:#}")),
    }
}

pub(crate) fn log_proposal(mem: &Connection, policy: &str, role: &str, key: &str, queued: bool, note: &str) -> Result<()> {
    crate::brain::init_tables(mem)?;
    mem.execute("INSERT INTO brain_proposals(ts, policy, author, interval, tokens, predicted, spread, queued, note) VALUES (?1,?2,?3,'5h',?4,0,0,?5,?6)", params![crate::util::now_ts(), policy, agent(role), key, queued as i64, note])?;
    Ok(())
}

// ------------------------------------------------------------------ the roles
/// The ML model's lead, acted on (owner 2026-10-13: "finish it all"): the machine-learning entry gate `mlgate` (its percentile searched by the bot's own
/// per-block search) and its pairing with training-half multiplicity survivors get tested through the same validated queue as every other variant, once each. Only when the
/// bot's score file exists; the lab's judging (cuts A-E, noisy tape, holdout) decides, not this rule.
fn ml_lead(ctx: &Ctx) -> Result<Vec<String>> {
    if !crate::platform::bot_scores_path().exists() { return Ok(vec![]); }
    let lb = crate::strategy::leaderboard(ctx.mem)?;
    let winners: Vec<String> = crate::quant::training_survivor_options(ctx.mem)?.into_iter().filter(|o| o != "mlgate").collect();
    let mut sets: Vec<Vec<String>> = vec![vec!["mlgate".to_string()]];
    for w in winners.iter().take(2) { sets.push(vec!["mlgate".to_string(), w.clone()]); }
    let mut out = vec![];
    for opts in sets {
        let key = crate::strategy::key(&opts);
        if lb.iter().any(|r| r["interval"] == "5h" && r["tokens"] == key.as_str()) || recent(ctx.mem, "strategist", "ml-lead", &key, 7 * DAY)? { continue; }
        let (queued, n) = queue(ctx, "strategist", &opts, "the candle ML model (ridge, regime interactions) is the lab's best lead; its entry gate skips stocks it scores low, the percentile searched per block");
        if !queued && n.contains("open requests") { break; } // the queue is full: try again at the next run, nothing is recorded
        log_proposal(ctx.mem, "ml-lead", "strategist", &key, queued, &n)?;
        remember(ctx.mem, "strategist", "ml-lead", &key, &format!("{key}: {n}"), "ml_lead")?;
        out.push(format!("ml-lead: 5h|{key} -> {n}"));
        if !queued { break; }
    }
    Ok(out)
}

pub fn strategist(ctx: &Ctx) -> Result<Vec<String>> {
    let mut out = match crate::online_brain::run(ctx) {
        Ok(lines) => lines,
        Err(e) => vec![format!(
            "Online ensemble unavailable; deterministic strategist continued: {e:#}"
        )],
    };
    out.extend(crate::brain::cycle(ctx, 1)?);
    out.extend(ml_lead(ctx)?);
    // inbox: the team's notes are kept in the knowledge store (they inform the plan below)
    if let Ok(inbox) = tool(ctx, "lab_inbox", json!({"agent": agent("strategist")})) {
        for l in inbox.lines().filter(|l| l.trim_start().starts_with("from ")) { remember(ctx.mem, "strategist", "inbox", l.trim(), l.trim(), "lab_inbox")?; }
    }
    // the shared plan, from code-checked facts (no numbers)
    let next: Vec<String> = crate::labtools::untested(ctx.mem, "5h", 3).unwrap_or_default();
    let mut items = vec![];
    if !next.is_empty() { items.push(format!("Test next: {}", next.join(", "))); }
    items.push("Re-check the tuned entry-margin and DCA bands whenever the tuning facts change".to_string());
    items.push("Every validated or candidate result gets a statistician consistency check before any promotion".to_string());
    let subject = items.join("|");
    if !recent(ctx.mem, "strategist", "plan", &subject, 6 * 3600)? {
        match tool(ctx, "lab_whiteboard_edit", json!({"op": "set_plan", "items": items, "author": agent("strategist")})) {
            Ok(t) => { remember(ctx.mem, "strategist", "plan", &subject, &items.join("; "), "lab_whiteboard_edit")?; out.push(format!("plan: {t}")); }
            Err(e) => out.push(format!("plan not set: {e:#}")),
        }
    }
    Ok(out)
}

pub fn tuner(ctx: &Ctx) -> Result<Vec<String>> {
    let cap = ctx.cfg.strategy.equities.first().copied().unwrap_or(5000.0);
    let ranges = crate::strategy::tuning_ranges(ctx.data, cap)?;
    if ranges.is_empty() { return Ok(vec!["no tuned range: the most profitable runs do not set the margin or the DCA depth differently from all runs".into()]); }
    let mut out = vec![];
    for (what, lo, hi) in ranges.into_iter().take(2) {
        let tok = format!("tune:{what}={lo:.2}-{hi:.2}");
        if crate::strategy::tune_token(&tok).is_none() { continue; }
        let key = crate::strategy::key(std::slice::from_ref(&tok));
        let why = format!("tuned range: the most profitable training runs set the {} in a band that differs clearly from all runs (tuning facts, training blocks only)", if what == "margin" { "entry trend margin" } else { "DCA depth" });
        let (queued, note) = queue(ctx, "tuner", &[tok], &why);
        log_proposal(ctx.mem, "tuning-band", "tuner", &key, queued, &note)?;
        out.push(format!("tuning-band: 5h|{key} -> {note}"));
    }
    Ok(out)
}

pub fn researcher(ctx: &Ctx) -> Result<Vec<String>> {
    // the topic searched longest ago (never searched first)
    let last = |q: &str| -> i64 { ctx.mem.query_row("SELECT coalesce(max(ts), 0) FROM brain_facts WHERE role = 'researcher' AND kind = 'topic' AND subject = ?1", [q], |r| r.get(0)).unwrap_or(0) };
    let (query, name, options, keywords) = TOPICS.iter().min_by_key(|t| last(t.0)).copied().ok_or_else(|| anyhow!("no topics"))?;
    remember(ctx.mem, "researcher", "topic", query, name, "lab_research_search")?;
    let found = tool(ctx, "lab_research_search", json!({"query": query, "max": 5}))?;
    crate::scout::init(ctx.mem)?;
    let mut out = vec![format!("topic: {name} ({query})")];
    let opts: Vec<String> = options.iter().map(|s| s.to_string()).collect();
    let (mut recorded, mut skipped) = (0, 0);
    for p in parse_papers(&found) {
        if recorded >= 3 { break; }
        if !mentions(&p, keywords) { skipped += 1; continue; }
        let known: bool = ctx.mem.query_row("SELECT EXISTS (SELECT 1 FROM research_ideas WHERE url = ?1)", [&p.url], |r| r.get(0)).unwrap_or(false);
        if known { continue; }
        let idea = format!("This paper discusses {name}; test the option(s) {} that implement it on the bot's entries.", opts.join(" and "));
        match tool(ctx, "lab_record_idea", json!({"url": p.url, "title": p.title, "published": p.published, "idea": idea, "options": opts, "author": agent("researcher")})) {
            Ok(t) => { recorded += 1; remember(ctx.mem, "researcher", "idea", &p.url, &format!("{} -> {}", p.title, opts.join("+")), "lab_record_idea")?; out.push(format!("idea: {} -> {} ({t})", p.title.chars().take(80).collect::<String>(), opts.join("+"))); }
            Err(e) => out.push(format!("idea skipped ({}): {e:#}", p.title.chars().take(60).collect::<String>())),
        }
    }
    if recorded == 0 { out.push(format!("no new paper that actually discusses {name} this time ({skipped} search hits did not mention it)")); }
    Ok(out)
}

pub fn auditor(ctx: &Ctx) -> Result<Vec<String>> {
    let mut out = vec![];
    let text = tool(ctx, "lab_new_results", json!({"agent": agent("auditor")}))?;
    let mut sent = 0;
    for r in parse_new_results(&text) {
        for (full, plain) in red_flags(&r) {
            if sent >= 2 { break; }
            if let Some(l) = note(ctx, "auditor", "strategist", &format!("{}|{}|{}", r.interval, r.key, plain.chars().skip(r.key.len() + 4).take(24).collect::<String>()), &full, &plain)? { out.push(l); sent += 1; }
        }
    }
    // (highlight explanations stay as the lab's own code writes them, src/highlights.rs)
    if out.is_empty() { out.push("nothing new to audit".into()); }
    Ok(out)
}

/// The pre-registered machine-learning test on the bot's real trades (src/mltrade.rs), at most once a day; Discord hears about it the first time and
/// whenever the verdict changes.
pub fn ml_watch(ctx: &Ctx) -> Result<Option<String>> {
    if recent(ctx.mem, "statistician", "ml-watch", "daily", DAY)? { return Ok(None); }
    let trades = crate::mltrade::load(ctx.data)?;
    let Some(w) = crate::mltrade::watch(&trades, 6) else { return Ok(Some("ML watch: too few stored trades to judge".into())) };
    let summary = w.summary();
    let before: Option<String> = ctx.mem.query_row("SELECT text FROM brain_facts WHERE role = 'statistician' AND kind = 'ml-watch' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
    remember(ctx.mem, "statistician", "ml-watch", "daily", &summary, "mltrade::watch")?;
    let changed = before.as_deref().map(|b| b.contains("QUALIFIES") != w.qualifies);
    if changed.unwrap_or(true) { crate::ops::discord("ML for trading: pre-registered test", &summary); }
    Ok(Some(format!("ML watch: {summary}")))
}

/// Daily pre-registered test on the market's own candles (`mlcandle::report`): refreshes the lab's candle store, runs the fixed test, and tells Discord the
/// first time and whenever the verdict flips. The full report is written to logs/candle-ml.txt; the knowledge store keeps a one-line summary.
pub fn candle_watch(ctx: &Ctx) -> Result<Option<String>> {
    if recent(ctx.mem, "statistician", "candle-ml", "daily", DAY)? { return Ok(None); }
    let db = rusqlite::Connection::open(ctx.cfg.candles_db())?;
    let _ = crate::candles::import(Some(&crate::platform::bot_history_json()), Some(&crate::platform::bot_store_path()), &db);
    let _ = crate::candles::import_universe(&crate::platform::bot_holdings_csv(), &db);
    // the scores the bot's own backtester reads (`sig:Ml`, `mlgate*`): refreshed daily so new bars are scored
    let _ = crate::mlscores::write(&crate::candles::load_prepared(&db)?, &crate::platform::bot_scores_path(), crate::mlscores::Scheme::from_ledger(&db));
    let v = crate::mlcandle::report(&crate::candles::load_prepared(&db)?, &crate::candles::load_costs(&db));
    let _ = std::fs::write(ctx.cfg.root.join("logs").join("candle-ml.txt"), &v.text);
    let head = v.text.lines().next().unwrap_or("").to_string();
    let ridge = v.text.lines().find(|l| l.trim_start().starts_with("procedure (params re-chosen")).unwrap_or("").trim().to_string();
    let verdict = v.text.lines().find(|l| l.starts_with("VERDICT")).unwrap_or("").trim_start_matches("VERDICT: ").to_string();
    let kind = if v.qualifies { "QUALIFIES" } else if verdict.starts_with("LEAD") { "LEAD" } else { "does not qualify" };
    let summary = format!("{kind} | {head} | {ridge}");
    let before: Option<String> = ctx.mem.query_row("SELECT text FROM brain_facts WHERE role = 'statistician' AND kind = 'candle-ml' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
    remember(ctx.mem, "statistician", "candle-ml", "daily", &summary, "mlcandle::report")?;
    let changed = before.as_deref().map(|b| b.split(' ').next() != summary.split(' ').next());
    if kind != "does not qualify" && changed.unwrap_or(true) {
        let t = format!("Candle ML verdict is now {kind}: test the entry gate mlgate and its pairings with validated options");
        let _ = note(ctx, "statistician", "strategist", &format!("ml-verdict:{kind}"), &t, &t);
    }
    if changed.unwrap_or(true) { crate::ops::discord("ML on all candles (95 symbols): pre-registered test", &summary); }
    Ok(Some(format!("Candle ML: {summary}")))
}

/// The daily improvement round on the candle model (`mlround::run`): recency schemes, extra features and meta-labelling against the trial ledger. The
/// report goes to logs/candle-improve.txt; Discord hears only when something is ADOPTED (paired difference above zero at the multiple-testing-adjusted level).
pub fn candle_improve(ctx: &Ctx) -> Result<Option<String>> {
    let newest: i64 = rusqlite::Connection::open(ctx.cfg.candles_db())?.query_row("SELECT coalesce(max(ts_ms), 0) FROM candles15", [], |r| r.get(0))?;
    let through: Option<String> = ctx.mem.query_row("SELECT text FROM brain_facts WHERE role = 'statistician' AND kind = 'improve-through' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
    if through.as_deref().and_then(|t| t.parse::<i64>().ok()) == Some(newest) { return Ok(None); } // learns whenever new bars arrive, day or night
    let r = crate::mlround::run(&ctx.cfg.candles_db(), &ctx.cfg.data_db())?;
    remember(ctx.mem, "statistician", "improve-through", "t", &newest.to_string(), "candles15")?;
    let _ = std::fs::write(ctx.cfg.root.join("logs").join("candle-improve.txt"), &r.text);
    let summary = format!("scheme {} | adopted features {:?} | ledger trials {}", r.adopted_scheme, r.adopted_features, r.text.lines().last().and_then(|l| l.rsplit(' ').next()).unwrap_or("?"));
    let before: Option<String> = ctx.mem.query_row("SELECT text FROM brain_facts WHERE role = 'statistician' AND kind = 'candle-improve' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
    remember(ctx.mem, "statistician", "candle-improve", "daily", &summary, "mlround::run")?;
    let adopted = r.adopted_scheme != "expanding" || !r.adopted_features.is_empty();
    if adopted && before.as_deref().map(|b| b.split(" | ledger").next() != summary.split(" | ledger").next()).unwrap_or(true) {
        crate::ops::discord("ML improvement ADOPTED (paired, multiple-testing adjusted)", &summary);
    }
    Ok(Some(format!("Candle improve: {summary}")))
}

pub fn statistician(ctx: &Ctx) -> Result<Vec<String>> {
    let mut out = vec![];
    if let Some(l) = ml_watch(ctx)? { out.push(l); }
    if let Some(l) = candle_watch(ctx)? { out.push(l); }
    if let Some(l) = candle_improve(ctx)? { out.push(l); }
    if let Some(l) = crate::brain_agents::digest(ctx)? { out.push(l); }
    // the quant review's verdict, passed on whenever it changes
    if let Ok(q) = tool(ctx, "lab_quant", json!({})) {
        if let Some(v) = q.lines().map(str::trim).find(|l| l.contains("VERDICT")) {
            let subject = crate::util::sha256_str(v);
            if let Some(l) = note(ctx, "statistician", "strategist", &subject, &format!("Quant review: {v}").chars().take(290).collect::<String>(), "Quant review verdict changed: read lab_quant before queuing more tests of the same kind")? { out.push(l); }
        }
    }
    // consistency of the results that passed: one lucky block is not an edge
    let text = tool(ctx, "lab_new_results", json!({"agent": agent("statistician")}))?;
    let lb = crate::strategy::leaderboard(ctx.mem)?;
    for r in parse_new_results(&text).into_iter().filter(|r| r.status == "validated" || r.status == "candidate") {
        let row = lb.iter().find(|l| l["interval"] == r.interval.as_str() && l["tokens"] == r.key.as_str());
        let (pos, worst) = row.map_or((None, None), |l| (l["a"]["pos"].as_f64(), l["a"]["worst"].as_f64()));
        crate::ops::discord(&format!("Brain: a variant passed the lab's verdict ({})", r.status), &format!("{}|{} is now {}", r.interval, r.key, r.status));
        let mut flags = vec![];
        if pos.is_some_and(|p| p < 55.0) { flags.push("fewer than 55% of its blocks are positive: the gain may come from a few lucky blocks"); }
        if worst.is_some_and(|w| w < -8.0) { flags.push("its worst block lost more than 8%"); }
        for f in flags { if let Some(l) = note(ctx, "statistician", "strategist", &format!("{}|{}|{}", r.interval, r.key, &f[..12]), &format!("{}|{} ({}): {f}", r.interval, r.key, r.status), &format!("{}|{}: {f}", r.interval, r.key))? { out.push(l); } }
    }
    if out.is_empty() { out.push("no change in the quant verdict and no new passing result to check".into()); }
    Ok(out)
}

pub fn execution(ctx: &Ctx) -> Result<Vec<String>> {
    let text = tool(ctx, "lab_paper_decisions", json!({"n": 12}))?;
    let ds = parse_decisions(&text);
    let Some(latest) = ds.first() else { return Ok(vec!["no paper decisions yet".into()]) };
    let mut out = vec![];
    if let Some((why, n)) = dominant_rejection(latest) {
        let syms: Vec<&str> = latest.rejections.iter().filter(|r| r.1 == why).map(|r| r.0.as_str()).collect();
        let subject = format!("rejection:{why}");
        // the tool lists only a few rejected symbols per decision, so no count is claimed: the symbols it shows are named instead
        let full = format!("Bot execution: the rejected symbols the bot listed at its latest decision ({}) share one reason: {why}", syms.join(", "));
        let plain = full.clone();
        let _ = n;
        if let Some(l) = note(ctx, "execution", "strategist", &subject, &full.chars().take(290).collect::<String>(), &plain.chars().take(290).collect::<String>())? {
            remember(ctx.mem, "execution", "finding", &subject, &full, "lab_paper_decisions")?;
            out.push(l);
        }
    }
    // the ML model's view of the names the bot rejected, from the score file the bot's own backtester reads
    {
        let names: Vec<String> = latest.rejections.iter().map(|r| r.0.clone()).collect();
        if let Some((ts, view)) = crate::mlscores::latest_view(&crate::platform::bot_scores_path(), &names) {
            if !view.is_empty() {
                let top: Vec<&str> = view.iter().filter(|v| v.1 >= 50.0).map(|v| v.0.as_str()).collect();
                let bottom: Vec<&str> = view.iter().filter(|v| v.1 < 50.0).map(|v| v.0.as_str()).collect();
                let line = format!("ML view at the newest scored bar ({}): of the names the bot rejected, the model rates {} in its top half and {} in its bottom half", crate::util::utc_minute(ts / 1000), if top.is_empty() { "none".into() } else { top.join(", ") }, if bottom.is_empty() { "none".into() } else { bottom.join(", ") });
                remember(ctx.mem, "execution", "ml-view", &line, &line, "mlscores::latest_view")?;
                out.push(line);
            }
        }
    }
    // entries blocked by the regime filter on most recent decisions: the matching option is worth one test
    let blocked = ds.iter().filter(|d| d.head.contains("regime filter") || d.funded == Some((0, d.funded.map_or(0, |f| f.1)))).count();
    if !ds.is_empty() && blocked * 2 >= ds.len() {
        let (queued, n) = queue(ctx, "execution", &["entrywide".to_string()], "most recent paper decisions funded nothing because the market regime filter blocked entries; test the wider entry band");
        log_proposal(ctx.mem, "execution-blocker", "execution", "entrywide", queued, &n)?;
        out.push(format!("execution-blocker: 5h|entrywide -> {n}"));
    }
    if out.is_empty() { out.push("paper decisions look consistent with the backtests: nothing to report".into()); }
    Ok(out)
}

// ------------------------------------------------------------------ running roles
fn stamp() -> String { let t = crate::util::utc_minute(crate::util::now_ts()); format!("{}{}{}-{}{}", &t[0..4], &t[5..7], &t[8..10], &t[11..13], &t[14..16]) }

/// Runs one role, writes its report (`brain/reports/<stamp>-brain-<role>.md`) and a line to `logs/brain.log`; a failing role is reported, never fatal.
pub fn run_role(ctx: &Ctx, role: &str) -> Vec<String> {
    let started = std::time::Instant::now();
    let _ = init_store(ctx.mem);
    let res = match role {
        "strategist" => strategist(ctx), "tuner" => tuner(ctx), "researcher" => researcher(ctx), "auditor" => auditor(ctx),
        "execution" => execution(ctx), "statistician" => statistician(ctx),
        r if crate::brain_agents::AGENT_ROLES.contains(&r) => crate::brain_agents::run(ctx, r), other => Err(anyhow!("unknown role {other}")),
    };
    let (ok, lines) = match res { Ok(l) => (true, l), Err(e) => (false, vec![format!("FAILED: {e:#}")]) };
    let secs = started.elapsed().as_secs();
    let requests = lines.iter().filter(|l| l.contains("-> queued")).count();
    let notes = lines.iter().filter(|l| l.starts_with("note to")).count();
    let report = format!("# Brain {} — {} (exit {}, {secs}s)\n\n## Verified by code (what was actually recorded during this run)\n- test requests queued: {requests};\n- notes sent: {notes};\n- strategist uses the checked online ensemble with persistent SQLite memory; every action remains tool-validated\n\n## What it did\n\n{}\n",
        agent(role), crate::util::utc_minute(crate::util::now_ts()), if ok { 0 } else { 1 }, lines.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n"));
    let dir = ctx.cfg.root.join("brain/reports");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{}-brain-{role}.md", stamp())), report);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(ctx.cfg.root.join("logs/brain.log")) {
        let _ = writeln!(f, "{} {role} {} in {secs}s: {}", crate::util::utc_minute(crate::util::now_ts()), if ok { "ok" } else { "FAILED" }, lines.join(" | ").chars().take(400).collect::<String>());
    }
    if !ok { crate::ops::discord(&format!("Brain role {role} failed"), &lines.join(" ").chars().take(300).collect::<String>()); }
    lines
}

/// The next role in rotation (state in the `counters` table, so a restart continues where it stopped).
pub fn next_role(mem: &Connection) -> Result<&'static str> {
    let i: i64 = mem.query_row("SELECT coalesce((SELECT value FROM counters WHERE name = 'brain_next_role'), 0)", [], |r| r.get(0))?;
    mem.execute("INSERT INTO counters(name, value) VALUES ('brain_next_role', ?1) ON CONFLICT(name) DO UPDATE SET value = excluded.value", [(i + 1) % ROLES.len() as i64])?;
    Ok(ROLES[(i as usize) % ROLES.len()])
}

/// Always-on process: one role every `every_secs`, in rotation, so each role runs about every `6 * every_secs`.
pub fn serve(ctx: &Ctx, every_secs: u64) -> ! {
    loop {
        let stop = ctx.cfg.stop_file().exists() || ctx.cfg.root.join("MAINTENANCE").exists() || ctx.cfg.root.join("brain/OFF").exists();
        if !stop {
            if let Ok(role) = next_role(ctx.mem) { let _ = run_role(ctx, role); }
        }
        std::thread::sleep(std::time::Duration::from_secs(every_secs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the fixtures are the lab's real tool output (copied from a live run), not invented text
    const PAPER_DECISIONS: &str = "BOT PAPER DECISIONS (newest 6 of its real paper trading; forward data):\n  2026-10-13: 2 funded per side | funded 2 of 2 | gates {\"entries_allowed\":true,\"stopped_out\":false,\"trend_ok\":true} | ABBV rejected: no signal value (not enough history or data); ACN rejected: no signal value (not enough history or data); AMGN rejected: other reason\n  2026-10-13: regime filter: SPY below its 24h average, no entries | funded 0 of 2 | gates {\"entries_allowed\":true,\"stopped_out\":false,\"trend_ok\":false} | \n  2026-10-10: regime filter: SPY below its 24h average, no entries | funded 0 of 2 | gates {\"entries_allowed\":true,\"stopped_out\":false,\"trend_ok\":false} | \nCLOSED PAPER TRADES: none yet";

    #[test]
    fn paper_decisions_are_parsed_and_the_dominant_rejection_found() {
        let ds = parse_decisions(PAPER_DECISIONS);
        assert_eq!(ds.len(), 3);
        assert_eq!(ds[0].funded, Some((2, 2)));
        assert_eq!(ds[0].rejections.len(), 3);
        assert_eq!(dominant_rejection(&ds[0]), Some(("no signal value (not enough history or data)".to_string(), 2)));
        assert_eq!(ds[1].funded, Some((0, 2)));
        assert!(ds[1].head.contains("regime filter") && ds[1].rejections.is_empty());
    }

    #[test]
    fn new_results_are_parsed_and_flags_follow_the_numbers() {
        let text = "  [validated] 5h|obv: training +2.31%/block, noisy +1.20%, win rate 46%, 9.0 trades/block\n  [failed-holdout] 5h|gate:vol: training +1.10%/block, noisy -0.40%, win rate not recorded (tested before win rates were recorded), 2.5 trades/block\n  [current strategy] 5h|baseline: training +2.14%/block, noisy ?, win rate 46%, 9.0 trades/block";
        let r = parse_new_results(text);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], NewResult { status: "validated".into(), interval: "5h".into(), key: "obv".into(), training: 2.31, noisy: Some(1.2), win: Some(46.0), trades: 9.0 });
        assert_eq!((r[1].noisy, r[1].win, r[1].trades), (Some(-0.4), None, 2.5));
        assert_eq!(r[2].noisy, None);
        assert!(red_flags(&r[0]).is_empty() && red_flags(&r[2]).is_empty());
        let f = red_flags(&r[1]);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].0, "5h|gate:vol: training +1.10%/block but noisy -0.40%: fragile under jittered prices and 1.5x costs");
        assert_eq!(f[1].0, "5h|gate:vol: only 2.5 trades per block: too few to measure an edge");
        assert_eq!(f[1].1, "5h|gate:vol: too few trades per block to measure an edge");
    }

    #[test]
    fn papers_are_parsed_from_search_output_and_withheld_ones_skipped() {
        let body = "[UNTRUSTED]\n- Volume and returns in US stocks (2024-05-01) https://arxiv.org/abs/2405.00001v1\n  summary text here\n- [WITHHELD by the security guard: injection] https://arxiv.org/abs/2405.00002\n- A title without a date https://doi.org/10.1/abc\n";
        let p = parse_papers(body);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0], Paper { title: "Volume and returns in US stocks".into(), published: "2024-05-01".into(), url: "https://arxiv.org/abs/2405.00001v1".into(), summary: "summary text here".into() });
        assert!(mentions(&p[0], &["volume"]) && !mentions(&p[0], &["chaikin"]), "a paper counts only if it uses the topic's own terms");
        assert_eq!(p[1].published, "");
    }

    #[test]
    fn every_research_topic_maps_to_valid_options() {
        for (q, name, opts, kws) in TOPICS {
            assert!(!q.is_empty() && !name.is_empty() && !kws.is_empty(), "{name}");
            let v: Vec<String> = opts.iter().map(|s| s.to_string()).collect();
            assert!(crate::strategy::why_invalid(&v).is_none(), "{name}: {opts:?}");
        }
        let names: std::collections::BTreeSet<&str> = TOPICS.iter().map(|t| t.0).collect();
        assert_eq!(names.len(), TOPICS.len(), "topics must be distinct");
    }

    #[test]
    fn stored_lessons_are_searchable() {
        let m = Connection::open_in_memory().unwrap();
        init_store(&m).unwrap();
        remember(&m, "strategist", "lesson", "a", "5h|obv: validated, training on the earlier blocks", "test").unwrap();
        remember(&m, "strategist", "skill", "own-skill", "# own skill\nHow to read a luck audit", "test").unwrap();
        assert_eq!(lessons(&m, &["luck".into()], 5).unwrap().len(), 1);
        assert_eq!(lessons(&m, &["OBV".into(), "validated".into()], 5).unwrap().len(), 1, "case-insensitive, all words must match");
        assert!(lessons(&m, &["absent".into()], 5).unwrap().is_empty());
    }

    #[test]
    fn the_store_prevents_repeats_and_roles_rotate() {
        let m = Connection::open_in_memory().unwrap();
        init_store(&m).unwrap();
        m.execute_batch("CREATE TABLE counters (name TEXT PRIMARY KEY, value INTEGER NOT NULL);").unwrap();
        assert!(!recent(&m, "auditor", "note", "x", DAY).unwrap());
        remember(&m, "auditor", "note", "x", "text", "t").unwrap();
        assert!(recent(&m, "auditor", "note", "x", DAY).unwrap());
        assert!(!recent(&m, "auditor", "note", "y", DAY).unwrap() && !recent(&m, "tuner", "note", "x", DAY).unwrap());
        let seq: Vec<&str> = (0..13).map(|_| next_role(&m).unwrap()).collect();
        assert_eq!(seq, vec!["strategist", "tuner", "researcher", "auditor", "execution", "statistician", "data", "steward", "drift", "risk", "promotion", "experimenter", "strategist"], "twelve roles take turns, the six that act included");
    }

    #[test]
    fn the_tuning_band_is_only_proposed_when_top_runs_differ_clearly() {
        let all: Vec<f64> = (0..100).map(|i| i as f64 / 10.0).collect();          // spread 0..10
        let same: Vec<f64> = (0..40).map(|i| 2.5 + (i % 10) as f64 * 0.5).collect(); // median near all-runs median
        assert_eq!(crate::strategy::band_if_different(&same, &all, 10.0), None);
        let shifted: Vec<f64> = (0..40).map(|i| 6.0 + (i % 10) as f64 * 0.2).collect();
        let (lo, hi) = crate::strategy::band_if_different(&shifted, &all, 10.0).unwrap();
        assert!(lo >= 6.0 && hi <= 7.8 && lo < hi, "{lo} {hi}");
        assert_eq!(crate::strategy::band_if_different(&shifted[..5], &all, 10.0), None, "too few top runs");
    }
}

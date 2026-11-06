//! Warnings ledger (Rust port of the bot's ops/warnings.py). Owner, 2026-10-11: "make sure all warnings are captured and fixed and marked as
//! fixed"; "i want thorough logging"; "i want claude cli to check these logs and fix".
//!
//! Every log of the bot and the lab is scanned incrementally, and both dashboards (lab :8790, bot console :8789) are checked
//! the way a viewer sees them (leaked bug text, failed tick steps, health, not answering). Logs are read from byte offsets kept in the database,
//! so nothing is read twice or skipped. Each warning or error line is reduced to a signature (time stamps, numbers, ids and ticker symbols
//! removed), so one recurring problem is one entry with a count, first and last seen. Silent jobs are warnings too: a log that should be written
//! on a schedule and is not ("STALE"). An entry stays open until it is marked:
//!   fixed    <id> --commit <sha> --note "...": the commit must exist in the bot or lab repo (checked). If the same warning appears again more than
//!            GRACE after the fix, code REOPENS it (the fix did not hold) and counts the reopen.
//!   expected <id> --note "...": a known, harmless condition; a reason is required.
//! The ledger is SQLite on disk (<bot state folder>/warnings.sqlite3); `report` writes docs/WARNINGS.md; account-like
//! numbers and secrets are redacted. The database schema and the signature rules are unchanged from the Python version, so the existing ledger
//! keeps working.
//!
//! usage: qwen-tools warnings scan | list [--all] | show <id> | fixed <id> --commit <sha> --note <text> | expected <id> --note <text> | report <out.md> | counts

use crate::misc_tools::{py_repr, py_str_pub};
use anyhow::{anyhow, bail, Result};
use chrono::{Local, NaiveDateTime, TimeZone};
use fancy_regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// A warning seen within 15 min after its fix may still be the old run's output.
pub const GRACE: i64 = 15 * 60;

/// Compiled once, with a generous backtracking limit (the default limit made the lookahead patterns fail on very long log lines).
fn build(pat: &str) -> Regex { fancy_regex::RegexBuilder::new(pat).backtrack_limit(500_000_000).build().expect("valid regex") }
fn rx(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| build(pat)) }
macro_rules! re { ($pat:expr) => {{ static C: OnceLock<Regex> = OnceLock::new(); rx(&C, $pat) }}; }

fn hit() -> &'static Regex { re!(r"\b(WARN|WARNING|ERROR|Error|error|FAULT|fault|panicked|panic|Traceback|Exception|exception|syntax error|FAILED|failed|refused|REFUSED|timed out|timeout|cannot|could not|unable to|denied|not reachable|QUARANTINED|rc=(?!0\b)\d+|exit (?!0\b)\d+)\b") }
fn not() -> &'static Regex { re!(r#"\b0 (failed|wrong|errors?)\b|failed: 0|\bfailed-(holdout|alt-cut|noisy|win|trades)\b|failed the full validation|\(standard error \d|-> refused: you already have \d+ open requests|failed on (unseen|cut)|HIGHLIGHT|status (failed|validated)|verdict|\[ok\] promote|0 new claim checks|refused (request|retire_belief|add_belief)|whiteboard: refused|no Hermes run since|^\s*(ok|wrong) hermes-|critic .*noise_fragile|the critic's text failed|passes \d+ of|memory fix for hermes-\w+ refused: entry not found verbatim|uvicorn\.error - INFO|^\s{6,}\S*(\+|%[0-9A-F]{2})\S*$"#) }
fn errorish() -> &'static Regex { re!(r"ERROR|Error|FAULT|panick|Traceback|syntax error|\[failed\]|PUSH FAILED|not reachable|rc=(?!0\b)\d+|exit (?!0\b)\d+") }
fn ts_re() -> &'static Regex { re!(r"(\d{4}-\d{2}-\d{2})[T ](\d{2}:\d{2}(?::\d{2})?)") }
fn dash_bad() -> &'static Regex { re!(r"unrecognised|unrecognized|Invalid column type|panicked|Traceback|\bNaN\b|\bundefined\b|\[object Object\]|error: |Error: |cannot reach|not reachable") }

struct Paths { home: PathBuf, rt: PathBuf, lab: PathBuf, bot: PathBuf, labrepo: PathBuf }
fn paths() -> Paths {
    let home = crate::platform::home();
    Paths { rt: crate::platform::bot_state_dir(), lab: crate::platform::lab_root(),
        bot: std::env::var("QWEN_BOT_ROOT").map(PathBuf::from).unwrap_or_else(|_| crate::platform::bot_repo()), labrepo: crate::platform::lab_repo(), home }
}

fn con(p: &Paths) -> Result<Connection> {
    std::fs::create_dir_all(&p.rt)?;
    let c = Connection::open(p.rt.join("warnings.sqlite3"))?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    c.execute_batch("
    CREATE TABLE IF NOT EXISTS offsets (path TEXT PRIMARY KEY, inode INTEGER, pos INTEGER);
    CREATE TABLE IF NOT EXISTS warnings (id INTEGER PRIMARY KEY, sig TEXT UNIQUE, source TEXT, log TEXT, severity TEXT,
        sample TEXT, count INTEGER, first_seen INTEGER, last_seen INTEGER, status TEXT DEFAULT 'open', commit_id TEXT,
        note TEXT, fixed_at INTEGER, reopened INTEGER DEFAULT 0);
    CREATE TABLE IF NOT EXISTS history (id INTEGER PRIMARY KEY, ts INTEGER, warning INTEGER, event TEXT, detail TEXT);
    ")?;
    Ok(c)
}

fn clip(s: &str, n: usize) -> String { s.chars().take(n).collect() }
fn sub(pat: &str, rep: &str, s: &str) -> String { build(pat).replace_all(s, rep).into_owned() }

/// One recurring problem = one signature: time stamps, numbers, ids, temp paths and ticker symbols removed.
pub fn signature(line: &str) -> String {
    let s = ts_re().replace_all(line, "").into_owned();
    let s = sub(r"\[\s*(INFO|WARN|ERROR|DEBUG)\s+", "[${1} ", &s);
    let s = sub(r"\b(EXCLUDED|for|of|symbol)\s+[A-Z][A-Z.]{0,5}\b", "${1} SYM", &s);
    let s = sub(r"\b[A-Z][A-Z.]{0,5} not pulled", "SYM not pulled", &s);
    let s = sub(r"\b[0-9a-f]{7,40}\b", "ID", &s);
    let s = sub(r"/(tmp|var/folders|private)/\S+", "/TMP", &s);
    let s = sub(r"\d+(\.\d+)?", "#", &s);
    clip(sub(r"\s+", " ", &s).trim(), 220)
}

/// Unix time of the time stamp at the start of the line (UTC when it ends in Z, local otherwise); None without one. Dates inside a message are
/// not times, so only the first 40 characters are looked at.
pub fn line_ts(line: &str) -> Option<i64> {
    let head: String = line.chars().take(40).collect();
    let m = ts_re().captures(&head).ok()??;
    let whole = m.get(0)?;
    let naive = NaiveDateTime::parse_from_str(&format!("{} {}", &m[1], clip(&m[2], 5)), "%Y-%m-%d %H:%M").ok()?;
    let after: String = line[whole.end()..].chars().take(12).collect();
    if re!(r"^(:\d{2})?(\.\d+)?Z").is_match(&after).unwrap_or(false) { return Some(naive.and_utc().timestamp()); }
    Some(Local.from_local_datetime(&naive).earliest().or_else(|| Local.from_local_datetime(&naive).latest()).map_or_else(|| naive.and_utc().timestamp(), |d| d.timestamp()))
}

/// Python `str.splitlines` (also splits on \r, \x0b, \x0c, \x1c-\x1e, \x85, \u2028, \u2029).
pub fn splitlines(s: &str) -> Vec<&str> {
    let (mut out, mut start, mut it) = (vec![], 0usize, s.char_indices().peekable());
    while let Some((i, c)) = it.next() {
        if matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}') {
            out.push(&s[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && it.peek().is_some_and(|(_, n)| *n == '\n') { it.next(); start += 1; }
        }
    }
    if start < s.len() { out.push(&s[start..]); }
    out
}

/// Adds one occurrence; a fixed warning seen again after its fix (+GRACE) is reopened.
pub fn record(c: &Connection, sig: &str, source: &str, log: &str, severity: &str, sample: &str, ts: i64, now: i64) -> Result<()> {
    let row: Option<(i64, String, Option<i64>)> = c.query_row("SELECT id, status, fixed_at FROM warnings WHERE sig = ?1", [sig], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
    let Some((wid, status, fixed_at)) = row else {
        c.execute("INSERT INTO warnings(sig, source, log, severity, sample, count, first_seen, last_seen) VALUES (?1,?2,?3,?4,?5,1,?6,?6)", params![sig, source, log, severity, clip(sample, 400), ts])?;
        let wid = c.last_insert_rowid();
        c.execute("INSERT INTO history(ts, warning, event, detail) VALUES (?1,?2,?3,?4)", params![now, wid, "new", clip(sample, 200)])?;
        return Ok(());
    };
    c.execute("UPDATE warnings SET count = count + 1, last_seen = max(last_seen, ?1), sample = ?2 WHERE id = ?3", params![ts, clip(sample, 400), wid])?;
    if status == "fixed" && fixed_at.is_some_and(|f| f != 0 && ts > f + GRACE) {
        c.execute("UPDATE warnings SET status = 'reopened', reopened = reopened + 1 WHERE id = ?1", [wid])?;
        c.execute("INSERT INTO history(ts, warning, event, detail) VALUES (?1,?2,?3,?4)", params![now, wid, "reopened: seen again after the fix", clip(sample, 200)])?;
    }
    Ok(())
}

fn relpath(path: &Path, home: &Path) -> String { path.strip_prefix(home).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string_lossy().into_owned()) }

/// Local date and time ("before the ledger" for a backlog line without a time stamp).
pub fn fmt(ts: i64) -> String {
    if ts == 0 { return "before the ledger (time unknown)".into(); }
    Local.timestamp_opt(ts, 0).single().map_or_else(|| ts.to_string(), |d| d.format("%Y-%m-%d %H:%M").to_string())
}

fn mtime(p: &Path) -> Option<f64> { std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()) }

fn now_i() -> i64 { crate::util::now_ts() }

/// Reads every log from where the last scan stopped, records warnings, and flags silent jobs.
pub fn scan() -> Result<String> {
    let p = paths(); let c = con(&p)?; let now = now_i();
    let tx = c.unchecked_transaction()?;
    let mut new = 0usize;
    let logs: Vec<(PathBuf, &str, &str)> = vec![(p.rt.join("logs"), ".err", "bot"), (p.rt.join("logs"), ".log", "bot"), (p.bot.join("ops/logs"), ".log", "bot-ops"), (p.lab.join("logs"), ".log", "lab")];
    for (dir, suffix, source) in &logs {
        let mut paths_: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
            .filter(|f| f.file_name().is_some_and(|n| { let n = n.to_string_lossy(); n.ends_with(suffix) && !n.starts_with('.') })).collect();
        paths_.sort();
        for path in paths_ {
            let Ok(st) = std::fs::metadata(&path) else { continue };
            let key = path.to_string_lossy().into_owned();
            let row: Option<(i64, i64)> = tx.query_row("SELECT inode, pos FROM offsets WHERE path = ?1", [&key], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            // first sight of a log: its last 2 MB; a rotated or truncated log starts again at 0
            let pos: u64 = match row { Some((ino, pos)) if ino as u64 == crate::platform::inode(&st) && pos as u64 <= st.len() => pos as u64, Some(_) => 0, None => st.len().saturating_sub(2_000_000) };
            let mut f = std::fs::File::open(&path)?;
            std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(pos))?;
            let mut data = Vec::new(); std::io::Read::read_to_end(&mut f, &mut data)?;
            // a line without its own time stamp takes the last one above it; in the backlog read at a log's first sight, a line before any time stamp
            // has an unknown time (0 = "before the ledger"); later reads are new lines, so: now
            let (mut last, fallback): (Option<i64>, i64) = (None, if row.is_some() { now } else { 0 });
            let text = String::from_utf8_lossy(&data).into_owned();
            let rel = relpath(&path, &p.home);
            for raw in splitlines(&text) {
                if let Some(t) = line_ts(raw) { last = Some(t); }
                if !hit().is_match(raw).unwrap_or(true) || not().is_match(raw).unwrap_or(false) { continue; }
                let sev = if errorish().is_match(raw).unwrap_or(false) { "error" } else { "warning" };
                record(&tx, &signature(raw), source, &rel, sev, raw.trim(), last.unwrap_or(fallback), now)?;
                new += 1;
            }
            tx.execute("INSERT OR REPLACE INTO offsets(path, inode, pos) VALUES (?1,?2,?3)", params![key, crate::platform::inode(&st) as i64, (pos + data.len() as u64) as i64])?;
        }
    }
    dashboards(&tx, now)?;
    builds_current(&tx, &p, now)?;
    let fresh: Vec<(PathBuf, f64)> = vec![(p.lab.join("logs/improve.log"), 60.0), (p.lab.join("logs/brain.log"), 30.0), (p.bot.join("ops/logs/monitor.log"), 60.0), (p.lab.join("logs/monitor.log"), 60.0), (p.rt.join("logs/bot.err"), 60.0)];
    let mt: Vec<Option<f64>> = fresh.iter().map(|(f, _)| mtime(f)).collect();
    let awake = mt.iter().flatten().cloned().fold(None, |a: Option<f64>, x| Some(a.map_or(x, |y| y.max(x)))).unwrap_or(now as f64);
    for ((f, mins), m) in fresh.iter().zip(mt) {
        let age = m.map(|m| (awake - m) / 60.0);
        if age.is_none_or(|a| a > *mins) {
            let rel = relpath(f, &p.home);
            record(&tx, &format!("STALE {rel}"), "health", &rel, "error",
                &format!("log not written for {} (expected at least every {} min): its job may have stopped", age.map_or("ever".to_string(), |a| format!("{a:.0} min")), crate::util::fmt_g(*mins)), now, now)?;
        }
    }
    tx.commit()?;
    let o: i64 = c.query_row("SELECT count(*) FROM warnings WHERE status IN ('open','reopened')", [], |r| r.get(0))?;
    Ok(format!("scan: {new} warning lines read; {o} open or reopened"))
}

fn fetch(url: &str) -> std::result::Result<String, (String, String)> {
    let host = url.split('/').nth(2).unwrap_or("");
    let out = Command::new("curl").args(["-s", "-m", "30", "-H", &format!("Host: {host}"), "-w", "\n%{http_code}", url]).output().map_err(|e| ("URLError".to_string(), e.to_string()))?;
    if !out.status.success() { return Err(("URLError".into(), format!("<urlopen error curl exit {}>", out.status.code().unwrap_or(-1)))); }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, "000"));
    match code.trim().parse::<u16>() { Ok(c) if c >= 400 => Err(("HTTPError".into(), format!("HTTP Error {c}"))), _ => Ok(body.to_string()) }
}

fn walk(c: &Connection, v: &Value, path: &str, name: &str, where_: &str, now: i64, seen: &mut HashSet<String>) -> Result<()> {
    match v {
        Value::Object(m) => for (k, x) in m { if path.is_empty() && k == "ticks" { continue; } walk(c, x, &format!("{path}.{k}"), name, where_, now, seen)?; },
        Value::Array(a) => for x in a { walk(c, x, path, name, where_, now, seen)?; },
        Value::String(s) if dash_bad().is_match(s).unwrap_or(false) => {
            let sig = format!("DASH {name} {path}: {}", signature(s));
            if seen.insert(sig.clone()) { record(c, &sig, "dashboard", where_, "error", &format!("{name} shows at {path}: {}", clip(s.trim(), 300)), now, now)?; }
        }
        _ => {}
    }
    Ok(())
}

/// The dashboards are checked the way a viewer sees them (owner 2026-10-11: "why are you not monitoring and fixing shit like this from
/// dashboard"): each must answer; no text may contain a leaked bug; the lab's newest tick steps must not have failed; its health numbers must be sane.
fn dashboards(c: &Connection, now: i64) -> Result<()> {
    for (name, url) in [("lab dashboard", "http://127.0.0.1:8790/api/snapshot"), ("bot console", "http://127.0.0.1:8789/api/status")] {
        let where_ = format!("{name} ({})", url.rsplit_once("/api").map_or(url, |x| x.0));
        let d: Value = match fetch(url).and_then(|b| serde_json::from_str::<Value>(&b).map_err(|e| ("JSONDecodeError".to_string(), e.to_string()))) {
            Ok(d) => d,
            Err((ty, e)) => { record(c, &format!("DASH {name} not answering: {ty}"), "dashboard", &where_, "error", &format!("{name} did not answer {url}: {e}"), now, now)?; continue; }
        };
        walk(c, &d, "", name, &where_, now, &mut HashSet::new())?;
        if name != "lab dashboard" { continue; }
        if let Some(steps) = d.get("ticks").and_then(Value::as_array).and_then(|t| t.first()).filter(|t| t.is_object()).and_then(|t| t.get("steps")).and_then(Value::as_array) {
            for step in steps {
                if step.get("status").and_then(Value::as_str) == Some("failed") {
                    let detail = match step.get("detail") { None => String::new(), Some(v) => py_str_pub(Some(v)) };
                    record(c, &format!("DASH lab tick step {} failed: {}", py_str_pub(step.get("name")), signature(&detail)), "dashboard", &where_, "error",
                        &format!("newest lab tick: step {} failed: {}", py_str_pub(step.get("name")), py_str_pub(step.get("detail"))), now, now)?;
                }
            }
        }
        if let Some(gen) = d.get("generated").and_then(Value::as_f64) {
            if now as f64 - gen > 600.0 { record(c, "DASH lab snapshot stale", "dashboard", &where_, "error", &format!("lab dashboard serves a snapshot from {}: stale data", fmt(gen as i64)), now, now)?; }
        }
        let h = d.get("health").cloned().filter(|h| h.is_object()).unwrap_or(Value::Object(Default::default()));
        let g = |k: &str| h.get(k);
        let n = |k: &str| g(k).and_then(Value::as_f64);
        let checks: Vec<(bool, String)> = vec![
            (g("schedule_loaded") == Some(&Value::Bool(false)), "the lab's hourly schedule is not loaded".into()),
            (n("last_tick").is_some_and(|t| t != 0.0 && now as f64 - t > 3.0 * 3600.0), "the lab's hourly tick has not run for over 3 hours".into()),
            (n("free_gb").is_some_and(|f| f < 0.5), format!("free memory is {} GB", py_str_pub(g("free_gb")))),
        ];
        for (bad, text) in checks { if bad { record(c, &format!("DASH lab health: {}", signature(&text)), "dashboard", &where_, "error", &format!("lab dashboard health: {text}"), now, now)?; } }
    }
    Ok(())
}

/// Nothing stale may run or be shown: each installed program must be at least as new as the newest code commit for it.
fn builds_current(c: &Connection, p: &Paths, now: i64) -> Result<()> {
    let newest = |repo: &Path, paths_: &[&str]| -> i64 {
        Command::new("git").arg("-C").arg(repo).args(["log", "-1", "--format=%ct", "--"]).args(paths_).output().ok().and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok()).unwrap_or(0)
    };
    let specs: [(&str, PathBuf, &Path, &[&str], &str); 2] = [("lab", p.lab.join("bin/qwen-lab"), &p.labrepo, &["src", "Cargo.toml", "Cargo.lock"], "error"), ("bot", p.rt.join("bin/bot"), &p.bot, &["src", "Cargo.toml", "Cargo.lock", "build.rs"], "warning")];
    for (name, binary, repo, paths_, sev) in specs {
        let Some(m) = mtime(&binary) else { continue };
        let commit = newest(repo, paths_);
        if commit != 0 && m + 60.0 < commit as f64 && now - commit > 20 * 60 {
            record(c, &format!("BUILD {name} installed binary older than its code"), "health", &binary.to_string_lossy().replace(&*p.home.to_string_lossy(), "~"), sev,
                &format!("the installed {name} binary ({}) is older than the newest {name} code commit ({}): the running program and the dashboards do not have the latest code{}", fmt(m as i64), fmt(commit),
                    if name == "bot" { " (the bot redeploys only while the US market is closed)" } else { "" }), now, now)?;
        }
    }
    Ok(())
}

/// True when sha is a commit in the bot or the lab repo.
fn git_has(p: &Paths, sha: &str) -> bool {
    [&p.bot, &p.labrepo].iter().any(|r| Command::new("git").arg("-C").arg(r).args(["cat-file", "-e", &format!("{sha}^{{commit}}")]).output().map(|o| o.status.success()).unwrap_or(false))
}

fn flag<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>> {
    match args.iter().position(|a| a == name) { None => Ok(None), Some(i) => args.get(i + 1).map(|s| Some(s.as_str())).ok_or_else(|| anyhow!("{name} needs a value")) }
}

/// Marks an entry fixed (a real commit required) or expected (a reason required).
pub fn mark(wid: i64, status: &str, args: &[String]) -> Result<String> {
    let p = paths(); let c = con(&p)?;
    let note = flag(args, "--note")?.unwrap_or("");
    if note.trim().chars().count() < 10 { bail!("refused: --note must say what was done (or why it is expected), at least 10 characters"); }
    let commit = flag(args, "--commit")?;
    if status == "fixed" && !commit.is_some_and(|s| !s.is_empty() && git_has(&p, s)) { bail!("refused: --commit {} is not a commit in the bot or lab repo; a warning is fixed only by a real commit", commit.map_or("None".to_string(), String::from)); }
    if c.query_row("SELECT 1 FROM warnings WHERE id = ?1", [wid], |_| Ok(())).optional()?.is_none() { bail!("no warning {wid}"); }
    let now = now_i();
    c.execute("UPDATE warnings SET status = ?1, commit_id = ?2, note = ?3, fixed_at = ?4 WHERE id = ?5", params![status, commit, note, now, wid])?;
    c.execute("INSERT INTO history(ts, warning, event, detail) VALUES (?1,?2,?3,?4)", params![now, wid, status, clip(&format!("{} {note}", commit.unwrap_or("")), 300)])?;
    Ok(format!("warning {wid} marked {status}"))
}

/// Account-like numbers and secrets removed for publishing.
pub fn redact(t: &str) -> String {
    let t = re!(r"\b\d{6,}\b").replace_all(t, "[number]").into_owned();
    re!(r"(?i)(api[_-]?key|secret|password|passwd|token|trd_?pwd)\s*[:=]\s*\S+").replace_all(&t, "${1}=[redacted]").into_owned()
}

/// Prints open and reopened entries (all with --all), most severe and newest first.
pub fn listing(all_rows: bool) -> Result<Vec<String>> {
    let c = con(&paths())?;
    let q = format!("SELECT id, status, severity, count, last_seen, log, sample, commit_id, note FROM warnings {}ORDER BY status = 'reopened' DESC, severity = 'error' DESC, last_seen DESC", if all_rows { "" } else { "WHERE status IN ('open','reopened') " });
    let mut st = c.prepare(&q)?;
    let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?, r.get::<_, String>(5)?, r.get::<_, String>(6)?, r.get::<_, Option<String>>(7)?, r.get::<_, Option<String>>(8)?)))?;
    let mut out = vec![];
    for r in rows { let r = r?;
        let mut l = format!("#{} [{}/{}] x{} last {} {}\n    {}", r.0, r.1, r.2, r.3, fmt(r.4), r.5, clip(&redact(&r.6), 300));
        if r.7.as_deref().is_some_and(|s| !s.is_empty()) || r.8.as_deref().is_some_and(|s| !s.is_empty()) { l.push_str(&format!("\n    -> {} {}", r.7.unwrap_or_default(), r.8.unwrap_or_default())); }
        out.push(l);
    }
    Ok(out)
}

pub fn report(out: &str) -> Result<String> {
    let c = con(&paths())?;
    let mut st = c.prepare("SELECT id, status, severity, count, first_seen, last_seen, log, sample, commit_id, note, fixed_at, reopened FROM warnings ORDER BY CASE status WHEN 'reopened' THEN 0 WHEN 'open' THEN 1 WHEN 'fixed' THEN 2 ELSE 3 END, severity = 'error' DESC, last_seen DESC")?;
    type R = (i64, String, String, i64, i64, i64, String, String, Option<String>, Option<String>, Option<i64>, i64);
    let rows: Vec<R> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?)))?.collect::<Result<_, _>>()?;
    let n = |s: &str| rows.iter().filter(|r| r.1 == s).count();
    let tz = crate::platform::tz_name();
    let mut md: Vec<String> = vec!["# Warnings and errors from the logs".into(), String::new(),
        format!("Updated {} {tz} by `qwen-tools warnings` (code). Every log of the bot and the lab is scanned hourly; one entry per distinct problem. **Fixed** = a real commit (checked) fixed it; if it is seen again after the fix it is **reopened** automatically. **Expected** = a known harmless condition, with the reason. The hourly Claude Code log doctor (`ops/log_doctor.sh`) works on the open entries. Account-like numbers and secrets are redacted.", Local::now().format("%Y-%m-%d %H:%M")),
        String::new(), format!("**{} reopened · {} open · {} fixed · {} expected**", n("reopened"), n("open"), n("fixed"), n("expected")), String::new()];
    for (title, keep) in [("Reopened (the fix did not hold)", "reopened"), ("Open", "open"), ("Fixed", "fixed"), ("Expected (harmless, with reason)", "expected")] {
        let part: Vec<&R> = rows.iter().filter(|r| r.1 == keep).collect();
        if part.is_empty() { continue; }
        md.extend([format!("## {title}"), String::new(), "| # | severity | seen | first / last | log | message | resolution |".into(), "|---|---|---|---|---|---|---|".into()]);
        for r in part {
            let msg = clip(&redact(&r.7).replace('|', "\\|").replace('\n', " "), 220);
            let res = format!("{}{}{}{}", r.8.as_deref().filter(|s| !s.is_empty()).map_or(String::new(), |s| format!("`{s}` ")), clip(&redact(r.9.as_deref().unwrap_or("")).replace('|', "\\|"), 200),
                match r.10 { Some(f) if f != 0 && (keep == "fixed" || keep == "expected") => format!(" (marked {})", fmt(f)), _ => String::new() }, if r.11 != 0 { format!(" (reopened {}x)", r.11) } else { String::new() });
            md.push(format!("| {} | {} | {}x | {} / {} | `{}` | {} | {} |", r.0, r.2, r.3, fmt(r.4), fmt(r.5), r.6, msg, res));
        }
        md.push(String::new());
    }
    std::fs::write(out, md.join("\n") + "\n")?;
    Ok(format!("report: {out}"))
}

pub fn main(a: &[String]) -> i32 {
    let run = || -> Result<Vec<String>> {
        let cmd = a.first().map(String::as_str).unwrap_or("list");
        Ok(match cmd {
            "scan" => vec![scan()?],
            "list" => listing(a.iter().any(|x| x == "--all"))?,
            "show" => {
                let id: i64 = a.get(1).ok_or_else(|| anyhow!("show needs an id"))?.parse()?;
                let c = con(&paths())?;
                let row: Option<Vec<Value>> = c.query_row("SELECT * FROM warnings WHERE id = ?1", [id], |r| (0..r.as_ref().column_count()).map(|i| Ok(match r.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => Value::Null, rusqlite::types::ValueRef::Integer(n) => Value::from(n), rusqlite::types::ValueRef::Real(f) => Value::from(f),
                    rusqlite::types::ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()), rusqlite::types::ValueRef::Blob(_) => Value::Null })).collect()).optional()?;
                vec![redact(&row.map_or("None".to_string(), |v| format!("({})", v.iter().map(py_repr).collect::<Vec<_>>().join(", "))))]
            }
            "fixed" | "expected" => vec![mark(a.get(1).ok_or_else(|| anyhow!("{cmd} needs an id"))?.parse()?, cmd, &a[2..])?],
            "report" => vec![report(a.get(1).ok_or_else(|| anyhow!("report needs an output file"))?)?],
            "counts" => {
                let c = con(&paths())?; let mut st = c.prepare("SELECT status, count(*) FROM warnings GROUP BY 1")?;
                let m: serde_json::Map<String, Value> = st.query_map([], |r| Ok((r.get::<_, String>(0)?, Value::from(r.get::<_, i64>(1)?))))?.collect::<Result<_, _>>()?;
                vec![py_repr(&Value::Object(m))]
            }
            _ => bail!("usage: warnings scan | list [--all] | show <id> | fixed <id> --commit <sha> --note <text> | expected <id> --note <text> | report <out.md> | counts"),
        })
    };
    match run() { Ok(lines) => { for l in lines { println!("{l}"); } 0 } Err(e) => { eprintln!("{e:#}"); 1 } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE warnings (id INTEGER PRIMARY KEY, sig TEXT UNIQUE, source TEXT, log TEXT, severity TEXT, sample TEXT, count INTEGER, first_seen INTEGER, last_seen INTEGER, status TEXT DEFAULT 'open', commit_id TEXT, note TEXT, fixed_at INTEGER, reopened INTEGER DEFAULT 0);
            CREATE TABLE history (id INTEGER PRIMARY KEY, ts INTEGER, warning INTEGER, event TEXT, detail TEXT);").unwrap();
        c
    }

    // expected values below were produced by the original Python on the same strings
    #[test]
    fn signatures_remove_what_changes_between_occurrences() {
        assert_eq!(signature("2026-10-13T09:52:21Z [ERROR  bot::x] failed for AAPL at 12:34 id abcdef1234 /tmp/abc/d 3.5 times"), "Z [ERROR bot::x] failed for SYM at #:# id ID /TMP # times");
        assert_eq!(signature("MSFT not pulled (3 of 80)"), "SYM not pulled (# of #)");
    }

    #[test]
    fn hit_and_not_decide_what_is_a_warning() {
        for (line, warn) in [("ERROR something broke", true), ("rc=1 in 361s", true), ("rc=0 in 361s", false), ("0 failed", false), ("5h|x: failed-holdout because", false), ("all fine", false), ("exit 2", true)] {
            assert_eq!(hit().is_match(line).unwrap() && !not().is_match(line).unwrap(), warn, "{line}");
        }
    }

    #[test]
    fn line_timestamps_utc_and_local() {
        assert_eq!(line_ts("2026-10-13T00:14:00Z hello"), Some(1_791_850_440));
        assert_eq!(line_ts("no stamp here"), None);
        assert_eq!(line_ts("2026-13-45 99:99 nonsense"), None);
        assert!(line_ts("x 2026-10-13 09:52:21,811 - omlx").is_some());
        assert_eq!(line_ts(&format!("{}2026-10-13T00:14Z", " ".repeat(45))), None, "only the first 40 characters count");
    }

    #[test]
    fn splitlines_like_python() {
        assert_eq!(splitlines("a\nb\r\nc\rd\x0ce"), vec!["a", "b", "c", "d", "e"]);
        assert_eq!(splitlines("trailing\n"), vec!["trailing"]);
        assert!(splitlines("").is_empty());
    }

    #[test]
    fn a_fixed_warning_reopens_only_after_the_grace_period() {
        let c = mem();
        record(&c, "sig", "bot", "x.log", "error", "boom", 100, 100).unwrap();
        c.execute("UPDATE warnings SET status='fixed', fixed_at=1000 WHERE sig='sig'", []).unwrap();
        record(&c, "sig", "bot", "x.log", "error", "boom", 1000 + GRACE, 0).unwrap();
        let st: String = c.query_row("SELECT status FROM warnings", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "fixed", "within the grace period it may be the old run's output");
        record(&c, "sig", "bot", "x.log", "error", "boom", 1000 + GRACE + 1, 0).unwrap();
        let (st, re, n): (String, i64, i64) = c.query_row("SELECT status, reopened, count FROM warnings", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((st.as_str(), re, n), ("reopened", 1, 3));
        let h: i64 = c.query_row("SELECT count(*) FROM history", [], |r| r.get(0)).unwrap();
        assert_eq!(h, 2, "new + reopened");
    }

    #[test]
    fn very_long_lines_do_not_break_the_matchers() {
        let long = format!("ERROR {}", "x%41".repeat(250_000));
        assert!(hit().is_match(&long).unwrap());
        let _ = not().is_match(&format!("      {}", "a".repeat(200_000))).unwrap();
        assert!(signature(&long).chars().count() <= 220);
    }

    #[test]
    fn redaction() {
        assert_eq!(redact("acct 281756455998 and api_key=abc123 token: xyz, 12345 stays"), "acct [number] and api_key=[redacted] token=[redacted] 12345 stays");
    }

    #[test]
    fn dashboard_walk_flags_leaked_bug_text_but_skips_ticks() {
        let c = mem();
        let d: Value = serde_json::from_str(r#"{"a":{"b":"value is NaN here"},"ticks":[{"x":"Traceback"}],"fine":"ok"}"#).unwrap();
        walk(&c, &d, "", "lab dashboard", "w", 5, &mut HashSet::new()).unwrap();
        let sigs: Vec<String> = c.prepare("SELECT sig FROM warnings").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(sigs, vec!["DASH lab dashboard .a.b: value is NaN here"]);
    }

    #[test]
    fn quote_helper_is_exported_for_python_style_text() { assert_eq!(crate::misc_tools::py_quote("a"), "'a'"); }
}

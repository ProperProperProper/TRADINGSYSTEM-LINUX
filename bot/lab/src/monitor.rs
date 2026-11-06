//! The lab + bot monitor (`qwen-ops monitor`, every 15 minutes), a port of the former `ops/lab_monitor.sh` that runs the same way on macOS, Linux and
//! Windows. It costs no Claude tokens while everything is healthy.
//!  1. checks: the bot service, OpenD, the brain, the learning loop's jobs, results still arriving, hung studies, disk, archive freshness;
//!  2. fixes what code can fix by itself (loads a missing job, restarts a dead or stuck one, redeploys newer code, kills a hung study);
//!  3. a fault it cannot fix starts ONE headless Claude session (at most once per `FIX_GAP_MIN`) that finds the root cause and fixes it;
//!  4. always rewrites `<lab root>/STATUS.md` (what is running, what was found, what was fixed).
//!
//! Logs: `<lab root>/logs/monitor.log`. The owner is away: nothing here ever asks a question.

use crate::botctl;
use crate::jobs;
use crate::opsctl;
use crate::platform::{self, Os};
use crate::service;
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FIX_GAP_MIN: i64 = 120;

fn now() -> i64 { chrono::Utc::now().timestamp() }

/// Read-only SQLite query returning the first column of the first row as text ("" on any problem).
fn sq(db: &Path, sql: &str) -> String {
    let Ok(c) = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY) else { return String::new() };
    let _ = c.busy_timeout(std::time::Duration::from_secs(5));
    c.query_row(sql, [], |r| {
        let v: rusqlite::types::Value = r.get(0)?;
        Ok(match v { rusqlite::types::Value::Null => String::new(), rusqlite::types::Value::Integer(i) => i.to_string(), rusqlite::types::Value::Real(f) => f.to_string(), rusqlite::types::Value::Text(t) => t, rusqlite::types::Value::Blob(_) => String::new() })
    }).unwrap_or_default()
}

/// All rows of a query as `|`-joined text lines.
fn sq_rows(db: &Path, sql: &str) -> Vec<String> {
    let Ok(c) = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY) else { return vec![] };
    let _ = c.busy_timeout(std::time::Duration::from_secs(5));
    let Ok(mut st) = c.prepare(sql) else { return vec![] };
    let n = st.column_count();
    st.query_map([], |r| {
        let mut cols = Vec::new();
        for i in 0..n {
            let v: rusqlite::types::Value = r.get(i)?;
            cols.push(match v { rusqlite::types::Value::Null => String::new(), rusqlite::types::Value::Integer(i) => i.to_string(), rusqlite::types::Value::Real(f) => f.to_string(), rusqlite::types::Value::Text(t) => t, rusqlite::types::Value::Blob(_) => String::new() });
        }
        Ok(cols.join("|"))
    }).map(|it| it.flatten().collect()).unwrap_or_default()
}

fn num(s: &str) -> i64 { s.trim().parse::<f64>().map(|f| f as i64).unwrap_or(0) }

fn append(path: &Path, line: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Tail of a file: its last `n` lines.
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|s| s.to_string()).collect()
}

/// Whether the git work tree of `repo` has no uncommitted changes.
fn clean(repo: &Path) -> bool { platform::git(repo, &["status", "--porcelain"]).is_some_and(|s| s.is_empty()) }

fn short(repo: &Path) -> String { platform::git(repo, &["rev-parse", "--short", "HEAD"]).unwrap_or_default() }

/// A calendar job is overdue when it has never produced its log or the log is older than
/// its allowed cadence. Loaded/last-exit state alone cannot detect a missed calendar firing.
fn periodic_overdue(last: i64, now: i64, max_age: i64) -> bool {
    last <= 0 || now.saturating_sub(last) > max_age
}

/// Commit time of the newest commit touching `pathspec` (unix seconds, 0 if unknown).
fn head_time(repo: &Path, pathspec: &[&str]) -> i64 {
    let mut a = vec!["log", "-1", "--format=%ct", "--"];
    a.extend_from_slice(pathspec);
    platform::git(repo, &a).and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// Version drift against `ops/versions.lock` (lines `<tool> <version>`): returns the lines that no longer match. A NOTICE for the owner only.
pub fn version_drift(lock: &str) -> Vec<String> {
    lock.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).filter(|l| {
        let mut it = l.split_whitespace();
        let (Some(tool), Some(want)) = (it.next(), it.next()) else { return false };
        let have = platform::run(tool, &["--version"]).and_then(|v| v.split_whitespace().nth(1).map(String::from)).unwrap_or_default();
        have != want
    }).map(String::from).collect()
}

/// The monitor, once.
pub fn run(args: &[String]) {
    let cfg_path = opsctl::config_path(args);
    let lab = platform::lab_root();
    let (repo, bot) = (platform::lab_repo(), platform::bot_repo());
    let logs = lab.join("logs");
    let _ = fs::create_dir_all(&logs);
    let log = logs.join("monitor.log");
    let status_file = lab.join("STATUS.md");
    let memdb = lab.join("memory/memory.sqlite3");
    let t0 = now();
    let stamp = chrono::Utc::now().format("%Y-%m-%dT%H:%MZ").to_string();
    // every commit reaches GitHub: push anything committed but not pushed (lab and bot repo; owner 2026-10-11)
    botctl::push_guard();
    let mut faults: Vec<String> = vec![];
    let mut fixed: Vec<String> = vec![];
    let mut user: Vec<String> = vec![];

    // --- 1. jobs: load what is missing, restart what has no process
    let revive = |label: &str, alive: bool, fixed: &mut Vec<String>, faults: &mut Vec<String>| {
        let mut st = service::state(label);
        if !st.loaded {
            if service::load(label) { fixed.push(format!("loaded {label}")); }
            st = service::state(label);
            if !st.loaded { faults.push(format!("scheduled job {label} is not loaded and could not be loaded")); }
        }
        if alive && st.loaded && !st.running {
            service::restart(label);
            fixed.push(format!("restarted {label}"));
        }
    };
    revive("com.claudemoomoobot.bot", true, &mut fixed, &mut faults);
    for l in jobs::ALWAYS_ON { revive(l, true, &mut fixed, &mut faults); }
    revive(jobs::TICK, false, &mut fixed, &mut faults);
    for label in [
        "com.claudemoomoobot.promote",
        "com.claudemoomoobot.logdoctor",
        "com.claudemoomoobot.fixreport",
        "com.claudemoomoobot.monitor",
        "com.claudemoomoobot.labexport",
        "com.claudemoomoobot.universefetch",
        "com.claudemoomoobot.fillprobe",
    ] {
        revive(label, false, &mut fixed, &mut faults);
    }
    // The daily report can remain loaded with exit 0 even after its calendar firing is
    // missed. Its own log is written at startup, so one kickstart restores the cadence and
    // naturally prevents another kickstart on the next 15-minute monitor pass.
    let fix_report_log = platform::bot_logs_dir().join("fix_report.log");
    if periodic_overdue(platform::mtime(&fix_report_log), t0, 30 * 3600) {
        service::restart("com.claudemoomoobot.fixreport");
        fixed.push("started the overdue daily fix report".into());
    }

    // --- always current (owner's rule): nothing runs older code than the latest commit. Only clean repos are built, so half-finished edits are never
    //     deployed; the bot goes through its deploy command and its test and lint gates.
    let lab_head = head_time(&repo, &["."]);
    let stable = jobs::stable_binary(&crate::config::Config::load(&cfg_path).unwrap_or_else(|_| crate::config::Config::for_root(&lab, "real")));
    let lab_bin = platform::mtime(&stable);
    if lab_head > lab_bin && clean(&repo) {
        // the one safe way to replace the binary: build, unload the jobs, replace, register again (qwen-ops redeploy)
        let (ok, text) = opsctl::redeploy_text(args);
        if ok { fixed.push(format!("lab updated to {}, every job unloaded and loaded again", short(&repo))); } else { faults.push(format!("lab redeploy problem at {}: {}", short(&repo), text.replace('\n', " ").chars().take(200).collect::<String>())); }
    }
    // a long-running loop keeps the code it started with: when the binary was installed by hand (no restart) the loop must be restarted, or new
    // options are never tested (gate:spyvol sat untested for 11 h behind an old process, 2026-10-13)
    let lab_bin = platform::mtime(&stable);
    if lab_bin > 0 {
        if let Some(p) = platform::processes("improve --forever").into_iter().find(|p| p.command.contains("qwen-lab")) {
            if (t0 - p.age_secs as i64) < lab_bin && service::state(jobs::IMPROVE).running {
                service::restart(jobs::IMPROVE);
                fixed.push("restarted the learning loop: it ran older code than the installed binary".into());
            }
        }
    }
    // the bot's own code only: commits that touch just lab/ (the lab mirror) or research/ (published outputs) do not change the bot
    let bot_head = head_time(&bot, &[".", ":!lab", ":!research"]);
    let bot_bin = platform::mtime(&platform::bot_state_dir().join("bin").join(platform::exe("bot")));
    if bot_head > bot_bin && clean(&bot) {
        let (ok, _) = botctl::deploy();
        if ok { fixed.push(format!("bot redeployed at {}", short(&bot))); } else { faults.push(format!("bot deploy failed at {} (tests or lint)", short(&bot))); }
    }
    let study = bot.join("target/release/examples").join(platform::exe("variant_study"));
    if bot_head > platform::mtime(&study) && clean(&bot) {
        let ok = Command::new("cargo").args(["build", "--release", "--examples"]).current_dir(&bot).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
        if ok { fixed.push(format!("research tools rebuilt at {}", short(&bot))); } else { faults.push("research tools build failed".into()); }
    }

    // --- pinned versions (owner: never upgrade models or systems once everything works): any drift is a notice, never auto-"fixed" by upgrading
    if let Ok(lock) = fs::read_to_string(repo.join("ops/versions.lock")) {
        let drift = version_drift(&lock);
        if !drift.is_empty() { user.push(format!("version drift from ops/versions.lock (expected: {}); no automatic action", drift.join("; "))); }
    }

    // --- OpenD (only the user can log in)
    if !crate::ops::port_open(11111) { user.push("OpenD is down or logged out: open the moomoo OpenD app and log in".into()); }

    // --- hung studies (a variant_study running for more than 2 hours)
    for p in platform::processes("examples/variant_study").into_iter().chain(platform::processes("examples\\variant_study")) {
        if p.age_secs >= 7200 {
            platform::kill_pid(p.pid);
            fixed.push(format!("killed a hung study (pid {}, running {} min)", p.pid, p.age_secs / 60));
        }
    }

    // --- results still arriving / the whiteboard working
    let (mut ok24, mut bad24) = (0, 0);
    if memdb.exists() {
        let last = num(&sq(&memdb, "select coalesce(max(ts),0) from (select max(ts) ts from strategy_runs union all select max(ts) from whiteboard_log)"));
        if last > 0 && t0 - last > 18_000 {
            service::restart(jobs::IMPROVE);
            fixed.push(format!("restarted the learning loop: no result or note for {} h", (t0 - last) / 3600));
        }
        ok24 = num(&sq(&memdb, &format!("select count(*) from whiteboard_log where accepted=1 and ts > {}", t0 - 86_400)));
        bad24 = num(&sq(&memdb, &format!("select count(*) from whiteboard_log where accepted=0 and ts > {}", t0 - 86_400)));
        if ok24 == 0 && bad24 >= 10 { faults.push(format!("the whiteboard refused {bad24} edits in 24 h and accepted none")); }
    }
    let nfail = tail(&logs.join("improve.log"), 400).iter().filter(|l| l.contains("study failed") || l.contains("nothing done")).count();
    if nfail > 8 { faults.push(format!("the learning loop log shows {nfail} failures in its last 400 lines")); }

    // --- bot
    let b = botctl::check(30);
    if b.contains("NOT LOADED") { faults.push("bot service not loaded".into()); }
    if b.contains("tasks: DOWN") { faults.push("a bot task is down".into()); }
    let errs: Vec<&str> = b.lines().filter(|l| { let t = l.trim_start(); t.split_whitespace().next().is_some_and(|n| n.parse::<u32>().is_ok()) && t.contains(" ERROR ") }).collect();
    if !errs.is_empty() { faults.push(format!("bot errors in the last 30 minutes: {}", errs.iter().take(2).map(|s| s.trim()).collect::<Vec<_>>().join(" ").chars().take(160).collect::<String>())); }

    // --- disk and archives
    if let Some(gb) = platform::disk_free_gb(&platform::home()) { if gb < 10.0 { faults.push(format!("only {gb:.0} GB of disk free")); } }
    let arch = platform::bot_backtest_archive_path();
    if arch.exists() {
        let age = t0 - platform::mtime(&arch);
        if age > 172_800 { faults.push(format!("the training archive is {} h old: the daily export is not running", age / 3600)); }
        let mb = fs::metadata(&arch).map_or(0, |m| m.len() / 1_000_000);
        if mb > 4000 { faults.push(format!("the training archive is {mb} MB")); }
    }

    // Discord: every change the monitor made and every fault it found (redacted by qwen-notify; no-op without a webhook)
    let notify = platform::tool("qwen-notify");
    if notify.exists() {
        for (title, list) in [("Lab monitor fixed", &fixed), ("Lab monitor FAULT", &faults)] {
            for f in list {
                let _ = Command::new(&notify).args(["--title", title, f]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
            }
        }
    }

    // --- 4. status file
    write_status(&status_file, &stamp, &memdb, &faults, &fixed, &user, ok24, bad24, &log);

    // --- log + escalate
    if !fixed.is_empty() { append(&log, &format!("{stamp} fixed: {}", fixed.join(" "))); }
    if !user.is_empty() {
        append(&log, &format!("{stamp} USER: {}", user.join(" ")));
        desktop_notice(&user[0]);
    }
    if faults.is_empty() {
        append(&log, &format!("{stamp} ok"));
        return;
    }
    // MAINTENANCE flag (an interactive session is changing things on purpose): no repair session while it is younger than 6 hours
    let maint = lab.join("MAINTENANCE");
    if platform::mtime(&maint) > 0 && t0 - platform::mtime(&maint) < 360 * 60 {
        append(&log, &format!("{stamp} faults during maintenance, no repair session: {}", faults.join(" ")));
        return;
    }
    append(&log, &format!("{stamp} FAULT: {}", faults.join(" ")));
    let lastfix_file = logs.join(".lastfix");
    let lastfix = fs::read_to_string(&lastfix_file).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
    if t0 - lastfix < FIX_GAP_MIN * 60 {
        append(&log, &format!("{stamp} a fix session ran < {FIX_GAP_MIN} min ago, waiting"));
        return;
    }
    let _ = fs::write(&lastfix_file, t0.to_string());
    repair_session(&faults, &stamp, &lab, &repo, &bot, &cfg_path, &logs, &log);
}

/// A desktop notice for something only the owner can do (macOS notification centre; elsewhere the log and Discord are enough).
fn desktop_notice(text: &str) {
    if platform::os() == Os::Mac {
        let esc = text.replace('"', "'");
        let _ = Command::new("osascript").args(["-e", &format!("display notification \"{esc}\" with title \"lab monitor\"")]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

#[allow(clippy::too_many_arguments)]
fn write_status(path: &Path, stamp: &str, memdb: &Path, faults: &[String], fixed: &[String], user: &[String], ok24: i64, bad24: i64, log: &Path) {
    let mut s = format!("# Status  (written by qwen-ops monitor at {stamp})\n\n## Bot\n```\n");
    for l in botctl::check(15).lines().take(6) { s.push_str(&l.chars().take(200).collect::<String>()); s.push('\n'); }
    s.push_str("```\n## Jobs\n");
    for l in opsctl::status() { s.push_str(&format!("- {l}\n")); }
    s.push_str("## Learning loop\n");
    s.push_str(&format!("- results (cut A / alternate cut B): {}  | newest: {} UTC\n", sq(memdb, "select sum(cut='A') || ' ' || sum(cut='B') from strategy_runs"), sq(memdb, "select datetime(max(ts),'unixepoch') from strategy_runs")));
    s.push_str(&format!("- whiteboard edits accepted/refused (24 h): {ok24}/{bad24}; active beliefs/plan items: {}\n", sq(memdb, "select sum(kind='belief') || ' ' || sum(kind='plan') from whiteboard where status='active'")));
    s.push_str(&format!("- winners remembered (>=15% over 14 sessions): {} (count, best %) - see HIGHLIGHTS.md\n", sq(memdb, "select count(*) || ' ' || coalesce(round(max(return_pct),1),0) from highlights")));
    s.push_str("- best results so far (interval|options @capital: mean % per 14-session block, training / holdout / noisy):\n");
    for r in sq_rows(memdb, "select '  '||interval||'|'||tokens||' @'||cast(round(equity) as int)||' ['||cut||']: '||printf('%+.2f',mean)||' / '||printf('%+.2f',h1)||' / '||printf('%+.2f',h2)||' / '||ifnull(printf('%+.2f',nh1),'?') from strategy_runs order by mean desc limit 5") {
        s.push_str(&format!("{r}\n"));
    }
    let list = |v: &[String], none: &str| if v.is_empty() { none.to_string() } else { v.join(" ") };
    s.push_str(&format!("## Monitor\n- faults now: {}\n- fixed this run: {}\n- needs the user: {}\n- recent log:\n", list(faults, "none"), list(fixed, "nothing needed"), list(user, "nothing")));
    for l in tail(log, 6) { s.push_str(&format!("    {l}\n")); }
    let _ = fs::write(path, s);
}

/// ONE headless Claude session that finds the root cause of the faults and fixes it.
#[allow(clippy::too_many_arguments)]
fn repair_session(faults: &[String], stamp: &str, lab: &Path, repo: &Path, bot: &Path, cfg_path: &Path, logs: &Path, log: &Path) {
    let Some(claude) = platform::claude_bin() else {
        append(log, &format!("{stamp} FAULT: the claude CLI was not found, no repair session (install it: docs/SETUP_*.md)"));
        return;
    };
    let joined = faults.join(" ");
    let prompt = format!("The lab/bot monitor found faults: {joined}.\nStart from STATE.md (the notes file). Find the ROOT CAUSE (logs: {logs}/improve.log, {logs}/monitor.log, {lab}/memory/memory.sqlite3 tables whiteboard_log / strategy_runs, the bot's log {botlog}/bot.err; bot repo {bot}) and fix it properly.\nRules: every commit is pushed right away (git push; the owner wants every fix on GitHub); real data only, never fabricate numbers; exits of the trading strategy are frozen; never ask questions (the user is away); keep token use low.\nNever install, delete, pull, upgrade or downgrade model runtimes automatically, and never revert or overwrite uncommitted changes. The checked online research ensemble is documented in docs/ONLINE_AI.md; it proposes stored-candle tests only. Never loosen the security guard (src/guard.rs). After a code fix: cargo test --release and cargo clippy --release must pass; then `qwen-ops redeploy --config {cfg}` rebuilds, replaces the binary and registers every job again; commit and git push; update STATE.md. For the bot repo use `bot-ops deploy` and `bot-ops check` there (or ./deploy.sh and ./check.sh on macOS/Linux).",
        logs = logs.display(), lab = lab.display(), botlog = platform::bot_logs_dir().display(), bot = bot.display(), cfg = cfg_path.display());
    let os_tools = match platform::os() {
        Os::Mac => "Bash(launchctl:*)",
        Os::Linux => "Bash(systemctl:*) Bash(journalctl:*)",
        Os::Windows => "Bash(schtasks:*) Bash(tasklist:*)",
    };
    let allowed = format!("Read Edit Write Grep Glob Bash(cargo:*) Bash(git:*) Bash(tail:*) Bash(grep:*) Bash(ls:*) {os_tools} Bash(sqlite3:*) Bash(curl:*) Bash(qwen-ops:*) Bash(bot-ops:*) Bash(target/release/qwen-lab:*) Bash(./deploy.sh) Bash(./check.sh:*) Bash(cat:*) Bash(sed:*) Bash(head:*)");
    let report = logs.join(format!("fix-{stamp}.md").replace(':', ""));
    let mut cmd = platform::command_for(&claude);
    cmd.current_dir(repo).args(["-p", &prompt, "--permission-mode", "acceptEdits", "--allowedTools", &allowed]).stdin(Stdio::null());
    if let Ok(f) = fs::File::create(&report) {
        match f.try_clone() { Ok(g) => { cmd.stdout(g); } Err(_) => { cmd.stdout(Stdio::null()); } }
        cmd.stderr(f);
    }
    let code = cmd.status().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    append(log, &format!("{stamp} fix session finished (exit {code}): {}", report.display()));
}

/// Where the monitor's own log is (for the dashboard and the docs).
pub fn log_path() -> PathBuf { platform::lab_root().join("logs/monitor.log") }

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: version drift lists only the pinned lines that no longer match the installed tool.
    #[test]
    fn version_drift_lists_mismatches() {
        let rustc = platform::run("rustc", &["--version"]).unwrap_or_default().split_whitespace().nth(1).unwrap_or("").to_string();
        assert!(version_drift(&format!("# comment\nrustc {rustc}\n")).is_empty());
        assert_eq!(version_drift("rustc 0.0.1\nno-such-tool-xyz 1.0\n").len(), 2);
    }

    /// Test: the SQLite helpers return text, not errors, for missing databases and tables.
    #[test]
    fn sqlite_helpers_are_forgiving() {
        assert_eq!(sq(Path::new("/no/such.sqlite3"), "select 1"), "");
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.sqlite3");
        let c = Connection::open(&p).unwrap();
        c.execute_batch("create table t(a integer, b text); insert into t values (1,'x'),(2,NULL);").unwrap();
        drop(c);
        assert_eq!(sq(&p, "select count(*) from t"), "2");
        assert_eq!(sq(&p, "select * from missing"), "");
        assert_eq!(sq_rows(&p, "select a, b from t order by a"), vec!["1|x", "2|"]);
        assert_eq!(num("12.9"), 12);
    }

    /// Test: the tail of a file is its last lines.
    #[test]
    fn tail_returns_the_last_lines() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.log");
        fs::write(&p, "1\n2\n3\n4\n").unwrap();
        assert_eq!(tail(&p, 2), vec!["3", "4"]);
        assert!(tail(&d.path().join("none"), 3).is_empty());
    }

    #[test]
    fn periodic_jobs_are_overdue_when_missing_or_past_the_grace_period() {
        assert!(periodic_overdue(0, 10_000, 100));
        assert!(!periodic_overdue(9_900, 10_000, 100));
        assert!(periodic_overdue(9_899, 10_000, 100));
    }
}

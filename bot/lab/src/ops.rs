//! Resource safety, in Rust, on macOS, Linux and Windows (replaces the zsh scripts resource_watch/heal/watchdog/train_guard/health; owner 2026-10-13:
//! "everything is rust only"). See docs/RESOURCE_SAFETY.md for the incident this exists for. The decisions are pure functions with unit tests; the
//! machine probes and the job control come from `platform` and `service`, so the same code runs on every OS.
//!
//! Commands (`qwen-ops <cmd>`): `watch` (every 5 min: sample, alert, then self-heal), `watchdog` (every 10 min: starts one headless Claude fixer when a
//! problem persists), `health` (one-shot report).

use crate::jobs;
use crate::platform::{self, Os};
use crate::service;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const SWAP_ALERT_MB: u64 = 1500;

// ---------------------------------------------------------------- paths and small helpers
pub fn home() -> PathBuf { platform::home() }
pub fn lab() -> PathBuf { platform::lab_root() }
fn logs() -> PathBuf { lab().join("logs") }
fn state() -> PathBuf { let d = lab().join("ops/.heal-state"); let _ = fs::create_dir_all(&d); d }
fn epoch() -> u64 { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

/// stdout of a command, trimmed; empty on any failure.
pub fn out(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}
fn now_str() -> String { chrono::Local::now().format("%F %T").to_string() }
fn append(path: &Path, line: &str) { if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) { let _ = writeln!(f, "{line}"); } }

/// True when something accepts connections on 127.0.0.1:`port` (OpenD listens on 11111).
pub fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_secs(2)).is_ok()
}

/// Posts to Discord through the separately installed, already-approved binary (this one never touches the network itself).
pub fn discord(title: &str, body: &str) {
    let bin = platform::tool("qwen-notify");
    if bin.exists() { let _ = Command::new(bin).args(["--title", title, body]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn(); }
}

// ---------------------------------------------------------------- parsing (pure, tested)
/// `{ 2.38 2.27 2.30 }` -> 2.38
pub fn parse_load(s: &str) -> f64 { s.split_whitespace().find_map(|t| t.parse::<f64>().ok()).unwrap_or(0.0) }
pub use crate::platform::{parse_memfree, parse_swap_mb};
/// pid of a launchd job from `launchctl list` (`Some("-")` = loaded but not running, None = not loaded)
pub fn launchd_pid(list: &str, label: &str) -> Option<String> {
    list.lines().find_map(|l| { let f: Vec<&str> = l.split_whitespace().collect(); (f.len() >= 3 && f[2] == label).then(|| f[0].to_string()) })
}

/// last exit status of a launchd job from `launchctl list` (second column)
pub fn launchd_status(list: &str, label: &str) -> Option<i32> {
    list.lines().find_map(|l| { let f: Vec<&str> = l.split_whitespace().collect(); (f.len() >= 3 && f[2] == label).then(|| f[1].parse().ok()).flatten() })
}
/// EX_CONFIG (78): launchd refuses to spawn the job. Seen after a binary was re-signed (stale launch constraints); a plain kickstart cannot fix it,
/// the job must be unloaded and loaded again.
pub fn needs_reregister(status: Option<i32>) -> bool { status == Some(78) }

// ---------------------------------------------------------------- decisions (pure, tested)

#[derive(Debug, Clone)]
pub struct Sample { pub load: f64, pub memfree: Option<u32>, pub swap_mb: u64, pub disk_gb: u64 }

impl Sample {
    pub fn line(&self, ts: &str) -> String {
        format!("{ts} load={:.2} memfree={}% swap={}.00M disk_free={}GB", self.load, self.memfree.map(|m| m.to_string()).unwrap_or_default(), self.swap_mb, self.disk_gb)
    }
    pub fn alerts(&self) -> Vec<String> {
        let mut a = Vec::new();
        if self.load >= 20.0 { a.push(format!("load {:.2}", self.load)); }
        if let Some(m) = self.memfree { if m < 10 { a.push(format!("memory free {m}%")); } }
        if self.swap_mb >= SWAP_ALERT_MB { a.push(format!("swap {}.00M", self.swap_mb)); }
        if self.disk_gb < 20 { a.push(format!("disk free {}GB", self.disk_gb)); }
        a
    }
}

// ---------------------------------------------------------------- state: "how long since I last did X"
fn since(key: &str) -> u64 { fs::read_to_string(state().join(key)).ok().and_then(|s| s.trim().parse::<u64>().ok()).map(|t| epoch().saturating_sub(t)).unwrap_or(999_999) }
fn mark(key: &str) { let _ = fs::write(state().join(key), epoch().to_string()); }
fn read_num(key: &str) -> Option<u64> { fs::read_to_string(state().join(key)).ok().and_then(|s| s.trim().parse().ok()) }
fn write_num(key: &str, v: u64) { let _ = fs::write(state().join(key), v.to_string()); }

fn act(msg: &str) {
    append(&logs().join("heal.log"), &format!("{} {msg}", now_str()));
    discord("Auto-heal", msg);
}
fn kick(label: &str) { service::restart(label); }

fn disk_free_gb() -> u64 { platform::disk_free_gb(&lab()).or_else(|| platform::disk_free_gb(&home())).unwrap_or(0.0) as u64 }

pub fn take_sample() -> Sample {
    Sample {
        load: platform::load1().unwrap_or(0.0),
        memfree: platform::mem_free_percent(),
        swap_mb: platform::swap_used_mb(),
        disk_gb: disk_free_gb(),
    }
}

fn trim_log(p: &Path, keep: usize) {
    if let Ok(s) = fs::read_to_string(p) { let l: Vec<&str> = s.lines().collect(); if l.len() > keep + 500 { let _ = fs::write(p, l[l.len() - keep..].join("\n") + "\n"); } }
}

// ---------------------------------------------------------------- watch = sample + alerts + heal
pub fn watch() {
    let s = take_sample();
    let ts = now_str(); let lg = logs();
    append(&lg.join("resource.log"), &s.line(&ts));
    for a in s.alerts() { append(&lg.join("alerts.log"), &format!("{ts} ALERT {a}")); }
    for f in ["resource.log", "alerts.log"] { trim_log(&lg.join(f), 5000); }
    heal();
}

/// The job label of a service name used by `heal`: `bot` is the SPY bot's job, the rest are the lab's.
fn heal_label(j: &str) -> String { if j == "bot" { "com.claudemoomoobot.bot".to_string() } else { format!("com.qwenlab.{j}") } }

fn heal() {
    let lg = logs();
    // 1. KeepAlive services loaded but not running, or not loaded
    for j in ["bot", "dash", "improve", "brain", "trainer"] {
        let label = heal_label(j);
        let st = service::state(&label);
        if st.loaded && !st.running && since(&format!("svc-{j}")) > 600 {
            if platform::os() == Os::Mac && needs_reregister(st.last_exit) {
                service::unload(&label);
                for _ in 0..6 { if service::load(&label) { break; } std::thread::sleep(std::time::Duration::from_secs(2)); }
                mark(&format!("svc-{j}")); act(&format!("FIX {j} could not be spawned (launchd EX_CONFIG 78, usually after a re-signed binary): unloaded and loaded again"));
            } else {
                service::restart(&label); mark(&format!("svc-{j}")); act(&format!("FIX {j} was not running: restarted"));
            }
        } else if !st.loaded && since(&format!("svc-{j}")) > 600 {
            let ok = service::load(&label); mark(&format!("svc-{j}"));
            act(&format!("FIX {j} was not loaded: {}", if ok { "loaded again" } else { "load failed (is it installed? `qwen-ops install`)" }));
        }
    }
    // 2. the brain server is running but has written nothing for 45 minutes (hung): restart it
    let age = fs::metadata(lg.join("brain.log")).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
    if service::state(jobs::BRAIN).running && age.is_some_and(|a| a > 2700) && since("brain-kick") > 2700 {
        kick(jobs::BRAIN); mark("brain-kick"); act(&format!("FIX brain log idle {}s: restarted the brain server", age.unwrap_or(0)));
    }
}

// ---------------------------------------------------------------- watchdog: free while healthy; one Claude fixer when trouble persists
pub fn watchdog() {
    let lg = logs(); let w = lg.join("watchdog.log"); let now = epoch();
    let mut probs: Vec<String> = vec![];
    // the brain server must be producing: its log is written by every role run (one every ten minutes)
    let brain_age = fs::metadata(lg.join("brain.log")).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs());
    if brain_age.is_none_or(|a| a > 3600) { probs.push(format!("the brain server has not run a role for {} min", brain_age.map_or(0, |a| a / 60))); }
    let alerts = fs::read_to_string(lg.join("alerts.log")).unwrap_or_default(); let al = alerts.lines().count() as u64;
    let pal = read_num("alerts").unwrap_or(al); write_num("alerts", al);
    if al > pal { probs.push(format!("new resource alerts: {}", alerts.lines().skip(pal as usize).collect::<Vec<_>>().join(";"))); }
    let sw = platform::swap_used_mb(); if sw > SWAP_ALERT_MB { probs.push(format!("swap {sw}MB")); }
    if !port_open(11111) { probs.push("OpenD port 11111 down".into()); }
    if probs.is_empty() { append(&w, &format!("{} ok", now_str())); trim_log(&w, 2000); return; }
    append(&w, &format!("{} PROBLEMS: {}", now_str(), probs.join(" ")));
    if lab().join("MAINTENANCE").exists() { return; }
    // OpenD down is not fixable by a headless session: the moomoo app needs a human login. Alert (at most every 6 h), no Claude session.
    if probs.iter().all(|p| p.starts_with("OpenD port")) {
        if now.saturating_sub(read_num("opend-alert").unwrap_or(0)) >= 6 * 3600 {
            write_num("opend-alert", now);
            discord("Watchdog: OpenD is down", "moomoo OpenD (127.0.0.1:11111) is not reachable; it needs a manual start and login. No repair session started: none can log in.");
        }
        append(&w, &format!("{} OpenD down needs a manual login: no Claude fixer started", now_str()));
        return;
    }
    if now.saturating_sub(read_num("claude-fix").unwrap_or(0)) < 1800 { append(&w, &format!("{} fix session ran <30 min ago", now_str())); return; }
    write_num("claude-fix", now);
    let joined = probs.join(" ");
    discord("Watchdog: Claude fixer started", &joined);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M").to_string(); let report = lg.join(format!("watchdog-fix-{stamp}.md"));
    let prompt = format!("The resource watchdog found: {joined}.\nContext: this {} machine ({} cores) runs the SPY bot, Rust lab jobs, and the checked online ensemble described in docs/ONLINE_AI.md. Fixes already applied and incident history are in docs/RESOURCE_SAFETY.md. Tools: `qwen-ops health`, logs/brain.log, logs/trainer.log, logs/improve.log, logs/heal.log, logs/alerts.log, brain/reports/*.md.\nRun `qwen-ops health`, find the ROOT CAUSE and FIX it. Back up config before editing. Rules: production automation stays Rust; never ask questions; real data only; never change provider access controls; never revert changes you did not make; never write the Discord webhook anywhere; do not touch trading logic. End with 3 lines: cause, fix, how to verify.", platform::os_name(), platform::ncpu());
    let os_tools = match platform::os() {
        Os::Mac => "Bash(launchctl:*) Bash(top:*) Bash(sysctl:*) Bash(vm_stat:*) Bash(pgrep:*) Bash(nc:*)",
        Os::Linux => "Bash(systemctl:*) Bash(journalctl:*) Bash(free:*) Bash(uptime:*) Bash(pgrep:*)",
        Os::Windows => "Bash(schtasks:*) Bash(tasklist:*) Bash(powershell:*)",
    };
    let allowed = format!("Read Edit Write Grep Glob Bash(tail:*) Bash(grep:*) Bash(ls:*) Bash(curl:*) Bash(cat:*) Bash(sed:*) Bash(head:*) Bash(cp:*) Bash(date:*) Bash(wc:*) Bash(ps:*) Bash(qwen-ops:*) {os_tools}");
    let outf = fs::File::create(&report).ok();
    let Some(claude) = platform::claude_bin() else {
        append(&w, &format!("{} FAULT: the claude CLI was not found (install it: docs/SETUP_*.md)", now_str()));
        return;
    };
    let mut cmd = platform::command_for(&claude);
    cmd.current_dir(lab()).args(["-p", &prompt, "--permission-mode", "acceptEdits"]);
    if platform::os() == Os::Mac { cmd.arg("--add-dir").arg(home().join("Library/LaunchAgents")); }
    cmd.args(["--allowedTools", &allowed]).stdin(Stdio::null());
    if let Some(f) = outf {
        match f.try_clone() { Ok(g) => { cmd.stdout(g); } Err(_) => { cmd.stdout(Stdio::null()); } }
        cmd.stderr(f);
    }
    let code = cmd.status().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    append(&w, &format!("{} fix session finished (exit {code}): logs/watchdog-fix-{stamp}.md", now_str()));
    let tail: String = fs::read_to_string(&report).unwrap_or_default().chars().take(900).collect();
    discord("Watchdog: Claude fixer finished", &tail);
}

// ---------------------------------------------------------------- health: one-shot report
pub fn health() {
    let lg = logs();
    println!("== now {} on {} ({} cores)", now_str(), platform::os_name(), platform::ncpu());
    println!("load {}  memory free: {}%  swap: {} MB", platform::load1().map_or("?".into(), |l| format!("{l:.2}")), platform::mem_free_percent().map(|m| m.to_string()).unwrap_or("?".into()), platform::swap_used_mb());
    println!("== services");
    for label in jobs::ALL.iter().map(|s| s.to_string()).chain(["com.claudemoomoobot.bot".to_string()]) {
        let st = service::state(&label);
        if st.loaded && st.last_exit.is_some_and(|c| c != 0) && !st.running { println!("NONZERO EXIT: {label} ({})", st.last_exit.unwrap_or(0)); }
        let short = label.rsplit('.').next().unwrap_or(&label);
        println!("{short}: {}", if !st.loaded { "NOT LOADED".to_string() } else if st.running { format!("running{}", st.pid.map(|p| format!(" pid {p}")).unwrap_or_default()) } else { "loaded, idle".to_string() });
    }
    println!("OpenD :11111 {}", if port_open(11111) { "up" } else { "DOWN" });
    println!("== last 6 samples");
    let rl = fs::read_to_string(lg.join("resource.log")).unwrap_or_default();
    for l in rl.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev() { println!("{l}"); }
    println!("== alerts (last 10)");
    let al = fs::read_to_string(lg.join("alerts.log")).unwrap_or_default();
    if al.trim().is_empty() { println!("none"); } else { for l in al.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev() { println!("{l}"); } }
    println!("== the deep-learning trainer (last 4 lines)");
    let tl = fs::read_to_string(lg.join("trainer.log")).unwrap_or_default();
    if tl.trim().is_empty() { println!("none yet"); } else { for l in tl.lines().rev().take(4).collect::<Vec<_>>().into_iter().rev() { println!("{}", l.chars().take(260).collect::<String>()); } }
    println!("== the brain (last 8 role runs)");
    let bl = fs::read_to_string(lg.join("brain.log")).unwrap_or_default();
    if bl.trim().is_empty() { println!("none yet"); } else { for l in bl.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev() { println!("{}", l.chars().take(200).collect::<String>()); } }
    let mem = lab().join("memory/memory.sqlite3");
    if let Ok(c) = rusqlite::Connection::open_with_flags(&mem, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        let ml: Option<String> = c.query_row("SELECT text FROM brain_facts WHERE role = 'statistician' AND kind = 'ml-watch' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
        println!("== ML for trading (pre-registered daily test): {}", ml.unwrap_or_else(|| "not run yet".into()));
        let cm: Option<String> = c.query_row("SELECT text FROM brain_facts WHERE role = 'statistician' AND kind = 'candle-ml' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
        println!("== ML on all candles (pre-registered daily test): {}", cm.unwrap_or_else(|| "not run yet".into()));
        let ci: Option<String> = c.query_row("SELECT text FROM brain_facts WHERE role = 'statistician' AND kind = 'candle-improve' ORDER BY id DESC LIMIT 1", [], |r| r.get(0)).ok();
        println!("== ML improvement round (daily, ledger-adjusted): {}", ci.unwrap_or_else(|| "not run yet".into()));
        let q: Result<(i64, i64), _> = c.query_row("SELECT count(*), coalesce(sum(queued), 0) FROM brain_proposals", [], |r| Ok((r.get(0)?, r.get(1)?)));
        if let Ok((n, queued)) = q { println!("== proposals so far: {n} made, {queued} queued (qwen-lab brain report compares the policies)"); }
    }
    println!("== auto-heal actions (last 10)");
    let hs = fs::read_to_string(lg.join("heal.log")).unwrap_or_default();
    if hs.trim().is_empty() { println!("none"); } else { for l in hs.lines().rev().take(10).collect::<Vec<_>>().into_iter().rev() { println!("{l}"); } }
    if platform::os() == Os::Mac {
        println!("== recent crash/diag reports");
        let mut reps: Vec<(std::time::SystemTime, String)> = fs::read_dir("/Library/Logs/DiagnosticReports").into_iter().flatten().flatten().filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.file_name().to_string_lossy().into_owned()))).collect();
        reps.sort_by_key(|r| std::cmp::Reverse(r.0)); for (_, n) in reps.iter().take(3) { println!("{n}"); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_system_tool_output() {
        assert_eq!(parse_load("{ 2.38 2.27 2.30 }"), 2.38);
        assert_eq!(parse_memfree("blah\nSystem-wide memory free percentage: 47%\n"), Some(47));
        assert_eq!(parse_swap_mb("total = 2048.00M  used = 1016.69M  free = 1031.31M  (encrypted)"), 1016);
        assert_eq!(parse_swap_mb("total = 0.00M  used = 0.00M  free = 0.00M"), 0);
    }

    #[test]
    fn launchctl_pid_lookup() {
        let l = "PID\tStatus\tLabel\n1820\t0\tcom.qwenlab.brain\n-\t0\tcom.qwenlab.train\n";
        assert_eq!(launchd_pid(l, "com.qwenlab.brain").as_deref(), Some("1820"));
        assert_eq!(launchd_pid(l, "com.qwenlab.train").as_deref(), Some("-"));
        assert_eq!(launchd_pid(l, "com.qwenlab.nope"), None);
    }

    #[test]
    fn exit_78_means_reload_not_kickstart() {
        let l = "PID\tStatus\tLabel\n-\t78\tcom.qwenlab.improve\n-\t0\tcom.qwenlab.tick\n123\t0\tcom.qwenlab.dash\n";
        assert_eq!(launchd_status(l, "com.qwenlab.improve"), Some(78));
        assert!(needs_reregister(launchd_status(l, "com.qwenlab.improve")));
        assert!(!needs_reregister(launchd_status(l, "com.qwenlab.tick")) && !needs_reregister(launchd_status(l, "com.qwenlab.nope")));
    }

    #[test]
    fn alerts_fire_at_the_documented_thresholds() {
        let ok = Sample { load: 3.0, memfree: Some(60), swap_mb: 900, disk_gb: 80 };
        assert!(ok.alerts().is_empty(), "normal operation (swap 900 MB) must not alert");
        let bad = Sample { load: 25.0, memfree: Some(5), swap_mb: 1600, disk_gb: 10 };
        assert_eq!(bad.alerts().len(), 4);
    }

    #[test]
    fn sample_line_keeps_the_log_format() {
        let s = Sample { load: 2.5, memfree: Some(49), swap_mb: 81, disk_gb: 93 };
        assert_eq!(s.line("2026-10-13 10:08:48"), "2026-10-13 10:08:48 load=2.50 memfree=49% swap=81.00M disk_free=93GB");
    }

    #[test]
    fn a_closed_port_is_reported_closed() {
        assert!(!port_open(1), "port 1 has no listener");
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(port_open(l.local_addr().unwrap().port()));
    }
}

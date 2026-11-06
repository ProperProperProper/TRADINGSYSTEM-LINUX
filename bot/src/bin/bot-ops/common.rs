//! Shared helpers of bot-ops: folders, logging, git, locks, tool calls, the market clock, headless Claude sessions.

use crate::platform::{self, Os};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The bot repo: `MOOMOO_BOT_DIR`, else the folder this binary was built from.
pub fn repo() -> PathBuf {
    std::env::var_os("MOOMOO_BOT_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}
/// The lab repo. Combined checkouts keep it below the bot repo; older separate checkouts use the platform default.
pub fn lab_repo() -> PathBuf {
    if let Some(path) = std::env::var_os("QWEN_LAB_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(path);
    }
    let bundled = repo().join("lab");
    if bundled.join("Cargo.toml").is_file() {
        bundled
    } else {
        platform::lab_repo()
    }
}
fn configured_lab_root(config: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(config).ok()?;
    text.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("root")?
            .trim_start()
            .strip_prefix('=')?
            .trim();
        Some(PathBuf::from(value.strip_prefix('"')?.strip_suffix('"')?))
    })
}
/// The lab data folder. Prefer lab.toml so headless jobs do not fall back to an unrelated ~/Projects path.
pub fn lab_root() -> PathBuf {
    std::env::var_os("QWEN_LAB_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| configured_lab_root(&lab_repo().join("lab.toml")))
        .unwrap_or_else(platform::lab_root)
}
/// The bot's runtime folder (installed binary, logs, paper state).
pub fn runtime() -> PathBuf {
    platform::bot_state_dir()
}
/// `<repo>/ops/logs` (not committed).
pub fn ops_logs() -> PathBuf {
    let d = repo().join("ops/logs");
    let _ = fs::create_dir_all(&d);
    d
}
pub const BOT_LABEL: &str = "com.claudemoomoobot.bot";

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub fn utc_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Appends `<utc time> <msg>` to a log file.
pub fn log(path: &Path, msg: &str) {
    if let Some(d) = path.parent() {
        let _ = fs::create_dir_all(d);
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {msg}", utc_stamp());
    }
}

/// `git -C repo <args>`: stdout (trimmed) when it succeeds.
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    platform::git(repo, args)
}
/// Whether the work tree has uncommitted changes.
pub fn dirty(repo: &Path) -> bool {
    git(repo, &["status", "--porcelain"]).is_none_or(|s| !s.is_empty())
}

fn tool_command(name: &str) -> Command {
    let mut cmd = Command::new(platform::tool(name));
    // qwen-tools is built by the lab but is also called by bot-owned scheduled
    // jobs. Those jobs do not inherit an interactive shell's path overrides, so
    // give every helper the same resolved roots bot-ops itself is using.
    cmd.env("MOOMOO_BOT_DIR", repo())
        .env("QWEN_LAB_DIR", lab_repo())
        .env("QWEN_LAB_ROOT", lab_root());
    cmd
}

/// Runs a helper tool (`qwen-tools`, `qwen-notify`, ...) from the tools folder with optional stdin; returns (success, stdout+stderr).
pub fn tool(name: &str, args: &[&str], stdin: Option<&str>) -> (bool, String) {
    let mut cmd = tool_command(name);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let Ok(mut child) = cmd.spawn() else {
        return (
            false,
            format!("{name} is not installed (qwen-ops install-tools in the lab repo)"),
        );
    };
    if let (Some(text), Some(mut si)) = (stdin, child.stdin.take()) {
        let _ = si.write_all(text.as_bytes());
    }
    match child.wait_with_output() {
        Ok(o) => (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        ),
        Err(e) => (false, e.to_string()),
    }
}

/// Discord notice through qwen-notify (redacted; no-op without a webhook).
pub fn discord(title: &str, body: &str) {
    let bin = platform::tool("qwen-notify");
    if bin.exists() {
        let _ = Command::new(bin)
            .args(["--title", title, body])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// OPEN when New York time is within `from..=to` (HHMM) on a weekday (holidays count as open: a job only waits).
pub fn market_window_open(from: u32, to: u32) -> bool {
    let (d, h) = platform::new_york_clock(now());
    d <= 5 && (from..=to).contains(&h)
}

/// The market state word of `qwen-tools market` (OPEN / CLOSED / ...).
pub fn market_state() -> String {
    tool("qwen-tools", &["market"], None).1.trim().to_string()
}

/// A directory used as a lock: created atomically; a lock older than `stale_min` minutes is taken over. None when someone else holds it.
pub struct Lock(PathBuf);
impl Lock {
    pub fn take(dir: &Path, stale_min: u64) -> Option<Lock> {
        if let Some(p) = dir.parent() {
            let _ = fs::create_dir_all(p);
        }
        if fs::create_dir(dir).is_ok() {
            return Some(Lock(dir.to_path_buf()));
        }
        let age = now() - platform::mtime(dir);
        if age > (stale_min * 60) as i64 {
            let _ = fs::remove_dir_all(dir);
            return fs::create_dir(dir).ok().map(|_| Lock(dir.to_path_buf()));
        }
        None
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// A headless Claude Code session: `claude -p <prompt> ...` run in `cwd`, output to `out`, killed after `limit`. Returns the exit code (142 on timeout, -1 if it
/// could not start). `allowed` is the `--allowedTools` list.
pub struct Session<'a> {
    pub prompt: &'a str,
    pub cwd: &'a Path,
    pub add_dirs: Vec<PathBuf>,
    pub allowed: Vec<String>,
    pub model: Option<&'a str>,
    pub out: &'a Path,
    pub limit: Duration,
}

/// The per-OS shell tool allowances every session gets.
pub fn os_tools() -> &'static str {
    match platform::os() {
        Os::Mac => "Bash(launchctl:*)",
        Os::Linux => "Bash(systemctl:*)",
        Os::Windows => "Bash(schtasks:*)",
    }
}

pub fn run_session(s: &Session) -> i32 {
    let Some(claude) = platform::claude_bin() else {
        return -1;
    };
    let mut cmd = platform::command_for(&claude);
    cmd.current_dir(s.cwd).args(["-p", s.prompt]);
    if let Some(m) = s.model {
        cmd.args(["--model", m, "--no-session-persistence"]);
    }
    cmd.args(["--permission-mode", "acceptEdits"]);
    for d in &s.add_dirs {
        cmd.arg("--add-dir").arg(d);
    }
    cmd.arg("--allowedTools")
        .args(&s.allowed)
        .stdin(Stdio::null());
    if let Ok(f) = fs::File::create(s.out) {
        match f.try_clone() {
            Ok(g) => {
                cmd.stdout(g);
            }
            Err(_) => {
                cmd.stdout(Stdio::null());
            }
        }
        cmd.stderr(f);
    }
    let start = Instant::now();
    match cmd.spawn() {
        Ok(mut child) => loop {
            match child.try_wait() {
                Ok(Some(st)) => return st.code().unwrap_or(-1),
                Ok(None) if start.elapsed() > s.limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return 142;
                }
                Ok(None) => std::thread::sleep(Duration::from_secs(2)),
                Err(_) => return -1,
            }
        },
        Err(_) => -1,
    }
}

/// True when another headless Claude session (`claude -p`) is running.
pub fn another_session_running() -> bool {
    !platform::processes("claude -p").is_empty() || !platform::processes("claude.exe -p").is_empty()
}

/// True when the owner's MAINTENANCE flag in the lab folder is younger than 6 hours.
pub fn maintenance() -> bool {
    let m = lab_root().join("MAINTENANCE");
    platform::mtime(&m) > 0 && now() - platform::mtime(&m) < 360 * 60
}

/// Minimal HTTP GET of `127.0.0.1:port/path` (the console and the lab dashboard); empty on any problem.
pub fn http_get(port: u16, path: &str) -> String {
    use std::io::Read;
    let Ok(mut s) = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_secs(3),
    ) else {
        return String::new();
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    if s.write_all(
        format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .is_err()
    {
        return String::new();
    }
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).to_string();
    text.split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default()
}

/// Whether OpenD accepts connections on 127.0.0.1:11111.
pub fn opend_up() -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], 11111)),
        Duration::from_secs(2),
    )
    .is_ok()
}

/// SHA-256 of a file as lowercase hex (None when unreadable).
pub fn sha256_file(p: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(p).ok()?;
    Some(hex(&Sha256::digest(&bytes)))
}
pub fn sha256_text(t: &str) -> String {
    use sha2::{Digest, Sha256};
    hex(&Sha256::digest(t.as_bytes()))
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Copies `new` over `target`; a running executable cannot be overwritten on Windows but can be renamed, so the old file is renamed aside first.
pub fn replace_file(new: &Path, target: &Path) -> std::io::Result<()> {
    let old = target.with_file_name(format!(
        "{}.old",
        target.file_name().and_then(|n| n.to_str()).unwrap_or("old")
    ));
    if target.exists() {
        let _ = fs::remove_file(&old);
        if fs::rename(target, &old).is_err() {
            fs::remove_file(target)?;
        }
    }
    fs::rename(new, target)?;
    let _ = fs::remove_file(&old);
    Ok(())
}

/// The newest modification time (unix seconds) among files.
pub fn newest_mtime(files: &[PathBuf]) -> i64 {
    files.iter().map(|f| platform::mtime(f)).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_tools_receive_every_resolved_project_root() {
        let cmd = tool_command("qwen-tools");
        let env: std::collections::BTreeMap<_, _> = cmd
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert_eq!(
            env.get("MOOMOO_BOT_DIR"),
            Some(&repo().display().to_string())
        );
        assert_eq!(
            env.get("QWEN_LAB_DIR"),
            Some(&lab_repo().display().to_string())
        );
        assert_eq!(
            env.get("QWEN_LAB_ROOT"),
            Some(&lab_root().display().to_string())
        );
    }

    #[test]
    fn lab_root_is_read_from_the_lab_config_without_shell_environment() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("lab.toml");
        std::fs::write(
            &config,
            "root = \"/var/tmp/qwen-lab-state\"\nmode = \"real\"\n",
        )
        .unwrap();
        assert_eq!(
            configured_lab_root(&config),
            Some(PathBuf::from("/var/tmp/qwen-lab-state"))
        );
    }

    /// Test: the lock is exclusive and released on drop.
    #[test]
    fn a_lock_is_exclusive() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l.lock");
        let a = Lock::take(&p, 10).unwrap();
        assert!(Lock::take(&p, 10).is_none());
        drop(a);
        assert!(Lock::take(&p, 10).is_some());
    }

    /// Test: hashes match the known SHA-256 of "abc".
    #[test]
    fn sha256_is_correct() {
        assert_eq!(
            sha256_text("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// Test: a local HTTP GET returns the body.
    #[test]
    fn http_get_reads_the_body() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::Read;
            if let Ok((mut s, _)) = l.accept() {
                let mut b = [0u8; 256];
                let _ = s.read(&mut b);
                let _ = s.write_all(b"HTTP/1.0 200 OK\r\n\r\n{\"ok\":1}");
            }
        });
        assert_eq!(http_get(port, "/api/x"), "{\"ok\":1}");
    }

    /// Test: the market window follows New York time (a Saturday is closed).
    #[test]
    fn market_window_is_a_weekday_window() {
        let _ = market_window_open(900, 1630);
        assert!(!market_window_open(2500, 2500));
    }
}

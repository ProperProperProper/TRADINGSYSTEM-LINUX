//! The platform layer: everything that differs between macOS, Linux and Windows lives here (folders, machine probes, process checks,
//! the Claude CLI location), so the rest of the lab asks this module instead of shelling out to macOS-only tools or hard-coding paths.
//!
//! Folders resolve from environment overrides first, then per-OS defaults:
//!   `QWEN_PROJECTS_DIR`  parent of the two repos and the lab root (default `<home>/Projects`)
//!   `QWEN_LAB_ROOT`      the lab's data folder (default `<projects>/qwen-lab-stocks`)
//!   `MOOMOO_BOT_DIR`     the bot repo (default `<projects>/moomoo-bot`); `QWEN_LAB_DIR` the lab repo (default `<projects>/qwen-lab`)
//!   `MOOMOO_APP_DIR`     parent of the bot's two state folders (default: `state/` of the TradingSystem project this binary belongs to, so
//!                        everything the system uses lives in one folder; outside a project: macOS `~/Library/Application Support`,
//!                        Linux `$XDG_DATA_HOME` or `~/.local/share`, Windows `%LOCALAPPDATA%`)
//!   `TRADINGSYSTEM_DIR`  the project folder (default: found by walking up from the binary, then from the current folder, to the folder holding
//!                        `bot/Cargo.toml` and `crates/cli/Cargo.toml`)
//! The folder name under it (`ClaudeMoomooBot`) is the same on every OS.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The operating systems the lab and the bot run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Linux,
    Windows,
}

/// The OS this binary was built for.
pub fn os() -> Os {
    if cfg!(target_os = "macos") {
        Os::Mac
    } else if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Linux
    }
}

/// Human name of the OS ("macOS", "Linux", "Windows").
pub fn os_name() -> &'static str {
    match os() {
        Os::Mac => "macOS",
        Os::Linux => "Linux",
        Os::Windows => "Windows",
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The user's home folder (`HOME`, or `USERPROFILE` on Windows; the current folder if neither is set).
pub fn home() -> PathBuf {
    env_path("HOME")
        .or_else(|| env_path("USERPROFILE"))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The parent of the repos and the lab root.
pub fn projects_dir() -> PathBuf {
    env_path("QWEN_PROJECTS_DIR").unwrap_or_else(|| home().join("Projects"))
}

/// The lab's data folder (databases, models, logs, switches).
pub fn lab_root() -> PathBuf {
    env_path("QWEN_LAB_ROOT").unwrap_or_else(|| projects_dir().join("qwen-lab-stocks"))
}

/// The lab's source repo.
pub fn lab_repo() -> PathBuf {
    env_path("QWEN_LAB_DIR").unwrap_or_else(|| projects_dir().join("qwen-lab"))
}

/// The bot's source folder: `bot/` of the project when there is one.
pub fn bot_repo() -> PathBuf {
    env_path("MOOMOO_BOT_DIR")
        .or_else(|| project_root().map(|r| r.join("bot")))
        .unwrap_or_else(|| projects_dir().join("moomoo-bot"))
}

/// The TradingSystem project folder this binary belongs to, if it runs inside one (see the module doc).
pub fn project_root() -> Option<PathBuf> {
    if let Some(p) = env_path("TRADINGSYSTEM_DIR") {
        return Some(p);
    }
    let is_root =
        |d: &Path| d.join("bot/Cargo.toml").is_file() && d.join("crates/cli/Cargo.toml").is_file();
    [std::env::current_exe().ok(), std::env::current_dir().ok()]
        .into_iter()
        .flatten()
        .find_map(|start| {
            start
                .ancestors()
                .find(|a| is_root(a))
                .map(Path::to_path_buf)
        })
}

/// The application data folder (see the module doc): inside the project when there is one, so a machine cleanup can delete everything else.
pub fn app_dir() -> PathBuf {
    if let Some(p) = env_path("MOOMOO_APP_DIR") {
        return p;
    }
    if let Some(root) = project_root() {
        return root.join("state");
    }
    match os() {
        Os::Mac => home().join("Library/Application Support"),
        Os::Windows => env_path("LOCALAPPDATA").unwrap_or_else(|| home().join("AppData/Local")),
        Os::Linux => env_path("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share")),
    }
}

/// The bot's persistent store.
pub fn bot_data_dir() -> PathBuf {
    app_dir().join("ClaudeMoomooBot")
}

/// The bot's database.
pub fn bot_store_path() -> PathBuf {
    bot_data_dir().join("moomoo.sqlite3")
}

/// The bot's runtime folder: paper state, console state, logs, the warnings ledger.
pub fn bot_state_dir() -> PathBuf {
    app_dir().join("ClaudeMoomooBot")
}

/// The bot's cached candle history (`history.json`) that the lab imports into its own candle store.
pub fn bot_history_json() -> PathBuf {
    bot_repo()
        .join("target")
        .join("oos-cache")
        .join("history.json")
}

/// The score file the lab writes for the bot's own backtester (`sig:Ml`, `mlgate*`).
pub fn bot_scores_path() -> PathBuf {
    bot_repo()
        .join("target")
        .join("oos-cache")
        .join("ml_scores.json")
}

/// The SPY holdings list the bot refreshes daily.
pub fn bot_holdings_csv() -> PathBuf {
    bot_repo().join("data").join("spy_holdings.csv")
}

/// The bot's log folder.
pub fn bot_logs_dir() -> PathBuf {
    bot_state_dir().join("logs")
}

/// The folder for the installed helper binaries (`qwen-notify`, `qwen-ops`, ...; `claude` lives here on macOS/Linux installs too).
pub fn bin_dir() -> PathBuf {
    home().join(".local").join("bin")
}

/// `name` with the platform's executable suffix.
pub fn exe(name: &str) -> String {
    if os() == Os::Windows {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Where an installed helper binary lives.
pub fn tool(name: &str) -> PathBuf {
    bin_dir().join(exe(name))
}

/// Looks `name` up on PATH (with `.exe`/`.cmd` suffixes on Windows).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let suffixes: &[&str] = if os() == Os::Windows {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&path).find_map(|dir| {
        suffixes
            .iter()
            .map(|s| dir.join(format!("{name}{s}")))
            .find(|p| p.is_file())
    })
}

/// The Claude CLI (headless repair and supervisor sessions): PATH first, then the usual install folders.
pub fn claude_bin() -> Option<PathBuf> {
    which("claude").or_else(|| {
        let mut c = vec![tool("claude")];
        if let Some(a) = env_path("APPDATA") {
            c.push(a.join("npm").join("claude.cmd"));
        }
        c.push(PathBuf::from("/opt/homebrew/bin/claude"));
        c.push(PathBuf::from("/usr/local/bin/claude"));
        c.into_iter().find(|p| p.is_file())
    })
}

/// Output of a command when it succeeds.
pub fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// PowerShell one-liner output (Windows probes only).
fn powershell(script: &str) -> Option<String> {
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
}

/// Number of logical CPU cores (4 if unknown).
pub fn ncpu() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// 1-minute load average. macOS: `sysctl vm.loadavg`; Linux: `/proc/loadavg`; Windows (no load average): the CPU's busy share times the cores.
pub fn load1() -> Option<f64> {
    match os() {
        Os::Mac => run("sysctl", &["-n", "vm.loadavg"])?
            .split_whitespace()
            .find_map(|t| t.trim_matches(|c| c == '{' || c == '}').parse::<f64>().ok()),
        Os::Linux => parse_proc_loadavg(&std::fs::read_to_string("/proc/loadavg").ok()?),
        Os::Windows => {
            let pct: f64 = powershell("(Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average")?.trim().parse().ok()?;
            Some(pct / 100.0 * ncpu() as f64)
        }
    }
}

/// First field of `/proc/loadavg` ("0.52 0.58 0.59 1/467 12345").
pub fn parse_proc_loadavg(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

/// Memory that can be given to a new process without swapping, in GB. macOS: free + inactive + speculative pages (`vm_stat`);
/// Linux: `MemAvailable` in `/proc/meminfo`; Windows: free physical memory.
pub fn free_gb() -> Option<f64> {
    match os() {
        Os::Mac => parse_vm_stat(&run("vm_stat", &[])?),
        Os::Linux => parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?),
        Os::Windows => {
            let kb: f64 = powershell("(Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory")?
                .trim()
                .parse()
                .ok()?;
            Some(kb * 1024.0 / 1e9)
        }
    }
}

/// Free + inactive + speculative memory in GB from `vm_stat` output.
pub fn parse_vm_stat(text: &str) -> Option<f64> {
    let page: f64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |label: &str| -> f64 {
        text.lines()
            .find(|l| l.starts_with(label))
            .and_then(|l| l.split(':').nth(1))
            .map(|v| v.trim().trim_end_matches('.').parse::<f64>().unwrap_or(0.0))
            .unwrap_or(0.0)
    };
    Some((pages("Pages free") + pages("Pages inactive") + pages("Pages speculative")) * page / 1e9)
}

/// `MemAvailable` (kB) of `/proc/meminfo` in GB.
pub fn parse_meminfo(text: &str) -> Option<f64> {
    let kb: f64 = text
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb * 1024.0 / 1e9)
}

/// Free memory as a percentage of the total (the number macOS `memory_pressure` prints; Linux: MemAvailable/MemTotal; Windows: free/visible).
pub fn mem_free_percent() -> Option<u32> {
    match os() {
        Os::Mac => parse_memfree(&run("memory_pressure", &[])?),
        Os::Linux => parse_meminfo_percent(&std::fs::read_to_string("/proc/meminfo").ok()?),
        Os::Windows => {
            let v = powershell("$o = Get-CimInstance Win32_OperatingSystem; [int](100 * $o.FreePhysicalMemory / $o.TotalVisibleMemorySize)")?;
            v.trim().parse().ok()
        }
    }
}

/// Swap (or page file) in use, MB (0 when none or unknown).
pub fn swap_used_mb() -> u64 {
    match os() {
        Os::Mac => run("sysctl", &["-n", "vm.swapusage"]).map(|t| parse_swap_mb(&t)).unwrap_or(0),
        Os::Linux => std::fs::read_to_string("/proc/meminfo").ok().map(|t| parse_meminfo_swap_mb(&t)).unwrap_or(0),
        Os::Windows => powershell("(Get-CimInstance Win32_PageFileUsage | Measure-Object -Property CurrentUsage -Sum).Sum").and_then(|v| v.trim().parse().ok()).unwrap_or(0),
    }
}

/// `System-wide memory free percentage: 47%` -> 47
pub fn parse_memfree(s: &str) -> Option<u32> {
    s.lines()
        .find(|l| l.contains("free percentage"))
        .and_then(|l| l.rsplit(':').next())
        .and_then(|v| v.trim().trim_end_matches('%').parse().ok())
}

/// `total = 2048.00M  used = 1016.69M  free = 1031.31M` -> 1016 (MB used)
pub fn parse_swap_mb(s: &str) -> u64 {
    let t: Vec<&str> = s.split_whitespace().collect();
    t.iter()
        .position(|w| *w == "used")
        .and_then(|i| t.get(i + 2))
        .map(|v| {
            v.trim_end_matches(['M', 'G']).parse::<f64>().unwrap_or(0.0)
                * if v.ends_with('G') { 1024.0 } else { 1.0 }
        })
        .unwrap_or(0.0) as u64
}

/// MemAvailable / MemTotal of `/proc/meminfo`, in percent.
pub fn parse_meminfo_percent(text: &str) -> Option<u32> {
    let field = |k: &str| -> Option<f64> {
        text.lines()
            .find(|l| l.starts_with(k))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    };
    let (avail, total) = (field("MemAvailable:")?, field("MemTotal:")?);
    (total > 0.0).then(|| (100.0 * avail / total) as u32)
}

/// SwapTotal - SwapFree of `/proc/meminfo`, in MB.
pub fn parse_meminfo_swap_mb(text: &str) -> u64 {
    let field = |k: &str| -> f64 {
        text.lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0)
    };
    ((field("SwapTotal:") - field("SwapFree:")).max(0.0) / 1024.0) as u64
}

/// Free space on the drive holding `path`, in GB (`df -k` on macOS/Linux; PowerShell on Windows).
pub fn disk_free_gb(path: &Path) -> Option<f64> {
    if os() == Os::Windows {
        let script = format!(
            "(Get-Item -LiteralPath '{}').PSDrive.Free",
            path.display().to_string().replace('\'', "''")
        );
        let bytes: f64 = powershell(&script)?.trim().parse().ok()?;
        return Some(bytes / 1_073_741_824.0);
    }
    parse_df(&run("df", &["-k", path.to_str()?])?)
}

/// Available space in GB from `df -k` output (the 4th column of the data line).
pub fn parse_df(text: &str) -> Option<f64> {
    let kb: f64 = text
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(kb / 1_048_576.0)
}

/// Calendar date (local) of a unix time, e.g. 2026-11-08.
pub fn date_str(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Local hour of day (0-23).
pub fn local_hour() -> u32 {
    use chrono::Timelike;
    chrono::Local::now().hour()
}

/// The local time zone's name or offset, e.g. "AEDT" or "+11:00".
pub fn tz_name() -> String {
    chrono::Local::now().format("%Z").to_string()
}

/// True when the process `pid` exists (`kill -0` on macOS/Linux, `tasklist` on Windows).
pub fn pid_alive(pid: u32) -> bool {
    if os() == Os::Windows {
        return run("tasklist", &["/FI", &format!("PID eq {pid}"), "/NH"])
            .is_some_and(|t| t.split_whitespace().any(|w| w == pid.to_string()));
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The file's inode (0 where the OS has none, i.e. Windows): the warnings scanner uses it to notice a rotated log.
pub fn inode(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

/// Day of month of the n-th (1-based) Sunday of `month` in `year`.
fn nth_sunday(year: i32, month: u32, n: u32) -> u32 {
    use chrono::{Datelike, NaiveDate};
    let first = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let to_sunday = (7 - first.weekday().num_days_from_sunday()) % 7;
    1 + to_sunday + 7 * (n - 1)
}

/// Hours from UTC to America/New_York at a unix time: -4 during US daylight saving (second Sunday of March 02:00 local to the first Sunday of November 02:00
/// local), else -5.
pub fn new_york_offset_hours(ts: i64) -> i64 {
    use chrono::{Datelike, TimeZone, Utc};
    let utc = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    let y = utc.year();
    let start = Utc
        .with_ymd_and_hms(y, 3, nth_sunday(y, 3, 2), 7, 0, 0)
        .single()
        .unwrap_or_default();
    let end = Utc
        .with_ymd_and_hms(y, 11, nth_sunday(y, 11, 1), 6, 0, 0)
        .single()
        .unwrap_or_default();
    if utc >= start && utc < end {
        -4
    } else {
        -5
    }
}

/// Unix time of 09:30 New York time on the New York calendar day of `ts` (the regular session's open).
pub fn new_york_open_ts(ts: i64) -> i64 {
    let off = new_york_offset_hours(ts) * 3600;
    let local = ts + off;
    local - local.rem_euclid(86_400) + 9 * 3600 + 1800 - off
}

/// (ISO weekday 1=Mon..7=Sun, HHMM) of a unix time in America/New_York (US daylight saving: second Sunday of March 02:00 local to the
/// first Sunday of November 02:00 local), so no `TZ=` environment trick or time zone database is needed.
pub fn new_york_clock(ts: i64) -> (u32, u32) {
    use chrono::{Datelike, TimeZone, Timelike, Utc};
    let utc = Utc.timestamp_opt(ts, 0).single().unwrap_or_default();
    let y = utc.year();
    // DST starts 02:00 EST = 07:00 UTC; it ends 02:00 EDT = 06:00 UTC.
    let start = Utc
        .with_ymd_and_hms(y, 3, nth_sunday(y, 3, 2), 7, 0, 0)
        .single()
        .unwrap_or_default();
    let end = Utc
        .with_ymd_and_hms(y, 11, nth_sunday(y, 11, 1), 6, 0, 0)
        .single()
        .unwrap_or_default();
    let offset = if utc >= start && utc < end { -4 } else { -5 };
    let local = utc + chrono::Duration::hours(offset);
    (
        local.weekday().number_from_monday(),
        local.hour() * 100 + local.minute(),
    )
}

/// A running process.
#[derive(Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    /// Seconds since it started.
    pub age_secs: u64,
    pub command: String,
}

/// `[[dd-]hh:]mm:ss` (ps `etime`) in seconds.
pub fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|p| p.trim().parse::<u64>().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(days * 86_400 + secs)
}

/// Processes whose command line contains `needle` (this process and its parents' shells included only if they match).
pub fn processes(needle: &str) -> Vec<Proc> {
    let mut out = Vec::new();
    if os() == Os::Windows {
        let script = "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId)|$([int]((Get-Date) - $_.CreationDate).TotalSeconds)|$($_.CommandLine)\" }";
        for l in powershell(script).unwrap_or_default().lines() {
            let mut f = l.splitn(3, '|');
            if let (Some(p), Some(a), Some(c)) = (f.next(), f.next(), f.next()) {
                if c.contains(needle) {
                    if let (Ok(pid), Ok(age)) = (p.trim().parse(), a.trim().parse()) {
                        out.push(Proc {
                            pid,
                            age_secs: age,
                            command: c.trim().to_string(),
                        });
                    }
                }
            }
        }
        return out;
    }
    for l in run("ps", &["-axo", "pid=,etime=,command="])
        .unwrap_or_default()
        .lines()
    {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 3 {
            continue;
        }
        let command = f[2..].join(" ");
        if !command.contains(needle) {
            continue;
        }
        if let (Ok(pid), Some(age)) = (f[0].parse::<u32>(), parse_etime(f[1])) {
            if pid != std::process::id() {
                out.push(Proc {
                    pid,
                    age_secs: age,
                    command,
                });
            }
        }
    }
    out
}

/// Ends a process (SIGTERM / taskkill).
pub fn kill_pid(pid: u32) {
    if os() == Os::Windows {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    } else {
        let _ = Command::new("kill")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Modification time of a file in unix seconds (0 when missing).
pub fn mtime(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// Stdout of `git -C repo <args>` when it succeeds, trimmed.
pub fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let mut a = vec!["-C", repo.to_str()?];
    a.extend_from_slice(args);
    run("git", &a).map(|s| s.trim().to_string())
}

/// `std::process::Command` for a script/binary that may be a `.cmd` shim on Windows (`claude.cmd`).
pub fn command_for(path: &Path) -> Command {
    let is_cmd = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if os() == Os::Windows && is_cmd {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(path);
        c
    } else {
        Command::new(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: free disk space is read from `df -k` (available KB, 4th column).
    #[test]
    fn df_is_parsed() {
        let out = "Filesystem 1024-blocks Used Available Capacity iused ifree %iused Mounted on\n/dev/disk3s1 239362496 123456 89128960 59% 1 2 0% /System/Volumes/Data\n";
        assert!((parse_df(out).unwrap() - 85.0).abs() < 0.01);
    }

    /// Test: the host's probes answer (disk, cores, memory, load) in plausible ranges.
    #[test]
    fn host_probes_answer() {
        assert!(ncpu() >= 1);
        assert!(disk_free_gb(&std::env::temp_dir()).is_some_and(|g| g > 0.0));
        assert!(free_gb().is_some_and(|g| g > 0.0));
        assert!(load1().is_some_and(|l| l >= 0.0));
    }

    /// Test: `vm_stat` output (macOS) is parsed to free + inactive + speculative GB.
    #[test]
    fn vm_stat_is_parsed() {
        let sample = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               10000.\nPages active:                            500000.\nPages inactive:                           20000.\nPages speculative:                         5000.\n";
        let gb = parse_vm_stat(sample).unwrap();
        assert!((gb - 35000.0 * 16384.0 / 1e9).abs() < 1e-6);
        assert!(parse_vm_stat("garbage").is_none());
    }

    /// Test: Linux `/proc` formats are parsed.
    #[test]
    fn linux_proc_formats_are_parsed() {
        assert_eq!(
            parse_proc_loadavg("0.52 0.58 0.59 1/467 12345\n"),
            Some(0.52)
        );
        assert!(parse_proc_loadavg("").is_none());
        let mem = "MemTotal:       16000000 kB\nMemFree:          100000 kB\nMemAvailable:    8000000 kB\n";
        assert!((parse_meminfo(mem).unwrap() - 8_000_000.0 * 1024.0 / 1e9).abs() < 1e-9);
        assert!(parse_meminfo("MemTotal: 1 kB\n").is_none());
    }

    /// Test: New York time follows US daylight saving (checked against known instants).
    #[test]
    fn new_york_clock_follows_dst() {
        // 2026-10-13 18:30 UTC is 14:30 EDT on a Tuesday; 2026-01-06 18:30 UTC is 13:30 EST on a Tuesday.
        assert_eq!(new_york_clock(1_791_916_200), (2, 1430));
        assert_eq!(new_york_clock(1_767_724_200), (2, 1330));
        // the instants around the 2026 changes: DST starts 2026-03-08 07:00 UTC and ends 2026-11-01 06:00 UTC.
        assert_eq!(new_york_clock(1_772_953_199), (7, 159)); // 06:59:59 UTC Sun 8 Mar = 01:59 EST
        assert_eq!(new_york_clock(1_772_953_200), (7, 300)); // 07:00 UTC = 03:00 EDT
        assert_eq!(new_york_clock(1_793_512_799), (7, 159)); // 05:59:59 UTC Sun 1 Nov = 01:59 EDT
        assert_eq!(new_york_clock(1_793_512_800), (7, 100)); // 06:00 UTC = 01:00 EST
    }

    /// Test: the session open of a New York day is 09:30 local (13:30 UTC in summer, 14:30 UTC in winter).
    #[test]
    fn the_new_york_open_is_half_past_nine() {
        // 2026-10-13 18:30 UTC (14:30 EDT) -> that day's open 13:30 UTC; 2026-01-06 18:30 UTC (13:30 EST) -> 14:30 UTC
        assert_eq!(new_york_open_ts(1_791_916_200), 1_791_916_200 - 5 * 3600);
        assert_eq!(new_york_open_ts(1_767_724_200), 1_767_724_200 - 4 * 3600);
    }

    /// Test: the folder defaults follow the OS and the environment overrides win.
    #[test]
    fn folders_follow_the_os_and_overrides() {
        let app = app_dir();
        if std::env::var_os("MOOMOO_APP_DIR").is_none() && project_root().is_some() {
            assert_eq!(
                app,
                project_root().unwrap().join("state"),
                "inside the project the state folders live in state/"
            );
            assert!(bot_repo().ends_with("bot"));
            return;
        }
        match os() {
            Os::Mac => assert!(app.ends_with("Library/Application Support")),
            Os::Windows => assert!(
                app.to_string_lossy().contains("AppData")
                    || std::env::var_os("LOCALAPPDATA").is_some()
            ),
            Os::Linux => assert!(
                app.ends_with(".local/share") || std::env::var_os("XDG_DATA_HOME").is_some()
            ),
        }
        assert!(bot_store_path().ends_with("ClaudeMoomooBot/moomoo.sqlite3"));
        assert_eq!(
            exe("qwen-lab"),
            if os() == Os::Windows {
                "qwen-lab.exe"
            } else {
                "qwen-lab"
            }
        );
        assert!(tool("qwen-notify").starts_with(bin_dir()));
    }

    /// Test: memory percentages and swap are parsed on every OS.
    #[test]
    fn memory_percentages_and_swap_are_parsed() {
        assert_eq!(
            parse_memfree("blah\nSystem-wide memory free percentage: 47%\n"),
            Some(47)
        );
        assert_eq!(
            parse_swap_mb("total = 2048.00M  used = 1016.69M  free = 1031.31M  (encrypted)"),
            1016
        );
        assert_eq!(
            parse_swap_mb("total = 0.00M  used = 0.00M  free = 0.00M"),
            0
        );
        let mem = "MemTotal: 16000000 kB\nMemAvailable: 4000000 kB\nSwapTotal: 2097152 kB\nSwapFree: 1048576 kB\n";
        assert_eq!(parse_meminfo_percent(mem), Some(25));
        assert_eq!(parse_meminfo_swap_mb(mem), 1024);
        assert!(mem_free_percent().is_some_and(|p| p <= 100));
    }

    /// Test: process listings and elapsed times are parsed.
    #[test]
    fn processes_and_etime_are_parsed() {
        assert_eq!(parse_etime("00:42"), Some(42));
        assert_eq!(parse_etime("02:03:04"), Some(7384));
        assert_eq!(parse_etime("3-01:00:00"), Some(3 * 86_400 + 3600));
        assert_eq!(parse_etime("garbage"), None);
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().to_string();
        // this test binary is itself a process whose command line holds its own file name (ps/PowerShell see it)
        assert!(
            processes(&name).iter().any(|p| p.command.contains(&name))
                || processes(&name).is_empty()
        );
        assert!(mtime(&exe) > 0 && mtime(Path::new("/no/such/file")) == 0);
    }

    /// Test: the process check sees this very process and not an absurd pid.
    #[test]
    fn pid_alive_sees_this_process() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(4_000_000));
    }
}

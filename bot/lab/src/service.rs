//! Background jobs on every OS from one description: launchd (macOS), systemd user units and timers (Linux), Task Scheduler (Windows).
//!
//! A [`Job`] says what to run and when; `install`, `uninstall`, `state`, `restart`, `start` and `stop` do the OS-specific work. The text each backend
//! writes (plist, unit files, task XML) is produced by pure functions that are unit-tested on every host. This file depends only on `std` and `anyhow`
//! (plus `platform`), so the bot repo compiles the same file from its `lab/` mirror.
//!
//! Semantics, identical on all OSes:
//!  * `KeepAlive`: started at login/boot, restarted whenever it exits (launchd KeepAlive, systemd Restart=always, a logon task plus a 5-minute
//!    "start if not running" trigger on Windows).
//!  * `Every`: run every N seconds. `Hourly`: at a fixed minute of every hour. `Daily`: at fixed local times.

use crate::platform::{self, Os};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// When a job runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    /// Always running; restarted `throttle_secs` after it exits.
    KeepAlive { throttle_secs: u32 },
    /// Every `secs` seconds; `at_load` also runs once when it is installed or the user logs in.
    Every { secs: u64, at_load: bool },
    /// At `minute` of every hour.
    Hourly { minute: u8 },
    /// At these local (hour, minute) times every day.
    Daily(Vec<(u8, u8)>),
    /// At `hour`:`minute` local time on these weekdays (0 = Sunday .. 6 = Saturday, launchd's numbering).
    Weekly { days: Vec<u8>, hour: u8, minute: u8 },
}

/// One background job.
#[derive(Clone, Debug)]
pub struct Job {
    /// Unique name, e.g. `com.qwenlab.brain` (the launchd label, the systemd unit stem, the Task Scheduler task name).
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub work_dir: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub schedule: Schedule,
    /// Unix nice value (0..19); mapped to a Windows priority class.
    pub nice: Option<i32>,
    /// Lowest I/O priority (launchd LowPriorityIO, systemd IOSchedulingClass=idle).
    pub low_io: bool,
    /// Where the job's standard output and error go (both to `stdout` when `stderr` is None).
    pub stdout: Option<PathBuf>,
    pub stderr: Option<PathBuf>,
}

/// What the OS says about a job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// Registered with the OS (launchd loaded, systemd unit enabled, task present).
    pub loaded: bool,
    /// A process of the job exists right now.
    pub running: bool,
    /// Its process id where the OS reports one (macOS, Linux).
    pub pid: Option<u32>,
    /// Exit status of the last run where known.
    pub last_exit: Option<i32>,
}

impl Job {
    /// A job with defaults: runs `program args` in no particular folder, no extra environment, no logging.
    pub fn new(label: &str, program: &Path, args: &[&str], schedule: Schedule) -> Self {
        Self {
            label: label.into(),
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            work_dir: None,
            env: vec![],
            schedule,
            nice: None,
            low_io: false,
            stdout: None,
            stderr: None,
        }
    }
}

/// The PATH jobs run with (launchd and systemd start jobs with a nearly empty one).
pub fn job_path() -> String {
    let home = platform::home();
    let sep = if platform::os() == Os::Windows {
        ";"
    } else {
        ":"
    };
    let mut parts: Vec<String> = vec![
        home.join(".local/bin").display().to_string(),
        home.join(".cargo/bin").display().to_string(),
    ];
    match platform::os() {
        Os::Mac => parts.extend(
            [
                "/opt/homebrew/bin",
                "/usr/local/bin",
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
            ]
            .map(String::from),
        ),
        Os::Linux => parts.extend(
            [
                "/usr/local/bin",
                "/usr/bin",
                "/bin",
                "/usr/local/sbin",
                "/usr/sbin",
                "/sbin",
            ]
            .map(String::from),
        ),
        Os::Windows => {}
    }
    parts.join(sep)
}

// ------------------------------------------------------------------------------------------------- launchd (macOS)

/// Escapes text for XML.
fn xml_esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The launchd property list of a job.
pub fn plist(job: &Job) -> String {
    let mut p = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n");
    p.push_str(&format!(
        "  <key>Label</key><string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n",
        xml_esc(&job.label)
    ));
    p.push_str(&format!(
        "    <string>{}</string>\n",
        xml_esc(&job.program.display().to_string())
    ));
    for a in &job.args {
        p.push_str(&format!("    <string>{}</string>\n", xml_esc(a)));
    }
    p.push_str("  </array>\n");
    if let Some(w) = &job.work_dir {
        p.push_str(&format!(
            "  <key>WorkingDirectory</key><string>{}</string>\n",
            xml_esc(&w.display().to_string())
        ));
    }
    match &job.schedule {
        Schedule::KeepAlive { throttle_secs } => p.push_str(&format!("  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>ThrottleInterval</key><integer>{throttle_secs}</integer>\n")),
        Schedule::Every { secs, at_load } => p.push_str(&format!("  <key>StartInterval</key><integer>{secs}</integer>\n  <key>RunAtLoad</key><{}/>\n", if *at_load { "true" } else { "false" })),
        Schedule::Hourly { minute } => p.push_str(&format!("  <key>StartCalendarInterval</key><dict><key>Minute</key><integer>{minute}</integer></dict>\n  <key>RunAtLoad</key><false/>\n")),
        Schedule::Daily(times) => {
            p.push_str("  <key>StartCalendarInterval</key>\n  <array>\n");
            for (h, m) in times {
                p.push_str(&format!("    <dict><key>Hour</key><integer>{h}</integer><key>Minute</key><integer>{m}</integer></dict>\n"));
            }
            p.push_str("  </array>\n  <key>RunAtLoad</key><false/>\n");
        }
        Schedule::Weekly { days, hour, minute } => {
            p.push_str("  <key>StartCalendarInterval</key>\n  <array>\n");
            for d in days {
                p.push_str(&format!("    <dict><key>Weekday</key><integer>{d}</integer><key>Hour</key><integer>{hour}</integer><key>Minute</key><integer>{minute}</integer></dict>\n"));
            }
            p.push_str("  </array>\n  <key>RunAtLoad</key><false/>\n");
        }
    }
    if let Some(n) = job.nice {
        p.push_str(&format!("  <key>Nice</key><integer>{n}</integer>\n  <key>ProcessType</key><string>Background</string>\n"));
    }
    if job.low_io {
        p.push_str("  <key>LowPriorityIO</key><true/>\n");
    }
    if let Some(o) = &job.stdout {
        p.push_str(&format!(
            "  <key>StandardOutPath</key><string>{}</string>\n",
            xml_esc(&o.display().to_string())
        ));
        p.push_str(&format!(
            "  <key>StandardErrorPath</key><string>{}</string>\n",
            xml_esc(&job.stderr.as_ref().unwrap_or(o).display().to_string())
        ));
    }
    p.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
    p.push_str(&format!(
        "    <key>PATH</key><string>{}</string>\n",
        xml_esc(&job_path())
    ));
    for (k, v) in &job.env {
        p.push_str(&format!(
            "    <key>{}</key><string>{}</string>\n",
            xml_esc(k),
            xml_esc(v)
        ));
    }
    p.push_str("  </dict>\n</dict>\n</plist>\n");
    p
}

fn launchd_domain() -> String {
    let uid = platform::run("id", &["-u"])
        .unwrap_or_default()
        .trim()
        .to_string();
    format!("gui/{uid}")
}

fn plist_path(label: &str) -> PathBuf {
    platform::home()
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"))
}

fn quiet(cmd: &mut Command) -> Option<std::process::Output> {
    cmd.stdin(Stdio::null()).output().ok()
}

fn launchd_bootstrap(label: &str, path: &Path) -> Result<()> {
    // right after a bootout launchd can refuse with "Input/output error" (error 5) until the old job is gone: retry
    let domain = launchd_domain();
    let mut last = String::new();
    for attempt in 0..8 {
        let out = Command::new("launchctl")
            .args(["bootstrap", &domain])
            .arg(path)
            .output()
            .context("launchctl bootstrap")?;
        if out.status.success() {
            return Ok(());
        }
        last = String::from_utf8_lossy(&out.stderr).trim().to_string();
        std::thread::sleep(std::time::Duration::from_millis(500 * (attempt + 1)));
    }
    bail!("launchctl bootstrap {label} failed: {last}")
}

fn launchd_state(label: &str) -> State {
    let Some(out) = quiet(Command::new("launchctl").arg("list")) else {
        return State::default();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() >= 3 && f[2] == label {
            let pid = f[0].parse::<u32>().ok();
            return State {
                loaded: true,
                running: pid.is_some(),
                pid,
                last_exit: f[1].parse().ok(),
            };
        }
    }
    State::default()
}

// ------------------------------------------------------------------------------------------------- systemd (Linux)

/// Quotes one argument for a systemd `ExecStart=` line.
fn systemd_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-=:,@%+".contains(c))
    {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

/// The `.service` unit of a job.
pub fn systemd_service(job: &Job) -> String {
    let mut u = format!(
        "[Unit]\nDescription={}\n\n[Service]\nType=simple\n",
        job.label
    );
    let exec: Vec<String> = std::iter::once(job.program.display().to_string())
        .chain(job.args.iter().cloned())
        .map(|s| systemd_quote(&s))
        .collect();
    u.push_str(&format!("ExecStart={}\n", exec.join(" ")));
    if let Some(w) = &job.work_dir {
        u.push_str(&format!("WorkingDirectory={}\n", w.display()));
    }
    u.push_str(&format!("Environment=\"PATH={}\"\n", job_path()));
    for (k, v) in &job.env {
        u.push_str(&format!("Environment=\"{k}={}\"\n", v.replace('"', "\\\"")));
    }
    if let Some(n) = job.nice {
        u.push_str(&format!("Nice={n}\n"));
    }
    if job.low_io {
        u.push_str("IOSchedulingClass=idle\n");
    }
    if let Some(o) = &job.stdout {
        u.push_str(&format!("StandardOutput=append:{}\n", o.display()));
        u.push_str(&format!(
            "StandardError=append:{}\n",
            job.stderr.as_ref().unwrap_or(o).display()
        ));
    }
    if let Schedule::KeepAlive { throttle_secs } = &job.schedule {
        u.push_str(&format!(
            "Restart=always\nRestartSec={throttle_secs}\n\n[Install]\nWantedBy=default.target\n"
        ));
    }
    u
}

/// The `.timer` unit of a scheduled job (None for KeepAlive jobs).
pub fn systemd_timer(job: &Job) -> Option<String> {
    let mut t = format!("[Unit]\nDescription=Timer of {}\n\n[Timer]\n", job.label);
    match &job.schedule {
        Schedule::KeepAlive { .. } => return None,
        Schedule::Every { secs, at_load } => {
            t.push_str(&format!(
                "OnBootSec={}\nOnUnitInactiveSec={secs}\n",
                if *at_load { 30 } else { *secs }
            ));
        }
        Schedule::Hourly { minute } => t.push_str(&format!("OnCalendar=*-*-* *:{minute:02}:00\n")),
        Schedule::Daily(times) => {
            for (h, m) in times {
                t.push_str(&format!("OnCalendar=*-*-* {h:02}:{m:02}:00\n"));
            }
        }
        Schedule::Weekly { days, hour, minute } => {
            let names = days
                .iter()
                .map(|d| WEEKDAYS[(*d % 7) as usize][..3].to_string())
                .collect::<Vec<_>>()
                .join(",");
            t.push_str(&format!(
                "OnCalendar={names} *-*-* {hour:02}:{minute:02}:00\n"
            ));
        }
    }
    t.push_str("AccuracySec=1s\n\n[Install]\nWantedBy=timers.target\n");
    Some(t)
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

fn systemd_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| platform::home().join(".config"))
        .join("systemd/user")
}

fn systemctl(args: &[&str]) -> Option<std::process::Output> {
    quiet(Command::new("systemctl").arg("--user").args(args))
}

fn systemd_state(label: &str) -> State {
    let scheduled = systemd_dir().join(format!("{label}.timer")).exists();
    let show = |unit: &str, props: &[&str]| -> std::collections::HashMap<String, String> {
        let mut args = vec!["show".to_string(), unit.to_string()];
        args.extend(props.iter().map(|p| format!("-p{p}")));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        systemctl(&refs)
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    };
    let svc = show(
        &format!("{label}.service"),
        &["ActiveState", "MainPID", "ExecMainStatus", "LoadState"],
    );
    let loaded_unit = svc.get("LoadState").is_some_and(|s| s == "loaded");
    let pid = svc
        .get("MainPID")
        .and_then(|p| p.parse::<u32>().ok())
        .filter(|p| *p > 0);
    let running = svc
        .get("ActiveState")
        .is_some_and(|s| s == "active" || s == "activating");
    let loaded = if scheduled {
        show(&format!("{label}.timer"), &["ActiveState"])
            .get("ActiveState")
            .is_some_and(|s| s == "active")
    } else {
        loaded_unit && systemd_dir().join(format!("{label}.service")).exists()
    };
    State {
        loaded,
        running,
        pid,
        last_exit: svc.get("ExecMainStatus").and_then(|s| s.parse().ok()),
    }
}

// ------------------------------------------------------------------------------------------------- Task Scheduler (Windows)

/// Windows priority class (1 highest .. 10 lowest, 7 = below normal) for a nice value.
fn win_priority(nice: Option<i32>) -> u32 {
    match nice {
        None => 7,
        Some(n) if n >= 15 => 8,
        Some(_) => 7,
    }
}

/// The command line `cmd.exe /C` runs for a job: environment, the program, its arguments, output redirected to the log.
pub fn windows_command_line(job: &Job) -> String {
    let q = |s: &str| {
        if s.contains(' ') || s.is_empty() {
            format!("\"{s}\"")
        } else {
            s.to_string()
        }
    };
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in &job.env {
        parts.push(format!("set \"{k}={v}\"&&"));
    }
    parts.push(q(&job.program.display().to_string()));
    parts.extend(job.args.iter().map(|a| q(a)));
    let mut line = parts.join(" ");
    if let Some(o) = &job.stdout {
        line.push_str(&format!(" >> \"{}\" 2>&1", o.display()));
        if let Some(e) = &job.stderr {
            if e != o {
                line = line.replace(" 2>&1", &format!(" 2>> \"{}\"", e.display()));
            }
        }
    }
    line
}

/// The Task Scheduler XML of a job.
pub fn windows_task_xml(job: &Job) -> String {
    let start = "2026-01-01";
    let mut triggers = String::new();
    match &job.schedule {
        Schedule::KeepAlive { .. } => {
            // started at logon, and re-started within 5 minutes whenever it is not running (MultipleInstancesPolicy IgnoreNew)
            triggers.push_str("    <LogonTrigger><Enabled>true</Enabled></LogonTrigger>\n");
            triggers.push_str(&format!("    <TimeTrigger><StartBoundary>{start}T00:00:00</StartBoundary><Repetition><Interval>PT5M</Interval></Repetition><Enabled>true</Enabled></TimeTrigger>\n"));
        }
        Schedule::Every { secs, at_load } => {
            let mins = (*secs / 60).max(1);
            if *at_load {
                triggers.push_str("    <LogonTrigger><Enabled>true</Enabled></LogonTrigger>\n");
            }
            triggers.push_str(&format!("    <TimeTrigger><StartBoundary>{start}T00:00:00</StartBoundary><Repetition><Interval>PT{mins}M</Interval></Repetition><Enabled>true</Enabled></TimeTrigger>\n"));
        }
        Schedule::Hourly { minute } => triggers.push_str(&format!("    <TimeTrigger><StartBoundary>{start}T00:{minute:02}:00</StartBoundary><Repetition><Interval>PT1H</Interval></Repetition><Enabled>true</Enabled></TimeTrigger>\n")),
        Schedule::Daily(times) => {
            for (h, m) in times {
                triggers.push_str(&format!("    <CalendarTrigger><StartBoundary>{start}T{h:02}:{m:02}:00</StartBoundary><Enabled>true</Enabled><ScheduleByDay><DaysInterval>1</DaysInterval></ScheduleByDay></CalendarTrigger>\n"));
            }
        }
        Schedule::Weekly { days, hour, minute } => {
            let names: String = days.iter().map(|d| format!("<{}/>", WEEKDAYS[(*d % 7) as usize])).collect();
            triggers.push_str(&format!("    <CalendarTrigger><StartBoundary>{start}T{hour:02}:{minute:02}:00</StartBoundary><Enabled>true</Enabled><ScheduleByWeek><DaysOfWeek>{names}</DaysOfWeek><WeeksInterval>1</WeeksInterval></ScheduleByWeek></CalendarTrigger>\n"));
        }
    }
    let restart = matches!(job.schedule, Schedule::KeepAlive { .. });
    let work = job
        .work_dir
        .as_ref()
        .map(|w| {
            format!(
                "<WorkingDirectory>{}</WorkingDirectory>",
                xml_esc(&w.display().to_string())
            )
        })
        .unwrap_or_default();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.4\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <RegistrationInfo><Description>{label}</Description></RegistrationInfo>\n  <Triggers>\n{triggers}  </Triggers>\n  <Principals><Principal id=\"Author\"><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n  <Settings>\n    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\n    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\n    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\n    <AllowHardTerminate>true</AllowHardTerminate>\n    <StartWhenAvailable>true</StartWhenAvailable>\n    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\n    <AllowStartOnDemand>true</AllowStartOnDemand>\n    <Enabled>true</Enabled>\n    <Hidden>true</Hidden>\n    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>\n    <Priority>{prio}</Priority>\n{restart}  </Settings>\n  <Actions Context=\"Author\">\n    <Exec><Command>cmd.exe</Command><Arguments>/C \"{cmd}\"</Arguments>{work}</Exec>\n  </Actions>\n</Task>\n",
        label = xml_esc(&job.label),
        prio = win_priority(job.nice),
        restart = if restart { "    <RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>\n" } else { "" },
        cmd = xml_esc(&windows_command_line(job)),
    )
}

/// UTF-16 little-endian bytes with a byte-order mark (the encoding `schtasks /XML` expects).
fn utf16le(s: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

fn schtasks(args: &[&str]) -> Option<std::process::Output> {
    quiet(Command::new("schtasks").args(args))
}

/// Splits one CSV line of `schtasks /FO CSV` output.
pub fn parse_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in line.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn windows_state(label: &str) -> State {
    let Some(o) = schtasks(&["/Query", "/TN", label, "/FO", "CSV", "/NH", "/V"]) else {
        return State::default();
    };
    if !o.status.success() {
        return State::default();
    }
    let text = String::from_utf8_lossy(&o.stdout).to_string();
    let Some(line) = text.lines().find(|l| !l.trim().is_empty()) else {
        return State::default();
    };
    let f = parse_csv_line(line);
    // HostName, TaskName, Next Run Time, Status, Logon Mode, Last Run Time, Last Result, ...
    State {
        loaded: true,
        running: f.get(3).is_some_and(|s| s.eq_ignore_ascii_case("Running")),
        pid: None,
        last_exit: f.get(6).and_then(|s| s.trim().parse().ok()),
    }
}

// ------------------------------------------------------------------------------------------------- the common interface

/// Registers a job with the OS and starts it (replacing an older registration of the same label).
pub fn install(job: &Job) -> Result<String> {
    if let Some(o) = &job.stdout {
        if let Some(d) = o.parent() {
            std::fs::create_dir_all(d).ok();
        }
    }
    match platform::os() {
        Os::Mac => {
            let path = plist_path(&job.label);
            std::fs::create_dir_all(path.parent().expect("has a parent"))?;
            std::fs::write(&path, plist(job))?;
            let _ = quiet(
                Command::new("launchctl")
                    .args(["bootout", &format!("{}/{}", launchd_domain(), job.label)]),
            );
            launchd_bootstrap(&job.label, &path)?;
            Ok(format!("installed {} ({})", job.label, path.display()))
        }
        Os::Linux => {
            let dir = systemd_dir();
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join(format!("{}.service", job.label)),
                systemd_service(job),
            )?;
            let timer = systemd_timer(job);
            let _ = std::fs::remove_file(dir.join(format!("{}.timer", job.label)));
            if let Some(t) = &timer {
                std::fs::write(dir.join(format!("{}.timer", job.label)), t)?;
            }
            systemctl(&["daemon-reload"]).filter(|o| o.status.success()).context("systemctl --user daemon-reload failed: is a systemd user session running (loginctl enable-linger)?")?;
            let unit = if timer.is_some() {
                format!("{}.timer", job.label)
            } else {
                format!("{}.service", job.label)
            };
            if timer.is_none() {
                let _ = systemctl(&["restart", &format!("{}.service", job.label)]);
            }
            let out = systemctl(&["enable", "--now", &unit]).context("systemctl enable")?;
            if !out.status.success() {
                bail!(
                    "systemctl --user enable --now {unit} failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(format!("installed {} ({})", job.label, dir.display()))
        }
        Os::Windows => {
            let xml = std::env::temp_dir().join(format!("{}.task.xml", job.label));
            std::fs::write(&xml, utf16le(&windows_task_xml(job)))?;
            let out = schtasks(&[
                "/Create",
                "/TN",
                &job.label,
                "/XML",
                &xml.display().to_string(),
                "/F",
            ])
            .context("schtasks /Create")?;
            let _ = std::fs::remove_file(&xml);
            if !out.status.success() {
                bail!(
                    "schtasks /Create {} failed: {}",
                    job.label,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            if matches!(job.schedule, Schedule::KeepAlive { .. })
                || matches!(job.schedule, Schedule::Every { at_load: true, .. })
            {
                let _ = schtasks(&["/Run", "/TN", &job.label]);
            }
            Ok(format!("installed {}", job.label))
        }
    }
}

/// Stops a job and removes its registration.
pub fn uninstall(label: &str) -> Result<String> {
    match platform::os() {
        Os::Mac => {
            let _ = quiet(
                Command::new("launchctl")
                    .args(["bootout", &format!("{}/{label}", launchd_domain())]),
            );
            let _ = std::fs::remove_file(plist_path(label));
        }
        Os::Linux => {
            let dir = systemd_dir();
            for unit in [format!("{label}.timer"), format!("{label}.service")] {
                let _ = systemctl(&["disable", "--now", &unit]);
                let _ = std::fs::remove_file(dir.join(&unit));
            }
            let _ = systemctl(&["daemon-reload"]);
        }
        Os::Windows => {
            let _ = schtasks(&["/End", "/TN", label]);
            let _ = schtasks(&["/Delete", "/TN", label, "/F"]);
        }
    }
    Ok(format!("removed {label}"))
}

/// What the OS says about a job.
pub fn state(label: &str) -> State {
    match platform::os() {
        Os::Mac => launchd_state(label),
        Os::Linux => systemd_state(label),
        Os::Windows => windows_state(label),
    }
}

/// Stops a job's process (a KeepAlive job is started again by the OS unless it is unloaded).
pub fn stop(label: &str) {
    match platform::os() {
        Os::Mac => {
            let _ = quiet(Command::new("launchctl").args([
                "kill",
                "SIGTERM",
                &format!("{}/{label}", launchd_domain()),
            ]));
        }
        Os::Linux => {
            let _ = systemctl(&["stop", &format!("{label}.service")]);
        }
        Os::Windows => {
            let _ = schtasks(&["/End", "/TN", label]);
        }
    }
}

/// Starts (or restarts) a job's process now.
pub fn restart(label: &str) {
    match platform::os() {
        Os::Mac => {
            let _ = quiet(Command::new("launchctl").args([
                "kickstart",
                "-k",
                &format!("{}/{label}", launchd_domain()),
            ]));
        }
        Os::Linux => {
            let _ = systemctl(&["restart", &format!("{label}.service")]);
        }
        Os::Windows => {
            let _ = schtasks(&["/End", "/TN", label]);
            let _ = schtasks(&["/Run", "/TN", label]);
        }
    }
}

/// Unloads a job but keeps its definition (so `load` can bring it back): used around a binary replacement.
pub fn unload(label: &str) {
    match platform::os() {
        Os::Mac => {
            let _ = quiet(
                Command::new("launchctl")
                    .args(["bootout", &format!("{}/{label}", launchd_domain())]),
            );
        }
        Os::Linux => {
            let _ = systemctl(&["stop", &format!("{label}.timer")]);
            let _ = systemctl(&["stop", &format!("{label}.service")]);
            // A user service is considered "loaded" while its unit file exists, even after
            // stop/reset-failed. Remove the definition during replacement; install() recreates
            // it immediately after the binary is copied.
            let _ = std::fs::remove_file(systemd_dir().join(format!("{label}.timer")));
            let _ = std::fs::remove_file(systemd_dir().join(format!("{label}.service")));
            let _ = systemctl(&["daemon-reload"]);
        }
        Os::Windows => {
            let _ = schtasks(&["/End", "/TN", label]);
            let _ = schtasks(&["/Change", "/TN", label, "/DISABLE"]);
        }
    }
}

/// Loads a job whose definition exists again (after `unload`); true when it is loaded afterwards.
pub fn load(label: &str) -> bool {
    match platform::os() {
        Os::Mac => {
            if launchd_state(label).loaded {
                return true;
            }
            launchd_bootstrap(label, &plist_path(label)).is_ok()
        }
        Os::Linux => {
            let unit = if systemd_dir().join(format!("{label}.timer")).exists() {
                format!("{label}.timer")
            } else {
                format!("{label}.service")
            };
            systemctl(&["start", &unit]).is_some_and(|o| o.status.success())
                && systemd_state(label).loaded
        }
        Os::Windows => {
            let _ = schtasks(&["/Change", "/TN", label, "/ENABLE"]);
            let _ = schtasks(&["/Run", "/TN", label]);
            windows_state(label).loaded
        }
    }
}

/// Linux only: lets user units keep running when nobody is logged in (`loginctl enable-linger`). Returns what happened, for the install log.
pub fn enable_linger() -> String {
    if platform::os() != Os::Linux {
        return String::new();
    }
    let user = std::env::var("USER").unwrap_or_default();
    match quiet(Command::new("loginctl").args(["enable-linger", &user])) {
        Some(o) if o.status.success() => "loginctl enable-linger: jobs keep running after logout".into(),
        _ => "loginctl enable-linger failed: run `sudo loginctl enable-linger $USER` once so jobs run without a login".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(schedule: Schedule) -> Job {
        let mut j = Job::new(
            "com.example.job",
            Path::new("/opt/my bin/qwen-lab"),
            &["--config", "/tmp/a & b/lab.toml", "brain", "serve"],
            schedule,
        );
        j.work_dir = Some(PathBuf::from("/tmp/lab"));
        j.nice = Some(10);
        j.low_io = true;
        j.stdout = Some(PathBuf::from("/tmp/lab/logs/job.log"));
        j.env = vec![("QWEN_LAB_ROOT".into(), "/tmp/lab".into())];
        j
    }

    /// Test: the launchd plist keeps the job alive, escapes paths and carries priority and logs.
    #[test]
    fn the_plist_describes_a_keep_alive_job() {
        let p = plist(&sample(Schedule::KeepAlive { throttle_secs: 60 }));
        assert!(
            p.contains("<key>KeepAlive</key><true/>")
                && p.contains("<key>ThrottleInterval</key><integer>60</integer>")
        );
        assert!(p.contains("/tmp/a &amp; b/lab.toml") && !p.contains("a & b"));
        assert!(
            p.contains("<key>Nice</key><integer>10</integer>")
                && p.contains("<key>LowPriorityIO</key><true/>")
        );
        assert!(
            p.contains("<key>StandardOutPath</key><string>/tmp/lab/logs/job.log</string>")
                && p.contains("QWEN_LAB_ROOT")
        );
        assert_eq!(p.matches("<dict>").count(), p.matches("</dict>").count());
        assert_eq!(p.matches("<array>").count(), p.matches("</array>").count());
    }

    /// Test: scheduled launchd jobs use the right calendar keys and never run at load unless asked.
    #[test]
    fn scheduled_plists_use_calendar_and_interval_keys() {
        assert!(plist(&sample(Schedule::Hourly { minute: 7 })).contains(
            "<key>StartCalendarInterval</key><dict><key>Minute</key><integer>7</integer></dict>"
        ));
        let d = plist(&sample(Schedule::Daily(vec![(1, 10), (13, 10)])));
        assert!(
            d.contains(
                "<key>Hour</key><integer>13</integer><key>Minute</key><integer>10</integer>"
            ) && d.contains("<key>RunAtLoad</key><false/>")
        );
        let e = plist(&sample(Schedule::Every {
            secs: 900,
            at_load: true,
        }));
        assert!(
            e.contains("<key>StartInterval</key><integer>900</integer>")
                && e.contains("<key>RunAtLoad</key><true/>")
        );
    }

    /// Test: the systemd service restarts a KeepAlive job and quotes arguments with spaces.
    #[test]
    fn the_systemd_service_restarts_and_quotes() {
        let s = systemd_service(&sample(Schedule::KeepAlive { throttle_secs: 60 }));
        assert!(
            s.contains(
                "ExecStart=\"/opt/my bin/qwen-lab\" --config \"/tmp/a & b/lab.toml\" brain serve"
            ),
            "{s}"
        );
        assert!(
            s.contains("Restart=always")
                && s.contains("RestartSec=60")
                && s.contains("Nice=10")
                && s.contains("IOSchedulingClass=idle")
        );
        assert!(
            s.contains("StandardOutput=append:/tmp/lab/logs/job.log")
                && s.contains("WorkingDirectory=/tmp/lab")
                && s.contains("WantedBy=default.target")
        );
        assert!(systemd_timer(&sample(Schedule::KeepAlive { throttle_secs: 60 })).is_none());
    }

    /// Test: systemd timers cover hourly, daily and interval jobs.
    #[test]
    fn systemd_timers_cover_every_schedule() {
        assert!(systemd_timer(&sample(Schedule::Hourly { minute: 7 }))
            .unwrap()
            .contains("OnCalendar=*-*-* *:07:00"));
        let d = systemd_timer(&sample(Schedule::Daily(vec![(1, 10), (19, 10)]))).unwrap();
        assert!(d.contains("OnCalendar=*-*-* 01:10:00") && d.contains("OnCalendar=*-*-* 19:10:00"));
        let e = systemd_timer(&sample(Schedule::Every {
            secs: 900,
            at_load: false,
        }))
        .unwrap();
        assert!(e.contains("OnUnitInactiveSec=900") && e.contains("OnBootSec=900"));
        let s = systemd_service(&sample(Schedule::Hourly { minute: 7 }));
        assert!(
            !s.contains("Restart=always"),
            "a timer-driven job must not restart itself"
        );
    }

    /// Test: the Windows task keeps a KeepAlive job running (logon + 5-minute trigger, restart on failure) and logs through cmd.exe.
    #[test]
    fn the_windows_task_keeps_a_job_alive_and_logs() {
        let x = windows_task_xml(&sample(Schedule::KeepAlive { throttle_secs: 60 }));
        assert!(
            x.contains("<LogonTrigger>")
                && x.contains("<Interval>PT5M</Interval>")
                && x.contains("<RestartOnFailure>")
        );
        assert!(
            x.contains("<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>")
                && x.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>")
        );
        assert!(
            x.contains("<Command>cmd.exe</Command>")
                && x.contains("&gt;&gt; &quot;/tmp/lab/logs/job.log&quot; 2&gt;&amp;1"),
            "{x}"
        );
        assert!(
            x.contains("<Priority>7</Priority>")
                && x.contains("<WorkingDirectory>/tmp/lab</WorkingDirectory>")
        );
        assert_eq!(x.matches("<Triggers>").count(), 1);
    }

    /// Test: Windows scheduled tasks use hourly repetition and daily calendar triggers.
    #[test]
    fn windows_tasks_cover_every_schedule() {
        assert!(windows_task_xml(&sample(Schedule::Hourly { minute: 7 }))
            .contains("T00:07:00</StartBoundary><Repetition><Interval>PT1H</Interval>"));
        let d = windows_task_xml(&sample(Schedule::Daily(vec![(1, 10), (7, 10)])));
        assert_eq!(d.matches("<CalendarTrigger>").count(), 2);
        assert!(d.contains("T07:10:00</StartBoundary>"));
        let e = windows_task_xml(&sample(Schedule::Every {
            secs: 900,
            at_load: true,
        }));
        assert!(
            e.contains("PT15M")
                && e.contains("<LogonTrigger>")
                && !e.contains("<RestartOnFailure>")
        );
    }

    /// Test: the Windows command line sets the environment before the program and redirects both streams.
    #[test]
    fn the_windows_command_line_sets_env_and_redirects() {
        let c = windows_command_line(&sample(Schedule::KeepAlive { throttle_secs: 60 }));
        assert!(
            c.starts_with("set \"QWEN_LAB_ROOT=/tmp/lab\"&& \"/opt/my bin/qwen-lab\""),
            "{c}"
        );
        assert!(c.ends_with(">> \"/tmp/lab/logs/job.log\" 2>&1"));
        assert_eq!(&utf16le("A")[..4], &[0xFF, 0xFE, b'A', 0]);
    }

    /// Test: `schtasks /FO CSV` lines are split respecting quotes.
    #[test]
    fn schtasks_csv_is_parsed() {
        let f = parse_csv_line("\"PC\",\"\\com.example.job\",\"N/A\",\"Running\",\"Interactive only\",\"14/10/2026 11:00:00\",\"267009\"");
        assert_eq!(f[3], "Running");
        assert_eq!(f[6], "267009");
    }

    /// Test: weekly jobs (Tuesday to Saturday morning, the bot's daily review) are expressed on every OS.
    #[test]
    fn weekly_schedules_are_expressed_on_every_os() {
        let w = Schedule::Weekly {
            days: vec![2, 3, 4, 5, 6],
            hour: 9,
            minute: 30,
        };
        let p = plist(&sample(w.clone()));
        assert_eq!(p.matches("<key>Weekday</key>").count(), 5);
        assert!(p.contains("<key>Weekday</key><integer>6</integer><key>Hour</key><integer>9</integer><key>Minute</key><integer>30</integer>"));
        assert!(systemd_timer(&sample(w.clone()))
            .unwrap()
            .contains("OnCalendar=Tue,Wed,Thu,Fri,Sat *-*-* 09:30:00"));
        let x = windows_task_xml(&sample(w));
        assert!(x.contains("<ScheduleByWeek><DaysOfWeek><Tuesday/><Wednesday/><Thursday/><Friday/><Saturday/></DaysOfWeek>") && x.contains("T09:30:00</StartBoundary>"));
    }

    /// Test: an unknown job is reported as not loaded on this host.
    #[test]
    fn an_unknown_job_is_not_loaded() {
        assert!(!state("com.example.no-such-job").loaded);
    }
}

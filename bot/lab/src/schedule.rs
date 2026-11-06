//! Installing the lab's background jobs. The job definitions live in `jobs`, the OS-specific work (launchd, systemd, Task Scheduler) in `service`; this
//! module copies the running binary to its stable path (and re-signs it on macOS) and registers the job. `schedule`, `dash`, `brain` and `trainer`
//! `install|uninstall|status` all end up here; `qwen-ops install` registers every job.

use crate::config::Config;
use crate::jobs;
use crate::platform::{self, Os};
use crate::service;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = jobs::TICK;
pub const DASH_LABEL: &str = jobs::DASH;
pub const BRAIN_LABEL: &str = jobs::BRAIN;
pub const TRAINER_LABEL: &str = jobs::TRAINER;
/// Stable code-signing identifier on macOS (a firewall rule keyed on it keeps matching across rebuilds).
pub const SIGN_IDENTIFIER: &str = "com.qwenlab.lab";

/// Copies the running binary to the stable path (atomically, so a job that is running right now is not disturbed). macOS: re-signs it with a stable
/// identifier. Returns the path and whether it was signed.
pub fn install_binary(cfg: &Config) -> Result<(PathBuf, bool)> { install_binary_from(cfg, &std::env::current_exe()?) }

/// Same, for the lab binary at `exe` (`qwen-ops` installs the freshly built `target/release/qwen-lab`).
pub fn install_binary_from(cfg: &Config, exe: &Path) -> Result<(PathBuf, bool)> {
    let stable = jobs::stable_binary(cfg);
    std::fs::create_dir_all(stable.parent().expect("has a parent"))?;
    std::fs::create_dir_all(cfg.logs_dir())?;
    if exe == stable.as_path() {
        return Ok((stable, false));
    }
    // a running executable cannot be overwritten on Windows but can be renamed away: move the old one aside first
    let tmp = stable.with_extension(if platform::os() == Os::Windows { "new.exe" } else { "new" });
    std::fs::copy(exe, &tmp).with_context(|| format!("copy {} to {}", exe.display(), tmp.display()))?;
    let signed = platform::os() == Os::Mac
        && Command::new("codesign").args(["--force", "--sign", "-", "--identifier", SIGN_IDENTIFIER]).arg(&tmp).output().is_ok_and(|o| o.status.success());
    replace_file(&tmp, &stable)?;
    Ok((stable, signed))
}

fn install_job(cfg: &Config, config_path: &Path, make: fn(&Config, &Path, &Path) -> service::Job) -> Result<String> {
    let (stable, signed) = install_binary(cfg)?;
    let job = make(cfg, &stable, config_path);
    let what = service::install(&job)?;
    let linger = service::enable_linger();
    Ok(format!("{what}{}{}", if platform::os() == Os::Mac { if signed { ", signed" } else { ", NOT re-signed" } } else { "" }, if linger.is_empty() { String::new() } else { format!("; {linger}") }))
}

fn state_line(label: &str) -> String {
    let s = service::state(label);
    if !s.loaded {
        return format!("not loaded: {label}");
    }
    format!("loaded: {label}\n  running: {}{}\n  last exit: {}", s.running, s.pid.map(|p| format!(" (pid {p})")).unwrap_or_default(), s.last_exit.map_or("unknown".into(), |c| c.to_string()))
}

/// Installs the binary copy and the hourly tick job, and loads it.
pub fn install(cfg: &Config, config_path: &Path) -> Result<String> { install_job(cfg, config_path, jobs::tick) }
/// Stops and removes the tick job.
pub fn uninstall() -> Result<String> { service::uninstall(LABEL) }
/// The tick job's state.
pub fn status() -> String { state_line(LABEL) }

/// Install the read-only monitoring page as a background service (localhost only, always on).
pub fn install_dash(cfg: &Config, config_path: &Path) -> Result<String> {
    Ok(format!("{} -> http://127.0.0.1:{}/", install_job(cfg, config_path, jobs::dash)?, cfg.dashboard.port))
}
/// Stops and removes the dashboard job.
pub fn uninstall_dash() -> Result<String> { service::uninstall(DASH_LABEL) }
/// Whether the dashboard job is loaded.
pub fn dash_status() -> String { state_line(DASH_LABEL) }

/// Installs and starts the brain server.
pub fn install_brain(cfg: &Config, config_path: &Path) -> Result<String> { install_job(cfg, config_path, jobs::brain) }
/// Stops and removes the brain server job.
pub fn uninstall_brain() -> Result<String> { service::uninstall(BRAIN_LABEL) }

/// Installs and starts the deep-learning trainer.
pub fn install_trainer(cfg: &Config, config_path: &Path) -> Result<String> { install_job(cfg, config_path, jobs::trainer) }
/// Stops and removes the trainer job.
pub fn uninstall_trainer() -> Result<String> { service::uninstall(TRAINER_LABEL) }

/// Installs and starts the strategy test loop.
pub fn install_improve(cfg: &Config, config_path: &Path) -> Result<String> { install_job(cfg, config_path, jobs::improve) }

/// Moves `new` over `target`. A running executable cannot be overwritten on Windows but can be renamed, so the old file is renamed to `<name>.old` first
/// (the next replace removes it).
pub fn replace_file(new: &Path, target: &Path) -> Result<()> {
    let old = target.with_file_name(format!("{}.old", target.file_name().and_then(|n| n.to_str()).unwrap_or("old")));
    if target.exists() {
        let _ = std::fs::remove_file(&old);
        if std::fs::rename(target, &old).is_err() {
            std::fs::remove_file(target).with_context(|| format!("replace {}", target.display()))?;
        }
    }
    std::fs::rename(new, target).with_context(|| format!("replace {} (is a job still running it? stop the lab jobs first)", target.display()))?;
    let _ = std::fs::remove_file(&old);
    Ok(())
}

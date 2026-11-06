//! `qwen-ops install | uninstall | status | restart | redeploy`: set up, inspect and update the lab on macOS, Linux and Windows. Replaces the shell
//! installers (`ops/install_tools.sh`, `ops/resource_safety/install.sh`, `ops/redeploy_lab.sh`), which remain only as one-line stubs.
//!
//! `redeploy` is the only safe way to replace the lab binary: it builds first (jobs keep running), then stops every job that runs the lab binary,
//! replaces the binary and the helper tools, registers every job again, and verifies that each one is loaded.

use crate::config::Config;
use crate::jobs;
use crate::platform::{self, Os};
use crate::schedule;
use crate::service;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The helper binaries: qwen-tools, qwen-ops, qwen-notify, qwen-guard.
pub const TOOLS: [&str; 4] = ["tools", "ops", "notify", "guard"];

/// Where `lab.toml` is: `--config <path>` if given, else `<lab root>/lab.toml`.
pub fn config_path(args: &[String]) -> PathBuf {
    args.iter().position(|a| a == "--config").and_then(|i| args.get(i + 1)).map(PathBuf::from).unwrap_or_else(|| platform::lab_root().join("lab.toml"))
}

fn cargo() -> PathBuf {
    platform::which("cargo").unwrap_or_else(|| platform::home().join(".cargo/bin").join(platform::exe("cargo")))
}

/// Runs `cargo build --release` in the lab repo (all binaries).
fn build(repo: &Path) -> Result<()> {
    let out = Command::new(cargo()).args(["build", "--release"]).current_dir(repo).stdin(Stdio::null()).output().context("cargo build")?;
    if !out.status.success() {
        bail!("cargo build --release failed:\n{}", String::from_utf8_lossy(&out.stderr).lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    Ok(())
}

/// Copies `target/release/qwen-<name>` into the helper-binary folder (macOS: ad-hoc signed with the stable identifier `com.qwenlab.<name>` so a firewall
/// rule keeps matching across rebuilds).
fn install_tool(repo: &Path, name: &str) -> Result<String> {
    let src = repo.join("target/release").join(platform::exe(&format!("qwen-{name}")));
    if !src.exists() {
        bail!("{} was not built", src.display());
    }
    let dst = platform::tool(&format!("qwen-{name}"));
    std::fs::create_dir_all(dst.parent().expect("has a parent"))?;
    let tmp = dst.with_file_name(format!("{}.new", dst.file_name().and_then(|n| n.to_str()).unwrap_or("tool")));
    std::fs::copy(&src, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    if platform::os() == Os::Mac {
        let ok = Command::new("codesign").args(["--force", "--sign", "-", "--identifier", &format!("com.qwenlab.{name}")]).arg(&tmp).output().is_ok_and(|o| o.status.success());
        if !ok {
            bail!("codesign of qwen-{name} failed");
        }
    }
    schedule::replace_file(&tmp, &dst)?;
    Ok(format!("qwen-{name}"))
}

fn install_tools(repo: &Path) -> Result<()> {
    for t in TOOLS {
        install_tool(repo, t)?;
    }
    Ok(())
}

/// Installs the git hooks that keep the Discord webhook out of both repos.
fn install_hooks(repo: &Path) {
    let guard = platform::tool("qwen-guard");
    let bot = platform::bot_repo();
    let mut cmd = Command::new(guard);
    cmd.arg("install-hooks").arg(repo);
    if bot.join(".git").exists() {
        cmd.arg(&bot);
    }
    let _ = cmd.stdin(Stdio::null()).status();
}

fn short_head(repo: &Path) -> String {
    platform::run("git", &["-C", &repo.display().to_string(), "rev-parse", "--short", "HEAD"]).unwrap_or_default().trim().to_string()
}

/// Creates the lab folders and `lab.toml` when they are missing (`qwen-lab init` does the same for the data).
fn ensure_lab(cfg_path: &Path, repo: &Path) -> Result<Config> {
    if !cfg_path.exists() {
        let q = repo.join("target/release").join(platform::exe("qwen-lab"));
        std::fs::create_dir_all(cfg_path.parent().expect("has a parent"))?;
        let st = Command::new(&q).arg("--config").arg(cfg_path).arg("init").stdin(Stdio::null()).status().with_context(|| format!("run {}", q.display()))?;
        if !st.success() {
            bail!("qwen-lab init failed");
        }
    }
    Config::load(cfg_path)
}

/// First-time setup (also safe to repeat): builds, installs the tools and the lab binary, registers every job, installs the git hooks.
pub fn install(args: &[String]) -> Result<()> {
    let repo = platform::lab_repo();
    let cfg_path = config_path(args);
    println!("platform: {} | lab repo {} | lab root {}", platform::os_name(), repo.display(), platform::lab_root().display());
    build(&repo)?;
    let cfg = ensure_lab(&cfg_path, &repo)?;
    install_tools(&repo)?;
    let (_stable, signed) = schedule::install_binary_from(&cfg, &repo.join("target/release").join(platform::exe("qwen-lab")))?;
    println!("lab binary installed{}", if platform::os() == Os::Mac { if signed { " (signed)" } else { " (NOT signed)" } } else { "" });
    let mut failed = Vec::new();
    for job in jobs::all(&cfg, &cfg_path) {
        match service::install(&job) {
            Ok(m) => println!("{m}"),
            Err(e) => {
                println!("FAILED {}: {e:#}", job.label);
                failed.push(job.label.clone());
            }
        }
    }
    let linger = service::enable_linger();
    if !linger.is_empty() {
        println!("{linger}");
    }
    install_hooks(&repo);
    if !failed.is_empty() {
        bail!("jobs not installed: {}", failed.join(", "));
    }
    println!("installed. Check with `qwen-ops status` and `qwen-ops health`; the dashboard is http://127.0.0.1:{}/", cfg.dashboard.port);
    Ok(())
}

/// Stops and removes every lab job (data and binaries stay).
pub fn uninstall() -> Result<()> {
    for l in jobs::ALL {
        println!("{}", service::uninstall(l)?);
    }
    Ok(())
}

/// One line per job: loaded, running, pid, last exit.
pub fn status() -> Vec<String> {
    jobs::ALL.iter().copied().chain(["com.claudemoomoobot.bot"]).map(|l| {
        let s = service::state(l);
        format!("{l}: {}", if !s.loaded { "NOT LOADED".to_string() } else { format!("loaded{}{}{}", if s.running { ", running" } else { ", idle" }, s.pid.map(|p| format!(" pid {p}")).unwrap_or_default(), s.last_exit.map(|c| format!(", last exit {c}")).unwrap_or_default()) })
    }).collect()
}

/// Builds the newest code and replaces the running lab with it. Prints `redeploy: done at <commit>` or the problems. Returns false on any problem.
pub fn redeploy(args: &[String]) -> bool {
    let (ok, text) = redeploy_text(args);
    print!("{text}");
    ok
}

/// `redeploy`, returning its report instead of printing it (the monitor calls this).
pub fn redeploy_text(args: &[String]) -> (bool, String) {
    let mut log = String::new();
    let ok = redeploy_into(args, &mut log);
    (ok, log)
}

fn redeploy_into(args: &[String], log: &mut String) -> bool {
    use std::fmt::Write as _;
    macro_rules! say {
        ($($a:tt)*) => {{ let _ = writeln!(log, $($a)*); }};
    }
    let repo = platform::lab_repo();
    let cfg_path = config_path(args);
    if let Err(e) = build(&repo) {
        say!("redeploy: build failed: {e:#}");
        return false;
    }
    let cfg = match Config::load(&cfg_path) {
        Ok(c) => c,
        Err(e) => {
            say!("redeploy: {} unreadable: {e:#}", cfg_path.display());
            return false;
        }
    };
    let maintenance = cfg.root.join("MAINTENANCE");
    let _ = std::fs::write(&maintenance, "redeploy in progress");
    let mut ok = true;
    for l in jobs::LAB_BINARY_JOBS {
        service::unload(l);
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    let q = repo.join("target/release").join(platform::exe("qwen-lab"));
    if let Err(e) = schedule::install_binary_from(&cfg, &q) {
        say!("redeploy: installing the lab binary failed: {e:#}");
        ok = false;
    }
    if let Err(e) = install_tools(&repo) {
        say!("redeploy: installing the tools failed: {e:#}");
        ok = false;
    }
    for job in jobs::all(&cfg, &cfg_path) {
        if let Err(e) = service::install(&job) {
            say!("redeploy: {} failed: {e:#}", job.label);
            ok = false;
        }
    }
    // a job that is not loaded after the installs (launchctl can refuse right after a bootout) is registered again, a few times
    for l in jobs::LAB_BINARY_JOBS {
        let mut loaded = false;
        for _ in 0..6 {
            if service::state(l).loaded || service::load(l) {
                loaded = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        if !loaded {
            say!("redeploy: {l} is NOT loaded");
            ok = false;
        }
    }
    install_hooks(&repo);
    let _ = std::fs::remove_file(&maintenance);
    if ok {
        say!("redeploy: done at {}", short_head(&repo));
    }
    ok
}

/// `qwen-ops install-tools`: builds and installs only the helper binaries (qwen-tools, qwen-ops, qwen-notify, qwen-guard).
pub fn install_tools_only() -> Result<()> {
    let repo = platform::lab_repo();
    build(&repo)?;
    install_tools(&repo)?;
    println!("installed the helper tools into {}", platform::bin_dir().display());
    Ok(())
}

/// `qwen-ops restart <label>`.
pub fn restart(label: &str) {
    service::restart(label);
    println!("restarted {label}");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: `--config` wins over the default location.
    #[test]
    fn config_path_follows_the_flag() {
        let a: Vec<String> = ["install", "--config", "/tmp/x/lab.toml"].iter().map(|s| s.to_string()).collect();
        assert_eq!(config_path(&a), PathBuf::from("/tmp/x/lab.toml"));
        assert!(config_path(&[]).ends_with("lab.toml"));
    }

    /// Test: status lists every lab job and the bot.
    #[test]
    fn status_lists_every_job() {
        let s = status();
        assert_eq!(s.len(), jobs::ALL.len() + 1);
        assert!(s.iter().any(|l| l.starts_with("com.claudemoomoobot.bot")));
    }

    /// Test: a file replaced by `replace_file` keeps only the new content and leaves no stray `.new` file.
    #[test]
    fn replace_file_swaps_the_content() {
        let d = tempfile::tempdir().unwrap();
        let (t, n) = (d.path().join("bin"), d.path().join("bin.new"));
        std::fs::write(&t, "old").unwrap();
        std::fs::write(&n, "new").unwrap();
        schedule::replace_file(&n, &t).unwrap();
        assert_eq!(std::fs::read_to_string(&t).unwrap(), "new");
        assert!(!n.exists());
    }
}

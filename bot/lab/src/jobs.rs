//! The lab's background jobs, defined once for every OS (see `service`). The same table drives `schedule install`, `dash install`, `brain install`,
//! `trainer install`, the monitor's revive step and `qwen-ops install|uninstall|status|redeploy`.

use crate::config::Config;
use crate::platform;
use crate::service::{Job, Schedule};
use std::path::{Path, PathBuf};

pub const TICK: &str = "com.qwenlab.tick";
pub const DASH: &str = "com.qwenlab.dash";
pub const BRAIN: &str = "com.qwenlab.brain";
pub const TRAINER: &str = "com.qwenlab.trainer";
pub const IMPROVE: &str = "com.qwenlab.improve";
pub const MONITOR: &str = "com.qwenlab.monitor";
pub const SUPERVISOR: &str = "com.qwenlab.supervisor";
pub const RESOURCEWATCH: &str = "com.qwenlab.resourcewatch";
pub const WATCHDOG: &str = "com.qwenlab.watchdog";

/// The jobs that run the lab binary itself (stopped and started around a binary replacement).
pub const LAB_BINARY_JOBS: [&str; 5] = [BRAIN, IMPROVE, DASH, TICK, TRAINER];
/// Every lab job, in install order.
pub const ALL: [&str; 9] = [BRAIN, IMPROVE, DASH, TRAINER, TICK, MONITOR, SUPERVISOR, RESOURCEWATCH, WATCHDOG];
/// The always-on jobs (the monitor revives them when they have no process).
pub const ALWAYS_ON: [&str; 4] = [BRAIN, IMPROVE, DASH, TRAINER];

/// The installed copy of the lab binary that the jobs run (so a rebuild never disturbs a running job).
pub fn stable_binary(cfg: &Config) -> PathBuf {
    cfg.root.join("bin").join(platform::exe("qwen-lab"))
}

fn lab_job(cfg: &Config, bin: &Path, config_path: &Path, label: &str, args: &[&str], schedule: Schedule) -> Job {
    let mut full: Vec<&str> = vec!["--config"];
    let cp = config_path.display().to_string();
    full.push(&cp);
    full.extend_from_slice(args);
    let mut j = Job::new(label, bin, &full, schedule);
    j.work_dir = Some(cfg.root.clone());
    // A config may live in the data root, not beside the source checkout. Only
    // infer a sibling bot repo when it actually has the bot's source layout.
    let explicit = std::env::var_os("MOOMOO_BOT_DIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    let bot_repo = explicit.or_else(|| repo_near_config(config_path)).or_else(|| {
        let fallback = platform::bot_repo();
        fallback.join("src/bin/bot").is_dir().then_some(fallback)
    });
    if let Some(bot_repo) = bot_repo {
        j.env.push(("MOOMOO_BOT_DIR".into(), bot_repo.display().to_string()));
    }
    j
}

fn repo_near_config(config_path: &Path) -> Option<PathBuf> {
    let candidate = config_path.parent()?.parent()?;
    (candidate.join("Cargo.toml").is_file() && candidate.join("src/bin/bot").is_dir()).then(|| candidate.to_path_buf())
}

fn lab_repo_near_config(config_path: &Path) -> Option<PathBuf> {
    let candidate = config_path.parent()?;
    (candidate.join("Cargo.toml").is_file() && candidate.join("src/bin/qwen-ops.rs").is_file()).then(|| candidate.to_path_buf())
}

fn logs(j: &mut Job, cfg: &Config, out: &str, err: &str) {
    j.stdout = Some(cfg.logs_dir().join(out));
    j.stderr = Some(cfg.logs_dir().join(err));
}

/// The hourly learning tick (`tick --auto`, at minute 7).
pub fn tick(cfg: &Config, bin: &Path, config_path: &Path) -> Job {
    let mut j = lab_job(cfg, bin, config_path, TICK, &["tick", "--auto"], Schedule::Hourly { minute: 7 });
    logs(&mut j, cfg, "launchd.out.log", "launchd.err.log");
    j
}

/// The read-only dashboard (always on).
pub fn dash(cfg: &Config, bin: &Path, config_path: &Path) -> Job {
    let mut j = lab_job(cfg, bin, config_path, DASH, &["dash", "serve"], Schedule::KeepAlive { throttle_secs: 30 });
    logs(&mut j, cfg, "dash.out.log", "dash.err.log");
    j
}

/// The always-on brain (one role every few minutes), low priority.
pub fn brain(cfg: &Config, bin: &Path, config_path: &Path) -> Job {
    let mut j = lab_job(cfg, bin, config_path, BRAIN, &["brain", "serve"], Schedule::KeepAlive { throttle_secs: 60 });
    j.nice = Some(10);
    logs(&mut j, cfg, "brain.out.log", "brain.err.log");
    j
}

/// The deep-learning trainer (always on), lowest priority.
pub fn trainer(cfg: &Config, bin: &Path, config_path: &Path) -> Job {
    let mut j = lab_job(cfg, bin, config_path, TRAINER, &["trainer"], Schedule::KeepAlive { throttle_secs: 60 });
    j.nice = Some(15);
    j.low_io = true;
    logs(&mut j, cfg, "trainer.out.log", "trainer.err.log");
    j
}

/// The strategy test loop (always on), lowest priority.
pub fn improve(cfg: &Config, bin: &Path, config_path: &Path) -> Job {
    let mut j = lab_job(cfg, bin, config_path, IMPROVE, &["improve", "--forever"], Schedule::KeepAlive { throttle_secs: 60 });
    j.nice = Some(19);
    j.low_io = true;
    j.stdout = Some(cfg.logs_dir().join("improve.log"));
    j
}

/// A job that runs a `qwen-ops` subcommand (monitor, supervisor, watch, watchdog).
fn ops_job(cfg: &Config, config_path: &Path, label: &str, sub: &str, schedule: Schedule, out: &str) -> Job {
    let config = config_path.display().to_string();
    let mut j = Job::new(label, &platform::tool("qwen-ops"), &[sub, "--config", &config], schedule);
    j.work_dir = Some(cfg.root.clone());
    j.env = vec![("QWEN_LAB_ROOT".into(), cfg.root.display().to_string())];
    if let Some(repo) = std::env::var_os("QWEN_LAB_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| lab_repo_near_config(config_path)) {
        j.env.push(("QWEN_LAB_DIR".into(), repo.display().to_string()));
    }
    if let Some(bot) = std::env::var_os("MOOMOO_BOT_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| repo_near_config(config_path)) {
        j.env.push(("MOOMOO_BOT_DIR".into(), bot.display().to_string()));
    }
    j.nice = Some(10);
    j.stdout = Some(cfg.logs_dir().join(out));
    j
}

/// Every lab job with the paths of this machine.
pub fn all(cfg: &Config, config_path: &Path) -> Vec<Job> {
    let bin = stable_binary(cfg);
    vec![
        brain(cfg, &bin, config_path),
        improve(cfg, &bin, config_path),
        dash(cfg, &bin, config_path),
        trainer(cfg, &bin, config_path),
        tick(cfg, &bin, config_path),
        ops_job(cfg, config_path, MONITOR, "monitor", Schedule::Every { secs: 900, at_load: true }, "monitor.launchd.log"),
        ops_job(cfg, config_path, SUPERVISOR, "supervisor", Schedule::Hourly { minute: 10 }, "supervisor.launchd.log"),
        ops_job(cfg, config_path, RESOURCEWATCH, "watch", Schedule::Every { secs: 300, at_load: false }, "qwen-ops-watch.err"),
        ops_job(cfg, config_path, WATCHDOG, "watchdog", Schedule::Every { secs: 600, at_load: false }, "qwen-ops-watchdog.err"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_repo_is_only_inferred_from_a_verified_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("lab").join("lab.toml");
        assert_eq!(repo_near_config(&cfg), None);
        std::fs::create_dir_all(tmp.path().join("src/bin/bot")).unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"bot\"\n").unwrap();
        assert_eq!(repo_near_config(&cfg), Some(tmp.path().to_path_buf()));
    }

    #[test]
    fn ops_jobs_use_the_installed_config_and_verified_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let lab = tmp.path().join("lab");
        let bot = tmp.path();
        std::fs::create_dir_all(lab.join("src/bin")).unwrap();
        std::fs::create_dir_all(bot.join("src/bin/bot")).unwrap();
        std::fs::write(lab.join("Cargo.toml"), "[package]\nname = \"lab\"\n").unwrap();
        std::fs::write(lab.join("src/bin/qwen-ops.rs"), "").unwrap();
        std::fs::write(bot.join("Cargo.toml"), "[package]\nname = \"bot\"\n").unwrap();
        let cfg_path = lab.join("lab.toml");
        let cfg = Config::for_root(&tmp.path().join("data"), "real");
        let job = ops_job(&cfg, &cfg_path, SUPERVISOR, "supervisor", Schedule::Daily(vec![(1, 10)]), "supervisor.launchd.log");
        assert_eq!(job.args, vec!["supervisor", "--config", cfg_path.to_str().unwrap()]);
        let expected_lab = std::env::var_os("QWEN_LAB_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or(lab);
        let expected_bot = std::env::var_os("MOOMOO_BOT_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| bot.to_path_buf());
        assert!(job.env.contains(&("QWEN_LAB_DIR".into(), expected_lab.display().to_string())));
        assert!(job.env.contains(&("MOOMOO_BOT_DIR".into(), expected_bot.display().to_string())));
    }

    /// Test: the table has every job once, with the schedules the lab relies on.
    #[test]
    fn the_table_has_every_job_with_its_schedule() {
        let cfg = Config::for_root(Path::new("/tmp/lab"), "real");
        let jobs = all(&cfg, Path::new("/tmp/lab/lab.toml"));
        let labels: Vec<&str> = jobs.iter().map(|j| j.label.as_str()).collect();
        assert_eq!(labels, ALL.to_vec());
        let by = |l: &str| jobs.iter().find(|j| j.label == l).unwrap();
        assert_eq!(by(TICK).schedule, Schedule::Hourly { minute: 7 });
        assert_eq!(by(SUPERVISOR).schedule, Schedule::Hourly { minute: 10 });
        assert_eq!(by(WATCHDOG).schedule, Schedule::Every { secs: 600, at_load: false });
        for l in ALWAYS_ON {
            assert!(matches!(by(l).schedule, Schedule::KeepAlive { .. }), "{l}");
        }
        assert_eq!(by(IMPROVE).args, vec!["--config", "/tmp/lab/lab.toml", "improve", "--forever"]);
        assert!(by(TRAINER).low_io && by(TRAINER).nice == Some(15));
        assert!(LAB_BINARY_JOBS.iter().all(|l| ALL.contains(l)));
    }

    /// Test: the macOS plist of the tick job still runs it at minute 7 with the launchd logs.
    #[test]
    fn the_tick_plist_runs_hourly_at_minute_seven() {
        let cfg = Config::for_root(Path::new("/tmp/lab & co"), "real");
        let p = crate::service::plist(&tick(&cfg, Path::new("/tmp/lab & co/bin/qwen-lab"), Path::new("/tmp/lab & co/lab.toml")));
        assert!(p.contains("<string>tick</string>") && p.contains("<string>--auto</string>"));
        assert!(p.contains("<key>StartCalendarInterval</key><dict><key>Minute</key><integer>7</integer></dict>") && p.contains("<key>RunAtLoad</key><false/>"));
        assert!(p.contains("/tmp/lab &amp; co/bin/qwen-lab") && p.contains("launchd.err.log"));
    }
}

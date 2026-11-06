//! `bot-ops deploy` (was deploy.sh): build, test, lint, install the trading service and register every bot job. The store, parameter records and paper
//! state live outside the installed binary and are never deleted (owner rule 2026-10-06: never wipe data). The service is restarted only when the program or
//! its installed inputs changed (28 needless restarts on 2026-10-11 taught that).

use crate::common::{self, BOT_LABEL};
use crate::platform::{self, Os};
use crate::service::{self, Job, Schedule};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Stable macOS signing identifier, so a LuLu firewall rule keeps matching after rebuilds.
const SIGN_ID: &str = "claude-moomoo-bot";

fn bin_dir() -> PathBuf {
    common::runtime().join("bin")
}
fn installed_bot() -> PathBuf {
    bin_dir().join(platform::exe("bot"))
}
fn installed_ops() -> PathBuf {
    bin_dir().join(platform::exe("bot-ops"))
}

/// The trading service job: always on, restarted 30 s after an exit; macOS keeps the machine awake with caffeinate.
pub fn bot_job() -> Job {
    let bot = installed_bot();
    let rt = common::runtime();
    let mut j = if platform::os() == Os::Mac {
        Job::new(
            BOT_LABEL,
            Path::new("/usr/bin/caffeinate"),
            &["-i", "-s", &bot.display().to_string(), "serve"],
            Schedule::KeepAlive { throttle_secs: 30 },
        )
    } else {
        Job::new(
            BOT_LABEL,
            &bot,
            &["serve"],
            Schedule::KeepAlive { throttle_secs: 30 },
        )
    };
    j.work_dir = Some(rt.clone());
    j.env = vec![("RUST_LOG".into(), "info".into())];
    j.stdout = Some(rt.join("logs/bot.out"));
    j.stderr = Some(rt.join("logs/bot.err"));
    j
}

/// The bot's other jobs (all run `bot-ops <subcommand>`).
pub fn other_jobs() -> Vec<Job> {
    let ops = installed_ops();
    let repo = common::repo();
    let rt = common::runtime();
    let mk = |label: &str, args: &[&str], schedule: Schedule, out: PathBuf| {
        let mut j = Job::new(label, &ops, args, schedule);
        j.work_dir = Some(repo.clone());
        j.env = vec![
            ("MOOMOO_BOT_DIR".into(), repo.display().to_string()),
            (
                "QWEN_LAB_DIR".into(),
                common::lab_repo().display().to_string(),
            ),
            (
                "QWEN_LAB_ROOT".into(),
                common::lab_root().display().to_string(),
            ),
        ];
        j.stdout = Some(out);
        j
    };
    vec![
        // Maintenance and publication jobs must be in the same table that deploy installs.
        mk(
            "com.claudemoomoobot.promote",
            &["promote"],
            Schedule::Hourly { minute: 35 },
            rt.join("logs/promote.launchd.log"),
        ),
        mk(
            "com.claudemoomoobot.logdoctor",
            &["log-doctor"],
            Schedule::Hourly { minute: 55 },
            rt.join("logs/log_doctor.launchd.log"),
        ),
        mk(
            "com.claudemoomoobot.fixreport",
            &["fix-report"],
            Schedule::Daily(vec![(0, 50)]),
            rt.join("logs/fix_report.launchd.log"),
        ),
        mk(
            "com.claudemoomoobot.monitor",
            &["monitor", "check"],
            Schedule::Every {
                secs: 900,
                at_load: true,
            },
            repo.join("ops/logs/launchd-monitor.out"),
        ),
        mk(
            "com.claudemoomoobot.labexport",
            &["lab-export"],
            Schedule::Daily(vec![(8, 30)]),
            repo.join("ops/logs/lab_export.launchd.log"),
        ),
        // Historical-data importer: checks hourly so expired 7-day slots are used promptly; it never places orders.
        mk(
            "com.claudemoomoobot.universefetch",
            &["universe-fetch"],
            Schedule::Every {
                secs: 3600,
                at_load: true,
            },
            repo.join("ops/logs/universe_fetch.log"),
        ),
        mk(
            "com.claudemoomoobot.fillprobe",
            &["fill-probe"],
            Schedule::Daily(vec![(0, 40)]),
            repo.join("ops/logs/fill_probe.launchd.log"),
        ),
    ]
}

fn run_step(what: &str, cmd: &mut Command) -> bool {
    println!("deploy: {what}");
    match cmd
        .current_dir(common::repo())
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            eprintln!(
                "deploy failed at '{what}':\n{}",
                text.lines()
                    .rev()
                    .take(40)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            false
        }
        Err(e) => {
            eprintln!("deploy failed at '{what}': {e}");
            false
        }
    }
}

/// The hash recorded for "same program, same inputs": sha256 of `<bot sha>\n<holdings sha>  data/spy_holdings.csv\n` (the text the shell version hashed).
pub fn inputs_sha(bot_sha: &str, holdings_sha: Option<&str>) -> String {
    let mut t = format!("{bot_sha}\n");
    if let Some(h) = holdings_sha {
        t.push_str(&format!("{h}  data/spy_holdings.csv\n"));
    }
    common::sha256_text(&t)
}

pub fn deploy() -> i32 {
    let repo = common::repo();
    let rt = common::runtime();
    let cargo = platform::which("cargo").unwrap_or_else(|| PathBuf::from("cargo"));
    if !run_step(
        "cargo build --release",
        Command::new(&cargo).args(["build", "--release", "--bin", "bot", "--bin", "bot-ops"]),
    ) {
        return 1;
    }
    // --test-threads=1: governor::tests::busy_loop_is_held_near_target reads the whole process' CPU time, so cargo's parallel runner lets sibling tests' CPU
    // leak into that measurement and can fail a correct governor under machine load (seen 2026-10-10).
    if !run_step(
        "cargo test --release --all-targets",
        Command::new(&cargo).args([
            "test",
            "--release",
            "--all-targets",
            "--",
            "--test-threads=1",
        ]),
    ) {
        return 1;
    }
    if !run_step(
        "cargo clippy --release --all-targets -D warnings",
        Command::new(&cargo).args([
            "clippy",
            "--release",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ]),
    ) {
        return 1;
    }
    let built = repo.join("target/release").join(platform::exe("bot"));
    let new_sha = common::sha256_file(&built).unwrap_or_default();
    let holdings = repo.join("data/spy_holdings.csv");
    let inputs = inputs_sha(&new_sha, common::sha256_file(&holdings).as_deref());
    let sha_file = bin_dir().join("bot.inputs-sha256");
    let force = std::env::var("DEPLOY_RESTART").is_ok_and(|v| v == "1");
    let st = service::state(BOT_LABEL);
    let running = !platform::processes(&installed_bot().display().to_string()).is_empty();
    let same = fs::read_to_string(&sha_file).is_ok_and(|s| s.trim() == inputs);
    let restart = force || !installed_bot().exists() || !same || !st.loaded || !running;
    let _ = fs::create_dir_all(rt.join("logs"));
    let _ = fs::create_dir_all(bin_dir());
    if !restart {
        // current with the newest commit (the monitor compares this time with the last bot commit)
        let _ = fs::OpenOptions::new()
            .write(true)
            .open(installed_bot())
            .and_then(|f| f.set_modified(std::time::SystemTime::now()));
    } else {
        service::unload(BOT_LABEL);
        for _ in 0..30 {
            if !service::state(BOT_LABEL).loaded {
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        // Linux may retain a stopped unit in systemd's transient loaded set until its next
        // garbage-collection pass. The unit file has already been removed by unload(), so a
        // non-running stale state is safe to replace.
        if service::state(BOT_LABEL).loaded && service::state(BOT_LABEL).running {
            eprintln!("deploy failed: the service is still loaded");
            return 1;
        }
        if !platform::processes(&installed_bot().display().to_string()).is_empty() {
            eprintln!("deploy failed: the bot process is still running");
            return 1;
        }
        if let Err(e) = install_bot(&built) {
            eprintln!("deploy failed: {e}");
            return 1;
        }
        if holdings.exists() {
            let _ = fs::copy(&holdings, rt.join("spy_holdings.csv"));
        }
        match service::install(&bot_job()) {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("deployment failed: {e:#}");
                return 1;
            }
        }
        if !service::state(BOT_LABEL).loaded {
            eprintln!("deployment failed: the service is not loaded");
            return 1;
        }
        let _ = fs::write(&sha_file, format!("{inputs}\n"));
    }
    // the helper binary the jobs run (a stable copy, so a rebuild never disturbs a running job)
    if let Err(e) = install_ops(&repo.join("target/release").join(platform::exe("bot-ops"))) {
        eprintln!("deploy: installing bot-ops failed: {e}");
        return 1;
    }
    install_jobs();
    install_hooks();
    if !restart {
        println!("bot program unchanged: trading service kept running (tests, lint and jobs done)");
    }
    println!(
        "deployed: {} serve (console http://127.0.0.1:8789, logs {})",
        installed_bot().display(),
        rt.join("logs/bot.err").display()
    );
    0
}

/// `bot-ops install-service`: installs the already built bot and registers ONLY the trading service (restarted by the system if it stops, started at login),
/// without the tests, the lint run or the maintenance jobs (some of those start headless Claude sessions). The service trades the SIMULATE account only.
pub fn install_service_cmd() -> i32 {
    let repo = common::repo();
    let rt = common::runtime();
    let built = repo.join("target/release").join(platform::exe("bot"));
    if !built.exists() {
        eprintln!("the bot is not built yet: run `cargo build --release --bin bot`");
        return 1;
    }
    let _ = fs::create_dir_all(rt.join("logs"));
    let _ = fs::create_dir_all(bin_dir());
    service::unload(BOT_LABEL);
    for _ in 0..30 {
        if !service::state(BOT_LABEL).loaded {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    if !platform::processes(&installed_bot().display().to_string()).is_empty() {
        eprintln!("install-service failed: the installed bot is still running");
        return 1;
    }
    if let Err(e) = install_bot(&built) {
        eprintln!("install-service failed: {e}");
        return 1;
    }
    let holdings = repo.join("data/spy_holdings.csv");
    if holdings.exists() {
        let _ = fs::copy(&holdings, rt.join("spy_holdings.csv"));
    }
    match service::install(&bot_job()) {
        Ok(m) => println!("{m}"),
        Err(e) => {
            eprintln!("install-service failed: {e:#}");
            return 1;
        }
    }
    if !service::state(BOT_LABEL).loaded {
        eprintln!("install-service failed: the service is not loaded");
        return 1;
    }
    println!(
        "trading service installed: {} serve (console http://127.0.0.1:8789, logs {})",
        installed_bot().display(),
        rt.join("logs/bot.err").display()
    );
    0
}

/// `bot-ops install-fetch`: installs the helper binary and registers ONLY the hourly historical-candle importer (`universe-fetch`), which fetches ThomasAi's weekly
/// pick first within the weekly quota and never places an order. None of the other jobs (some start headless Claude sessions) is registered.
pub fn install_fetch_cmd() -> i32 {
    let repo = common::repo();
    let built = repo.join("target/release").join(platform::exe("bot-ops"));
    if !built.exists() {
        eprintln!("bot-ops is not built yet: run `cargo build --release --bin bot-ops`");
        return 1;
    }
    if let Err(e) = install_ops(&built) {
        eprintln!("install-fetch failed: {e}");
        return 1;
    }
    let mut done = 0;
    for job in other_jobs()
        .into_iter()
        .filter(|j| j.label.ends_with(".universefetch"))
    {
        match service::install(&job) {
            Ok(m) => {
                println!("{m}");
                done += 1;
            }
            Err(e) => {
                eprintln!("install-fetch failed: {e:#}");
                return 1;
            }
        }
    }
    if done == 0 {
        eprintln!("install-fetch failed: the importer job is not in the job table");
        return 1;
    }
    0
}

/// Copies the built bot to its runtime folder (macOS: signed with the stable identifier).
fn install_bot(built: &Path) -> std::io::Result<()> {
    let dst = installed_bot();
    let tmp = dst.with_file_name(format!(
        "{}.new",
        dst.file_name().and_then(|n| n.to_str()).unwrap_or("bot")
    ));
    fs::copy(built, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    }
    if platform::os() == Os::Mac {
        let ok = Command::new("codesign")
            .args(["--force", "--sign", "-", "--identifier", SIGN_ID])
            .arg(&tmp)
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            return Err(std::io::Error::other("codesign failed"));
        }
    }
    common::replace_file(&tmp, &dst)
}

fn install_ops(built: &Path) -> std::io::Result<()> {
    for dst in [installed_ops(), platform::tool("bot-ops")] {
        if let Some(d) = dst.parent() {
            fs::create_dir_all(d)?;
        }
        let tmp = dst.with_file_name(format!(
            "{}.new",
            dst.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("bot-ops")
        ));
        fs::copy(built, &tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        }
        common::replace_file(&tmp, &dst)?;
    }
    Ok(())
}

/// Registers (or refreshes) every bot job whose definition changed or that is not loaded.
fn install_jobs() {
    let dir = common::runtime().join("jobs");
    let _ = fs::create_dir_all(&dir);
    for job in other_jobs() {
        let def = common::sha256_text(&format!(
            "{:?}",
            (
                &job.label,
                &job.program,
                &job.args,
                &job.schedule,
                &job.work_dir,
                &job.env,
                &job.stdout
            )
        ));
        let f = dir.join(format!("{}.sha", job.label));
        let unchanged = fs::read_to_string(&f).is_ok_and(|s| s.trim() == def);
        if unchanged && service::state(&job.label).loaded {
            continue;
        }
        match service::install(&job) {
            Ok(_) => {
                let _ = fs::write(&f, format!("{def}\n"));
            }
            Err(e) => eprintln!("deploy: job {} failed: {e:#}", job.label),
        }
    }
    let l = service::enable_linger();
    if !l.is_empty() {
        println!("{l}");
    }
}

/// `bot-ops install`: registers the jobs without building.
pub fn install_jobs_cmd() -> i32 {
    if !installed_bot().exists() {
        eprintln!("the bot is not installed yet: run `bot-ops deploy` (it builds, tests and installs everything)");
        return 1;
    }
    install_jobs();
    install_hooks();
    println!("jobs registered");
    0
}

/// Post-commit hooks push every commit (both repos) and mirror the lab into lab/ of this repo; the webhook guard hooks keep the Discord URL out of git.
pub fn install_hooks() {
    let repo = common::repo();
    let ops = installed_ops().display().to_string().replace('\\', "/");
    let hook = format!("#!/bin/sh\n# installed by bot-ops deploy: push every commit (bot-ops push-guard)\n(\"{ops}\" push-guard \"{}\" >/dev/null 2>&1 &)\n", repo.display().to_string().replace('\\', "/"));
    write_hook(&repo, "post-commit", &hook);
    let lab = common::lab_repo();
    if lab.join(".git").exists() {
        let hook = format!("#!/bin/sh\n# installed by the bot's bot-ops deploy: push every lab commit, then mirror it into the bot repo lab/ (which pushes too)\n(\"{ops}\" push-guard \"{lab}\" >/dev/null 2>&1; sleep 2; \"{ops}\" sync-lab >> \"{log}\" 2>&1) &\n",
            lab = lab.display().to_string().replace('\\', "/"), log = common::ops_logs().join("sync_lab.log").display().to_string().replace('\\', "/"));
        write_hook(&lab, "post-commit", &hook);
    }
    let guard = platform::tool("qwen-guard");
    if guard.exists() {
        // the webhook can never reach git (pre-commit / pre-push)
        let _ = Command::new(guard)
            .arg("install-hooks")
            .arg(&repo)
            .arg(&lab)
            .stdin(Stdio::null())
            .status();
    }
}

fn write_hook(repo: &Path, name: &str, text: &str) {
    let dir = repo.join(".git/hooks");
    if !dir.is_dir() {
        return;
    }
    let p = dir.join(name);
    if fs::write(&p, text).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&p, fs::Permissions::from_mode(0o755));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: the inputs hash is the hash of the text the shell version hashed (so a switch from deploy.sh does not restart the bot).
    #[test]
    fn the_inputs_hash_keeps_the_shell_format() {
        let h = inputs_sha("abc", Some("def"));
        assert_eq!(h, common::sha256_text("abc\ndef  data/spy_holdings.csv\n"));
        assert_eq!(inputs_sha("abc", None), common::sha256_text("abc\n"));
    }

    /// Test: the bot job is an always-on service that logs to the runtime folder, and the other jobs run bot-ops.
    #[test]
    fn the_job_table_is_complete() {
        let j = bot_job();
        assert_eq!(j.label, BOT_LABEL);
        assert!(matches!(
            j.schedule,
            Schedule::KeepAlive { throttle_secs: 30 }
        ));
        assert!(j.stderr.as_ref().unwrap().ends_with("logs/bot.err"));
        let labels: Vec<String> = other_jobs().iter().map(|j| j.label.clone()).collect();
        assert_eq!(
            labels,
            [
                "promote",
                "logdoctor",
                "fixreport",
                "monitor",
                "labexport",
                "universefetch",
                "fillprobe"
            ]
            .map(|n| format!("com.claudemoomoobot.{n}"))
        );
        let jobs = other_jobs();
        let schedule = |name: &str| {
            jobs.iter()
                .find(|j| j.label.ends_with(name))
                .unwrap()
                .schedule
                .clone()
        };
        assert!(matches!(
            schedule("logdoctor"),
            Schedule::Hourly { minute: 55 }
        ));
        assert!(matches!(schedule("fixreport"), Schedule::Daily(times) if times == vec![(0, 50)]));
        assert!(matches!(
            schedule("monitor"),
            Schedule::Every {
                secs: 900,
                at_load: true
            }
        ));
        assert!(other_jobs()
            .iter()
            .all(|j| j.program.to_string_lossy().contains("bot-ops")));
        assert!(other_jobs()
            .iter()
            .all(|j| j.env.iter().any(|e| e.0 == "QWEN_LAB_ROOT")));
    }
}

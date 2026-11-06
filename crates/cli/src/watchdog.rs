//! `thomas watchdog`: runs every 10 minutes (launchd) and repairs what is safe to repair without a person.
//!
//! * more than one competition-bot chain (each supervisor starts its own bot, each bot downloads and can order): everything except the launchd-owned chain is stopped;
//! * the competition bot running older code than its files: stopped, so the supervisor restarts it;
//! * the portal or the installed candle importer older than the code: restarted / reinstalled;
//! * local-only repository policy: the watchdog never performs network writes or `git push`;
//! * the thomas binary built from older sources than the committed code: rebuilt and re-signed (only when the crates have no uncommitted edits).
//!
//! What it did is appended to `data/watchdog.log` and summarised in `data/watchdog.json`. It never touches an account.
use std::path::Path;
use std::process::Command;

use crate::deploy;
use crate::ops;

#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub cmd: String,
}

/// Rows of `ps -axo pid=,ppid=,command=`.
pub fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (pid, ppid) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
            let cmd = it.collect::<Vec<_>>().join(" ");
            (!cmd.is_empty()).then_some(Proc { pid, ppid, cmd })
        })
        .collect()
}

/// Whether `cmd` is a python process running `-m <module>` (not a program that merely mentions the module in its arguments, such as a headless `claude -p` session whose prompt names it).
pub fn is_module(cmd: &str, module: &str) -> bool {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    tokens
        .first()
        .is_some_and(|exe| exe.to_lowercase().contains("python"))
        && tokens.windows(2).any(|w| w[0] == "-m" && w[1] == module)
}

/// Pids to stop so that only the chain under `keep_supervisor` (the one launchd runs) remains: every other supervisor and every bot whose parent is not it.
pub fn extra_chains(procs: &[Proc], keep_supervisor: u32) -> Vec<u32> {
    procs
        .iter()
        .filter(|p| {
            (is_module(&p.cmd, "papercompmoomoo.supervisor") && p.pid != keep_supervisor)
                || (is_module(&p.cmd, "papercompmoomoo.bot") && p.ppid != keep_supervisor)
        })
        .map(|p| p.pid)
        .collect()
}

/// "ok" or the error text of a command result.
fn outcome(r: Result<String, String>) -> String {
    r.map_or_else(|e| e, |_| "ok".to_string())
}

fn sh(dir: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    let o = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// One pass. Returns the actions taken (and failures) as text lines.
pub fn run(root: &Path, _now_ms: i64) -> Vec<String> {
    let mut log: Vec<String> = Vec::new();
    let mut did = |s: String| log.push(s);
    // 1. one competition-bot chain
    let ps = sh(root, "ps", &["-axo", "pid=,ppid=,command="]).unwrap_or_default();
    let procs = parse_ps(&ps);
    if let Some(keep) = ops::job_pid("local.papercompmoomoo.supervisor") {
        for pid in extra_chains(&procs, keep) {
            let r = sh(root, "kill", &[&pid.to_string()]);
            did(format!(
                "stopped extra competition-bot process {pid}: {}",
                outcome(r)
            ));
        }
    }
    // 2. the competition bot on older code than its files
    let status: Option<serde_json::Value> =
        std::fs::read_to_string(root.join("papercompmoomoo/data/status.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
    let reported = status
        .as_ref()
        .and_then(|s| s["code_hash"].as_str().map(str::to_string));
    if let (Some(h), Some(keep)) = (reported, ops::job_pid("local.papercompmoomoo.supervisor"))
        && h != deploy::sibling_disk_hash(root)
        && let Some(bot) = procs
            .iter()
            .find(|p| is_module(&p.cmd, "papercompmoomoo.bot") && p.ppid == keep)
    {
        let r = sh(root, "kill", &[&bot.pid.to_string()]);
        did(format!(
            "restarted the competition bot (it ran older code): {}",
            outcome(r)
        ));
    }
    // 3. the thomas binary older than the committed code
    let crates_dirty = sh(
        root,
        "git",
        &["status", "--porcelain", "crates", "bot/src", "bot/lab/src"],
    )
    .map(|s| !s.is_empty())
    .unwrap_or(true);
    if !crates_dirty
        && deploy::BUILT_FROM
            != thomas_core::source_hash::source_hash(
                root,
                &thomas_core::source_hash::BUILD_DIRS,
                &thomas_core::source_hash::BUILD_EXTENSIONS,
            )
    {
        let built = sh(root, "nice", &["-n", "19", "cargo", "build", "--release"]);
        match built {
            Ok(_) => {
                let _ = sh(
                    root,
                    "codesign",
                    &[
                        "-f",
                        "-s",
                        "-",
                        "-i",
                        "com.thomasai.thomas",
                        "target/release/thomas",
                    ],
                );
                did("rebuilt and re-signed thomas (it was built from older sources)".into());
            }
            Err(e) => did(format!(
                "could not rebuild thomas: {}",
                e.lines().last().unwrap_or("")
            )),
        }
    }
    // 4. the portal and the importer copy
    if deploy::problems(root)
        .iter()
        .any(|p| p.text.contains("portal started before"))
    {
        let r = ops::restart_job(ops::PORTAL_LABEL);
        did(format!(
            "restarted the portal onto the current binary: {}",
            outcome(r)
        ));
    }
    if deploy::problems(root)
        .iter()
        .any(|p| p.text.contains("installed candle importer"))
    {
        let r = sh(
            &root.join("bot"),
            "target/release/bot-ops",
            &["install-fetch"],
        );
        did(format!("reinstalled the candle importer: {}", outcome(r)));
    }
    log
}

/// Appends the result to the log and writes the summary the portal shows.
pub fn record(root: &Path, now_ms: i64, actions: &[String]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("data/watchdog.log"))?;
    for a in actions {
        writeln!(f, "[{}] {a}", ops::utc(now_ms))?;
    }
    std::fs::write(
        root.join("data/watchdog.json"),
        serde_json::json!({"last_run_ms": now_ms, "last_run": ops::utc(now_ms), "actions": actions}).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = "  100     1 /usr/sbin/cfprefsd\n58866     1 python -m papercompmoomoo.supervisor\n58870 58866 python -m papercompmoomoo.bot\n61000 59999 python -m papercompmoomoo.supervisor\n61004 61000 python -m papercompmoomoo.bot\n62000     1 python -m papercompmoomoo.dashboard\n";

    #[test]
    fn ps_rows_are_parsed_and_junk_is_skipped() {
        let p = parse_ps(PS);
        assert_eq!(p.len(), 6);
        assert_eq!(
            (p[2].pid, p[2].ppid, p[2].cmd.as_str()),
            (58870, 58866, "python -m papercompmoomoo.bot")
        );
        assert!(parse_ps("not a row\n\n").is_empty());
    }

    #[test]
    fn only_the_chain_launchd_runs_survives_and_the_dashboard_is_never_touched() {
        let p = parse_ps(PS);
        assert_eq!(extra_chains(&p, 58866), vec![61000, 61004]);
        assert!(
            extra_chains(&p, 61000).contains(&58866),
            "the choice follows launchd, not age"
        );
        let only = parse_ps(
            "58866 1 python -m papercompmoomoo.supervisor\n58870 58866 python -m papercompmoomoo.bot\n",
        );
        assert!(extra_chains(&only, 58866).is_empty());
        // a bot whose supervisor is not the launchd one is stopped even when its own supervisor is gone
        let orphan = parse_ps(
            "58866 1 python -m papercompmoomoo.supervisor\n70000 1 python -m papercompmoomoo.bot\n",
        );
        assert_eq!(extra_chains(&orphan, 58866), vec![70000]);
    }

    #[test]
    fn a_session_that_only_mentions_the_bot_is_not_a_bot_process() {
        let p = parse_ps(
            "58866 1 /x/Python.app/Contents/MacOS/Python -m papercompmoomoo.supervisor\n80000 1 /Users/me/.local/bin/claude -p You are the monitor: kill the one papercompmoomoo.bot pid from ps\n80001 1 sh -c python -m papercompmoomoo.bot\n",
        );
        assert_eq!(
            extra_chains(&p, 58866),
            Vec::<u32>::new(),
            "the monitor's headless session and a shell wrapper are never stopped"
        );
        assert!(is_module(
            "/x/Python -m papercompmoomoo.bot",
            "papercompmoomoo.bot"
        ));
        assert!(!is_module(
            "claude -p papercompmoomoo.bot",
            "papercompmoomoo.bot"
        ));
    }

    #[test]
    fn watchdog_source_contains_no_git_push_path() {
        let source = include_str!("watchdog.rs");
        assert!(!source.contains("&[\"push\"]"));
    }

}

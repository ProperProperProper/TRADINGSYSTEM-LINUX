//! The quant supervisor (`qwen-ops supervisor`, hourly), a port of the former `supervisor/quant_supervisor.sh` that runs on macOS, Linux and Windows.
//! A Claude weekly-limit response ends the unattended run; Codex is reserved for interactive use.
//! (Owner, 2026-10-11: "you claude need an agent to make sure everything is automated and top tier learning testing and application"; "make this think like
//! a quant trader"; "fix things, don't leave anything in limbo. ever".)
//! One Claude Code session acts as the research lead and automation supervisor of the lab and the bot: automation health, a quant analysis of the testing,
//! improvements to testing / learning / applying in code, guidance to the brain roles (training-half facts only). Everything it changes is committed and
//! pushed; what it did and what comes next is kept in `supervisor/SUPERVISOR_STATE.md` (persistent context, read first next time). Nothing is left in limbo:
//! after the session any leftover change that passes the tests is committed, else a FAULT is logged (the hourly log doctor picks it up).

use crate::jobs;
use crate::monitor::tail;
use crate::platform::{self, Os};
use crate::service;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SESSION_LIMIT: Duration = Duration::from_secs(3300);

fn log_line(log: &Path, msg: &str) {
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(
            f,
            "{} {msg}",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
    }
}

fn hit_weekly_limit(report: &Path) -> bool {
    fs::read_to_string(report).is_ok_and(|text| text.to_ascii_lowercase().contains("weekly limit"))
}

fn run_agent(cmd: &mut Command, report: &Path) -> i32 {
    if let Ok(f) = fs::File::create(report) {
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
                Ok(Some(st)) => break st.code().unwrap_or(-1),
                Ok(None) if start.elapsed() > SESSION_LIMIT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break 142;
                }
                Ok(None) => std::thread::sleep(Duration::from_secs(5)),
                Err(_) => break -1,
            }
        },
        Err(_) => -1,
    }
}

/// Runs a helper tool and returns its combined output (stdin optional).
fn tool_output(tool: &str, args: &[&str], stdin: Option<&str>) -> String {
    let mut cmd = Command::new(platform::tool(tool));
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let Ok(mut child) = cmd.spawn() else {
        return format!("({tool} not installed)");
    };
    if let (Some(text), Some(mut si)) = (stdin, child.stdin.take()) {
        let _ = si.write_all(text.as_bytes());
    }
    child
        .wait_with_output()
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )
        })
        .unwrap_or_default()
}

fn snapshot(port: u16) -> String {
    ureq::get(&format!("http://127.0.0.1:{port}/api/snapshot"))
        .timeout(Duration::from_secs(60))
        .call()
        .ok()
        .and_then(|r| r.into_string().ok())
        .unwrap_or_default()
}

fn cut(lines: Vec<String>, width: usize) -> String {
    lines
        .into_iter()
        .map(|l| l.chars().take(width).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn completion_code(agent_code: i32, recovered_verified_work: bool) -> i32 {
    if recovered_verified_work {
        0
    } else {
        agent_code
    }
}

/// The bundle: everything the session needs, computed by code (cheap: no exploring), including ACTION ITEMS: every verdict and open problem the dashboard
/// shows (owner 2026-10-11: "it needs to analyse everything, inc this in the dash, all by itself and self improve").
fn bundle(lab: &Path, repo: &Path, cfg_path: &Path, port: u16) -> String {
    let q = |args: &[&str]| -> String {
        let bin = lab.join("bin").join(platform::exe("qwen-lab"));
        let mut a = vec!["--config", cfg_path.to_str().unwrap_or("")];
        a.extend_from_slice(args);
        Command::new(bin)
            .args(a)
            .stdin(Stdio::null())
            .output()
            .map(|o| {
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                )
            })
            .unwrap_or_default()
    };
    let snap = snapshot(port);
    let mut b = format!(
        "# Supervisor bundle {}\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")
    );
    b += "\n## Quant review, UNSEEN half (human view; NEVER quote these numbers to the brain roles)\n";
    b += &q(&["quant"]);
    b += "\n## Quant review, TRAINING half (what the agents and the planner see)\n";
    b += &q(&["quant", "--training"]);
    b += "\n## Entry gating\n";
    let mcp = Command::new(lab.join("bin").join(platform::exe("qwen-lab")))
        .args([
            "--config",
            cfg_path.to_str().unwrap_or(""),
            "mcp",
            "--agent",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut c) = mcp {
        if let Some(mut si) = c.stdin.take() {
            let _ = si.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"lab_entry_gating\",\"arguments\":{}}}\n");
        }
        if let Ok(o) = c.wait_with_output() {
            b += &tool_output(
                "qwen-tools",
                &["mcp-text"],
                Some(&String::from_utf8_lossy(&o.stdout)),
            );
        }
    }
    b += "\n## ACTION ITEMS: every verdict and open problem the dashboard shows (computed by code; act on them)\n";
    b += &tool_output("qwen-tools", &["action-items"], Some(&snap));
    b += "\n## Agents: tests and results\n";
    b += &tool_output("qwen-tools", &["agent-results"], Some(&snap));
    b += "\n## Automation health\n### background jobs (loaded, running, pid, last exit)\n";
    for l in crate::opsctl::status() {
        b += &format!("{l}\n");
    }
    b += "### newest lab tick\n";
    let mut ticks: Vec<PathBuf> = fs::read_dir(lab.join("logs"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("tick-") && n.ends_with(".log"))
        })
        .collect();
    ticks.sort_by_key(|p| std::cmp::Reverse(platform::mtime(p)));
    if let Some(t) = ticks.first() {
        b += &cut(
            fs::read_to_string(t)
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect(),
            200,
        );
        b += "\n";
    }
    b += "### the brain (last 12 role runs)\n";
    b += &cut(tail(&lab.join("logs/brain.log"), 12), 200);
    b += "\n### ML on the candles (daily tests)\n";
    b += &cut(tail(&lab.join("logs/candle-ml.txt"), 22), 240);
    b += "\n";
    b += &cut(tail(&lab.join("logs/candle-improve.txt"), 14), 240);
    b += "\n### improve loop (last 8 lines)\n";
    b += &cut(tail(&lab.join("logs/improve.log"), 8), 200);
    b += "\n### open warnings (ledger)\n";
    b += &tool_output("qwen-tools", &["warnings", "list"], None)
        .lines()
        .take(40)
        .collect::<Vec<_>>()
        .join("\n");
    b += "\n\n## Newest brain reports (verified parts)\n";
    let mut reps: Vec<PathBuf> = fs::read_dir(lab.join("brain/reports"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    reps.sort_by_key(|p| std::cmp::Reverse(platform::mtime(p)));
    for r in reps.iter().take(4) {
        b += &format!(
            "### {}\n",
            r.file_name().and_then(|n| n.to_str()).unwrap_or("")
        );
        b += &cut(
            fs::read_to_string(r)
                .unwrap_or_default()
                .lines()
                .take(12)
                .map(String::from)
                .collect(),
            240,
        );
        b += "\n";
    }
    b += "\n## Processes (duplicates?)\n";
    let mut procs: Vec<String> = platform::processes("qwen-lab")
        .into_iter()
        .chain(platform::processes("claude -p"))
        .chain(platform::processes("bot serve"))
        .map(|p| p.command.chars().take(120).collect())
        .collect();
    procs.sort();
    b += &procs.join("\n");
    b += "\n\n## Your previous state (supervisor/SUPERVISOR_STATE.md)\n";
    b += &fs::read_to_string(repo.join("supervisor/SUPERVISOR_STATE.md"))
        .unwrap_or_else(|_| "(first run)\n".into());
    b
}

/// The session prompt (the bundle path, the market state and the folders are filled in).
fn prompt(
    bundle_path: &Path,
    market: &str,
    lab: &Path,
    repo: &Path,
    bot: &Path,
    cfg_path: &Path,
) -> String {
    format!("You are the quant supervisor of an automated research lab and its SPY paper-trading bot: a quant-trader research lead
and the guardian of the automation. Read {b} first (everything is there, computed by code), then work within this session:
1. AUTOMATION: every job must run on schedule and produce results (lab tick hourly, improve loop continuous, the brain one role per 5 minutes,
   log doctor :55, fix report daily, monitors every 15 min, resource watch, bot service). Find and fix root causes of
   anything stopped, failing, stale or stuck. No limbo: finish what you start.
2. QUANT ANALYSIS of the testing: judge what the tests really show (paired per-block evidence, multiple testing, risk-adjusted
   results, overfitting decay, entry gating, why ideas fail). Decide the most valuable next improvement to how the lab TESTS,
   LEARNS or APPLIES (e.g. what the planner tests next, a better-motivated option family, risk-reducing entry ideas, sharper
   feedback to the agents, a missing check). Implement it in code with a unit test: lab code in {repo} (cargo test
   --release, cargo clippy --all-targets -- -D warnings, then `qwen-ops redeploy`); bot research code in
   {bot} (its CLAUDE.md rules; redeploy it with `bot-ops deploy`, market open or closed: it is paper only; now {market}).
3. GUIDE the brain where it helps: `{lab}/bin/qwen-lab --config {cfg} note --to brain-<role> \"...\"` (its roles read their inbox).
   Use ONLY training-half facts (what lab_experiments / lab_quant --training show): never an unseen-half number or a status the roles cannot see.
0. The candle research is done by the lab's own small models and BRAIN (docs/BRAIN.md): twelve code-and-statistics roles in one always-on process, the candle ML
   model (ridge with market-regime interactions, refitted every 14 sessions on older data, scored for the bot's own backtester as Signal::Ml / the searched
   entry gate mlgate), the deep-learning trainer (own autodiff, residual nets and Transformers, evolutionary search) and a daily improvement round (recency
   schemes, extra features, meta-labelling) judged against a trial ledger with paired, multiple-testing-adjusted intervals. Each run is a review point: check
   what the brain queued and what the tests said (brain report, logs/brain.log, logs/candle-ml.txt, logs/candle-improve.txt, the lab's results), name what to
   change, and remove blockers in code. The checked online ensemble is documented in docs/ONLINE_AI.md; two models must agree and Claude or an OpenCode free validator validates before a proposal. Never make model prose a trading verdict. Real data only; negative results
   are results. Hygiene: no duplicate requests or processes (jobs and processes are in the bundle), no invented facts; fix what causes them.
   Long-term owner goal: everything in Rust and the same on macOS, Linux and Windows. Port one shell helper to Rust when it is safe (tested, behaviour identical).
4. ACTION ITEMS (in the bundle): every verdict and open problem the dashboard shows. Each must end this session DONE
   (committed) or written in SUPERVISOR_STATE.md with its concrete next step for the next run. An item that needs new code
   means YOU write that code (research-only, tested, committed); the loop then tests it. Work on the highest-value items first.
5. Commit each finished change at once with a thorough message (problem, change, verification with real numbers, live impact)
   and git push. Update supervisor/SUPERVISOR_STATE.md in {repo} (under 60 lines: what you found, did, measured, and
   the next step) and commit it. Leave NOTHING uncommitted.
Install lab changes yourself after committing: `qwen-ops redeploy --config {cfg}` (builds, replaces the binary and the tools, registers every job again). Do NOT
create the MAINTENANCE flag (it pauses the automation, including you).
Never: touch the REAL account or send orders; change exits; edit the live strategy (src/engine/live_options.rs LIVE) or the
verdict rules / promotion thresholds (owner decisions; only the promotion job applies a fully validated variant); loosen any
check; upgrade or pull models or versions; wipe data, memory, records or the archive; invent data or results (every number
comes from the bundle or a command you ran); revert or commit work you did not make. Finish with a short report.
 Use sub-agents (the Agent tool) only for independent work that runs in parallel and saves time; keep them few and brief (session usage matters).",
        b = bundle_path.display(), market = market, lab = lab.display(), repo = repo.display(), bot = bot.display(), cfg = cfg_path.display())
}

/// One supervisor run.
pub fn run(args: &[String]) -> i32 {
    let lab = platform::lab_root();
    let (repo, bot) = (platform::lab_repo(), platform::bot_repo());
    let cfg_path = crate::opsctl::config_path(args);
    let dir = lab.join("supervisor/bundles");
    let _ = fs::create_dir_all(&dir);
    let log = lab.join("logs/supervisor.log");
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M").to_string();
    let port = crate::config::Config::load(&cfg_path)
        .map(|c| c.dashboard.port)
        .unwrap_or(8790);

    // when not to run
    let maint = lab.join("MAINTENANCE");
    if platform::mtime(&maint) > 0
        && chrono::Utc::now().timestamp() - platform::mtime(&maint) < 360 * 60
    {
        log_line(&log, "skip: owner maintenance");
        return 0;
    }
    if !platform::processes("claude -p").is_empty() {
        log_line(
            &log,
            "skip: another Claude Code session is running (retried next slot)",
        );
        return 0;
    }
    let Some(claude) = platform::claude_bin() else {
        log_line(&log, "FAULT: claude CLI missing");
        return 1;
    };
    for r in [&repo, &bot] {
        if platform::git(r, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty()) {
            log_line(&log, &format!("skip: {} has uncommitted work of another session (push_guard commits it after 1 h)", r.file_name().and_then(|n| n.to_str()).unwrap_or("repo")));
            return 0;
        }
    }
    let b = bundle(&lab, &repo, &cfg_path, port);
    let bpath = dir.join(format!("bundle-{stamp}.md"));
    let _ = fs::write(&bpath, &b);
    log_line(&log, &format!("bundle {} bytes", b.len()));
    let market = tool_output("qwen-tools", &["market"], None)
        .trim()
        .to_string();
    let p = prompt(&bpath, &market, &lab, &repo, &bot, &cfg_path);
    log_line(&log, &format!("session start (market {market})"));
    let session = dir.join(format!("session-{stamp}.md"));
    let os_tools = match platform::os() {
        Os::Mac => "Bash(launchctl:*)",
        Os::Linux => "Bash(systemctl:*)",
        Os::Windows => "Bash(schtasks:*)",
    };
    let lab_bin = lab.join("bin").join(platform::exe("qwen-lab"));
    let mut cmd = platform::command_for(&claude);
    cmd.current_dir(&repo)
        .args([
            "-p",
            &p,
            "--model",
            "claude-sonnet-5-5",
            "--no-session-persistence",
            "--permission-mode",
            "acceptEdits",
            "--add-dir",
        ])
        .arg(&bot)
        .arg("--add-dir")
        .arg(&lab)
        .args([
            "--allowedTools",
            "mcp__qwen-lab",
            "mcp__moomoo-bot",
            "Agent",
            "Read",
            "Edit",
            "Write",
            "Grep",
            "Glob",
            "Bash(git:*)",
            "Bash(cargo:*)",
            "Bash(./deploy.sh)",
            "Bash(./check.sh:*)",
            "Bash(tail:*)",
            "Bash(grep:*)",
            "Bash(sed:*)",
            "Bash(head:*)",
            "Bash(ls:*)",
            "Bash(cat:*)",
            "Bash(sqlite3:*)",
            os_tools,
            "Bash(qwen-ops:*)",
            "Bash(bot-ops:*)",
            "Bash(qwen-tools:*)",
            "Bash(curl:*)",
            "Bash(target/release/qwen-lab:*)",
            "Bash(./target/release/qwen-lab:*)",
            &format!("Bash({}:*)", lab_bin.display()),
        ])
        .stdin(Stdio::null());
    let mut code = run_agent(&mut cmd, &session);
    log_line(
        &log,
        &format!(
            "session exit {code} -> {}",
            session.file_name().and_then(|n| n.to_str()).unwrap_or("")
        ),
    );
    let final_report = session.clone();
    if code != 0 && hit_weekly_limit(&session) {
        log_line(
            &log,
            "Claude weekly limit; unattended Codex fallback disabled",
        );
    }
    // No limbo: test every crate in a shared worktree before committing any of
    // its changes. The lab can be a subdirectory of the bot repository.
    let mut handled = std::collections::HashSet::new();
    let mut recovered_verified_work = false;
    for r in [&repo, &bot] {
        let Some(root) = platform::git(r, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
        else {
            log_line(
                &log,
                &format!("FAULT: {} is not a git worktree", r.display()),
            );
            code = 1;
            continue;
        };
        if !handled.insert(root.clone())
            || platform::git(&root, &["status", "--porcelain"]).is_some_and(|s| s.is_empty())
        {
            continue;
        }
        let crates: Vec<&PathBuf> = [&repo, &bot]
            .into_iter()
            .filter(|p| {
                platform::git(p, &["rev-parse", "--show-toplevel"])
                    .is_some_and(|s| Path::new(&s) == root)
            })
            .collect();
        let pass = !crates.is_empty()
            && crates.iter().all(|p| {
                Command::new("cargo")
                    .args(["test", "--release", "--", "--test-threads=1"])
                    .current_dir(p)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            });
        if pass {
            let msg = format!("Quant supervisor: commit verified session changes\n\nThe supervisor session of {stamp} left changes; all crates in this worktree passed cargo test --release. See {} for the session report.", final_report.display());
            let committed = platform::git(&root, &["add", "-A"]).is_some()
                && platform::git(&root, &["commit", "-q", "-m", &msg]).is_some()
                && platform::git(&root, &["push", "-q"]).is_some();
            if committed {
                log_line(
                    &log,
                    &format!("committed leftover work in {} (tests pass)", root.display()),
                );
                recovered_verified_work = true;
            } else {
                log_line(
                    &log,
                    &format!(
                        "FAULT: could not commit or push leftover work in {}",
                        root.display()
                    ),
                );
                code = 1;
            }
        } else {
            log_line(&log, &format!("FAULT: {} has uncommitted supervisor changes that FAIL cargo test; left for the log doctor", root.display()));
            code = 1;
        }
    }
    // An agent can exhaust its output quota after it has finished writing a valid change and
    // report. If our independent release-test gate then commits and pushes that work, the scrum
    // run achieved its outcome and must be recorded as fresh rather than as a permanent failure.
    if recovered_verified_work && code != 0 {
        log_line(
            &log,
            &format!("recovered agent exit {code}: verified changes committed and pushed"),
        );
        code = completion_code(code, recovered_verified_work);
        log_line(
            &log,
            &format!(
                "session exit 0 -> {}",
                final_report
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
            ),
        );
    }
    if code != 0 {
        log_line(&log, &format!("FAULT: supervisor session exited {code}"));
        log_line(
            &log,
            &format!(
                "session exit {code} -> {}",
                final_report
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
            ),
        );
    }
    // the dashboard shows what the supervisor did (its state and the end of its newest report)
    let _ = fs::copy(
        repo.join("supervisor/SUPERVISOR_STATE.md"),
        dir.join("STATE.md"),
    );
    if let Ok(text) = fs::read(&final_report) {
        let from = text.len().saturating_sub(4000);
        let _ = fs::write(dir.join("last_report.md"), &text[from..]);
    }
    log_line(&log, "done");
    let _ = (service::state(jobs::SUPERVISOR), 0);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_limit_detection_stops_the_unattended_run() {
        let tmp = tempfile::tempdir().unwrap();
        let report = tmp.path().join("session.md");
        assert!(!hit_weekly_limit(&report));
        fs::write(
            &report,
            "You've hit your weekly limit · resets 8am (Australia/Melbourne)\n",
        )
        .unwrap();
        assert!(hit_weekly_limit(&report));
        fs::write(&report, "Authentication failed\n").unwrap();
        assert!(!hit_weekly_limit(&report));
    }

    #[test]
    fn verified_committed_fallback_work_is_a_successful_scrum_outcome() {
        assert_eq!(super::completion_code(1, true), 0);
        assert_eq!(super::completion_code(1, false), 1);
        assert_eq!(super::completion_code(0, false), 0);
    }
}

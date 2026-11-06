//! The bot's periodic data jobs: the quota-aware historical importer, the SPY holdings refresh and the one-off fill probe.

use crate::common;
use crate::platform;
use crate::pushguard::looks_secret;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SPY_URL: &str = "https://www.ssga.com/us/en/intermediary/library-content/products/fund-data/etfs/us/holdings-daily-us-en-spy.xlsx";

fn cargo() -> PathBuf {
    platform::which("cargo").unwrap_or_else(|| PathBuf::from("cargo"))
}
fn example(name: &str) -> PathBuf {
    common::repo()
        .join("target/release/examples")
        .join(platform::exe(name))
}

/// Refreshes the historical candle cache for eligible SPY holdings. The example enforces
/// moomoo's rolling quota, keeps the live reserve, paces requests and safely skips names
/// until a quota slot expires. It never places an order.
pub fn universe_fetch() -> i32 {
    let log = common::ops_logs().join("universe_fetch.log");
    if !run_logged(
        Command::new(cargo()).args(["build", "--release", "--example", "universe_fetch"]),
        &log,
    ) {
        common::log(&log, "build failed");
        return 1;
    }
    let ok = run_logged(&mut Command::new(example("universe_fetch")), &log);
    common::log(
        &log,
        if ok {
            "quota-aware universe import completed"
        } else {
            "universe import failed; will retry on the next scheduled run"
        },
    );
    i32::from(!ok)
}

/// Runs a command in the repo appending its output to `log`; true on success.
fn run_logged(cmd: &mut Command, log: &Path) -> bool {
    let f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .ok();
    if let Some(f) = &f {
        if let Ok(g) = f.try_clone() {
            cmd.stdout(g);
        }
        if let Ok(g) = f.try_clone() {
            cmd.stderr(g);
        }
    }
    cmd.current_dir(common::repo())
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// `bot-ops lab-export`: adds fresh 14-session look-forward backtests (oos / random / chosen, with entry features) to the research archive that the lab learns
/// from; refreshes SPY's holdings and pulls missing candle history within the quota (paced, with a reserve for the live bot); no orders.
pub fn lab_export() -> i32 {
    let log = common::ops_logs().join("lab_export.log");
    if !run_logged(
        Command::new(cargo()).args([
            "build",
            "--release",
            "--example",
            "lab_export",
            "--example",
            "universe_fetch",
            "--example",
            "holdings_check",
        ]),
        &log,
    ) {
        common::log(&log, "build failed");
        return 1;
    }
    // SPY's latest official holdings, checked against moomoo (delisted / unavailable removed), installed for the service
    spy_holdings_refresh();
    // fill in the research history of SPY's holdings that the candle quota did not allow yet (heaviest first; more fit as old pulls expire)
    let _ = run_logged(&mut Command::new(example("universe_fetch")), &log);
    // a new seed every day (day number), so each run adds settings the archive has not seen; the capital the bot trades: `auto` = a 5,000 USD account
    let seed = (common::now() / 86_400 % 1000) * 10;
    let _ = run_logged(
        Command::new(example("lab_export"))
            .args(["5h", "1", "120"])
            .env("LAB_CAPITALS", "auto")
            .env("LAB_SEED_BASE", seed.to_string()),
        &log,
    );
    common::log(&log, &format!("lab export done (seed base {seed})"));
    // publish the lab's research outputs (highlights, status, newest report) to research/ in this repo; never account data
    publish_research();
    0
}

/// `bot-ops spy-holdings-refresh`: refreshes the universe from SPY's official daily holdings file, keeps only what moomoo can trade and installs it for the
/// service. On any failure the current files stay untouched.
pub fn spy_holdings_refresh() -> i32 {
    let repo = common::repo();
    let log = repo.join("ops/logs/spy_holdings.log");
    let dir = repo.join("target/holdings");
    let _ = fs::create_dir_all(&dir);
    common::log(&log, "SPY holdings refresh");
    let xlsx = dir.join("spy.xlsx");
    let curl = platform::which("curl").unwrap_or_else(|| PathBuf::from("curl"));
    let mut ok = false;
    for _ in 0..3 {
        if Command::new(&curl)
            .args(["-sSL", "-m", "60", "-A", "Mozilla/5.0", "-o"])
            .arg(&xlsx)
            .arg(SPY_URL)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
    if !ok {
        common::log(&log, "download failed; keeping the current file");
        return 0;
    }
    let cand = dir.join("candidate.csv");
    if !common::tool(
        "qwen-tools",
        &[
            "spy-holdings-xlsx",
            &xlsx.display().to_string(),
            &cand.display().to_string(),
        ],
        None,
    )
    .0
    {
        common::log(&log, "bad spreadsheet; keeping the current file");
        return 0;
    }
    if !run_logged(Command::new(example("holdings_check")).arg(&cand), &log) {
        common::log(&log, "moomoo check failed; keeping the current file");
        return 0;
    }
    let rt = common::runtime();
    if rt.is_dir()
        && fs::copy(
            repo.join("data/spy_holdings.csv"),
            rt.join("spy_holdings.csv"),
        )
        .is_ok()
    {
        common::log(&log, "installed for the service");
    }
    0
}

/// `bot-ops fill-probe`: one-off calibration (daily slot a few minutes after the US open): how does the moomoo PAPER simulator fill orders of increasing size?
/// Buys and immediately sells back AAPL in growing sizes (paper account only), saving data/fill_probe.json. Disables itself once it has the full ladder.
pub fn fill_probe() -> i32 {
    let repo = common::repo();
    let log = repo.join("ops/logs/fill_probe.log");
    let data = repo.join("data/fill_probe.json");
    let n: i64 = common::tool(
        "qwen-tools",
        &["fill-probe-count", &data.display().to_string()],
        None,
    )
    .1
    .trim()
    .parse()
    .unwrap_or(0);
    if n >= 4 {
        common::log(&log, &format!("probe already done ({n} rows)"));
        let _ = crate::service::uninstall("com.claudemoomoobot.fillprobe");
        return 0;
    }
    if !crate::check::report(5).contains("market open") {
        common::log(&log, "market closed: skipped");
        return 0;
    }
    let _ = run_logged(
        Command::new(cargo()).args(["build", "--release", "--example", "fill_probe"]),
        &log,
    );
    let ok = run_logged(
        Command::new(example("fill_probe")).args(["AAPL", "300,30000,150000,500000"]),
        &log,
    );
    common::log(&log, &format!("probe finished (ok {ok})"));
    // the decision, from the real measurements only: every size filled completely both ways and the worst slippage against the quote is small
    let verdict = common::tool(
        "qwen-tools",
        &["fill-probe-verdict", &data.display().to_string()],
        None,
    )
    .1;
    common::log(&log, &format!("verdict: {}", verdict.replace('\n', " ")));
    if verdict.lines().next() == Some("OK") {
        let _ = fs::write(common::runtime().join("liquidity_model.on"), &verdict);
        common::log(&log, "liquidity model switched ON for the live bot");
    }
    0
}

/// `bot-ops sync-lab`: mirrors the lab's latest commits (~/Projects/qwen-lab, branch master) into lab/ of this repo and pushes, and the read-only MCP server
/// (~/Projects/moomoo-mcp, branch main) into mcp/. Only committed lab files are copied, so the lab's local data, memory and logs never enter this repo.
pub fn sync_lab() -> i32 {
    let repo = common::repo();
    if common::dirty(&repo) {
        println!("sync_lab: bot repo has uncommitted changes; not syncing");
        return 0;
    }
    let lab = common::lab_repo();
    let subj = common::git(&lab, &["log", "-1", "--format=%s"]).unwrap_or_default();
    let body: String = common::git(&lab, &["log", "-1", "--format=%b"])
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with("Co-Authored-By"))
        .collect::<Vec<_>>()
        .join("\n");
    let head = common::git(&lab, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let msg = format!("Sync lab/ from qwen-lab: {subj}\n\n{body}\nMirrored from qwen-lab {head} by bot-ops sync-lab (research code only; no data).");
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["subtree", "pull", "--prefix=lab"])
        .arg(&lab)
        .args(["master", "-m", &msg])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            if text.contains("up to date") {
                println!("sync_lab: lab/ already up to date");
            } else if common::git(&repo, &["push", "-q"]).is_some() {
                println!("sync_lab: lab/ synced and pushed");
            } else {
                println!("sync_lab: lab/ push failed");
            }
        }
        Ok(o) => println!(
            "sync_lab: lab/ subtree pull failed: {}",
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .last()
                .unwrap_or("")
        ),
        Err(e) => println!("sync_lab: lab/ subtree pull failed: {e}"),
    }
    // the read-only MCP server is mirrored into mcp/ the same way
    let mcp = platform::projects_dir().join("moomoo-mcp");
    if mcp.join(".git").exists() && !common::dirty(&repo) {
        let msg = format!(
            "Sync mcp/ from moomoo-mcp: {}",
            common::git(&mcp, &["log", "-1", "--format=%s"]).unwrap_or_default()
        );
        let pulled = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["subtree", "pull", "--prefix=mcp"])
            .arg(&mcp)
            .args(["main", "-m", &msg])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if pulled && common::git(&repo, &["push", "-q"]).is_some() {
            println!("sync_lab: mcp/ synced and pushed");
        }
    }
    0
}

/// Mirrors `src` into `dst` (copies every file, removes files of `dst` that are no longer in `src`), like `rsync -a --delete`.
pub fn mirror_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    let mut keep = std::collections::HashSet::new();
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        keep.insert(e.file_name());
        if e.file_type()?.is_dir() {
            mirror_dir(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), &to)?;
        }
    }
    for e in fs::read_dir(dst)? {
        let e = e?;
        if !keep.contains(&e.file_name()) {
            if e.file_type()?.is_dir() {
                fs::remove_dir_all(e.path())?;
            } else {
                fs::remove_file(e.path())?;
            }
        }
    }
    Ok(())
}

/// Claude Code's memory folder for this user: `~/.claude/projects/<home path with every non-alphanumeric character as '-'>/memory`.
pub fn claude_memory_dir() -> PathBuf {
    let enc: String = platform::home()
        .display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    platform::home()
        .join(".claude/projects")
        .join(enc)
        .join("memory")
}

fn copy_if(src: &Path, dst: &Path) {
    if src.is_file() {
        if let Some(d) = dst.parent() {
            let _ = fs::create_dir_all(d);
        }
        let _ = fs::copy(src, dst);
    }
}

fn newest_with(dir: &Path, suffix: &str) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(suffix))
        })
        .collect();
    v.sort_by_key(|p| std::cmp::Reverse(platform::mtime(p)));
    v.into_iter().next()
}

fn all_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            all_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// `bot-ops publish-state`: publishes the lab's working state into this repo's state/ folder (owner 2026-10-12: "update repo all things except api keys and
/// account deets"), so others can follow how the local model works and learns. Only CURRENT material is published (state/brain, state/lab, state/claude).
/// Never copied: databases, logs, credentials, the bot's account data. Every file is screened with the push guard's secret/account pattern first.
pub fn publish_state() -> i32 {
    let bot = common::repo();
    let lab = common::lab_root();
    let tmp = std::env::temp_dir().join(format!("bot-ops-state-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    for d in ["lab", "brain/reports", "claude"] {
        let _ = fs::create_dir_all(tmp.join(d));
    }
    copy_if(
        &common::lab_repo().join("supervisor/SUPERVISOR_STATE.md"),
        &tmp.join("lab/SUPERVISOR_STATE.md"),
    );
    let ql = lab.join("bin").join(platform::exe("qwen-lab"));
    if ql.exists() {
        let cfg = lab.join("lab.toml");
        for (sub, file) in [("report", "report.txt"), ("ml", "ml-for-trading.txt")] {
            if let Ok(o) = Command::new(&ql)
                .arg("--config")
                .arg(&cfg)
                .args(["brain", sub])
                .stdin(Stdio::null())
                .output()
            {
                let _ = fs::write(tmp.join("brain").join(file), o.stdout);
            }
        }
        let tail: Vec<String> = fs::read_to_string(lab.join("logs/brain.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let _ = fs::write(
            tmp.join("brain/recent-runs.txt"),
            tail[tail.len().saturating_sub(60)..].join("\n"),
        );
    }
    for f in ["candle-ml.txt", "candle-improve.txt"] {
        copy_if(&lab.join("logs").join(f), &tmp.join("brain").join(f));
    }
    // the newest report of each of the twelve roles
    for role in [
        "strategist",
        "tuner",
        "researcher",
        "auditor",
        "execution",
        "statistician",
        "data",
        "steward",
        "drift",
        "risk",
        "promotion",
        "experimenter",
    ] {
        if let Some(r) = newest_with(&lab.join("brain/reports"), &format!("-brain-{role}.md")) {
            copy_if(
                &r,
                &tmp.join("brain/reports")
                    .join(r.file_name().unwrap_or_default()),
            );
        }
    }
    if let Ok(rd) = fs::read_dir(claude_memory_dir()) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "md") {
                copy_if(&e.path(), &tmp.join("claude").join(e.file_name()));
            }
        }
    }
    let mut files = vec![];
    all_files(&tmp, &mut files);
    let flagged: Vec<String> = files
        .iter()
        .filter(|f| looks_secret(&fs::read_to_string(f).unwrap_or_default()))
        .map(|f| f.strip_prefix(&tmp).unwrap_or(f).display().to_string())
        .collect();
    if !flagged.is_empty() {
        println!(
            "{} publish_state: REFUSED, a file looks like a secret or account data: {}",
            common::utc_stamp(),
            flagged.join(" ")
        );
        let _ = fs::remove_dir_all(&tmp);
        return 1;
    }
    let ok = mirror_dir(&tmp, &bot.join("state")).is_ok();
    let _ = fs::remove_dir_all(&tmp);
    if !ok {
        return 1;
    }
    let _ = Command::new("git")
        .arg("-C")
        .arg(&bot)
        .args(["add", "state"])
        .status();
    if Command::new("git")
        .arg("-C")
        .arg(&bot)
        .args(["diff", "--cached", "--quiet", "--", "state"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return 0;
    }
    let msg = format!("State: the brain's reports, the ML verdicts and Claude's notes ({})\n\nPublished by bot-ops publish-state from the lab runtime and Claude Code's memory; only current material, screened for secrets and account data before commit.\nNo code change; live trading unaffected.", chrono::Utc::now().format("%Y-%m-%d %H:%M UTC"));
    if Command::new("git")
        .arg("-C")
        .arg(&bot)
        .args(["commit", "-q", "-m", &msg, "--", "state"])
        .status()
        .is_ok_and(|s| s.success())
        && common::git(&bot, &["push", "-q"]).is_some()
    {
        println!("publish_state: committed and pushed");
        common::discord(
            "State published",
            "the brain's reports, the ML verdicts and Claude's notes pushed to the repo",
        );
    }
    0
}

/// `bot-ops publish-research`: publishes the lab's research outputs to research/ in this repo for people monitoring it: HIGHLIGHTS.md, STATUS.md and the newest
/// daily lab report. Never account data: any file that mentions an account number pattern is skipped. Only research/ is staged and committed.
pub fn publish_research() -> i32 {
    let repo = common::repo();
    let src = common::lab_root();
    let _ = fs::create_dir_all(repo.join("research"));
    let re = regex::Regex::new(r"(?i)((paper|real|trading) account [0-9]{4,}|acc_id|account_id|api[_-]?key|password|secret)").expect("valid regex");
    let latest = newest_with(&src.join("reports"), ".md").filter(|p| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("day-"))
    });
    let mut list = vec![src.join("HIGHLIGHTS.md"), src.join("STATUS.md")];
    list.extend(latest.clone());
    for f in list {
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        if re.is_match(&text) {
            println!(
                "publish_research: skipped {} (looks like account or credential data)",
                f.file_name().and_then(|n| n.to_str()).unwrap_or("")
            );
            continue;
        }
        let dst = if Some(&f) == latest.as_ref() {
            repo.join("research/lab-report-latest.md")
        } else {
            repo.join("research")
                .join(f.file_name().unwrap_or_default())
        };
        let _ = fs::write(dst, text);
    }
    let _ = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["add", "research"])
        .status();
    if Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .is_ok_and(|s| s.success())
    {
        println!("publish_research: nothing new");
        return 0;
    }
    let msg = format!("Research outputs from the lab ({})\n\nresearch/HIGHLIGHTS.md: every backtest run of 15%+ over 14 sessions with its facts and the luck check;\nresearch/STATUS.md: lab and bot health, results counts and best variants so far; research/lab-report-latest.md: the lab's newest\ndaily report. Copied from the lab's local folder by bot-ops publish-research; files that mention account numbers or credentials are never copied.\nResearch only; no code or live-trading change.", chrono::Utc::now().format("%F"));
    if Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["commit", "-q", "-m", &msg, "--", "research"])
        .status()
        .is_ok_and(|s| s.success())
        && common::git(&repo, &["push", "-q"]).is_some()
    {
        println!("publish_research: published");
    }
    let _ = std::io::stdout().flush();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: mirror_dir copies new files and removes stale ones.
    #[test]
    fn mirror_dir_copies_and_deletes() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::create_dir_all(a.join("sub")).unwrap();
        fs::write(a.join("one.txt"), "1").unwrap();
        fs::write(a.join("sub/two.txt"), "2").unwrap();
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join("stale.txt"), "x").unwrap();
        mirror_dir(&a, &b).unwrap();
        assert!(
            b.join("one.txt").exists()
                && b.join("sub/two.txt").exists()
                && !b.join("stale.txt").exists()
        );
    }

    /// Test: Claude's memory folder follows the home path encoding on every OS.
    #[test]
    fn the_claude_memory_folder_follows_the_home_encoding() {
        let m = claude_memory_dir();
        assert!(m.ends_with("memory") && m.to_string_lossy().contains(".claude"));
    }
}

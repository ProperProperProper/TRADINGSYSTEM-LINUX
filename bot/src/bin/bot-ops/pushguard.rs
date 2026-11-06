//! `bot-ops push-guard [repo ...]` (was ops/push_guard.sh). Push guarantee (owner, 2026-10-11: "pushed to repo every time things fixed"): every commit in the
//! bot repo and the lab repo reaches GitHub. Runs from both repos' post-commit hooks and from both monitors every 15 minutes as a safety net, so a push that
//! failed (network, a rejected push) is retried until it lands. Also commits work left untouched for over an hour (owner: never leave anything uncommitted),
//! refusing anything that looks like a secret. Never resets or discards anything. A rejected push is retried after `git pull --rebase` only when the working
//! tree is clean (never touches someone's uncommitted work).

use crate::common::{self, Lock};
use crate::platform;
use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIG_FILE_BYTES: u64 = 5_000_000;

/// A line that looks like a credential or an account id.
pub fn looks_secret(text: &str) -> bool {
    let re = Regex::new(r#"(?i)((api[_-]?key|secret|passphrase|password|passwd|trd_?pwd)["' ]*[:=]["' ]*[A-Za-z0-9/+_-]{8,}|acc(ount)?_?id["' ]*[:=]["' ]*[1-8][0-9]{5,})"#).expect("valid regex");
    re.is_match(text)
}

/// Paths of changed and new files from `git status --porcelain --untracked-files=all` (renames: the new name).
fn changed_files(repo: &Path) -> Vec<String> {
    common::git(repo, &["status", "--porcelain", "--untracked-files=all"])
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let p = l.get(3..).unwrap_or("");
            p.rsplit(" -> ")
                .next()
                .unwrap_or(p)
                .trim_matches('"')
                .to_string()
        })
        .filter(|p| !p.is_empty())
        .collect()
}

fn gitq(repo: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn guard_one(repo: &Path, log: &Path) {
    if !repo.join(".git").exists() {
        return;
    }
    let name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string();
    // one push at a time per repo (the hook and the monitor can fire together)
    let Some(_lock) = Lock::take(&repo.join(".git/push_guard.lock"), 10) else {
        return;
    };
    let Some(branch) = common::git(repo, &["symbolic-ref", "--short", "HEAD"]) else {
        return;
    };
    // Owner (2026-10-11): "commit them always. there should never be uncommitted left for ages". Work nobody has touched for IDLE_MIN minutes is committed
    // (all changed and new files that .gitignore allows), unless it looks like a secret or account data (then it is left and logged as a REFUSED warning for
    // the log doctor) or a file is over 5 MB (left out and logged).
    let idle_min: i64 = std::env::var("IDLE_MIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let busy = repo.join(".git/rebase-merge").is_dir()
        || repo.join(".git/rebase-apply").is_dir()
        || repo.join(".git/MERGE_HEAD").exists();
    let files = changed_files(repo);
    if !files.is_empty() && !busy {
        let paths: Vec<PathBuf> = files.iter().map(|f| repo.join(f)).collect();
        let newest = common::newest_mtime(&paths);
        if common::now() - newest >= idle_min * 60 {
            let big: Vec<&String> = files
                .iter()
                .filter(|f| {
                    fs::metadata(repo.join(f))
                        .is_ok_and(|m| m.is_file() && m.len() > BIG_FILE_BYTES)
                })
                .collect();
            let mut blob = common::git(repo, &["diff", "HEAD", "--no-color"]).unwrap_or_default();
            for f in common::git(repo, &["ls-files", "--others", "--exclude-standard"])
                .unwrap_or_default()
                .lines()
            {
                blob.push_str(&fs::read_to_string(repo.join(f)).unwrap_or_default());
            }
            if looks_secret(&blob) {
                common::log(log, &format!("{name}: auto-commit REFUSED: the uncommitted changes look like a secret or account data; left for a human"));
            } else {
                gitq(repo, &["add", "-A"]);
                for f in &big {
                    gitq(repo, &["reset", "-q", "--", f]);
                    common::log(
                        log,
                        &format!("{name}: left out of auto-commit (over 5 MB): {f}"),
                    );
                }
                if !gitq(repo, &["diff", "--cached", "--quiet"]) {
                    let list = common::git(repo, &["diff", "--cached", "--name-status"])
                        .unwrap_or_default()
                        .lines()
                        .take(40)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let stat =
                        common::git(repo, &["diff", "--cached", "--shortstat"]).unwrap_or_default();
                    let msg = format!("Auto-commit: work left uncommitted for over {idle_min} min\n\nOwner rule (2026-10-11): nothing stays uncommitted. bot-ops push-guard committed these changes because nobody had touched\nthem for {idle_min}+ minutes; they were made by a session that did not commit them. Review them; the next commit should\nsay what they were for.\n{stat}\n{list}\n\nCo-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>");
                    if gitq(repo, &["commit", "-q", "-m", &msg]) {
                        common::log(log, &format!("{name}: auto-committed idle work ({stat})"));
                    }
                }
            }
        }
    }
    let _ = gitq(repo, &["fetch", "-q", "origin", &branch]);
    let ahead: usize = common::git(
        repo,
        &["rev-list", "--count", &format!("origin/{branch}..HEAD")],
    )
    .and_then(|s| s.parse().ok())
    .unwrap_or(0);
    if ahead == 0 {
        return;
    }
    let subj = common::git(
        repo,
        &["log", "--format=%h %s", &format!("origin/{branch}..HEAD")],
    )
    .unwrap_or_default()
    .lines()
    .take(5)
    .map(|l| l.chars().take(140).collect::<String>())
    .collect::<Vec<_>>()
    .join("\n");
    for i in 1..=3u64 {
        if gitq(repo, &["push", "-q", "origin", &branch]) {
            common::log(
                log,
                &format!("{name}: pushed {ahead} commit(s) to origin/{branch}"),
            );
            common::discord(&format!("{name}: pushed {ahead} commit(s)"), &subj);
            return;
        }
        if common::git(repo, &["status", "--porcelain", "--untracked-files=no"])
            .is_some_and(|s| s.is_empty())
        {
            let _ = gitq(repo, &["pull", "-q", "--rebase", "origin", &branch]);
        }
        std::thread::sleep(std::time::Duration::from_secs(i * 5));
    }
    common::log(log, &format!("{name}: PUSH FAILED ({ahead} commit(s) not on GitHub); retried by the next monitor run"));
    common::discord(
        &format!("{name}: PUSH FAILED"),
        &format!("{ahead} commit(s) are not on GitHub yet; retried by the monitor"),
    );
}

pub fn run(repos: &[&str]) -> i32 {
    let log = common::ops_logs().join("push_guard.log");
    let list: Vec<PathBuf> = if repos.is_empty() {
        vec![common::repo(), common::lab_repo()]
    } else {
        repos.iter().map(PathBuf::from).collect()
    };
    for r in list {
        guard_one(&r, &log);
    }
    let _ = platform::os();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: credentials and account ids are recognised, ordinary text is not.
    #[test]
    fn secrets_are_recognised() {
        assert!(looks_secret("api_key = \"abcd1234efgh\""));
        assert!(looks_secret("{\"acc_id\": 281756455998}"));
        assert!(looks_secret("TRD_PWD: abcdefghij"));
        assert!(!looks_secret(
            "the account was opened a few days ago; password handling is documented"
        ));
        assert!(!looks_secret("acc_id is read from the API at run time"));
    }

    /// Test: porcelain paths are extracted, renames use the new name.
    #[test]
    fn changed_files_come_from_porcelain() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        assert!(gitq(r, &["init", "-q"]));
        fs::write(r.join("a.txt"), "x").unwrap();
        fs::create_dir_all(r.join("sub")).unwrap();
        fs::write(r.join("sub/b.txt"), "y").unwrap();
        let mut f = changed_files(r);
        f.sort();
        assert_eq!(f, vec!["a.txt", "sub/b.txt"]);
    }
}

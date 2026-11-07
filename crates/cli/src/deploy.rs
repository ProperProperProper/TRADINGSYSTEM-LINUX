//! "Is everything running on the latest local code, and is nothing left uncommitted?" answered exactly, for `thomas doctor`.
//!
//! * This machine is intentionally local-only: commits ahead of the remote are expected and are never a deployment problem.
//! * Both repositories (this one and papercompmoomoo): no uncommitted files.
//! * The `thomas` binary: the fingerprint of the sources it was built from (`SOURCE_HASH`, see `thomas_core::source_hash`) equals the fingerprint of the sources
//!   on disk now. A binary older than the code is reported, whatever the file times say.
//! * The competition bot: the `code_hash` it wrote to its status file when it started equals the fingerprint of its `src/` now.
//! * Long-running processes (the portal) started after the binary they run was last replaced.
use std::path::Path;
use std::process::Command;
use thomas_core::source_hash::{BUILD_DIRS, BUILD_EXTENSIONS, source_hash};

use crate::ops::Problem;

pub const BUILT_FROM: &str = env!("SOURCE_HASH");

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Uncommitted files and commits ahead of the remote at `dir` (None when `dir` is not a git repository).
/// The ahead count is diagnostic only; local-only operation must never push or warn about it.
pub fn repo_state(dir: &Path) -> Option<(usize, usize)> {
    let dirty = git(dir, &["status", "--porcelain"])?.lines().count();
    let unpushed = git(dir, &["rev-list", "--count", "@{u}..HEAD"])
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    Some((dirty, unpushed))
}

/// Problems with the sibling bot's code: running code differs from the files, or the bot never reported its code.
pub fn sibling_code_problem(status_hash: Option<&str>, disk_hash: &str) -> Option<String> {
    match status_hash {
        None => Some(
            "the competition bot has not reported which code it runs (restart it so it does)"
                .into(),
        ),
        Some(h) if h != disk_hash => Some(
            "the competition bot is running older code than the files on disk: restart it".into(),
        ),
        _ => None,
    }
}

/// The sibling's fingerprint, the algorithm of `thomas_core::source_hash` over `src/papercompmoomoo/*.py`.
pub fn sibling_disk_hash(root: &Path) -> String {
    source_hash(
        &root.join("papercompmoomoo"),
        &["src/papercompmoomoo"],
        &["py"],
    )
}

fn process_older_than_file(pattern: &str, file: &Path) -> Option<bool> {
    let pid = String::from_utf8_lossy(
        &Command::new("pgrep")
            .args(["-f", pattern])
            .output()
            .ok()?
            .stdout,
    )
    .lines()
    .next()?
    .trim()
    .to_string();
    let secs: u64 = String::from_utf8_lossy(
        &Command::new("ps")
            .args(["-o", "etimes=", "-p", &pid])
            .output()
            .ok()?
            .stdout,
    )
    .trim()
    .parse()
    .ok()?;
    let age = std::fs::metadata(file)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?
        .as_secs();
    Some(secs > age)
}

/// All deployment problems for the project at `root`.
pub fn problems(root: &Path) -> Vec<Problem> {
    let mut p = Vec::new();
    let mut add = |error: bool, text: String| p.push(Problem { error, text });
    for (name, dir) in [
        ("TradingSystem", root.to_path_buf()),
        ("papercompmoomoo", root.join("papercompmoomoo")),
    ] {
        match repo_state(&dir) {
            None => add(false, format!("{name} is not a git repository here")),
            Some((dirty, _ahead)) => {
                if dirty > 0 {
                    add(
                        false,
                        format!("{name}: {dirty} uncommitted file(s): commit them locally"),
                    );
                }
            }
        }
    }
    let now_hash = source_hash(root, &BUILD_DIRS, &BUILD_EXTENSIONS);
    if now_hash != BUILT_FROM {
        add(true, "the thomas binary was built from older sources than the code on disk: run cargo build --release and re-sign it".into());
    }
    let status: Option<serde_json::Value> =
        std::fs::read_to_string(root.join("papercompmoomoo/data/status.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok());
    if let Some(msg) = sibling_code_problem(
        status.as_ref().and_then(|s| s["code_hash"].as_str()),
        &sibling_disk_hash(root),
    ) {
        add(true, msg);
    }
    if process_older_than_file("release/thomas portal", &root.join("target/release/thomas"))
        == Some(true)
    {
        let hint = if cfg!(target_os = "linux") {
            "systemctl --user restart com.thomasai.portal.service"
        } else {
            "launchctl kickstart -k gui/$(id -u)/com.thomasai.portal"
        };
        add(
            true,
            format!(
                "the portal started before the thomas binary was last replaced: restart it ({hint})"
            ),
        );
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sibling_code_check_names_old_code_and_missing_reports() {
        assert!(sibling_code_problem(Some("abc"), "abc").is_none());
        assert!(
            sibling_code_problem(Some("abc"), "def")
                .unwrap()
                .contains("older code")
        );
        assert!(
            sibling_code_problem(None, "def")
                .unwrap()
                .contains("not reported")
        );
    }

    #[test]
    fn repo_state_still_counts_ahead_commits_for_diagnostics() {
        let d = std::env::temp_dir().join(format!("thomas-deploy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let remote = d.join("remote.git");
        let work = d.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(dir)
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "{args:?}"
            )
        };
        std::fs::create_dir_all(&remote).unwrap();
        run(&remote, &["init", "-q", "--bare", "-b", "main"]);
        run(&work, &["init", "-q", "-b", "main"]);
        run(&work, &["config", "user.email", "t@t"]);
        run(&work, &["config", "user.name", "t"]);
        std::fs::write(work.join("a.txt"), "1").unwrap();
        run(&work, &["add", "."]);
        run(&work, &["commit", "-q", "-m", "one"]);
        run(
            &work,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        run(&work, &["push", "-q", "-u", "origin", "main"]);
        assert_eq!(repo_state(&work), Some((0, 0)));
        std::fs::write(work.join("b.txt"), "2").unwrap();
        assert_eq!(repo_state(&work), Some((1, 0)), "an uncommitted file");
        run(&work, &["add", "."]);
        run(&work, &["commit", "-q", "-m", "two"]);
        assert_eq!(repo_state(&work), Some((0, 1)), "a commit not pushed");
        assert_eq!(repo_state(&d.join("nowhere")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_source_fingerprint_changes_with_any_byte_of_any_listed_file_and_ignores_the_rest() {
        let d = std::env::temp_dir().join(format!("thomas-hash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/sub")).unwrap();
        std::fs::create_dir_all(d.join("src/__pycache__")).unwrap();
        std::fs::write(d.join("src/a.py"), "print(1)").unwrap();
        std::fs::write(d.join("src/sub/b.py"), "print(2)").unwrap();
        let h = || source_hash(&d, &["src"], &["py"]);
        let base = h();
        std::fs::write(d.join("src/__pycache__/c.py"), "ignored").unwrap();
        std::fs::write(d.join("src/notes.txt"), "ignored").unwrap();
        assert_eq!(h(), base, "caches and other file types do not count");
        std::fs::write(d.join("src/sub/b.py"), "print(3)").unwrap();
        assert_ne!(h(), base);
        let _ = std::fs::remove_dir_all(&d);
    }
}

//! How the lab talks to the bot's operations: the bot repo ships `bot-ops` (Rust, every OS: deploy, check, push-guard, ...). On a machine where it has
//! not been built yet, the old shell scripts are used on macOS and Linux (they are one-line stubs for `bot-ops` once it exists).

use crate::platform::{self, Os};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The bot-ops binary: built in the bot repo, or installed beside the other tools.
pub fn bot_ops() -> Option<PathBuf> {
    [platform::bot_repo().join("target/release").join(platform::exe("bot-ops")), platform::tool("bot-ops")].into_iter().find(|p| p.is_file())
}

/// Runs a bot-ops subcommand (or its shell equivalent) and returns (success, combined output).
pub fn run(sub: &str, args: &[&str], script: &str) -> (bool, String) {
    let repo = platform::bot_repo();
    let mut cmd = match bot_ops() {
        Some(b) => {
            let mut c = Command::new(b);
            c.arg(sub).args(args);
            c
        }
        None if platform::os() != Os::Windows && repo.join(script).exists() => {
            let mut c = Command::new("/bin/bash");
            c.arg(repo.join(script)).args(args);
            c
        }
        None => return (false, format!("bot-ops is not built ({}): run `cargo build --release --bin bot-ops` in the bot repo", repo.display())),
    };
    match cmd.current_dir(&repo).stdin(Stdio::null()).output() {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (false, format!("{sub}: {e}")),
    }
}

/// The bot's health report for the last `minutes` of logs (`check.sh` / `bot-ops check`).
pub fn check(minutes: u32) -> String {
    run("check", &[&minutes.to_string()], "check.sh").1
}

/// Builds, tests and redeploys the bot (`deploy.sh` / `bot-ops deploy`); true when it succeeded.
pub fn deploy() -> (bool, String) {
    run("deploy", &[], "deploy.sh")
}

/// Commits and pushes anything committed-but-not-pushed or left idle in the lab and bot repos.
pub fn push_guard() {
    let _ = run("push-guard", &[], "ops/push_guard.sh");
}

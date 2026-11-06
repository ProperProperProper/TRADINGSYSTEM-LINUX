//! `qwen-guard staged | push | install-hooks REPO...` -- the Discord webhook can never reach git (owner rule 2026-10-13).
//!   staged         pre-commit hook: blocks a commit whose staged changes add a webhook URL or the webhook file
//!   push           pre-push hook (reads git's stdin): blocks a push of any commit that adds one
//!   install-hooks  writes the two hooks (two-line stubs that exec this binary) into each repo's .git/hooks
//! Fails closed: if this binary is missing the hooks error out and the commit/push does not happen.
use qwen_lab::webhook_guard::{diff_leaks, is_webhook_file};
use std::io::Read;
use std::process::{exit, Command};

fn git(args: &[&str]) -> String {
    Command::new("git").args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn block(why: &str) -> ! {
    eprintln!("BLOCKED by qwen-guard: {why} (Discord webhook URLs must never be committed or pushed)");
    exit(1)
}

fn staged() {
    if git(&["diff", "--cached", "--name-only"]).lines().any(is_webhook_file) { block("the webhook file is staged"); }
    if diff_leaks(&git(&["diff", "--cached", "-U0"])) { block("a webhook URL is in the staged changes"); }
}

fn push() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let zero = "0".repeat(40);
    for line in input.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 || f[1] == zero { continue; }
        // an existing remote branch: only the new commits; a new branch: everything not already on a remote
        let range: Vec<String> = if f[3] == zero { vec![f[1].to_string(), "--not".into(), "--remotes".into()] } else { vec![format!("{}..{}", f[3], f[1])] };
        let mut p = vec!["log", "-p", "--format="]; p.extend(range.iter().map(String::as_str));
        if diff_leaks(&git(&p)) { block("a commit being pushed contains a webhook URL"); }
        let mut n = vec!["log", "--name-only", "--format="]; n.extend(range.iter().map(String::as_str));
        if git(&n).lines().any(is_webhook_file) { block("a commit being pushed contains the webhook file"); }
    }
}

fn install(repos: &[String]) {
    let bin = qwen_lab::platform::tool("qwen-guard").to_string_lossy().replace('\\', "/");
    for repo in repos {
        let dir = std::path::Path::new(repo).join(".git/hooks");
        if !dir.is_dir() { eprintln!("skip {repo}: no .git/hooks"); continue; }
        for (hook, arg) in [("pre-commit", "staged"), ("pre-push", "push")] {
            let p = dir.join(hook);
            let ours = std::fs::read_to_string(&p).map(|s| s.contains("qwen-guard")).unwrap_or(true);
            if !ours { eprintln!("kept the existing {hook} in {repo} (not ours): add `{bin} {arg}` to it by hand"); continue; }
            std::fs::write(&p, format!("#!/bin/sh\n# installed by qwen-guard: Discord webhook URLs never reach git\nexec \"{bin}\" {arg}\n")).expect("write hook");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).expect("chmod hook");
            }
            println!("installed {hook} in {repo}");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("staged") => staged(),
        Some("push") => push(),
        Some("install-hooks") => install(&args[1..]),
        _ => { eprintln!("usage: qwen-guard staged | push | install-hooks REPO..."); exit(2) }
    }
}

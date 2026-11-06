//! Code checks for the daily fix report (Rust port of the bot's ops/fix_report_check.py; used by ops/fix_report.sh).
//!
//!   check <commits.md> <report.md>     exit 0 when every number and every commit id in the report appears in the commits (hard rule: never invent
//!                                      data or results); otherwise prints what is unverified, exit 1
//!   fallback <commits.md> <report.md>  writes the report from the commit messages as they are (used when the check fails)
//!   secrets <report.md>                exit 1 when the report looks like it contains an account number, key or password
//!   index <docs/fixes dir>             rewrites docs/fixes/README.md, the list of all reports, newest first

use anyhow::{anyhow, Result};
use fancy_regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;

struct Res { num: Regex, hash: Regex, any_num: Regex }
fn big(p: &str) -> Regex { fancy_regex::RegexBuilder::new(p).backtrack_limit(500_000_000).build().expect("valid regex") }
fn res() -> Res {
    Res {
        // a number not glued to a word (commit ids, identifiers like 5h|obv, version strings are checked separately or skipped)
        num: big(r"(?<![\w.:/|=-])[+-]?\d+(?:[.,]\d+)*%?(?![\w])"),
        hash: Regex::new(r"`([0-9a-f]{7,12})`").unwrap(),
        any_num: Regex::new(r"\d+(?:\.\d+)?").unwrap(),
    }
}

fn norm(n: &str) -> String { n.trim_matches(|c| c == '+' || c == '-' || c == '%').replace(',', "") }
fn all(re: &Regex, text: &str) -> Vec<String> { re.find_iter(text).filter_map(|m| m.ok()).map(|m| m.as_str().to_string()).collect() }
fn numbers(r: &Res, text: &str) -> BTreeSet<String> { all(&r.num, text).iter().map(|m| norm(m)).collect() }
fn py_list(v: &[String]) -> String { format!("[{}]", v.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", ")) }

pub fn check(commits: &str, report: &str) -> Result<i32> {
    let r = res();
    let (src, rep) = (std::fs::read_to_string(commits)?, std::fs::read_to_string(report)?);
    let mut known = numbers(&r, &src);
    known.extend(all(&r.any_num, &src).iter().map(|x| norm(x)));
    let bad: Vec<String> = numbers(&r, &rep).into_iter().filter(|n| !known.contains(n)).collect();
    let hashes: BTreeSet<String> = Regex::new(r"(?m)^### \w+ ([0-9a-f]{7,12}) ").unwrap().captures_iter(&src).filter_map(|c| c.ok()).map(|c| c[1].to_string()).collect();
    let mut bad_ids: Vec<String> = r.hash.captures_iter(&rep).filter_map(|c| c.ok()).map(|c| c[1].to_string()).filter(|h| !hashes.iter().any(|x| h.starts_with(x.as_str()) || x.starts_with(h.as_str()))).collect();
    bad_ids.sort();
    if !rep.contains("**Problem:**") || !rep.contains("**Action taken:**") { println!("missing the Problem / Action taken structure"); return Ok(1); }
    if !bad.is_empty() || !bad_ids.is_empty() {
        println!("unverified numbers {}; unknown commit ids {}", py_list(&bad.iter().take(12).cloned().collect::<Vec<_>>()), py_list(&bad_ids.iter().take(6).cloned().collect::<Vec<_>>()));
        return Ok(1);
    }
    Ok(0)
}

/// Splits a text into the blocks that start at a line beginning with `### ` (text before the first one is dropped, as the original did).
fn blocks(src: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = vec![];
    let mut pos = 0;
    for line in src.split_inclusive('\n') { if line.starts_with("### ") { starts.push(pos); } pos += line.len(); }
    starts.iter().enumerate().map(|(i, &s)| &src[s..starts.get(i + 1).copied().unwrap_or(src.len())]).collect()
}

pub fn fallback(commits: &str, report: &str) -> Result<i32> {
    let src = std::fs::read_to_string(commits)?;
    let head_re = Regex::new(r"^### (\w+) (\S+) \((.*)\)").unwrap();
    let mut out: Vec<String> = vec!["_The commits as written (each commit message states what changed and why, the live-trading impact and how it was verified)._".into(), String::new()];
    for block in blocks(&src) {
        let (head, body) = block.split_once('\n').unwrap_or((block, ""));
        let (title, rest) = body.trim().split_once('\n').unwrap_or((body.trim(), ""));
        let caps = head_re.captures(head)?.ok_or_else(|| anyhow!("commit heading not understood: {head}"))?;
        let rest = rest.trim().lines().filter(|l| !l.starts_with("Co-Authored-By")).collect::<Vec<_>>().join("\n");
        out.extend([format!("## {title}"), String::new(), format!("**Action taken:** `{}` in the {} repo ({}).", &caps[2], &caps[1], &caps[3]), String::new(),
            "**Problem and solution (commit message):**".into(), String::new(), if rest.is_empty() { "not stated in the commit".into() } else { rest }, String::new()]);
    }
    std::fs::write(report, out.join("\n"))?;
    Ok(0)
}

/// The leftmost "possible secret" in `text`, as the original regex found it: a key word followed by a value, an account id, or a bare 12+ digit
/// number that is not glued to a word/hyphen, not a YYYYMMDDHHMM stamp (backup file names) and not followed by a word character. Written without
/// lookarounds (linear time): the lookaround version hit the backtracking limit on large reports.
pub fn find_secret(text: &str) -> Option<String> {
    static A: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static STAMP: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let a = A.get_or_init(|| regex::Regex::new(r"(?i)(?:api[_-]?key|secret|passphrase|password|passwd|trd_?pwd|token)\s*[:=]\s*\S{6,}|acc(?:ount)?[_ ]?id\s*[:=]?\s*[1-8]\d{5,}").unwrap());
    let stamp = STAMP.get_or_init(|| regex::Regex::new(r"^20\d{2}(?:0[1-9]|1[0-2])[0-3]\d[0-2]\d[0-5]\d").unwrap());
    let first_a = a.find(text).map(|m| (m.start(), m.as_str().to_string()));
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut digits: Option<(usize, String)> = None;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
            let run = &text[start..i];
            let before_ok = text[..start].chars().next_back().is_none_or(|c| !(is_word(c) || c == '-'));
            let after_ok = text[i..].chars().next().is_none_or(|c| !is_word(c));
            if run.len() >= 12 && before_ok && after_ok && !stamp.is_match(run) { digits = Some((start, run.to_string())); break; }
        } else { i += 1; }
    }
    match (first_a, digits) { (Some(a), Some(d)) => Some(if d.0 < a.0 { d.1 } else { a.1 }), (Some(a), None) => Some(a.1), (None, Some(d)) => Some(d.1), (None, None) => None }
}

pub fn secrets(report: &str) -> Result<i32> {
    let text = std::fs::read_to_string(report)?;
    if let Some(m) = find_secret(&text) {
        println!("possible secret: {}...", m.chars().take(12).collect::<String>());
        return Ok(1);
    }
    Ok(0)
}

pub fn index(dir: &str) -> Result<i32> {
    let name_re = Regex::new(r"^\d{4}-\d{2}-\d{2}.*\.md$").unwrap();
    let mut files: Vec<String> = std::fs::read_dir(dir)?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|f| name_re.is_match(f).unwrap_or(false)).collect();
    files.sort(); files.reverse();
    let mut lines: Vec<String> = vec!["# Fix reports".into(), String::new(),
        "Every 24 hours `ops/fix_report.sh` publishes what was fixed or changed in the bot repo and the lab repo since the previous report: the problem, the solution and the action taken (commit ids, verification, live-trading impact), from the commit messages only; code checks every number and commit id before it is published.".into(), String::new()];
    for f in &files {
        let first = std::fs::read_to_string(Path::new(dir).join(f))?.lines().find(|l| l.starts_with("Window:")).unwrap_or("").trim().to_string();
        lines.push(format!("- [{}]({f}): {first}", &f[..f.len() - 3]));
    }
    std::fs::write(Path::new(dir).join("README.md"), lines.join("\n") + "\n")?;
    Ok(0)
}

/// Drops the bot's own "Sync lab/ from qwen-lab" and "Daily fix report" commits from the collected commit list (formerly inline Python in
/// ops/fix_report.sh). A commit block is `### repo sha (date)` + its message; the title is the message's first line.
pub fn filter_commits(text: &str) -> String {
    let mut starts: Vec<usize> = vec![0];
    let mut pos = 0;
    for line in text.split_inclusive('\n') { if line.starts_with("### ") && pos != 0 { starts.push(pos); } pos += line.len(); }
    let mut out = String::new();
    for (i, &s) in starts.iter().enumerate() {
        let block = &text[s..starts.get(i + 1).copied().unwrap_or(text.len())];
        let title = block.split('\n').nth(1).unwrap_or("");
        if title.starts_with("Sync lab/ from qwen-lab") || title.starts_with("Daily fix report") { continue; }
        out.push_str(block);
    }
    out
}

pub fn main(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("filter") {
        let mut input = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
        print!("{}", filter_commits(&input));
        return 0;
    }
    let r = match (args.first().map(String::as_str), args.len()) {
        (Some("check"), 3) => check(&args[1], &args[2]), (Some("fallback"), 3) => fallback(&args[1], &args[2]),
        (Some("secrets"), 2) => secrets(&args[1]), (Some("index"), 2) => index(&args[1]),
        _ => { eprintln!("usage: fix-report check|fallback <commits.md> <report.md> | secrets <report.md> | index <dir>"); return 2 }
    };
    r.unwrap_or_else(|e| { eprintln!("fix-report: {e:#}"); 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMITS: &str = "### bot abc1234 (2026-10-12)\nFix the thing\n\nReturn went from 1.5% to 2.25% over 14 sessions.\nCo-Authored-By: x\n### lab 9f8e7d6c5b (2026-10-13)\nSecond\nbody 3 files\n";

    #[test]
    fn numbers_not_glued_to_words_are_found() {
        let r = res();
        assert_eq!(numbers(&r, "got 1,234.5% and +7 but not v2.5x or 5h|obv or abc1.5 or a=3"), ["1234.5", "7"].iter().map(|s| s.to_string()).collect());
    }

    #[test]
    fn check_accepts_known_numbers_and_ids_and_rejects_invented_ones() {
        let d = tempfile::tempdir().unwrap(); let (c, rp) = (d.path().join("c.md"), d.path().join("r.md"));
        std::fs::write(&c, COMMITS).unwrap();
        std::fs::write(&rp, "**Problem:** return 1.5%\n**Action taken:** `abc1234` and `9f8e7d6c` (3 files)").unwrap();
        assert_eq!(check(c.to_str().unwrap(), rp.to_str().unwrap()).unwrap(), 0);
        std::fs::write(&rp, "**Problem:** return 99.9%\n**Action taken:** `deadbee`").unwrap();
        assert_eq!(check(c.to_str().unwrap(), rp.to_str().unwrap()).unwrap(), 1);
        std::fs::write(&rp, "no structure 1.5%").unwrap();
        assert_eq!(check(c.to_str().unwrap(), rp.to_str().unwrap()).unwrap(), 1);
    }

    #[test]
    fn mirror_and_report_commits_are_filtered_out() {
        let t = "### bot a1 (d)\nSync lab/ from qwen-lab: x\nbody\n### bot b2 (d)\nReal fix\nbody\n\n### lab c3 (d)\nDaily fix report 2026-10-12\n### lab d4 (d)\nOther\n";
        assert_eq!(filter_commits(t), "### bot b2 (d)\nReal fix\nbody\n\n### lab d4 (d)\nOther\n");
        assert_eq!(filter_commits(""), "");
        assert_eq!(filter_commits("preamble\n### bot b (d)\nKeep\n"), "preamble\n### bot b (d)\nKeep\n");
    }

    #[test]
    fn secret_search_matches_the_original_rules_and_survives_big_input() {
        assert_eq!(find_secret("x api_key = abcdef123456 y").as_deref(), Some("api_key = abcdef123456"));
        assert_eq!(find_secret("number 123456789012 here").as_deref(), Some("123456789012"));
        assert_eq!(find_secret("a 123456789012abc b"), None, "followed by a word character");
        assert_eq!(find_secret("id-123456789012 and x123456789012"), None, "glued to a hyphen or a word");
        assert_eq!(find_secret("stamp 202610130945 ok"), None, "YYYYMMDDHHMM backup names are not accounts");
        assert_eq!(find_secret("7 digits 1234567 only"), None);
        assert_eq!(find_secret("key: 12345 then account id 281756455998").as_deref(), Some("account id 281756455998"), "leftmost wins");
        assert!(find_secret(&"lorem ipsum dolor 123 sit amet\n".repeat(50_000)).is_none());
    }

    #[test]
    fn secrets_but_not_timestamps_are_flagged() {
        let d = tempfile::tempdir().unwrap(); let p = d.path().join("r.md");
        for (text, flagged) in [("api_key = abcdef123456", true), ("account id 281756455998", true), ("number 123456789012 here", true), ("backup 202610130945 ok", false), ("short 12345", false)] {
            std::fs::write(&p, text).unwrap();
            assert_eq!(secrets(p.to_str().unwrap()).unwrap() == 1, flagged, "{text}");
        }
    }

    #[test]
    fn fallback_and_index_write_the_original_layout() {
        let d = tempfile::tempdir().unwrap(); let (c, rp) = (d.path().join("c.md"), d.path().join("r.md"));
        std::fs::write(&c, COMMITS).unwrap();
        fallback(c.to_str().unwrap(), rp.to_str().unwrap()).unwrap();
        let out = std::fs::read_to_string(&rp).unwrap();
        assert!(out.contains("## Fix the thing\n\n**Action taken:** `abc1234` in the bot repo (2026-10-12).") && !out.contains("Co-Authored-By"), "{out}");
        assert!(out.ends_with("body 3 files\n") || out.ends_with("body 3 files"), "{out}");
        std::fs::write(d.path().join("2026-10-12.md"), "Window: 2026-10-11..12\nx").unwrap(); std::fs::write(d.path().join("2026-10-13.md"), "x").unwrap();
        index(d.path().to_str().unwrap()).unwrap();
        let idx = std::fs::read_to_string(d.path().join("README.md")).unwrap();
        assert!(idx.find("2026-10-13").unwrap() < idx.find("2026-10-12").unwrap() && idx.contains("- [2026-10-12](2026-10-12.md): Window: 2026-10-11..12"), "{idx}");
    }
}

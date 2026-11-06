//! Generates docs/functions/*.md: one page per Rust source file, one entry per function (tests excluded), from the code itself (Rust port of
//! tools/gen_function_docs.py). Each entry: name, signature, location and the function's doc comment (/// lines) or, without one, the NOTE/plain
//! comments directly above it. Nothing is invented: a function with no comment is listed with its signature only and flagged in the index.
//! Secrets guard: any line matching account/credential patterns aborts generation.
//!
//! `qwen-tools gen-function-docs <repo root>`

use anyhow::{bail, Result};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

struct Res { fnre: Regex, secret: Regex, sig_end: Regex, brace_tail: Regex, ws: Regex, line_comment: Regex }

fn res() -> Res {
    Res {
        fnre: Regex::new(r"^(\s*)((?:pub(?:\([a-z]+\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*))").unwrap(),
        secret: Regex::new(r#"(?i)(api[_-]?key|secret|passphrase|password|token)\s*[:=]\s*["'][^"']{6,}|\b\d{6,}\b.*acc_id|acc_id\s*[:=]\s*[1-8]\d{5,}"#).unwrap(),
        sig_end: Regex::new(r"\{\s*$|;\s*$|\{\s*\}\s*$").unwrap(),
        brace_tail: Regex::new(r"\s*\{.*$").unwrap(),
        ws: Regex::new(r"\s+").unwrap(),
        line_comment: Regex::new(r"^//\s?").unwrap(),
    }
}

/// Tracked Rust source files of the repo (build output and generated protobuf code excluded), sorted.
fn files(root: &Path) -> Result<Vec<String>> {
    let out = Command::new("git").arg("-C").arg(root).args(["ls-files", "*.rs"]).output()?;
    if !out.status.success() { bail!("git ls-files failed"); }
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout).split_whitespace().map(String::from).filter(|f| !f.starts_with("target/") && !f.starts_with("proto/")).collect();
    v.sort();
    Ok(v)
}

/// A function's signature from its first line to the opening brace or semicolon, on one line.
fn signature(r: &Res, lines: &[&str], i: usize) -> String {
    let mut sig: Vec<&str> = vec![];
    for line in lines.iter().take((i + 30).min(lines.len())).skip(i) {
        let t = line.trim();
        sig.push(t);
        if r.sig_end.is_match(t) || t.ends_with('{') { break; }
    }
    let s = sig.join(" ");
    let s = r.brace_tail.replace(&s, "").to_string();
    let s = s.trim_end_matches(';').trim().to_string();
    r.ws.replace_all(&s, " ").to_string()
}

/// The doc comment (///) and other comment lines directly above line i, skipping attributes.
fn comments_above(r: &Res, lines: &[&str], i: usize) -> (Vec<String>, Vec<String>) {
    let (mut doc, mut notes) = (vec![], vec![]);
    let mut j = i as isize - 1;
    while j >= 0 {
        let t = lines[j as usize].trim();
        if t.starts_with("#[") || t.starts_with("#![") { j -= 1; continue; }
        if let Some(d) = t.strip_prefix("///") { doc.insert(0, d.trim().to_string()); }
        else if t.starts_with("//") { notes.insert(0, r.line_comment.replace(t, "").replace("NOTE(agents):", "Note:").trim().to_string()); }
        else { break; }
        j -= 1;
    }
    (doc, notes)
}

/// The module's //! header comment lines.
fn module_doc(lines: &[&str]) -> Vec<String> {
    let mut d = vec![];
    for t in lines {
        let s = t.trim();
        if let Some(x) = s.strip_prefix("//!") { d.push(x.trim().to_string()); } else if !s.is_empty() && !s.starts_with("//") { break; }
    }
    d
}

/// Line indexes of #[cfg(test)] modules (excluded).
fn test_ranges(lines: &[&str]) -> BTreeSet<usize> {
    let mut skip = BTreeSet::new();
    for (i, l) in lines.iter().enumerate() {
        if l.trim().starts_with("#[cfg(test)]") {
            let (mut depth, mut started) = (0i32, false);
            for (j, line) in lines.iter().enumerate().skip(i) {
                depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
                started = started || line.contains('{');
                skip.insert(j);
                if started && depth <= 0 { break; }
            }
        }
    }
    skip
}

pub fn generate(root: &Path) -> Result<String> {
    let r = res();
    let outdir = root.join("docs/functions");
    std::fs::create_dir_all(&outdir)?;
    // regenerate from a clean folder: a page for a source file that no longer exists must not survive (found 2026-10-13 after removing modules)
    for e in std::fs::read_dir(&outdir)?.flatten() { if e.path().extension().is_some_and(|x| x == "md") { std::fs::remove_file(e.path())?; } }
    let (mut index, mut total, mut undocumented): (Vec<(String, String, usize, usize)>, usize, usize) = (vec![], 0, 0);
    for f in files(root)? {
        let text = std::fs::read_to_string(root.join(&f))?;
        let lines: Vec<&str> = text.split('\n').collect();
        for (n, l) in lines.iter().enumerate() { if r.secret.is_match(l) { bail!("refusing: {f}:{} looks like an account or credential detail", n + 1); } }
        let skip = test_ranges(&lines);
        let mut entries: Vec<(String, String, usize, Vec<String>, Vec<String>, usize)> = vec![];
        for (i, l) in lines.iter().enumerate() {
            let Some(m) = r.fnre.captures(l) else { continue };
            if skip.contains(&i) || l.trim().starts_with("//") { continue; }
            let prev = if i > 0 { lines[i - 1].trim() } else { "" };
            if prev.starts_with("#[test]") || prev.starts_with("#[tokio::test") { continue; }
            let (doc, notes) = comments_above(&r, &lines, i);
            entries.push((m[3].to_string(), signature(&r, &lines, i), i + 1, doc, notes, m[1].chars().count()));
        }
        if entries.is_empty() { continue; }
        let mdoc = module_doc(&lines);
        let page = f.replace('/', "__").replace(".rs", ".md");
        let mut md: Vec<String> = vec![format!("# `{f}`"), String::new()];
        if !mdoc.is_empty() { md.extend(mdoc.iter().map(|x| if x.is_empty() { ">".to_string() } else { format!("> {x}") })); md.push(String::new()); }
        md.push(format!("{} functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.", entries.len()));
        md.push(String::new());
        for (name, sig, line, doc, notes, indent) in &entries {
            total += 1;
            md.extend([format!("## `{name}`"), String::new(), format!("`{f}:{line}`{}", if *indent > 0 { "  (method / nested)" } else { "" }), String::new(), "```rust".into(), sig.clone(), "```".into(), String::new()]);
            if !doc.is_empty() { md.push(doc.join(" ").trim().to_string()); md.push(String::new()); }
            if !notes.is_empty() { md.push(format!("Notes from the code: {}", notes.join(" ").trim())); md.push(String::new()); }
            if doc.is_empty() && notes.is_empty() { undocumented += 1; md.extend(["_No comment in the code; the signature above is the description._".to_string(), String::new()]); }
        }
        std::fs::write(outdir.join(&page), md.join("\n"))?;
        let without = entries.iter().filter(|e| e.3.is_empty() && e.4.is_empty()).count();
        index.push((f.clone(), page, entries.len(), without));
    }
    let mut idx: Vec<String> = vec![
        "# Function reference".into(), String::new(),
        "One page per source file, one entry per function (tests excluded), generated from the code by `qwen-tools gen-function-docs`.".into(),
        "No account numbers, credentials or API keys appear in these pages (the generator refuses to run if it finds any).".into(), String::new(),
        format!("{total} functions in {} files; {undocumented} have no comment in the code (listed with their signature only).", index.len()), String::new(),
        "| File | Functions | Without comment |".into(), "|---|---|---|".into()];
    idx.extend(index.iter().map(|(f, p, n, u)| format!("| [`{f}`]({p}) | {n} | {u} |")));
    std::fs::write(outdir.join("README.md"), idx.join("\n") + "\n")?;
    Ok(format!("{}: {total} functions in {} files, {undocumented} without comment", root.display(), index.len()))
}

pub fn main(args: &[String]) -> i32 {
    let Some(root) = args.first() else { eprintln!("usage: gen-function-docs <repo root>"); return 2 };
    match generate(Path::new(root)) { Ok(s) => { println!("{s}"); 0 } Err(e) => { eprintln!("{e}"); 1 } }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_one_line_up_to_the_brace() {
        let r = res();
        let lines = ["pub fn demo(", "    a: u32,", "    b: &str,", ") -> Result<()> {", "    body"];
        assert_eq!(signature(&r, &lines, 0), "pub fn demo( a: u32, b: &str, ) -> Result<()>");
    }

    #[test]
    fn doc_and_plain_comments_above_are_found_skipping_attributes() {
        let r = res();
        let lines = ["// NOTE(agents): careful", "/// Does the thing.", "/// More.", "#[inline]", "pub fn x() {}"];
        let (doc, notes) = comments_above(&r, &lines, 4);
        assert_eq!(doc, vec!["Does the thing.", "More."]);
        assert_eq!(notes, vec!["Note: careful"]);
    }

    #[test]
    fn test_modules_are_excluded_and_secrets_refused() {
        let lines = ["fn a() {}", "#[cfg(test)]", "mod tests {", "    fn b() {}", "}", "fn c() {}"];
        assert_eq!(test_ranges(&lines).into_iter().collect::<Vec<_>>(), vec![1, 2, 3, 4]);
        // assembled at run time so this source never contains a credential-shaped literal (the generator would refuse to document it)
        let fake = format!("let {}_key = {}abcdef123456{};", "api", "\"", "\"");
        assert!(res().secret.is_match(&fake));
        assert!(!res().secret.is_match("let tokens = vec![];"));
    }
}

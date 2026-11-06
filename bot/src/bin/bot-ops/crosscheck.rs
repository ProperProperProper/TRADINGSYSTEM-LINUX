//! `bot-ops cross-check [target ...]`: proves the crate still compiles for Linux and Windows from any machine, without rustup
//! (was ops/cross_check.sh). It uses a private copy of the same rustc in ~/.cache/xtargets/rust (official build plus the Linux
//! and Windows standard libraries, see docs/SETUP_LINUX.md "cross-checking"). `cargo check` needs no machine code, so the C
//! compiler and archiver that libsqlite3-sys and ring call are replaced by `bot-ops fake-cc`, which writes empty output files.

use crate::common;
use std::process::Command;

const DEFAULT_TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-gnu"];
const MAX_LINES: usize = 70;

/// The file a C compiler or archiver call asks to create: `-o FILE`, `-oFILE`, else the first `*.a` argument.
pub fn fake_cc_output(args: &[String]) -> Option<String> {
    let mut out = None;
    let mut prev = "";
    for a in args {
        if prev == "-o" {
            out = Some(a.clone());
        } else if let Some(rest) = a.strip_prefix("-o").filter(|r| !r.is_empty()) {
            out = Some(rest.to_string());
        }
        prev = a;
    }
    out.or_else(|| args.iter().find(|a| a.ends_with(".a")).cloned())
}

/// `bot-ops fake-cc ARGS`: stub C compiler / archiver that succeeds and writes an empty file wherever an output is asked for.
pub fn fake_cc(args: &[&str]) -> i32 {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    if let Some(out) = fake_cc_output(&args) {
        let _ = std::fs::write(out, b"");
    }
    0
}

/// Lines starting with error/warning, "Finished" or "could not", each with up to 7 following lines, at most `max` lines
/// (what the old script's `grep -E ... -A7 | head` printed).
pub fn filter_output(text: &str, max: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut keep = vec![false; lines.len()];
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("error")
            || l.starts_with("warning")
            || l.contains("Finished")
            || l.contains("could not")
        {
            for k in keep.iter_mut().take(lines.len().min(i + 8)).skip(i) {
                *k = true;
            }
        }
    }
    lines
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l.to_string())
        .take(max)
        .collect()
}

pub fn run(targets: &[&str]) -> i32 {
    let x =
        std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache/xtargets");
    let rustc = x.join("rust/bin/rustc");
    if !rustc.exists() {
        eprintln!(
            "cross-check: {} missing (see the header of src/bin/bot-ops/crosscheck.rs)",
            x.join("rust").display()
        );
        return 2;
    }
    let me = std::env::current_exe()
        .map(|p| format!("{} fake-cc", p.display()))
        .unwrap_or_default();
    let max = std::env::var("LINES_MAX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_LINES);
    let list: Vec<&str> = if targets.is_empty() {
        DEFAULT_TARGETS.to_vec()
    } else {
        targets.iter().flat_map(|t| t.split_whitespace()).collect()
    };
    for t in list {
        println!("== cargo check --target {t}");
        let u = t.replace('-', "_");
        let out = Command::new("cargo")
            .args(["check", "--target", t, "--lib", "--bins", "--examples"])
            .current_dir(common::repo())
            .env("RUSTC", &rustc)
            .env(format!("CC_{u}"), &me)
            .env(format!("AR_{u}"), &me)
            .env("CARGO_TARGET_DIR", "target/xcheck")
            .output();
        match out {
            Ok(o) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                for l in filter_output(&text, max) {
                    println!("{l}");
                }
            }
            Err(e) => eprintln!("cross-check: could not run cargo: {e}"),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn output_file_is_found_in_every_form() {
        assert_eq!(
            fake_cc_output(&v(&["-c", "x.c", "-o", "x.o"])),
            Some("x.o".into())
        );
        assert_eq!(
            fake_cc_output(&v(&["-c", "-ox.o", "x.c"])),
            Some("x.o".into())
        );
        assert_eq!(
            fake_cc_output(&v(&["crs", "libfoo.a", "a.o"])),
            Some("libfoo.a".into())
        );
        assert_eq!(fake_cc_output(&v(&["-c", "x.c"])), None);
    }

    #[test]
    fn fake_cc_writes_an_empty_file() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("out.o");
        assert_eq!(fake_cc(&["-o", f.to_str().unwrap()]), 0);
        assert_eq!(std::fs::metadata(&f).unwrap().len(), 0);
    }

    #[test]
    fn filter_keeps_errors_with_context_and_the_finish_line() {
        let text =
            "Checking a\nerror[E0001]: bad\n  --> x.rs\nCompiling b\n    Finished dev\nunrelated";
        assert_eq!(
            filter_output(text, 70),
            vec![
                "error[E0001]: bad",
                "  --> x.rs",
                "Compiling b",
                "    Finished dev",
                "unrelated"
            ]
        );
        assert_eq!(filter_output(text, 2).len(), 2);
        assert!(filter_output("Checking a\nCompiling b", 70).is_empty());
    }
}

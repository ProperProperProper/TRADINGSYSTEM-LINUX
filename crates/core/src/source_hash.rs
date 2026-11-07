// A fingerprint of the source files a program was built from, so "is it running the latest code?" has an exact answer instead of a guess from file times.
// The same algorithm is used when a binary is built (build.rs `include!`s this file) and when `thomas doctor` checks, and papercompmoomoo's `deploy.code_hash`
// implements it in Python: SHA-256 over, for every file sorted by relative path with `/` separators, the path, a NUL byte, the file's bytes and a NUL byte.
// Files under `target`, `.git`, `__pycache__`, `.venv` and `data` are skipped, as is anything that is not a source file of the listed extensions.
use sha2::{Digest, Sha256};
use std::path::Path;

fn collect(
    root: &Path,
    dir: &Path,
    extensions: &[&str],
    out: &mut Vec<(String, std::path::PathBuf)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !matches!(
                name.as_str(),
                "target" | ".git" | "__pycache__" | ".venv" | "data" | "node_modules"
            ) && !name.ends_with(".egg-info")
            {
                collect(root, &p, extensions, out);
            }
        } else if (p
            .extension()
            .is_some_and(|x| extensions.iter().any(|w| x == *w))
            || extensions.contains(&name.as_str()))
            && p.strip_prefix(root).is_ok()
        {
            let rel = p
                .strip_prefix(root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            out.push((rel, p));
        }
    }
}

/// Fingerprint of the files with the given `extensions` (or exact file names) under each of `dirs` (relative to `root`).
pub fn source_hash(root: &Path, dirs: &[&str], extensions: &[&str]) -> String {
    let mut files = Vec::new();
    for d in dirs {
        collect(root, &root.join(d), extensions, &mut files);
    }
    files.sort();
    let mut h = Sha256::new();
    for (rel, path) in files {
        h.update(rel.as_bytes());
        h.update([0u8]);
        h.update(std::fs::read(&path).unwrap_or_default());
        h.update([0u8]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The files that make up the `thomas` binaries: this workspace's crates and the bot library they use.
pub const BUILD_DIRS: [&str; 2] = ["crates", "bot/src"];
pub const BUILD_EXTENSIONS: [&str; 2] = ["rs", "Cargo.toml"];

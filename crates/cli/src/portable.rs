//! Moving the whole system to another machine: `thomas bundle` packs the irreplaceable data (moomoo's candle quota is 100 stocks per 7 days and Yahoo reaches back
//! only about 60 days, so the history cannot be fetched again), `thomas restore` unpacks it, and `thomas setup` runs every step from a fresh clone to a running system.
//! The code and docs travel in git; the data travels as a bundle (a GitHub release asset of the private repository, or a file).
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::process::Command;

/// SQLite files under the repository that the bundle carries (copied with VACUUM INTO: consistent even while a job writes).
pub const DB_FILES: [&str; 9] = [
    "data/thomas.sqlite3",
    "data/candles.sqlite3",
    "data/backtests.sqlite3",
    "papercompmoomoo/data/brain.sqlite3",
    "papercompmoomoo/data/board.sqlite3",
    "papercompmoomoo/data/journal.sqlite3",
    "papercompmoomoo/data/common1m.sqlite3",
    "papercompmoomoo/data/asx_journal.sqlite3",
    "papercompmoomoo/data/hk_journal.sqlite3",
];
/// Small files that travel with them.
pub const PLAIN_FILES: [&str; 7] = [
    "data/selection.json",
    "data/spy_holdings.csv",
    "data/spy_removed.csv",
    "data/tuned.json",
    "data/bench.json",
    "papercompmoomoo/data/status.json",
    "papercompmoomoo/data/status.csv",
];
/// The bot's own store folder (inside the project, `state/ClaudeMoomooBotArchive`): its candle store (the importer appends to it and `thomas snapshot` copies from
/// it), its backtest archives and its per-interval parameter records. A new machine must start from the same history. Old `quarantine-*` folders are not carried.
pub fn bot_archive() -> PathBuf {
    moomoo_bot::platform::bot_archive_dir()
}

fn sh(cmd: &mut Command) -> Result<()> {
    let o = cmd
        .output()
        .with_context(|| format!("starting {:?}", cmd.get_program()))?;
    if !o.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&o.stderr).trim()
        );
    }
    Ok(())
}

fn vacuum_into(from: &Path, to: &Path) -> Result<()> {
    let c = Connection::open_with_flags(from, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    c.busy_timeout(std::time::Duration::from_secs(60))?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    c.execute("VACUUM INTO ?1", [to.to_string_lossy().as_ref()])?;
    Ok(())
}

fn integrity_ok(path: &Path) -> Result<bool> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let r: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    Ok(r == "ok")
}

fn safe_stamp(stamp: &str) -> Result<()> {
    anyhow::ensure!(
        !stamp.is_empty()
            && stamp
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "stamp must contain only ASCII letters, digits, '-', '_' or '.'"
    );
    Ok(())
}

fn safe_under(base: &Path, relative: &Path) -> Result<PathBuf> {
    anyhow::ensure!(
        !relative.is_absolute()
            && !relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "path escapes its base directory"
    );
    let root = base.canonicalize()?;
    let candidate = base.join(relative);
    let mut existing = candidate.as_path();
    while !existing.exists() {
        existing = existing.parent().context("path has no existing parent")?;
    }
    anyhow::ensure!(
        existing.canonicalize()?.starts_with(&root),
        "path resolves outside its base directory"
    );
    if std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("refusing to write through symlink {}", candidate.display());
    }
    Ok(candidate)
}

/// Whether `path` (which may not exist yet) lies inside `root`: the nearest existing ancestor is resolved (links followed) and must be inside.
fn inside(root: &Path, path: &Path) -> Result<bool> {
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent().context("path has no existing parent")?;
    }
    Ok(existing.canonicalize()?.starts_with(root))
}

fn extracted(tmp: &Path, relative: &Path) -> Result<PathBuf> {
    let root = tmp.canonicalize()?;
    let path = tmp.join(relative);
    let real = path.canonicalize()?;
    anyhow::ensure!(
        real.starts_with(&root),
        "bundle entry {} resolves outside the temporary directory",
        relative.display()
    );
    Ok(real)
}

fn temp_dir(tag: &str) -> Result<PathBuf> {
    let base = std::env::temp_dir().join(format!("thomas-{tag}-{}", std::process::id()));
    for suffix in 0..100u32 {
        let d = if suffix == 0 {
            base.clone()
        } else {
            base.with_extension(suffix.to_string())
        };
        match std::fs::create_dir(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!(
        "could not create a unique temporary directory under {}",
        base.display()
    )
}

/// Writes `<out_dir>/thomas-data-<stamp>.tar.gz` holding data/ (the databases, the picks, the tuning results) and archive/ (every SQLite file in the bot's store
/// folder `bot_archive`, when it exists). Returns the file.
pub fn bundle(repo: &Path, out_dir: &Path, stamp: &str, bot_archive: &Path) -> Result<PathBuf> {
    safe_stamp(stamp)?;
    let repo_root = repo.canonicalize()?;
    anyhow::ensure!(
        inside(&repo_root, bot_archive)?,
        "bot archive must be inside the project"
    );
    anyhow::ensure!(
        repo.join(DB_FILES[0]).exists(),
        "{} does not exist: nothing to bundle",
        DB_FILES[0]
    );
    std::fs::create_dir_all(out_dir)?;
    let stage = out_dir.join(format!("stage-{stamp}"));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join("data"))?;
    for f in DB_FILES {
        let src = repo.join(f);
        if src.exists() {
            vacuum_into(&src, &stage.join(f))?;
        }
    }
    for f in PLAIN_FILES {
        let src = repo.join(f);
        if src.exists() {
            std::fs::copy(&src, stage.join(f))?;
        }
    }
    if bot_archive.is_dir() {
        for e in std::fs::read_dir(bot_archive)? {
            let e = e?;
            let p = e.path();
            if p.is_file() && p.extension().is_some_and(|x| x == "sqlite3") {
                vacuum_into(&p, &stage.join("archive").join(e.file_name()))?;
            }
        }
    }
    let out = out_dir.join(format!("thomas-data-{stamp}.tar.gz"));
    let r = sh(Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(&stage)
        .arg("."));
    let _ = std::fs::remove_dir_all(&stage);
    r?;
    Ok(out)
}

/// Unpacks a bundle into `repo` (and the bot's store files into the folder `bot_archive`). Every database is integrity-checked before anything is put in place, and nothing that
/// exists is overwritten unless `force`. Returns the files placed.
pub fn restore(
    repo: &Path,
    tarball: &Path,
    bot_archive: &Path,
    force: bool,
) -> Result<Vec<String>> {
    let repo_root = repo.canonicalize()?;
    anyhow::ensure!(
        inside(&repo_root, bot_archive)?,
        "bot archive must be inside the project"
    );
    let tmp = temp_dir("restore")?;
    let result = (|| -> Result<Vec<String>> {
        sh(Command::new("tar")
            .arg("-xzf")
            .arg(tarball)
            .arg("-C")
            .arg(&tmp))?;
        let mut moves: Vec<(PathBuf, PathBuf)> = Vec::new();
        for f in DB_FILES.iter().chain(PLAIN_FILES.iter()) {
            let src = tmp.join(f);
            if src.exists() {
                moves.push((
                    extracted(&tmp, Path::new(f))?,
                    safe_under(repo, Path::new(f))?,
                ));
            }
        }
        if tmp.join("archive").is_dir() {
            let archive = extracted(&tmp, Path::new("archive"))?;
            std::fs::create_dir_all(bot_archive)?;
            for e in std::fs::read_dir(archive)? {
                let e = e?;
                let p = e.path();
                if p.is_file() && p.extension().is_some_and(|x| x == "sqlite3") {
                    let name = e.file_name();
                    let relative = Path::new("archive").join(&name);
                    moves.push((
                        extracted(&tmp, &relative)?,
                        safe_under(bot_archive, Path::new(&name))?,
                    ));
                }
            }
        }
        anyhow::ensure!(
            !moves.is_empty(),
            "{} holds no data files",
            tarball.display()
        );
        for (src, _) in moves
            .iter()
            .filter(|(s, _)| s.extension().is_some_and(|e| e == "sqlite3"))
        {
            anyhow::ensure!(
                integrity_ok(src)?,
                "{} failed its integrity check: the bundle is damaged",
                src.display()
            );
        }
        let existing: Vec<String> = moves
            .iter()
            .filter(|(_, d)| d.exists())
            .map(|(_, d)| d.display().to_string())
            .collect();
        if !force && !existing.is_empty() {
            bail!(
                "refusing to overwrite {} (use --force)",
                existing.join(", ")
            );
        }
        let mut placed = Vec::new();
        for (src, dst) in moves {
            if let Some(p) = dst.parent() {
                std::fs::create_dir_all(p)?;
            }
            for ext in ["-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{ext}", dst.display()));
            }
            std::fs::copy(&src, &dst)?;
            placed.push(dst.display().to_string());
        }
        Ok(placed)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// `owner/name` from a GitHub remote URL (https or ssh).
pub fn slug_from_url(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches(".git");
    let tail = u
        .strip_prefix("https://github.com/")
        .or_else(|| u.strip_prefix("http://github.com/"))
        .or_else(|| u.strip_prefix("git@github.com:"))?;
    let mut it = tail.split('/');
    let (o, n) = (it.next()?, it.next()?);
    (!o.is_empty() && !n.is_empty() && it.next().is_none()).then(|| format!("{o}/{n}"))
}

fn origin_slug(repo: &Path) -> Result<String> {
    let o = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(repo)
        .output()?;
    slug_from_url(&String::from_utf8_lossy(&o.stdout)).context("origin is not a GitHub repository")
}

/// Uploads a bundle as a release asset of this repository (private like the repository itself).
pub fn publish(repo: &Path, tarball: &Path, stamp: &str) -> Result<String> {
    let slug = origin_slug(repo)?;
    let tag = format!("data-{stamp}");
    sh(Command::new("gh")
        .args(["release", "create", &tag, "--repo", &slug, "--title", &format!("Data bundle {stamp}"), "--notes", "Databases, picks and tuning results from `thomas bundle`; restore with `thomas restore`."])
        .arg(tarball))?;
    Ok(format!(
        "published {} to {slug} as release {tag}",
        tarball.display()
    ))
}

/// Downloads the newest data bundle release of this repository into `dir` and returns the file.
pub fn download_latest(repo: &Path, dir: &Path) -> Result<PathBuf> {
    let slug = origin_slug(repo)?;
    std::fs::create_dir_all(dir)?;
    sh(Command::new("gh")
        .args([
            "release",
            "download",
            "--repo",
            &slug,
            "--pattern",
            "thomas-data-*.tar.gz",
            "--clobber",
            "--dir",
        ])
        .arg(dir))?;
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("thomas-data-"))
        })
        .collect();
    found.sort();
    found.pop().context("no data bundle was downloaded")
}

fn on_path(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// The install hint for a missing tool.
pub fn hint(tool: &str) -> &'static str {
    match tool {
        "cargo" => "install Rust: https://rustup.rs",
        "git" => "install git: xcode-select --install",
        "gh" => "install the GitHub CLI and log in: brew install gh && gh auth login",
        _ => "install it",
    }
}

fn run_in(dir: &Path, cmd: &str, args: &[&str]) -> Result<()> {
    let st = Command::new(cmd)
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("starting {cmd}"))?;
    anyhow::ensure!(st.success(), "{cmd} {} failed", args.join(" "));
    Ok(())
}

/// Every step from a fresh clone to the running system, in order. `bundle` is a data bundle file; without it the newest release of this repository is used.
pub fn setup_steps(repo: &Path, bundle: Option<PathBuf>) -> Vec<SetupStep> {
    let r = repo.to_path_buf();
    let (r2, r3, r4, r5, r6) = (r.clone(), r.clone(), r.clone(), r.clone(), r.clone());
    let mut steps = vec![
        SetupStep {
            name: "tools (cargo, git)",
            done: Box::new(|| false),
            run: Box::new(|| {
                let missing: Vec<&str> = ["cargo", "git"]
                    .into_iter()
                    .filter(|t| !on_path(t))
                    .collect();
                anyhow::ensure!(
                    missing.is_empty(),
                    "missing: {}",
                    missing
                        .iter()
                        .map(|t| format!("{t} ({})", hint(t)))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
                Ok("all present".into())
            }),
        },
        SetupStep {
            name: "git hooks (journal and tasks must change with code; commits push)",
            done: Box::new({
                let r = r.clone();
                move || {
                    Command::new("git")
                        .args(["config", "core.hooksPath"])
                        .current_dir(&r)
                        .output()
                        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "hooks")
                }
            }),
            run: Box::new({
                let r = r.clone();
                move || {
                    run_in(&r, "git", &["config", "core.hooksPath", "hooks"])
                        .map(|_| "core.hooksPath = hooks".into())
                }
            }),
        },
        SetupStep {
            name: "data (databases, picks, the bot's candle store)",
            done: Box::new({
                let r = r2.clone();
                move || r.join(DB_FILES[0]).exists()
            }),
            run: Box::new(move || {
                let file = match &bundle {
                    Some(f) => f.clone(),
                    None => download_latest(&r2, &r2.join("data/bundles"))?,
                };
                let placed = restore(&r2, &file, &bot_archive(), false)?;
                Ok(format!("{} files placed", placed.len()))
            }),
        },
        SetupStep {
            name: "sibling project papercompmoomoo (candle feed)",
            done: Box::new({
                let r = r.clone();
                move || r.join("papercompmoomoo/src").exists()
            }),
            run: Box::new({
                let r = r.clone();
                move || {
                    if r.join("papercompmoomoo/src").exists() {
                        return Ok("present in repository".into());
                    }
                    let slug = origin_slug(&r)?;
                    let owner = slug.split('/').next().context("origin has no owner")?;
                    run_in(
                        &r,
                        "gh",
                        &[
                            "repo",
                            "clone",
                            &format!("{owner}/papercompmoomoo"),
                            "papercompmoomoo",
                        ],
                    )?;
                    Ok("cloned".into())
                }
            }),
        },
        SetupStep {
            name: "sibling environment and its converter and supervisor jobs",
            done: Box::new(|| {
                ["converter", "supervisor"]
                    .iter()
                    .all(|j| crate::ops::agent_installed(&format!("local.papercompmoomoo.{j}")))
            }),
            run: Box::new({
                let r = r.clone();
                move || {
                    let dir = r.join("papercompmoomoo");
                    anyhow::ensure!(dir.exists(), "papercompmoomoo is not cloned yet");
                    if !dir.join(".venv").exists() {
                        run_in(&dir, "python3", &["-m", "venv", ".venv"])?;
                    }
                    run_in(&dir, ".venv/bin/pip", &["install", "--quiet", "-e", "."])?;
                    let st = Command::new(".venv/bin/python")
                        .args(["-m", "papercompmoomoo.install", "all"])
                        .env("PYTHONPATH", "src")
                        .current_dir(&dir)
                        .status()?;
                    anyhow::ensure!(
                        st.success(),
                        "installing the competition project's jobs failed"
                    );
                    Ok("converter and supervisor jobs installed".into())
                }
            }),
        },
        SetupStep {
            name: "moomoo SDK, pinned (tools/moomoo-venv)",
            done: Box::new({
                let r = r.clone();
                move || {
                    let want = std::fs::read_to_string(r.join("versions.lock"))
                        .map(|t| thomas_core::export::parse_lock(&t))
                        .unwrap_or_default();
                    let wanted = format!(
                        "moomoo_api-{}.dist-info",
                        want.get("moomoo_api").cloned().unwrap_or_default()
                    );
                    std::fs::read_dir(r.join("tools/moomoo-venv/lib"))
                        .ok()
                        .into_iter()
                        .flatten()
                        .flatten()
                        .any(|py| py.path().join("site-packages").join(&wanted).is_dir())
                }
            }),
            run: Box::new({
                let r = r.clone();
                move || {
                    let want = thomas_core::export::parse_lock(&std::fs::read_to_string(
                        r.join("versions.lock"),
                    )?);
                    let ver = want
                        .get("moomoo_api")
                        .context("moomoo_api missing in versions.lock")?
                        .clone();
                    run_in(&r, "python3", &["-m", "venv", "tools/moomoo-venv"])?;
                    run_in(
                        &r,
                        "tools/moomoo-venv/bin/pip",
                        &["install", "--quiet", &format!("moomoo-api=={ver}")],
                    )?;
                    Ok(format!("moomoo-api {ver} installed"))
                }
            }),
        },
        SetupStep {
            name: "build (thomas, bot-ops)",
            done: Box::new(|| false),
            run: Box::new(move || {
                run_in(&r3, "cargo", &["build", "--release"])?;
                run_in(
                    &r3.join("bot"),
                    "cargo",
                    &["build", "--release", "--bin", "bot-ops"],
                )?;
                if cfg!(target_os = "macos") {
                    // a stable identifier keeps the firewall from asking again after every rebuild
                    run_in(
                        &r3,
                        "codesign",
                        &[
                            "-f",
                            "-s",
                            "-",
                            "-i",
                            "com.thomasai.thomas",
                            "target/release/thomas",
                        ],
                    )?;
                    run_in(
                        &r3.join("bot"),
                        "codesign",
                        &[
                            "-f",
                            "-s",
                            "-",
                            "-i",
                            "claude-moomoo-bot",
                            "target/release/bot-ops",
                        ],
                    )?;
                }
                Ok("built".into())
            }),
        },
        SetupStep {
            name: "scheduled ThomasAI brain cycle and read-only portal",
            done: Box::new(|| {
                crate::ops::agent_installed(crate::ops::PORTAL_LABEL)
                    && crate::ops::agent_installed(crate::ops::LABEL)
            }),
            run: Box::new(move || {
                run_in(&r4, "target/release/thomas", &["schedule", "install"])?;
                Ok("installed".into())
            }),
        },
        SetupStep {
            name: "bot candle importer (needs moomoo OpenD, logged in)",
            done: Box::new(|| crate::ops::agent_installed("com.claudemoomoobot.universefetch")),
            run: Box::new(move || {
                let opend = if cfg!(target_os = "linux") {
                    [
                        PathBuf::from("/usr/local/bin/moomoo-opend-gui"),
                        crate::ops::expand_tilde(Path::new("~/.com.moomoo.OpenD")),
                    ]
                    .into_iter()
                    .find(|p| p.exists())
                    .unwrap_or_else(|| PathBuf::from("/usr/local/bin/moomoo-opend-gui"))
                } else {
                    crate::ops::expand_tilde(Path::new("~/Applications/moomoo_OpenD.app"))
                };
                anyhow::ensure!(
                    opend.exists(),
                    "moomoo OpenD is not installed at {}: install version 10.11.7108 (see versions.lock), log in with your moomoo account, then run `thomas setup` again",
                    opend.display()
                );
                run_in(
                    &r5.join("bot"),
                    "target/release/bot-ops",
                    &["install-fetch"],
                )?;
                Ok("installed".into())
            }),
        },
        SetupStep {
            name: "verify the data",
            done: Box::new(|| false),
            run: Box::new(move || {
                run_in(&r6, "target/release/thomas", &["verify"])?;
                Ok("all checks passed".into())
            }),
        },
    ];
    let rank = |n: &str| {
        [
            "tools",
            "git hooks",
            "build",
            "data",
            "scheduled jobs",
            "sibling project",
            "sibling environment",
            "moomoo SDK",
            "bot candle importer",
            "verify",
        ]
        .iter()
        .position(|p| n.starts_with(p))
        .unwrap_or(99)
    };
    steps.sort_by_key(|s| rank(s.name));
    steps
}

/// A setup step: what it does and the command that does it.
pub struct SetupStep {
    pub name: &'static str,
    /// Skip when this returns true (already done).
    pub done: Box<dyn Fn() -> bool>,
    pub run: Box<dyn Fn() -> Result<String>>,
}

/// Runs the steps in order, printing each result; a failing step is reported and the rest still run. Returns the number of failures.
pub fn run_steps(steps: &[SetupStep], dry_run: bool) -> usize {
    let mut failed = 0;
    for s in steps {
        if (s.done)() {
            println!("ok       {} (already done)", s.name);
        } else if dry_run {
            println!("would do {}", s.name);
        } else {
            match (s.run)() {
                Ok(m) => println!("done     {}: {}", s.name, m.lines().last().unwrap_or("")),
                Err(e) => {
                    failed += 1;
                    println!("FAILED   {}: {e:#}", s.name);
                }
            }
        }
    }
    failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("thomas-portable-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("data")).unwrap();
        d
    }

    #[test]
    fn a_bundle_round_trips_the_databases_and_refuses_to_overwrite_without_force() {
        let (a, b) = (temp("a"), temp("b"));
        {
            let c = thomas_core::store::open(&a.join("data/thomas.sqlite3")).unwrap();
            c.execute(
                "INSERT INTO import_state(source, last_ms) VALUES('x', 1)",
                [],
            )
            .unwrap();
        }
        std::fs::write(a.join("data/selection.json"), "{\"symbols\":[\"AAPL\"]}").unwrap();
        let arch = a.join("bot-store");
        std::fs::create_dir_all(arch.join("quarantine-old")).unwrap();
        Connection::open(arch.join("moomoo.sqlite3"))
            .unwrap()
            .execute_batch("CREATE TABLE k(v); INSERT INTO k VALUES(7);")
            .unwrap();
        Connection::open(arch.join("backtest_archive.sqlite3"))
            .unwrap()
            .execute_batch("CREATE TABLE k(v); INSERT INTO k VALUES(8);")
            .unwrap();
        Connection::open(arch.join("quarantine-old/old.sqlite3"))
            .unwrap()
            .execute_batch("CREATE TABLE k(v);")
            .unwrap();
        let tar = bundle(&a, &a.join("out"), "t1", &arch).unwrap();
        assert!(
            tar.exists() && !a.join("out/stage-t1").exists(),
            "the staging folder is cleaned up"
        );
        let b_arch = b.join("restored-store");
        let placed = restore(&b, &tar, &b_arch, false).unwrap();
        assert!(placed.len() >= 3, "{placed:?}");
        let c = Connection::open(b.join("data/thomas.sqlite3")).unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM import_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            std::fs::read_to_string(b.join("data/selection.json")).unwrap(),
            "{\"symbols\":[\"AAPL\"]}"
        );
        let v = |f: &str| -> i64 {
            Connection::open(b_arch.join(f))
                .unwrap()
                .query_row("SELECT v FROM k", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(
            (v("moomoo.sqlite3"), v("backtest_archive.sqlite3")),
            (7, 8),
            "the bot's candle store and backtest archive come along"
        );
        assert!(
            !b_arch.join("quarantine-old").exists() && !b_arch.join("old.sqlite3").exists(),
            "quarantine folders stay behind"
        );
        let e = restore(&b, &tar, &b_arch, false).unwrap_err();
        assert!(format!("{e}").contains("refusing to overwrite"), "{e}");
        assert!(restore(&b, &tar, &b_arch, true).is_ok());
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
    }

    #[test]
    fn a_damaged_or_empty_bundle_is_refused_before_anything_is_placed() {
        let (a, b) = (temp("c"), temp("d"));
        std::fs::write(a.join("junk.tar.gz"), b"not a tarball").unwrap();
        assert!(restore(&b, &a.join("junk.tar.gz"), &b.join("arch"), false).is_err());
        assert!(!b.join("data/thomas.sqlite3").exists());
        assert!(
            bundle(&b, &b.join("out"), "x", &b.join("none")).is_err(),
            "no thomas.sqlite3, nothing to bundle"
        );
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b));
    }

    #[test]
    fn every_setup_step_is_named_in_order_and_missing_tools_come_with_a_hint() {
        let names: Vec<&str> = setup_steps(Path::new("/nonexistent"), None)
            .iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names.len(), 10);
        let order = [
            "tools",
            "git hooks",
            "build",
            "data",
            "scheduled jobs",
            "sibling project",
            "sibling environment",
            "moomoo SDK",
            "bot candle importer",
            "verify",
        ];
        for (n, want) in names.iter().zip(order) {
            assert!(n.starts_with(want), "{n} should start with {want}");
        }
        assert!(hint("cargo").contains("rustup") && hint("gh").contains("gh auth login"));
        // a fresh checkout has no data yet, so the data step is not "done"
        let steps = setup_steps(Path::new("/nonexistent"), None);
        assert!(
            !(steps[3].done)(),
            "the data step is not done on a fresh checkout"
        );
    }

    #[test]
    fn github_slugs_come_from_https_and_ssh_remotes() {
        assert_eq!(
            slug_from_url("https://github.com/ProperProperProper/TradingSystem.git\n").as_deref(),
            Some("ProperProperProper/TradingSystem")
        );
        assert_eq!(
            slug_from_url("git@github.com:o/n.git").as_deref(),
            Some("o/n")
        );
        assert_eq!(slug_from_url("https://example.com/o/n"), None);
        assert_eq!(slug_from_url("https://github.com/only"), None);
        assert_eq!(slug_from_url("https://notgithub.com/o/n"), None);
        assert_eq!(slug_from_url("https://github.com.evil/o/n"), None);
    }

    #[test]
    fn a_state_folder_that_does_not_exist_yet_is_inside_the_project_but_a_foreign_one_is_not() {
        let a = temp("inside");
        let root = a.canonicalize().unwrap();
        assert!(
            inside(&root, &a.join("state/ClaudeMoomooBotArchive")).unwrap(),
            "a fresh clone has no state/ folder yet"
        );
        assert!(!inside(&root, &std::env::temp_dir().join("elsewhere-never-created")).unwrap());
        let _ = std::fs::remove_dir_all(a);
    }

    #[test]
    fn bundle_stamps_cannot_escape_the_output_directory() {
        let (a, _) = (temp("stamp"), temp("stamp-unused"));
        let err = bundle(&a, &a.join("out"), "../outside", &a.join("bot-store"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("stamp"));
        let _ = std::fs::remove_dir_all(a);
    }

    #[test]
    fn setup_skips_done_steps_reports_failures_and_still_runs_the_rest() {
        let ran = Rc::new(Cell::new(0));
        let mk = |name: &'static str, done: bool, ok: bool, ran: Rc<Cell<i32>>| SetupStep {
            name,
            done: Box::new(move || done),
            run: Box::new(move || {
                ran.set(ran.get() + 1);
                if ok {
                    Ok("fine".into())
                } else {
                    bail!("broken")
                }
            }),
        };
        let steps = [
            mk("a", true, true, ran.clone()),
            mk("b", false, false, ran.clone()),
            mk("c", false, true, ran.clone()),
        ];
        assert_eq!(run_steps(&steps, true), 0);
        assert_eq!(ran.get(), 0, "a dry run changes nothing");
        assert_eq!(run_steps(&steps, false), 1);
        assert_eq!(
            ran.get(),
            2,
            "the done step is skipped, the failing one does not stop the next"
        );
    }
}

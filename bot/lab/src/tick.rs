//! The scheduled hourly tick. One idempotent, resumable, single-instance pass that imports the real archives (the bot's paper record and backtest
//! archive), refreshes the win/lose analysis and the promotion request, writes the report, backs up, and checks the size limit. Every step is
//! guarded, time-boxed and logged; a failing step never stops the next one; with no data it does nothing harmful and says so.

use crate::config::Config;
use crate::context::{self, Ctx};
use crate::util::now_ts;
use crate::{btarchive, cull, db, paper, report, sysinfo};
use anyhow::Result;
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Probes {
    pub load1: Option<f64>,
    pub ncpu: usize,
    pub free_gb: Option<f64>,
}

impl Probes {
    /// Live probes of this machine: load, cores, free memory.
    pub fn live(_cfg: &Config) -> Self {
        Self { load1: sysinfo::load1(), ncpu: sysinfo::ncpu(), free_gb: sysinfo::free_gb() }
    }
}

#[derive(Debug, Clone)]
pub struct TickOptions {
    pub full: bool,
    pub now: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    /// ok | skipped | failed | blocked
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct TickReport {
    pub full: bool,
    pub steps: Vec<Step>,
    pub log_path: Option<PathBuf>,
}

impl TickReport {
    /// The step called `name`.
    pub fn get(&self, name: &str) -> Option<&Step> { self.steps.iter().find(|s| s.name == name) }
    /// The tick report as text, one line per step.
    pub fn text(&self) -> String {
        let mut s = format!("tick ({})\n", if self.full { "full" } else { "light" });
        for st in &self.steps { let _ = writeln!(s, "  [{}] {}: {}", st.status, st.name, st.detail); }
        s
    }
}

struct LockGuard(PathBuf);
// Removes the tick lock file when the guard goes out of scope.
impl Drop for LockGuard { fn drop(&mut self) { std::fs::remove_file(&self.0).ok(); } }

/// Whether a process with this id exists (signal 0 checks without sending anything).
fn pid_alive(pid: u32) -> bool {
    crate::platform::pid_alive(pid)
}

/// A named counter from the memory database, if set.
fn counter(mem: &rusqlite::Connection, name: &str) -> Option<i64> {
    mem.query_row("SELECT value FROM counters WHERE name = ?1", [name], |r| r.get(0)).ok()
}

/// Sets a named counter in the memory database.
fn set_counter(mem: &rusqlite::Connection, name: &str, v: i64) {
    mem.execute("INSERT INTO counters(name, value) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET value = excluded.value", rusqlite::params![name, v]).ok();
}

/// Decide light or full for a scheduler invocation: full once a day at the configured hour, otherwise light.
pub fn auto_full(cfg: &Config, mem: &rusqlite::Connection, now: i64, local_hour: u32) -> bool {
    let last = counter(mem, "last_full_tick").unwrap_or(0);
    if let Some(h) = cfg.schedule.full_every_hours.filter(|h| *h > 0) {
        return now - last >= h * 3600;
    }
    local_hour == cfg.schedule.full_hour && now - last >= cfg.schedule.min_hours_between_full * 3600
}

/// One scheduled tick: guards, then imports and the light or full learning steps; writes a tick log.
pub fn tick(ctx: &Ctx, opts: &TickOptions, probes: &Probes) -> Result<TickReport> {
    let cfg = ctx.cfg;
    let mut rep = TickReport { full: opts.full, steps: Vec::new(), log_path: None };
    let push = |rep: &mut TickReport, name: &str, status: &str, detail: String| rep.steps.push(Step { name: name.into(), status: status.into(), detail });

    // Guard 1: kill switch.
    if cfg.stop_file().exists() {
        push(&mut rep, "guard", "blocked", "STOP file present: the kill switch is engaged".into());
        return finish(cfg, rep);
    }
    // Guard 2: single instance (a stale lock from a crashed tick is replaced).
    let lock_path = cfg.root.join("tick.lock");
    if let Ok(meta) = std::fs::metadata(&lock_path) {
        let age = meta.modified().ok().and_then(|m| m.elapsed().ok()).map_or(0, |d| d.as_secs() as i64);
        // A lock whose process is gone (tick killed by a restart or crash) is stale at once; a live one only after the hours limit.
        let owner_alive = std::fs::read_to_string(&lock_path).ok().and_then(|t| t.trim().parse::<u32>().ok()).is_none_or(pid_alive);
        if owner_alive && age < cfg.schedule.lock_stale_hours * 3600 {
            push(&mut rep, "lock", "skipped", format!("another tick is running (lock is {} minutes old)", age / 60));
            return finish(cfg, rep);
        }
        std::fs::remove_file(&lock_path).ok();
        push(&mut rep, "lock", "ok", "replaced a stale lock".into());
    }
    std::fs::create_dir_all(&cfg.root)?;
    std::fs::write(&lock_path, format!("{}\n", std::process::id()))?;
    let _guard = LockGuard(lock_path);

    let s = &cfg.schedule;
    push(&mut rep, "guards", "ok", format!("load {} (cap {:.1}), free memory {} GB", probes.load1.map_or("?".into(), |l| format!("{l:.1}")), probes.ncpu as f64 * s.max_load_per_cpu, probes.free_gb.map_or("?".into(), |g| format!("{g:.1}"))));

    // Step 2: real inputs, read-only.
    let p = paper::import_best_effort(cfg, ctx.mem, ctx.data);
    push(&mut rep, "import_paper", if matches!(p.source.as_str(), "ok" | "absent" | "disabled") { "ok" } else { "failed" }, format!("source {} | state {} | {} trades, {} days{}", p.source, p.state, p.trades_total, p.days, if p.stale { " | STALE" } else { "" }));
    let b = btarchive::import_best_effort(cfg, ctx.mem, ctx.data);
    push(&mut rep, "import_backtests", if matches!(b.source.as_str(), "ok" | "absent" | "disabled") { "ok" } else { "failed" },
        format!("source {} | +{} runs, +{} trades | total {} runs ({} with trades, {} out-of-sample), {} trades{}", b.source, b.new_runs, b.new_trades, b.runs_total, b.runs_with_trades, b.oos_runs, b.trades_total, if b.note.is_empty() { String::new() } else { format!(" | {}", b.note) }));

    // Step 3: is there anything to learn from? The lab uses whatever exists now: complete windows plus one provisional (still
    // growing) window, three in all, and 200 trades. It does not wait for a full set of weeks.
    let wins = context::windows_info(ctx.data).unwrap_or_default();
    let complete = wins.iter().filter(|(_, c)| *c).count();
    let min_windows = cfg.evidence.min_windows;
    let trades: i64 = ctx.data.query_row("SELECT count(*) FROM trades", [], |r| r.get(0)).unwrap_or(0);
    let have_data = wins.len() >= min_windows && trades >= 200;
    if !have_data {
        let missing = min_windows.saturating_sub(wins.len()) as i64;
        let eta = if missing > 0 { format!(" At one new window per {} days it can start around {}.", db::block_secs(ctx.data) / 86_400, sysinfo::date_str(opts.now + missing * db::block_secs(ctx.data))) } else { String::new() };
        push(&mut rep, "sufficiency", "skipped", format!("too little to learn from yet: {} windows ({complete} complete; need {min_windows}), {trades} trades (need 200). Nothing is invented; the tick only reports.{eta}", wins.len()));
    } else {
        push(&mut rep, "sufficiency", "ok", format!("{} windows ({complete} complete, {} provisional), {trades} trades: learning from what exists now", wins.len(), wins.len() - complete));
    }
    if have_data {
        // Why runs win (>=60% of trades profitable) or lose: cheap, so every tick.
        match crate::winrate::analyse(ctx.mem, ctx.data, cfg.strategy.equities.first().copied().unwrap_or(5000.0)) {
            Ok((sm, _)) => push(&mut rep, "winrate", "ok", format!("{} runs; {} of {} reasons confirmed on later blocks", sm["runs"], sm["confirmed"], sm["reasons_tested"])),
            Err(e) => push(&mut rep, "winrate", "skipped", format!("{e:#}")),
        }
        // Promotion requests: a variant that passes the full validation is handed to the bot's promotion job (Claude Code applies it
        // in code while the market is closed)
        match crate::promote::run(ctx.mem, &cfg.root) {
            Ok(n) => push(&mut rep, "promote", "ok", if n.is_empty() { "nothing qualifies".into() } else { n.join("; ") }),
            Err(e) => push(&mut rep, "promote", "failed", format!("{e:#}")),
        }
        // research ideas from papers that are not about markets are marked not relevant (kept)
        if let Ok(n) = crate::scout::demote_off_topic(ctx.mem) {
            if n > 0 { push(&mut rep, "research", "ok", format!("{n} off-topic research idea(s) marked not relevant")); }
        }
        // (the brain's roles run in their own always-on process, `qwen-lab brain serve`, src/brain_roles.rs)
        // HARD RULE (owner 2026-10-11): no fake or manipulated data or results reach the strategy or the dashboard. Results come from code
        // only; the brain's stored texts lose every number the lab never showed.
        match crate::facts::scrub_stored(ctx) {
            Ok(n) => push(&mut rep, "facts", "ok", format!("{} stored texts scrubbed of unverified numbers", n.len())),
            Err(e) => push(&mut rep, "facts", "failed", format!("{e:#}")),
        }
    }

    // Step 10: housekeeping (always).
    match report::write(cfg, ctx.mem, ctx.data) { Ok(p) => push(&mut rep, "report", "ok", p.display().to_string()), Err(e) => push(&mut rep, "report", "failed", format!("{e:#}")) }
    if opts.full {
        let last_backup = counter(ctx.mem, "last_backup").unwrap_or(0);
        if opts.now - last_backup >= 20 * 3600 {
            let r = (|| -> Result<String> {
                std::fs::create_dir_all(cfg.backup_dir())?;
                let dest = cfg.backup_dir().join(format!("memory-{}.sqlite3", opts.now));
                ctx.mem.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
                let mut files: Vec<_> = std::fs::read_dir(cfg.backup_dir())?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
                files.sort();
                while files.len() > 14 { std::fs::remove_file(files.remove(0))?; }
                set_counter(ctx.mem, "last_backup", opts.now);
                Ok(dest.display().to_string())
            })();
            match r { Ok(p) => push(&mut rep, "backup", "ok", p), Err(e) => push(&mut rep, "backup", "failed", format!("{e:#}")) }
        } else { push(&mut rep, "backup", "skipped", "a backup was made in the last 20 hours".into()); }
        if cull::needs_cull(cfg) {
            match cull::cull(cfg, ctx.mem, ctx.data, false, false) {
                Ok(c) => push(&mut rep, "cull", if c.backpressure { "failed" } else { "ok" }, format!("{} -> {} bytes, {} runs removed{}", c.bytes_before, c.bytes_after, c.runs_removed, if c.backpressure { " | STILL OVER THE LIMIT" } else { "" })),
                Err(e) => push(&mut rep, "cull", "failed", format!("{e:#}")),
            }
        } else { push(&mut rep, "cull", "skipped", format!("data database at {:.1} MB, under its limit", db::size_bytes(&cfg.data_db()) as f64 / 1e6)); }
        set_counter(ctx.mem, "last_full_tick", opts.now);
    }
    set_counter(ctx.mem, "last_tick", opts.now);
    finish(cfg, rep)
}

/// Writes the tick log and deletes tick logs older than 30 days.
fn finish(cfg: &Config, mut rep: TickReport) -> Result<TickReport> {
    std::fs::create_dir_all(cfg.logs_dir())?;
    let path = cfg.logs_dir().join(format!("tick-{}.log", now_ts()));
    std::fs::write(&path, rep.text())?;
    // Keep 30 days of tick logs.
    if let Ok(rd) = std::fs::read_dir(cfg.logs_dir()) {
        for e in rd.filter_map(|e| e.ok()) {
            let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|m| m.elapsed().ok()).is_some_and(|d| d.as_secs() > 30 * 86_400);
            if old && e.file_name().to_string_lossy().starts_with("tick-") { std::fs::remove_file(e.path()).ok(); }
        }
    }
    rep.log_path = Some(path);
    Ok(rep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::SynthSpec;
    use crate::testutil::{lab, Lab};

    /// Test fixture: machine probes with the given load.
    fn probes(load: f64) -> Probes { Probes { load1: Some(load), ncpu: 10, free_gb: Some(8.0) } }

    /// Test fixture: tick options, full or light.
    fn opts(full: bool) -> TickOptions { TickOptions { full, now: 1_800_000_000 } }

    /// Test fixture: a lab with seven windows of synthetic data.
    fn data_lab() -> Lab {
        lab(&SynthSpec { windows: 7, days_per_window: 28, entries_per_day: 8, runs_per_window: 6, planted: Some(("vol_ratio".into(), 0.02)), seed: 5, ..Default::default() })
    }

    /// Test fixture: a real-mode lab with no data at all.
    fn empty_lab() -> Lab {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_root(dir.path(), "real");
        cfg.paper.archive_path = dir.path().join("none.sqlite3");
        cfg.backtest_archive.path = dir.path().join("none-bt.sqlite3");
        let mem = db::open(&cfg.memory_db(), true).unwrap();
        let data = db::open(&cfg.data_db(), false).unwrap();
        db::migrate_memory(&mem).unwrap();
        db::migrate_data(&data).unwrap();
        Lab { _dir: dir, cfg, mem, data }
    }

    /// Test helper: runs one tick on the lab.
    fn run_tick(l: &Lab, o: &TickOptions, p: &Probes) -> TickReport {
        let ctx = Ctx { cfg: &l.cfg, mem: &l.mem, data: &l.data };
        tick(&ctx, o, p).unwrap()
    }

    /// Test: with no real data the tick reports and does nothing harmful.
    #[test]
    fn with_no_real_data_the_tick_reports_and_does_nothing_harmful() {
        let l = empty_lab();
        let r = run_tick(&l, &opts(true), &probes(1.0));
        assert_eq!(r.get("sufficiency").unwrap().status, "skipped");
        assert!(r.get("sufficiency").unwrap().detail.contains("Nothing is invented"));
        for absent in ["winrate", "promote", "facts"] { assert!(r.get(absent).is_none(), "{absent} must not run without data"); }
        assert!(r.get("report").unwrap().status == "ok");
        assert!(r.log_path.as_ref().unwrap().exists());
        assert!(!l.cfg.root.join("tick.lock").exists(), "the lock must be released");
    }

    /// Test: kill switch and lock stop the tick and a stale lock is replaced.
    #[test]
    fn kill_switch_and_lock_stop_the_tick_and_a_stale_lock_is_replaced() {
        let l = empty_lab();
        std::fs::write(l.cfg.stop_file(), "").unwrap();
        let r = run_tick(&l, &opts(true), &probes(1.0));
        assert_eq!(r.steps.len(), 1);
        assert_eq!(r.get("guard").unwrap().status, "blocked");
        std::fs::remove_file(l.cfg.stop_file()).unwrap();
        std::fs::write(l.cfg.root.join("tick.lock"), format!("{}\n", std::process::id())).unwrap();
        let r = run_tick(&l, &opts(false), &probes(1.0));
        assert_eq!(r.get("lock").unwrap().status, "skipped");
        assert!(l.cfg.root.join("tick.lock").exists(), "a live lock belongs to the other tick and must not be removed");
        let mut l2 = empty_lab();
        l2.cfg.schedule.lock_stale_hours = 0;
        std::fs::write(l2.cfg.root.join("tick.lock"), "123").unwrap();
        let r = run_tick(&l2, &opts(false), &probes(1.0));
        assert_eq!(r.get("lock").unwrap().detail, "replaced a stale lock");
        assert!(r.get("report").is_some());
        // A lock left by a dead process is replaced at once, however young.
        let l3 = empty_lab();
        std::fs::write(l3.cfg.root.join("tick.lock"), "2147483646").unwrap();
        let r = run_tick(&l3, &opts(false), &probes(1.0));
        assert_eq!(r.get("lock").unwrap().detail, "replaced a stale lock");
    }

    /// Test: a full tick runs every step in order and a second tick makes no second backup.
    #[test]
    fn a_full_tick_runs_every_step_and_a_second_tick_makes_no_second_backup() {
        let l = data_lab();
        let r = run_tick(&l, &opts(true), &probes(1.0));
        for step in ["import_paper", "import_backtests", "sufficiency", "winrate", "promote", "facts", "report", "backup", "cull"] {
            assert!(r.get(step).is_some(), "missing step {step}: {}", r.text());
        }
        assert!(r.steps.iter().all(|s| s.status != "failed"), "{}", r.text());
        assert!(std::fs::read_dir(l.cfg.backup_dir()).unwrap().count() == 1);
        let r2 = run_tick(&l, &TickOptions { now: 1_800_000_060, ..opts(true) }, &probes(1.0));
        assert!(r2.steps.iter().all(|s| s.status != "failed"), "{}", r2.text());
        assert_eq!(r2.get("backup").unwrap().status, "skipped");
    }

    /// Test: a light tick makes no backup and does not cull.
    #[test]
    fn a_light_tick_makes_no_backup() {
        let l = data_lab();
        let r = run_tick(&l, &opts(false), &probes(1.0));
        for absent in ["backup", "cull"] { assert!(r.get(absent).is_none(), "{absent}"); }
        assert!(r.get("winrate").is_some() && r.get("report").is_some());
    }

    /// Test: full runs once a day at the configured hour.
    #[test]
    fn full_runs_once_a_day_at_the_configured_hour() {
        let l = empty_lab();
        let now = 1_800_000_000;
        assert!(auto_full(&l.cfg, &l.mem, now, l.cfg.schedule.full_hour));
        assert!(!auto_full(&l.cfg, &l.mem, now, l.cfg.schedule.full_hour + 1));
        run_tick(&l, &TickOptions { full: true, now }, &probes(1.0));
        assert!(!auto_full(&l.cfg, &l.mem, now + 3600, l.cfg.schedule.full_hour), "not twice within 20 hours");
        assert!(auto_full(&l.cfg, &l.mem, now + 21 * 3600, l.cfg.schedule.full_hour));
        // with `full_every_hours` the full tick runs at any hour once that many hours have passed
        let mut cfg = l.cfg.clone();
        cfg.schedule.full_every_hours = Some(3);
        let last = counter(&l.mem, "last_full_tick").unwrap_or(0);
        assert!(!auto_full(&cfg, &l.mem, last + 2 * 3600, cfg.schedule.full_hour + 5));
        assert!(auto_full(&cfg, &l.mem, last + 3 * 3600, cfg.schedule.full_hour + 5));
    }
}

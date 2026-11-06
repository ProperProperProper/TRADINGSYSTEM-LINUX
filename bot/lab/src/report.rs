//! The lab's daily report: data coverage, the paper record, the backtest archive by population, operations and alerts. Numbers first.

use crate::config::Config;
use crate::db;
use anyhow::Result;
use rusqlite::Connection;
use std::fmt::Write;

/// First column of the first row of `sql` (panics on error; report building only).
fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T { c.query_row(sql, [], |r| r.get(0)).unwrap() }

/// Builds the lab's markdown report: data coverage, paper record, backtest archive by population, operations and alerts.
pub fn build(cfg: &Config, mem: &Connection, data: &Connection) -> Result<String> {
    let mut s = String::new();
    let today = crate::util::now_ts() / 86_400;
    writeln!(s, "# Lab report (day {today}, mode: {})\n", cfg.mode)?;
    writeln!(s, "Lab version {}\n", crate::util::CODE_VERSION)?;

    writeln!(s, "## 1. Data coverage")?;
    let runs: i64 = one(data, "SELECT count(*) FROM runs");
    let trades: i64 = one(data, "SELECT count(*) FROM trades");
    let windows: i64 = one(data, "SELECT count(DISTINCT window_label) FROM runs");
    writeln!(s, "- {runs} runs, {trades} trades, {windows} windows, {} ingest batches", one::<i64>(data, "SELECT count(*) FROM ingest_batches"))?;
    writeln!(s, "- quarantined files: {}", one::<i64>(mem, "SELECT count(*) FROM ingest_log WHERE status = 'quarantined'"))?;
    writeln!(s, "- degraded runs (missing feature columns): {}\n", one::<i64>(data, "SELECT count(*) FROM runs WHERE degraded = 1"))?;

    writeln!(s, "## 1b. Paper record (forward data from the bot's paper account)")?;
    let rep = crate::paper::import_best_effort(cfg, mem, data);
    writeln!(s, "- source: {}{} | state: {} | {} run segment(s), {} closed trades, {} days", rep.source, if rep.note.is_empty() { String::new() } else { format!(" ({})", rep.note) }, rep.state, rep.runs, rep.trades_total, rep.days)?;
    if rep.stale { writeln!(s, "- paper record is stale (newest row older than {} s): the bot may be stopped", cfg.paper.stale_secs)?; }
    if rep.epoch_reset { writeln!(s, "- the archive was recreated: a new import epoch ({}) was started, older rows are kept", rep.epoch)?; }
    for line in crate::paper::prompt_block(cfg, data, &rep)?.lines().skip(1) { writeln!(s, "- {line}")?; }
    let d = crate::paper::search_decay(data, 8, 500)?;
    if d.verdict == "insufficient" { writeln!(s, "- search-score vs forward-return check: insufficient ({} search/return pairs, 8 needed)", d.n_pairs)?; }
    else { writeln!(s, "- search-score vs forward-return: Spearman {:+.3} over {} pairs, placebo p {:.3} (small sample; context only)", d.spearman, d.n_pairs, d.placebo_p)?; }
    writeln!(s)?;

    writeln!(s, "## 1c. Backtest archive by population (descriptive only; selection bias applies to top-ranked and chosen runs)")?;
    {
        let mut st = data.prepare(
            "SELECT r.role, r.selection, count(DISTINCT r.run_id), count(DISTINCT t.run_id), count(t.run_id), avg(t.ret_margin), avg(t.ret_margin > 0), avg(r.return_pct) \
             FROM runs r LEFT JOIN trades t ON t.run_id = r.run_id WHERE r.run_key IS NOT NULL GROUP BY r.role, r.selection ORDER BY r.role, r.selection")?;
        let rows: Vec<(String, String, i64, i64, i64, Option<f64>, Option<f64>, Option<f64>)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?.collect::<Result<_, _>>()?;
        if rows.is_empty() { writeln!(s, "- no archive runs imported yet")?; }
        for (role, sel, runs, with, trades, mean, win, ret) in rows {
            writeln!(s, "- {role} [{sel}]: {runs} runs ({with} with trades), {trades} trades, mean return on margin per trade {}, win rate {}, mean run return {}%",
                mean.map_or("n/a".into(), |m| format!("{m:+.4}")), win.map_or("n/a".into(), |w| format!("{:.0}%", w * 100.0)), ret.map_or("n/a".into(), |r| format!("{r:+.1}")))?;
        }
    }
    writeln!(s)?;

    writeln!(s, "## 2. Operations")?;
    let data_size = db::size_bytes(&cfg.data_db());
    let mem_size = db::size_bytes(&cfg.memory_db());
    writeln!(s, "- data.sqlite3: {:.1} MB of {:.0} MB limit; memory.sqlite3: {:.2} MB", data_size as f64 / 1e6, cfg.limits.data_db_limit_bytes as f64 / 1e6, mem_size as f64 / 1e6)?;
    let mut alerts: Vec<String> = Vec::new();
    if data_size >= cfg.limits.data_db_limit_bytes { alerts.push("data database is at its size limit: run `cull`".into()); }
    if data_size * 10 >= cfg.limits.data_db_limit_bytes * 8 { alerts.push("data database above 80% of its limit".into()); }
    if mem_size >= cfg.limits.memory_db_alert_bytes { alerts.push("memory database above its alert size".into()); }
    if cfg.stop_file().exists() { alerts.push("kill switch (STOP file) is engaged".into()); }
    if let Ok((ts, dry, after)) = mem.query_row("SELECT ts, dry_run, bytes_after FROM cull_log ORDER BY id DESC LIMIT 1", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))) {
        writeln!(s, "- last cull: day {} ({}), size after {:.1} MB", ts / 86_400, if dry == 1 { "dry run" } else { "applied" }, after as f64 / 1e6)?;
    }
    writeln!(s, "\n## 3. Alerts")?;
    if alerts.is_empty() { writeln!(s, "- none")?; }
    for a in alerts { writeln!(s, "- {a}")?; }
    Ok(s)
}

/// Writes today's report to the reports folder.
pub fn write(cfg: &Config, mem: &Connection, data: &Connection) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(cfg.reports_dir())?;
    let path = cfg.reports_dir().join(format!("day-{}.md", crate::util::now_ts() / 86_400));
    std::fs::write(&path, build(cfg, mem, data)?)?;
    Ok(path)
}

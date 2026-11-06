//! The paper-trading archive as a forward record (spec 4.3). The bot appends everything its PAPER account does to a
//! separate SQLite file; this module reads it strictly read-only, copies it incrementally into `data.sqlite3`, and
//! tells the rest of the lab how much to trust it. The archive can be empty or thin, so every use goes through a
//! sufficiency ladder (absent / empty / thin / usable / rich).
//!
//! Amounts: both the absolute paper amounts (USDT, exactly as the bot recorded them) and the same figures as percent of
//! each run's starting equity are stored, at the user's instruction (2026-10-05). Credentials are never stored, and
//! text fields still pass the privacy scrub.

use crate::config::Config;
use crate::contract;
use crate::stats;
use crate::util::{now_ts, sha256_str};
use anyhow::Result;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PaperReport {
    /// absent | schema_mismatch | locked | ok  (state of the SOURCE file)
    pub source: String,
    /// empty | thin | usable | rich  (how much forward data the lab holds)
    pub state: String,
    pub epoch: i64,
    pub epoch_reset: bool,
    pub new_trades: usize,
    pub new_equity: usize,
    pub new_searches: usize,
    pub runs: i64,
    pub trades_total: i64,
    pub days: i64,
    pub last_ts_ms: Option<i64>,
    pub stale: bool,
    pub note: String,
}

/// Reads a named counter from the memory database (None if never set).
pub(crate) fn counter(mem: &Connection, name: &str) -> Result<Option<i64>> {
    Ok(mem.query_row("SELECT value FROM counters WHERE name = ?1", [name], |r| r.get(0)).optional()?)
}

/// Sets a named counter in the memory database.
pub(crate) fn set_counter(mem: &Connection, name: &str, v: i64) -> Result<()> {
    mem.execute("INSERT INTO counters(name, value) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET value = excluded.value", params![name, v])?;
    Ok(())
}

/// Column names of `table`.
fn columns(c: &Connection, table: &str) -> Result<Vec<String>> {
    let mut s = c.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?;
    let v = s.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

/// What the bot's archive must contain for the import to proceed (extra columns are ignored).
const REQUIRED: &[(&str, &[&str])] = &[
    ("runs", &["run_id", "started_ms", "note"]),
    ("params", &["id", "json", "first_used_ms"]),
    ("equity", &["run_id", "ts_ms", "equity", "start_equity", "realized", "open_pnl", "max_drawdown_pct", "open_positions", "closed_trades", "wins", "params_id", "mode"]),
    ("trades", &["id", "run_id", "symbol", "side", "entry_ts", "exit_ts", "entry", "exit", "qty", "leverage", "fees", "funding", "pnl", "ret_margin", "reason", "params_id"]),
    ("searches", &["id", "run_id", "ts_ms", "json"]),
];

/// Checks the paper archive has every required table and column; returns what is missing.
fn check_schema(c: &Connection) -> std::result::Result<(), String> {
    for (table, cols) in REQUIRED {
        let have = columns(c, table).map_err(|e| e.to_string())?;
        if have.is_empty() { return Err(format!("table `{table}` is missing")); }
        for col in *cols {
            if !have.iter().any(|h| h == col) { return Err(format!("`{table}.{col}` is missing")); }
        }
    }
    Ok(())
}

/// Cleans a text field from the archive (privacy scrub, no control characters, at most 200 characters).
pub(crate) fn clean(text: &str) -> Result<String> {
    contract::scrub(&format!("\"{}\"", text.to_ascii_lowercase()).replace("\"\"", "\""))?;
    let t: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
    Ok(t)
}

/// Refuses JSON that contains a forbidden field.
pub(crate) fn scrub_json(json: &str) -> Result<()> {
    contract::scrub(json)
}

/// `x` as a percentage of `base` (None without a positive base).
pub(crate) fn pct(x: f64, base: Option<f64>) -> Option<f64> { base.filter(|b| *b > 0.0).map(|b| x / b * 100.0) }

/// Imports the bot's paper-trading archive (read-only); routes to the moomoo importer for stock labs.
pub fn import(cfg: &Config, mem: &Connection, data: &Connection, now_ms: i64) -> Result<PaperReport> {
    let mut rep = PaperReport { epoch: counter(mem, "paper_epoch")?.unwrap_or(1), ..Default::default() };
    // Real forward data never enters a synthetic (test-machinery) lab.
    if cfg.mode == "synthetic" {
        rep.source = "disabled".into();
        rep.note = "synthetic labs never read the paper archive".into();
        return finish(cfg, data, rep, now_ms);
    }
    let path = &cfg.paper.archive_path;
    if cfg.paper.source == "moomoo" {
        return crate::paper_moomoo::import(cfg, mem, data, now_ms, rep);
    }
    if !path.exists() {
        rep.source = "absent".into();
        return finish(cfg, data, rep, now_ms);
    }
    // Read-only, no creation, no journal writes: the bot's file is never modified by the lab.
    let src = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) {
        Ok(c) => c,
        Err(e) => { rep.source = "locked".into(); rep.note = format!("cannot open archive: {e}"); return finish(cfg, data, rep, now_ms); }
    };
    src.busy_timeout(std::time::Duration::from_secs(5))?;
    // One read transaction = one consistent snapshot, without copying a possibly huge file and without blocking the writer.
    if let Err(e) = src.execute_batch("BEGIN") { rep.source = "locked".into(); rep.note = format!("cannot read archive: {e}"); return finish(cfg, data, rep, now_ms); }
    if let Err(why) = check_schema(&src) { rep.source = "schema_mismatch".into(); rep.note = why; return finish(cfg, data, rep, now_ms); }
    rep.source = "ok".into();

    // The archive is append-only, so ids only grow. Smaller ids or a different first run mean it was recreated.
    let first_started: Option<i64> = src.query_row("SELECT min(started_ms) FROM runs", [], |r| r.get(0))?;
    let max_trade: i64 = src.query_row("SELECT coalesce(max(id),0) FROM trades", [], |r| r.get(0))?;
    let max_search: i64 = src.query_row("SELECT coalesce(max(id),0) FROM searches", [], |r| r.get(0))?;
    let max_eq: i64 = src.query_row("SELECT coalesce(max(rowid),0) FROM equity", [], |r| r.get(0))?;
    let (wm_t, wm_s, wm_e) = (counter(mem, "paper_wm_trades")?.unwrap_or(0), counter(mem, "paper_wm_searches")?.unwrap_or(0), counter(mem, "paper_wm_equity")?.unwrap_or(0));
    let stored_first = counter(mem, "paper_first_started")?;
    let reset = (stored_first.is_some() && first_started.is_some() && stored_first != first_started) || max_trade < wm_t || max_search < wm_s || max_eq < wm_e;
    let (epoch, wm_t, wm_s, wm_e) = if reset { rep.epoch_reset = true; (rep.epoch + 1, 0, 0, 0) } else { (rep.epoch, wm_t, wm_s, wm_e) };
    rep.epoch = epoch;

    // Starting equity per run turns amounts into percentages (the amounts themselves are stored too).
    let mut start_eq: HashMap<i64, f64> = HashMap::new();
    {
        let mut s = src.prepare("SELECT run_id, start_equity FROM equity e WHERE ts_ms = (SELECT min(ts_ms) FROM equity WHERE run_id = e.run_id)")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))? { let (id, v) = r?; start_eq.insert(id, v); }
    }

    let tx = data.unchecked_transaction()?;
    {
        let mut s = src.prepare("SELECT run_id, started_ms, note FROM runs")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))? {
            let (id, st, note) = r?;
            tx.execute("INSERT OR IGNORE INTO paper_runs(epoch, run_id, started_ms, note) VALUES (?1,?2,?3,?4)", params![epoch, id, st, clean(&note)?])?;
        }
        let mut s = src.prepare("SELECT id, json, first_used_ms FROM params")?;
        for r in s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?)))? {
            let (id, json, first) = r?;
            scrub_json(&json)?;
            tx.execute("INSERT OR IGNORE INTO paper_params(epoch, id, params_hash, json, first_used_ms) VALUES (?1,?2,?3,?4,?5)", params![epoch, id, sha256_str(&json), json, first])?;
        }
        // Columns the bot added later (decision time and entry-state features) are optional.
        let has = |col: &str| src.query_row("SELECT count(*) FROM pragma_table_info('trades') WHERE name = ?1", [col], |r| r.get::<_, i64>(0)).map(|n| n > 0).unwrap_or(false);
        let (dec, feat) = (if has("decided_ts") { "decided_ts" } else { "NULL AS decided_ts" }, if has("features") { "features" } else { "NULL AS features" });
        let mut s = src.prepare(&format!("SELECT id, run_id, symbol, side, entry_ts, exit_ts, entry, exit, leverage, fees, funding, pnl, ret_margin, reason, params_id, qty, {dec}, {feat} FROM trades WHERE id > ?1 ORDER BY id"))?;
        let mut rows = s.query([wm_t])?;
        while let Some(r) = rows.next()? {
            let run: i64 = r.get(1)?;
            let base = start_eq.get(&run).copied();
            let (fees, funding, pnl): (f64, f64, f64) = (r.get(9)?, r.get(10)?, r.get(11)?);
            let features_text: Option<String> = match r.get::<_, Option<String>>(17)? {
                Some(t) => { scrub_json(&t)?; Some(t.chars().take(20_000).collect()) }
                None => None,
            };
            tx.execute(
                "INSERT OR IGNORE INTO paper_trades(epoch, id, run_id, symbol, side, entry_ts, exit_ts, entry, exit, qty, leverage, pnl, fees, funding, pnl_pct, fees_pct, funding_pct, ret_margin, reason, params_id, decided_ts, features_json) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
                params![epoch, r.get::<_, i64>(0)?, run, clean(&r.get::<_, String>(2)?)?, clean(&r.get::<_, String>(3)?)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?,
                    r.get::<_, f64>(6)?, r.get::<_, f64>(7)?, r.get::<_, f64>(15)?, r.get::<_, f64>(8)?, pnl, fees, funding, pct(pnl, base), pct(fees, base), pct(funding, base),
                    r.get::<_, f64>(12)?, clean(&r.get::<_, String>(13)?)?, r.get::<_, Option<i64>>(14)?,
                    r.get::<_, Option<i64>>(16)?.map(|d| d.div_euclid(1000)), features_text],
            )?;
            rep.new_trades += 1;
        }
        let mut s = src.prepare("SELECT rowid, run_id, ts_ms, equity, start_equity, realized, open_pnl, max_drawdown_pct, open_positions, closed_trades, wins, params_id, mode FROM equity WHERE rowid > ?1 ORDER BY rowid")?;
        let mut rows = s.query([wm_e])?;
        while let Some(r) = rows.next()? {
            let base: f64 = r.get(4)?;
            let b = Some(base);
            tx.execute(
                "INSERT OR IGNORE INTO paper_equity(epoch, run_id, ts_ms, equity, start_equity, realized, open_pnl, equity_index, realized_pct, open_pnl_pct, max_drawdown_pct, open_positions, closed_trades, wins, params_id, mode) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![epoch, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, f64>(3)?, base, r.get::<_, f64>(5)?, r.get::<_, f64>(6)?,
                    pct(r.get::<_, f64>(3)?, b), pct(r.get::<_, f64>(5)?, b), pct(r.get::<_, f64>(6)?, b),
                    r.get::<_, f64>(7)?, r.get::<_, i64>(8)?, r.get::<_, i64>(9)?, r.get::<_, i64>(10)?, r.get::<_, Option<i64>>(11)?, clean(&r.get::<_, String>(12)?)?],
            )?;
            rep.new_equity += 1;
        }
        let mut s = src.prepare("SELECT id, run_id, ts_ms, json FROM searches WHERE id > ?1 ORDER BY id")?;
        let mut rows = s.query([wm_s])?;
        while let Some(r) = rows.next()? {
            let raw: String = r.get(3)?;
            let Ok(v) = serde_json::from_str::<Value>(&raw) else { continue };
            let chosen = v.get("chosen").cloned().unwrap_or(Value::Null);
            let settings = serde_json::to_string(&chosen)?;
            scrub_json(&settings)?;
            let m = v.get("metrics");
            let f = |k: &str| m.and_then(|m| m.get(k)).and_then(|x| x.as_f64());
            let ret = match (f("end_equity"), f("start_equity")) { (Some(e), Some(s)) if s > 0.0 => Some((e / s - 1.0) * 100.0), _ => None };
            tx.execute(
                "INSERT OR IGNORE INTO paper_searches(epoch, id, run_id, ts_ms, choice, signal, hold, top, lookback, seed, evaluated, raw_best_score, robust_score, start_equity, end_equity, return_pct, max_drawdown_pct, trades, wins, liquidations, settings_json) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
                params![epoch, r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                    v.get("choice").and_then(|x| x.as_str()).map(|s| clean(s).unwrap_or_default()),
                    chosen.get("signal").and_then(|x| x.as_str()).map(|s| clean(s).unwrap_or_default()),
                    chosen.get("hold").and_then(|x| x.as_i64()), chosen.get("top").and_then(|x| x.as_i64()), chosen.get("lookback").and_then(|x| x.as_i64()),
                    v.get("seed").map(|x| x.to_string()), v.get("evaluated").and_then(|x| x.as_i64()), v.get("raw_best_score").and_then(|x| x.as_f64()),
                    v.get("robust_score").and_then(|x| x.as_f64()), f("start_equity"), f("end_equity"), ret, f("max_drawdown_pct"), f("trades").map(|x| x as i64), f("wins").map(|x| x as i64),
                    f("liquidations").map(|x| x as i64), settings],
            )?;
            rep.new_searches += 1;
        }
    }
    tx.commit()?;
    src.execute_batch("COMMIT")?;
    set_counter(mem, "paper_epoch", epoch)?;
    set_counter(mem, "paper_wm_trades", max_trade)?;
    set_counter(mem, "paper_wm_searches", max_search)?;
    set_counter(mem, "paper_wm_equity", max_eq)?;
    if let Some(f) = first_started { set_counter(mem, "paper_first_started", f)?; }
    finish(cfg, data, rep, now_ms)
}

/// Current counts and sufficiency state from what the lab already holds (read-only; no import).
pub fn current(cfg: &Config, data: &Connection, now_ms: i64) -> Result<PaperReport> {
    finish(cfg, data, PaperReport { source: "cached".into(), ..Default::default() }, now_ms)
}

/// Fill in counts and the sufficiency state from what the lab holds (so a missing source still reports honestly).
pub(crate) fn finish(cfg: &Config, data: &Connection, mut rep: PaperReport, now_ms: i64) -> Result<PaperReport> {
    rep.runs = data.query_row("SELECT count(*) FROM (SELECT DISTINCT epoch, run_id FROM paper_runs)", [], |r| r.get(0))?;
    rep.trades_total = data.query_row("SELECT count(*) FROM paper_trades", [], |r| r.get(0))?;
    rep.days = data.query_row("SELECT count(DISTINCT ts_ms / 86400000) FROM paper_equity", [], |r| r.get(0))?;
    rep.last_ts_ms = data.query_row("SELECT max(ts_ms) FROM paper_equity", [], |r| r.get::<_, Option<i64>>(0))?;
    rep.stale = rep.last_ts_ms.is_some_and(|t| now_ms - t > cfg.paper.stale_secs * 1000);
    let p = &cfg.paper;
    rep.state = if rep.trades_total == 0 { "empty" }
        else if rep.trades_total < p.usable_trades || rep.days < p.usable_days { "thin" }
        else if rep.trades_total >= p.rich_trades && rep.days >= p.rich_days { "rich" }
        else { "usable" }.into();
    Ok(rep)
}

/// Mean of values and a day-clustered bootstrap 95% interval (NaN interval with fewer than 5 days).
fn mean_ci(values: &[(i64, f64)], n_boot: usize, seed: u64) -> (f64, f64, f64) {
    let mut by_day: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for (d, v) in values { by_day.entry(*d).or_default().push(*v); }
    let days: Vec<Vec<f64>> = by_day.into_values().collect();
    let all: Vec<f64> = values.iter().map(|v| v.1).collect();
    let mean = stats::mean(&all);
    if days.len() < 5 { return (mean, f64::NAN, f64::NAN); }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut means = Vec::with_capacity(n_boot);
    for _ in 0..n_boot {
        let (mut s, mut n) = (0.0, 0usize);
        for _ in 0..days.len() { let d = &days[rng.gen_range(0..days.len())]; s += d.iter().sum::<f64>(); n += d.len(); }
        means.push(s / n as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (mean, means[(n_boot as f64 * 0.025) as usize], means[(n_boot as f64 * 0.975) as usize])
}

/// The PAPER RECORD block placed in model prompts: built from aggregates only, never raw rows or account amounts.
pub fn prompt_block(cfg: &Config, data: &Connection, rep: &PaperReport) -> Result<String> {
    let mut s = format!("PAPER RECORD (forward results from the bot's paper account; state: {}): {} closed trades over {} days in {} run segment(s).\n", rep.state, rep.trades_total, rep.days, rep.runs);
    // Latest segment's account figures: absolute paper amounts alongside their percentages (user instruction 2026-10-05).
    if let Some((eq, start, realized, open, idx)) = data.query_row(
        "SELECT equity, start_equity, realized, open_pnl, equity_index FROM paper_equity ORDER BY epoch DESC, ts_ms DESC LIMIT 1", [],
        |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, Option<f64>>(4)?))).optional()? {
        s.push_str(&format!("Latest paper account: equity {eq:.2} {cur} from a start of {start:.2} {cur} ({:+.2}%), realised {realized:+.2} {cur}, open P&L {open:+.2} {cur}.\n",
            idx.map_or(0.0, |i| i - 100.0), cur = cfg.paper.currency));
    }
    match rep.state.as_str() {
        "empty" => s.push_str("No closed paper trades exist yet. Do not draw any conclusion from the paper account.\n"),
        "thin" => s.push_str(&format!("The record is thin ({} trades over {} days; usable needs {}+ trades and {}+ days). Draw NO conclusions from it.\n", rep.trades_total, rep.days, cfg.paper.usable_trades, cfg.paper.usable_days)),
        _ => {
            let mut st = data.prepare("SELECT exit_ts / 86400000, ret_margin FROM paper_trades")?;
            let vals: Vec<(i64, f64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
            let (m, lo, hi) = mean_ci(&vals, 1000, 7);
            let wins = vals.iter().filter(|v| v.1 > 0.0).count();
            let (tot_pnl, tot_fees): (f64, f64) = data.query_row("SELECT coalesce(sum(pnl),0), coalesce(sum(fees),0) FROM paper_trades", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
            s.push_str(&format!("Closed-trade totals: P&L {tot_pnl:+.2} {cur}, fees {tot_fees:.2} {cur}.\n", cur = cfg.paper.currency));
            s.push_str(&format!("Win rate {:.0}% ({} of {}); mean return per trade {:+.4} (95% interval {:+.4} to {:+.4}, days resampled). Forward and small sample: context, not proof.\n",
                100.0 * wins as f64 / vals.len() as f64, wins, vals.len(), m, lo, hi));
            let dd: Option<f64> = data.query_row("SELECT max(max_drawdown_pct) FROM paper_equity", [], |r| r.get(0))?;
            s.push_str(&format!("Worst paper drawdown seen: {:.1}%.\n", dd.unwrap_or(0.0)));
            let mut st = data.prepare("SELECT reason, count(*) FROM paper_trades GROUP BY reason ORDER BY 2 DESC LIMIT 5")?;
            let reasons: Vec<String> = st.query_map([], |r| Ok(format!("{} {}", r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?.collect::<Result<_, _>>()?;
            s.push_str(&format!("Exits: {}.\n", reasons.join(", ")));
        }
    }
    if let Some((signal, hold, top, robust, choice)) = data.query_row(
        "SELECT signal, hold, top, robust_score, choice FROM paper_searches ORDER BY epoch DESC, id DESC LIMIT 1", [],
        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?, r.get::<_, Option<f64>>(3)?, r.get::<_, Option<String>>(4)?))).optional()? {
        s.push_str(&format!("Latest search chose signal {} (hold {} bars, top {}), robust in-sample score {}, outcome {}.\n",
            signal.unwrap_or_default(), hold.unwrap_or(0), top.unwrap_or(0), robust.map_or("n/a".into(), |v| format!("{v:.1}")), choice.unwrap_or_default()));
    }
    if rep.stale { s.push_str("The paper record is stale (the bot may be stopped).\n"); }
    Ok(s)
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchDecay {
    pub verdict: String, // insufficient | measured
    pub n_pairs: usize,
    pub spearman: f64,
    pub placebo_p: f64,
}

/// Does the in-sample score of the settings a search chose predict the forward paper return until the next search?
/// Needs at least `min_pairs` (search, forward-return) pairs; otherwise it says so and stops.
pub fn search_decay(data: &Connection, min_pairs: usize, n_perm: usize) -> Result<SearchDecay> {
    let mut s = data.prepare("SELECT epoch, run_id, ts_ms, robust_score FROM paper_searches WHERE robust_score IS NOT NULL ORDER BY epoch, run_id, ts_ms")?;
    let searches: Vec<(i64, i64, i64, f64)> = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
    let mut score = Vec::new();
    let mut fwd = Vec::new();
    for (i, (epoch, run, ts, sc)) in searches.iter().enumerate() {
        let next = searches.get(i + 1).filter(|n| n.0 == *epoch && n.1 == *run).map(|n| n.2).unwrap_or(i64::MAX);
        let eq = |bound_lo: i64, bound_hi: i64, asc: bool| -> Result<Option<f64>> {
            let sql = format!("SELECT equity_index FROM paper_equity WHERE epoch = ?1 AND run_id = ?2 AND ts_ms >= ?3 AND ts_ms <= ?4 AND equity_index IS NOT NULL ORDER BY ts_ms {} LIMIT 1", if asc { "ASC" } else { "DESC" });
            Ok(data.query_row(&sql, params![epoch, run, bound_lo, bound_hi], |r| r.get(0)).optional()?)
        };
        if let (Some(a), Some(b)) = (eq(*ts, next, true)?, eq(*ts, next, false)?) {
            if a > 0.0 { score.push(*sc); fwd.push((b / a - 1.0) * 100.0); }
        }
    }
    if score.len() < min_pairs {
        return Ok(SearchDecay { verdict: "insufficient".into(), n_pairs: score.len(), spearman: f64::NAN, placebo_p: f64::NAN });
    }
    let rho = stats::spearman(&score, &fwd);
    let mut rng = ChaCha8Rng::seed_from_u64(11);
    let mut shuffled = fwd.clone();
    let mut extreme = 0usize;
    for _ in 0..n_perm {
        use rand::seq::SliceRandom;
        shuffled.shuffle(&mut rng);
        if stats::spearman(&score, &shuffled).abs() >= rho.abs() - 1e-12 { extreme += 1; }
    }
    Ok(SearchDecay { verdict: "measured".into(), n_pairs: score.len(), spearman: rho, placebo_p: (1.0 + extreme as f64) / (1.0 + n_perm as f64) })
}

/// Convenience for callers that must never fail a research cycle because of the archive.
pub fn import_best_effort(cfg: &Config, mem: &Connection, data: &Connection) -> PaperReport {
    import(cfg, mem, data, now_ts() * 1000).unwrap_or_else(|e| PaperReport { source: "error".into(), state: "empty".into(), note: format!("{e:#}"), ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    const DDL: &str = "
        CREATE TABLE runs(run_id INTEGER PRIMARY KEY, started_ms INTEGER NOT NULL, note TEXT NOT NULL);
        CREATE TABLE params(id INTEGER PRIMARY KEY, json TEXT NOT NULL UNIQUE, first_used_ms INTEGER NOT NULL);
        CREATE TABLE equity(run_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL, equity REAL NOT NULL, start_equity REAL NOT NULL, realized REAL NOT NULL,
          open_pnl REAL NOT NULL, max_drawdown_pct REAL NOT NULL, open_positions INTEGER NOT NULL, closed_trades INTEGER NOT NULL, wins INTEGER NOT NULL,
          params_id INTEGER, mode TEXT NOT NULL, PRIMARY KEY(run_id, ts_ms));
        CREATE TABLE trades(id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL, symbol TEXT NOT NULL, side TEXT NOT NULL, entry_ts INTEGER NOT NULL,
          exit_ts INTEGER NOT NULL, entry REAL NOT NULL, exit REAL NOT NULL, qty REAL NOT NULL, leverage REAL NOT NULL, fees REAL NOT NULL, funding REAL NOT NULL,
          pnl REAL NOT NULL, ret_margin REAL NOT NULL, reason TEXT NOT NULL, params_id INTEGER, UNIQUE(run_id, symbol, entry_ts, exit_ts));
        CREATE TABLE positions(run_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL, json TEXT NOT NULL, PRIMARY KEY(run_id, ts_ms));
        CREATE TABLE searches(id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL, ts_ms INTEGER NOT NULL, json TEXT NOT NULL);";

    const DAY: i64 = 86_400_000;
    const WALLET: f64 = 123.456; // a distinctive "account amount" that must never reach the lab

    struct Fixture { _dir: tempfile::TempDir, cfg: Config, mem: Connection, data: Connection, archive: std::path::PathBuf }

    /// Test fixture: a real-mode lab with a paper-archive path.
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_root(dir.path(), "real");
        cfg.paper.archive_path = dir.path().join("archive").join("paper_archive.sqlite3");
        cfg.paper.source = "bybit".into(); // these fixtures use the legacy archive layout (synthetic test data only)
        cfg.paper.currency = "USDT".into();
        let mem = db::open(&cfg.memory_db(), true).unwrap();
        let data = db::open(&cfg.data_db(), false).unwrap();
        db::migrate_memory(&mem).unwrap();
        db::migrate_data(&data).unwrap();
        let archive = cfg.paper.archive_path.clone();
        Fixture { _dir: dir, cfg, mem, data, archive }
    }

    /// Test fixture: creates the crypto bot's paper archive schema at `path`.
    fn create_archive(path: &std::path::Path) -> Connection {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let c = Connection::open(path).unwrap();
        c.pragma_update(None, "journal_mode", "WAL").unwrap();
        c.execute_batch(DDL).unwrap();
        c
    }

    /// One run of `days` days with one equity row per day and `n_trades` trades, plus a search per day.
    fn fill(c: &Connection, run: i64, t0: i64, days: i64, n_trades: i64, first_trade_id: i64) {
        c.execute("INSERT OR IGNORE INTO runs VALUES (?1, ?2, 'service start')", params![run, t0]).unwrap();
        c.execute("INSERT OR IGNORE INTO params VALUES (1, '{\"signal\":\"CalmDip\",\"hold\":99,\"top\":4}', ?1)", [t0]).unwrap();
        for d in 0..days {
            let ts = t0 + d * DAY;
            c.execute("INSERT OR IGNORE INTO equity VALUES (?1,?2,?3,?4,?5,?6,?7,0,0,0,1,'TRADING')", params![run, ts, WALLET * (1.0 + 0.001 * d as f64), WALLET, WALLET * 0.001 * d as f64, 0.0, 1.5]).unwrap();
            c.execute("INSERT INTO searches(run_id, ts_ms, json) VALUES (?1,?2,?3)", params![run, ts, format!(
                "{{\"choice\":\"NewBest\",\"chosen\":{{\"signal\":\"CalmDip\",\"hold\":99,\"top\":4,\"lookback\":0,\"long_only\":true}},\"evaluated\":100000,\"raw_best_score\":{},\"robust_score\":{},\"seed\":12345,\"metrics\":{{\"end_equity\":{},\"start_equity\":{},\"max_drawdown_pct\":2.0,\"trades\":10,\"wins\":6,\"liquidations\":0}}}}", 100 + d, 10 + d, WALLET * 1.01, WALLET)]).unwrap();
        }
        for i in 0..n_trades {
            let ts = t0 + (i % days.max(1)) * DAY + 1000 * i;
            c.execute("INSERT INTO trades(id, run_id, symbol, side, entry_ts, exit_ts, entry, exit, qty, leverage, fees, funding, pnl, ret_margin, reason, params_id) VALUES (?1,?2,'AAAUSDT','long',?3,?4,1.0,1.01,55.5,11.0,0.05,0.0,?5,?6,'Take profit',1)",
                params![first_trade_id + i, run, ts, ts + 3_600_000, WALLET * 0.002 * if i % 3 == 0 { -1.0 } else { 1.0 }, 0.02 * if i % 3 == 0 { -1.0 } else { 1.0 }]).unwrap();
        }
    }

    /// Test helper: runs the paper import at a fixed time.
    fn import_now(f: &Fixture) -> PaperReport { import(&f.cfg, &f.mem, &f.data, 1_800_000_000_000).unwrap() }

    /// Test: newer archive columns decision time and features are imported when present.
    #[test]
    fn newer_archive_columns_decision_time_and_features_are_imported_when_present() {
        let f = fixture();
        let a = create_archive(&f.archive);
        a.execute_batch("ALTER TABLE trades ADD COLUMN decided_ts INTEGER; ALTER TABLE trades ADD COLUMN features TEXT;").unwrap();
        fill(&a, 1, 1_790_000_000_000, 3, 4, 1);
        a.execute("UPDATE trades SET decided_ts = entry_ts - 1800000, features = '{\"coin\":{\"ret_24h_pct\":-3.2}}' WHERE id <= 2", []).unwrap();
        let r = import_now(&f);
        assert_eq!(r.new_trades, 4);
        let (dec, entry, feats): (Option<i64>, i64, Option<String>) = f.data.query_row("SELECT decided_ts, entry_ts, features_json FROM paper_trades WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(entry / 1000 - dec.unwrap(), entry / 1000 - dec.unwrap(), "stored in seconds");
        assert!(feats.unwrap().contains("ret_24h_pct"));
        // older rows without the new columns stay NULL, never invented
        let (dec4, feats4): (Option<i64>, Option<String>) = f.data.query_row("SELECT decided_ts, features_json FROM paper_trades WHERE id = 4", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!(dec4.is_none() && feats4.is_none());
        // a poisoned features text is rejected like every other text field
        a.execute("INSERT INTO trades(id, run_id, symbol, side, entry_ts, exit_ts, entry, exit, qty, leverage, fees, funding, pnl, ret_margin, reason, params_id, features) VALUES (99,1,'X','long',1790000009000,1790000010000,1,1,1,1,0,0,0,0,'r',1,'{\"api_key\":\"x\"}')", []).unwrap();
        assert!(import(&f.cfg, &f.mem, &f.data, 1_800_000_000_000).is_err());
    }

    /// Test: synthetic labs never read the real archive.
    #[test]
    fn synthetic_labs_never_read_the_real_archive() {
        let mut f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 3, 5, 1);
        f.cfg.mode = "synthetic".into();
        let r = import_now(&f);
        assert_eq!((r.source.as_str(), r.trades_total), ("disabled", 0));
    }

    /// Test: missing archive is absent not an error.
    #[test]
    fn missing_archive_is_absent_not_an_error() {
        let f = fixture();
        let r = import_now(&f);
        assert_eq!((r.source.as_str(), r.state.as_str(), r.trades_total), ("absent", "empty", 0));
    }

    /// Test: sufficiency ladder boundaries.
    #[test]
    fn sufficiency_ladder_boundaries() {
        let f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 1, 0, 1);
        assert_eq!(import_now(&f).state, "empty");
        fill(&a, 1, 1_790_000_000_000, 1, 29, 1);
        assert_eq!(import_now(&f).state, "thin"); // 29 trades
        // 30 trades but only 13 days -> still thin
        let f2 = fixture();
        let a2 = create_archive(&f2.archive);
        fill(&a2, 1, 1_790_000_000_000, 13, 30, 1);
        assert_eq!(import_now(&f2).state, "thin");
        let f3 = fixture();
        let a3 = create_archive(&f3.archive);
        fill(&a3, 1, 1_790_000_000_000, 14, 30, 1);
        assert_eq!(import_now(&f3).state, "usable");
        let f4 = fixture();
        let a4 = create_archive(&f4.archive);
        fill(&a4, 1, 1_790_000_000_000, 28, 200, 1);
        assert_eq!(import_now(&f4).state, "rich");
    }

    /// Test: import is incremental and idempotent.
    #[test]
    fn import_is_incremental_and_idempotent() {
        let f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 3, 10, 1);
        let r1 = import_now(&f);
        assert_eq!((r1.new_trades, r1.new_equity, r1.new_searches), (10, 3, 3));
        let r2 = import_now(&f);
        assert_eq!((r2.new_trades, r2.new_equity, r2.new_searches), (0, 0, 0));
        a.execute_batch("INSERT INTO trades(id, run_id, symbol, side, entry_ts, exit_ts, entry, exit, qty, leverage, fees, funding, pnl, ret_margin, reason, params_id) VALUES (500,1,'BBBUSDT','long',1790000100000,1790000200000,1,1.1,10,11,0.1,0,5,0.05,'Exit',1)").unwrap();
        let r3 = import_now(&f);
        assert_eq!(r3.new_trades, 1);
        assert_eq!(r3.trades_total, 11);
    }

    /// Test: amounts and percentages are both stored and consistent.
    #[test]
    fn amounts_and_percentages_are_both_stored_and_consistent() {
        let f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 5, 10, 1);
        import_now(&f);
        // absolute paper amounts exactly as the bot recorded them
        let (eq, start): (f64, f64) = f.data.query_row("SELECT equity, start_equity FROM paper_equity ORDER BY ts_ms LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!((eq - WALLET).abs() < 1e-9 && (start - WALLET).abs() < 1e-9);
        let (pnl, qty, fees, pnl_pct): (f64, f64, f64, f64) = f.data.query_row("SELECT pnl, qty, fees, pnl_pct FROM paper_trades WHERE id = 2", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert!((pnl - WALLET * 0.002).abs() < 1e-9 && (qty - 55.5).abs() < 1e-9 && (fees - 0.05).abs() < 1e-9);
        // ...and the same figures as percent of the run's starting equity
        assert!((pnl_pct - pnl / WALLET * 100.0).abs() < 1e-9, "{pnl_pct}");
        let idx: f64 = f.data.query_row("SELECT equity_index FROM paper_equity ORDER BY ts_ms LIMIT 1", [], |r| r.get(0)).unwrap();
        assert!((idx - 100.0).abs() < 1e-9);
        let (s_start, s_end): (f64, f64) = f.data.query_row("SELECT start_equity, end_equity FROM paper_searches LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert!((s_start - WALLET).abs() < 1e-9 && (s_end - WALLET * 1.01).abs() < 1e-9);
    }

    /// Test: the original archive is never modified.
    #[test]
    fn the_original_archive_is_never_modified() {
        let f = fixture();
        {
            let a = create_archive(&f.archive);
            fill(&a, 1, 1_790_000_000_000, 4, 12, 1);
            a.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        }
        let before = (crate::util::sha256_file(&f.archive).unwrap(), std::fs::metadata(&f.archive).unwrap().len(), std::fs::metadata(&f.archive).unwrap().modified().unwrap());
        import_now(&f);
        import_now(&f);
        let after = (crate::util::sha256_file(&f.archive).unwrap(), std::fs::metadata(&f.archive).unwrap().len(), std::fs::metadata(&f.archive).unwrap().modified().unwrap());
        assert_eq!(before, after);
    }

    /// Test: segments are never merged and a recreated archive starts a new epoch.
    #[test]
    fn segments_are_never_merged_and_a_recreated_archive_starts_a_new_epoch() {
        let f = fixture();
        {
            let a = create_archive(&f.archive);
            fill(&a, 1, 1_790_000_000_000, 3, 5, 1);
            fill(&a, 2, 1_790_500_000_000, 3, 5, 6);
        }
        let r = import_now(&f);
        assert_eq!((r.runs, r.trades_total, r.epoch), (2, 10, 1));
        // The bot's archive is deleted and recreated with fewer rows (ids restart): nothing is silently lost.
        std::fs::remove_file(&f.archive).ok();
        std::fs::remove_file(f.archive.with_extension("sqlite3-wal")).ok();
        std::fs::remove_file(f.archive.with_extension("sqlite3-shm")).ok();
        {
            let a = create_archive(&f.archive);
            fill(&a, 1, 1_795_000_000_000, 2, 3, 1);
        }
        let r2 = import_now(&f);
        assert!(r2.epoch_reset && r2.epoch == 2, "{r2:?}");
        assert_eq!(r2.trades_total, 13); // old 10 kept + 3 new in epoch 2
        assert_eq!(r2.runs, 3);
    }

    /// Test: schema drift and garbage leave the lab working.
    #[test]
    fn schema_drift_and_garbage_leave_the_lab_working() {
        let f = fixture();
        std::fs::create_dir_all(f.archive.parent().unwrap()).unwrap();
        let c = Connection::open(&f.archive).unwrap();
        c.execute_batch("CREATE TABLE runs(run_id INTEGER PRIMARY KEY);").unwrap();
        drop(c);
        let r = import_now(&f);
        assert_eq!(r.source, "schema_mismatch");
        assert!(r.note.contains("runs") || r.note.contains("params"), "{}", r.note);
        std::fs::write(&f.archive, b"this is not a database").unwrap();
        let r2 = import_now(&f);
        assert!(r2.source == "locked" || r2.source == "schema_mismatch", "{r2:?}");
        // never panics and the best-effort wrapper never errors out a research cycle
        let r3 = import_best_effort(&f.cfg, &f.mem, &f.data);
        assert!(!r3.state.is_empty());
    }

    /// Test: poisoned text fields are rejected.
    #[test]
    fn poisoned_text_fields_are_rejected() {
        let f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 2, 3, 1);
        a.execute("INSERT INTO params VALUES (9, '{\"api_key\":\"abc\"}', 1)", []).unwrap();
        assert!(import(&f.cfg, &f.mem, &f.data, 1_800_000_000_000).is_err());
        let n: i64 = f.data.query_row("SELECT count(*) FROM paper_params", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0); // the failed import rolled back completely
    }

    /// Test: prompt block is honest about thin data and contains no rows or amounts.
    #[test]
    fn prompt_block_is_honest_about_thin_data_and_contains_no_rows_or_amounts() {
        let f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 3, 5, 1);
        let r = import_now(&f);
        let b = prompt_block(&f.cfg, &f.data, &r).unwrap();
        assert!(b.contains("thin") && b.contains("Draw NO conclusions"), "{b}");
        assert!(!b.contains("AAAUSDT") && !b.contains("win rate"), "no raw rows or symbols: {b}");
        assert!(b.contains("123.46 USDT") && b.contains('%'), "amounts and percentage expected: {b}");
        let g = fixture();
        let a = create_archive(&g.archive);
        fill(&a, 1, 1_790_000_000_000, 20, 60, 1);
        let r = import_now(&g);
        let b = prompt_block(&g.cfg, &g.data, &r).unwrap();
        assert!(b.contains("state: usable") && b.contains("Win rate") && b.contains("95% interval") && b.contains("Closed-trade totals"), "{b}");
    }

    /// Test: staleness is reported.
    #[test]
    fn staleness_is_reported() {
        let f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 2, 1, 1);
        let fresh = import(&f.cfg, &f.mem, &f.data, 1_790_000_000_000 + DAY + 1000).unwrap();
        assert!(!fresh.stale);
        let stale = import(&f.cfg, &f.mem, &f.data, 1_790_000_000_000 + DAY + 3 * 3_600_000).unwrap();
        assert!(stale.stale);
    }

    /// Test: search decay refuses to conclude on few pairs and measures when enough.
    #[test]
    fn search_decay_refuses_to_conclude_on_few_pairs_and_measures_when_enough() {
        let f = fixture();
        let a = create_archive(&f.archive);
        fill(&a, 1, 1_790_000_000_000, 5, 0, 1);
        import_now(&f);
        let d = search_decay(&f.data, 8, 200).unwrap();
        assert_eq!(d.verdict, "insufficient");
        let g = fixture();
        let b = create_archive(&g.archive);
        fill(&b, 1, 1_790_000_000_000, 20, 0, 1);
        import_now(&g);
        let d = search_decay(&g.data, 8, 200).unwrap();
        assert_eq!(d.verdict, "measured");
        assert!(d.n_pairs >= 8 && d.placebo_p > 0.0 && d.placebo_p <= 1.0);
    }
}

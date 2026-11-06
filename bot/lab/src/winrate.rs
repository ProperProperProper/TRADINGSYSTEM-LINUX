//! Why runs win or lose (user 2026-10-09: "anything under 60% win rate is a loser, anything above is a winner; it must learn why").
//!
//! Unit: one backtest run = one set of settings traded unchanged for one 14-session block, from the bot's archive (random and
//! out-of-sample runs only: unselected, so no search bias), at the bot's capital, with at least `MIN_TRADES` closed trades.
//! A run is a WINNER when at least 60% of its closed trades made money, else a LOSER.
//!
//! Candidate reasons, per run:
//! - `setting`: every numeric or yes/no field of the settings, and each category (signal, regime, trend line kind ...);
//! - `entry`: the average entry conditions of its trades (stock and market state at the decision: returns, distance from
//!   average, volume, SPY trend, breadth, time of day ...);
//! - `exit mix` (how, not why): the share of its trades closed by each exit reason.
//!
//! Each reason is learned on the EARLIER two thirds of the blocks (direction and split point at the training median) and judged
//! only on the LATER third: winner share on the good side minus the bad side, with a 95% interval from resampling whole blocks
//! (trades in one block share the market, so they are not independent), and false-discovery control over all reasons.
//! A reason is `confirmed` only when the later blocks agree. Nothing here is typed by the model; the model reads the result.

use crate::stats;
use anyhow::Result;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A run wins when at least this share of its closed trades made money (user rule).
pub const WIN_RATE: f64 = 0.60;
/// Closed trades a run needs before its win rate means anything.
pub const MIN_TRADES: i64 = 5;
/// Block resamples for the intervals.
const BOOT: usize = 1000;

/// One run: block, time, winner or not, and its candidate reasons by name.
struct RunObs {
    block: String,
    start: i64,
    win: bool,
    x: BTreeMap<String, (String, f64)>,
}

/// One tested reason.
#[derive(Debug, Clone)]
pub struct Finding {
    pub driver: String,
    pub kind: String,
    /// +1: higher values go with winners; -1: higher values go with losers.
    pub direction: i8,
    pub split: f64,
    pub train_auc: f64,
    pub test_auc: f64,
    pub test_win_good: f64,
    pub test_win_bad: f64,
    pub delta: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub q: f64,
    pub n_train: usize,
    pub n_test: usize,
    pub confirmed: bool,
}

/// Creates the findings tables if missing.
pub fn init(mem: &Connection) -> Result<()> {
    mem.execute_batch(
        "CREATE TABLE IF NOT EXISTS winrate_findings (
            id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, driver TEXT NOT NULL, kind TEXT NOT NULL, direction INTEGER NOT NULL, split REAL NOT NULL,
            train_auc REAL NOT NULL, test_auc REAL NOT NULL, test_win_good REAL NOT NULL, test_win_bad REAL NOT NULL, delta REAL NOT NULL,
            ci_low REAL NOT NULL, ci_high REAL NOT NULL, q REAL NOT NULL, n_train INTEGER NOT NULL, n_test INTEGER NOT NULL, confirmed INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS winrate_summary (ts INTEGER PRIMARY KEY, json TEXT NOT NULL);",
    )?;
    Ok(())
}

/// Flattens settings JSON into named numbers: numbers as they are, yes/no as 1/0, text as one category flag `name=value`.
fn flatten(prefix: &str, v: &Value, out: &mut BTreeMap<String, (String, f64)>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let name = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten(&name, x, out);
            }
        }
        Value::Number(n) => {
            if let Some(x) = n.as_f64().filter(|x| x.is_finite()) {
                out.insert(format!("setting {prefix}"), ("setting".into(), x));
            }
        }
        Value::Bool(b) => {
            out.insert(format!("setting {prefix}"), ("setting".into(), if *b { 1.0 } else { 0.0 }));
        }
        Value::String(s) => {
            out.insert(format!("setting {prefix}={s}"), ("setting".into(), 1.0));
        }
        _ => {}
    }
}

/// Loads the runs of one capital with their label and candidate reasons.
fn load(data: &Connection, capital: f64) -> Result<Vec<RunObs>> {
    let mut st = data.prepare(
        "SELECT run_id, window_label, window_start, params_json, json_extract(metrics_json, '$.trades'), json_extract(metrics_json, '$.wins') \
         FROM runs WHERE run_key IS NOT NULL AND role IN ('random', 'oos') AND abs(json_extract(metrics_json, '$.start_equity') - ?1) < 0.5 \
         AND json_extract(metrics_json, '$.trades') >= ?2",
    )?;
    let rows: Vec<(String, String, i64, String, i64, i64)> =
        st.query_map(params![capital, MIN_TRADES], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?.collect::<Result<_, _>>()?;
    // Average entry conditions and exit mix of the runs whose trades are stored.
    let names = crate::db::registry(data)?;
    let mut entry: BTreeMap<String, BTreeMap<String, (f64, usize)>> = BTreeMap::new();
    if !names.is_empty() {
        let cols: String = names.iter().map(|f| format!(", f.f_{f}")).collect();
        let mut st = data.prepare(&format!(
            "SELECT t.run_id, t.reason{cols} FROM trades t JOIN trade_features f ON f.run_id = t.run_id AND f.idx = t.idx JOIN runs r ON r.run_id = t.run_id \
             WHERE r.run_key IS NOT NULL AND r.role IN ('random', 'oos') AND abs(json_extract(r.metrics_json, '$.start_equity') - ?1) < 0.5"
        ))?;
        let mut q = st.query([capital])?;
        while let Some(r) = q.next()? {
            let id: String = r.get(0)?;
            let reason: String = r.get(1)?;
            let e = entry.entry(id).or_default();
            let ex = e.entry(format!("exit share {}", reason.split(['(', ':']).next().unwrap_or(&reason).trim())).or_insert((0.0, 0));
            ex.0 += 1.0;
            for (k, f) in names.iter().enumerate() {
                if let Ok(Some(v)) = r.get::<_, Option<f64>>(2 + k) {
                    if v.is_finite() {
                        let s = e.entry(format!("entry {f}")).or_insert((0.0, 0));
                        s.0 += v;
                        s.1 += 1;
                    }
                }
            }
            e.entry("__n".into()).or_insert((0.0, 0)).0 += 1.0;
        }
    }
    let mut out = Vec::new();
    for (id, block, start, pj, trades, wins) in rows {
        let mut x = BTreeMap::new();
        if let Ok(v) = serde_json::from_str::<Value>(&pj) {
            flatten("", &v, &mut x);
        }
        if let Some(e) = entry.get(&id) {
            let n = e.get("__n").map_or(0.0, |v| v.0);
            for (k, (sum, cnt)) in e {
                if k == "__n" {
                    continue;
                }
                if k.starts_with("exit share") {
                    if n > 0.0 {
                        x.insert(k.clone(), ("exit mix".into(), sum / n));
                    }
                } else if *cnt > 0 {
                    x.insert(k.clone(), ("entry".into(), sum / *cnt as f64));
                }
            }
        }
        out.push(RunObs { block, start, win: wins as f64 / trades as f64 >= WIN_RATE, x });
    }
    Ok(out)
}

/// Area under the ROC curve of `x` for telling winners from losers (0.5 = no information), ties counted half.
fn auc(xs: &[(f64, bool)]) -> f64 {
    let (mut pos, mut neg): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for &(x, w) in xs {
        if w { pos.push(x) } else { neg.push(x) }
    }
    if pos.is_empty() || neg.is_empty() {
        return 0.5;
    }
    let mut all: Vec<(f64, bool)> = xs.to_vec();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    // Mann-Whitney U from average ranks.
    let mut rank_sum = 0.0;
    let mut i = 0;
    while i < all.len() {
        let mut j = i;
        while j + 1 < all.len() && all[j + 1].0 == all[i].0 {
            j += 1;
        }
        let r = (i + j) as f64 / 2.0 + 1.0;
        for item in &all[i..=j] {
            if item.1 {
                rank_sum += r;
            }
        }
        i = j + 1;
    }
    let (np, nn) = (pos.len() as f64, neg.len() as f64);
    (rank_sum - np * (np + 1.0) / 2.0) / (np * nn)
}

/// Winner share on the good side of `split` minus the bad side, for `direction` (+1: above is good).
fn split_delta(obs: &[(f64, bool, usize)], split: f64, direction: i8) -> Option<(f64, f64, f64)> {
    let (mut gw, mut gn, mut bw, mut bn) = (0.0, 0.0, 0.0, 0.0);
    for &(x, w, _) in obs {
        let good = if direction > 0 { x > split } else { x <= split };
        if good { gn += 1.0; if w { gw += 1.0 } } else { bn += 1.0; if w { bw += 1.0 } }
    }
    (gn >= 10.0 && bn >= 10.0).then(|| (gw / gn, bw / bn, gw / gn - bw / bn))
}

/// Learns every reason on the earlier blocks and judges it on the later ones; stores the findings and a summary. Returns them.
pub fn analyse(mem: &Connection, data: &Connection, capital: f64) -> Result<(Value, Vec<Finding>)> {
    init(mem)?;
    let runs = load(data, capital)?;
    anyhow::ensure!(runs.len() >= 100, "only {} runs with at least {MIN_TRADES} closed trades at {capital} USD", runs.len());
    // Time split by block: the earliest two thirds of blocks train, the latest third tests.
    let mut blocks: Vec<(i64, String)> = runs.iter().map(|r| (r.start, r.block.clone())).collect::<BTreeSet<_>>().into_iter().collect();
    blocks.sort();
    let mut seen = BTreeSet::new();
    blocks.retain(|b| seen.insert(b.1.clone()));
    let cut = blocks.len() * 2 / 3;
    let train_blocks: BTreeSet<&String> = blocks[..cut].iter().map(|b| &b.1).collect();
    let block_index: BTreeMap<&String, usize> = blocks.iter().enumerate().map(|(i, b)| (&b.1, i)).collect();
    let (train, test): (Vec<&RunObs>, Vec<&RunObs>) = runs.iter().partition(|r| train_blocks.contains(&r.block));
    let share = |v: &[&RunObs]| v.iter().filter(|r| r.win).count() as f64 / v.len().max(1) as f64;
    let names: BTreeSet<(String, String)> = runs.iter().flat_map(|r| r.x.iter().map(|(k, (kind, _))| (k.clone(), kind.clone()))).collect();
    let test_blocks: Vec<usize> = test.iter().map(|r| block_index[&r.block]).collect::<BTreeSet<_>>().into_iter().collect();
    let mut rng = ChaCha8Rng::seed_from_u64(60);
    let mut found: Vec<Finding> = Vec::new();
    let mut pvals: Vec<f64> = Vec::new();
    for (name, kind) in names {
        let tr: Vec<(f64, bool)> = train.iter().filter_map(|r| r.x.get(&name).map(|v| (v.1, r.win))).collect();
        let te: Vec<(f64, bool, usize)> = test.iter().filter_map(|r| r.x.get(&name).map(|v| (v.1, r.win, block_index[&r.block]))).collect();
        if tr.len() < 50 || te.len() < 30 {
            continue;
        }
        let distinct: BTreeSet<u64> = tr.iter().map(|v| v.0.to_bits()).collect();
        if distinct.len() < 2 {
            continue;
        }
        let a = auc(&tr);
        let direction: i8 = if a >= 0.5 { 1 } else { -1 };
        let mut xs: Vec<f64> = tr.iter().map(|v| v.0).collect();
        xs.sort_by(f64::total_cmp);
        let mut split = stats::quantile(&xs, 0.5);
        // A yes/no or category flag splits between its two values.
        if distinct.len() == 2 {
            split = (xs[0] + xs[xs.len() - 1]) / 2.0;
        }
        let Some((g, b, d)) = split_delta(&te, split, direction) else { continue };
        // Block bootstrap on the later blocks.
        let by_block: BTreeMap<usize, Vec<(f64, bool, usize)>> = te.iter().fold(BTreeMap::new(), |mut m, o| { m.entry(o.2).or_insert_with(Vec::new).push(*o); m });
        let mut ds = Vec::with_capacity(BOOT);
        for _ in 0..BOOT {
            let mut sample = Vec::new();
            for _ in 0..test_blocks.len() {
                let blk = test_blocks[rng.gen_range(0..test_blocks.len())];
                if let Some(v) = by_block.get(&blk) { sample.extend_from_slice(v); }
            }
            if let Some((_, _, x)) = split_delta(&sample, split, direction) { ds.push(x); }
        }
        if ds.len() < BOOT / 2 {
            continue;
        }
        ds.sort_by(f64::total_cmp);
        let (lo, hi) = (stats::quantile(&ds, 0.025), stats::quantile(&ds, 0.975));
        let p = ((ds.iter().filter(|x| **x <= 0.0).count() as f64 + 1.0) / (ds.len() as f64 + 1.0) * 2.0).min(1.0);
        let te_auc = auc(&te.iter().map(|v| (v.0, v.1)).collect::<Vec<_>>());
        pvals.push(p);
        found.push(Finding { driver: name, kind, direction, split, train_auc: a, test_auc: te_auc, test_win_good: g, test_win_bad: b, delta: d,
            ci_low: lo, ci_high: hi, q: 1.0, n_train: tr.len(), n_test: te.len(), confirmed: false });
    }
    let q = stats::bh_qvalues(&pvals);
    for (f, qv) in found.iter_mut().zip(q) {
        f.q = qv;
        f.confirmed = f.ci_low > 0.0 && qv <= 0.10 && (f.test_auc - 0.5) * f.direction as f64 > 0.0;
    }
    found.sort_by(|a, b| b.confirmed.cmp(&a.confirmed).then(b.delta.total_cmp(&a.delta)));
    let now = crate::util::now_ts();
    let tx = mem.unchecked_transaction()?;
    tx.execute("DELETE FROM winrate_findings", [])?;
    for f in &found {
        tx.execute(
            "INSERT INTO winrate_findings(ts, driver, kind, direction, split, train_auc, test_auc, test_win_good, test_win_bad, delta, ci_low, ci_high, q, n_train, n_test, confirmed) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![now, f.driver, f.kind, f.direction, f.split, f.train_auc, f.test_auc, f.test_win_good, f.test_win_bad, f.delta, f.ci_low, f.ci_high, f.q, f.n_train as i64, f.n_test as i64, f.confirmed],
        )?;
    }
    let summary = json!({
        "capital": capital, "runs": runs.len(), "blocks": blocks.len(), "train_blocks": cut, "test_blocks": blocks.len() - cut,
        "winner_share_train": share(&train), "winner_share_test": share(&test), "win_rate_rule": WIN_RATE, "min_trades": MIN_TRADES,
        "reasons_tested": found.len(), "confirmed": found.iter().filter(|f| f.confirmed).count(),
        "first_block": blocks.first().map(|b| b.0), "last_block": blocks.last().map(|b| b.0),
    });
    tx.execute("INSERT OR REPLACE INTO winrate_summary(ts, json) VALUES (?1, ?2)", params![now, summary.to_string()])?;
    tx.commit()?;
    Ok((summary, found))
}

/// One plain sentence for a finding, from its numbers only.
pub fn sentence(f: &Finding) -> String {
    let side = if f.direction > 0 { "above" } else { "at or below" };
    let what = f.driver.replace("setting ", "setting `").replace("entry ", "entry `").replace("exit share ", "share of exits by `");
    let what = if what.contains('`') { format!("{what}`") } else { what };
    format!(
        "{what} {side} {:.4}: {:.0}% of runs were winners (>=60% win rate) vs {:.0}% otherwise in later unseen blocks (difference {:+.0} points, 95% {:+.0} to {:+.0}; AUC {:.2} early, {:.2} later; q {:.3})",
        f.split, f.test_win_good * 100.0, f.test_win_bad * 100.0, f.delta * 100.0, f.ci_low * 100.0, f.ci_high * 100.0, f.train_auc, f.test_auc, f.q
    )
}

/// The stored findings as whiteboard text for the model: what makes winners and losers, confirmed first.
pub fn board_text(mem: &Connection, limit: usize) -> Result<String> {
    init(mem)?;
    let summary: Option<String> = mem.query_row("SELECT json FROM winrate_summary ORDER BY ts DESC LIMIT 1", [], |r| r.get(0)).ok();
    let Some(summary) = summary else { return Ok(String::new()) };
    let s: Value = serde_json::from_str(&summary)?;
    let mut out = format!(
        "WHY RUNS WIN OR LOSE (winner = >=60% of closed trades profitable; {} runs at {} USD; winners {:.0}% early, {:.0}% in later blocks; learned early, judged later):\n",
        s["runs"], s["capital"], s["winner_share_train"].as_f64().unwrap_or(0.0) * 100.0, s["winner_share_test"].as_f64().unwrap_or(0.0) * 100.0
    );
    let mut st = mem.prepare("SELECT driver, kind, direction, split, train_auc, test_auc, test_win_good, test_win_bad, delta, ci_low, ci_high, q, n_train, n_test, confirmed FROM winrate_findings ORDER BY confirmed DESC, delta DESC LIMIT ?1")?;
    let rows = st.query_map([limit as i64], |r| Ok(Finding {
        driver: r.get(0)?, kind: r.get(1)?, direction: r.get::<_, i64>(2)? as i8, split: r.get(3)?, train_auc: r.get(4)?, test_auc: r.get(5)?, test_win_good: r.get(6)?,
        test_win_bad: r.get(7)?, delta: r.get(8)?, ci_low: r.get(9)?, ci_high: r.get(10)?, q: r.get(11)?, n_train: r.get::<_, i64>(12)? as usize, n_test: r.get::<_, i64>(13)? as usize, confirmed: r.get(14)?,
    }))?;
    for f in rows {
        let f = f?;
        out.push_str(&format!("  [{}{}] {}\n", if f.confirmed { "CONFIRMED" } else { "not confirmed" }, if f.kind == "exit mix" { ", how not why" } else { "" }, sentence(&f)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: auc is 1 for a perfect separator, 0 for a reversed one and 0.5 for no information.
    #[test]
    fn auc_matches_hand_cases() {
        assert!((auc(&[(1.0, false), (2.0, false), (3.0, true), (4.0, true)]) - 1.0).abs() < 1e-12);
        assert!(auc(&[(1.0, true), (2.0, true), (3.0, false), (4.0, false)]).abs() < 1e-12);
        assert!((auc(&[(1.0, true), (1.0, false)]) - 0.5).abs() < 1e-12);
    }

    /// Test: settings flatten to numbers, yes/no flags and category flags.
    #[test]
    fn settings_flatten_to_named_numbers() {
        let mut m = BTreeMap::new();
        flatten("", &json!({"hold": 3, "flip": true, "signal": "Return", "risk": {"trend_bars": 5, "x": null}}), &mut m);
        assert_eq!(m["setting hold"].1, 3.0);
        assert_eq!(m["setting flip"].1, 1.0);
        assert_eq!(m["setting signal=Return"].1, 1.0);
        assert_eq!(m["setting risk.trend_bars"].1, 5.0);
        assert!(!m.contains_key("setting risk.x"));
    }

    /// Test: the split difference needs at least ten runs on each side.
    #[test]
    fn split_difference_counts_both_sides() {
        let obs: Vec<(f64, bool, usize)> = (0..40).map(|i| (i as f64, i >= 20 && i % 2 == 0, 0)).collect();
        let (g, b, d) = split_delta(&obs, 19.5, 1).unwrap();
        assert!((g - 0.5).abs() < 1e-12 && b == 0.0 && (d - 0.5).abs() < 1e-12);
        assert!(split_delta(&obs[..15], 5.0, 1).is_none());
    }
}

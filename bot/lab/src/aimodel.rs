//! THE MODEL (owner 2026-10-13: "make it a proper AI model for this trading bot and learning").
//!
//! One packaged, versioned model with the parts a proper model has:
//!   * an ARTIFACT: the fitted weights, the feature list, the training window and scheme, a calibration table, saved as JSON (`<lab>/model/v<N>.json`);
//!   * TRAINING (`train`): the bot's top-20 universe, the bot's own score components, SPY-regime interactions, a ridge whose strength is chosen on held-out data;
//!   * INFERENCE on the live bars (`predict_live`): the newest moment's ranking of the bot's top-20 stocks, each with a score, a percentile, a CALIBRATED expected
//!     excess return and hit rate (measured on a held-out tail), and the three features that drove it (the model's own explanation);
//!   * a REGISTRY with a current-version pointer, and MONITORING: every version's live out-of-sample IC after its training end, tracked per session;
//!   * a LIFECYCLE (`maintain`): a new version every 14 sessions; the previous version is restored if the new one is significantly worse over the same sessions.
//!
//! Nothing here touches the live strategy, an account or an order: it ranks stocks and reports how well it did.

use crate::candles::Bar;
use crate::mlcandle::{Ridge, Sample, FEATURES, MIN_CROSS};
use crate::mlscores::{tuned_ridge, Scheme};
use crate::{mlbot, mlcandle as m};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DAY_MS: i64 = 86_400_000;
pub const RETRAIN_SESSIONS: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bin { pub pct_lo: f64, pub pct_hi: f64, pub mean_bps: f64, pub hit: f64, pub n: usize }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub version: u32,
    pub created: i64,
    pub trained_through_ms: i64,
    pub samples: usize,
    pub scheme: String,
    pub features: Vec<String>,
    pub ridge: Ridge,
    /// Realised excess return by decile of the within-moment percentile, measured out of sample (a model fitted on the earlier 85% of the sessions, scored on the rest).
    pub calibration: Vec<Bin>,
    /// Rank IC of that held-out tail.
    pub heldout_ic: f64,
}

pub fn feature_names() -> Vec<String> {
    FEATURES.iter().map(|s| s.to_string()).chain(mlbot::EXTRA.iter().map(|s| s.to_string()))
        .chain(["ret_26 x spy trend", "vol_26 x spy trend", "rsi x spy trend", "|ret_26| x vol_26"].iter().map(|s| s.to_string())).collect()
}

fn scheme_name(s: Scheme) -> String {
    match (s.window_days, s.half_life_days) { (Some(d), _) => format!("rolling {d} days"), (_, Some(h)) => format!("half-life {h:.0} days"), _ => "all older history".into() }
}

/// Samples in the bot's top-20 universe with all features (the interactions last). `labelled` = completed labels (training), else the newest unlabelled bars.
fn universe_samples(series: &BTreeMap<String, Vec<Bar>>, labelled: bool, step: i64) -> Vec<Sample> {
    let base = if labelled { m::build(series, step) } else { m::build_live(series) };
    mlbot::interactions(&mlbot::in_universe(&mlbot::extend(series, &base), FEATURES.len()))
}

fn session(ts: i64) -> i64 { (ts + m::et_offset_ms(ts)).div_euclid(DAY_MS) }

/// Pool-adjacent-violators: the closest NON-DECREASING sequence to the bin means (weighted by n). A higher percentile can never promise less than a lower one;
/// the noise in a small held-out tail is averaged out instead of being presented as structure. Hit rates are made monotone the same way.
pub fn isotonic(bins: &mut [Bin]) {
    fn pav(v: &[(f64, f64)]) -> Vec<f64> {
        let mut blocks: Vec<(f64, f64, usize)> = vec![]; // (weighted sum, weight, count)
        for (x, w) in v { blocks.push((x * w, *w, 1)); while blocks.len() > 1 { let n = blocks.len(); let (a, b) = (blocks[n - 2], blocks[n - 1]); if a.0 / a.1.max(1e-12) <= b.0 / b.1.max(1e-12) { break; } blocks.truncate(n - 2); blocks.push((a.0 + b.0, a.1 + b.1, a.2 + b.2)); } }
        blocks.iter().flat_map(|b| std::iter::repeat_n(b.0 / b.1.max(1e-12), b.2)).collect()
    }
    let m = pav(&bins.iter().map(|b| (b.mean_bps, b.n.max(1) as f64)).collect::<Vec<_>>());
    let h = pav(&bins.iter().map(|b| (b.hit, b.n.max(1) as f64)).collect::<Vec<_>>());
    for (i, b) in bins.iter_mut().enumerate() { b.mean_bps = m[i]; b.hit = h[i]; }
}

/// Decile calibration of `pred` against the realised excess returns, by within-moment percentile.
pub fn calibrate(test: &[Sample], pred: &[f64]) -> Vec<Bin> {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, s) in test.iter().enumerate() { by.entry(s.ts).or_default().push(i); }
    let mut sums = vec![(0.0f64, 0usize, 0usize); 10];
    for idx in by.values().filter(|v| v.len() >= MIN_CROSS) {
        let mut o = idx.clone(); o.sort_by(|&a, &b| pred[a].total_cmp(&pred[b]));
        for (pos, &i) in o.iter().enumerate() {
            let pct = pos as f64 / (o.len() - 1) as f64;
            let b = ((pct * 10.0) as usize).min(9);
            sums[b].0 += test[i].excess; sums[b].1 += usize::from(test[i].excess > 0.0); sums[b].2 += 1;
        }
    }
    sums.iter().enumerate().map(|(b, s)| Bin { pct_lo: b as f64 * 10.0, pct_hi: (b + 1) as f64 * 10.0, mean_bps: if s.2 > 0 { s.0 / s.2 as f64 * 1e4 } else { 0.0 }, hit: if s.2 > 0 { s.1 as f64 / s.2 as f64 } else { 0.5 }, n: s.2 }).collect()
}

fn mean_ic(test: &[Sample], pred: &[f64]) -> f64 {
    let w = crate::mlimprove::weekly(test, pred);
    let (s, n) = w.values().fold((0.0, 0usize), |a, x| (a.0 + x.0, a.1 + x.2));
    if n == 0 { 0.0 } else { s / n as f64 }
}

/// Trains a new version on everything labelled so far.
pub fn train(series: &BTreeMap<String, Vec<Bar>>, scheme: Scheme, version: u32) -> Result<Artifact> {
    let samples = universe_samples(series, true, 1);
    if samples.len() < 5000 { return Err(anyhow!("too little data to train ({} samples)", samples.len())); }
    let mut sessions: Vec<i64> = samples.iter().map(|s| session(s.ts)).collect(); sessions.sort(); sessions.dedup();
    let cut = sessions[sessions.len() * 85 / 100];
    let tend = |v: &[Sample]| v.iter().map(|s| s.ts).max().unwrap_or(0);
    // held-out tail for calibration and the honest IC: fit on the earlier 85% (2 sessions purged), score the rest
    let (early, tail): (Vec<Sample>, Vec<Sample>) = (samples.iter().filter(|s| session(s.ts) + 2 < cut).cloned().collect(), samples.iter().filter(|s| session(s.ts) >= cut).cloned().collect());
    let probe = tuned_ridge(&early, tend(&early), scheme.window_days, scheme.half_life_days);
    let pt: Vec<f64> = tail.iter().map(|s| probe.predict(s)).collect();
    let mut calibration = calibrate(&tail, &pt);
    isotonic(&mut calibration);
    let heldout_ic = mean_ic(&tail, &pt);
    let ridge = tuned_ridge(&samples, tend(&samples), scheme.window_days, scheme.half_life_days);
    Ok(Artifact { version, created: crate::util::now_ts(), trained_through_ms: tend(&samples), samples: samples.len(), scheme: scheme_name(scheme), features: feature_names(), ridge, calibration, heldout_ic })
}

#[derive(Debug, Clone)]
pub struct Pick { pub symbol: String, pub score: f64, pub pct: f64, pub expected_bps: f64, pub hit: f64, pub why: Vec<(String, f64)> }

/// Standardises a set of scores to mean 0 and standard deviation 1 (empty in, empty out).
pub fn zscore(v: &[f64]) -> Vec<f64> {
    if v.is_empty() { return vec![]; }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    let sd = (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt().max(1e-12);
    v.iter().map(|x| (x - m) / sd).collect()
}

/// Where the artifact ranks the bot's top-20 stocks at the newest moment with enough names: (bar open ms, picks best first).
impl Artifact {
    /// The model's score for one sample.
    pub fn predict(&self, s: &Sample) -> f64 { self.ridge.predict(s) }
}

/// Where the artifact ranks the bot's top-20 stocks at the newest moment with enough names: (bar open ms, picks best first).
pub fn predict_live(series: &BTreeMap<String, Vec<Bar>>, a: &Artifact, deep: &[crate::deepnet::DeepModel]) -> Option<(i64, Vec<Pick>)> {
    let names: Vec<&String> = series.keys().collect();
    let live = universe_samples(series, false, 1);
    let mut by: BTreeMap<i64, Vec<&Sample>> = BTreeMap::new();
    for s in &live { by.entry(s.ts).or_default().push(s); }
    let (ts, group) = by.iter().rev().find(|(_, v)| v.len() >= MIN_CROSS).map(|(t, v)| (*t, v.clone()))?;
    let mut scored: Vec<(&Sample, f64)> = group.iter().map(|s| (*s, a.predict(s))).collect();
    // a CHAMPION deep model (it beat the ridge at the adjusted level on out-of-sample windows) is blended in: both scores are standardised within the moment and averaged
    if let Some(d) = deep.first() {
        // only members of the FIRST member's genome are used (a bag being replaced must never mix shapes: the rows are built for one sequence length)
        let members: Vec<&crate::deepnet::DeepModel> = deep.iter().filter(|m| m.genome == d.genome).collect();
        let owned: Vec<Sample> = group.iter().map(|s| (*s).clone()).collect();
        let rows = crate::deepnet::build_rows(series, &owned, d.genome.seq_len);
        if rows.len() == owned.len() {
            let per: Vec<Vec<f64>> = members.iter().map(|m| zscore(&crate::deepnet::predict(m, &rows))).collect();
            let dp: Vec<f64> = (0..rows.len()).map(|i| per.iter().map(|p| p[i]).sum::<f64>() / per.len() as f64).collect();
            let (zr, zd) = (zscore(&scored.iter().map(|x| x.1).collect::<Vec<_>>()), zscore(&dp));
            for (i, x) in scored.iter_mut().enumerate() { x.1 = 0.5 * zr[i] + 0.5 * zd[i]; }
        }
    }
    scored.sort_by(|x, y| y.1.total_cmp(&x.1));
    let n = scored.len();
    let picks = scored.iter().enumerate().map(|(rank, (s, score))| {
        let pct = (n - 1 - rank) as f64 / (n - 1) as f64 * 100.0;
        let bin = a.calibration.iter().find(|b| pct >= b.pct_lo && pct <= b.pct_hi).or(a.calibration.last());
        let mut why: Vec<(String, f64)> = a.ridge.contributions(s).into_iter().enumerate().map(|(j, c)| (a.features.get(j).cloned().unwrap_or_default(), c)).collect();
        why.sort_by(|x, y| y.1.abs().total_cmp(&x.1.abs()));
        why.truncate(3);
        Pick { symbol: names[s.sym].clone(), score: *score, pct, expected_bps: bin.map_or(0.0, |b| b.mean_bps), hit: bin.map_or(0.5, |b| b.hit), why }
    }).collect();
    Some((ts, picks))
}

// ------------------------------------------------------------------ the prediction ledger: every live call is scored against what really happened
pub fn init_ledger(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ml_predictions (version INTEGER NOT NULL, ts_ms INTEGER NOT NULL, symbol TEXT NOT NULL, score REAL NOT NULL, pct REAL NOT NULL,
        expected_bps REAL NOT NULL, excess REAL, PRIMARY KEY (version, ts_ms, symbol));")
}

/// Stores one live ranking (the model's calls, made before the outcome exists). A repeat of the same (version, bar, stock) is ignored.
pub fn record_predictions(db: &rusqlite::Connection, version: u32, ts: i64, picks: &[Pick]) -> rusqlite::Result<usize> {
    init_ledger(db)?;
    let mut n = 0;
    for p in picks { n += db.execute("INSERT OR IGNORE INTO ml_predictions(version, ts_ms, symbol, score, pct, expected_bps) VALUES (?1,?2,?3,?4,?5,?6)", rusqlite::params![version, ts, p.symbol, p.score, p.pct, p.expected_bps])?; }
    Ok(n)
}

/// Fills in the realised excess return (next 26 bars, minus the average of the stocks ranked at that moment) for every stored call whose outcome has arrived.
pub fn resolve_predictions(db: &rusqlite::Connection, series: &BTreeMap<String, Vec<Bar>>) -> Result<usize> {
    init_ledger(db)?;
    let moments: Vec<(u32, i64)> = db.prepare("SELECT DISTINCT version, ts_ms FROM ml_predictions WHERE excess IS NULL")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect();
    let mut done = 0;
    for (version, ts) in moments {
        let rows: Vec<(String, f64)> = db.prepare("SELECT symbol, score FROM ml_predictions WHERE version = ?1 AND ts_ms = ?2")?.query_map(rusqlite::params![version, ts], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect();
        let fwd: Vec<Option<f64>> = rows.iter().map(|(s, _)| series.get(s).and_then(|b| b.binary_search_by_key(&ts, |x| x.ts).ok().and_then(|i| b.get(i + m::H).map(|e| (e.close / b[i].close).ln())))).collect();
        if fwd.iter().any(|f| f.is_none()) { continue; } // the outcome is not complete yet
        let mean = fwd.iter().flatten().sum::<f64>() / fwd.len() as f64;
        for ((s, _), f) in rows.iter().zip(&fwd) { db.execute("UPDATE ml_predictions SET excess = ?1 WHERE version = ?2 AND ts_ms = ?3 AND symbol = ?4", rusqlite::params![f.unwrap_or(mean) - mean, version, ts, s])?; done += 1; }
    }
    Ok(done)
}

#[derive(Debug, Clone, Default)]
pub struct Scoreboard { pub moments: usize, pub calls: usize, pub pending: usize, pub ic: f64, pub ic_se: f64, pub top_bps: f64, pub bottom_bps: f64, pub expected_top_bps: f64 }

/// The model's real record: every resolved call. top/bottom = the best and worst 3 of the 20 at each moment.
pub fn scoreboard(db: &rusqlite::Connection, version: Option<u32>) -> Result<Scoreboard> {
    init_ledger(db)?;
    let q = if version.is_some() { "SELECT version, ts_ms, score, excess, expected_bps FROM ml_predictions WHERE excess IS NOT NULL AND version = ?1 ORDER BY ts_ms" } else { "SELECT version, ts_ms, score, excess, expected_bps FROM ml_predictions WHERE excess IS NOT NULL AND ?1 IS NULL ORDER BY ts_ms" };
    let mut by: BTreeMap<(u32, i64), Vec<(f64, f64, f64)>> = BTreeMap::new();
    for r in db.prepare(q)?.query_map([version], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?)))?.flatten() { by.entry((r.0, r.1)).or_default().push((r.2, r.3, r.4)); }
    let pending: i64 = db.query_row("SELECT count(*) FROM ml_predictions WHERE excess IS NULL", [], |r| r.get(0))?;
    let mut sb = Scoreboard { pending: pending as usize, ..Default::default() };
    let (mut per_day, mut top, mut bot, mut exp): (BTreeMap<i64, Vec<f64>>, Vec<f64>, Vec<f64>, Vec<f64>) = Default::default();
    for ((_, ts), v) in &by {
        if v.len() < 6 { continue; }
        let (p, e): (Vec<f64>, Vec<f64>) = v.iter().map(|x| (x.0, x.1)).unzip();
        per_day.entry(session(*ts)).or_default().push(crate::brain::spearman(&p, &e));
        let mut o = v.clone(); o.sort_by(|a, b| b.0.total_cmp(&a.0));
        top.push(o[..3].iter().map(|x| x.1).sum::<f64>() / 3.0); bot.push(o[o.len() - 3..].iter().map(|x| x.1).sum::<f64>() / 3.0); exp.push(o[..3].iter().map(|x| x.2).sum::<f64>() / 3.0);
        sb.moments += 1; sb.calls += v.len();
    }
    let daily: BTreeMap<i64, f64> = per_day.into_iter().map(|(d, v)| (d, v.iter().sum::<f64>() / v.len() as f64)).collect();
    if !daily.is_empty() {
        let n = daily.len() as f64;
        sb.ic = daily.values().sum::<f64>() / n;
        sb.ic_se = if daily.len() >= 2 { (daily.values().map(|x| (x - sb.ic).powi(2)).sum::<f64>() / (n - 1.0)).sqrt() / n.sqrt() } else { f64::NAN };
    }
    let avg = |v: &Vec<f64>| if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 };
    sb.top_bps = avg(&top) * 1e4; sb.bottom_bps = avg(&bot) * 1e4; sb.expected_top_bps = avg(&exp);
    Ok(sb)
}

impl Scoreboard {
    pub fn line(&self) -> String {
        if self.moments == 0 { return format!("no resolved calls yet ({} waiting for their outcome)", self.pending); }
        format!("{} calls over {} rankings resolved ({} waiting): rank IC {:+.3} ({}), its top 3 realised {:+.1} bps vs the bottom 3 {:+.1} bps (the model expected {:+.1} bps for the top 3)", self.calls, self.moments, self.pending, self.ic, if self.ic_se.is_finite() { format!("se {:.3}", self.ic_se) } else { "se needs 2 sessions".into() }, self.top_bps, self.bottom_bps, self.expected_top_bps)
    }
}

// ------------------------------------------------------------------ registry
pub fn dir(root: &Path) -> PathBuf { root.join("model") }
pub fn save(root: &Path, a: &Artifact) -> Result<()> {
    std::fs::create_dir_all(dir(root))?;
    let p = dir(root).join(format!("v{}.json", a.version));
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(a)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}
pub fn load(root: &Path, version: u32) -> Result<Artifact> { Ok(serde_json::from_slice(&std::fs::read(dir(root).join(format!("v{version}.json")))?)?) }
pub fn versions(root: &Path) -> Vec<u32> {
    let mut v: Vec<u32> = std::fs::read_dir(dir(root)).into_iter().flatten().flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_prefix('v')).and_then(|n| n.strip_suffix(".json")).and_then(|n| n.parse().ok())).collect();
    v.sort();
    v
}
pub fn current(root: &Path) -> Option<u32> { std::fs::read_to_string(dir(root).join("current")).ok().and_then(|t| t.trim().parse().ok()) }
pub fn set_current(root: &Path, v: u32) -> Result<()> { std::fs::create_dir_all(dir(root))?; std::fs::write(dir(root).join("current"), format!("{v}\n"))?; Ok(()) }

// ------------------------------------------------------------------ monitoring
/// Per-session mean rank IC of each artifact on the sessions AFTER its training end (plus a 3-day purge): its real out-of-sample record.
pub fn forward_daily(series: &BTreeMap<String, Vec<Bar>>, arts: &[Artifact]) -> BTreeMap<u32, BTreeMap<i64, f64>> {
    let samples = universe_samples(series, true, 4);
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, s) in samples.iter().enumerate() { by.entry(s.ts).or_default().push(i); }
    let mut out = BTreeMap::new();
    for a in arts {
        let mut per: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
        for (ts, idx) in &by {
            if *ts <= a.trained_through_ms + 3 * DAY_MS || idx.len() < MIN_CROSS { continue; }
            let p: Vec<f64> = idx.iter().map(|&i| a.predict(&samples[i])).collect();
            let e: Vec<f64> = idx.iter().map(|&i| samples[i].excess).collect();
            per.entry(session(*ts)).or_default().push(crate::brain::spearman(&p, &e));
        }
        out.insert(a.version, per.into_iter().map(|(d, v)| (d, v.iter().sum::<f64>() / v.len() as f64)).collect());
    }
    out
}

/// (mean, standard error, sessions) of a series of daily values.
pub fn summarize(d: &BTreeMap<i64, f64>) -> Option<(f64, f64, usize)> {
    if d.len() < 5 { return None; }
    let n = d.len() as f64;
    let mean = d.values().sum::<f64>() / n;
    let sd = (d.values().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0)).sqrt();
    Some((mean, sd / n.sqrt(), d.len()))
}

/// Restore the older version when the newer one is significantly worse over the sessions both have seen (95%, at least 10 sessions).
pub fn should_roll_back(newer: &BTreeMap<i64, f64>, older: &BTreeMap<i64, f64>) -> bool {
    let diff: BTreeMap<i64, f64> = newer.iter().filter_map(|(d, x)| older.get(d).map(|y| (*d, x - y))).collect();
    summarize(&diff).is_some_and(|(mean, se, n)| n >= 10 && mean + 1.96 * se < 0.0)
}

/// The lifecycle step the steward runs: a new version every `RETRAIN_SESSIONS` sessions, forward records for the latest versions, and the rollback rule.
/// Returns report lines.
pub fn maintain(root: &Path, series: &BTreeMap<String, Vec<Bar>>, scheme: Scheme) -> Result<Vec<String>> {
    let mut out = vec![];
    let vs = versions(root);
    let newest_label_ms = m::build(series, 4).iter().map(|s| s.ts).max().unwrap_or(0);
    let last = vs.last().and_then(|v| load(root, *v).ok());
    if last.as_ref().is_none_or(|a| session(newest_label_ms) - session(a.trained_through_ms) >= RETRAIN_SESSIONS * 7 / 5) {
        let a = train(series, scheme, vs.last().map_or(1, |v| v + 1))?;
        out.push(format!("trained version {} on {} samples through {}: held-out IC {:+.4}, scheme {}", a.version, a.samples, crate::util::utc_minute(a.trained_through_ms / 1000), a.heldout_ic, a.scheme));
        save(root, &a)?;
        set_current(root, a.version)?;
    }
    let recent: Vec<Artifact> = versions(root).iter().rev().take(4).rev().filter_map(|v| load(root, *v).ok()).collect();
    let fwd = forward_daily(series, &recent);
    for a in &recent {
        if let Some((mean, se, n)) = fwd.get(&a.version).and_then(summarize) { out.push(format!("version {} live out-of-sample IC {mean:+.4} (se {se:.4}) over {n} sessions", a.version)); }
    }
    // rollback: the current version against its predecessor on the sessions both have seen
    if let (Some(cur), true) = (current(root), recent.len() >= 2) {
        if let Some(pos) = recent.iter().position(|a| a.version == cur) {
            if pos >= 1 {
                let (n, o) = (&fwd[&recent[pos].version], &fwd[&recent[pos - 1].version]);
                if should_roll_back(n, o) { set_current(root, recent[pos - 1].version)?; out.push(format!("ROLLED BACK to version {}: version {cur} was significantly worse over the same sessions", recent[pos - 1].version)); crate::ops::discord("Model rolled back", &format!("version {cur} was significantly worse than version {} over the same out-of-sample sessions; version {} is current again", recent[pos - 1].version, recent[pos - 1].version)); }
            }
        }
    }
    let summary: BTreeMap<String, serde_json::Value> = fwd.iter().map(|(v, d)| (format!("v{v}"), serde_json::json!({ "sessions": d.len(), "summary": summarize(d).map(|x| [x.0, x.1]) }))).collect();
    std::fs::write(dir(root).join("monitor.json"), serde_json::to_vec_pretty(&summary)?)?;
    Ok(out)
}

/// The model card as markdown, from the current artifact and the monitor file.
pub fn card(root: &Path) -> Result<String> {
    let v = current(root).ok_or_else(|| anyhow!("no model trained yet: qwen-lab model train"))?;
    let a = load(root, v)?;
    let mut s = format!("# Model card: version {} of {} (current)\n\n", a.version, versions(root).len());
    s += &format!("* What it is: a ridge regression (own solver) predicting each stock's return over the next session minus the average of the bot's top-20 liquid SPY holdings, from {} features: the lab's 23 candle features, the bot's own screener score components, SPY-trend distances and four SPY-regime interactions.\n", a.features.len());
    s += &format!("* Trained on {} samples through {}, scheme: {}. Strength chosen on held-out data. Held-out IC of the calibration split: {:+.4}.\n", a.samples, crate::util::utc_minute(a.trained_through_ms / 1000), a.scheme, a.heldout_ic);
    s += "* Output: per stock a score, a percentile in the top-20, a calibrated expected excess return and hit rate (table below) and the three features that drove it.\n\n| percentile | expected excess (bps, one session) | hit rate | n |\n|---|---|---|---|\n";
    for b in &a.calibration { s += &format!("| {:.0}-{:.0} | {:+.1} | {:.0}% | {} |\n", b.pct_lo, b.pct_hi, b.mean_bps, b.hit * 100.0, b.n); }
    if let Ok(t) = std::fs::read_to_string(dir(root).join("monitor.json")) { s += &format!("\n## Live monitoring (out of sample, per version)\n```json\n{t}\n```\n"); }
    s += "\n## Limits\nOne bull-leaning window (2026), 80 stocks, a small linear signal: the calibration shows the SIZE of the effect, which is a few tens of bps per session before costs and not proven after them (docs/BRAIN.md). The model never trades; it is research only.\n";
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(n: usize, bars: usize, seed: u64) -> BTreeMap<String, Vec<Bar>> {
        let mut st = seed | 1;
        let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let start = 1_775_000_000_000i64 / DAY_MS * DAY_MS + 13 * 3_600_000 + 30 * 60_000;
        let common: Vec<f64> = (0..bars).map(|_| rnd() * 0.003).collect();
        (0..n).map(|s| {
            let mut p = 40.0 + s as f64; let mut prev = 0.0;
            let v: Vec<Bar> = (0..bars).map(|i| {
                let day = i / 26; let ts = start + ((day / 5) as i64 * 7 + (day % 5) as i64) * DAY_MS + (i % 26) as i64 * 900_000;
                let o = p; let r = common[i] + rnd() * 0.004 - 0.2 * prev; prev = r; p *= 1.0 + r;
                Bar { ts, open: o, high: o.max(p) * 1.001, low: o.min(p) * 0.999, close: p, volume: 1000.0 * (1.0 + (s % 7) as f64) * (1.0 + rnd() + 0.6) }
            }).collect();
            (if s == 0 { "SPY".to_string() } else { format!("S{s:02}") }, v)
        }).collect()
    }

    #[test]
    fn the_calibration_table_reports_the_realised_return_by_percentile() {
        // 30 names x 60 moments; the prediction equals the truth, so higher percentiles must have higher realised returns and hit rates
        let mut test = vec![]; let mut pred = vec![];
        for t in 0..60 { for k in 0..30 { let e = (k as f64 - 14.5) / 1000.0; test.push(Sample { ts: t * 3_600_000, week: 0, sym: k, x: vec![], excess: e, raw: e }); pred.push(e); } }
        let c = calibrate(&test, &pred);
        assert_eq!(c.len(), 10);
        assert!(c.windows(2).all(|w| w[1].mean_bps > w[0].mean_bps && w[1].hit >= w[0].hit), "{c:?}");
        assert!(c[0].mean_bps < 0.0 && c[9].mean_bps > 0.0 && c[9].hit == 1.0 && c[0].hit == 0.0);
    }

    #[test]
    fn isotonic_calibration_is_monotone_and_keeps_an_already_monotone_table() {
        let mk = |m: &[f64]| -> Vec<Bin> { m.iter().enumerate().map(|(i, x)| Bin { pct_lo: i as f64 * 10.0, pct_hi: i as f64 * 10.0 + 10.0, mean_bps: *x, hit: 0.5 + x / 1000.0, n: 100 }).collect() };
        let mut noisy = mk(&[20.0, -18.0, -9.0, 3.0, 11.0, 18.0, 22.0, 21.0, -16.0, 29.0]);
        isotonic(&mut noisy);
        assert!(noisy.windows(2).all(|w| w[1].mean_bps >= w[0].mean_bps - 1e-9 && w[1].hit >= w[0].hit - 1e-9), "{noisy:?}");
        let total: f64 = [20.0, -18.0, -9.0, 3.0, 11.0, 18.0, 22.0, 21.0, -16.0, 29.0].iter().sum();
        assert!((noisy.iter().map(|b| b.mean_bps).sum::<f64>() - total).abs() < 1e-6, "pooling keeps the total");
        let mut fine = mk(&[-10.0, -5.0, 0.0, 4.0, 9.0]);
        let before: Vec<f64> = fine.iter().map(|b| b.mean_bps).collect();
        isotonic(&mut fine);
        assert_eq!(fine.iter().map(|b| b.mean_bps).collect::<Vec<_>>(), before);
    }

    #[test]
    fn every_live_call_is_stored_then_scored_against_the_real_outcome() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        let day = DAY_MS; let t0 = 1_775_000_000_000i64 / day * day + 14 * 3_600_000;
        // 20 stocks over 60 bars; stock k drifts by (k - 9.5) bp per bar, so the stock number IS the right ranking
        let series: BTreeMap<String, Vec<Bar>> = (0..20).map(|k| (format!("S{k:02}"), (0..60).map(|i| { let c = 100.0 * (1.0 + (k as f64 - 9.5) * 1e-4).powi(i); Bar { ts: t0 + i as i64 * 900_000, open: c, high: c, low: c, close: c, volume: 1.0 } }).collect())).collect();
        let picks = |good: bool| -> Vec<Pick> { (0..20).map(|k| Pick { symbol: format!("S{k:02}"), score: if good { k as f64 } else { -(k as f64) }, pct: 0.0, expected_bps: 5.0, hit: 0.5, why: vec![] }).collect() };
        let ts = t0 + 10 * 900_000;
        assert_eq!(record_predictions(&db, 1, ts, &picks(true)).unwrap(), 20);
        assert_eq!(record_predictions(&db, 1, ts, &picks(true)).unwrap(), 0, "the same call is stored once");
        assert_eq!(record_predictions(&db, 2, ts, &picks(false)).unwrap(), 20);
        assert_eq!(scoreboard(&db, Some(1)).unwrap().moments, 0, "nothing resolved yet");
        // an outcome that has not arrived yet (the bar 26 later does not exist) stays pending
        record_predictions(&db, 1, t0 + 50 * 900_000, &picks(true)).unwrap();
        assert_eq!(resolve_predictions(&db, &series).unwrap(), 40, "only the two moments with complete outcomes");
        let good = scoreboard(&db, Some(1)).unwrap();
        let bad = scoreboard(&db, Some(2)).unwrap();
        assert!(good.ic > 0.99 && good.top_bps > 0.0 && good.bottom_bps < 0.0 && good.pending == 20, "{good:?}");
        assert!(bad.ic < -0.99 && bad.top_bps < 0.0, "the wrong ranking is scored as wrong: {bad:?}");
        assert!(good.line().contains("rank IC") && bad.moments == 1);
    }

    #[test]
    fn zscore_standardises_and_handles_the_empty_case() {
        let z = zscore(&[1.0, 2.0, 3.0, 4.0]);
        assert!(z.iter().sum::<f64>().abs() < 1e-12 && (z.iter().map(|x| x * x).sum::<f64>() / 4.0 - 1.0).abs() < 1e-9);
        assert!(zscore(&[]).is_empty());
        assert!(zscore(&[5.0, 5.0]).iter().all(|x| *x == 0.0), "a constant has no spread: all zeros, no NaN");
    }

    #[test]
    fn the_rollback_rule_needs_enough_sessions_and_a_significant_loss() {
        let mk = |f: &dyn Fn(i64) -> f64, n: i64| -> BTreeMap<i64, f64> { (0..n).map(|d| (d, f(d))).collect() };
        let older = mk(&|d| 0.05 + 0.01 * ((d % 3) as f64 - 1.0), 20);
        let worse = mk(&|d| -0.05 + 0.01 * ((d % 4) as f64 - 1.5), 20);
        let similar = mk(&|d| 0.05 + 0.01 * ((d % 5) as f64 - 2.0), 20);
        assert!(should_roll_back(&worse, &older), "clearly worse over 20 common sessions");
        assert!(!should_roll_back(&similar, &older), "no significant difference");
        assert!(!should_roll_back(&mk(&|_| -0.5, 6), &mk(&|_| 0.5, 6)), "fewer than 10 sessions: no decision");
        assert!(summarize(&mk(&|_| 0.1, 4)).is_none() && summarize(&older).is_some());
    }

    #[test]
    fn the_registry_saves_loads_lists_and_points_at_the_current_version() {
        let d = tempfile::tempdir().unwrap();
        let mk = |v: u32| Artifact { version: v, created: 1, trained_through_ms: 2, samples: 3, scheme: "all older history".into(), features: vec!["a".into()], ridge: Ridge::fit(&(0..50).map(|i| Sample { ts: i, week: 0, sym: 0, x: vec![i as f64, (i * i % 7) as f64], excess: i as f64 * 0.01, raw: 0.0 }).collect::<Vec<_>>(), 1.0), calibration: vec![], heldout_ic: 0.1 };
        assert!(versions(d.path()).is_empty() && current(d.path()).is_none());
        save(d.path(), &mk(1)).unwrap(); save(d.path(), &mk(2)).unwrap();
        set_current(d.path(), 2).unwrap();
        assert_eq!((versions(d.path()), current(d.path())), (vec![1, 2], Some(2)));
        let a = load(d.path(), 2).unwrap();
        let probe = Sample { ts: 0, week: 0, sym: 0, x: vec![10.0, 3.0], excess: 0.0, raw: 0.0 };
        assert_eq!(a.ridge.predict(&probe), mk(2).ridge.predict(&probe), "the saved model predicts exactly what the trained one does");
        assert_eq!(a.ridge.contributions(&probe).len(), 2);
    }

    #[test]
    fn it_trains_saves_and_ranks_the_live_top_twenty_with_explanations() {
        let series = market(34, 26 * 60, 5);
        let d = tempfile::tempdir().unwrap();
        let a = train(&series, Scheme::default(), 1).unwrap();
        assert_eq!((a.version, a.features.len(), a.calibration.len()), (1, feature_names().len(), 10));
        assert!(a.heldout_ic.is_finite() && a.samples > 5000);
        save(d.path(), &a).unwrap();
        let back = load(d.path(), 1).unwrap();
        let (ts, picks) = predict_live(&series, &back, &[]).expect("a live ranking");
        let newest = series.values().next().unwrap().last().unwrap().ts;
        assert!(ts <= newest && ts > newest - 2 * 3_600_000, "the ranking is for one of the newest bars");
        assert_eq!(picks.len(), mlbot::UNIVERSE);
        assert!(picks.windows(2).all(|w| w[0].score >= w[1].score && w[0].pct >= w[1].pct), "best first");
        assert!((picks[0].pct - 100.0).abs() < 1e-9 && picks[19].pct == 0.0);
        assert!(picks.iter().all(|p| p.why.len() == 3 && p.why.iter().all(|w| w.1.is_finite()) && p.expected_bps.is_finite() && (0.0..=1.0).contains(&p.hit)));
        assert!(picks.iter().all(|p| p.symbol != "SPY"), "the reference index is never ranked");
    }

    #[test]
    fn a_bag_with_mixed_shapes_is_filtered_to_the_first_members_genome_and_never_panics() {
        let series = market(34, 26 * 60, 5);
        let a = train(&series, Scheme::default(), 1).unwrap();
        let samples = universe_samples(&series, true, 4);
        let mk = |seq_len: usize| {
            let rows = crate::deepnet::build_rows(&series, &samples, seq_len);
            let refs: Vec<&crate::deepnet::Row> = rows.iter().collect();
            let g = crate::deepnet::Genome { kind: crate::deepnet::Kind::Res, width: 16, blocks: 1, heads: 1, dropout: 0.0, lr: 0.01, weight_decay: 0.0, epochs: 1, seq_len, moments_per_batch: 4, mse_weight: 0.1 };
            crate::deepnet::train(&refs, &g, 1, 1)
        };
        let (m8, m16) = (mk(8), mk(16));
        let alone = predict_live(&series, &a, std::slice::from_ref(&m8)).unwrap().1;
        let mixed = predict_live(&series, &a, &[m8.clone(), m16]).unwrap().1;
        assert_eq!(alone.iter().map(|p| p.score).collect::<Vec<_>>(), mixed.iter().map(|p| p.score).collect::<Vec<_>>(), "the other-shaped member is ignored");
        assert_eq!(predict_live(&series, &a, &[]).unwrap().1.len(), 20);
    }

    #[test]
    fn the_lifecycle_trains_once_then_waits_for_new_data_and_writes_the_monitor_file() {
        let series = market(34, 26 * 70, 8);
        let d = tempfile::tempdir().unwrap();
        let first = maintain(d.path(), &series, Scheme::default()).unwrap();
        assert!(first.iter().any(|l| l.contains("trained version 1")), "{first:?}");
        assert_eq!((versions(d.path()), current(d.path())), (vec![1], Some(1)));
        let again = maintain(d.path(), &series, Scheme::default()).unwrap();
        assert!(!again.iter().any(|l| l.contains("trained version")), "no new version until a new session of labelled data exists: {again:?}");
        assert!(dir(d.path()).join("monitor.json").exists());
        assert!(card(d.path()).unwrap().contains("Model card: version 1"));
    }
}

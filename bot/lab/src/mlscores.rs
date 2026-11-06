//! The candle model as a signal for the BOT'S OWN backtester (owner 2026-10-13: "does it work like the bot back tester??? it should").
//! Writes, for every 15-minute bar, the model's score for the next session's relative return, in the file the bot's research engine reads (`Signal::Ml`,
//! `variant_study ... sig:Ml`). The bot's engine then does everything else itself: top-20 universe, trend filter, entries, exits, sizing, its measured fees
//! and spreads, 14-session walk-forward blocks and the per-block parameter search.
//!
//! The features are the lab's 23 plus the bot's own score components (`mlbot.rs`); training and scoring cover the bot's top-20 liquid universe only.
//! Causality: the model is refitted at the start of every 14-session block on samples that ended at least `PURGE` sessions earlier (their one-session labels
//! are then complete), with the ridge strength chosen on the last quarter of that training period. Every score in a block comes from that one model.

use crate::candles::Bar;
use crate::mlcandle::{Sample, DAY_MS};
use std::collections::BTreeMap;

pub const BLOCK: usize = 14;
pub const PURGE: usize = 2;
/// Scores start once this many sessions of history exist.
pub const FIRST: usize = 40;

fn session_of(ts: i64) -> i64 { (ts + crate::mlcandle::et_offset_ms(ts)).div_euclid(DAY_MS) }

/// How the model is trained: the whole older history (None, None), a rolling window of days, or exponential recency weights (half-life in days). Chosen by the
/// improvement round's ledger (an ADOPTED scheme), never by hand.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Scheme { pub window_days: Option<i64>, pub half_life_days: Option<f64> }

impl Scheme {
    /// Parses a scheme name written by the improvement round ("rolling 126 days", "half-life 42 days"); anything else (expanding, bags) is the default.
    pub fn parse(name: &str) -> Scheme {
        let num = |p: &str| name.strip_prefix(p).and_then(|r| r.split_whitespace().next()).and_then(|x| x.parse::<f64>().ok());
        if let Some(d) = num("rolling ") { Scheme { window_days: Some(d as i64), ..Scheme::default() } }
        else if let Some(h) = num("half-life ") { Scheme { half_life_days: Some(h), ..Scheme::default() } }
        else { Scheme::default() }
    }
    /// The best ADOPTED scheme in the trial ledger (highest IC), or the default.
    pub fn from_ledger(db: &rusqlite::Connection) -> Scheme {
        crate::mlimprove::init(db).ok();
        db.query_row("SELECT name FROM ml_trials WHERE adopted = 1 AND family IN ('scheme', 'explore') ORDER BY value DESC LIMIT 1", [], |r| r.get::<_, String>(0)).map(|n| Scheme::parse(&n)).unwrap_or_default()
    }
}

/// Ridge with the strength chosen on the last quarter of the training period, optionally recency-weighted / windowed.
pub fn tuned_ridge(train: &[Sample], end: i64, window_days: Option<i64>, half_life_days: Option<f64>) -> crate::mlcandle::Ridge {
    use crate::mlcandle::Ridge;
    let tr: Vec<Sample> = train.iter().filter(|s| window_days.is_none_or(|w| s.ts > end - w * DAY_MS)).cloned().collect();
    let w = move |s: &Sample| half_life_days.map_or(1.0, |h| 0.5f64.powf((end - s.ts) as f64 / DAY_MS as f64 / h));
    let mut ts: Vec<i64> = tr.iter().map(|s| s.ts).collect(); ts.sort(); ts.dedup();
    if ts.len() < 8 { return Ridge::fit_w(&tr, 100.0, &w); }
    let cut = ts[ts.len() * 3 / 4];
    let (fit, val): (Vec<Sample>, Vec<Sample>) = (tr.iter().filter(|s| s.ts < cut - 3 * DAY_MS).cloned().collect(), tr.iter().filter(|s| s.ts >= cut).cloned().collect());
    if fit.len() < 2000 || val.len() < 500 { return Ridge::fit_w(&tr, 100.0, &w); }
    let mut best = (f64::MIN, 100.0);
    for a in [10.0, 100.0, 1000.0, 10_000.0, 100_000.0] {
        let r = Ridge::fit_w(&fit, a, &w);
        let ic = crate::mlcandle::evaluate(&val, &val.iter().map(|s| r.predict(s)).collect::<Vec<_>>(), 5).ic;
        if ic > best.0 { best = (ic, a); }
    }
    Ridge::fit_w(&tr, best.1, &w)
}

/// Scores for every sample of `all` (same order); None before the first fit.
pub fn score_samples(all: &[Sample], member: &dyn Fn(&Sample) -> bool, scheme: Scheme) -> Vec<Option<f64>> {
    let mut sessions: Vec<i64> = all.iter().map(|s| session_of(s.ts)).collect(); sessions.sort(); sessions.dedup();
    let pos: BTreeMap<i64, usize> = sessions.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let sp: Vec<usize> = all.iter().map(|s| pos[&session_of(s.ts)]).collect();
    let mut out = vec![None; all.len()];
    let mut start = FIRST;
    while start < sessions.len() {
        let end = (start + BLOCK).min(sessions.len());
        let train: Vec<Sample> = all.iter().zip(&sp).filter(|(s, p)| **p + PURGE < start && member(s)).map(|(s, _)| s.clone()).collect();
        if train.len() >= 3000 {
            let tend = train.iter().map(|s| s.ts).max().unwrap_or(0);
            let r = tuned_ridge(&crate::mlbot::interactions(&train), tend, scheme.window_days, scheme.half_life_days);
            let idx: Vec<usize> = (0..all.len()).filter(|&i| sp[i] >= start && sp[i] < end && member(&all[i])).collect();
            let block: Vec<Sample> = crate::mlbot::interactions(&idx.iter().map(|&i| all[i].clone()).collect::<Vec<_>>());
            for (k, &i) in idx.iter().enumerate() { out[i] = Some(r.predict(&block[k])); }
        }
        start = end;
    }
    out
}

/// `{"SYMBOL": [[bar open ms, score], ...]}` for the bot's engine.
pub fn table(series: &BTreeMap<String, Vec<Bar>>, scheme: Scheme) -> BTreeMap<String, Vec<(i64, f64)>> {
    let names: Vec<&String> = series.keys().collect();
    let nb = crate::mlcandle::FEATURES.len();
    let all = crate::mlbot::extend(series, &crate::mlcandle::build(series, 1));
    let sc = score_samples(&all, &|s: &Sample| s.x[nb + 5] > 0.5, scheme); // the bot ranks only its top-20 universe: train and score those
    let mut out: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
    for (s, v) in all.iter().zip(sc) { if let Some(v) = v { out.entry(names[s.sym].clone()).or_default().push((s.ts, v)); } }
    out
}

pub fn write(series: &BTreeMap<String, Vec<Bar>>, path: &std::path::Path, scheme: Scheme) -> anyhow::Result<(usize, usize)> {
    let t = table(series, scheme);
    let n = t.values().map(|v| v.len()).sum();
    let rounded: BTreeMap<&String, Vec<(i64, f64)>> = t.iter().map(|(k, v)| (k, v.iter().map(|(a, b)| (*a, (*b * 1e7).round() / 1e7)).collect())).collect();
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    // written next to the target and renamed, so a study reading the file never sees a half-written one
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&rounded)?)?;
    std::fs::rename(&tmp, path)?;
    Ok((t.len(), n))
}

/// Where the model ranks `symbols` at the newest scored bar in the score file: (bar open ms, [(symbol, percentile 0-100 among all scored symbols at that bar)]).
/// None when the file is missing or empty. This is what the bot's backtester reads, so it is the model's actual view.
pub fn latest_view(path: &std::path::Path, symbols: &[String]) -> Option<(i64, Vec<(String, f64)>)> {
    let t: BTreeMap<String, Vec<(i64, f64)>> = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let ts = t.values().filter_map(|v| v.last().map(|x| x.0)).max()?;
    let at: BTreeMap<&String, f64> = t.iter().filter_map(|(k, v)| v.iter().rev().find(|x| x.0 == ts).map(|x| (k, x.1))).collect();
    if at.len() < 5 { return None; }
    let vals: Vec<f64> = at.values().copied().collect();
    let pct = |x: f64| 100.0 * vals.iter().filter(|v| **v < x).count() as f64 / vals.len() as f64;
    Some((ts, symbols.iter().filter_map(|s| at.get(s).map(|x| (s.clone(), pct(*x)))).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: i64, x: f64, y: f64, sym: usize) -> Sample { Sample { ts, week: ts / (7 * DAY_MS), sym, x: vec![x; crate::mlcandle::FEATURES.len()], excess: y, raw: y } }

    /// 70 trading days (Mon-Fri), 30 names, one moment per day at 15:00 UTC; the label is -x (a clean reversal).
    fn data(flip_after: Option<usize>) -> Vec<Sample> {
        let mut out = vec![];
        let mut st = 7u64;
        let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let base = 1_775_000_000_000i64 / DAY_MS * DAY_MS + 15 * 3_600_000;
        let mut day = 0usize;
        for d in 0..100 {
            if d % 7 >= 5 { continue; }
            for k in 0..60 { for s in 0..30 {
                let x = rnd();
                let y = if flip_after.is_some_and(|f| day >= f) { 9.0 } else { -x };
                out.push(sample(base + d as i64 * DAY_MS + k * 900_000, x, y, s));
            } }
            day += 1;
        }
        out
    }

    #[test]
    fn the_latest_view_ranks_symbols_at_the_newest_scored_bar() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.json");
        let mut t: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
        for (i, s) in ["A", "B", "C", "D", "E", "F"].iter().enumerate() { t.insert(s.to_string(), vec![(100, 9.0), (200, i as f64)]); }
        t.insert("OLD".into(), vec![(100, 1.0)]); // not scored at the newest bar
        std::fs::write(&p, serde_json::to_vec(&t).unwrap()).unwrap();
        let (ts, v) = latest_view(&p, &["F".into(), "A".into(), "OLD".into(), "ZZZ".into()]).unwrap();
        assert_eq!(ts, 200);
        assert_eq!(v.len(), 2, "unscored or unknown symbols are left out");
        assert!((v[0].1 - 100.0 * 5.0 / 6.0).abs() < 1e-9 && v[1].1 == 0.0, "{v:?}");
        assert!(latest_view(&d.path().join("missing.json"), &[]).is_none());
    }

    #[test]
    fn schemes_parse_from_ledger_names_and_the_best_adopted_one_is_used() {
        assert_eq!(Scheme::parse("rolling 126 days"), Scheme { window_days: Some(126), ..Scheme::default() });
        assert_eq!(Scheme::parse("half-life 42 days"), Scheme { half_life_days: Some(42.0), ..Scheme::default() });
        assert_eq!(Scheme::parse("bag: expanding+126d+hl42"), Scheme::default());
        let db = rusqlite::Connection::open_in_memory().unwrap();
        assert_eq!(Scheme::from_ledger(&db), Scheme::default(), "nothing adopted: the default");
        crate::mlimprove::record(&db, "scheme", "rolling 85 days", "ic", 0.05, 0.0, 0.1, true).unwrap();
        crate::mlimprove::record(&db, "explore", "half-life 63 days", "ic", 0.09, 0.0, 0.1, true).unwrap();
        crate::mlimprove::record(&db, "scheme", "rolling 126 days", "ic", 0.20, 0.0, 0.1, false).unwrap();
        assert_eq!(Scheme::from_ledger(&db), Scheme { half_life_days: Some(63.0), ..Scheme::default() }, "the best ADOPTED one; an unadopted higher IC does not count");
    }

    #[test]
    fn scores_start_after_the_warm_up_and_follow_a_planted_signal() {
        let all = data(None);
        let sc = score_samples(&all, &|_| true, Scheme::default());
        let mut sessions: Vec<i64> = all.iter().map(|s| session_of(s.ts)).collect(); sessions.sort(); sessions.dedup();
        assert!(sc.iter().zip(&all).filter(|(_, s)| session_of(s.ts) < sessions[FIRST]).all(|(v, _)| v.is_none()));
        let scored: Vec<(f64, f64)> = sc.iter().zip(&all).filter_map(|(v, s)| v.map(|v| (v, s.excess))).collect();
        assert!(scored.len() > 5000);
        let ic = crate::brain::spearman(&scored.iter().map(|x| x.0).collect::<Vec<_>>(), &scored.iter().map(|x| x.1).collect::<Vec<_>>());
        assert!(ic > 0.9, "the score must follow the planted reversal: {ic}");
    }

    #[test]
    fn a_score_never_depends_on_labels_at_or_after_its_block() {
        let clean = data(None);
        let sc = score_samples(&clean, &|_| true, Scheme::default());
        let mut sessions: Vec<i64> = clean.iter().map(|s| session_of(s.ts)).collect(); sessions.sort(); sessions.dedup();
        let cut_session = sessions[FIRST + BLOCK]; // the second block starts here
        let day_of_cut = sessions.iter().position(|s| *s == cut_session).unwrap();
        let spoiled = data(Some(day_of_cut - PURGE)); // labels garbled from 2 sessions before the second block
        let sc2 = score_samples(&spoiled, &|_| true, Scheme::default());
        for (i, s) in clean.iter().enumerate() {
            if session_of(s.ts) < cut_session { assert_eq!(sc[i], sc2[i], "an earlier block's score changed when only later labels changed"); }
        }
        assert!(clean.iter().enumerate().any(|(i, s)| session_of(s.ts) >= cut_session && sc[i] != sc2[i]), "the later block does use the garbled labels, so the test can fail");
    }
}

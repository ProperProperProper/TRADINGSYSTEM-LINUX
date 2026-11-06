//! Stricter statistics for improving the candle model (owner 2026-10-13, "implement 2, 3, 4, 5, 6"):
//!   * a TRIAL LEDGER: every model, feature or scheme tried is recorded, and every interval is widened for the number of trials so far
//!     (Bonferroni: the tail probability is 0.025 / trials), because the more things are tried the more likely one looks good by luck;
//!   * PAIRED comparisons: a change is judged by the week-by-week difference against the model it modifies on the SAME test windows
//!     (much tighter than comparing two separate intervals);
//!   * a change is only ADOPTED when the paired difference is above zero at the adjusted level.

use crate::mlcandle::{Sample, MIN_CROSS};
use rusqlite::{params, Connection};
use std::collections::BTreeMap;

pub fn init(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ml_trials (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, family TEXT NOT NULL, name TEXT NOT NULL, metric TEXT NOT NULL,
        value REAL NOT NULL, lo REAL NOT NULL, hi REAL NOT NULL, adopted INTEGER NOT NULL DEFAULT 0, UNIQUE(family, name, metric));")
}

/// Records (or updates) one trial. A trial counts once however many times it is re-run.
pub fn record(db: &Connection, family: &str, name: &str, metric: &str, value: f64, lo: f64, hi: f64, adopted: bool) -> rusqlite::Result<()> {
    init(db)?;
    db.execute("INSERT INTO ml_trials(ts, family, name, metric, value, lo, hi, adopted) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(family, name, metric) DO UPDATE SET ts = excluded.ts, value = excluded.value, lo = excluded.lo, hi = excluded.hi, adopted = excluded.adopted",
        params![crate::util::now_ts(), family, name, metric, value, lo, hi, adopted as i64])?;
    Ok(())
}

pub fn count(db: &Connection) -> usize { init(db).ok(); db.query_row("SELECT count(*) FROM ml_trials", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize }

/// Per-week sums of the per-moment rank IC and top-minus-bottom-fifth spread: (ic sum, spread sum, moments).
pub fn weekly(test: &[Sample], pred: &[f64]) -> BTreeMap<i64, (f64, f64, usize)> {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in test.iter().enumerate() { by.entry(x.ts).or_default().push(i); }
    let mut weeks: BTreeMap<i64, (f64, f64, usize)> = BTreeMap::new();
    for idx in by.values().filter(|v| v.len() >= MIN_CROSS) {
        let p: Vec<f64> = idx.iter().map(|&i| pred[i]).collect();
        let e: Vec<f64> = idx.iter().map(|&i| test[i].excess).collect();
        let ic = crate::brain::spearman(&p, &e);
        let mut o: Vec<usize> = (0..idx.len()).collect();
        o.sort_by(|&a, &b| p[b].total_cmp(&p[a]));
        let k = (idx.len() / 5).max(1);
        let spread = o[..k].iter().map(|&i| e[i]).sum::<f64>() / k as f64 - o[o.len() - k..].iter().map(|&i| e[i]).sum::<f64>() / k as f64;
        let w = weeks.entry(test[idx[0]].week).or_default();
        w.0 += ic; w.1 += spread; w.2 += 1;
    }
    weeks
}

/// Interval of a statistic by resampling whole weeks. `tail` is the probability in each tail (0.025 = 95%). Returns (value, lo, hi).
pub fn interval(stat: &[(f64, usize)], tail: f64, seed: u64) -> (f64, f64, f64) {
    let pooled = |sel: &mut dyn Iterator<Item = usize>| { let (s, n) = sel.fold((0.0, 0usize), |a, i| (a.0 + stat[i].0, a.1 + stat[i].1)); if n == 0 { 0.0 } else { s / n as f64 } };
    let value = pooled(&mut (0..stat.len()));
    if stat.is_empty() { return (0.0, 0.0, 0.0); }
    let draws = ((20.0 / tail.max(1e-6)) as usize).clamp(4000, 200_000);
    let mut st = seed | 1;
    let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; st };
    let mut v: Vec<f64> = (0..draws).map(|_| pooled(&mut (0..stat.len()).map(|_| (rnd() % stat.len() as u64) as usize))).collect();
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v[((q * draws as f64) as usize).min(draws - 1)];
    (value, at(tail), at(1.0 - tail))
}

fn pick(w: &BTreeMap<i64, (f64, f64, usize)>, spread: bool) -> Vec<(f64, usize)> { w.values().map(|x| (if spread { x.1 } else { x.0 }, x.2)).collect() }

/// IC and spread intervals at the ADJUSTED level for `trials` trials so far: (ic, lo, hi, spread bps, lo, hi).
pub fn adjusted(test: &[Sample], pred: &[f64], trials: usize, seed: u64) -> [f64; 6] {
    let w = weekly(test, pred);
    let tail = 0.025 / trials.max(1) as f64;
    let (a, b, c) = interval(&pick(&w, false), tail, seed);
    let (d, e, f) = interval(&pick(&w, true), tail, seed + 1);
    [a, b, c, d * 1e4, e * 1e4, f * 1e4]
}

/// Paired difference (new - base) of the weekly IC on the same test set, at the adjusted level: (diff, lo, hi). Moments are matched by position, so both
/// predictions must be for the same `test`.
pub fn paired_ic(test: &[Sample], base: &[f64], new: &[f64], trials: usize, seed: u64) -> (f64, f64, f64) {
    let (wa, wb) = (weekly(test, base), weekly(test, new));
    let diff: Vec<(f64, usize)> = wa.iter().filter_map(|(k, a)| wb.get(k).map(|b| (b.0 - a.0, a.2.min(b.2)))).collect();
    interval(&diff, 0.025 / trials.max(1) as f64, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ts: i64, week: i64, e: f64) -> Sample { Sample { ts, week, sym: 0, x: vec![], excess: e, raw: e } }

    /// 30 weeks, 2 moments each, 20 names: prediction = truth + noise of the given size.
    fn world(noise: f64, seed: u64) -> (Vec<Sample>, Vec<f64>) {
        let mut st = seed | 1;
        let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let (mut t, mut p) = (vec![], vec![]);
        for w in 0..30 { for m in 0..2 { for _ in 0..20 { let e = rnd(); t.push(s(w * 7 * 86_400_000 + m * 3_600_000, w, e)); p.push(e + noise * rnd()); } } }
        (t, p)
    }

    #[test]
    fn the_adjusted_interval_widens_with_the_number_of_trials_and_the_value_is_unchanged() {
        let (t, p) = world(3.0, 5);
        let one = adjusted(&t, &p, 1, 7);
        let many = adjusted(&t, &p, 40, 7);
        assert!((one[0] - many[0]).abs() < 1e-12);
        assert!(many[1] < one[1] && many[2] > one[2], "{one:?} {many:?}");
    }

    #[test]
    fn a_real_improvement_is_found_paired_and_noise_is_not() {
        let (t, base) = world(6.0, 3);
        // the "new" model is the base plus a bit of the truth: really better
        let better: Vec<f64> = base.iter().zip(&t).map(|(b, s)| b + 3.0 * s.excess).collect();
        let (d, lo, _) = paired_ic(&t, &base, &better, 20, 1);
        assert!(d > 0.0 && lo > 0.0, "a real gain must be above zero even at 20 trials: {d} {lo}");
        // the "new" model is the base plus unrelated noise: not better
        let mut st = 99u64; let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let noisy: Vec<f64> = base.iter().map(|b| b + 0.5 * rnd()).collect();
        let (_, lo2, hi2) = paired_ic(&t, &base, &noisy, 20, 1);
        assert!(lo2 < 0.0 && hi2 > 0.0, "an irrelevant change must straddle zero: {lo2} {hi2}");
    }

    #[test]
    fn the_ledger_counts_each_trial_once() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(count(&db), 0);
        record(&db, "feature", "gap", "paired_ic", 0.01, -0.01, 0.03, false).unwrap();
        record(&db, "feature", "gap", "paired_ic", 0.02, -0.01, 0.04, false).unwrap();
        record(&db, "feature", "peer", "paired_ic", 0.00, -0.02, 0.02, false).unwrap();
        assert_eq!(count(&db), 2);
        let v: f64 = db.query_row("SELECT value FROM ml_trials WHERE name = 'gap'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 0.02, "a re-run updates the same trial");
    }
}

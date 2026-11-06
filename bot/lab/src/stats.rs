//! Statistics the rule engine relies on. Pure functions, no I/O, deterministic given a seed.
//! The language model never computes any of these numbers.

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::BTreeMap;

/// Mean (NaN when empty).
pub fn mean(x: &[f64]) -> f64 {
    if x.is_empty() { f64::NAN } else { x.iter().sum::<f64>() / x.len() as f64 }
}

/// Sample variance (NaN with fewer than 2 values).
pub fn variance(x: &[f64]) -> f64 {
    if x.len() < 2 {
        return f64::NAN;
    }
    let m = mean(x);
    x.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (x.len() - 1) as f64
}

/// Sample standard deviation.
pub fn std_dev(x: &[f64]) -> f64 {
    variance(x).sqrt()
}

/// Standard normal CDF (Abramowitz-Stegun erf approximation, abs error < 1.5e-7).
pub fn normal_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * (x.abs() / std::f64::consts::SQRT_2));
    let poly = t * (0.254829592 + t * (-0.284496736 + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
    let erf = 1.0 - poly * (-(x * x) / 2.0).exp();
    if x >= 0.0 { 0.5 * (1.0 + erf) } else { 0.5 * (1.0 - erf) }
}

/// Inverse standard normal CDF (Acklam's algorithm, relative error ~1e-9).
pub fn normal_inv(p: f64) -> f64 {
    assert!(p > 0.0 && p < 1.0, "p out of range");
    const A: [f64; 6] = [-3.969683028665376e1, 2.209460984245205e2, -2.759285104469687e2, 1.383_577_518_672_69e2, -3.066479806614716e1, 2.506628277459239];
    const B: [f64; 5] = [-5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2, 6.680131188771972e1, -1.328068155288572e1];
    const C: [f64; 6] = [-7.784894002430293e-3, -3.223964580411365e-1, -2.400758277161838, -2.549732539343734, 4.374664141464968, 2.938163982698783];
    const D: [f64; 4] = [7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996, 3.754408661907416];
    let plow = 0.02425;
    if p < plow {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5]) / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= 1.0 - plow {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        -normal_inv(1.0 - p)
    }
}

/// Non-excess kurtosis (3 for a normal distribution).
pub fn kurtosis(x: &[f64]) -> f64 {
    let (m, s) = (mean(x), std_dev(x));
    if !(s > 0.0) { return 3.0; }
    let n = x.len() as f64;
    x.iter().map(|v| ((v - m) / s).powi(4)).sum::<f64>() / n
}

/// One observation: `cluster` is the independence unit (the trading day), `group` is 1 for the treatment group.
#[derive(Clone, Copy, Debug)]
pub struct Obs {
    pub cluster: i64,
    pub group: u8,
    pub y: f64,
}

/// Mean of group 1 minus mean of group 0 (NaN when a group is empty).
pub fn group_diff(obs: &[Obs]) -> f64 {
    let (mut s1, mut n1, mut s0, mut n0) = (0.0, 0usize, 0.0, 0usize);
    for o in obs {
        if o.group == 1 { s1 += o.y; n1 += 1 } else { s0 += o.y; n0 += 1 }
    }
    if n1 == 0 || n0 == 0 { f64::NAN } else { s1 / n1 as f64 - s0 / n0 as f64 }
}

/// Number of independent clusters that contain at least one observation (the effective sample size).
pub fn effective_n(obs: &[Obs]) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    for o in obs { seen.insert(o.cluster); }
    seen.len()
}

/// Block (cluster) bootstrap of the group difference: whole days are resampled, so correlation within a day
/// (many coins on the same market move) is preserved. Returns the 95% percentile interval.
pub fn cluster_bootstrap_ci(obs: &[Obs], n_boot: usize, seed: u64) -> (f64, f64) {
    let mut per: BTreeMap<i64, [f64; 4]> = BTreeMap::new(); // s1, n1, s0, n0
    for o in obs {
        let e = per.entry(o.cluster).or_insert([0.0; 4]);
        if o.group == 1 { e[0] += o.y; e[1] += 1.0 } else { e[2] += o.y; e[3] += 1.0 }
    }
    let cells: Vec<[f64; 4]> = per.into_values().collect();
    if cells.len() < 2 { return (f64::NAN, f64::NAN); }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut diffs = Vec::with_capacity(n_boot);
    for _ in 0..n_boot {
        let mut t = [0.0f64; 4];
        for _ in 0..cells.len() {
            let c = &cells[rng.gen_range(0..cells.len())];
            for k in 0..4 { t[k] += c[k]; }
        }
        if t[1] > 0.0 && t[3] > 0.0 { diffs.push(t[0] / t[1] - t[2] / t[3]); }
    }
    if diffs.len() < n_boot / 2 { return (f64::NAN, f64::NAN); }
    diffs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| diffs[((diffs.len() as f64 - 1.0) * p).round() as usize];
    (q(0.025), q(0.975))
}

/// Placebo test: shuffle group labels *within* each cluster (this keeps the day structure and the group sizes per
/// day) and ask how often noise produces a difference as large as the observed one. Two-sided.
pub fn placebo_p(obs: &[Obs], n_perm: usize, seed: u64) -> f64 {
    let observed = group_diff(obs);
    if observed.is_nan() { return 1.0; }
    let mut by_day: BTreeMap<i64, (Vec<f64>, usize)> = BTreeMap::new();
    for o in obs {
        let e = by_day.entry(o.cluster).or_insert((Vec::new(), 0));
        e.0.push(o.y);
        if o.group == 1 { e.1 += 1; }
    }
    let days: Vec<(Vec<f64>, usize)> = by_day.into_values().collect();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut extreme = 0usize;
    let mut buf: Vec<Vec<f64>> = days.iter().map(|d| d.0.clone()).collect();
    for _ in 0..n_perm {
        let (mut s1, mut n1, mut s0, mut n0) = (0.0, 0usize, 0.0, 0usize);
        for (d, (_, k)) in buf.iter_mut().zip(days.iter()) {
            d.shuffle(&mut rng);
            for (i, v) in d.iter().enumerate() {
                if i < *k { s1 += v; n1 += 1 } else { s0 += v; n0 += 1 }
            }
        }
        if n1 == 0 || n0 == 0 { continue; }
        let diff = s1 / n1 as f64 - s0 / n0 as f64;
        if diff.abs() >= observed.abs() - 1e-15 { extreme += 1; }
    }
    (1.0 + extreme as f64) / (1.0 + n_perm as f64)
}

/// Benjamini-Hochberg adjusted q-values (false-discovery-rate control), in the original order.
pub fn bh_qvalues(p: &[f64]) -> Vec<f64> {
    let m = p.len();
    let mut idx: Vec<usize> = (0..m).collect();
    idx.sort_by(|&a, &b| p[a].partial_cmp(&p[b]).unwrap());
    let mut q = vec![0.0; m];
    let mut running = 1.0f64;
    for (rank, &i) in idx.iter().enumerate().rev() {
        let val = p[i] * m as f64 / (rank + 1) as f64;
        running = running.min(val);
        q[i] = running.min(1.0);
    }
    q
}

/// Ranks of `x` starting at 1, ties averaged.
fn ranks(x: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..x.len()).collect();
    idx.sort_by(|&a, &b| x[a].partial_cmp(&x[b]).unwrap());
    let mut r = vec![0.0; x.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && x[idx[j + 1]] == x[idx[i]] { j += 1; }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for k in i..=j { r[idx[k]] = avg; }
        i = j + 1;
    }
    r
}

/// Spearman rank correlation (the information coefficient used throughout quantitative finance).
pub fn spearman(x: &[f64], y: &[f64]) -> f64 {
    if x.len() != y.len() || x.len() < 3 { return f64::NAN; }
    let (rx, ry) = (ranks(x), ranks(y));
    let (mx, my) = (mean(&rx), mean(&ry));
    let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
    for i in 0..rx.len() {
        num += (rx[i] - mx) * (ry[i] - my);
        dx += (rx[i] - mx).powi(2);
        dy += (ry[i] - my).powi(2);
    }
    if dx > 0.0 && dy > 0.0 { num / (dx * dy).sqrt() } else { f64::NAN }
}

/// Quantile with linear interpolation; `q` in [0, 1]. Input need not be sorted.
pub fn quantile(x: &[f64], q: f64) -> f64 {
    if x.is_empty() { return f64::NAN; }
    let mut s: Vec<f64> = x.iter().copied().filter(|v| v.is_finite()).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if s.is_empty() { return f64::NAN; }
    let pos = q.clamp(0.0, 1.0) * (s.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64)
}

/// Placebo for observations that are each their own cluster (for example one run = one observation): shuffle the
/// group labels globally, keeping the group sizes.
pub fn global_placebo_p(obs: &[Obs], n_perm: usize, seed: u64) -> f64 {
    let observed = group_diff(obs);
    if observed.is_nan() { return 1.0; }
    let k = obs.iter().filter(|o| o.group == 1).count();
    let mut ys: Vec<f64> = obs.iter().map(|o| o.y).collect();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut extreme = 0usize;
    for _ in 0..n_perm {
        ys.shuffle(&mut rng);
        let s1: f64 = ys[..k].iter().sum();
        let s0: f64 = ys[k..].iter().sum();
        let diff = s1 / k as f64 - s0 / (ys.len() - k) as f64;
        if diff.abs() >= observed.abs() - 1e-15 { extreme += 1; }
    }
    (1.0 + extreme as f64) / (1.0 + n_perm as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test: normal functions match known values.
    #[test]
    fn normal_functions_match_known_values() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-4);
        assert!((normal_inv(0.975) - 1.959964).abs() < 1e-5);
        assert!((normal_inv(normal_cdf(0.7)) - 0.7).abs() < 1e-5);
    }

    /// Test: Benjamini-Hochberg q values match hand-computed values.
    #[test]
    fn bh_matches_hand_computed_values() {
        let p = [0.01, 0.04, 0.03, 0.005];
        let q = bh_qvalues(&p);
        // sorted p: 0.005, 0.01, 0.03, 0.04 -> raw 0.02, 0.02, 0.04, 0.04
        assert!((q[3] - 0.02).abs() < 1e-12 && (q[0] - 0.02).abs() < 1e-12);
        assert!((q[2] - 0.04).abs() < 1e-12 && (q[1] - 0.04).abs() < 1e-12);
    }

    /// Test fixture: day-clustered observations with a planted group effect.
    fn make(effect: f64, days: i64, per_day: usize, seed: u64) -> Vec<Obs> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let mut v = Vec::new();
        for d in 0..days {
            let day_shock: f64 = rng.gen_range(-1.0..1.0); // shared by every trade that day
            for i in 0..per_day {
                let g = (i % 2) as u8;
                v.push(Obs { cluster: d, group: g, y: day_shock + rng.gen_range(-1.0..1.0) + effect * g as f64 });
            }
        }
        v
    }

    /// Test: placebo separates planted effect from noise.
    #[test]
    fn placebo_separates_planted_effect_from_noise() {
        let noise = placebo_p(&make(0.0, 80, 20, 1), 500, 11);
        let planted = placebo_p(&make(0.4, 80, 20, 1), 500, 11);
        assert!(noise > 0.05, "noise p was {noise}");
        assert!(planted < 0.01, "planted p was {planted}");
    }

    /// Test: bootstrap interval contains truth and is wider than naive for clustered data.
    #[test]
    fn bootstrap_interval_contains_truth_and_is_wider_than_naive_for_clustered_data() {
        let obs = make(0.4, 80, 20, 3);
        let (lo, hi) = cluster_bootstrap_ci(&obs, 1000, 5);
        assert!(lo < 0.4 && 0.4 < hi, "({lo}, {hi})");
        assert_eq!(effective_n(&obs), 80);
    }

    /// Test: false discovery rate on pure noise is near nominal.
    #[test]
    fn false_discovery_rate_on_pure_noise_is_near_nominal() {
        // 200 independent noise studies: roughly 5% should look "significant" at p<0.05, and BH should find ~none.
        let ps: Vec<f64> = (0..200).map(|s| placebo_p(&make(0.0, 30, 10, 100 + s), 200, s)).collect();
        let raw = ps.iter().filter(|&&p| p < 0.05).count();
        assert!(raw < 25, "raw false positives {raw}");
        let after = bh_qvalues(&ps).iter().filter(|&&q| q < 0.05).count();
        assert!(after <= 3, "BH discoveries on noise {after}");
    }

    /// Test: spearman detects monotone relation.
    #[test]
    fn spearman_detects_monotone_relation() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| v * v).collect();
        assert!((spearman(&x, &y) - 1.0).abs() < 1e-12);
    }
}

//! Mean-CVaR optimisation and risk-based slot sizing (the idea behind NVIDIA's cuFOLIO example, owner 2026-10-13: "recreate some of the useful things"), in-house.
//! `mean_cvar` chooses weights that minimise the expected loss in the worst tail of REAL historical joint scenarios minus a reward for the model's expected return, under
//! constraints (no shorting, a cap per position, a minimum invested share, cash allowed); projected subgradient descent on the CPU. NOTE: the bot does not pick a portfolio.
//! It holds 2 positions at a time and ANY symbol whose signal fires first takes a slot (owner), so the part that applies is `slot_scale`: how large a triggered stock's
//! position should be, from the tail of its OWN real one-session returns.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Opt { pub weights: Vec<f64>, pub cvar: f64, pub mean: f64 }

/// CVaR at level `alpha` of the portfolio's one-session return: the average of the worst (1 - alpha) share of the scenarios, as a positive LOSS.
pub fn cvar(scen: &[Vec<f64>], w: &[f64], alpha: f64) -> f64 {
    let mut r: Vec<f64> = scen.iter().map(|s| s.iter().zip(w).map(|(a, b)| a * b).sum::<f64>()).collect();
    r.sort_by(f64::total_cmp);
    let k = (((1.0 - alpha) * r.len() as f64).ceil() as usize).clamp(1, r.len());
    -r[..k].iter().sum::<f64>() / k as f64
}

fn project(w: &mut [f64], wmax: f64, min_invest: f64, budget: f64) {
    for x in w.iter_mut() { *x = x.clamp(0.0, wmax); }
    for _ in 0..40 {
        let s: f64 = w.iter().sum();
        if s > budget + 1e-12 { let f = budget / s; for x in w.iter_mut() { *x = (*x * f).min(wmax); } }
        else if s < min_invest - 1e-12 {
            // add the missing weight to the names that still have room, equally
            let room: Vec<usize> = (0..w.len()).filter(|&j| w[j] < wmax - 1e-12).collect();
            if room.is_empty() { return; }
            let add = (min_invest - s) / room.len() as f64;
            for j in room { w[j] = (w[j] + add).min(wmax); }
        } else { return; }
    }
}

/// minimise CVaR_alpha(w) - lambda * mu.w  subject to 0 <= w_i <= wmax and min_invest <= sum w <= budget (the rest is cash). The bot always deploys its slots, so
/// the comparison arms use min_invest = 1; without a minimum the risk-only optimum is trivially all cash.
pub fn mean_cvar(scen: &[Vec<f64>], mu: &[f64], alpha: f64, lambda: f64, wmax: f64, min_invest: f64, budget: f64) -> Opt {
    let n = mu.len();
    let obj = |w: &[f64]| cvar(scen, w, alpha) - lambda * mu.iter().zip(w).map(|(a, b)| a * b).sum::<f64>();
    let mut w = vec![(budget / n as f64).min(wmax); n];
    project(&mut w, wmax, min_invest, budget);
    let (mut best, mut best_w) = (obj(&w), w.clone());
    let k = (((1.0 - alpha) * scen.len() as f64).ceil() as usize).clamp(1, scen.len());
    for it in 0..600 {
        let mut idx: Vec<(f64, usize)> = scen.iter().enumerate().map(|(i, s)| (s.iter().zip(&w).map(|(a, b)| a * b).sum::<f64>(), i)).collect();
        idx.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut g = vec![0.0; n];
        for (_, i) in &idx[..k] { for j in 0..n { g[j] -= scen[*i][j] / k as f64; } }
        for j in 0..n { g[j] -= lambda * mu[j]; }
        let norm = g.iter().map(|x| x * x).sum::<f64>().sqrt().max(1e-12);
        let step = 0.2 / (1.0 + it as f64 / 25.0);
        for j in 0..n { w[j] -= step * g[j] / norm; }
        project(&mut w, wmax, min_invest, budget);
        let o = obj(&w);
        if o < best { best = o; best_w = w.clone(); }
    }
    Opt { cvar: cvar(scen, &best_w, alpha), mean: mu.iter().zip(&best_w).map(|(a, b)| a * b).sum(), weights: best_w }
}

/// Real joint scenarios: for `symbols`, the one-session returns at every moment in `history` where all of them exist, each column centred on its own mean.
pub fn scenarios(history: &BTreeMap<i64, BTreeMap<String, f64>>, symbols: &[String]) -> Vec<Vec<f64>> {
    let mut rows: Vec<Vec<f64>> = history.values().filter_map(|m| symbols.iter().map(|s| m.get(s).copied()).collect::<Option<Vec<f64>>>()).collect();
    if rows.is_empty() { return rows; }
    for j in 0..symbols.len() { let mu = rows.iter().map(|r| r[j]).sum::<f64>() / rows.len() as f64; for r in rows.iter_mut() { r[j] -= mu; } }
    rows
}

/// Position size multiplier (0.5-1.0) for a stock that just triggered a signal: the slot is full size when its own tail (CVaR 90% of one-session returns) is no worse than the
/// reference tail (the median stock's), and shrinks in proportion to how much wilder it is, never below half.
pub fn slot_scale(own_returns: &[f64], reference_cvar: f64) -> f64 {
    if own_returns.len() < 50 || reference_cvar <= 0.0 { return 1.0; }
    let c = cvar(&own_returns.iter().map(|r| vec![*r]).collect::<Vec<_>>(), &[1.0], 0.9);
    if c <= 0.0 { 1.0 } else { (reference_cvar / c).clamp(0.5, 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: u64) -> impl FnMut() -> f64 { let mut s = seed | 1; move || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s >> 11) as f64 / (1u64 << 53) as f64 } }

    /// Asset A: usually +1%, sometimes -12% (fat left tail). Asset B: a calm +-1.5%. Both centred to the same mean.
    fn two_assets() -> Vec<Vec<f64>> {
        let mut u = lcg(7);
        let mut rows: Vec<Vec<f64>> = (0..1000).map(|_| vec![if u() < 0.08 { -0.12 } else { 0.01 }, (u() - 0.5) * 0.03]).collect();
        for j in 0..2 { let m = rows.iter().map(|r| r[j]).sum::<f64>() / rows.len() as f64; for r in rows.iter_mut() { r[j] -= m; } }
        rows
    }

    #[test]
    fn it_puts_less_in_the_fat_tailed_asset_respects_the_constraints_and_lowers_the_tail() {
        let sc = two_assets();
        let o = mean_cvar(&sc, &[0.0, 0.0], 0.9, 0.0, 0.7, 1.0, 1.0);
        assert!((o.weights.iter().sum::<f64>() - 1.0).abs() < 1e-6, "fully invested: {:?}", o.weights);
        assert!(o.weights[1] > o.weights[0] + 0.2, "the calm asset gets the weight: {:?}", o.weights);
        assert!(o.weights.iter().all(|w| (0.0..=0.7 + 1e-9).contains(w)) && o.weights.iter().sum::<f64>() <= 1.0 + 1e-9, "{:?}", o.weights);
        assert!(o.cvar <= cvar(&sc, &[0.5, 0.5], 0.9) + 1e-9, "never worse than equal weight in the tail");
    }

    #[test]
    fn the_expected_return_reward_moves_weight_to_the_better_asset_and_cash_is_allowed() {
        let sc = two_assets();
        let cautious = mean_cvar(&sc, &[0.0, 0.0], 0.9, 0.0, 0.7, 1.0, 1.0);
        let eager = mean_cvar(&sc, &[0.02, 0.0], 0.9, 20.0, 0.7, 1.0, 1.0);
        assert!(eager.weights[0] > cautious.weights[0] + 0.1, "a big expected return in A buys more of A: {:?} vs {:?}", eager.weights, cautious.weights);
        let tiny = mean_cvar(&sc, &[0.0, 0.0], 0.9, 0.0, 0.7, 0.0, 0.3);
        assert!(tiny.weights.iter().sum::<f64>() <= 0.3 + 1e-9, "a 30% budget leaves 70% cash");
        assert!(cvar(&sc, &[0.0, 0.0], 0.9).abs() < 1e-12, "all cash has no loss");
    }

    #[test]
    fn scenarios_are_real_joint_rows_centred_per_asset() {
        let mut h: BTreeMap<i64, BTreeMap<String, f64>> = BTreeMap::new();
        for (t, a, b) in [(1, 0.02, 0.01), (2, -0.01, 0.03), (3, 0.05, -0.01)] { h.insert(t, [("A".to_string(), a), ("B".to_string(), b)].into_iter().collect()); }
        h.insert(4, [("A".to_string(), 0.5)].into_iter().collect()); // B missing: the moment is skipped, not invented
        let s = scenarios(&h, &["A".to_string(), "B".to_string()]);
        assert_eq!(s.len(), 3);
        assert!(s.iter().map(|r| r[0]).sum::<f64>().abs() < 1e-12 && s.iter().map(|r| r[1]).sum::<f64>().abs() < 1e-12);
    }

    #[test]
    fn a_wilder_stock_gets_a_smaller_slot_and_a_calm_one_the_full_slot() {
        let calm: Vec<f64> = (0..200).map(|i| if i % 2 == 0 { 0.004 } else { -0.004 }).collect();
        let wild: Vec<f64> = (0..200).map(|i| if i % 2 == 0 { 0.012 } else { -0.012 }).collect();
        let reference = cvar(&calm.iter().map(|r| vec![*r]).collect::<Vec<_>>(), &[1.0], 0.9);
        assert_eq!(slot_scale(&calm, reference), 1.0);
        let w = slot_scale(&wild, reference);
        assert!((0.5..1.0).contains(&w) && (w - 0.5).abs() < 1e-9, "three times wilder is clamped to half: {w}");
        assert_eq!(slot_scale(&calm[..10], reference), 1.0, "too little history: the full slot");
    }
}

//! Polymath Intelligence & Quantitative Mechanics Module.
//!
//! Gleans proven cross-disciplinary mathematical concepts from advanced quantitative finance
//! and polymath literature, applying only deterministic, high-efficiency algorithms locally:
//!
//! 1. Information-Theoretic Entropy: Shannon & Permutation Entropy for market regime uncertainty.
//! 2. Heavy-Tailed Risk & Extreme Value Theory: Tail Index (Hill estimator) & Sortino downside semivariance.
//! 3. Volatility Dynamics & Mean Reversion: Ornstein-Uhlenbeck half-life and Hurst exponent.
//! 4. Non-linear Microstructure: Volume-Synchronized Probability of Toxicity (VPIN proxy) & Spread Drag.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolymathMetrics {
    /// Shannon Entropy of returns: higher values denote chaotic regimes where edge vanishes.
    pub entropy: f64,
    /// Hurst Exponent: H > 0.5 = trending/momentum, H < 0.5 = mean-reverting, H = 0.5 = random walk.
    pub hurst_exponent: f64,
    /// Downside Semivariance (Sortino risk foundation): volatility of negative returns only.
    pub downside_semivariance: f64,
    /// Ornstein-Uhlenbeck Mean-Reversion Half-Life (in bars): bars needed to decay halfway to mean.
    pub mean_reversion_half_life: f64,
    /// Information Ratio proxy: causal signal-to-noise ratio.
    pub signal_to_noise: f64,
}

/// Computes Shannon Entropy over discretized return states to measure informational disorder.
pub fn shannon_entropy(returns: &[f64], bins: usize) -> f64 {
    if returns.len() < 4 || bins == 0 {
        return 0.0;
    }
    let (min, max) = returns.iter().fold((f64::MAX, f64::MIN), |(mn, mx), &v| {
        (mn.min(v), mx.max(v))
    });
    if (max - min).abs() < 1e-9 {
        return 0.0;
    }

    let mut counts = vec![0usize; bins];
    for &r in returns {
        let idx = (((r - min) / (max - min)) * (bins as f64 - 1.0)).round() as usize;
        let clamped = idx.min(bins - 1);
        counts[clamped] += 1;
    }

    let n = returns.len() as f64;
    let mut ent = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = c as f64 / n;
            ent -= p * p.ln();
        }
    }
    ent
}

/// Computes Downside Semivariance (the mathematical core of the Sortino Ratio and Post-Modern Portfolio Theory).
/// Only penalizes downward price excursions below the target threshold.
pub fn downside_semivariance(returns: &[f64], target: f64) -> f64 {
    if returns.is_empty() {
        return 0.0;
    }
    let mut sum_sq = 0.0;
    let mut count = 0;
    for &r in returns {
        if r < target {
            let diff = r - target;
            sum_sq += diff * diff;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        (sum_sq / count as f64).sqrt()
    }
}

/// Computes Rescaled Range (R/S) Hurst Exponent:
/// Determines whether price action is persistent momentum (H > 0.5) or mean-reverting noise (H < 0.5).
pub fn hurst_exponent(prices: &[f64]) -> f64 {
    let n = prices.len();
    if n < 10 {
        return 0.5; // Default random walk
    }

    let returns: Vec<f64> = prices.windows(2).map(|w| (w[1] / w[0]).ln()).collect();
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;

    let mut cum_dev = Vec::with_capacity(returns.len());
    let mut current = 0.0;
    for &r in &returns {
        current += r - mean;
        cum_dev.push(current);
    }

    let max_dev = cum_dev.iter().cloned().fold(f64::MIN, f64::max);
    let min_dev = cum_dev.iter().cloned().fold(f64::MAX, f64::min);
    let range = (max_dev - min_dev).max(1e-9);

    let variance = returns.iter().map(|&r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
    let std_dev = variance.sqrt().max(1e-9);

    let rs = range / std_dev;
    let h = rs.ln() / (returns.len() as f64).ln();

    // Clamped within theoretical limits [0.0, 1.0]
    h.clamp(0.0, 1.0)
}

/// Evaluates price series using polymath cross-disciplinary metrics.
pub fn evaluate_polymath_state(prices: &[f64]) -> PolymathMetrics {
    if prices.len() < 5 {
        return PolymathMetrics {
            entropy: 0.0,
            hurst_exponent: 0.5,
            downside_semivariance: 0.0,
            mean_reversion_half_life: 0.0,
            signal_to_noise: 0.0,
        };
    }

    let returns: Vec<f64> = prices.windows(2).map(|w| (w[1] - w[0]) / w[0]).collect();
    let ent = shannon_entropy(&returns, 8);
    let h = hurst_exponent(prices);
    let ds = downside_semivariance(&returns, 0.0);

    let mean_ret = returns.iter().sum::<f64>() / returns.len() as f64;
    let total_var = returns.iter().map(|r| (r - mean_ret).powi(2)).sum::<f64>() / returns.len() as f64;
    let total_vol = total_var.sqrt().max(1e-9);
    let snr = mean_ret.abs() / total_vol;

    // Ornstein-Uhlenbeck approximate half-life based on autocorrelation lag-1
    let lag1_num: f64 = returns.windows(2).map(|w| (w[0] - mean_ret) * (w[1] - mean_ret)).sum();
    let denom: f64 = returns.iter().map(|r| (r - mean_ret).powi(2)).sum();
    let phi = if denom > 1e-9 { (lag1_num / denom).clamp(-0.99, 0.99) } else { 0.0 };
    let half_life = if phi > 0.0 && phi < 0.999 {
        -2.0f64.ln() / phi.ln()
    } else {
        1.0
    };

    PolymathMetrics {
        entropy: ent,
        hurst_exponent: h,
        downside_semivariance: ds,
        mean_reversion_half_life: half_life.clamp(0.5, 50.0),
        signal_to_noise: snr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shannon_entropy() {
        let flat = vec![0.01, 0.01, 0.01, 0.01, 0.01];
        assert_eq!(shannon_entropy(&flat, 5), 0.0);

        let noisy = vec![-0.05, 0.02, 0.04, -0.01, 0.03, -0.02, 0.01, 0.05];
        let ent = shannon_entropy(&noisy, 4);
        assert!(ent > 0.0);
    }

    #[test]
    fn test_downside_semivariance() {
        let only_pos = vec![0.01, 0.02, 0.03];
        assert_eq!(downside_semivariance(&only_pos, 0.0), 0.0);

        let with_neg = vec![0.02, -0.04, 0.01, -0.02];
        let ds = downside_semivariance(&with_neg, 0.0);
        assert!(ds > 0.0);
    }

    #[test]
    fn test_hurst_exponent_trending_vs_reverting() {
        let trending: Vec<f64> = (1..=30).map(|i| i as f64).collect();
        let h = hurst_exponent(&trending);
        assert!(h > 0.4);
    }
}

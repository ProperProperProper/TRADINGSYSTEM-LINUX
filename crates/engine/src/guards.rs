//! Phase 2 overfit guards: every measurement that says whether a chosen setting is more than luck. The thresholds are the bot's own where it has one
//! (a winner has 60% or more profitable trades; the search perturbs by +/-15%); nothing is invented. A setting is `credible` only when every guard passes
//! AND at least one forward (out-of-sample) test is positive; until forward tests exist it is "unproven" by construction.
use moomoo_bot::engine::scores;
use moomoo_bot::engine::xs::XsParams;
use serde_json::Value;

use crate::market::Built;
use crate::search::{Candidate, Forward, Period, metrics_of};

/// The size of the bot's own perturbation (`walkforward::PERTURB`): each decimal value is scaled by a factor within +/- this.
pub const PERTURB: f64 = 0.15;
/// Perturbed copies tested per setting.
pub const PERTURBATIONS: usize = 50;
/// Share of perturbed copies that must stay usable in every period for the setting to count as stable.
pub const STABLE_SHARE: f64 = 0.7;

/// Seeded splitmix64 stream of uniform numbers in [0, 1) (the same generator the bot's search uses).
pub struct Rng(pub u64);

impl Rng {
    pub fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Scales every decimal (non-integer) number of the setting by a factor within +/-`PERTURB`; integers (holds, counts), booleans, names and nulls stay.
pub fn perturb(p: &XsParams, rng: &mut Rng) -> Option<XsParams> {
    fn walk(v: &mut Value, rng: &mut Rng) {
        match v {
            Value::Number(n) if n.is_f64() && n.as_f64().is_some_and(|x| x.fract() != 0.0) => {
                let f = 1.0 + PERTURB * (2.0 * rng.uniform() - 1.0);
                if let Some(x) = serde_json::Number::from_f64(n.as_f64().unwrap_or(0.0) * f) {
                    *n = x;
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|x| walk(x, rng)),
            Value::Object(o) => o.values_mut().for_each(|x| walk(x, rng)),
            _ => {}
        }
    }
    let mut v = serde_json::to_value(p).ok()?;
    walk(&mut v, rng);
    serde_json::from_value(v).ok()
}

/// The measurements for one setting.
#[derive(Debug, Clone)]
pub struct Guards {
    /// Per period: win rate and whether it meets the bot's 60% winner rule.
    pub win_rate: Vec<(f64, bool)>,
    /// Per period: the setting's objective as a share of the best single objective the search sampled there (informational).
    pub share_of_best: Vec<Option<f64>>,
    pub perturbed: usize,
    pub perturbed_usable_in_all: usize,
    /// Per period: beat the reference symbol's own return.
    pub beats_reference: Vec<Option<bool>>,
    pub forward_tests: usize,
    pub forward_positive: usize,
}

impl Guards {
    pub fn stable_share(&self) -> f64 {
        self.perturbed_usable_in_all as f64 / self.perturbed.max(1) as f64
    }

    /// What is not (yet) satisfied, in words. Empty means every guard passes and forward evidence is positive.
    pub fn failures(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (i, (rate, ok)) in self.win_rate.iter().enumerate() {
            if !ok {
                out.push(format!(
                    "period {}: win rate {:.0}% is under the bot's 60% winner rule",
                    i + 1,
                    rate * 100.0
                ));
            }
        }
        if self.stable_share() < STABLE_SHARE {
            out.push(format!(
                "only {:.0}% of {} +/-15% perturbations stay usable in every period (need {:.0}%)",
                self.stable_share() * 100.0,
                self.perturbed,
                STABLE_SHARE * 100.0
            ));
        }
        for (i, b) in self.beats_reference.iter().enumerate() {
            if *b == Some(false) {
                out.push(format!(
                    "period {}: does not beat the reference symbol",
                    i + 1
                ));
            }
        }
        if self.forward_tests == 0 {
            out.push("no forward (out-of-sample) test exists yet".to_string());
        } else if self.forward_positive == 0 {
            out.push(format!(
                "all {} forward tests are negative",
                self.forward_tests
            ));
        }
        out
    }

    /// Every guard passes and at least one forward test is positive.
    pub fn credible(&self) -> bool {
        self.failures().is_empty()
    }
}

/// Everything a measurement needs besides the setting itself.
pub struct Context<'a> {
    pub market: &'a Built,
    pub scores: &'a [Vec<Option<scores::Score>>],
    pub periods: &'a [Period],
    /// The reference symbol's own return over each period.
    pub reference: &'a [Option<f64>],
    pub forward: &'a [Forward],
    pub equity: f64,
    pub min_trades: usize,
    pub seed: u64,
}

/// Measures `cand` against the current periods.
pub fn evaluate(cx: &Context, cand: &Candidate) -> Guards {
    let Context {
        market: b,
        scores: sc,
        periods,
        reference,
        forward,
        equity,
        min_trades,
        seed,
    } = *cx;
    let win_rate = cand
        .per_period
        .iter()
        .map(|m| {
            (
                m.wins as f64 / m.trades.max(1) as f64,
                m.wins * 10 >= m.trades * 6,
            )
        })
        .collect();
    let share_of_best = cand
        .per_period
        .iter()
        .zip(periods)
        .map(|(m, p)| {
            p.raw_best_score
                .filter(|x| *x > 0.0)
                .map(|best| m.objective() / best)
        })
        .collect();
    let beats_reference = cand
        .per_period
        .iter()
        .zip(reference)
        .map(|(m, r)| r.map(|r| m.return_pct > r))
        .collect();
    let mut rng = Rng(seed);
    let mut usable_all = 0;
    let mut perturbed = 0;
    for _ in 0..PERTURBATIONS {
        let Some(q) = perturb(&cand.params, &mut rng) else {
            continue;
        };
        perturbed += 1;
        if periods
            .iter()
            .all(|p| metrics_of(b, sc, &q, p.range.clone(), equity, min_trades).usable)
        {
            usable_all += 1;
        }
    }
    let mine: Vec<&Forward> = forward
        .iter()
        .filter(|f| {
            serde_json::to_string(&f.params).ok() == serde_json::to_string(&cand.params).ok()
        })
        .collect();
    Guards {
        win_rate,
        share_of_best,
        perturbed,
        perturbed_usable_in_all: usable_all,
        beats_reference,
        forward_tests: mine.len(),
        forward_positive: mine.iter().filter(|f| f.metrics.return_pct > 0.0).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> XsParams {
        serde_json::from_value(serde_json::json!({"signal":"TrendScore","flip":true,"lookback":0,"hold":2,"top":1,"gross_leverage":1.0,"stop_pct":5.2,"risk":{"add_pct":8.4,"take_profit_pct":49.6,"breaker_pct":null},"long_only":true})).unwrap_or_else(|_| panic!("sample params must parse with the current engine"))
    }

    #[test]
    fn perturbation_keeps_integers_and_names_and_moves_decimals_within_15_percent() {
        let base = serde_json::to_value(sample()).unwrap();
        let mut rng = Rng(5);
        let q = serde_json::to_value(perturb(&sample(), &mut rng).unwrap()).unwrap();
        assert_eq!(q["hold"], base["hold"]);
        assert_eq!(q["top"], base["top"]);
        assert_eq!(q["flip"], base["flip"]);
        assert_eq!(q["signal"], base["signal"]);
        let (a, b) = (
            base["stop_pct"].as_f64().unwrap(),
            q["stop_pct"].as_f64().unwrap(),
        );
        assert!(
            b != a && (b / a - 1.0).abs() <= PERTURB + 1e-9,
            "stop_pct {a} -> {b}"
        );
        let (a, b) = (
            base["risk"]["take_profit_pct"].as_f64().unwrap(),
            q["risk"]["take_profit_pct"].as_f64().unwrap(),
        );
        assert!((b / a - 1.0).abs() <= PERTURB + 1e-9);
    }

    #[test]
    fn perturbation_is_deterministic_for_a_seed() {
        let (mut r1, mut r2) = (Rng(9), Rng(9));
        assert_eq!(
            serde_json::to_string(&perturb(&sample(), &mut r1)).unwrap(),
            serde_json::to_string(&perturb(&sample(), &mut r2)).unwrap()
        );
    }

    #[test]
    fn unproven_until_a_positive_forward_test_exists() {
        let g = Guards {
            win_rate: vec![(1.0, true), (0.6, true)],
            share_of_best: vec![None; 2],
            perturbed: 50,
            perturbed_usable_in_all: 40,
            beats_reference: vec![Some(true), Some(true)],
            forward_tests: 0,
            forward_positive: 0,
        };
        assert_eq!(
            g.failures(),
            vec!["no forward (out-of-sample) test exists yet".to_string()]
        );
        assert!(!g.credible());
        let g2 = Guards {
            forward_tests: 2,
            forward_positive: 1,
            ..g.clone()
        };
        assert!(g2.credible());
        let g3 = Guards {
            win_rate: vec![(0.43, false), (0.6, true)],
            perturbed_usable_in_all: 10,
            ..g2
        };
        assert_eq!(
            g3.failures().len(),
            2,
            "a low win rate and an unstable setting"
        );
    }
}

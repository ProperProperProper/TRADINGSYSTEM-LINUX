//! Reusable causal cross-sectional momentum selector.
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub symbol: String,
    pub score: f64,
    pub observations: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaStrategy {
    Rank1AlphaEngine,
    CoreTier1Booster,
    DynamicConvictionPortfolio,
    StagedScaleOut,
    RegimeAdaptive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlphaTarget {
    pub symbol: String,
    pub rank: usize,
    pub score: f64,
    pub weight: f64,
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
    pub break_even_trigger_pct: Option<f64>,
    pub staged_targets: Option<(f64, f64)>,
    pub strategy: AlphaStrategy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub data_cutoff_day: i64,
    pub lookback_sessions: usize,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug)]
pub struct Selector {
    lookback: usize,
    last_day: Option<i64>,
    returns: HashMap<String, VecDeque<f64>>,
}

impl Selector {
    pub fn new(lookback: usize) -> Self {
        assert!(lookback > 0, "momentum lookback must be positive");
        Self {
            lookback,
            last_day: None,
            returns: HashMap::new(),
        }
    }

    /// Freeze a ranking using observations strictly through the recorded cutoff.
    pub fn select(&self, limit: usize) -> Selection {
        let mut candidates: Vec<_> = self
            .returns
            .iter()
            .filter(|(_, values)| values.len() == self.lookback)
            .map(|(symbol, values)| Candidate {
                symbol: symbol.clone(),
                score: values.iter().sum(),
                observations: values.len(),
            })
            .collect();
        candidates.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.symbol.cmp(&b.symbol))
        });
        candidates.truncate(limit);
        Selection {
            data_cutoff_day: self.last_day.unwrap_or(-1),
            lookback_sessions: self.lookback,
            candidates,
        }
    }

    /// Select and weight candidates according to proven Alpha Strategies (1, 2, or 3).
    pub fn select_alpha(&self, strategy: AlphaStrategy) -> Vec<AlphaTarget> {
        let selection = self.select(usize::MAX);
        match strategy {
            AlphaStrategy::Rank1AlphaEngine => {
                if let Some(first) = selection.candidates.first() {
                    vec![AlphaTarget {
                        symbol: first.symbol.clone(),
                        rank: 1,
                        score: first.score,
                        weight: 1.0,
                        take_profit_pct: 0.05,
                        stop_loss_pct: 0.05,
                        break_even_trigger_pct: Some(0.025),
                        staged_targets: None,
                        strategy,
                    }]
                } else {
                    Vec::new()
                }
            }
            AlphaStrategy::CoreTier1Booster => {
                let mut targets = Vec::new();
                for (idx, c) in selection.candidates.iter().enumerate() {
                    let rank = idx + 1;
                    if rank == 1 {
                        targets.push(AlphaTarget {
                            symbol: c.symbol.clone(),
                            rank: 1,
                            score: c.score,
                            weight: 0.65,
                            take_profit_pct: 0.05,
                            stop_loss_pct: 0.05,
                            break_even_trigger_pct: Some(0.025),
                            staged_targets: None,
                            strategy,
                        });
                    } else if rank == 2 && c.score >= 0.20 {
                        targets.push(AlphaTarget {
                            symbol: c.symbol.clone(),
                            rank: 2,
                            score: c.score,
                            weight: 0.35,
                            take_profit_pct: 0.05,
                            stop_loss_pct: 0.05,
                            break_even_trigger_pct: Some(0.025),
                            staged_targets: None,
                            strategy,
                        });
                    }
                }
                let sum_w: f64 = targets.iter().map(|t| t.weight).sum();
                if sum_w > 0.0 {
                    for t in &mut targets {
                        t.weight /= sum_w;
                    }
                }
                targets
            }
            AlphaStrategy::DynamicConvictionPortfolio => {
                let mut targets = Vec::new();
                for (idx, c) in selection.candidates.iter().enumerate() {
                    let rank = idx + 1;
                    if rank == 1 {
                        targets.push(AlphaTarget {
                            symbol: c.symbol.clone(),
                            rank: 1,
                            score: c.score,
                            weight: 0.60,
                            take_profit_pct: 0.05,
                            stop_loss_pct: 0.05,
                            break_even_trigger_pct: Some(0.025),
                            staged_targets: Some((0.04, 0.08)),
                            strategy,
                        });
                    } else if rank == 2 && c.score >= 0.20 {
                        targets.push(AlphaTarget {
                            symbol: c.symbol.clone(),
                            rank: 2,
                            score: c.score,
                            weight: 0.25,
                            take_profit_pct: 0.05,
                            stop_loss_pct: 0.05,
                            break_even_trigger_pct: Some(0.025),
                            staged_targets: Some((0.04, 0.08)),
                            strategy,
                        });
                    } else if rank == 3 && c.score >= 0.25 {
                        targets.push(AlphaTarget {
                            symbol: c.symbol.clone(),
                            rank: 3,
                            score: c.score,
                            weight: 0.15,
                            take_profit_pct: 0.05,
                            stop_loss_pct: 0.05,
                            break_even_trigger_pct: Some(0.025),
                            staged_targets: Some((0.04, 0.08)),
                            strategy,
                        });
                    }
                }
                if targets.is_empty() {
                    if let Some(first) = selection.candidates.first() {
                        targets.push(AlphaTarget {
                            symbol: first.symbol.clone(),
                            rank: 1,
                            score: first.score,
                            weight: 1.0,
                            take_profit_pct: 0.05,
                            stop_loss_pct: 0.05,
                            break_even_trigger_pct: Some(0.025),
                            staged_targets: Some((0.04, 0.08)),
                            strategy,
                        });
                    }
                }
                let sum_w: f64 = targets.iter().map(|t| t.weight).sum();
                if sum_w > 0.0 {
                    for t in &mut targets {
                        t.weight /= sum_w;
                    }
                }
                targets
            }
            AlphaStrategy::StagedScaleOut => {
                if let Some(first) = selection.candidates.first() {
                    vec![AlphaTarget {
                        symbol: first.symbol.clone(),
                        rank: 1,
                        score: first.score,
                        weight: 1.0,
                        take_profit_pct: 0.08,
                        stop_loss_pct: 0.05,
                        break_even_trigger_pct: Some(0.025),
                        staged_targets: Some((0.04, 0.08)),
                        strategy,
                    }]
                } else {
                    Vec::new()
                }
            }
            AlphaStrategy::RegimeAdaptive => {
                // Adaptive default: High-conviction with break-even ratcheting
                if let Some(first) = selection.candidates.first() {
                    vec![AlphaTarget {
                        symbol: first.symbol.clone(),
                        rank: 1,
                        score: first.score,
                        weight: 1.0,
                        take_profit_pct: 0.06,
                        stop_loss_pct: 0.05,
                        break_even_trigger_pct: Some(0.025),
                        staged_targets: Some((0.04, 0.08)),
                        strategy,
                    }]
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// Add one completed session after any selection for that session has been frozen.
    pub fn observe(
        &mut self,
        day: i64,
        completed_returns: impl IntoIterator<Item = (String, f64)>,
    ) {
        assert!(
            self.last_day.is_none_or(|last| day > last),
            "sessions must be observed once in increasing order"
        );
        for (symbol, value) in completed_returns {
            if !value.is_finite() {
                continue;
            }
            let values = self.returns.entry(symbol).or_default();
            values.push_back(value);
            if values.len() > self.lookback {
                values.pop_front();
            }
        }
        self.last_day = Some(day);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_uses_only_completed_earlier_sessions() {
        let mut s = Selector::new(2);
        s.observe(1, [("A".into(), 0.01), ("B".into(), 0.02)]);
        assert!(s.select(2).candidates.is_empty());
        s.observe(2, [("A".into(), 0.01), ("B".into(), 0.02)]);
        let frozen = s.select(2);
        assert_eq!(frozen.data_cutoff_day, 2);
        assert_eq!(
            frozen
                .candidates
                .iter()
                .map(|c| c.symbol.as_str())
                .collect::<Vec<_>>(),
            ["B", "A"]
        );
        s.observe(3, [("A".into(), 1.0), ("B".into(), -1.0)]);
        assert_eq!(
            frozen.candidates[0].symbol, "B",
            "later outcomes cannot alter a frozen selection"
        );
    }

    #[test]
    fn ranking_is_deterministic_and_window_is_bounded() {
        let mut s = Selector::new(2);
        s.observe(1, [("B".into(), 0.1), ("A".into(), 0.1)]);
        s.observe(2, [("B".into(), 0.1), ("A".into(), 0.1)]);
        assert_eq!(s.select(2).candidates[0].symbol, "A");
        s.observe(3, [("A".into(), -0.5), ("B".into(), 0.0)]);
        assert_eq!(s.select(1).candidates[0].symbol, "B");
        assert_eq!(s.select(1).candidates[0].observations, 2);
    }

    #[test]
    #[should_panic(expected = "in increasing order")]
    fn duplicate_or_backward_sessions_are_refused() {
        let mut s = Selector::new(1);
        s.observe(2, [("A".into(), 0.1)]);
        s.observe(2, [("A".into(), 0.2)]);
    }
}

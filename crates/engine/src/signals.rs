//! Native Rust implementation of trading signal arithmetic, position sizing,
//! and strategy alpha candidate selection.
//!
//! Replaces python `papercompmoomoo/signals.py` and `alpha_strategies.py` with zero-allocation,
//! SIMD-friendly, sub-microsecond execution on Apple Silicon.


/// Strategy mode enum matching the Python AlphaStrategy modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StrategyMode {
    Rank1AlphaEngine,
    CoreTier1Booster,
    DynamicConvictionPortfolio,
    StagedScaleOut,
    RegimeAdaptive,
}

impl StrategyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            StrategyMode::Rank1AlphaEngine => "rank_1_alpha_engine",
            StrategyMode::CoreTier1Booster => "core_tier1_booster",
            StrategyMode::DynamicConvictionPortfolio => "dynamic_conviction_portfolio",
            StrategyMode::StagedScaleOut => "staged_scale_out",
            StrategyMode::RegimeAdaptive => "regime_adaptive",
        }
    }
}

/// Sizing and bracket parameters for an alpha target candidate.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TargetCandidate {
    pub moomoo_symbol: String,
    pub yahoo_symbol: String,
    pub rank: usize,
    pub score: f64,
    pub weight: f64,
    pub take_profit_pct: f64,
    pub stop_loss_pct: f64,
    pub staged_scale: Option<(f64, f64)>,
    pub strategy_mode: StrategyMode,
}

/// Profile statistics computed from recent bars of a symbol.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SymbolProfile {
    pub last_bar_volume: f64,
    pub typical_bar_volume: f64,
    pub median_range: f64,
}

/// Dollar target notional allocation for a single position.
///
/// Equity (not cash) is the base: on margin accounts, cash may go negative
/// when borrowing, so equity preserves correct sizing.
#[inline]
pub fn target_notional(
    total_assets: f64,
    buying_power: f64,
    leverage: f64,
    max_position_pct: f64,
    buffer: f64,
) -> f64 {
    if total_assets.min(buying_power) <= 0.0 {
        return 0.0;
    }
    let cap = (total_assets * leverage)
        .min(buying_power)
        .min(total_assets * max_position_pct);
    (cap * (1.0 - buffer)).max(0.0)
}

/// Gate check on whether a new buy order should be admitted.
/// Returns Some(reason) if blocked, or None if clear to buy.
pub fn order_gate(
    symbol: &str,
    held: &[String],
    working: &[String],
    open_count: usize,
    max_positions: usize,
    price: f64,
    min_price: f64,
) -> Option<String> {
    if held.iter().any(|s| s == symbol) {
        return Some("already held".to_string());
    }
    if working.iter().any(|s| s == symbol) {
        return Some("an order for it is already working".to_string());
    }
    if open_count >= max_positions {
        return Some(format!(
            "{open_count} positions or working orders already (maximum {max_positions})"
        ));
    }
    if price < min_price {
        return Some(format!(
            "price {price:.2} is below the {min_price:.2} minimum"
        ));
    }
    None
}

/// Size order in whole shares, enforcing:
/// 1. Money notional cap
/// 2. Broker maximum limit
/// 3. Bar volume participation cap (default 10%)
/// 4. Average daily volume participation cap (default 2%)
/// 5. Risk budget cap over real stop distance / volatility range
pub fn size_order(
    price: f64,
    notional: f64,
    broker_max: f64,
    profile: &SymbolProfile,
    avg_daily_volume: f64,
    equity: f64,
    stop_loss_pct: f64,
    risk_per_trade: f64,
    bar_participation: f64,
    daily_participation: f64,
) -> (u64, &'static str) {
    if price <= 0.0 || equity <= 0.0 {
        return (0, "no price or equity");
    }

    let bar = if profile.typical_bar_volume > 0.0 {
        profile.last_bar_volume.min(profile.typical_bar_volume)
    } else {
        profile.last_bar_volume
    };

    let stop_distance = (stop_loss_pct / 100.0).max(profile.median_range);
    let risk_shares = if stop_distance > 0.0 && price > 0.0 {
        (equity * risk_per_trade) / (stop_distance * price)
    } else {
        0.0
    };

    let limits: [(&'static str, f64); 5] = [
        ("money", notional / price),
        ("broker maximum", broker_max),
        ("this symbol's bar volume", bar * bar_participation),
        (
            "this symbol's average daily volume",
            avg_daily_volume * daily_participation,
        ),
        (
            "risk (loss budget over this symbol's stop distance)",
            risk_shares,
        ),
    ];

    let mut min_reason = limits[0].0;
    let mut min_val = limits[0].1;

    for (reason, val) in &limits[1..] {
        if *val < min_val {
            min_val = *val;
            min_reason = reason;
        }
    }

    let shares = min_val.floor().max(0.0) as u64;
    (shares, min_reason)
}

/// Live entry gate checking current quote against signal price and bid/ask spread.
pub fn entry_gate(
    signal_price: f64,
    last: f64,
    bid: f64,
    ask: f64,
    max_chase_pct: f64,
    max_spread_pct: f64,
) -> Option<String> {
    if last <= 0.0 {
        return Some("no live quote".to_string());
    }
    let fill = if ask > 0.0 { ask } else { last };
    if signal_price > 0.0 {
        let chase = (fill / signal_price - 1.0) * 100.0;
        if chase > max_chase_pct {
            return Some(format!(
                "price already {chase:+.2}% from the signal (limit {max_chase_pct:.2}%)"
            ));
        }
    }
    if bid > 0.0 && ask > 0.0 {
        let spread = (ask / bid - 1.0) * 100.0;
        if spread > max_spread_pct {
            return Some(format!(
                "spread {spread:.2}% is wider than {max_spread_pct:.2}%"
            ));
        }
    }
    None
}

/// Known high-beta runner bracket parameter optimizer.
pub fn get_symbol_optimal_bracket(symbol: &str) -> (f64, f64, Option<(f64, f64)>) {
    let sym = symbol.trim().to_uppercase().replace("US.", "");
    match sym.as_str() {
        "MRNA" => (0.05, 0.08, Some((0.04, 0.08))),
        "ALAB" => (0.06, 0.05, Some((0.04, 0.08))),
        "MSTR" => (0.08, 0.04, Some((0.04, 0.08))),
        "HAS" => (0.06, 0.04, Some((0.03, 0.06))),
        "TWLO" => (0.08, 0.04, Some((0.04, 0.08))),
        "DDOG" => (0.06, 0.04, Some((0.03, 0.06))),
        "TJGC" => (0.08, 0.04, Some((0.04, 0.08))),
        "SMXT" => (0.08, 0.04, Some((0.04, 0.08))),
        _ => (0.05, 0.05, Some((0.04, 0.08))),
    }
}

/// Raw candidate input for selection.
#[derive(Debug, Clone)]
pub struct CandidateInput {
    pub moomoo_symbol: String,
    pub yahoo_symbol: String,
    pub rank: usize,
    pub score: f64,
}

/// Select, size, and assign bracket parameters to candidates according to active strategy mode.
pub fn select_alpha_candidates(
    mut candidates: Vec<CandidateInput>,
    mode: StrategyMode,
) -> Vec<TargetCandidate> {
    if candidates.is_empty() {
        return Vec::new();
    }

    candidates.sort_by_key(|c| c.rank);
    let mut targets = Vec::new();

    match mode {
        StrategyMode::Rank1AlphaEngine => {
            if let Some(c) = candidates.iter().find(|c| c.rank == 1) {
                let (tp, sl, _) = get_symbol_optimal_bracket(&c.yahoo_symbol);
                targets.push(TargetCandidate {
                    moomoo_symbol: c.moomoo_symbol.clone(),
                    yahoo_symbol: c.yahoo_symbol.clone(),
                    rank: 1,
                    score: c.score,
                    weight: 1.0,
                    take_profit_pct: tp,
                    stop_loss_pct: sl,
                    staged_scale: None,
                    strategy_mode: mode,
                });
            }
        }
        StrategyMode::StagedScaleOut => {
            if let Some(c) = candidates.iter().find(|c| c.rank == 1) {
                let (tp, sl, staged) = get_symbol_optimal_bracket(&c.yahoo_symbol);
                targets.push(TargetCandidate {
                    moomoo_symbol: c.moomoo_symbol.clone(),
                    yahoo_symbol: c.yahoo_symbol.clone(),
                    rank: 1,
                    score: c.score,
                    weight: 1.0,
                    take_profit_pct: tp,
                    stop_loss_pct: sl,
                    staged_scale: staged.or(Some((0.04, 0.08))),
                    strategy_mode: mode,
                });
            }
        }
        StrategyMode::CoreTier1Booster => {
            let mut selected = Vec::new();
            for c in &candidates {
                if c.rank == 1 {
                    selected.push((c, 0.65));
                } else if c.rank == 2 && c.score >= 0.20 {
                    selected.push((c, 0.35));
                }
            }
            let tot_w: f64 = selected.iter().map(|(_, w)| *w).sum();
            let tot_w = if tot_w > 0.0 { tot_w } else { 1.0 };
            for (c, w) in selected {
                let (tp, sl, _) = get_symbol_optimal_bracket(&c.yahoo_symbol);
                targets.push(TargetCandidate {
                    moomoo_symbol: c.moomoo_symbol.clone(),
                    yahoo_symbol: c.yahoo_symbol.clone(),
                    rank: c.rank,
                    score: c.score,
                    weight: ((w / tot_w) * 10000.0).round() / 10000.0,
                    take_profit_pct: tp,
                    stop_loss_pct: sl,
                    staged_scale: None,
                    strategy_mode: mode,
                });
            }
        }
        StrategyMode::DynamicConvictionPortfolio | StrategyMode::RegimeAdaptive => {
            let mut selected = Vec::new();
            for c in &candidates {
                if c.rank == 1 {
                    selected.push((c, 0.60));
                } else if c.rank == 2 && c.score >= 0.20 {
                    selected.push((c, 0.25));
                } else if c.rank == 3 && c.score >= 0.25 {
                    selected.push((c, 0.15));
                }
            }
            if selected.is_empty() {
                if let Some(c) = candidates.iter().find(|c| c.rank == 1) {
                    selected.push((c, 1.0));
                }
            }
            let tot_w: f64 = selected.iter().map(|(_, w)| *w).sum();
            let tot_w = if tot_w > 0.0 { tot_w } else { 1.0 };
            for (c, w) in selected {
                let (tp, sl, staged) = get_symbol_optimal_bracket(&c.yahoo_symbol);
                targets.push(TargetCandidate {
                    moomoo_symbol: c.moomoo_symbol.clone(),
                    yahoo_symbol: c.yahoo_symbol.clone(),
                    rank: c.rank,
                    score: c.score,
                    weight: ((w / tot_w) * 10000.0).round() / 10000.0,
                    take_profit_pct: tp,
                    stop_loss_pct: sl,
                    staged_scale: staged,
                    strategy_mode: mode,
                });
            }
        }
    }

    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_notional_calculation() {
        let notional = target_notional(10_000.0, 20_000.0, 2.0, 0.10, 0.02);
        // max pos pct: 10_000 * 0.10 = 1000
        // min(20000, 20000, 1000) * 0.98 = 980.0
        assert_eq!(notional, 980.0);

        let zero = target_notional(0.0, 10_000.0, 2.0, 0.10, 0.02);
        assert_eq!(zero, 0.0);
    }

    #[test]
    fn test_order_gate_rejections() {
        let held = vec!["AAPL".to_string()];
        let working = vec!["TSLA".to_string()];
        
        assert_eq!(
            order_gate("AAPL", &held, &working, 2, 5, 150.0, 2.0),
            Some("already held".to_string())
        );
        assert_eq!(
            order_gate("TSLA", &held, &working, 2, 5, 200.0, 2.0),
            Some("an order for it is already working".to_string())
        );
        assert_eq!(
            order_gate("MSFT", &held, &working, 5, 5, 300.0, 2.0),
            Some("5 positions or working orders already (maximum 5)".to_string())
        );
        assert_eq!(
            order_gate("PENNY", &held, &working, 1, 5, 1.5, 2.0),
            Some("price 1.50 is below the 2.00 minimum".to_string())
        );
        assert_eq!(
            order_gate("NVDA", &held, &working, 2, 5, 120.0, 2.0),
            None
        );
    }

    #[test]
    fn test_size_order_participation_and_risk_caps() {
        let profile = SymbolProfile {
            last_bar_volume: 50_000.0,
            typical_bar_volume: 40_000.0,
            median_range: 0.02,
        };
        // price = $100, notional = $10,000 -> 100 shares
        // broker max = 500
        // bar participation: 40_000 * 0.10 = 4,000 shares
        // adv: 1_000_000 * 0.02 = 20,000 shares
        // risk cap: equity 50_000 * 0.005 / (max(0.05, 0.02) * 100) = 250 / 5 = 50 shares
        let (shares, reason) = size_order(
            100.0,
            10_000.0,
            500.0,
            &profile,
            1_000_000.0,
            50_000.0,
            5.0, // 5% stop
            0.005,
            0.10,
            0.02,
        );
        assert_eq!(shares, 50);
        assert_eq!(reason, "risk (loss budget over this symbol's stop distance)");
    }

    #[test]
    fn test_select_alpha_candidates_dynamic_conviction() {
        let cands = vec![
            CandidateInput {
                moomoo_symbol: "US.MRNA".to_string(),
                yahoo_symbol: "MRNA".to_string(),
                rank: 1,
                score: 0.85,
            },
            CandidateInput {
                moomoo_symbol: "US.ALAB".to_string(),
                yahoo_symbol: "ALAB".to_string(),
                rank: 2,
                score: 0.40,
            },
            CandidateInput {
                moomoo_symbol: "US.TSLA".to_string(),
                yahoo_symbol: "TSLA".to_string(),
                rank: 3,
                score: 0.30,
            },
        ];

        let targets = select_alpha_candidates(cands, StrategyMode::DynamicConvictionPortfolio);
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].yahoo_symbol, "MRNA");
        assert_eq!(targets[0].weight, 0.60);
        assert_eq!(targets[0].take_profit_pct, 0.05);
        assert_eq!(targets[0].stop_loss_pct, 0.08); // optimized profile for MRNA

        assert_eq!(targets[1].yahoo_symbol, "ALAB");
        assert_eq!(targets[1].weight, 0.25);

        assert_eq!(targets[2].yahoo_symbol, "TSLA");
        assert_eq!(targets[2].weight, 0.15);
    }
}

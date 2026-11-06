//! Order gating, risk-aware sizing, and daily plan execution logic in Rust.

#[derive(Clone, Debug, PartialEq)]
pub struct OrderSizing {
    pub shares: usize,
    pub limited_by: &'static str,
}

#[derive(Clone, Debug)]
pub struct SymbolVolumeProfile {
    pub last_bar_volume: f64,
    pub typical_bar_volume: f64,
    pub median_range_pct: f64,
}

/// Why a new buy order must NOT be placed now, or None when it is allowed.
pub fn order_gate(
    symbol: &str,
    held: &std::collections::HashSet<String>,
    working: &std::collections::HashSet<String>,
    open_count: usize,
    max_positions: usize,
    price: f64,
    min_price: f64,
) -> Option<String> {
    if held.contains(symbol) {
        return Some("already held".to_string());
    }
    if working.contains(symbol) {
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

/// Sizing calculation for a symbol based on notional target, broker max,
/// bar participation, ADV participation, and risk budget over stop distance.
/// Sizing calculation for a symbol based on notional target, broker max,
/// bar participation, ADV participation, and risk budget over stop distance,
/// factoring in round-trip transaction costs (slippage and broker fees).
#[allow(clippy::too_many_arguments)]
pub fn size_order(
    price: f64,
    notional: f64,
    broker_max: f64,
    profile: &SymbolVolumeProfile,
    avg_daily_volume: f64,
    equity: f64,
    stop_loss_pct: f64,
    risk_per_trade: f64,
    bar_participation: f64,
    daily_participation: f64,
) -> OrderSizing {
    size_order_with_cost(
        price,
        notional,
        broker_max,
        profile,
        avg_daily_volume,
        equity,
        stop_loss_pct,
        risk_per_trade,
        bar_participation,
        daily_participation,
        5.0, // default 5 bps round-trip cost
    )
}

/// Sizing calculation explicitly factoring in round-trip cost in basis points.
#[allow(clippy::too_many_arguments)]
pub fn size_order_with_cost(
    price: f64,
    notional: f64,
    broker_max: f64,
    profile: &SymbolVolumeProfile,
    avg_daily_volume: f64,
    equity: f64,
    stop_loss_pct: f64,
    risk_per_trade: f64,
    bar_participation: f64,
    daily_participation: f64,
    cost_bps: f64,
) -> OrderSizing {
    if price <= 0.0 || equity <= 0.0 {
        return OrderSizing {
            shares: 0,
            limited_by: "no price or equity",
        };
    }
    let bar = profile.last_bar_volume.min(if profile.typical_bar_volume > 0.0 {
        profile.typical_bar_volume
    } else {
        profile.last_bar_volume
    });
    // Total effective stop distance includes the stop percentage plus round-trip transaction drag
    let cost_pct = cost_bps / 100.0;
    let stop_distance = ((stop_loss_pct + cost_pct) / 100.0).max(profile.median_range_pct);
    let limits = [
        ("money", notional / price),
        ("broker maximum", broker_max),
        ("this symbol's bar volume", bar * bar_participation),
        ("this symbol's average daily volume", avg_daily_volume * daily_participation),
        (
            "risk (loss budget over this symbol's stop distance)",
            equity * risk_per_trade / (stop_distance * price),
        ),
    ];

    let mut min_shares = f64::MAX;
    let mut min_reason = "money";
    for (reason, val) in limits {
        if val < min_shares {
            min_shares = val;
            min_reason = reason;
        }
    }
    OrderSizing {
        shares: min_shares.max(0.0) as usize,
        limited_by: min_reason,
    }
}

/// Equal-dollar position value for EACH candidate so all of them fit within
/// equity * leverage (capped by funds_cap * leverage) and buying_power * (1 - buffer).
pub fn notional_each(
    equity: f64,
    buying_power: f64,
    candidates: usize,
    funds_cap: f64,
    leverage: f64,
    buffer: f64,
) -> f64 {
    if candidates == 0 || equity.min(buying_power) <= 0.0 {
        return 0.0;
    }
    let by_funds = funds_cap * leverage;
    let by_equity = equity * leverage / candidates as f64;
    let by_power = buying_power * (1.0 - buffer) / candidates as f64;
    0.0f64.max(by_funds.min(by_equity).min(by_power))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_order_gate_checks() {
        let mut held = HashSet::new();
        let working = HashSet::new();
        held.insert("US.AAPL".to_string());
        assert_eq!(
            order_gate("US.AAPL", &held, &working, 0, 15, 100.0, 0.01),
            Some("already held".to_string())
        );
        assert_eq!(
            order_gate("US.MSFT", &held, &working, 15, 15, 100.0, 0.01),
            Some("15 positions or working orders already (maximum 15)".to_string())
        );
        assert_eq!(
            order_gate("US.PENNY", &held, &working, 1, 15, 0.005, 0.01),
            Some("price 0.01 is below the 0.01 minimum".to_string())
        );
        assert_eq!(
            order_gate("US.PENNY", &held, &working, 1, 15, 0.05, 0.01),
            None
        );
    }

    #[test]
    fn test_notional_each() {
        let val = notional_each(100_000.0, 150_000.0, 15, 10_000.0, 2.0, 0.01);
        assert!(val > 0.0);
        assert!(val <= 20_000.0);
    }
}

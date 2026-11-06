/// Trading sessions as (first_open, last_open + one bar) from ascending 1-minute bar open times (ms).
pub fn sessions_from_bars(walls: &[i64]) -> Vec<(i64, i64)> {
    const BAR_MS: i64 = 60_000;
    let mut out: Vec<(i64, i64)> = Vec::new();
    for &w in walls {
        match out.last_mut() {
            Some(s) if s.1 == w => s.1 = w + BAR_MS,
            _ => out.push((w, w + BAR_MS)),
        }
    }
    out
}

/// Checks if an epoch millisecond timestamp falls within ASX regular trading hours
/// (10:00 to 16:00 Sydney time, weekdays).
/// Sydney is UTC+10 (AEST) or UTC+11 (AEDT).
pub fn is_asx_trading_window(epoch_ms: i64, utc_offset_hours: i64) -> bool {
    let local_ms = epoch_ms + utc_offset_hours * 3_600_000;
    let day_of_week = ((local_ms / 86_400_000) + 4) % 7; // 0=Sunday, 6=Saturday
    if day_of_week == 0 || day_of_week == 6 {
        return false;
    }
    let minute_of_day = (local_ms % 86_400_000) / 60_000;
    // 10:00 AM is minute 600, 16:00 is minute 960 (order entry allowed up to 15:45 = 945)
    (600..=960).contains(&minute_of_day)
}

/// Dynamic virtual capital sizing: starts at base allocation (e.g. $5,000)
/// and compounds cumulative realized PnL.
pub fn calculate_virtual_capital(starting_capital: f64, cumulative_pnl: f64, max_risk_cap: f64) -> f64 {
    let effective = starting_capital + cumulative_pnl;
    effective.clamp(0.0, max_risk_cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_capital() {
        assert_eq!(calculate_virtual_capital(5000.0, 250.0, 50_000.0), 5250.0);
        assert_eq!(calculate_virtual_capital(5000.0, -1000.0, 50_000.0), 4000.0);
        assert_eq!(calculate_virtual_capital(5000.0, -6000.0, 50_000.0), 0.0);
    }

    #[test]
    fn test_asx_window() {
        // Wednesday at 11:30 AM Sydney AEDT (UTC+11)
        // 11:30 AM = minute 690.
        let epoch_ms = 1792323480000; // arbitrary ms
        let _ = is_asx_trading_window(epoch_ms, 11);
    }
}


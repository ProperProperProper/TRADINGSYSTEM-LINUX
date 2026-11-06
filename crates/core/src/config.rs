//! The one place for the research window settings (owner, 2026-10-18): one 40-session window, one 20-session warm-up, one search over the rest. Everything that needs these numbers reads them from here.

/// Sessions of 1-minute candles used for learning, for every symbol and both sources (moomoo and Yahoo).
/// Yahoo's 1-minute limit is 60 calendar days, which is about this many trading sessions.
/// Whether this system downloads from Yahoo on its own (the trading calendar, 1-minute bars for every symbol, the paper trader's live feed). OFF since
/// 2026-10-19 (owner): another project downloads from yfinance and Yahoo should not be asked by two programs. The manual commands (`fetch-extra`,
/// `fetch-sessions`, `fetch-paper`) still work; the scheduled jobs do not call them while this is false.
pub const YAHOO_DOWNLOADS: bool = false;
pub const WINDOW_SESSIONS: usize = 40;

/// Sessions of history before a symbol is scored. The active research contract
/// is one-minute NASDAQ evidence; session bar counts are derived from the
/// exchange calendar and are never interpreted as a 15-minute interval.
pub const WARMUP_SESSIONS: usize = 20;

/// Sessions the search runs over: the window minus the warm-up (the first sessions only warm the indicators up).
pub const SEARCH_SESSIONS: usize = WINDOW_SESSIONS - WARMUP_SESSIONS;

/// Length of each overlapping search period in sessions: the bot's own search window (`moomoo_bot::engine::TEST_SESSIONS`, checked by a test in thomas-engine).
pub const PERIOD_SESSIONS: usize = 14;

/// Sessions between the starts of consecutive periods (owner 2026-10-18: "as new data comes in add more chunks that overlap"). 20 scored sessions hold
/// periods starting at 0, 3 and 6, so the first run has three; every 3 new sessions add another.
pub const PERIOD_STEP_SESSIONS: usize = 3;

/// Start sessions (calendar indices) of the periods that fit the span `span_first..span_end`: every `step` sessions on the grid through `anchor`, each `len` long.
pub fn grid_starts(
    anchor: usize,
    span_first: usize,
    span_end: usize,
    len: usize,
    step: usize,
) -> Vec<usize> {
    if step == 0 || len == 0 {
        return Vec::new();
    }
    let mut s = if anchor >= span_first {
        anchor - (anchor - span_first) / step * step
    } else {
        anchor + (span_first - anchor).div_ceil(step) * step
    };
    let mut out = Vec::new();
    while s + len <= span_end {
        out.push(s);
        s += step;
    }
    out
}

/// Closed trades a setting needs in one 14-session period to count as usable in the Phase 2 search. The bot's own floor is 8, but measured on the real
/// periods (30,000 draws each, 2026-10-18) it leaves nothing usable: floor 8 gives 0 of 3 periods with usable settings, 6 gives 1, 4 gives 2, 3 gives all 3.
/// 3 is the highest floor at which every period has candidates; results at this floor are low-confidence and are labelled so.
pub const MIN_TRADES_SEARCH: usize = 3;

/// Later, non-overlapping periods a setting must have been tested forward on before it can become champion or be rejected ("multiple periods", owner 2026-10-18).
pub const MIN_FORWARD_PERIODS: usize = 2;

/// Percentage points a challenger's mean forward return must beat the champion's by to replace it: the bot's own promotion margin ("at least +1 point better").
pub const PROMOTION_MARGIN_PCT: f64 = 1.0;

/// Open time (ms) of the first session of the window: the `window` latest sessions of `calendar` (ascending (open, close) pairs)
/// that open at or before `last_ms`. The whole calendar when it has fewer sessions.
pub fn window_start(calendar: &[(i64, i64)], last_ms: i64, window: usize) -> Option<i64> {
    let upto = calendar.partition_point(|&(open, _)| open <= last_ms);
    calendar[..upto]
        .get(upto.saturating_sub(window))
        .map(|s| s.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_latest_n_sessions() {
        let cal: Vec<(i64, i64)> = (0..10).map(|i| (i * 100, i * 100 + 50)).collect();
        assert_eq!(window_start(&cal, 950, 3), Some(700), "sessions 7, 8, 9");
        assert_eq!(window_start(&cal, 950, 10), Some(0));
        assert_eq!(
            window_start(&cal, 950, 40),
            Some(0),
            "fewer sessions than the window: all of them"
        );
        assert_eq!(
            window_start(&cal, 420, 2),
            Some(300),
            "only sessions that opened by last_ms count"
        );
        assert_eq!(window_start(&[], 5, 3), None);
    }

    #[test]
    fn periods_start_every_step_sessions_on_a_fixed_grid() {
        assert_eq!(
            grid_starts(100, 100, 120, PERIOD_SESSIONS, PERIOD_STEP_SESSIONS),
            vec![100, 103, 106],
            "three periods in 20 sessions"
        );
        assert_eq!(
            grid_starts(100, 103, 123, 14, 3),
            vec![103, 106, 109],
            "three new sessions slide the span and add one period on the same grid"
        );
        assert_eq!(
            grid_starts(103, 100, 120, 14, 3),
            vec![100, 103, 106],
            "an anchor inside the span gives the same grid"
        );
        assert_eq!(
            grid_starts(100, 100, 129, 14, 3),
            vec![100, 103, 106, 109, 112, 115]
        );
        assert!(
            grid_starts(100, 100, 110, 14, 3).is_empty() && grid_starts(0, 0, 20, 14, 0).is_empty()
        );
    }

    #[test]
    fn the_settings_leave_twenty_search_sessions() {
        assert_eq!(
            (WINDOW_SESSIONS, WARMUP_SESSIONS, SEARCH_SESSIONS),
            (40, 20, 20)
        );
    }
}

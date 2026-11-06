//! Native Rust 1-minute candle storage, validation and normalization into common1m.sqlite3 format.

use anyhow::Result;
use rusqlite::Connection;

pub const BAR_MS: i64 = 60_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar1m {
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Checks whether a bar may enter the store:
/// On the 1-minute grid, non-negative, valid OHLC, high >= open/close >= low.
pub fn is_acceptable_bar(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> bool {
    let flat_no_trade = v == 0.0 && (o - h).abs() < 1e-9 && (h - l).abs() < 1e-9 && (l - c).abs() < 1e-9;
    (ts % BAR_MS == 0)
        && (v > 0.0 || flat_no_trade)
        && o > 0.0
        && h > 0.0
        && l > 0.0
        && c > 0.0
        && h >= o.max(c)
        && l <= o.min(c)
        && h >= l
        && [o, h, l, c, v].iter().all(|x| x.is_finite())
}

/// Initializes common1m database tables with standard schemas.
pub fn init_common_schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS bars(
            symbol TEXT NOT NULL, ts INTEGER NOT NULL, open REAL NOT NULL, high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL, volume REAL NOT NULL,
            source TEXT NOT NULL DEFAULT 'yahoo', updated_ms INTEGER NOT NULL DEFAULT 0, vol_y REAL, vol_m REAL,
            PRIMARY KEY(symbol, ts)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS baselines(symbol TEXT PRIMARY KEY, avg_volume REAL NOT NULL, computed TEXT NOT NULL) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS hot(symbol TEXT PRIMARY KEY, requested_ms INTEGER NOT NULL, reason TEXT NOT NULL) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS volume_scale(symbol TEXT PRIMARY KEY, scale REAL NOT NULL, updated TEXT NOT NULL) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;",
    )?;
    Ok(())
}

/// Reconciles two volume sources (Moomoo and Yahoo):
/// If both present, averages them; otherwise takes whichever is present.
pub fn reconcile_volume(vol_y: Option<f64>, vol_m: Option<f64>) -> f64 {
    match (vol_y, vol_m) {
        (Some(y), Some(m)) if y > 0.0 && m > 0.0 => (y + m) / 2.0,
        (Some(y), _) => y.max(0.0),
        (_, Some(m)) => m.max(0.0),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_acceptable_bar() {
        assert!(is_acceptable_bar(1788874200000, 10.0, 10.5, 9.8, 10.2, 1000.0));
        // Reject off-grid
        assert!(!is_acceptable_bar(1788874200001, 10.0, 10.5, 9.8, 10.2, 1000.0));
        // Reject high < open
        assert!(!is_acceptable_bar(1788874200000, 10.0, 9.5, 9.0, 9.2, 1000.0));
        // Accept zero volume flat no-trade bar
        assert!(is_acceptable_bar(1788874200000, 10.0, 10.0, 10.0, 10.0, 0.0));
    }

    #[test]
    fn test_reconcile_volume() {
        assert_eq!(reconcile_volume(Some(100.0), Some(200.0)), 150.0);
        assert_eq!(reconcile_volume(Some(100.0), None), 100.0);
        assert_eq!(reconcile_volume(None, Some(200.0)), 200.0);
    }
}

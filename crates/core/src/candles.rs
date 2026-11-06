//! Loads stored candles from a snapshot database and validates them.
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Clone, Copy)]
pub struct Bar {
    pub ts_ms: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Bars per symbol, sorted by time.
pub type Candles = BTreeMap<String, Vec<Bar>>;

pub fn load(db: &Path) -> Result<Candles> {
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare(
        "SELECT symbol, wall_ms, open, high, low, close, volume FROM candles ORDER BY symbol, wall_ms",
    )?;
    let mut out = Candles::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Bar {
                ts_ms: r.get(1)?,
                open: r.get::<_, Option<f64>>(2)?.unwrap_or(f64::NAN),
                high: r.get::<_, Option<f64>>(3)?.unwrap_or(f64::NAN),
                low: r.get::<_, Option<f64>>(4)?.unwrap_or(f64::NAN),
                close: r.get::<_, Option<f64>>(5)?.unwrap_or(f64::NAN),
                volume: r.get::<_, Option<f64>>(6)?.unwrap_or(f64::NAN),
            },
        ))
    })?;
    for row in rows {
        let (sym, bar) = row?;
        out.entry(sym).or_default().push(bar);
    }
    Ok(out)
}

#[derive(Debug, Default, Serialize)]
pub struct SymbolReport {
    pub bars: usize,
    pub bad_ohlc: usize,
    pub non_positive_price: usize,
    pub zero_volume: usize,
    /// Gaps longer than 3x the median spacing that are not overnight/weekend breaks.
    pub intraday_gaps: usize,
    pub median_step_ms: i64,
    pub first_ms: i64,
    pub last_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub symbols: usize,
    pub bars: usize,
    pub bad_symbols: Vec<String>,
    pub per_symbol: BTreeMap<String, SymbolReport>,
}

const SIX_HOURS_MS: i64 = 6 * 3600 * 1000;

pub fn validate(c: &Candles) -> Report {
    let mut per_symbol = BTreeMap::new();
    let mut bad_symbols = Vec::new();
    let mut total = 0;
    for (sym, bars) in c {
        total += bars.len();
        let mut r = SymbolReport {
            bars: bars.len(),
            ..Default::default()
        };
        let mut steps: Vec<i64> = bars.windows(2).map(|w| w[1].ts_ms - w[0].ts_ms).collect();
        steps.sort_unstable();
        r.median_step_ms = steps.get(steps.len() / 2).copied().unwrap_or(0);
        r.first_ms = bars.first().map_or(0, |b| b.ts_ms);
        r.last_ms = bars.last().map_or(0, |b| b.ts_ms);
        for b in bars {
            let prices = [b.open, b.high, b.low, b.close];
            if prices.iter().any(|p| !p.is_finite() || *p <= 0.0) {
                r.non_positive_price += 1;
            } else if b.high < b.low || b.high < b.open.max(b.close) || b.low > b.open.min(b.close)
            {
                r.bad_ohlc += 1;
            }
            if b.volume.is_nan() || b.volume <= 0.0 {
                r.zero_volume += 1;
            }
        }
        for w in bars.windows(2) {
            let d = w[1].ts_ms - w[0].ts_ms;
            // Shorter than 6h and more than 3x normal spacing = missing intraday bars.
            if d > 3 * r.median_step_ms && d < SIX_HOURS_MS {
                r.intraday_gaps += 1;
            }
        }
        if r.bad_ohlc + r.non_positive_price > 0 {
            bad_symbols.push(sym.clone());
        }
        per_symbol.insert(sym.clone(), r);
    }
    Report {
        symbols: c.len(),
        bars: total,
        bad_symbols,
        per_symbol,
    }
}

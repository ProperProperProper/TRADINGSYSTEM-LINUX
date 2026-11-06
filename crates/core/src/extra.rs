//! Fetches real Yahoo 1-minute bars for symbols moomoo has no candles for, into `candles_extra`.
use anyhow::Result;
use rusqlite::Connection;
use std::{thread::sleep, time::Duration};

use crate::symbols::yahoo_symbol;
use crate::yahoo::{self, Client, Fetch, MAX_1M_DAYS};

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Default)]
pub struct Summary {
    pub symbols: usize,
    pub with_bars: usize,
    pub no_data: usize,
    pub bars: usize,
}

/// Every symbol in the map as (symbol, has moomoo candles). Hard rule: data operations always cover all symbols, never a subset.
pub fn all_symbols(conn: &Connection) -> Result<Vec<(String, bool)>> {
    let mut st = conn
        .prepare("SELECT moomoo_symbol, has_moomoo_candles FROM symbols ORDER BY moomoo_symbol")?;
    Ok(st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?
        .collect::<std::result::Result<_, _>>()?)
}

pub fn run(conn: &Connection, now_ms: i64) -> Result<Summary> {
    run_symbols(conn, now_ms, &all_symbols(conn)?, MAX_1M_DAYS)
}

/// A 1-minute bar is final only once its 1 minutes are over; the bar in progress carries partial volume and a close that is not final.
pub const BAR_MS: i64 = 60_000;

/// The same for the given symbols and the last `days` days: the live feed of the local paper trader (a few days of a few dozen symbols, not the whole window).
/// Bars still in progress at `now_ms` are never stored.
pub fn run_symbols(
    conn: &Connection,
    now_ms: i64,
    symbols: &[(String, bool)],
    days: i64,
) -> Result<Summary> {
    let client = Client::new()?;
    let from = now_ms - days * DAY_MS;
    let mut sum = Summary::default();
    for (sym, has_moomoo) in symbols.iter().cloned() {
        let ysym = yahoo_symbol(&sym);
        let (returned, name, bars) = match client.bars_1m(&ysym, from / 1000, now_ms / 1000)? {
            Fetch::Bars {
                returned,
                name,
                bars,
            } if returned.eq_ignore_ascii_case(&ysym) => (Some(returned), Some(name), bars),
            Fetch::Bars { returned, name, .. } => (Some(returned), Some(name), Vec::new()),
            Fetch::Unavailable(_) => (None, None, Vec::new()),
        };
        // Symbols with moomoo candles keep their price-agreement row from `reconcile`; only candle-less ones get this identity row.
        // Do this after the request so a transient Yahoo failure does not erase the last successful check.
        if !has_moomoo {
            conn.execute("DELETE FROM source_checks WHERE moomoo_symbol=?", [&sym])?;
        }
        let bars: Vec<_> = bars
            .into_iter()
            .filter(|b| b.ts_ms + BAR_MS <= now_ms)
            .collect();
        sleep(Duration::from_millis(300));
        sum.symbols += 1;
        if bars.is_empty() {
            sum.no_data += 1
        } else {
            sum.with_bars += 1
        }
        let tx = conn.unchecked_transaction()?;
        for b in &bars {
            tx.execute(
                "INSERT OR REPLACE INTO candles_extra VALUES(?,?,?,?,?,?,?,?,?,?)",
                (
                    &sym,
                    &ysym,
                    b.ts_ms,
                    b.open,
                    b.high,
                    b.low,
                    b.close,
                    b.volume,
                    yahoo::SOURCE,
                    now_ms,
                ),
            )?;
        }
        if !has_moomoo {
            tx.execute(
                "INSERT INTO source_checks(ts_ms,moomoo_symbol,yahoo_symbol,yahoo_returned_symbol,yahoo_name,overlap_bars,missing_in_yahoo) VALUES(?,?,?,?,?,0,0)",
                (now_ms, &sym, &ysym, &returned, &name),
            )?;
        }
        tx.commit()?;
        sum.bars += bars.len();
    }
    Ok(sum)
}

/// Regular-session length: 09:30 to 16:00 ET.
pub const SESSION_MS: i64 = 23_400_000;

/// Fetches the real trading calendar (SPY daily bars) from `from_s` to now into `sessions_calendar`.
pub fn fetch_sessions(conn: &Connection, from_s: i64, now_ms: i64) -> Result<usize> {
    let opens = Client::new()?.daily_opens("SPY", from_s, now_ms / 1000)?;
    let tx = conn.unchecked_transaction()?;
    for o in &opens {
        tx.execute(
            "INSERT OR REPLACE INTO sessions_calendar VALUES(?,?,?,?)",
            (o, o + SESSION_MS, yahoo::SOURCE, now_ms),
        )?;
    }
    tx.commit()?;
    Ok(opens.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    #[test]
    fn all_symbols_returns_every_symbol_with_its_moomoo_flag() {
        let c = Connection::open_in_memory().unwrap();
        store::init(&c).unwrap();
        for (t, spy, has) in [("AAA", 1, 1), ("BBB", 1, 0), ("CCC", 0, 0)] {
            c.execute("INSERT INTO symbols VALUES(?,?,?,'n','tradable',?,0,?,NULL,NULL,'unverified',NULL,NULL,NULL,NULL,'t',0)", (t, "US(QotMarket=11)", t, spy, has)).unwrap();
        }
        assert_eq!(
            all_symbols(&c).unwrap(),
            vec![
                ("AAA".to_string(), true),
                ("BBB".to_string(), false),
                ("CCC".to_string(), false)
            ]
        );
    }
}

//! Reads native Moomoo 1-minute bars from papercompmoomoo's local SQLite store
//! into this research engine's secondary-candle table. This import is local-only
//! and excludes any legacy rows marked as Yahoo.
//!
//! Only what is new is read: for each symbol the import starts at the newest bar `candles_extra` already holds (minus one bar, so a bar stored while it was still
//! in progress is replaced by its final values). Every row is checked before it is stored: the ticker must be in this system's map, the time must sit on a
//! 1-minute boundary, the prices must be a plausible bar, and the bar must be closed (a bar in progress has partial volume).
use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::Path;

use crate::candles::Bar;
use crate::extra::BAR_MS;
use crate::symbols::yahoo_symbol;
use crate::yahoo;

/// How far before the previous import the changed rows are looked for again (an import that overlaps a write is not missed; reading a row twice changes nothing).
const REREAD_MS: i64 = 120_000;

/// The other project's candle store (relative to this project's folder).
pub const EXTERNAL_DB: &str = "papercompmoomoo/data/common1m.sqlite3";
pub const SYMBOL_MAP: &str = "data/symbol_map.csv";
const IMPORT_SOURCE: &str = "moomoo";

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// Symbols of the map that gained bars.
    pub symbols: usize,
    pub bars: usize,
    /// Tickers in the other store that are not in this system's map.
    pub unknown_tickers: usize,
    /// Rows refused: off the 1-minute grid, implausible prices, or a bar still in progress.
    pub rejected_rows: usize,
}

/// Imports the new bars of every symbol in the map from the SQLite store at `db`. A missing store is an error (nothing to import is not the same as nothing wrong).
pub fn import_store(conn: &Connection, db: &Path, now_ms: i64) -> Result<Summary> {
    import_store_inner(conn, db, None, now_ms)
}

/// Imports against the canonical symbol map even when the derived `symbols`
/// table is empty (for example after an upgrade or on a fresh clone).
pub fn import_store_mapped(
    conn: &Connection,
    db: &Path,
    map: &Path,
    now_ms: i64,
) -> Result<Summary> {
    import_store_inner(conn, db, Some(map), now_ms)
}

fn import_store_inner(
    conn: &Connection,
    db: &Path,
    map: Option<&Path>,
    now_ms: i64,
) -> Result<Summary> {
    let other = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open {}", db.display()))?;
    other.busy_timeout(std::time::Duration::from_secs(30))?;
    let mut by_yahoo: HashMap<String, (String, String)> = HashMap::new();
    let mut st = conn.prepare("SELECT moomoo_symbol FROM symbols")?;
    for s in st.query_map([], |r| r.get::<_, String>(0))? {
        let m = s?;
        let y = yahoo_symbol(&m);
        by_yahoo.insert(y.to_uppercase(), (m, y));
    }
    if let Some(map) = map {
        by_yahoo.clear();
        let text = std::fs::read_to_string(map)
            .with_context(|| format!("cannot read canonical symbol map {}", map.display()))?;
        for line in text.lines().skip(1) {
            let fields: Vec<_> = line.splitn(4, ',').collect();
            if fields.len() >= 3 && !fields[0].is_empty() && !fields[2].is_empty() {
                by_yahoo.insert(
                    fields[2].to_uppercase(),
                    (fields[0].to_owned(), fields[2].to_owned()),
                );
            }
        }
        anyhow::ensure!(!by_yahoo.is_empty(), "canonical symbol map is empty");
    }
    let mut have: HashMap<String, i64> = HashMap::new();
    let mut st = conn
        .prepare("SELECT moomoo_symbol, max(wall_ms) FROM candles_extra GROUP BY moomoo_symbol")?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (m, ts) = r?;
        have.insert(m, ts);
    }
    // Rows changed since the last import are read again, but only while their
    // current source remains Moomoo. Legacy Yahoo rows are never imported.
    let has_source = other
        .prepare("SELECT 1 FROM pragma_table_info('bars') WHERE name='source'")?
        .exists([])?;
    anyhow::ensure!(has_source, "Moomoo candle store has no source column; refusing ambiguous legacy bars");
    let stamped = other
        .prepare("SELECT 1 FROM pragma_table_info('bars') WHERE name='updated_ms'")?
        .exists([])?;
    // the time of the last read is kept on its own (import_state), not guessed from the rows: nothing else that writes candles_extra can move it. A store never read
    // before has no mark, so the first read takes every row.
    let source = db.display().to_string();
    let mark: Option<i64> = conn
        .query_row(
            "SELECT last_ms FROM import_state WHERE source=?",
            [&source],
            |r| r.get(0),
        )
        .ok();
    let changed_since = mark.map_or(0, |m| (m - REREAD_MS).max(0));
    let tickers: Vec<String> = other
        .prepare("SELECT DISTINCT symbol FROM bars WHERE source='moomoo'")?
        .query_map([], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut sum = Summary::default();
    for ticker in tickers {
        let Some((sym, ysym)) = by_yahoo.get(&ticker.to_uppercase().replace('.', "-")) else {
            sum.unknown_tickers += 1;
            continue;
        };
        let from = have.get(sym).map_or(0, |t| t - BAR_MS);
        let mut q = other.prepare(if stamped {
            "SELECT ts, open, high, low, close, volume FROM bars WHERE symbol=?1 AND source='moomoo' AND (ts>=?2 OR updated_ms>?3) ORDER BY ts"
        } else {
            "SELECT ts, open, high, low, close, volume FROM bars WHERE symbol=?1 AND source='moomoo' AND ts>=?2 AND ?3=?3 ORDER BY ts"
        })?;
        let rows: Vec<Bar> = q
            .query_map((&ticker, from, changed_since), |r| {
                Ok(Bar {
                    ts_ms: r.get(0)?,
                    open: r.get(1)?,
                    high: r.get(2)?,
                    low: r.get(3)?,
                    close: r.get(4)?,
                    volume: r.get(5)?,
                })
            })?
            .collect::<std::result::Result<_, _>>()?;
        let tx = conn.unchecked_transaction()?;
        let mut stored = 0;
        for b in rows {
            if b.ts_ms.rem_euclid(BAR_MS) != 0 || b.ts_ms + BAR_MS > now_ms || !yahoo::sane(&b) {
                sum.rejected_rows += 1;
                continue;
            }
            tx.execute(
                "INSERT OR REPLACE INTO candles_extra VALUES(?,?,?,?,?,?,?,?,?,?)",
                (
                    sym,
                    ysym,
                    b.ts_ms,
                    b.open,
                    b.high,
                    b.low,
                    b.close,
                    b.volume,
                    IMPORT_SOURCE,
                    now_ms,
                ),
            )?;
            stored += 1;
        }
        tx.commit()?;
        if stored > 0 {
            sum.symbols += 1;
            sum.bars += stored;
        }
    }
    conn.execute(
        "INSERT OR REPLACE INTO import_state(source, last_ms) VALUES(?,?)",
        (&source, now_ms),
    )?;
    Ok(sum)
}

/// New trading sessions from the reference symbol's stored 1-minute bars, with no network request: a session is a UTC day on which SPY has bars, it opens at that
/// day's first bar and is `SESSION_MS` long (the same length the Yahoo calendar used). Days that already have a calendar row are left alone. Returns the sessions added.
pub fn derive_sessions(conn: &Connection, now_ms: i64) -> Result<usize> {
    const DAY_MS: i64 = 86_400_000;
    let mut st = conn.prepare("SELECT min(wall_ms) FROM candles_extra WHERE moomoo_symbol='SPY' GROUP BY wall_ms / ?1 ORDER BY 1")?;
    let opens: Vec<i64> = st
        .query_map([DAY_MS], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut added = 0;
    for open in opens {
        let known: i64 = conn.query_row(
            "SELECT count(*) FROM sessions_calendar WHERE open_ms / ?1 = ?2 / ?1",
            (DAY_MS, open),
            |r| r.get(0),
        )?;
        if known == 0 {
            conn.execute(
                "INSERT INTO sessions_calendar VALUES(?,?,?,?)",
                (open, open + crate::extra::SESSION_MS, "candles", now_ms),
            )?;
            added += 1;
        }
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    const T0: i64 = 1_792_330_200_000; // 2026-10-18 13:30:00 UTC, on the 1-minute grid

    fn store_with(symbols: &[&str]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::init(&c).unwrap();
        for s in symbols {
            c.execute("INSERT INTO symbols VALUES(?,?,?,'n','tradable',1,0,0,NULL,NULL,'unverified',NULL,NULL,NULL,NULL,'t',0)", (s, "US(QotMarket=11)", s)).unwrap();
        }
        c
    }

    fn other_store(path: &Path, rows: &[(&str, i64, f64, f64, f64, f64, f64)]) {
        let o = Connection::open(path).unwrap();
        o.execute_batch("CREATE TABLE bars(symbol TEXT NOT NULL, ts INTEGER NOT NULL, open REAL, high REAL, low REAL, close REAL, volume REAL, source TEXT NOT NULL DEFAULT 'moomoo', updated_ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(symbol, ts))").unwrap();
        for r in rows {
            o.execute(
                "INSERT INTO bars(symbol,ts,open,high,low,close,volume) VALUES(?,?,?,?,?,?,?)",
                (r.0, r.1, r.2, r.3, r.4, r.5, r.6),
            )
            .unwrap();
        }
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("thomas-ingest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn canonical_map_import_does_not_depend_on_the_derived_symbols_table() {
        let d = temp("canonical-map");
        let db = d.join("c.sqlite3");
        let map = d.join("map.csv");
        other_store(&db, &[("BRK-B", T0, 100.0, 101.0, 99.0, 100.5, 50.0)]);
        std::fs::write(
            &map,
            "moomoo_symbol,moomoo_market,yahoo_symbol,name\nBRK.B,US,BRK-B,Berkshire\n",
        )
        .unwrap();
        let c = store_with(&[]);
        let summary = import_store_mapped(&c, &db, &map, T0 + BAR_MS).unwrap();
        assert_eq!(
            (summary.symbols, summary.bars, summary.unknown_tickers),
            (1, 1, 0)
        );
        assert_eq!(
            c.query_row("SELECT moomoo_symbol FROM candles_extra", [], |row| row
                .get::<_, String>(
                0
            ))
            .unwrap(),
            "BRK.B"
        );
    }

    #[test]
    fn only_closed_aligned_plausible_bars_of_known_symbols_are_stored_and_a_second_import_changes_nothing()
     {
        let d = temp("a");
        let db = d.join("c.sqlite3");
        other_store(
            &db,
            &[
                ("AAA", T0, 10.0, 11.0, 9.0, 10.5, 1000.0),
                ("AAA", T0 + BAR_MS, 10.0, 11.0, 9.0, 10.5, 1000.0),
                ("AAA", T0 + BAR_MS + 300_000, 10.0, 11.0, 9.0, 10.5, 1000.0), // off the grid
                ("AAA", T0 + 2 * BAR_MS, 10.0, 9.0, 11.0, 10.5, 1000.0),       // high below low
                ("AAA", T0 + 3 * BAR_MS, 10.0, 11.0, 9.0, 10.5, 1000.0), // still in progress at "now"
                ("BRK-B", T0, 100.0, 101.0, 99.0, 100.5, 50.0),
                ("ZZZ", T0, 1.0, 2.0, 1.0, 1.5, 5.0),
            ],
        );
        let c = store_with(&["AAA", "BRK.B"]);
        let now = T0 + 3 * BAR_MS + 600_000; // the bar starting at T0 + 3 bars ends after now
        let s = import_store(&c, &db, now).unwrap();
        assert_eq!((s.symbols, s.bars, s.unknown_tickers), (2, 3, 1), "{s:?}");
        assert_eq!(
            s.rejected_rows, 3,
            "off-grid, high below low, in progress: {s:?}"
        );
        let y: String = c
            .query_row(
                "SELECT yahoo_symbol FROM candles_extra WHERE moomoo_symbol='BRK.B'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(y, "BRK-B");
        let again = import_store(&c, &db, now).unwrap();
        assert_eq!(
            again.bars, 3,
            "only the last stored bar and the one before it are read again (AAA twice, BRK-B once), not the whole history"
        );
        let n: i64 = c
            .query_row("SELECT count(*) FROM candles_extra", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3, "re-importing stores nothing twice");
        assert!(import_store(&c, &d.join("missing.sqlite3"), now).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bar_stored_in_progress_is_replaced_by_its_final_values_on_the_next_import() {
        let d = temp("b");
        let db = d.join("c.sqlite3");
        other_store(
            &db,
            &[
                ("AAA", T0, 10.0, 11.0, 9.0, 10.5, 1000.0),
                ("AAA", T0 + BAR_MS, 10.0, 11.0, 9.0, 10.5, 1000.0),
            ],
        );
        let c = store_with(&["AAA"]);
        import_store(&c, &db, T0 + 2 * BAR_MS).unwrap();
        // the other project later replaces its last bar with the final values and adds the next one
        let o = Connection::open(&db).unwrap();
        o.execute(
            "UPDATE bars SET close=10.9, volume=2500 WHERE ts=?",
            [T0 + BAR_MS],
        )
        .unwrap();
        o.execute(
            "INSERT INTO bars(symbol,ts,open,high,low,close,volume) VALUES('AAA', ?, 10.9, 11.5, 10.8, 11.2, 800)",
            [T0 + 2 * BAR_MS],
        )
        .unwrap();
        import_store(&c, &db, T0 + 3 * BAR_MS).unwrap();
        let (close, volume): (f64, f64) = c
            .query_row(
                "SELECT close, volume FROM candles_extra WHERE wall_ms=?",
                [T0 + BAR_MS],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((close, volume), (10.9, 2500.0));
        let n: i64 = c
            .query_row("SELECT count(*) FROM candles_extra", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_legacy_yahoo_revision_is_not_imported_over_a_moomoo_bar()
     {
        let d = temp("c");
        let db = d.join("c.sqlite3");
        let o = Connection::open(&db).unwrap();
        o.execute_batch("CREATE TABLE bars(symbol TEXT NOT NULL, ts INTEGER NOT NULL, open REAL, high REAL, low REAL, close REAL, volume REAL, source TEXT, updated_ms INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(symbol, ts))").unwrap();
        for k in 0..3 {
            o.execute(
                "INSERT INTO bars VALUES('AAA', ?, 10, 11, 9, 10.5, 1000, 'moomoo', 5)",
                [T0 + k * BAR_MS],
            )
            .unwrap();
        }
        let c = store_with(&["AAA"]);
        let now = T0 + 10 * BAR_MS;
        import_store(&c, &db, now).unwrap();
        // something else writes candles_extra with a far later time (a manual fetch): the mark must not move with it
        c.execute(
            "INSERT INTO candles_extra VALUES('AAA','AAA',?,1,2,1,1,10,'yahoo',?)",
            (T0 - BAR_MS, now + 1_000_000),
        )
        .unwrap();
        // A legacy Yahoo row must not replace the native Moomoo version.
        o.execute(
            "UPDATE bars SET volume=2222, source='yahoo', updated_ms=? WHERE ts=?",
            (now + 1_000, T0),
        )
        .unwrap();
        let again = import_store(&c, &db, now + 5_000).unwrap();
        assert_eq!(again.bars, 0, "legacy Yahoo data is excluded: {again:?}");
        let v: f64 = c
            .query_row(
                "SELECT volume FROM candles_extra WHERE wall_ms=?",
                [T0],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, 1000.0, "the imported Moomoo bar remains authoritative");
        let mark: i64 = c
            .query_row("SELECT last_ms FROM import_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mark, now + 5_000, "the mark is the time of the last read");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sessions_come_from_spy_bars_once_per_day_and_known_days_are_kept() {
        let c = store_with(&["SPY"]);
        let day = 86_400_000_i64;
        for w in [T0, T0 + 60_000, T0 + day, T0 + day + 60_000, T0 + 2 * day] {
            c.execute(
                "INSERT INTO candles_extra VALUES('SPY','SPY',?,1,2,1,1,10,'yahoo',0)",
                [w],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO sessions_calendar VALUES(?,?,'yahoo',0)",
            (T0, T0 + crate::extra::SESSION_MS),
        )
        .unwrap();
        assert_eq!(
            derive_sessions(&c, 5).unwrap(),
            2,
            "day 1 was already known"
        );
        assert_eq!(derive_sessions(&c, 6).unwrap(), 0, "nothing twice");
        let opens: Vec<i64> = c
            .prepare("SELECT open_ms FROM sessions_calendar ORDER BY open_ms")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(opens, [T0, T0 + day, T0 + 2 * day]);
    }
}

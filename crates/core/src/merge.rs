//! One candle base from two sources: moomoo is primary, legacy provider fills only what moomoo lacks (a symbol without candles, a missing
//! bar, or a zero-volume bar inside legacy provider's window). Every row says which source it came from. Nothing is invented: legacy provider's
//! turnover, which it does not report, is estimated as volume x (high + low + close) / 3 and flagged.
use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::symbols::external_symbol;

const BAR_MS: i64 = 60_000;

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub moomoo_bars: usize,
    pub external_bars: usize,
    pub external_replaced_zero_volume: usize,
    pub placeholder_bars_removed: usize,
    pub symbols: usize,
    pub external_only_symbols: usize,
    /// Flat zero-volume bars for no-trade slots (see `carry_fill`).
    pub carry_bars: usize,
}

/// First and last bar time of every run of consecutive 1-minute bars (the session edges).
pub fn session_edges(ts: &BTreeSet<i64>) -> HashSet<i64> {
    let mut edges = HashSet::new();
    let mut prev: Option<i64> = None;
    for &t in ts {
        if prev.is_none_or(|p| t - p != BAR_MS) {
            edges.insert(t);
            if let Some(p) = prev {
                edges.insert(p);
            }
        }
        prev = Some(t);
    }
    if let Some(p) = prev {
        edges.insert(p);
    }
    edges
}

/// Market calendar: bar times at which at least half of the symbols already listed at that time have a bar.
pub fn active_calendar(sym_ts: &BTreeMap<String, Vec<i64>>) -> BTreeSet<i64> {
    let mut count: BTreeMap<i64, usize> = BTreeMap::new();
    for ts in sym_ts.values() {
        for t in ts {
            *count.entry(*t).or_default() += 1;
        }
    }
    let mut firsts: Vec<i64> = sym_ts.values().filter_map(|v| v.first().copied()).collect();
    firsts.sort_unstable();
    count
        .into_iter()
        .filter(|(t, n)| *n * 2 >= firsts.partition_point(|f| f <= t))
        .map(|(t, _)| t)
        .collect()
}

/// Calendar slots a symbol has no bar for, between its first bar and its last (plus trailing slots up to the calendar end when
/// the symbol traded in the final session). The slot is a no-trade interval: moomoo's own convention for it is a flat bar at
/// the previous close with zero volume (seen for CIEN), and that is what is returned: (slot, previous close).
pub fn carry_fill(ts: &[(i64, f64)], calendar: &BTreeSet<i64>) -> Vec<(i64, f64)> {
    let (Some(&(first, _)), Some(&(last, _))) = (ts.first(), ts.last()) else {
        return vec![];
    };
    let have: HashSet<i64> = ts.iter().map(|x| x.0).collect();
    let cal_end = calendar.iter().next_back().copied().unwrap_or(last);
    // still trading in the final session = a real bar within one session (26 bars) of the calendar end
    let end = if cal_end - last <= 26 * BAR_MS {
        cal_end
    } else {
        last
    };
    let mut out = Vec::new();
    let mut prev_close = ts[0].1;
    let mut idx = 0;
    for &slot in calendar.range(first..=end) {
        while idx < ts.len() && ts[idx].0 <= slot {
            prev_close = ts[idx].1;
            idx += 1;
        }
        if !have.contains(&slot) {
            out.push((slot, prev_close));
        }
    }
    out
}

/// Turnover estimate for a legacy provider bar (legacy provider reports none).
pub fn estimate_turnover(volume: f64, high: f64, low: f64, close: f64) -> f64 {
    volume * (high + low + close) / 3.0
}

/// Copies the bot's store to `to` with SQLite's online backup (read-only on the source), then checks the copy.
pub fn snapshot(from: &Path, to: &Path) -> Result<()> {
    let src = Connection::open_with_flags(from, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tmp = to.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut dst = Connection::open(&tmp)?;
        let b = rusqlite::backup::Backup::new(&src, &mut dst)?;
        b.run_to_completion(256, std::time::Duration::from_millis(5), None)?;
    }
    let check: String =
        Connection::open(&tmp)?.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    anyhow::ensure!(check == "ok", "snapshot integrity check failed: {check}");
    std::fs::rename(&tmp, to)?;
    Ok(())
}

pub fn run(ours: &Connection, snapshot: &Path) -> Result<Summary> {
    // A machine with no moomoo snapshot yet (a fresh install, or cleared data) gets a legacy provider-only base; moomoo rows replace it later.
    let bot = if snapshot.exists() {
        Some(Connection::open_with_flags(
            snapshot,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?)
    } else {
        None
    };
    let mut sum = Summary::default();

    let mut placeholders: Vec<(String, i64, i64)> = Vec::new();
    {
        let mut st = ours.prepare(
            "SELECT moomoo_symbol, first_ms, last_ms FROM data_gaps WHERE kind='placeholder'",
        )?;
        for r in st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
            placeholders.push(r?);
        }
    }

    // moomoo bars: (symbol, ts) -> (ohlc, volume, turnover)
    type Row = (f64, f64, f64, f64, f64, f64);
    let mut moomoo: BTreeMap<(String, i64), Row> = BTreeMap::new();
    if let Some(bot) = &bot {
        let has_candles: bool = bot
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='candles'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if has_candles {
            let mut st = bot.prepare("SELECT symbol, wall_ms, open, high, low, close, volume, turnover FROM candles WHERE kind='trades'")?;
            for r in st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    (
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ),
                ))
            })? {
                let (s, w, row): (String, i64, Row) = r?;
                if placeholders
                    .iter()
                    .any(|(ps, a, z)| *ps == s && w >= *a && w <= *z)
                {
                    sum.placeholder_bars_removed += 1;
                } else {
                    moomoo.insert((s, w), row);
                }
            }
        }
    }
    sum.moomoo_bars = moomoo.len();

    // legacy provider archive: only what moomoo lacks.
    let mut external: Vec<(String, i64, f64, f64, f64, f64, f64)> = Vec::new();
    {
        let mut st = ours.prepare("SELECT moomoo_symbol, wall_ms, open, high, low, close, volume FROM candles_extra ORDER BY moomoo_symbol, wall_ms")?;
        for r in st.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })? {
            external.push(r?);
        }
    }
    // Session edges from the timestamps most symbols traded at.
    let mut by_symbol: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for y in &external {
        by_symbol.entry(y.0.clone()).or_default().push(y.1);
    }
    let calendar = active_calendar(&by_symbol);
    let edges = session_edges(&calendar);

    // moomoo rows keep the snapshot's prices. Their volume is the common database's for that bar (the competition project's converter has put Moomoo's volume into
    // legacy provider's unit and averaged it with legacy provider's there, so both bots read the same number); where the common database has no bar the snapshot's own volume stands.
    let common_volume: std::collections::HashMap<(&str, i64), f64> = external
        .iter()
        .filter(|y| y.6 > 0.0)
        .map(|y| ((y.0.as_str(), y.1), y.6))
        .collect();
    let tx = ours.unchecked_transaction()?;
    tx.execute("DELETE FROM candles_merged", [])?;
    {
        let mut ins = tx.prepare("INSERT INTO candles_merged VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")?;
        for ((s, w), (o, h, l, c, v, t)) in &moomoo {
            let blended = common_volume
                .get(&(s.as_str(), *w))
                .copied()
                .filter(|_| *v > 0.0)
                .unwrap_or(*v);
            let f = if *v > 0.0 { blended / v } else { 1.0 };
            ins.execute((
                s,
                external_symbol(s),
                w,
                o,
                h,
                l,
                c,
                blended,
                t * f,
                "moomoo",
                0,
                1,
            ))?;
        }
        let moomoo_symbols: HashSet<&str> = moomoo.keys().map(|k| k.0.as_str()).collect();
        let mut external_only: HashSet<&str> = HashSet::new();
        for (s, w, o, h, l, c, v) in &external {
            if v.is_nan() || *v <= 0.0 {
                continue; // no real legacy provider volume: nothing to add
            }
            let existing = moomoo.get(&(s.clone(), *w));
            let replace_zero = match existing {
                Some(row) if row.4 > 0.0 => continue, // moomoo has a real bar: it wins
                Some(_) => true,
                None => false,
            };
            let turnover = estimate_turnover(*v, *h, *l, *c);
            let reliable = i32::from(!edges.contains(w));
            if replace_zero {
                tx.execute(
                    "DELETE FROM candles_merged WHERE moomoo_symbol=? AND wall_ms=?",
                    (s, w),
                )?;
                sum.external_replaced_zero_volume += 1;
            }
            ins.execute((
                s,
                external_symbol(s),
                w,
                o,
                h,
                l,
                c,
                v,
                turnover,
                "external",
                1,
                reliable,
            ))?;
            sum.external_bars += 1;
            if !moomoo_symbols.contains(s.as_str()) {
                external_only.insert(s.as_str());
            }
        }
        sum.external_only_symbols = external_only.len();
    }
    // Carry fill: no-trade slots, flagged, lowest priority (any real bar replaces it on the next merge).
    {
        let mut rows: BTreeMap<String, Vec<(i64, f64)>> = BTreeMap::new();
        {
            let mut st = tx.prepare("SELECT moomoo_symbol, wall_ms, close FROM candles_merged ORDER BY moomoo_symbol, wall_ms")?;
            for r in st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })? {
                let (s, w, c) = r?;
                rows.entry(s).or_default().push((w, c));
            }
        }
        let cal = active_calendar(
            &rows
                .iter()
                .map(|(s, v)| (s.clone(), v.iter().map(|x| x.0).collect()))
                .collect(),
        );
        let mut ins = tx.prepare("INSERT INTO candles_merged VALUES(?,?,?,?,?,?,?,?,?,?,?,?)")?;
        for (s, v) in &rows {
            for (slot, close) in carry_fill(v, &cal) {
                ins.execute((
                    s,
                    external_symbol(s),
                    slot,
                    close,
                    close,
                    close,
                    close,
                    0.0,
                    0.0,
                    "carry",
                    0,
                    1,
                ))?;
                sum.carry_bars += 1;
            }
        }
    }
    sum.symbols = tx.query_row(
        "SELECT count(DISTINCT moomoo_symbol) FROM candles_merged",
        [],
        |r| r.get(0),
    )?;
    tx.commit()?;
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;

    #[test]
    fn snapshot_copies_a_database_and_leaves_the_source_alone() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a.sqlite3"), d.path().join("b.sqlite3"));
        Connection::open(&a)
            .unwrap()
            .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES(1),(2);")
            .unwrap();
        snapshot(&a, &b).unwrap();
        assert_eq!(
            Connection::open(&b)
                .unwrap()
                .query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert!(
            snapshot(
                &d.path().join("missing.sqlite3"),
                &d.path().join("c.sqlite3")
            )
            .is_err()
        );
    }

    #[test]
    fn a_later_moomoo_bar_overwrites_the_external_bar() {
        let d = tempfile::tempdir().unwrap();
        let snap = d.path().join("snap.sqlite3");
        let b = Connection::open(&snap).unwrap();
        b.execute_batch("CREATE TABLE candles(symbol TEXT, kind TEXT, wall_ms INTEGER, open REAL, high REAL, low REAL, close REAL, volume REAL, turnover REAL);").unwrap();
        let o = Connection::open_in_memory().unwrap();
        store::init(&o).unwrap();
        o.execute(
            "INSERT INTO candles_extra VALUES('AAA','AAA',0,10,12,8,11,70,'external',0)",
            [],
        )
        .unwrap();
        run(&o, &snap).unwrap();
        assert_eq!(
            o.query_row("SELECT source FROM candles_merged", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "external"
        );
        // moomoo later supplies the same bar (quota freed, pulled again)
        b.execute(
            "INSERT INTO candles VALUES('AAA','trades',0,10,11,9,10,100,1000)",
            [],
        )
        .unwrap();
        run(&o, &snap).unwrap();
        let (src, vol, est): (String, f64, i64) = o
            .query_row(
                "SELECT source, volume, turnover_estimated FROM candles_merged",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (src.as_str(), vol, est),
            ("moomoo", 70.0, 0),
            "moomoo prices, the common database's volume"
        );
    }

    #[test]
    fn without_a_moomoo_snapshot_the_base_is_external_only() {
        let d = tempfile::tempdir().unwrap();
        let o = Connection::open_in_memory().unwrap();
        store::init(&o).unwrap();
        o.execute(
            "INSERT INTO candles_extra VALUES('AAA','AAA',0,10,12,8,11,70,'external',0)",
            [],
        )
        .unwrap();
        let sum = run(&o, &d.path().join("absent.sqlite3")).unwrap();
        assert_eq!(
            (sum.moomoo_bars, sum.external_bars, sum.external_only_symbols),
            (0, 1, 1)
        );
    }

    #[test]
    fn carry_fills_interior_and_trailing_slots_with_the_previous_close() {
        let cal: BTreeSet<i64> = (0..6).map(|i| i * BAR_MS).collect();
        // real bars at slots 0, 1 and 4: slots 2 and 3 are interior gaps, 5 is trailing (calendar end) and the symbol still trades
        let v = vec![(0, 10.0), (BAR_MS, 11.0), (4 * BAR_MS, 12.0)];
        assert_eq!(
            carry_fill(&v, &cal),
            vec![(2 * BAR_MS, 11.0), (3 * BAR_MS, 11.0), (5 * BAR_MS, 12.0)]
        );
        // nothing before the first bar, and a symbol that stopped long before the calendar end is not extended
        let late = vec![(2 * BAR_MS, 5.0), (3 * BAR_MS, 6.0)];
        assert_eq!(
            carry_fill(&late, &cal),
            vec![(4 * BAR_MS, 6.0), (5 * BAR_MS, 6.0)]
        );
        let far: BTreeSet<i64> = (0..100).map(|i| i * BAR_MS).collect();
        assert!(carry_fill(&[(0, 1.0), (BAR_MS, 1.0)], &far).is_empty());
        assert!(carry_fill(&[], &cal).is_empty());
    }

    #[test]
    fn calendar_counts_only_symbols_already_listed() {
        let mut m: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        m.insert("OLD1".into(), vec![0, 1, 2, 3]);
        m.insert("OLD2".into(), vec![0, 1, 2, 3]);
        m.insert("NEW".into(), vec![2, 3]);
        m.insert("NEW2".into(), vec![2, 3]);
        m.insert("NEW3".into(), vec![2, 3]);
        assert_eq!(
            active_calendar(&m).into_iter().collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
            "slots 0-1 count only the 2 symbols listed then"
        );
    }

    #[test]
    fn merge_marks_edges_using_symbols_listed_at_that_time() {
        let d = tempfile::tempdir().unwrap();
        let o = Connection::open_in_memory().unwrap();
        store::init(&o).unwrap();
        for (s, times) in [
            ("AAA", vec![0_i64, BAR_MS]),
            ("BBB", vec![BAR_MS]),
            ("CCC", vec![BAR_MS]),
        ] {
            for w in times {
                o.execute(
                    "INSERT INTO candles_extra VALUES(?,?,?,10,11,9,10,100,'external',0)",
                    (s, s, w),
                )
                .unwrap();
            }
        }
        run(&o, &d.path().join("missing.sqlite3")).unwrap();
        let reliable: i64 = o
            .query_row(
                "SELECT volume_reliable FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms=0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            reliable, 0,
            "the first bar is an edge for the only listed symbol"
        );
    }

    #[test]
    fn a_moomoo_bar_keeps_its_prices_and_takes_the_common_databases_volume_where_it_has_the_bar() {
        let d = tempfile::tempdir().unwrap();
        let snap = d.path().join("snap.sqlite3");
        let b = Connection::open(&snap).unwrap();
        b.execute_batch("CREATE TABLE candles(symbol TEXT, kind TEXT, wall_ms INTEGER, open REAL, high REAL, low REAL, close REAL, volume REAL, turnover REAL);").unwrap();
        let o = Connection::open_in_memory().unwrap();
        store::init(&o).unwrap();
        for k in 0..2_i64 {
            b.execute(
                "INSERT INTO candles VALUES('AAA', 'trades', ?, 10, 11, 9, 10, 120, 1200)",
                [k * BAR_MS],
            )
            .unwrap();
        }
        b.execute(
            "INSERT INTO candles VALUES('ZZZ', 'trades', 0, 10, 11, 9, 10, 50, 500)",
            [],
        )
        .unwrap();
        // the common database (imported as candles_extra) holds AAA's first bar with the averaged volume 100, nothing for the second bar or for ZZZ
        o.execute(
            "INSERT INTO candles_extra VALUES('AAA','AAA',0,10,11,9,10,100,'external',0)",
            [],
        )
        .unwrap();
        run(&o, &snap).unwrap();
        let q = |sql: &str| o.query_row(sql, [], |r| r.get::<_, f64>(0)).unwrap();
        assert_eq!(
            q("SELECT volume FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms=0"),
            100.0,
            "the common database's volume"
        );
        assert_eq!(
            q("SELECT turnover FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms=0"),
            1000.0,
            "turnover follows the volume"
        );
        assert_eq!(
            q(&format!(
                "SELECT volume FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms={BAR_MS}"
            )),
            120.0,
            "no bar in the common database: the snapshot's own"
        );
        assert_eq!(
            q("SELECT volume FROM candles_merged WHERE moomoo_symbol='ZZZ'"),
            50.0
        );
        assert_eq!(
            q("SELECT count(*) FROM candles_merged WHERE source='moomoo'"),
            3.0,
            "all three stay moomoo rows"
        );
    }

    #[test]
    fn edges_are_first_and_last_bar_of_each_run() {
        let ts: BTreeSet<i64> = [0, 60_000, 1_800_000, 10_000_000, 1_060_000]
            .into_iter()
            .collect();
        let e = session_edges(&ts);
        let mut v: Vec<i64> = e.into_iter().collect();
        v.sort_unstable();
        assert_eq!(v, vec![0, 60_000, 1_060_000, 1_800_000, 10_000_000]);
    }

    #[test]
    fn turnover_estimate_is_volume_times_typical_price() {
        assert!((estimate_turnover(100.0, 11.0, 9.0, 10.0) - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn moomoo_wins_external_fills_gaps_and_replaces_zero_volume() {
        let d = tempfile::tempdir().unwrap();
        let snap = d.path().join("snap.sqlite3");
        let b = Connection::open(&snap).unwrap();
        b.execute_batch("CREATE TABLE candles(symbol TEXT, kind TEXT, wall_ms INTEGER, open REAL, high REAL, low REAL, close REAL, volume REAL, turnover REAL);").unwrap();
        for (w, v) in [(0_i64, 100.0), (60_000, 0.0)] {
            b.execute(
                "INSERT INTO candles VALUES('AAA','trades',?,10,11,9,10,?,?)",
                (w, v, v * 10.0),
            )
            .unwrap();
        }
        let o = Connection::open_in_memory().unwrap();
        store::init(&o).unwrap();
        // legacy provider has: ts 0 (moomoo real -> must not win), ts 60000 (moomoo zero volume -> replaced), ts 1800000 (missing -> filled),
        // a zero-volume bar (ignored) and a symbol moomoo lacks.
        for (s, w, v) in [
            ("AAA", 0_i64, 999.0),
            ("AAA", 60_000, 50.0),
            ("AAA", 1_800_000, 60.0),
            ("AAA", 2_700_000, 0.0),
            ("BBB", 0, 70.0),
        ] {
            o.execute(
                "INSERT INTO candles_extra VALUES(?,?,?,10,12,8,11,?,'external',0)",
                (s, s, w, v),
            )
            .unwrap();
        }
        let sum = run(&o, &snap).unwrap();
        assert_eq!(
            sum,
            Summary {
                moomoo_bars: 2,
                external_bars: 3,
                external_replaced_zero_volume: 1,
                placeholder_bars_removed: 0,
                symbols: 2,
                external_only_symbols: 1,
                carry_bars: 0
            }
        );
        let q = |sql: &str| o.query_row(sql, [], |r| r.get::<_, f64>(0)).unwrap();
        assert_eq!(
            q("SELECT volume FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms=0"),
            999.0,
            "moomoo's bar keeps its prices; the volume is the common database's for that bar"
        );
        assert_eq!(
            q("SELECT volume FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms=60000"),
            50.0
        );
        assert_eq!(
            q("SELECT turnover FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms=1800000"),
            60.0 * 31.0 / 3.0
        );
        assert_eq!(
            q("SELECT count(*) FROM candles_merged WHERE source='external' AND turnover_estimated=1"),
            3.0
        );
        assert_eq!(
            q("SELECT count(*) FROM candles_merged WHERE moomoo_symbol='AAA' AND wall_ms=2700000"),
            0.0,
            "zero-volume legacy provider bar adds nothing"
        );
    }
}

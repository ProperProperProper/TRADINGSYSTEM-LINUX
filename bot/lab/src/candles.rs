//! The lab's own copy of the market candles (everything on disk, owner rule): 15-minute bars of every symbol the bot has pulled, merged from the bot's
//! research cache (`moomoo-bot/target/oos-cache/history.json`, the longest history moomoo gives, from 2026-01-02; it lives in a build folder that
//! `cargo clean` would erase) and the bot's live store (`moomoo.sqlite3`, newer bars). Imported into `<lab>/data/candles.sqlite3`, merged by (symbol, time).
//! The machine-learning tests (`mlcandle.rs`) read only from here.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar { pub ts: i64, pub open: f64, pub high: f64, pub low: f64, pub close: f64, pub volume: f64 }

#[derive(Deserialize)]
struct Ohlcv { open: f64, high: f64, low: f64, close: f64, volume: f64 }

pub fn init(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS candles15 (symbol TEXT NOT NULL, ts_ms INTEGER NOT NULL, open REAL NOT NULL, high REAL NOT NULL, low REAL NOT NULL,
        close REAL NOT NULL, volume REAL NOT NULL, PRIMARY KEY (symbol, ts_ms)) WITHOUT ROWID;")?;
    Ok(())
}

/// A bar is usable when its prices are positive and ordered; anything else is dropped (and counted by the importer).
pub fn valid(b: &Bar) -> bool { b.open > 0.0 && b.close > 0.0 && b.low > 0.0 && b.low <= b.high && b.volume >= 0.0 && b.low <= b.close.min(b.open) + 1e-9 && b.high + 1e-9 >= b.close.max(b.open) }

/// Merges the research cache and the bot's store into `out`. Returns (bars read, bars written, bars dropped as invalid).
pub fn import(history_json: Option<&Path>, bot_db: Option<&Path>, out: &Connection) -> Result<(usize, usize, usize)> {
    init(out)?;
    let (mut read, mut dropped) = (0usize, 0usize);
    let mut merged: BTreeMap<(String, i64), Bar> = BTreeMap::new();
    if let Some(p) = history_json.filter(|p| p.exists()) {
        let all: Vec<(String, Vec<(i64, Ohlcv)>)> = serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(p)?))?;
        for (sym, bars) in all { for (ts, c) in bars { read += 1; let b = Bar { ts, open: c.open, high: c.high, low: c.low, close: c.close, volume: c.volume }; if valid(&b) { merged.insert((sym.clone(), ts), b); } else { dropped += 1; } } }
    }
    if let Some(p) = bot_db.filter(|p| p.exists()) {
        let c = Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut st = c.prepare("SELECT symbol, wall_ms, open, high, low, close, volume FROM candles WHERE kind = 'trades'")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, Bar { ts: r.get(1)?, open: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0), high: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0), low: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0), close: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0), volume: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0) })))?;
        for row in rows { let (sym, b) = row?; read += 1; if valid(&b) { merged.insert((sym, b.ts), b); } else { dropped += 1; } }
    }
    let tx = out.unchecked_transaction()?;
    let mut written = 0usize;
    {
        let mut ins = tx.prepare("INSERT OR REPLACE INTO candles15(symbol, ts_ms, open, high, low, close, volume) VALUES (?1,?2,?3,?4,?5,?6,?7)")?;
        for ((sym, ts), b) in &merged { ins.execute(params![sym, ts, b.open, b.high, b.low, b.close, b.volume])?; written += 1; }
    }
    tx.commit()?;
    if let Some(p) = bot_db.filter(|p| p.exists()) { let _ = import_costs(p, out); }
    Ok((read, written, dropped))
}

/// Every symbol's bars, oldest first.
pub fn load(db: &Connection) -> Result<BTreeMap<String, Vec<Bar>>> {
    init(db)?;
    let mut st = db.prepare("SELECT symbol, ts_ms, open, high, low, close, volume FROM candles15 ORDER BY symbol, ts_ms")?;
    let mut out: BTreeMap<String, Vec<Bar>> = BTreeMap::new();
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, Bar { ts: r.get(1)?, open: r.get(2)?, high: r.get(3)?, low: r.get(4)?, close: r.get(5)?, volume: r.get(6)? })))? {
        let (s, b) = row?;
        out.entry(s).or_default().push(b);
    }
    Ok(out)
}

/// The bot's universe: SPY's holdings (its official file, by weight) -- the same list the bot's loader reads. Stored in the lab's candle store.
pub fn import_universe(holdings_csv: &Path, out: &Connection) -> Result<usize> {
    out.execute_batch("CREATE TABLE IF NOT EXISTS universe (symbol TEXT PRIMARY KEY, rank INTEGER NOT NULL);")?;
    let text = std::fs::read_to_string(holdings_csv)?;
    let mut rows: Vec<(f64, String)> = text.lines().filter(|l| !l.starts_with('#') && !l.starts_with("rank,")).filter_map(|l| {
        let c: Vec<&str> = l.split(',').collect();
        let (t, w) = (c.get(1)?.trim(), c.last()?.trim().parse::<f64>().ok()?);
        (!t.is_empty() && w.is_finite()).then(|| (w, t.to_string()))
    }).collect();
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    let tx = out.unchecked_transaction()?;
    tx.execute("DELETE FROM universe", [])?;
    for (i, (_, s)) in rows.iter().enumerate() { tx.execute("INSERT OR REPLACE INTO universe(symbol, rank) VALUES (?1, ?2)", params![s, i as i64 + 1])?; }
    tx.commit()?;
    Ok(rows.len())
}

pub fn load_universe(db: &Connection) -> Vec<String> {
    let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS universe (symbol TEXT PRIMARY KEY, rank INTEGER NOT NULL);");
    let Ok(mut st) = db.prepare("SELECT symbol FROM universe ORDER BY rank") else { return vec![] };
    st.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect()).unwrap_or_default()
}

/// The bot's data hygiene (examples/common/mod.rs there): only the universe plus the SPY reference; candles are unadjusted, so a jump between consecutive bars
/// (open / previous close outside 0.6..1.8) is a split: only the history AFTER a symbol's last jump is kept; symbols with fewer than 520 bars left are dropped.
/// An empty `universe` keeps every symbol. Returns the cleaned series and one line per change.
pub fn prepare(series: &BTreeMap<String, Vec<Bar>>, universe: &[String]) -> (BTreeMap<String, Vec<Bar>>, Vec<String>) {
    let keep: std::collections::BTreeSet<&String> = universe.iter().collect();
    let (mut out, mut notes) = (BTreeMap::new(), vec![]);
    for (s, bars) in series {
        if !universe.is_empty() && s != "SPY" && !keep.contains(s) { notes.push(format!("{s}: not an SPY holding, left out")); continue; }
        let last_jump = bars.windows(2).enumerate().filter(|(_, w)| !(0.6..=1.8).contains(&(w[1].open / w[0].close))).map(|(i, _)| i + 1).next_back();
        let kept: Vec<Bar> = match last_jump { Some(i) => { notes.push(format!("{s}: split jump, {} of {} bars kept", bars.len() - i, bars.len())); bars[i..].to_vec() } None => bars.clone() };
        if kept.len() > 26 * 20 { out.insert(s.clone(), kept); } else { notes.push(format!("{s}: too little history left, dropped")); }
    }
    (out, notes)
}

/// The cleaned series the machine-learning code uses.
pub fn load_prepared(db: &Connection) -> Result<BTreeMap<String, Vec<Bar>>> { Ok(prepare(&load(db)?, &load_universe(db)).0) }

/// Trading costs measured by the bot: the commission per order (USD, from its real paper orders) and each symbol's half-spread (fraction of price, the median
/// over the bot's recorded order books at a ~$2,500 order).
#[derive(Debug, Clone)]
pub struct Costs { pub commission: f64, pub half: BTreeMap<String, f64>, pub default_half: f64 }

impl Costs {
    pub fn half_spread(&self, symbol: &str) -> f64 { self.half.get(symbol).copied().unwrap_or(self.default_half) }
    /// Cost of one buy or one sell of `size` USD in `symbol`, as a fraction of the position.
    pub fn one_way(&self, symbol: &str, size: f64) -> f64 { self.commission / size + self.half_spread(symbol) }
}

/// Reads the bot's order fees and order books into the lab's store. Returns (symbols with a spread, commission USD).
pub fn import_costs(bot_db: &Path, out: &Connection) -> Result<(usize, f64)> {
    out.execute_batch("CREATE TABLE IF NOT EXISTS costs (symbol TEXT PRIMARY KEY, half REAL NOT NULL, rows INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value REAL NOT NULL);")?;
    let c = Connection::open_with_flags(bot_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let commission: f64 = c.query_row("SELECT coalesce(avg(total), 0) FROM order_fees WHERE total > 0", [], |r| r.get(0))?;
    let mut per: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut st = c.prepare("SELECT symbol, json FROM rules")?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (sym, js) = row?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&js) else { continue };
        let Some(book) = v["book"].as_array() else { continue };
        let level = book.iter().find(|b| b[0].as_f64().unwrap_or(0.0) >= 2500.0).or(book.last());
        if let Some(b) = level { if let (Some(buy), Some(sell)) = (b[1].as_f64(), b[2].as_f64()) { if buy.is_finite() && sell.is_finite() && buy >= 0.0 && sell >= 0.0 { per.entry(sym).or_default().push((buy + sell) / 2.0); } } }
    }
    let tx = out.unchecked_transaction()?;
    for (sym, mut v) in per.clone() { v.sort_by(f64::total_cmp); tx.execute("INSERT OR REPLACE INTO costs(symbol, half, rows) VALUES (?1, ?2, ?3)", params![sym, v[v.len() / 2], v.len() as i64])?; }
    if commission > 0.0 { tx.execute("INSERT OR REPLACE INTO meta(key, value) VALUES ('commission_usd', ?1)", params![commission])?; }
    tx.commit()?;
    Ok((per.len(), commission))
}

/// The stored costs; falls back to the bot's measured $1.00 per order and a 2 bp half-spread only if nothing was imported (and says so through the values).
pub fn load_costs(db: &Connection) -> Costs {
    let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS costs (symbol TEXT PRIMARY KEY, half REAL NOT NULL, rows INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value REAL NOT NULL);");
    let commission = db.query_row("SELECT value FROM meta WHERE key = 'commission_usd'", [], |r| r.get::<_, f64>(0)).unwrap_or(1.0);
    let mut half = BTreeMap::new();
    if let Ok(mut st) = db.prepare("SELECT symbol, half FROM costs") { if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))) { for r in rows.flatten() { half.insert(r.0, r.1); } } }
    let mut v: Vec<f64> = half.values().copied().collect(); v.sort_by(f64::total_cmp);
    let default_half = if v.is_empty() { 0.0002 } else { v[v.len() / 2] };
    Costs { commission, half, default_half }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(ts: i64, c: f64) -> Bar { Bar { ts, open: c, high: c + 1.0, low: c - 1.0, close: c, volume: 10.0 } }

    #[test]
    fn invalid_bars_are_recognised() {
        assert!(valid(&bar(1, 100.0)));
        assert!(!valid(&Bar { low: 101.0, ..bar(1, 100.0) }), "low above high");
        assert!(!valid(&Bar { close: 0.0, ..bar(1, 100.0) }));
        assert!(!valid(&Bar { volume: -1.0, ..bar(1, 100.0) }));
        assert!(!valid(&Bar { close: 150.0, ..bar(1, 100.0) }), "close outside the range");
    }

    #[test]
    fn costs_are_read_from_the_bots_fees_and_books() {
        let d = tempfile::tempdir().unwrap();
        let bot = d.path().join("bot.sqlite3");
        { let c = Connection::open(&bot).unwrap(); c.execute_batch("CREATE TABLE order_fees(order_id_ex TEXT, ts_ms INTEGER, symbol TEXT, side INTEGER, qty REAL, price REAL, total REAL, items TEXT);
            INSERT INTO order_fees VALUES ('1',0,'A',1,1,1,0.99,'[]'), ('2',0,'A',2,1,1,1.01,'[]');
            CREATE TABLE rules(symbol TEXT, ts_ms INTEGER, json TEXT);
            INSERT INTO rules VALUES ('AAA',1,'{\"book\":[[1000,0.0001,0.0001],[5000,0.0003,0.0005]]}'), ('AAA',2,'{\"book\":[[5000,0.0007,0.0007]]}'), ('AAA',3,'{\"book\":[[5000,0.0001,0.0001]]}'), ('BBB',1,'not json');").unwrap(); }
        let out = Connection::open_in_memory().unwrap();
        let (n, comm) = import_costs(&bot, &out).unwrap();
        assert_eq!(n, 1);
        assert!((comm - 1.0).abs() < 1e-12);
        let c = load_costs(&out);
        assert!((c.half_spread("AAA") - 0.0004).abs() < 1e-12, "median of 0.0004, 0.0007, 0.0001");
        assert_eq!(c.half_spread("ZZZ"), c.default_half);
        assert!((c.one_way("AAA", 2500.0) - (1.0 / 2500.0 + 0.0004)).abs() < 1e-12);
    }

    #[test]
    fn the_bots_hygiene_keeps_only_holdings_and_history_after_the_last_split() {
        let mk = |n: usize, f: &dyn Fn(usize) -> f64| -> Vec<Bar> { (0..n).map(|i| { let c = f(i); Bar { ts: i as i64 * 900_000, open: c, high: c + 0.1, low: c - 0.1, close: c, volume: 10.0 } }).collect() };
        let mut s = BTreeMap::new();
        s.insert("AAA".to_string(), mk(1000, &|_| 100.0));
        s.insert("SPY".to_string(), mk(1000, &|_| 400.0));
        s.insert("ZZZ".to_string(), mk(1000, &|_| 50.0)); // not a holding
        s.insert("SPL".to_string(), mk(1000, &|i| if i < 300 { 200.0 } else { 100.0 })); // 2:1 split at bar 300
        s.insert("NEW".to_string(), mk(300, &|_| 10.0)); // too short
        let (out, notes) = prepare(&s, &["AAA".into(), "SPL".into(), "NEW".into()]);
        assert_eq!(out.keys().cloned().collect::<Vec<_>>(), vec!["AAA", "SPL", "SPY"]);
        assert_eq!(out["SPL"].len(), 700, "only the bars after the jump");
        assert!(notes.iter().any(|n| n.contains("ZZZ")) && notes.iter().any(|n| n.contains("SPL")) && notes.iter().any(|n| n.contains("NEW")));
        let d = tempfile::tempdir().unwrap();
        let csv = d.path().join("h.csv");
        std::fs::write(&csv, "# comment\nrank,ticker,name,weight_pct\n1,BBB,B Corp,2.5\n2,AAA,A Corp,7.5\n").unwrap();
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(import_universe(&csv, &db).unwrap(), 2);
        assert_eq!(load_universe(&db), vec!["AAA", "BBB"], "ordered by weight");
    }

    #[test]
    fn import_merges_sources_newer_wins_and_roundtrips() {
        let d = tempfile::tempdir().unwrap();
        let hist = d.path().join("h.json");
        std::fs::write(&hist, r#"[["AAA",[[900000,{"open":10.0,"high":11.0,"low":9.0,"close":10.5,"volume":5.0,"turnover":1.0}],[1800000,{"open":10.5,"high":12.0,"low":10.0,"close":11.0,"volume":6.0,"turnover":1.0}],[2700000,{"open":0.0,"high":1.0,"low":0.5,"close":1.0,"volume":1.0,"turnover":1.0}]]]]"#).unwrap();
        let bot = d.path().join("bot.sqlite3");
        { let c = Connection::open(&bot).unwrap(); c.execute_batch("CREATE TABLE candles(symbol TEXT, kind TEXT, wall_ms INTEGER, open REAL, high REAL, low REAL, close REAL, volume REAL, turnover REAL, fetched_ms INTEGER);
            INSERT INTO candles VALUES ('AAA','trades',1800000,10.5,12.0,10.0,11.5,7.0,1.0,0), ('AAA','trades',3600000,11.5,12.5,11.0,12.0,8.0,1.0,0), ('BBB','other',900000,1,1,1,1,1,1,0);").unwrap(); }
        let out = Connection::open_in_memory().unwrap();
        let (read, written, dropped) = import(Some(&hist), Some(&bot), &out).unwrap();
        assert_eq!((read, dropped), (5, 1), "the bot's non-trade kinds are not read; the bar with open 0 is dropped");
        assert_eq!(written, 3);
        let all = load(&out).unwrap();
        let aaa = &all["AAA"];
        assert_eq!(aaa.iter().map(|b| b.ts).collect::<Vec<_>>(), vec![900000, 1800000, 3600000]);
        assert_eq!(aaa[1].close, 11.5, "the bot's store is newer and wins for the same bar");
        assert_eq!(import(Some(&hist), Some(&bot), &out).unwrap().1, 3, "importing again changes nothing");
    }
}

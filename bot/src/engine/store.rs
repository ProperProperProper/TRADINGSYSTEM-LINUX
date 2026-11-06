//! The permanent SQLite store (user rule 2026-10-06: store everything, including candle data;
//! only cull candle data once the database reaches 10 GB). It lives in the archive folder beside
//! the runtime folder, which deploy resets never touch.
//!
//! Tables: `candles` (every fetched 1-minute traded candle), `rules` (each measurement),
//! `accounts` and `broker_positions` (each moomoo paper account read), `universe` (each screen),
//! `orders` (every real paper order state seen), `order_fees` (moomoo's fees per order), `quota`
//! (each candle-quota read), `paper` (the engine paper account per bar), `paper_trades`, `events`
//! (every engine decision, denied entry, gate, exclusion and reconciliation), `market` (per-bar
//! market context), `searches`.

use super::metrics::Trade;
use super::rules::Rules;
use super::xs::EngineEvent;
use super::Bar;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

/// The size at which the oldest candle rows are culled.
pub const LIMIT_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Share of the candle rows removed per cull (oldest first).
const CULL_FRACTION: f64 = 0.05;

pub struct Store {
    conn: Mutex<Connection>,
    limit: u64,
}

/// A closed paper trade for the console chart: (symbol, entry wall ms, exit wall ms, pnl, reason).
pub type TradeRow = (String, i64, i64, f64, String);

/// One bar of the engine paper account.
pub struct PaperPoint {
    pub run_id: i64,
    pub wall_ms: i64,
    pub equity: f64,
    pub start_equity: f64,
    pub realized: f64,
    pub open_pnl: f64,
    pub max_drawdown_pct: f64,
    pub open_positions: usize,
    pub closed_trades: usize,
    pub wins: usize,
    pub params_json: String,
    pub mode: String,
    pub positions_json: String,
}

impl Store {
    /// Opens (or creates) the store with the default size limit.
    pub fn open(path: &Path) -> Result<Store> {
        Self::open_with_limit(path, LIMIT_BYTES)
    }

    /// Opens (or creates) the store with size limit `limit` bytes; creates every table.
    pub fn open_with_limit(path: &Path, limit: u64) -> Result<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        // Incremental auto-vacuum so a cull actually returns space (must be set before tables).
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS candles(symbol TEXT NOT NULL, kind TEXT NOT NULL, wall_ms INTEGER NOT NULL,
                 open REAL, high REAL, low REAL, close REAL, volume REAL, turnover REAL, fetched_ms INTEGER,
                 PRIMARY KEY(symbol, kind, wall_ms)) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS candles_wall ON candles(wall_ms);
             CREATE TABLE IF NOT EXISTS rules(symbol TEXT NOT NULL, ts_ms INTEGER NOT NULL, json TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS accounts(ts_ms INTEGER NOT NULL, account TEXT, total_assets REAL, cash REAL,
                 power REAL, market_val REAL);
             CREATE TABLE IF NOT EXISTS broker_positions(ts_ms INTEGER NOT NULL, symbol TEXT, qty REAL, cost_price REAL, price REAL);
             CREATE TABLE IF NOT EXISTS universe(ts_ms INTEGER NOT NULL, rank INTEGER, symbol TEXT, name TEXT, sec_type INTEGER,
                 exch_type INTEGER, avg_turnover REAL);
             CREATE TABLE IF NOT EXISTS orders(ts_ms INTEGER NOT NULL, order_id INTEGER, json TEXT);
             CREATE TABLE IF NOT EXISTS order_fees(order_id_ex TEXT PRIMARY KEY, ts_ms INTEGER, symbol TEXT, side INTEGER,
                 qty REAL, price REAL, total REAL, items TEXT);
             CREATE TABLE IF NOT EXISTS quota(ts_ms INTEGER NOT NULL, used INTEGER, remain INTEGER, detail TEXT);
             CREATE TABLE IF NOT EXISTS runs(run_id INTEGER PRIMARY KEY, started_ms INTEGER, note TEXT);
             CREATE TABLE IF NOT EXISTS paper(run_id INTEGER NOT NULL, wall_ms INTEGER NOT NULL, equity REAL, start_equity REAL,
                 realized REAL, open_pnl REAL, max_drawdown_pct REAL, open_positions INTEGER, closed_trades INTEGER,
                 wins INTEGER, params TEXT, mode TEXT, positions TEXT, PRIMARY KEY(run_id, wall_ms));
             CREATE TABLE IF NOT EXISTS paper_trades(run_id INTEGER NOT NULL, symbol TEXT, side TEXT, entry_ms INTEGER,
                 exit_ms INTEGER, entry REAL, exit REAL, qty REAL, pnl REAL, fees REAL, reason TEXT, json TEXT,
                 PRIMARY KEY(run_id, symbol, entry_ms, exit_ms));
             CREATE TABLE IF NOT EXISTS events(run_id INTEGER, wall_ms INTEGER, kind TEXT, symbol TEXT, detail TEXT);
             CREATE INDEX IF NOT EXISTS events_wall ON events(run_id, wall_ms);
             CREATE TABLE IF NOT EXISTS market(run_id INTEGER, wall_ms INTEGER, json TEXT, PRIMARY KEY(run_id, wall_ms));
             CREATE TABLE IF NOT EXISTS searches(run_id INTEGER, finished_ms INTEGER, json TEXT);",
        )?;
        Ok(Store {
            conn: Mutex::new(conn),
            limit,
        })
    }

    /// The store's connection (locked).
    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Used bytes (allocated pages minus free pages).
    pub fn size_bytes(&self) -> Result<u64> {
        let c = self.conn();
        let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let free: i64 = c.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
        Ok(((pages - free).max(0) * size) as u64)
    }

    // NOTE(agents): The ONLY deletion in the store (user rule): at the 10 GB limit the oldest
    //               candle rows go, a slice at a time, until the store is below the limit again.
    //               Nothing else is ever deleted.
    /// Cull the oldest candles while the store is at or above its limit; rows removed.
    pub fn enforce_limit(&self) -> Result<usize> {
        let mut removed = 0;
        while self.size_bytes()? >= self.limit {
            let c = self.conn();
            let total: i64 = c.query_row("SELECT COUNT(*) FROM candles", [], |r| r.get(0))?;
            if total == 0 {
                break;
            }
            let n = ((total as f64 * CULL_FRACTION).ceil() as i64).max(1);
            let cutoff: i64 = c.query_row(
                "SELECT wall_ms FROM candles ORDER BY wall_ms LIMIT 1 OFFSET ?1",
                [n - 1],
                |r| r.get(0),
            )?;
            removed += c.execute("DELETE FROM candles WHERE wall_ms <= ?1", [cutoff])?;
            c.execute_batch("PRAGMA incremental_vacuum;")?;
        }
        Ok(removed)
    }

    /// Saves candles of `symbol` (replacing any with the same time).
    pub fn candles(
        &self,
        symbol: &str,
        kind: &str,
        bars: &std::collections::BTreeMap<i64, Bar>,
        now: i64,
    ) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT OR REPLACE INTO candles VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            )?;
            for (w, b) in bars {
                st.execute(params![
                    symbol, kind, w, b.open, b.high, b.low, b.close, b.volume, b.turnover, now
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Loads the real archived candles for startup hydration. This never contacts moomoo and
    /// returns only the requested candle kind, grouped by symbol.
    pub fn load_candles(
        &self,
        kind: &str,
    ) -> Result<std::collections::HashMap<String, std::collections::BTreeMap<i64, Bar>>> {
        let c = self.conn();
        let mut q = c.prepare("SELECT symbol, wall_ms, open, high, low, close, volume, turnover FROM candles WHERE kind = ?1 ORDER BY symbol, wall_ms")?;
        let rows = q.query_map([kind], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                Bar {
                    open: r.get(2)?,
                    high: r.get(3)?,
                    low: r.get(4)?,
                    close: r.get(5)?,
                    volume: r.get(6)?,
                    turnover: r.get(7)?,
                },
            ))
        })?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (sym, wall, bar) = row?;
            out.entry(sym)
                .or_insert_with(std::collections::BTreeMap::new)
                .insert(wall, bar);
        }
        Ok(out)
    }

    /// Saves one rules measurement of `symbol`.
    pub fn rules(&self, symbol: &str, r: &Rules, now: i64) -> Result<()> {
        self.conn().execute(
            "INSERT INTO rules VALUES(?1,?2,?3)",
            params![symbol, now, serde_json::to_string(r)?],
        )?;
        Ok(())
    }

    /// The latest stored rules measurement of every symbol (real past measurements, each with its
    /// own `book_ts`), for a restart to continue from.
    pub fn latest_rules(&self) -> Result<std::collections::HashMap<String, Rules>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT symbol, json FROM rules r WHERE ts_ms = (SELECT MAX(ts_ms) FROM rules WHERE symbol = r.symbol)",
        )?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = std::collections::HashMap::new();
        for row in rows {
            let (sym, json) = row?;
            out.insert(sym, serde_json::from_str(&json)?);
        }
        Ok(out)
    }

    /// Saves one account read and the positions held at that moment.
    pub fn account(&self, a: &super::moomoo::Account, held: &[super::moomoo::Held]) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO accounts VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                a.ts,
                a.id.to_string(),
                a.total_assets,
                a.cash,
                a.power,
                a.market_val
            ],
        )?;
        for h in held {
            tx.execute(
                "INSERT INTO broker_positions VALUES(?1,?2,?3,?4,?5)",
                params![a.ts, h.symbol, h.qty, h.cost_price, h.price],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Saves the universe at `ts` with each listing's rank.
    pub fn universe(&self, ts: i64, list: &[super::moomoo::Listing]) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        for (i, l) in list.iter().enumerate() {
            tx.execute(
                "INSERT INTO universe VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    ts,
                    i as i64 + 1,
                    l.symbol,
                    l.name,
                    l.sec_type,
                    l.exch_type,
                    l.avg_turnover
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// One observed state of a real paper order (any JSON the service keeps for it).
    pub fn order(&self, ts: i64, order_id: u64, json: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO orders VALUES(?1,?2,?3)",
            params![ts, order_id as i64, json],
        )?;
        Ok(())
    }

    /// moomoo's fee for one filled order (kept once per order).
    #[allow(clippy::too_many_arguments)]
    pub fn order_fee(
        &self,
        ts: i64,
        f: &super::moomoo::OrderFee,
        symbol: &str,
        side: i32,
        qty: f64,
        price: f64,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO order_fees VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                f.order_id_ex,
                ts,
                symbol,
                side,
                qty,
                price,
                f.total,
                serde_json::to_string(&f.items)?
            ],
        )?;
        Ok(())
    }

    /// Every real fee observation: (symbol, notional USD, total fee USD), oldest first.
    pub fn fee_points(&self) -> Result<Vec<(String, f64, f64)>> {
        let c = self.conn();
        let mut st =
            c.prepare("SELECT symbol, qty * price, total FROM order_fees ORDER BY ts_ms")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Saves one candle-quota read.
    pub fn quota(&self, q: &super::moomoo::QuotaState) -> Result<()> {
        self.conn().execute(
            "INSERT INTO quota VALUES(?1,?2,?3,?4)",
            params![q.ts, q.used, q.remain, serde_json::to_string(&q.pulled)?],
        )?;
        Ok(())
    }

    /// Records the start of a service run.
    pub fn start_run(&self, run_id: i64, note: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO runs VALUES(?1,?1,?2)",
            params![run_id, note],
        )?;
        Ok(())
    }

    /// One bar of the paper account plus its closed trades (`wall` maps engine clock times).
    /// `features` gives the research lab's entry-state blob for a trade; it is called only for trades
    /// not stored yet and kept in the trade's json (with `decided_wall_ms`, the deciding bar in wall time).
    pub fn paper(
        &self,
        p: &PaperPoint,
        trades: &[Trade],
        wall: impl Fn(i64) -> Option<i64>,
        features: impl Fn(&Trade) -> serde_json::Value,
    ) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO paper VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                p.run_id,
                p.wall_ms,
                p.equity,
                p.start_equity,
                p.realized,
                p.open_pnl,
                p.max_drawdown_pct,
                p.open_positions as i64,
                p.closed_trades as i64,
                p.wins as i64,
                p.params_json,
                p.mode,
                p.positions_json
            ],
        )?;
        for t in trades {
            let (entry_ms, exit_ms) = (wall(t.entry_ts), wall(t.exit_ts));
            let known: bool = tx
                .query_row(
                    "SELECT 1 FROM paper_trades WHERE run_id=?1 AND symbol=?2 AND entry_ms IS ?3 AND exit_ms IS ?4",
                    params![p.run_id, t.symbol, entry_ms, exit_ms],
                    |_| Ok(true),
                )
                .optional()?
                .is_some();
            if known {
                continue;
            }
            let mut json = serde_json::to_value(t)?;
            json["features"] = features(t);
            json["decided_wall_ms"] = t.decided_ts.and_then(&wall).into();
            tx.execute(
                "INSERT OR IGNORE INTO paper_trades VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    p.run_id,
                    t.symbol,
                    format!("{:?}", t.side),
                    entry_ms,
                    exit_ms,
                    t.entry,
                    t.exit,
                    t.qty,
                    t.pnl,
                    t.fees,
                    t.reason,
                    json.to_string()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Average daily dollar turnover and daily volatility of `symbol` from its stored real candles (see `moomoo::liquidity_from_bars`).
    pub fn liquidity(&self, symbol: &str) -> Option<(f64, f64)> {
        let c = self.conn();
        let mut st = c.prepare("SELECT wall_ms, open, high, low, close, volume, turnover FROM candles WHERE symbol = ?1 AND kind = 'trades' ORDER BY wall_ms DESC LIMIT 900").ok()?;
        let rows = st
            .query_map([symbol], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    Bar {
                        open: r.get(1)?,
                        high: r.get(2)?,
                        low: r.get(3)?,
                        close: r.get(4)?,
                        volume: r.get(5)?,
                        turnover: r.get(6)?,
                    },
                ))
            })
            .ok()?
            .filter_map(|x| x.ok())
            .collect::<std::collections::BTreeMap<_, _>>();
        super::moomoo::liquidity_from_bars(&rows, 20)
    }

    /// Saves engine events of run `run_id` (clock times mapped to wall times).
    pub fn events(
        &self,
        run_id: i64,
        events: &[EngineEvent],
        wall: impl Fn(i64) -> Option<i64>,
    ) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        for e in events {
            tx.execute(
                "INSERT INTO events VALUES(?1,?2,?3,?4,?5)",
                params![
                    run_id,
                    wall(e.ts_ms),
                    e.kind,
                    e.symbol,
                    e.detail.to_string()
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Saves the market context of one bar.
    pub fn market(&self, run_id: i64, wall_ms: i64, json: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO market VALUES(?1,?2,?3)",
            params![run_id, wall_ms, json],
        )?;
        Ok(())
    }

    /// Saves one finished search.
    pub fn search(&self, run_id: i64, finished_ms: i64, json: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO searches VALUES(?1,?2,?3)",
            params![run_id, finished_ms, json],
        )?;
        Ok(())
    }

    /// The paper account's equity over ALL runs (every service start is a run): the paper state carries over restarts, so its
    /// chart must too (it showed only the current run, which a restart resets).
    pub fn equity_curve_all(&self) -> Result<Vec<(i64, f64)>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT wall_ms, equity FROM paper ORDER BY wall_ms")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Closed paper trades of ALL runs, oldest exit first.
    pub fn trades_all(&self) -> Result<Vec<TradeRow>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT symbol, entry_ms, exit_ms, pnl, reason FROM paper_trades ORDER BY exit_ms",
        )?;
        let rows = st.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Number of stored candles.
    pub fn candle_count(&self) -> Result<i64> {
        Ok(self
            .conn()
            .query_row("SELECT COUNT(*) FROM candles", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Test fixture: a flat candle at price `p`.
    fn bar(p: f64) -> Bar {
        Bar {
            open: p,
            high: p,
            low: p,
            close: p,
            volume: 1.0,
            turnover: p,
        }
    }

    /// Test: the paper chart's history spans every run (a restart starts a new run; the chart must not reset).
    #[test]
    fn paper_history_spans_every_run() {
        let dir = std::env::temp_dir().join(format!("moomoo-store-runs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Store::open_with_limit(&dir.join("s.sqlite3"), u64::MAX).unwrap();
        for (run, wall, eq) in [(1, 100, 5000.0), (2, 200, 5010.0), (3, 300, 4990.0)] {
            s.conn().execute("INSERT INTO paper(run_id, wall_ms, equity, start_equity) VALUES (?1, ?2, ?3, 5000)", rusqlite::params![run, wall, eq]).unwrap();
        }
        assert_eq!(
            s.equity_curve_all().unwrap(),
            vec![(100, 5000.0), (200, 5010.0), (300, 4990.0)],
            "all runs, in time order"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Test: stores candles and culls only the oldest at the limit.
    #[test]
    fn stores_candles_and_culls_only_the_oldest_at_the_limit() {
        let dir = std::env::temp_dir().join(format!("moomoo-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("s.sqlite3");
        let s = Store::open_with_limit(&path, u64::MAX).unwrap();
        let bars: BTreeMap<i64, Bar> = (0..2000)
            .map(|i| (i * 60_000, bar(100.0 + i as f64)))
            .collect();
        s.candles("TSLA", "trades", &bars, 1).unwrap();
        s.search(1, 2, "{}").unwrap();
        assert_eq!(s.candle_count().unwrap(), 2000);
        assert_eq!(
            s.enforce_limit().unwrap(),
            0,
            "below the limit nothing is deleted"
        );
        let size = s.size_bytes().unwrap();
        drop(s);
        let s = Store::open_with_limit(&path, size).unwrap();
        let removed = s.enforce_limit().unwrap();
        assert!(removed > 0 && s.size_bytes().unwrap() < size);
        let c = s.conn();
        let oldest: i64 = c
            .query_row("SELECT MIN(wall_ms) FROM candles", [], |r| r.get(0))
            .unwrap();
        assert!(oldest > 0, "the oldest candles went first");
        let searches: i64 = c
            .query_row("SELECT COUNT(*) FROM searches", [], |r| r.get(0))
            .unwrap();
        assert_eq!(searches, 1, "nothing but candles is culled");
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

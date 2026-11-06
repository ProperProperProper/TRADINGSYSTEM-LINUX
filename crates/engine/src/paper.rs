//! The local paper trader. It is the bot's own engine (`xs::XsPortfolio`, the same code the backtests run, with the measured per-symbol rules: lot sizes, minimum
//! order value, commission table, order-book spread and impact cost, the liquidity cap) stepped one newly CLOSED candle at a time on real market data, with its whole
//! state kept in the database. No order ever leaves the machine. Because the backtest is nothing but `step` over a range of candles, stepping incrementally gives
//! the same trades as a backtest of the same period (tests/paper.rs proves it trade for trade, through a save and load after every candle).
//!
//! The account starts flat with 5,000 USD at the first run and never looks at candles before it. The setting it trades is re-searched on the last 14 sessions of
//! its universe once per session (the bot's own live procedure, which the walk-forward validated) and applied from the next candle on.
use anyhow::{Context, Result};
use moomoo_bot::engine::scores;
use moomoo_bot::engine::walkforward::{self, SearchSpace};
use moomoo_bot::engine::xs::{XsParams, XsPortfolio};
use moomoo_bot::engine::{Market, START_CAPITAL};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Sessions the setting is searched on (the bot's live search window).
pub const SEARCH_SESSIONS: usize = 14;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperAccount {
    pub portfolio: XsPortfolio,
    /// Real (wall) open time of the last candle stepped; candles up to it are done.
    pub last_wall: i64,
    pub start_ms: i64,
    pub start_equity: f64,
    /// The setting in force and when (wall time of the candle it was searched at) it was chosen.
    pub params: Option<XsParams>,
    pub params_wall: i64,
}

impl PaperAccount {
    /// A fresh flat account that will trade candles after `last_wall`.
    pub fn new(now_ms: i64, last_wall: i64, equity: f64) -> Self {
        PaperAccount {
            portfolio: XsPortfolio::new(equity),
            last_wall,
            start_ms: now_ms,
            start_equity: equity,
            params: None,
            params_wall: 0,
        }
    }
}

pub fn load(conn: &Connection) -> Result<Option<PaperAccount>> {
    let json: Option<String> = conn
        .query_row("SELECT json FROM paper_state WHERE id=1", [], |r| r.get(0))
        .optional()?;
    json.map(|j| serde_json::from_str(&j).context("paper_state is not a valid account"))
        .transpose()
}

pub fn save(conn: &Connection, a: &PaperAccount, now_ms: i64) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO paper_state(id, json, updated_ms) VALUES(1, ?, ?)",
        rusqlite::params![serde_json::to_string(a)?, now_ms],
    )?;
    Ok(())
}

/// Indices of the candles that are closed (the reference symbol has a complete candle there) and newer than the last one stepped.
pub fn new_candles(m: &Market, last_wall: i64) -> Vec<usize> {
    let spy = m
        .symbols
        .iter()
        .position(|s| s == moomoo_bot::engine::REFERENCE);
    (0..m.ts.len())
        .filter(|&t| m.wall[t] > last_wall && spy.is_some_and(|s| m.bars[s][t].is_some()))
        .collect()
}

/// The wall time of the newest closed candle, if any.
pub fn latest_closed(m: &Market) -> Option<i64> {
    let spy = m
        .symbols
        .iter()
        .position(|s| s == moomoo_bot::engine::REFERENCE)?;
    (0..m.ts.len())
        .rev()
        .find(|&t| m.bars[spy][t].is_some())
        .map(|t| m.wall[t])
}

/// Re-searches the setting (in `space`) on the last `SEARCH_SESSIONS` sessions that end at candle `t_end` (inclusive) and returns the bot's robust winner, if any setting is usable.
pub fn search_params(
    m: &Market,
    sc: &[Vec<Option<scores::Score>>],
    t_end: usize,
    per_session: usize,
    space: SearchSpace,
    seed: u64,
) -> Result<Option<XsParams>> {
    anyhow::ensure!(per_session > 0, "per_session must be positive");
    anyhow::ensure!(t_end < m.ts.len(), "search candle is outside the market");
    let start = (t_end + 1).saturating_sub(SEARCH_SESSIONS * per_session);
    let range = start..t_end + 1;
    let slice = m.slice(range.clone());
    let rows: Vec<Vec<Option<scores::Score>>> =
        sc.iter().map(|r| r[range.clone()].to_vec()).collect();
    walkforward::KEEP_ALL_USABLE.store(false, std::sync::atomic::Ordering::Relaxed);
    let rep = walkforward::search_random(
        &slice,
        &rows,
        START_CAPITAL,
        Instant::now() + Duration::from_secs(1800),
        seed,
        &[],
        space,
    )?;
    Ok(rep.params)
}

/// Steps the account through `candles` (ascending indices of closed candles) with its current setting; returns how many candles were stepped. With no setting the
/// account stays flat (the candles are still consumed, so it never trades the past).
pub fn step(
    acc: &mut PaperAccount,
    m: &Market,
    sc: &[Vec<Option<scores::Score>>],
    candles: &[usize],
) -> usize {
    acc.portfolio.reindex(&m.symbols);
    for &t in candles {
        if let Some(p) = &acc.params {
            acc.portfolio.step(m, sc, t, p);
        }
        acc.last_wall = m.wall[t];
    }
    candles.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_account_is_flat_and_survives_a_round_trip_through_the_database() {
        let c = Connection::open_in_memory().unwrap();
        thomas_core::store::init(&c).unwrap();
        assert!(load(&c).unwrap().is_none());
        let a = PaperAccount::new(1_000, 500, 5_000.0);
        save(&c, &a, 2_000).unwrap();
        let b = load(&c).unwrap().unwrap();
        assert_eq!(
            (b.last_wall, b.start_ms, b.start_equity),
            (500, 1_000, 5_000.0)
        );
        assert_eq!(b.portfolio.equity, 5_000.0);
        assert!(
            b.portfolio.positions.is_empty() && b.portfolio.trades.is_empty() && b.params.is_none()
        );
        save(&c, &b, 3_000).unwrap();
        let n: i64 = c
            .query_row("SELECT count(*) FROM paper_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "one account row, replaced in place");
    }
}

//! How far back moomoo's 15m candle history reaches for one symbol already in this week's quota (a re-pull costs no quota).
//! Read-only: pulls candles, stores nothing, places no orders.
//!
//!   cargo run --release --example history_depth -- SPY 2023-01-03
use anyhow::{Context, Result};
use chrono::NaiveDate;
use moomoo_bot::engine::moomoo::{fmt_et, Gateway};

/// Pulls `symbol`'s 15m candles from the given date and prints the first and last candle and the count.
#[tokio::main]
async fn main() -> Result<()> {
    let sym = std::env::args().nth(1).unwrap_or_else(|| "SPY".into());
    let from = NaiveDate::parse_from_str(
        &std::env::args()
            .nth(2)
            .unwrap_or_else(|| "2023-01-03".into()),
        "%Y-%m-%d",
    )
    .context("date YYYY-MM-DD")?;
    let gw = Gateway::connect("claude-moomoo-history-depth").await?;
    let q = gw.quota().await?;
    anyhow::ensure!(q.pulled.contains_key(&sym), "{sym} is not in this week's quota: a pull would spend a unit; pick a symbol pulled in the last 7 days");
    let s = gw.sessions_from(from).await?;
    let (b, off) = gw.bars(&sym, s.list()[0].0, &s).await?;
    let days: std::collections::BTreeSet<_> = b
        .keys()
        .map(|&w| moomoo_bot::engine::moomoo::et_date(w))
        .collect();
    println!("{sym}: {} candles on {} sessions, first {}, last {} ({off} off the calendar); calendar has {} sessions from {from}",
        b.len(), days.len(), b.keys().next().map_or("-".into(), |&w| fmt_et(w)), b.keys().next_back().map_or("-".into(), |&w| fmt_et(w)), s.list().len());
    Ok(())
}

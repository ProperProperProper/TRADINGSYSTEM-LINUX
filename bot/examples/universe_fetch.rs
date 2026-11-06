//! Fill target/oos-cache/history.json with the 15m history (from the research start, `common::RESEARCH_FROM`) of SPY's holdings that
//! the research cache lacks or holds only from a later start, in weight order, within the candle quota: symbols already pulled this week are free, a new one costs one unit and is only
//! taken while more than RESERVE units remain (the live bot keeps its own headroom). Delisting or otherwise ineligible securities
//! are skipped BEFORE any quota is spent. Run it daily: quota frees up as old pulls expire.
//!
//!   cargo run --release --example universe_fetch
mod common;

use anyhow::{Context, Result};
use moomoo_bot::engine::moomoo::{ineligible, load_holdings, prefer_selection, Gateway, SPY_POOL};
use moomoo_bot::engine::{Bar, REFERENCE};
use std::collections::BTreeSet;
use std::time::Duration;

/// Quota units left untouched for the live service.
const RESERVE: i32 = 4;

/// Fills the research cache with the 15m history of SPY's holdings it lacks, heaviest first, paced and within the quota reserve.
#[tokio::main]
async fn main() -> Result<()> {
    let holdings = load_holdings(std::path::Path::new("data/spy_holdings.csv"))
        .context("data/spy_holdings.csv missing")?;
    // ThomasAi's weekly pick first (when a fresh one exists), then SPY's weight order
    let wanted: Vec<String> = prefer_selection(holdings)
        .into_iter()
        .take(SPY_POOL)
        .chain(std::iter::once(REFERENCE.to_string()))
        .collect();
    let path = "target/oos-cache/history.json";
    let mut rows: Vec<(String, Vec<(i64, Bar)>)> = match std::fs::read(path) {
        Ok(x) => serde_json::from_slice(&x)?,
        Err(_) => Vec::new(),
    };
    // Symbols already fetched from this research start (a marker file per start date), so a deepening pull happens once.
    let research = common::research_from()?;
    let marker = format!("target/oos-cache/deep-{research}.json");
    let mut deep: BTreeSet<String> = std::fs::read(&marker)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let have: BTreeSet<String> = rows
        .iter()
        .map(|r| r.0.clone())
        .filter(|s| deep.contains(s))
        .collect();
    let todo: Vec<&String> = wanted.iter().filter(|s| !have.contains(*s)).collect();
    eprintln!(
        "{} of {} wanted symbols already cached; {} to fetch",
        wanted.len() - todo.len(),
        wanted.len(),
        todo.len()
    );
    if todo.is_empty() {
        return Ok(());
    }
    let gw = Gateway::connect("claude-moomoo-universe-fetch").await?;
    let sessions = gw.sessions_from(research).await?;
    let from = sessions.list()[0].0;
    let mut q = gw.quota().await?;
    let info = gw
        .static_info(&todo.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        .await?;
    let mut fetched = 0;
    for sym in todo {
        match info.get(sym) {
            None => {
                eprintln!("  {sym}: no static information, skipped");
                continue;
            }
            Some(b) => {
                // The reference (SPY, an NYSE Arca fund) is never traded, only measured: the listing rule applies to holdings only.
                if let Some(why) = ineligible(b).filter(|_| sym.as_str() != REFERENCE) {
                    eprintln!("  {sym}: skipped before spending quota ({why})");
                    continue;
                }
            }
        }
        let free = q.pulled.contains_key(sym);
        if !free && q.remain <= RESERVE {
            // a new stock would spend quota: skip it (it waits for old pulls to expire) but keep deepening the free ones
            eprintln!("  {sym}: not in this week's quota and the reserve is reached (remaining {}); waits", q.remain);
            continue;
        }
        match gw.bars(sym, from, &sessions).await {
            Ok((b, off)) => {
                eprintln!(
                    "  {sym}: {} candles ({off} off the calendar dropped){}",
                    b.len(),
                    if free { " [free re-pull]" } else { "" }
                );
                rows.retain(|r| &r.0 != sym);
                rows.push((sym.clone(), b.into_iter().collect()));
                deep.insert(sym.clone());
                fetched += 1;
            }
            Err(e) => eprintln!("  {sym}: FAILED {e:#}"),
        }
        q = gw.quota().await?;
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
    std::fs::create_dir_all("target/oos-cache")?;
    std::fs::write(path, serde_json::to_vec(&rows)?)?;
    std::fs::write(&marker, serde_json::to_vec(&deep)?)?;
    eprintln!(
        "fetched {fetched}; quota now used {}, remaining {}",
        q.used, q.remain
    );
    Ok(())
}

//! Research data: the longest 15m history moomoo gives (from the clock epoch) for symbols ALREADY
//! pulled inside the current 7-day candle-quota window, so it costs no quota (re-pulling a symbol
//! in the window is free). Any symbol not in moomoo's pulled list is skipped, and the run stops if
//! the used quota moves. Written to target/oos-cache/history.json for examples/oos_study.rs; the
//! bot's store is not touched. Paced well below moomoo's request limit (the live bot shares it).
//!
//!   cargo run --release --example history_fetch
use anyhow::Result;
use moomoo_bot::engine::moomoo::{Gateway, CLOCK_EPOCH};
use moomoo_bot::engine::{Bar, REFERENCE};
use rusqlite::{Connection, OpenFlags};
use std::time::Duration;

/// Pulls the full 15m candle history (from the clock epoch) for every symbol already in the store, within the candle quota, into the research cache.
#[tokio::main]
async fn main() -> Result<()> {
    let db = Connection::open_with_flags(
        moomoo_bot::platform::bot_store_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let mut st =
        db.prepare("SELECT DISTINCT symbol FROM candles WHERE kind = 'trades' ORDER BY symbol")?;
    let syms: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let gw = Gateway::connect("claude-moomoo-history-fetch").await?;
    let sessions = gw.sessions().await?;
    let q0 = gw.quota().await?;
    eprintln!(
        "quota before: used {}, remaining {}, pulled {}",
        q0.used,
        q0.remain,
        q0.pulled.len()
    );
    let from = sessions.list()[0].0;
    eprintln!("history from {:?} (clock epoch)", CLOCK_EPOCH);
    let mut rows: Vec<(String, Vec<(i64, Bar)>)> = Vec::new();
    for sym in &syms {
        if !q0.pulled.contains_key(sym) {
            eprintln!("  SKIP {sym}: not pulled in the current quota window (would cost quota)");
            continue;
        }
        match gw.bars(sym, from, &sessions).await {
            Ok((b, off)) => {
                eprintln!(
                    "  {sym}: {} candles ({} off the calendar dropped), first {:?}",
                    b.len(),
                    off,
                    b.keys()
                        .next()
                        .map(|&w| moomoo_bot::engine::moomoo::fmt_et(w))
                );
                rows.push((sym.clone(), b.into_iter().collect()));
            }
            Err(e) => eprintln!("  {sym}: FAILED {e:#}"),
        }
        let q = gw.quota().await?;
        anyhow::ensure!(
            q.used == q0.used,
            "quota used moved ({} -> {}): stopping",
            q0.used,
            q.used
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
    }
    anyhow::ensure!(rows.iter().any(|r| r.0 == REFERENCE), "{REFERENCE} missing");
    let q = gw.quota().await?;
    eprintln!("quota after: used {}, remaining {}", q.used, q.remain);
    std::fs::create_dir_all("target/oos-cache")?;
    std::fs::write("target/oos-cache/history.json", serde_json::to_vec(&rows)?)?;
    Ok(())
}

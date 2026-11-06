//! Add one symbol's full 15m history (from the clock epoch) to target/oos-cache/history.json, e.g. a new trend
//! reference. Costs ONE candle-quota unit if the symbol is not already pulled in the current 7-day window; the
//! bot's store is not touched.
//!
//!   cargo run --release --example ref_fetch -- SPY
use anyhow::{Context, Result};
use moomoo_bot::engine::moomoo::Gateway;
use moomoo_bot::engine::Bar;

/// Pulls one symbol's full candle history into the research cache (for a reference or a missing symbol), respecting the quota.
#[tokio::main]
async fn main() -> Result<()> {
    let sym = std::env::args().nth(1).context("symbol")?;
    let gw = Gateway::connect("claude-moomoo-ref-fetch").await?;
    let sessions = gw.sessions().await?;
    let q0 = gw.quota().await?;
    eprintln!("quota before: used {}, remaining {}", q0.used, q0.remain);
    anyhow::ensure!(q0.remain > 5, "candle quota nearly used up");
    let (b, off) = gw.bars(&sym, sessions.list()[0].0, &sessions).await?;
    eprintln!(
        "{sym}: {} candles ({off} off the calendar dropped), first {:?}",
        b.len(),
        b.keys()
            .next()
            .map(|&w| moomoo_bot::engine::moomoo::fmt_et(w))
    );
    let path = "target/oos-cache/history.json";
    let mut rows: Vec<(String, Vec<(i64, Bar)>)> = match std::fs::read(path) {
        Ok(x) => serde_json::from_slice(&x)?,
        Err(_) => Vec::new(),
    };
    rows.retain(|r| r.0 != sym);
    rows.push((sym.clone(), b.into_iter().collect()));
    std::fs::create_dir_all("target/oos-cache")?;
    std::fs::write(path, serde_json::to_vec(&rows)?)?;
    let q = gw.quota().await?;
    eprintln!("quota after: used {}, remaining {}", q.used, q.remain);
    Ok(())
}

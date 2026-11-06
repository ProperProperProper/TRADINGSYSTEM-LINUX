//! Does moomoo's US PAPER (SIMULATE) account accept a fractional share quantity over the API?
//! Places ONE limit buy of `QTY` (default 0.5) share of `SYMBOL` at half the current bid (so it
//! cannot fill), reads the order back, then cancels it. Paper account only (the gateway refuses
//! anything else). Leaves no position behind.
//!
//!   cargo run --release --example fractional_check -- [SYMBOL] [QTY]
use anyhow::{Context, Result};
use moomoo_bot::engine::moomoo::{order_final, Gateway};
use std::time::Duration;

/// Checks whether moomoo paper accepts a fractional-share order, with a limit price far below the bid so it cannot fill; cancels it afterwards.
#[tokio::main]
async fn main() -> Result<()> {
    let symbol = std::env::args().nth(1).unwrap_or_else(|| "F".into());
    let qty: f64 = std::env::args()
        .nth(2)
        .map(|q| q.parse())
        .transpose()?
        .unwrap_or(0.5);
    let gw = Gateway::connect("claude-moomoo-fractional-check").await?;
    let q = gw.quotes(std::slice::from_ref(&symbol)).await?;
    let q = q.get(&symbol).context("no quote")?;
    let price = ((q.bid * 0.5) * 100.0).round() / 100.0;
    println!(
        "{symbol}: bid {} ask {}; limit buy {qty} @ {price} (cannot fill)",
        q.bid, q.ask
    );
    let (oid, _) = match gw
        .place_limit_probe(&symbol, 1, qty, price, "claude-moomoo-bot fractional_check")
        .await
    {
        Ok(x) => x,
        Err(e) => {
            println!("REJECTED at placement: {e:#}");
            return Ok(());
        }
    };
    println!("accepted: order {oid}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    for o in gw.orders(&[oid]).await? {
        println!(
            "  status {} qty {} filled {} err {:?}",
            o.status, o.qty, o.fill_qty, o.last_err
        );
        if !order_final(o.status) {
            gw.cancel(oid).await?;
            println!("  cancel sent");
        }
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    for o in gw.orders(&[oid]).await? {
        println!(
            "  after cancel: status {} qty {} filled {} err {:?}",
            o.status, o.qty, o.fill_qty, o.last_err
        );
    }
    Ok(())
}

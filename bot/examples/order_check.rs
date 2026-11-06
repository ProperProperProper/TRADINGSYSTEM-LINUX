//! Verifies, on the PAPER account only and through the bot's own `Gateway` code path
//! (`place_market`, `orders`, `account`, `order_fees`), what the bot relies on for execution: a
//! market order placed with TrdEnv SIMULATE fills and the order list reports its fill quantity and
//! average price. Trd_GetOrderFee is tried too (moomoo answers "Paper trading is not supported"),
//! so each order's fee is measured as the paper cash change around its fill minus the fill value
//! (`derived_fee`) and stored as a real fee observation in the bot's store. Buys `QTY` share(s) of
//! `SYMBOL` and sells them again. The gateway refuses anything but the US SIMULATE account.
//!
//!   cargo run --release --example order_check -- SYMBOL [QTY]     (US market must be open)
use anyhow::{bail, Result};
use moomoo_bot::engine::moomoo::{derived_fee, order_final, Gateway, OrderFee};
use moomoo_bot::engine::store::Store;
use std::time::Duration;

/// Buys and sells one small lot on the PAPER account and prints the real fills, fees and cash change (end-to-end order check).
#[tokio::main]
async fn main() -> Result<()> {
    let symbol = std::env::args().nth(1).unwrap_or_else(|| "INTC".into());
    let qty: f64 = std::env::args()
        .nth(2)
        .map(|q| q.parse())
        .transpose()?
        .unwrap_or(1.0);
    let gw = Gateway::connect("claude-moomoo-order-check").await?;
    println!("paper account {} (SIMULATE)", gw.account);
    let store = Store::open(&moomoo_bot::platform::bot_store_path()).ok();
    let mut ids_ex = Vec::new();
    for side in [1, 2] {
        let before = gw.account().await?.cash;
        let (oid, oid_ex) = gw
            .place_market(&symbol, side, qty, "claude-moomoo-bot order_check")
            .await?;
        println!(
            "{} placed: order {oid} ({oid_ex})",
            if side == 1 { "BUY" } else { "SELL" }
        );
        ids_ex.push(oid_ex.clone());
        let mut last = None;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            if let Some(o) = gw
                .orders(&[oid])
                .await?
                .into_iter()
                .find(|o| o.order_id == oid)
            {
                let done = order_final(o.status);
                last = Some(o);
                if done {
                    break;
                }
            }
        }
        let Some(o) = last.filter(|o| o.status == 11) else {
            bail!("order {oid} did not fill")
        };
        let after = gw.account().await?.cash;
        let value = o.fill_qty * o.fill_avg_price.unwrap_or(0.0);
        let fee = derived_fee(side, value, before, after);
        println!("  filled {} @ {:?}; cash {before:.4} -> {after:.4}; fill value {value:.4}; fee {fee:.4}", o.fill_qty, o.fill_avg_price);
        if let Some(st) = &store {
            let f = OrderFee {
                order_id_ex: oid_ex,
                total: fee,
                items: vec![("derived from paper cash change".into(), fee)],
            };
            st.order_fee(
                chrono::Utc::now().timestamp_millis(),
                &f,
                &symbol,
                side,
                o.fill_qty,
                o.fill_avg_price.unwrap_or(0.0),
            )?;
        }
    }
    match gw.order_fees(&ids_ex).await {
        Ok(f) => println!("Trd_GetOrderFee: {f:?}"),
        Err(e) => println!("Trd_GetOrderFee: {e:#}"),
    }
    Ok(())
}

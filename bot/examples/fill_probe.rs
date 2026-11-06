//! How does the moomoo PAPER simulator fill orders of increasing size? For one very liquid stock: read the quote, BUY at market, wait for the
//! fill, then SELL the same quantity straight back, and record fill price against the quote and the fee (from the cash change). PAPER account only
//! (the gateway refuses anything else); needs the US market open. The results are appended to data/fill_probe.json: real measurements that the
//! liquidity model is calibrated on, instead of the top-of-book snapshot it used.
//!
//!   cargo run --release --example fill_probe -- [SYMBOL=AAPL] [notionals=300,30000,150000,500000]
use anyhow::{bail, Context, Result};
use moomoo_bot::engine::moomoo::{derived_fee, order_final, Gateway};
use serde_json::json;
use std::time::Duration;

/// Places one paper market order, waits up to 60 s for it to fill completely, and returns (filled qty, average price, fee derived from the cash change, cash after).
async fn fill(gw: &Gateway, symbol: &str, side: i32, qty: f64) -> Result<(f64, f64, f64, f64)> {
    let before = gw.account().await?.cash;
    let (oid, _) = gw
        .place_market(symbol, side, qty, "claude-moomoo-bot fill_probe")
        .await?;
    let mut last = None;
    for _ in 0..40 {
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
    let o = last
        .filter(|o| o.status == 11)
        .context("order did not fill completely")?;
    let after = gw.account().await?.cash;
    let value = o.fill_qty * o.fill_avg_price.unwrap_or(0.0);
    Ok((
        o.fill_qty,
        o.fill_avg_price.unwrap_or(0.0),
        derived_fee(side, value, before, after),
        after,
    ))
}

/// Fill probe: buys then sells a ladder of order sizes on the PAPER account and records fill completeness and slippage in data/fill_probe.json.
#[tokio::main]
async fn main() -> Result<()> {
    let symbol = std::env::args().nth(1).unwrap_or_else(|| "AAPL".into());
    let ladder: Vec<f64> = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "300,30000,150000,500000".into())
        .split(',')
        .filter_map(|x| x.parse().ok())
        .collect();
    let gw = Gateway::connect("claude-moomoo-fill-probe").await?;
    let acct = gw.account().await?;
    println!(
        "paper account {} (SIMULATE): cash {:.2}",
        gw.account, acct.cash
    );
    let path = "data/fill_probe.json";
    let mut rows: Vec<serde_json::Value> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    for notional in ladder {
        anyhow::ensure!(
            notional <= acct.cash * 0.6,
            "probe size {notional} is more than 60% of the cash"
        );
        let q = gw
            .quotes(std::slice::from_ref(&symbol))
            .await?
            .remove(&symbol)
            .context("no quote")?;
        anyhow::ensure!(q.valid(), "no valid quote");
        let qty = (notional / q.ask).floor().max(1.0);
        println!(
            "\\n{symbol}: quote bid {} x {} / ask {} x {}; probing {qty} shares (~{:.0} USD)",
            q.bid,
            q.bid_vol,
            q.ask,
            q.ask_vol,
            qty * q.ask
        );
        let buy = fill(&gw, &symbol, 1, qty).await;
        let (bq, bp, bfee) = match &buy {
            Ok(b) => (b.0, b.1, b.2),
            Err(e) => bail!("BUY failed ({e:#}); nothing to sell back"),
        };
        let q2 = gw
            .quotes(std::slice::from_ref(&symbol))
            .await?
            .remove(&symbol)
            .context("no quote")?;
        let sell = fill(&gw, &symbol, 2, bq).await;
        match sell {
            Ok((sq, sp, sfee, _)) => {
                let buy_slip = (bp / q.ask - 1.0) * 1e4;
                let sell_slip = (1.0 - sp / q2.bid) * 1e4;
                println!("  BUY  {bq} @ {bp:.4}: {buy_slip:+.2} bps vs ask, fee {bfee:.2}\\n  SELL {sq} @ {sp:.4}: {sell_slip:+.2} bps vs bid, fee {sfee:.2}\\n  round trip: {:+.2} USD on {:.0}", (sp - bp) * sq - bfee - sfee, bq * bp);
                rows.push(json!({"ts": chrono::Utc::now().to_rfc3339(), "symbol": symbol, "qty": bq, "notional": bq * bp, "bid": q.bid, "ask": q.ask, "bid_vol": q.bid_vol, "ask_vol": q.ask_vol,
                    "buy_avg": bp, "sell_avg": sp, "buy_slip_bps_vs_ask": buy_slip, "sell_slip_bps_vs_bid": sell_slip, "buy_fee": bfee, "sell_fee": sfee, "sell_qty": sq, "bid_after": q2.bid, "ask_after": q2.ask}));
            }
            Err(e) => bail!("SELL failed ({e:#}): you hold {bq} {symbol} in the paper account; sell it manually"),
        }
        std::fs::write(path, serde_json::to_vec_pretty(&rows)?)?;
    }
    println!("\\nsaved {} measurements to {path}", rows.len());
    Ok(())
}

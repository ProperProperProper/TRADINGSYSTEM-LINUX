//! Replays the paper engine over the stored real candles with ONE settings file (JSON of `XsParams`), exactly as the live
//! service drives it per closed 5h candle (step with the settings in force, then `decide_close`, then `defer_new_decisions`),
//! and prints every candle from `FROM` (YYYY-MM-DD, default 2026-10-08): wall time, pending decision, positions, trades.
//! Purpose: show when the engine alone would enter, without any service plumbing. Usage: replay_decision <params.json> [FROM]
use anyhow::{Context, Result};
use moomoo_bot::engine::execution::Ledger;
use moomoo_bot::engine::moomoo::{build_market, SymbolData};
use moomoo_bot::engine::xs::{XsParams, XsPortfolio};
use moomoo_bot::engine::{scores, walkforward, Interval, BAR_MS};

mod common;
use common::{big_market, fmt, load_data, sessions};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let p: XsParams = serde_json::from_slice(&std::fs::read(args.get(1).context("params.json")?)?)?;
    let from = chrono::NaiveDate::parse_from_str(
        args.get(2).map_or("2026-10-08", |s| s.as_str()),
        "%Y-%m-%d",
    )?
    .and_hms_opt(0, 0, 0)
    .unwrap()
    .and_utc()
    .timestamp_millis();
    let cal = sessions().await?;
    let data = load_data()?;
    let big = big_market(&cal, &data, Interval::H5)?;
    let m = &big.m;
    let sc = scores::compute(m, walkforward::UNIVERSE);
    let start = (0..m.wall.len())
        .find(|&t| m.wall[t] >= from)
        .context("no candle after FROM")?;
    let mut pf = XsPortfolio::new(5000.0);
    pf.enable_events();
    pf.install_entry_gate(true);
    // warm up on the candles before FROM so the state is the live one at FROM
    let cal5 = cal.with_interval(Interval::H5);
    let keep: Vec<SymbolData> = data
        .iter()
        .filter(|d| m.symbols.contains(&d.symbol))
        .map(|d| SymbolData {
            symbol: d.symbol.clone(),
            trades: d.trades.clone(),
            listing_wall: d.listing_wall,
            eligible: d.eligible,
            rules: d.rules.clone(),
        })
        .collect();
    for t in start.saturating_sub(60)..m.ts.len() {
        if t >= start {
            // what the live service would order at this candle's OPEN: the preview on the market up to the previous candle, priced at each symbol's open
            let prefix = build_market(&cal5, &m.wall[..t], &keep)?;
            let sc_prefix = scores::compute(&prefix, walkforward::UNIVERSE);
            let mids: std::collections::HashMap<String, f64> = (0..m.symbols.len())
                .filter_map(|s| m.bars[s][t].map(|b| (m.symbols[s].clone(), b.open)))
                .collect();
            let plan = Ledger::default()
                .preview(&pf, &prefix, &sc_prefix, &p, m.ts[t], m.wall[t], &mids)?;
            println!(
                "  PREVIEW at {} open: {}",
                fmt(m.wall[t]),
                if plan.is_empty() {
                    "EMPTY".to_string()
                } else {
                    plan.iter()
                        .map(|x| {
                            format!(
                                "{} {} x{}",
                                if x.side == 1 { "BUY" } else { "SELL" },
                                x.symbol,
                                x.qty
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            );
        }
        let before = (pf.positions.len(), pf.trades.len());
        pf.step(m, &sc, t, &p);
        pf.decide_close(m, &sc, t, &p);
        pf.defer_new_decisions(m.ts[t] + BAR_MS, m.ts[t]);
        let ev = pf.take_events();
        if t < start {
            continue;
        }
        let v = serde_json::to_value(&pf)?;
        println!(
            "{} candle ts={} pos {}->{} trades {}->{} pending={} note={}",
            fmt(m.wall[t]),
            m.ts[t],
            before.0,
            pf.positions.len(),
            before.1,
            pf.trades.len(),
            v["pending_targets"],
            pf.allocation_note
        );
        for e in ev.iter().filter(|e| e.kind != "decision") {
            println!("    event {} {:?} {}", e.kind, e.symbol, e.detail);
        }
    }
    Ok(())
}

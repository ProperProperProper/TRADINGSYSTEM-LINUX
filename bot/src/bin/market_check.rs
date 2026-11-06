//! Which markets OpenD serves quotes for: stock lookup (static info) and a quote snapshot for an
//! ASX stock (BHP) and a US stock (AAPL). Read-only; uses no historical-candle quota.
use anyhow::Result;
use moomoo_bot::opend::{id, OpenD, DEFAULT_ADDR};
use moomoo_bot::pb;

/// Probe: asks OpenD for static info and a quote of one US and one ASX stock to show which markets this login can see.
#[tokio::main]
async fn main() -> Result<()> {
    let mut c = OpenD::connect(DEFAULT_ADDR, "claude-moomoo-market-check").await?;
    for (market, code, label) in [(51, "BHP", "ASX"), (11, "AAPL", "US")] {
        let sec = pb::qot_common::Security {
            market,
            code: code.into(),
        };
        let info = c
            .call::<pb::qot_get_static_info::Response>(
                id::QOT_GET_STATIC_INFO,
                &pb::qot_get_static_info::Request {
                    c2s: pb::qot_get_static_info::C2s {
                        market: None,
                        sec_type: None,
                        security_list: vec![sec.clone()],
                        ..Default::default()
                    },
                },
            )
            .await;
        match info {
            Ok(s) => println!(
                "{label} {code} lookup: {:?}",
                s.static_info_list.first().map(|x| (
                    &x.basic.name,
                    x.basic.lot_size,
                    &x.basic.list_time
                ))
            ),
            Err(e) => println!("{label} {code} lookup refused: {e:#}"),
        }
        let snap = c
            .call::<pb::qot_get_security_snapshot::Response>(
                id::QOT_GET_SECURITY_SNAPSHOT,
                &pb::qot_get_security_snapshot::Request {
                    c2s: pb::qot_get_security_snapshot::C2s {
                        security_list: vec![sec],
                        ..Default::default()
                    },
                },
            )
            .await;
        match snap {
            Ok(s) => println!(
                "{label} {code} snapshot: last price {:?}",
                s.snapshot_list.first().map(|x| x.basic.cur_price)
            ),
            Err(e) => println!("{label} {code} snapshot refused: {e:#}"),
        }
    }
    Ok(())
}

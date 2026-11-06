//! Read-only: does moomoo publish S&P 500 / index membership as a "plate" (a sector or index set)? Lists the US plates whose
//! name looks like an S&P / index set and, for a chosen plate, its members with the delisting flag. No candle requests, no orders.
//!
//!   cargo run --release --example universe_probe                      # list matching plates
//!   cargo run --release --example universe_probe -- <plate code>      # members of a plate, e.g. US.BK1001
use anyhow::Result;
use moomoo_bot::opend::{id, OpenD, DEFAULT_ADDR};
use moomoo_bot::pb;

/// Probe: lists a moomoo plate's members (with a code argument) or the available plates; read-only.
#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::var("OPEND").unwrap_or_else(|_| DEFAULT_ADDR.into());
    let mut c = OpenD::connect(&addr, "claude-moomoo-universe-probe").await?;
    if let Some(code) = std::env::args().nth(1) {
        let code = code.trim_start_matches("US.").to_string();
        let r = c
            .call::<pb::qot_get_plate_security::Response>(
                id::QOT_GET_PLATE_SECURITY,
                &pb::qot_get_plate_security::Request {
                    c2s: pb::qot_get_plate_security::C2s {
                        plate: pb::qot_common::Security {
                            market: 11,
                            code: code.clone(),
                        },
                        sort_field: None,
                        ascend: None,
                        ..Default::default()
                    },
                },
            )
            .await?;
        println!("plate {code}: {} members", r.static_info_list.len());
        for s in r.static_info_list.iter().take(200) {
            println!(
                "  {} {} delisting={:?} listed={:?}",
                s.basic.security.code, s.basic.name, s.basic.delisting, s.basic.list_time
            );
        }
        return Ok(());
    }
    for (ty, label) in [(0, "all"), (1, "industry"), (3, "concept")] {
        let r = c
            .call::<pb::qot_get_plate_set::Response>(
                id::QOT_GET_PLATE_SET,
                &pb::qot_get_plate_set::Request {
                    c2s: pb::qot_get_plate_set::C2s {
                        market: 11,
                        plate_set_type: ty,
                        ..Default::default()
                    },
                },
            )
            .await;
        match r {
            Ok(r) => {
                println!("{label}: {} plates", r.plate_info_list.len());
                for p in &r.plate_info_list {
                    println!(
                        "  {} | {}",
                        p.plate.code,
                        p.name.split(" 法:").next().unwrap_or(&p.name)
                    );
                }
            }
            Err(e) => println!("{label}: unavailable ({e:#})"),
        }
    }
    Ok(())
}

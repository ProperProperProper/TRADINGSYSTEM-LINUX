//! Read-only: which moomoo PAPER accounts OpenD reports and their balances. Real accounts are only counted,
//! never queried. No candle requests (no quota), no orders.
//!
//!   cargo run --release --example accounts_check
use anyhow::Result;
use moomoo_bot::opend::{id, OpenD, DEFAULT_ADDR};
use moomoo_bot::pb;

/// Lists the accounts OpenD reports and prints the PAPER accounts' funds; real accounts are counted but never queried.
#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::var("OPEND").unwrap_or_else(|_| DEFAULT_ADDR.into());
    let mut c = OpenD::connect(&addr, "claude-moomoo-accounts-check").await?;
    let accs = c
        .call::<pb::trd_get_acc_list::Response>(
            id::TRD_GET_ACC_LIST,
            &pb::trd_get_acc_list::Request {
                c2s: pb::trd_get_acc_list::C2s {
                    user_id: 0,
                    trd_category: Some(1),
                    need_general_sec_account: Some(true),
                },
            },
        )
        .await?;
    let real = accs.acc_list.iter().filter(|a| a.trd_env != 0).count();
    println!(
        "{} account(s) reported: {} paper, {real} real (real accounts are never queried)",
        accs.acc_list.len(),
        accs.acc_list.len() - real
    );
    for acc in accs.acc_list.iter().filter(|a| a.trd_env == 0) {
        println!(
            "PAPER account {} | markets {:?} | sim type {:?} | status {:?} | firm {:?}",
            acc.acc_id,
            acc.trd_market_auth_list,
            acc.sim_acc_type,
            acc.acc_status,
            acc.security_firm
        );
        for &m in acc.trd_market_auth_list.iter().filter(|&&m| m == 2) {
            let f = c
                .call::<pb::trd_get_funds::Response>(
                    id::TRD_GET_FUNDS,
                    &pb::trd_get_funds::Request {
                        c2s: pb::trd_get_funds::C2s {
                            header: pb::trd_common::TrdHeader {
                                trd_env: 0,
                                acc_id: acc.acc_id,
                                trd_market: m,
                                jp_acc_type: None,
                            },
                            refresh_cache: Some(true),
                            currency: None,
                            asset_category: None,
                        },
                    },
                )
                .await;
            match f.map(|x| x.funds) {
                Ok(Some(f)) => println!("   US paper funds: net assets {:.2}, cash {:.2}, buying power {:.2}, currency {:?}", f.total_assets, f.cash, f.power, f.currency),
                Ok(None) => println!("   US paper funds: none returned"),
                Err(e) => println!("   US paper funds unavailable: {e:#}"),
            }
        }
    }
    Ok(())
}

//! Answers, from moomoo OpenD itself, what the bot can use: login state, quote rights, the
//! historical-candle quota, the trading accounts (paper and real) with their markets, paper
//! balances, and whether 1-minute candles come back for a US stock (AAPL) and an ASX stock (BHP).
//! Read-only: places no orders. Uses 2 of the 7-day historical-candle quota (AAPL, BHP), and
//! none if they were already pulled within 7 days.
//!
//!   cargo run --release --bin probe            (OpenD must be running and logged in)
//!
//! Env: `OPEND=<host:port>` (default 127.0.0.1:11111).
use anyhow::Result;
use moomoo_bot::opend::{id, OpenD, DEFAULT_ADDR};
use moomoo_bot::pb;

/// Human name of a moomoo market code.
fn market_name(m: i32) -> &'static str {
    match m {
        1 => "HK",
        2 => "US",
        3 => "CN (paper only)",
        4 => "HKCC",
        5 => "Futures",
        6 => "SG",
        8 => "AU",
        15 => "JP",
        _ => "other",
    }
}

/// Read-only probe of OpenD: login state, accounts, funds, candle quota and a sample candle request.
#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::var("OPEND").unwrap_or_else(|_| DEFAULT_ADDR.into());
    let mut c = OpenD::connect(&addr, "claude-moomoo-probe").await?;
    println!(
        "OpenD {addr}: server version {}, connection {}, keep-alive every {} s",
        c.server_ver, c.conn_id, c.keep_alive_secs
    );

    let g = c
        .call::<pb::get_global_state::Response>(
            id::GET_GLOBAL_STATE,
            &pb::get_global_state::Request {
                c2s: pb::get_global_state::C2s { user_id: 0 },
            },
        )
        .await?;
    println!(
        "logged in: quotes {}, trading {}; US market state {}",
        g.qot_logined, g.trd_logined, g.market_us
    );

    match c
        .call::<pb::get_user_info::Response>(id::GET_USER_INFO, &pb::get_user_info::Request { c2s: pb::get_user_info::C2s { flag: None } })
        .await
    {
        Ok(u) => println!(
            "user: US quote right {:?}, subscription quota {:?}, historical-candle quota {:?} (stocks per 7 days), attribution {:?}",
            u.us_qot_right, u.sub_quota, u.history_kl_quota, u.user_attribution
        ),
        Err(e) => println!("user info unavailable: {e:#}"),
    }

    match c
        .call::<pb::qot_request_history_kl_quota::Response>(
            id::QOT_REQUEST_HISTORY_KL_QUOTA,
            &pb::qot_request_history_kl_quota::Request {
                c2s: pb::qot_request_history_kl_quota::C2s {
                    b_get_detail: Some(false),
                    ..Default::default()
                },
            },
        )
        .await
    {
        Ok(q) => println!(
            "historical-candle quota: used {}, remaining {}",
            q.used_quota, q.remain_quota
        ),
        Err(e) => println!("historical-candle quota unavailable: {e:#}"),
    }

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
        .await;
    match accs {
        Ok(a) => {
            println!("{} trading account(s):", a.acc_list.len());
            for acc in &a.acc_list {
                let env = if acc.trd_env == 0 { "PAPER" } else { "REAL" };
                let markets: Vec<&str> = acc
                    .trd_market_auth_list
                    .iter()
                    .map(|&m| market_name(m))
                    .collect();
                println!(
                    "  {env} account {} markets {markets:?} firm {:?} sim type {:?} status {:?}",
                    acc.acc_id, acc.security_firm, acc.sim_acc_type, acc.acc_status
                );
                if acc.trd_env != 0 {
                    continue; // read paper balances only
                }
                for &m in acc
                    .trd_market_auth_list
                    .iter()
                    .filter(|&&m| m == 2 || m == 8)
                {
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
                        Ok(Some(f)) => println!(
                            "    {} paper funds: net assets {:.2}, cash {:.2}, buying power {:.2}, currency {:?}",
                            market_name(m), f.total_assets, f.cash, f.power, f.currency
                        ),
                        Ok(None) => println!("    {} paper funds: none returned", market_name(m)),
                        Err(e) => println!("    {} paper funds unavailable: {e:#}", market_name(m)),
                    }
                }
            }
        }
        Err(e) => println!("trading accounts unavailable: {e:#}"),
    }

    let today = chrono::Utc::now().date_naive();
    let begin = (today - chrono::Duration::days(7))
        .format("%Y-%m-%d")
        .to_string();
    let end = today.format("%Y-%m-%d").to_string();
    for (market, code, label) in [(11, "AAPL", "US"), (51, "BHP", "ASX")] {
        let r = c
            .call::<pb::qot_request_history_kl::Response>(
                id::QOT_REQUEST_HISTORY_KL,
                &pb::qot_request_history_kl::Request {
                    c2s: pb::qot_request_history_kl::C2s {
                        rehab_type: 0, // unadjusted
                        kl_type: 7,    // 15 minutes
                        security: pb::qot_common::Security {
                            market,
                            code: code.into(),
                        },
                        begin_time: begin.clone(),
                        end_time: end.clone(),
                        max_ack_kl_num: Some(1000),
                        need_kl_fields_flag: None,
                        next_req_key: None,
                        extended_time: Some(false),
                        session: None,
                        ..Default::default()
                    },
                },
            )
            .await;
        match r {
            Ok(k) => {
                let real: Vec<_> = k.kl_list.iter().filter(|x| !x.is_blank).collect();
                println!(
                    "{label} {code} 1m candles {begin}..{end}: {} ({} blank); first {:?} last {:?} close {:?}; more pages: {}",
                    real.len(),
                    k.kl_list.len() - real.len(),
                    real.first().map(|x| &x.time),
                    real.last().map(|x| &x.time),
                    real.last().and_then(|x| x.close_price),
                    k.next_req_key.is_some()
                );
            }
            Err(e) => println!("{label} {code} 1m candles unavailable: {e:#}"),
        }
    }
    Ok(())
}

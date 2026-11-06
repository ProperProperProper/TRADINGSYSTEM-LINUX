//! Long-running keep-alive check against a running OpenD: connects, then sends KeepAlive at the SDK's period (4/5 of the announced interval) for the given
//! number of seconds (default 180) and prints every round trip. Read-only; uses no historical-candle quota.
use anyhow::{ensure, Result};
use moomoo_bot::opend::{OpenD, DEFAULT_ADDR};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<()> {
    let total: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(180);
    let mut c = OpenD::connect(DEFAULT_ADDR, "claude-moomoo-keepalive-check").await?;
    println!("connected: server_ver {}, conn_id {}, announced interval {} s, pinging every {} s for {total} s", c.server_ver, c.conn_id, c.keep_alive_secs, c.keep_alive_every());
    let (start, mut n) = (Instant::now(), 0);
    while start.elapsed() < Duration::from_secs(total) {
        let t = Instant::now();
        let server_time = c.keep_alive().await?;
        n += 1;
        println!(
            "ping {n} at {:>4.0} s: ok in {} ms, server time {server_time}",
            start.elapsed().as_secs_f64(),
            t.elapsed().as_millis()
        );
        tokio::time::sleep(Duration::from_secs(c.keep_alive_every())).await;
    }
    ensure!(n > 0, "no KeepAlive was sent");
    println!(
        "PASS: {n} KeepAlive round trips over {} s on one connection",
        start.elapsed().as_secs()
    );
    Ok(())
}

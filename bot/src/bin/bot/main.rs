//! moomoo mean-reversion bot for US stocks, long only, supported live intervals,
//! settings from a random search, decisions executed as REAL moomoo paper
//! orders (US SIMULATE account only).
//!
//!   bot serve        paper-trading service + console on 127.0.0.1:8789

use anyhow::Result;
use moomoo_bot::engine::runtime_dir;

mod service;

/// Entry point: `bot serve` runs the trading service from the runtime folder.
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match std::env::args().nth(1).as_deref() {
        Some("serve") => service::serve(runtime_dir()).await,
        _ => {
            eprintln!("usage: bot serve");
            std::process::exit(2);
        }
    }
}

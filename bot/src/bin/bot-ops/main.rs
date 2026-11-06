//! `bot-ops`: everything the shell scripts of this repo did, in Rust, identical on macOS, Linux and Windows.
//!
//!   bot-ops deploy                 build, test, lint, install the bot service and every bot job (was deploy.sh)
//!   bot-ops check [minutes]        compact health check (was check.sh)
//!   bot-ops install                register the bot's background jobs only (deploy does this too)
//!   bot-ops install-service        install the built bot and register ONLY the trading service (no tests, no maintenance jobs)
//!   bot-ops install-fetch          register ONLY the hourly candle importer (fetches ThomasAi's weekly pick first, never trades)
//!   bot-ops monitor check|improve  hourly health check / weekday review (was ops/monitor.sh)
//!   bot-ops cross-check [target ...]  compile-check for Linux and Windows without rustup (was ops/cross_check.sh); fake-cc is its stub C compiler
//!   bot-ops push-guard [repo ...]  every commit reaches GitHub; idle work is committed (was ops/push_guard.sh)
//!   bot-ops log-doctor | fix-report | promote | lab-export | fill-probe
//!   bot-ops spy-holdings-refresh | sync-lab | publish-state | publish-research
//!
//! The shell files with the old names are one-line stubs that run these. The platform layer (folders, probes, job control) is shared with the lab: the files
//! `lab/src/platform.rs` and `lab/src/service.rs` (mirrored from ~/Projects/qwen-lab) are compiled into this binary.
#![allow(dead_code)]

#[path = "../../../lab/src/platform.rs"]
mod platform;
#[path = "../../../lab/src/service.rs"]
mod service;

mod check;
mod common;
mod crosscheck;
mod deploy;
mod doctor;
mod periodic;
mod pushguard;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let code = match args.first().map(String::as_str) {
        Some("deploy") => deploy::deploy(),
        Some("install") => deploy::install_jobs_cmd(),
        Some("install-service") => deploy::install_service_cmd(),
        Some("install-fetch") => deploy::install_fetch_cmd(),
        Some("cross-check") => crosscheck::run(&rest),
        Some("fake-cc") => crosscheck::fake_cc(&rest),
        Some("check") => check::run(rest.first().and_then(|m| m.parse().ok()).unwrap_or(60)),
        Some("push-guard") => pushguard::run(&rest),
        Some("monitor") => doctor::monitor(rest.first().copied().unwrap_or("check")),
        Some("log-doctor") => doctor::log_doctor(),
        Some("fix-report") => doctor::fix_report(),
        Some("promote") => doctor::promote(),
        Some("lab-export") => periodic::lab_export(),
        Some("universe-fetch") => periodic::universe_fetch(),
        Some("fill-probe") => periodic::fill_probe(),
        Some("spy-holdings-refresh") => periodic::spy_holdings_refresh(),
        Some("sync-lab") => periodic::sync_lab(),
        Some("publish-state") => periodic::publish_state(),
        Some("publish-research") => periodic::publish_research(),
        _ => {
            eprintln!("usage: bot-ops deploy | install | check [minutes] | monitor check|improve | push-guard [repo ...] | log-doctor | fix-report | promote | universe-fetch | fill-probe | spy-holdings-refresh");
            2
        }
    };
    std::process::exit(code);
}

//! Does letting every symbol into the universe (rule 14) change how many settings are usable, against the bot's own top-20 universe?
//! Same periods, same draws, only the universe differs (skipped without data).
use moomoo_bot::engine::{scores, walkforward};
use std::path::PathBuf;
use thomas_core::config::{PERIOD_SESSIONS, PERIOD_STEP_SESSIONS, WINDOW_SESSIONS};
use thomas_engine::market::{apply_window, build_on, load_symbol_data};
use thomas_engine::replay::{apply_research_settings, use_liquidity_model, use_warmup_sessions};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn universe_size_against_usable_settings() {
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data not present");
        return;
    }
    use_liquidity_model(true);
    let mut l = load_symbol_data(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
    )
    .unwrap();
    apply_window(&mut l, WINDOW_SESSIONS);
    apply_research_settings();
    let b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    moomoo_bot::engine::governor::UNTHROTTLED.store(true, std::sync::atomic::Ordering::Relaxed);
    let space = walkforward::SearchSpace {
        samples: 30_000,
        min_trades: walkforward::MIN_TRADES,
        ..walkforward::space_for(moomoo_bot::engine::Interval::H5)
    };
    for universe in [walkforward::UNIVERSE, 50, 100, b.market.symbols.len()] {
        let sc = scores::compute(&b.market, universe);
        let first = (0..b.market.ts.len())
            .find(|&t| sc.iter().any(|r| r[t].is_some()))
            .unwrap();
        let mut line = format!(
            "UNIVERSE {universe:>3}: symbols ever scored {:>3} | usable per period:",
            sc.iter().filter(|r| r.iter().any(|x| x.is_some())).count()
        );
        for k in 0..3 {
            let start = first + k * PERIOD_STEP_SESSIONS * 2;
            let r = start..start + PERIOD_SESSIONS * 2;
            let m = b.market.slice(r.clone());
            let rows: Vec<Vec<Option<scores::Score>>> =
                sc.iter().map(|row| row[r.clone()].to_vec()).collect();
            let rep = walkforward::search_random(
                &m,
                &rows,
                5000.0,
                std::time::Instant::now() + std::time::Duration::from_secs(600),
                11 + k as u64,
                &[],
                space,
            )
            .unwrap();
            line += &format!(" {:>5}", rep.usable);
        }
        eprintln!("{line}");
    }
    // trade floor against usable settings, all symbols allowed in
    let sc = scores::compute(&b.market, b.market.symbols.len());
    let first = (0..b.market.ts.len())
        .find(|&t| sc.iter().any(|r| r[t].is_some()))
        .unwrap();
    for floor in [8, 6, 5, 4, 3] {
        let space = walkforward::SearchSpace {
            samples: 30_000,
            min_trades: floor,
            ..walkforward::space_for(moomoo_bot::engine::Interval::H5)
        };
        let mut line = format!("FLOOR {floor} trades: usable per 14-session period:");
        for k in 0..3 {
            let start = first + k * PERIOD_STEP_SESSIONS * 2;
            let r = start..start + PERIOD_SESSIONS * 2;
            let m = b.market.slice(r.clone());
            let rows: Vec<Vec<Option<scores::Score>>> =
                sc.iter().map(|row| row[r.clone()].to_vec()).collect();
            let rep = walkforward::search_random(
                &m,
                &rows,
                5000.0,
                std::time::Instant::now() + std::time::Duration::from_secs(600),
                11 + k as u64,
                &[],
                space,
            )
            .unwrap();
            line += &format!(" {:>5}", rep.usable);
        }
        eprintln!("{line}");
    }
    use_warmup_sessions(0, 2);
}

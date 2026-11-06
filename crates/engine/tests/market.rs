//! Runs the bot's own market builder on the real stored candles (skipped if the data snapshot is absent).
use std::path::PathBuf;
use thomas_engine::market::{RESEARCH_FROM_MS, build_1m, calendar_sessions, load_symbol_data};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn research_floor_is_2026_01_02_utc() {
    assert_eq!(RESEARCH_FROM_MS, 1_767_312_000_000);
}

#[test]
fn real_candles_build_a_valid_bot_market() {
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data/candles.sqlite3 not present");
        return;
    }
    let l = load_symbol_data(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
    )
    .unwrap();
    assert!(
        l.data.iter().any(|d| d.symbol == "SPY"),
        "reference symbol present"
    );
    assert!(
        l.data.len() >= 70,
        "most SPY holdings with candles survive: {}",
        l.data.len()
    );
    // every dropped symbol is explained
    assert!(
        l.dropped
            .iter()
            .all(|(s, why)| !s.is_empty() && !why.is_empty())
    );
    let b = build_1m(&l.data).unwrap();
    assert!(
        b.sessions_count >= 96,
        "at least the 96 sessions moomoo covers: {}",
        b.sessions_count
    );
    // The bot starts its market at the reference symbol's (SPY) first candle, so only sessions from there on are used.
    let spy_first = l
        .data
        .iter()
        .find(|d| d.symbol == "SPY")
        .unwrap()
        .listing_wall;
    let used = calendar_sessions(&l.data)
        .iter()
        .filter(|(open, _)| *open >= spy_first)
        .count();
    assert_eq!(
        b.market.ts.len(),
        used * 26,
        "the expected one-minute session bars from SPY's first candle"
    );
    assert_eq!(b.market.wall[0], spy_first);
    assert!(b.market.validate().is_ok());
    let spy: std::collections::BTreeSet<String> =
        moomoo_bot::engine::moomoo::load_holdings(&data("spy_holdings.csv"))
            .unwrap()
            .into_iter()
            .collect();
    let missing: Vec<&String> = spy
        .iter()
        .filter(|s| !b.market.symbols.contains(s))
        .collect();
    eprintln!(
        "SPY holdings {} / in market {} / missing {:?}",
        spy.len(),
        spy.iter().filter(|s| b.market.symbols.contains(s)).count(),
        missing
    );
    assert!(
        missing.is_empty(),
        "every SPY holding must be in the market: {missing:?}"
    );
    eprintln!(
        "market: {} symbols, {} bars, {} sessions in calendar, loaded {}, dropped {:?}, rejected by engine {:?}",
        b.market.symbols.len(),
        b.market.ts.len(),
        b.sessions_count,
        l.data.len(),
        l.dropped,
        b.rejected
    );
    // bars strictly increase on the trading clock and wall clock
    assert!(b.market.ts.windows(2).all(|w| w[1] - w[0] == 60_000));
    assert!(b.market.wall.windows(2).all(|w| w[1] > w[0]));
}

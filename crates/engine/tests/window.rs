//! The 40-session learning window and the 20-session warm-up, on the real data (skipped without it).
use std::path::PathBuf;
use thomas_core::config::{SEARCH_SESSIONS, WARMUP_SESSIONS, WINDOW_SESSIONS};
use thomas_engine::market::{apply_window, build_on, load_symbol_data};
use thomas_engine::replay::{apply_research_settings, use_warmup_sessions};
use thomas_engine::search::compute_scores_all;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn forty_sessions_for_every_symbol_leave_twenty_search_sessions() {
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data not present");
        return;
    }
    let mut l = load_symbol_data(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
    )
    .unwrap();
    let before = l.data.len();
    let start = apply_window(&mut l, WINDOW_SESSIONS).expect("window start");
    assert_eq!(l.data.len(), before, "no symbol is lost");
    // sessions of 1m bars per symbol inside the window
    let cal: Vec<i64> = l
        .calendar
        .iter()
        .map(|s| s.0)
        .filter(|o| *o >= start)
        .collect();
    assert_eq!(cal.len(), WINDOW_SESSIONS);
    let sessions_of = |d: &moomoo_bot::engine::moomoo::SymbolData| {
        cal.iter()
            .zip(l.calendar.iter().filter(|s| s.0 >= start))
            .filter(|(o, s)| d.trades.range(**o..s.1).next().is_some())
            .count()
    };
    let full: Vec<&str> = l
        .data
        .iter()
        .filter(|d| sessions_of(d) == WINDOW_SESSIONS)
        .map(|d| d.symbol.as_str())
        .collect();
    let short: Vec<(&str, usize)> = l
        .data
        .iter()
        .map(|d| (d.symbol.as_str(), sessions_of(d)))
        .filter(|x| x.1 < WINDOW_SESSIONS)
        .collect();
    eprintln!(
        "WINDOW start {start}: {} symbols, {} with the full {WINDOW_SESSIONS} sessions of 1m, short: {short:?}",
        l.data.len(),
        full.len()
    );
    assert!(l.data.iter().all(|d| sessions_of(d) <= WINDOW_SESSIONS));
    assert!(
        full.len() * 100 >= l.data.len() * 95,
        "at least 95% of symbols have the full window (the rest are new listings)"
    );

    apply_research_settings();
    let b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    let sc = compute_scores_all(&b);
    let first = (0..b.market.ts.len())
        .find(|&t| sc.iter().any(|r| r[t].is_some()))
        .expect("scores exist");
    let scored_sessions = (b.market.ts.len() - first) / 2;
    let scored_symbols = sc.iter().filter(|r| r.iter().any(|x| x.is_some())).count();
    eprintln!(
        "WINDOW 5h market {} candles; first score at candle {first}; scored sessions {scored_sessions}; symbols scored (all allowed in) {scored_symbols}",
        b.market.ts.len()
    );
    assert!(b.market.validate().is_ok() && b.rejected.is_empty());
    assert_eq!(b.market.ts.len(), WINDOW_SESSIONS * 2);
    assert_eq!(
        scored_sessions, SEARCH_SESSIONS,
        "scoring starts after the warm-up candles"
    );
    assert_eq!(SEARCH_SESSIONS, WINDOW_SESSIONS - WARMUP_SESSIONS);
    // rule 14: every symbol is scored except the reference (SPY) and symbols with less history than the warm-up needs
    let unscored: Vec<&str> = (0..b.market.symbols.len())
        .filter(|&s| sc[s].iter().all(Option::is_none))
        .map(|s| b.market.symbols[s].as_str())
        .collect();
    eprintln!("WINDOW unscored symbols: {unscored:?}");
    assert!(
        unscored.iter().all(|s| ["SPY", "SKYD", "VYLR"].contains(s)),
        "only the reference and the two new listings may be unscored: {unscored:?}"
    );
    use_warmup_sessions(0, 2);
}

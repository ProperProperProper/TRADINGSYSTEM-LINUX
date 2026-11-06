//! Which symbols have data, which can be traded in a backtest (have moomoo rules), and which actually get scored (skipped without data).
use moomoo_bot::engine::{scores, walkforward};
use std::path::PathBuf;
use thomas_core::config::WINDOW_SESSIONS;
use thomas_engine::market::{apply_window, build_on, load_symbol_data};
use thomas_engine::replay::apply_research_settings;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn what_gets_tested() {
    let c = data("candles.sqlite3");
    let s = data("spy_holdings.csv");
    if !c.exists() || !s.exists() || c.metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data not present or empty");
        return;
    }
    let mut l = load_symbol_data(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
    )
    .unwrap();
    apply_window(&mut l, WINDOW_SESSIONS).unwrap();
    apply_research_settings();
    let b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    let m = &b.market;
    let with_rules = (0..m.symbols.len())
        .filter(|&s| m.rules(s).is_some())
        .count();
    let any_score = |sc: &[Vec<Option<scores::Score>>]| {
        sc.iter().filter(|r| r.iter().any(|x| x.is_some())).count()
    };
    let tradeable = scores::compute_with(m, walkforward::UNIVERSE, true);
    let everyone = scores::compute_with(m, walkforward::UNIVERSE, false);
    let ever_universe = |sc: &[Vec<Option<scores::Score>>]| any_score(sc);
    eprintln!(
        "COVERAGE symbols in the market {} | with moomoo trading rules {with_rules} | scored at least once when only tradeable symbols may join: {} | scored at least once when every symbol may join (research only): {}",
        m.symbols.len(),
        ever_universe(&tradeable),
        ever_universe(&everyone)
    );
    let wide = scores::compute_with(m, m.symbols.len(), true);
    eprintln!(
        "COVERAGE scored at least once if the universe were all {} symbols instead of {}: {}",
        m.symbols.len(),
        walkforward::UNIVERSE,
        any_score(&wide)
    );
    eprintln!(
        "COVERAGE universe size per candle (the bot's rule): {}",
        walkforward::UNIVERSE
    );
    let no_rules: Vec<&str> = (0..m.symbols.len())
        .filter(|&s| m.rules(s).is_none())
        .map(|s| m.symbols[s].as_str())
        .take(5)
        .collect();
    eprintln!("COVERAGE examples without rules: {no_rules:?}");
    assert_eq!(m.symbols.len(), 503);
}

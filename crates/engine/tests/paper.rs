//! The local paper trader against the engine's own backtest on real data: stepping candle by candle, with the account written to JSON and read back after every
//! candle, gives exactly the trades and the equity of a backtest of the same period (skipped without data).
use moomoo_bot::engine::walkforward::{self, SearchSpace};
use moomoo_bot::engine::xs;
use std::path::PathBuf;
use thomas_engine::paper::{self, PaperAccount};
use thomas_engine::replay::use_liquidity_model;
use thomas_engine::search::prepare_for;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn stepping_one_closed_candle_at_a_time_reproduces_the_backtest_trade_for_trade() {
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data not present");
        return;
    }
    use_liquidity_model(true);
    let p = prepare_for(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
        moomoo_bot::engine::Interval::H5,
    )
    .unwrap();
    let (m, sc) = (&p.built.market, &p.scores);
    let total = m.ts.len();
    // a real setting: the bot's robust winner of a search on the 14 sessions before the paper period
    let paper_start = total - 10 * p.per_session;
    let space = SearchSpace {
        samples: 3000,
        min_trades: 2,
        ..walkforward::space_for(p.interval)
    };
    let params = paper::search_params(m, sc, paper_start - 1, p.per_session, space, 5)
        .unwrap()
        .expect("a usable setting on the 14 sessions before");

    // the reference: the engine's backtest of the paper period
    let range = paper_start..total;
    let mut reference = xs::backtest(
        &p.built.market,
        sc,
        range.clone(),
        &params,
        moomoo_bot::engine::START_CAPITAL,
    );

    // the paper trader: candle by candle, state through JSON after each candle
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    thomas_core::store::init(&conn).unwrap();
    let mut acc = PaperAccount::new(
        1,
        m.wall[paper_start - 1],
        moomoo_bot::engine::START_CAPITAL,
    );
    acc.params = Some(params);
    paper::save(&conn, &acc, 1).unwrap();
    let mut stepped = 0;
    while let Some(&t) = paper::new_candles(m, acc.last_wall).first() {
        if t >= total {
            break;
        }
        let mut a = paper::load(&conn).unwrap().unwrap();
        stepped += paper::step(&mut a, m, sc, &[t]);
        paper::save(&conn, &a, 2).unwrap();
        acc = a;
    }
    assert_eq!(
        stepped,
        range.len(),
        "every closed candle of the period was stepped once"
    );
    assert_eq!(
        paper::new_candles(m, acc.last_wall).len(),
        0,
        "nothing left to step"
    );

    // the reference closes what is still open at the end of the test; do the same on the paper account
    let last = range.end - 1;
    acc.portfolio
        .close_all(m, m.ts[last] + moomoo_bot::engine::BAR_MS, "End of test");
    reference.close_all(m, m.ts[last] + moomoo_bot::engine::BAR_MS, "End of test"); // already closed: no effect
    assert_eq!(
        acc.portfolio.trades.len(),
        reference.trades.len(),
        "same number of trades"
    );
    for (a, b) in acc.portfolio.trades.iter().zip(&reference.trades) {
        assert_eq!(
            (a.symbol.as_str(), a.entry_ts, a.exit_ts),
            (b.symbol.as_str(), b.entry_ts, b.exit_ts)
        );
        assert!(
            (a.pnl - b.pnl).abs() < 1e-9 && (a.fees - b.fees).abs() < 1e-9,
            "{} pnl {} vs {}",
            a.symbol,
            a.pnl,
            b.pnl
        );
    }
    assert!(
        (acc.portfolio.equity - reference.equity).abs() < 1e-9,
        "equity {} vs {}",
        acc.portfolio.equity,
        reference.equity
    );
}

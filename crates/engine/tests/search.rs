//! The Phase 2 search on the real window: three overlapping periods now, and new data adds more without repeating the old ones (skipped without data).
use std::path::PathBuf;
use thomas_core::config::WINDOW_SESSIONS;
use thomas_engine::market::{Loaded, apply_window, build_on, load_symbol_data};
use thomas_engine::replay::{apply_research_settings, use_liquidity_model, use_warmup_sessions};
use thomas_engine::search::{Settings, compute_scores_all, record, run_periods};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

fn load() -> Loaded {
    load_symbol_data(
        &data("candles.sqlite3"),
        &data("thomas.sqlite3"),
        &data("spy_holdings.csv"),
    )
    .unwrap()
}

/// One run on `l` (windowed to 40 sessions) against the shared store.
fn run(
    conn: &rusqlite::Connection,
    mut l: Loaded,
    samples: usize,
) -> thomas_engine::search::Report {
    apply_window(&mut l, WINDOW_SESSIONS);
    apply_research_settings();
    let b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    let sc = compute_scores_all(&b);
    let first = (0..b.market.ts.len())
        .find(|&t| sc.iter().any(|r| r[t].is_some()))
        .unwrap();
    let cfg = Settings {
        first_scored: first,
        candles_per_session: 2,
        interval: moomoo_bot::engine::Interval::H5,
        equity: moomoo_bot::engine::START_CAPITAL,
        samples,
        min_trades: thomas_core::config::MIN_TRADES_SEARCH,
        seed: 7,
    };
    let rep = run_periods(conn, &b, &sc, &l.calendar, cfg, 1).unwrap();
    record(conn, &b, &rep, cfg, 1).unwrap();
    use_warmup_sessions(0, 2);
    rep
}

fn stored(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT count(*) FROM search_periods", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn new_data_adds_overlapping_periods_and_old_ones_are_not_repeated() {
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data not present");
        return;
    }
    use_liquidity_model(true);
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    thomas_core::store::init(&conn).unwrap();

    // the same market three sessions earlier: the last 3 sessions of every symbol removed
    let full = load();
    let max_wall = full
        .data
        .iter()
        .filter_map(|d| d.trades.keys().next_back().copied())
        .max()
        .unwrap();
    let last = full.calendar.iter().rposition(|s| s.0 <= max_wall).unwrap();
    let cut = full.calendar[last - 2].0;
    let mut earlier = load();
    for d in earlier.data.iter_mut() {
        d.trades.retain(|w, _| *w < cut);
    }
    let a = run(&conn, earlier, 300);
    assert_eq!(
        (a.periods.len(), a.new_periods(), stored(&conn)),
        (3, 3, 3),
        "20 scored sessions hold three overlapping 14-session periods"
    );
    assert!(
        a.periods
            .windows(2)
            .all(|w| w[0].range.end > w[1].range.start),
        "they overlap"
    );
    assert!(
        a.forward.is_empty(),
        "no later non-overlapping period exists yet"
    );

    // three more sessions of data: one more period, the two it shares with before are not searched again
    let b = run(&conn, load(), 300);
    assert_eq!(
        (b.periods.len(), b.new_periods(), stored(&conn)),
        (3, 1, 4),
        "one period added by 3 new sessions"
    );
    assert_eq!(
        b.periods.iter().map(|p| p.new).collect::<Vec<_>>(),
        vec![false, false, true]
    );
    // running again on the same data repeats nothing
    let c = run(&conn, load(), 300);
    assert_eq!((c.new_periods(), stored(&conn)), (0, 4));
    // every winning setting is kept (not only a top 20), and a repeat run keeps the same set
    let winners: i64 = conn
        .query_row("SELECT count(*) FROM winning_settings", [], |r| r.get(0))
        .unwrap();
    assert!(
        winners > 20,
        "more than a top 20 per period is kept: {winners}"
    );
    // every candidate is evaluated on every current period
    assert!(
        c.candidates
            .iter()
            .all(|x| x.per_period.len() == c.periods.len())
    );
}

#[test]
fn searching_the_periods_side_by_side_gives_the_same_result_as_one_after_the_other() {
    use moomoo_bot::engine::walkforward::{self, SearchSpace};
    use std::time::{Duration, Instant};
    if !data("candles.sqlite3").exists() || !data("spy_holdings.csv").exists() || data("candles.sqlite3").metadata().map(|m| m.len()).unwrap_or(0) == 0 {
        eprintln!("skipped: data not present");
        return;
    }
    use_liquidity_model(true);
    let mut l = load();
    apply_window(&mut l, WINDOW_SESSIONS);
    apply_research_settings();
    let b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    let sc = compute_scores_all(&b);
    let first = (0..b.market.ts.len())
        .find(|&t| sc.iter().any(|r| r[t].is_some()))
        .unwrap();
    let cfg = Settings {
        first_scored: first,
        candles_per_session: 2,
        interval: moomoo_bot::engine::Interval::H5,
        equity: moomoo_bot::engine::START_CAPITAL,
        samples: 500,
        min_trades: thomas_core::config::MIN_TRADES_SEARCH,
        seed: 11,
    };
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    thomas_core::store::init(&conn).unwrap();
    let rep = run_periods(&conn, &b, &sc, &l.calendar, cfg, 1).unwrap(); // parallel
    let space = SearchSpace {
        samples: cfg.samples,
        min_trades: cfg.min_trades,
        ..walkforward::space_for(moomoo_bot::engine::Interval::H5)
    };
    assert!(rep.periods.len() >= 3 && rep.periods.iter().all(|p| p.new));
    for (k, p) in rep.periods.iter().enumerate() {
        let m = b.market.slice(p.range.clone());
        let rows: Vec<Vec<_>> = sc.iter().map(|r| r[p.range.clone()].to_vec()).collect();
        let seq = walkforward::search_random(
            &m,
            &rows,
            cfg.equity,
            Instant::now() + Duration::from_secs(600),
            cfg.seed + k as u64,
            &[],
            space,
        )
        .unwrap();
        assert_eq!(p.usable, seq.usable, "period {k}: usable settings differ");
        assert_eq!(p.evaluated, seq.evaluated, "period {k}: draws differ");
        assert_eq!(
            serde_json::to_string(&p.winner).unwrap(),
            serde_json::to_string(&seq.params).unwrap(),
            "period {k}: winner differs"
        );
    }
}

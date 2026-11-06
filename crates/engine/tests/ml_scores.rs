//! The ML score matrix must be causal: changing prices from candle T on cannot change any score before T (skipped without data).
use std::path::PathBuf;
use thomas_core::config::WINDOW_SESSIONS;
use thomas_engine::learner::{self, build_rows, ml_scores};
use thomas_engine::market::{apply_window, build_on, load_symbol_data};
use thomas_engine::replay::{apply_research_settings, use_liquidity_model};

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data")
        .join(name)
}

#[test]
fn ml_scores_never_use_a_future_price() {
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
    let mut b = build_on(&l, moomoo_bot::engine::Interval::H5).unwrap();
    let first = learner::LOOKBACK;
    let ps = learner::periods(&b, &l.calendar, first.max(40), 2);
    let starts: Vec<usize> = ps.iter().map(|p| p.1.start).collect();
    assert!(starts.len() >= 3, "{starts:?}");
    let trainer = learner::mlp_trainer(7, 8, 5, 0.01);
    let rows = build_rows(&b);
    let before = ml_scores(&b, &rows, &starts, &trainer);
    let scored: usize = before.iter().map(|r| r.iter().flatten().count()).sum();
    assert!(scored > 10_000, "only {scored} scores");

    // from candle T on, every price doubles
    let t_cut = *starts.last().unwrap() + 2;
    for row in b.market.bars.iter_mut() {
        for bar in row.iter_mut().skip(t_cut).flatten() {
            bar.open *= 2.0;
            bar.high *= 2.0;
            bar.low *= 2.0;
            bar.close *= 2.0;
        }
    }
    let rows2 = build_rows(&b);
    let after = ml_scores(&b, &rows2, &starts, &trainer);
    for (s, (x, y)) in before.iter().zip(&after).enumerate() {
        for t in 0..t_cut {
            assert_eq!(
                x[t], y[t],
                "symbol {s} candle {t}: a score before the change moved"
            );
        }
    }
    let moved = before
        .iter()
        .zip(&after)
        .any(|(x, y)| (t_cut..x.len()).any(|t| x[t] != y[t]));
    assert!(
        moved,
        "scores after the change should differ (the test would prove nothing otherwise)"
    );
}

use std::collections::BTreeMap;
use thomas_core::candles::{Bar, Candles, validate};

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
    Bar {
        ts_ms: ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
    }
}

fn good(ts: i64) -> Bar {
    bar(ts, 10.0, 11.0, 9.0, 10.0, 100.0)
}

fn one(bars: Vec<Bar>) -> Candles {
    let mut m = BTreeMap::new();
    m.insert("X".to_string(), bars);
    m
}

#[test]
fn valid_symbol_has_no_findings() {
    let r = validate(&one((0..5).map(|i| good(i * 60_000)).collect()));
    let s = &r.per_symbol["X"];
    assert_eq!((r.symbols, r.bars), (1, 5));
    assert!(r.bad_symbols.is_empty());
    assert_eq!(
        (
            s.bad_ohlc,
            s.non_positive_price,
            s.zero_volume,
            s.intraday_gaps
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(s.median_step_ms, 60_000);
}

#[test]
fn high_below_low_is_bad_ohlc_and_marks_the_symbol() {
    let r = validate(&one(vec![
        good(0),
        bar(60_000, 10.0, 8.0, 9.0, 10.0, 100.0),
    ]));
    assert_eq!(r.per_symbol["X"].bad_ohlc, 1);
    assert_eq!(r.bad_symbols, vec!["X".to_string()]);
}

#[test]
fn non_positive_price_is_counted_separately_from_bad_ohlc() {
    let r = validate(&one(vec![
        good(0),
        bar(60_000, 10.0, 11.0, 9.0, 0.0, 100.0),
    ]));
    let s = &r.per_symbol["X"];
    assert_eq!((s.non_positive_price, s.bad_ohlc), (1, 0));
    assert_eq!(r.bad_symbols, vec!["X".to_string()]);
}

#[test]
fn zero_volume_is_counted_but_does_not_mark_the_symbol_bad() {
    let r = validate(&one(vec![
        good(0),
        bar(60_000, 10.0, 11.0, 9.0, 10.0, 0.0),
    ]));
    assert_eq!(r.per_symbol["X"].zero_volume, 1);
    assert!(r.bad_symbols.is_empty());
}

#[test]
fn missing_intraday_bars_are_a_gap() {
    let r = validate(&one(vec![
        good(0),
        good(60_000),
        good(1_800_000),
        good(5_400_000),
    ]));
    assert_eq!(r.per_symbol["X"].intraday_gaps, 1);
}

#[test]
fn overnight_break_over_six_hours_is_not_a_gap() {
    let r = validate(&one(vec![
        good(0),
        good(60_000),
        good(1_800_000),
        good(66_600_000),
    ]));
    assert_eq!(r.per_symbol["X"].intraday_gaps, 0);
}

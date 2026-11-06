//! The parameter search over closed 1m regular-session bars of US stocks and ETFs, plus the
//! strategy grids. Day-long windows are mapped to
//! trading time (one trading day = DAY_BARS) and leverage fixed at 1x.
//!
//! User rule (2026-10-03): ONE window (the latest 14 sessions, BARS bars), never split into in-sample and out-of-sample parts. `search_live` backtests 200,000
//! random combinations over the whole window; usable settings have positive net return,
//! pass the two safety rules (no liquidation, drawdown <= 25%), and have at least `MIN_TRADES` trades. The
//! best by `objective` challenges the settings paper is using (`choose_live`).
//! The honest check without splitting a window is chronological: choose on one
//! 14-day window, trade the next, never-seen one (dashboard chart, research).

use super::governor;
use super::metrics::Metrics;
use super::scores::Score;
use super::{day_scaled, Market, BARS, DAY_BARS, TEST_SESSIONS};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// The top 20 US stocks by trailing dollar volume (the engine screener, causal) among the
/// candidates with complete moomoo rules (lot size, fees, measured book).
pub const UNIVERSE: usize = 20;
/// Measure only the selected top 20. Missing rules reduce the count; do not
/// scan additional candidates beyond the user's cap.
pub const CANDIDATES: usize = UNIVERSE;
/// Safety rule: the same value as the live drawdown stop.
pub const MAX_DRAWDOWN_PCT: f64 = xs::DRAWDOWN_STOP_PCT;

/// Search objective: return % minus half the max drawdown %.
fn objective(m: &Metrics) -> f64 {
    m.return_pct() - 0.5 * m.max_drawdown_pct
}

#[cfg(test)]
pub(crate) mod tests {

    /// Test: interval spaces keep trading time and 1m is the live space.
    #[test]
    fn interval_spaces_keep_trading_time_and_1m_is_the_live_space() {
        use crate::engine::Interval;
        let m15 = space_for(Interval::M1);
        assert_eq!((m15.hold_bars, m15.candle_bars), (LIVE_SPACE.hold_bars, 1));
        assert_eq!(
            space_for(Interval::H1).hold_bars,
            (7, 13),
            "1-2 sessions of 1h candles"
        );
        assert_eq!(space_for(Interval::H4).hold_bars, (2, 3));
        assert_eq!(space_for(Interval::D1).hold_bars, (1, 2), "1-2 sessions");
        assert_eq!(per_candle(day_scaled(48), 26), 1, "never zero");
    }

    use super::*;

    /// Test fixture: a trend-margin band from a gently oscillating reference series.
    pub(crate) fn test_band() -> MarginBand {
        let mut m = crate::engine::scores::tests::market(&[(0..120)
            .map(|i| 100.0 + (i as f64 * 0.3).sin())
            .collect()]);
        m.symbols[0] = crate::engine::REFERENCE.into();
        MarginBand::new(&m).unwrap()
    }

    /// Test: random params vary stay in bounds and are seeded.
    #[test]
    fn random_params_vary_stay_in_bounds_and_are_seeded() {
        let band = test_band();
        let mut a = Rng(7);
        let mut b = Rng(7);
        let xs: Vec<_> = (0..2000).map(|_| random_params(&mut a, &band)).collect();
        let ys: Vec<_> = (0..2000).map(|_| random_params(&mut b, &band)).collect();
        assert_eq!(xs, ys, "same seed, same sample");
        assert_ne!(
            xs,
            (0..2000)
                .map(|_| random_params(&mut Rng(8), &band))
                .collect::<Vec<_>>()
        );
        for p in &xs {
            assert!(
                p.long_only
                    && (day_scaled(96)..=day_scaled(192)).contains(&p.hold)
                    && (1..=5).contains(&p.top)
            );
            assert_eq!(p.regime, Regime::MarketTrend);
            assert!(p.risk.trend_exit && p.risk.short_stop_pct.is_none());
            assert_eq!(p.gross_leverage, 1.0, "no leverage");
            for v in [
                p.stop_pct,
                p.risk.take_profit_pct,
                p.risk.close_stop_pct,
                p.risk.add_pct,
                p.risk.breaker_pct,
            ]
            .into_iter()
            .flatten()
            {
                assert!(
                    (3.0..=50.0).contains(&v) || (5.0..=25.0).contains(&v),
                    "{v}"
                );
            }
        }
        // Real variety, not a small fixed list.
        let holds: std::collections::BTreeSet<_> = xs.iter().map(|p| p.hold).collect();
        assert!(holds.len() > 20);
    }

    /// FNV-1a over the Debug text of `sample_space` draws for three seeds.
    fn space_fingerprint(space: SearchSpace) -> u64 {
        let band = test_band();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for seed in [1u64, 104_729, 900_001] {
            for p in sample_space(seed, 3000, &band, space) {
                // The entry-only research fields (added 2026-10-08) print as None in live draws: leave them out of the text so the
                // fingerprint still proves the live draw sequence is bit-for-bit the one recorded before any research field existed.
                let text = format!("{p:?}").replace(", entry_kind: None, entry_step: None, entry_gate: None, grade_rank: None, grade_trend: None, grade_risk: None, grade_dd: None, loss_pause: None, roll_stop: None, ml_gate: None", "");
                for b in text.bytes() {
                    h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
                }
            }
        }
        h
    }

    /// Test: the ML gate percentile is drawn by the search (10-60, varying) only when the research option is on.
    #[test]
    fn the_ml_gate_percentile_is_searched_and_only_when_enabled() {
        let band = test_band();
        let off = sample_space(7, 400, &band, LIVE_SPACE);
        assert!(
            off.iter().all(|p| p.risk.ml_gate.is_none()),
            "off by default"
        );
        let on = sample_space(
            7,
            400,
            &band,
            SearchSpace {
                gates: Gates {
                    ml_gate: true,
                    ..LIVE_SPACE.gates
                },
                ..LIVE_SPACE
            },
        );
        let qs: Vec<f64> = on.iter().filter_map(|p| p.risk.ml_gate).collect();
        assert_eq!(qs.len(), on.len(), "every draw carries a percentile");
        assert!(
            qs.iter().all(|q| (10.0..=60.0).contains(q))
                && qs.iter().any(|q| *q < 25.0)
                && qs.iter().any(|q| *q > 45.0),
            "searched across the range, not fixed"
        );
    }

    /// Test: live space draws are unchanged by research fields.
    #[test]
    fn live_space_draws_are_unchanged_by_research_fields() {
        // Fingerprint recorded (2026-10-06) with the code BEFORE `SearchSpace::direction` existed:
        // the live draws are bit-for-bit the same sequence.
        assert_eq!(LIVE_SPACE.direction, None);
        // Re-recorded 2026-10-16 when the user fixed the live basket at one position (LIVE_TOP = (1, 1)); nothing else changed.
        assert_eq!(space_fingerprint(LIVE_SPACE), 0xa3ee_be95_84ed_4b5b);
        // And `direction: None` is one plain draw per combination from the same stream.
        let band = test_band();
        let mut rng = Rng(42);
        let plain: Vec<_> = (0..500)
            .map(|_| random_params_v(&mut rng, &band, LIVE_EXIT, LIVE_HOLD, 1))
            .collect();
        assert_eq!(sample_space(42, 500, &band, LIVE_SPACE), plain);
    }

    /// Test: direction filter keeps only that direction.
    #[test]
    fn direction_filter_keeps_only_that_direction() {
        let band = test_band();
        for d in [true, false] {
            let space = SearchSpace {
                direction: Some(d),
                ..LIVE_SPACE
            };
            let ps = sample_space(9, 2000, &band, space);
            assert!(ps
                .iter()
                .all(|p| p.flip == d && p.long_only && p.gross_leverage == 1.0));
            let sigs: std::collections::BTreeSet<_> =
                ps.iter().map(|p| format!("{:?}", p.signal)).collect();
            assert_eq!(sigs.len(), XS_SIGNALS.len(), "every signal still drawn");
        }
    }

    /// Test: margin band takes percentiles of spys own distance above its average.
    #[test]
    fn margin_band_takes_percentiles_of_spys_own_distance_above_its_average() {
        let mut m = crate::engine::scores::tests::market(&[(0..120)
            .map(|i| 100.0 + (i as f64 * 0.3).sin())
            .collect()]);
        m.symbols[0] = crate::engine::REFERENCE.into();
        let band = MarginBand::new(&m).unwrap();
        let (lo, hi) = (band.margin(26, band.pctl.0), band.margin(26, band.pctl.1));
        assert!(lo > 0.0 && hi >= lo && hi < 2.0, "{lo} {hi}");
        assert!(band.margin(26, 100.0) >= hi && band.margin(26, 0.0) < 0.0);
        let mut rng = Rng(5);
        for _ in 0..200 {
            let p = random_params(&mut rng, &band);
            let (b, x) = (p.risk.trend_bars.unwrap(), p.risk.trend_margin_pct.unwrap());
            assert!(
                x >= band.margin(b, band.pctl.0) - 1e-9 && x <= band.margin(b, band.pctl.1) + 1e-9
            );
        }
    }

    /// Test: a tuned entry margin is drawn inside its range while the exit margin keeps the live draw's value.
    #[test]
    fn tuned_entry_margin_keeps_the_exit_margin() {
        let band = test_band();
        let live = sample_space(11, 200, &band, LIVE_SPACE);
        let tuned = sample_space(
            11,
            200,
            &band,
            SearchSpace {
                gates: Gates {
                    margin: Some((0.2, 0.6)),
                    ..Gates::OFF
                },
                ..LIVE_SPACE
            },
        );
        assert!(
            tuned.iter().all(|t| t
                .risk
                .trend_margin_pct
                .is_some_and(|m| (0.2..=0.6).contains(&m))),
            "entry margin from the tuned range"
        );
        assert!(
            tuned.iter().all(|t| t.risk.trend_exit_margin_pct.is_some()),
            "the exit margin is always pinned"
        );
        // the first draw shares its random stream with the live draw (the tuned range consumes one extra number afterwards)
        let (l, t) = (&live[0], &tuned[0]);
        assert_eq!(
            t.risk.trend_exit_margin_pct, l.risk.trend_margin_pct,
            "exit margin = the live draw"
        );
        assert_eq!(
            (
                t.hold,
                t.stop_pct,
                t.risk.take_profit_pct,
                t.risk.close_stop_pct
            ),
            (
                l.hold,
                l.stop_pct,
                l.risk.take_profit_pct,
                l.risk.close_stop_pct
            ),
            "exits untouched"
        );
    }

    /// Test: the DCA research options draw the add-on-a-dip entry exactly as specified (never, always, inside the range).
    #[test]
    fn dca_options_draw_the_add_entry_as_specified() {
        let band = test_band();
        let with = |dca| SearchSpace {
            gates: Gates { dca, ..Gates::OFF },
            ..LIVE_SPACE
        };
        assert!(
            sample_space(7, 300, &band, with(Some((1.0, 5.0, 20.0))))
                .iter()
                .all(|p| p.risk.add_pct.is_none()),
            "dca:off never adds"
        );
        let on = sample_space(7, 300, &band, with(Some((0.0, 2.0, 6.0))));
        assert!(
            on.iter()
                .all(|p| p.risk.add_pct.is_some_and(|a| (2.0..=6.0).contains(&a))),
            "dca:tight always adds at 2-6% against"
        );
    }

    /// Test: perturb keeps structure and limits.
    #[test]
    fn perturb_keeps_structure_and_limits() {
        let band = test_band();
        let mut rng = Rng(3);
        for _ in 0..2000 {
            let p = random_params(&mut rng, &band);
            let q = perturb(&p, &mut rng, &band);
            let floor = band.margin(q.risk.trend_bars.unwrap(), band.pctl.0);
            assert!(
                q.risk.trend_margin_pct.is_some_and(|x| x >= floor - 1e-9),
                "band floor"
            );
            assert_eq!(
                (p.signal, p.flip, p.top, p.risk.vol_scaled, p.regime),
                (q.signal, q.flip, q.top, q.risk.vol_scaled, q.regime)
            );
            assert_eq!(p.stop_pct.is_some(), q.stop_pct.is_some());
            assert_eq!(p.risk.breaker_pct.is_some(), q.risk.breaker_pct.is_some());
            assert!(q.gross_leverage == 1.0 && q.hold >= 1);
            assert!(q.long_only);
        }
    }

    /// Test: percentiles and candidates come from the data.
    #[test]
    fn percentiles_and_candidates_come_from_the_data() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.5), 51.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
        let c = candidates(&v, &[0.1, 0.5, 0.9], 1.0, 1.0, 95.0);
        assert_eq!(c, vec![None, Some(11.0), Some(51.0), Some(90.0)]);
        // Scaled and clamped; duplicates removed.
        let c = candidates(&v, &[0.9, 0.95, 1.0], 1.05, 1.0, 100.0);
        assert_eq!(c.first(), Some(&None));
        assert!(c.iter().flatten().all(|x| (1.0..=100.0).contains(x)));
        assert!(c.windows(2).skip(1).all(|w| w[0].unwrap() < w[1].unwrap()));
    }

    /// Test: best cell prefers a stable block over a lone spike.
    #[test]
    fn best_cell_prefers_a_stable_block_over_a_lone_spike() {
        // (0,0) is a lone 90 spike next to an unusable cell and a bad one; the 3x3 block
        // around (2,2) is uniformly good.
        let g = vec![
            vec![Some(90.0), None, Some(-20.0), Some(-20.0)],
            vec![Some(-30.0), Some(-30.0), Some(-20.0), Some(-20.0)],
            vec![Some(-20.0), Some(30.0), Some(35.0), Some(33.0)],
            vec![Some(-20.0), Some(31.0), Some(34.0), Some(32.0)],
        ];
        let (i, j) = best_cell(&g).unwrap();
        assert!((2..=3).contains(&i) && (1..=3).contains(&j), "{i},{j}");
        assert_eq!(best_cell(&[vec![None, None]]), None);
    }

    /// Test: excursions measure adverse and favourable moves.
    #[test]
    fn excursions_measure_adverse_and_favourable_moves() {
        use crate::engine::{Bar, Side};
        let bar = |o: f64, h: f64, l: f64, c: f64| Bar {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
            turnover: 1.0,
        };
        let m = Market {
            ts: vec![0, 60_000, 1_800_000],
            symbols: vec!["AAA".into()],
            bars: vec![vec![
                Some(bar(100.0, 101.0, 98.0, 99.0)),
                Some(bar(99.0, 108.0, 95.0, 105.0)),
                Some(bar(105.0, 106.0, 104.0, 105.0)),
            ]],
            ..Market::default()
        };
        let t = Trade {
            symbol: "AAA".into(),
            side: Side::Long,
            entry_ts: 0,
            exit_ts: 1_800_000,
            entry: 100.0,
            exit: 105.0,
            qty: 1.0,
            leverage: 1.0,
            fees: 0.0,
            funding: 0.0,
            pnl: 5.0,
            r: 0.0,
            reason: String::new(),
            decided_ts: None,
        };
        let e = excursions(&m, &[t]);
        assert_eq!(e.len(), 1);
        assert!(
            (e[0].0 - 5.0).abs() < 1e-9
                && (e[0].1 - 8.0).abs() < 1e-9
                && (e[0].2 - 5.0).abs() < 1e-9
        );
    }

    /// Test fixture: a window result with the given label, bars, return, drawdown and liquidations.
    fn win(label: &str, bars: usize, ret: f64, dd: f64, liq: usize) -> WindowResult {
        WindowResult {
            label: label.into(),
            bars,
            return_pct: ret,
            max_drawdown_pct: dd,
            trades: 10,
            liquidations: liq,
            profit_factor: 1.5,
            buy_hold_pct: 0.0,
            trade_list: vec![],
        }
    }

    /// Test fixture: a candidate with the given out-of-sample windows and forward result.
    fn cand(oos: Vec<WindowResult>, forward: Option<WindowResult>) -> Candidate {
        Candidate {
            params: sample_params(1, 1, &test_band()).remove(0),
            chosen_bar_ts: 0,
            in_sample_return_pct: 50.0,
            in_sample: win("in-sample", 1344, 50.0, 10.0, 0),
            in_sample_drawdown_pct: 10.0,
            oos,
            forward,
        }
    }

    /// Test: walk forward verdict needs every window and a long enough forward track.
    #[test]
    fn walk_forward_verdict_needs_every_window_and_a_long_enough_forward_track() {
        let good = || vec![win("w1", 1344, 5.0, 10.0, 0), win("w2", 1344, 2.0, 12.0, 0)];
        let ok = verdict(&cand(good(), Some(win("fwd", 300, 1.0, 5.0, 0))));
        assert!(ok.eligible && ok.reasons.is_empty());
        // Forward track too short, even though profitable.
        let short = verdict(&cand(good(), Some(win("fwd", 50, 3.0, 2.0, 0))));
        assert!(!short.eligible && short.reasons[0].contains(&format!("50 of {FORWARD_MIN_BARS}")));
        // No forward track at all.
        assert!(!verdict(&cand(good(), None)).eligible);
        // One out-of-sample window loses, liquidates or breaches drawdown.
        for bad in [
            win("w1", 1344, -1.0, 5.0, 0),
            win("w1", 1344, 9.0, 5.0, 1),
            win("w1", 1344, 9.0, 30.0, 0),
        ] {
            let v = verdict(&cand(
                vec![bad, win("w2", 1344, 2.0, 12.0, 0)],
                Some(win("fwd", 300, 1.0, 5.0, 0)),
            ));
            assert!(!v.eligible && v.reasons.len() == 1, "{:?}", v.reasons);
        }
        // Without supplied out-of-sample windows, a long enough profitable forward track decides.
        assert!(verdict(&cand(vec![], Some(win("fwd", 300, 1.0, 5.0, 0)))).eligible);
        assert!(!verdict(&cand(vec![], None)).eligible);
        // Forward track that loses.
        assert!(!verdict(&cand(good(), Some(win("fwd", 300, -0.5, 5.0, 0)))).eligible);
    }

    /// Test: champion is kept unless the new best scores higher.
    #[test]
    fn champion_is_kept_unless_the_new_best_scores_higher() {
        // Challenger better: switch.
        assert_eq!(
            pick(Some(("new", 20.0)), Some(("old", 1.0))),
            (Some("new"), Choice::NewBest)
        );
        // Better, but only by less than the margin: keep the settings in use.
        assert_eq!(
            pick(Some(("new", 3.0)), Some(("old", 1.0))),
            (Some("old"), Choice::KeptLastBest)
        );
        // Challenger not better (lower or equal): keep the settings in use.
        assert_eq!(
            pick(Some(("new", 1.0)), Some(("old", 2.0))),
            (Some("old"), Choice::KeptLastBest)
        );
        assert_eq!(
            pick(Some(("new", 2.0)), Some(("old", 2.0))),
            (Some("old"), Choice::KeptLastBest)
        );
        // A champion is retained only when its current-window score is usable.
        assert_eq!(
            pick(None, Some(("old", 1.0))),
            (Some("old"), Choice::KeptLastBest)
        );
        // The champion fails the current-window gate: the challenger takes over.
        assert_eq!(
            pick(Some(("new", 1.0)), None),
            (Some("new"), Choice::NewBest)
        );
        // Same settings found again: kept, not "new".
        assert_eq!(
            pick(Some(("same", 3.0)), Some(("same", 3.0))),
            (Some("same"), Choice::KeptLastBest)
        );
        // Nothing passes the safety rules: no settings, paper opens nothing.
        assert_eq!(pick::<&str>(None, None), (None, Choice::NoneSafe));
    }

    #[test]
    fn losing_and_flat_backtests_are_not_usable_even_when_other_rules_pass() {
        let mut m = Metrics {
            start_equity: 1000.0,
            end_equity: 1001.0,
            trades: MIN_TRADES,
            max_drawdown_pct: 1.0,
            ..Metrics::default()
        };
        assert!(usable(&m));
        m.end_equity = 1000.0;
        assert!(!usable(&m), "flat after fees is not a winner");
        m.end_equity = 999.0;
        assert!(!usable(&m), "a loss is not a winner");
        m.end_equity = f64::NAN;
        assert!(!usable(&m), "non-finite returns are not winners");
    }
}

// ---------------------------------------------------------------- grids

use super::metrics::Trade;
use super::xs::{self, Regime, Signal, XsParams};

/// (signal, lookback, flip) for every signal: unflipped buys the lowest values,
/// flipped the highest; `Return` at each of its lookbacks. 18 variants.
fn signal_variants() -> Vec<(Signal, usize, bool)> {
    let ml: &[Signal] = if xs::research::on(&xs::research::ML_SIGNAL) {
        &[Signal::Ml]
    } else {
        &[]
    };
    XS_SIGNALS
        .iter()
        .chain(ml)
        .flat_map(|&s| {
            lookbacks(s)
                .iter()
                .flat_map(move |&lb| [false, true].map(|flip| (s, lb, flip)))
        })
        .collect()
}

/// Every ranking signal (screener scores, return, funding).
pub const XS_SIGNALS: [Signal; 8] = [
    Signal::Return,
    Signal::TrendScore,
    Signal::PriceAction,
    Signal::Volatility,
    Signal::Rsi,
    Signal::Pulse,
    Signal::Funding,
    Signal::CalmDip,
];

/// Lookbacks searched: only `Return` uses one.
fn lookbacks(signal: Signal) -> &'static [usize] {
    if signal == Signal::Return {
        &[16, DAY_BARS]
    } else {
        &[0]
    }
}

/// How this bar's paper settings were chosen (champion/challenger).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Choice {
    /// The search found settings that beat the ones in use (or none were in use).
    NewBest,
    /// Nothing scored higher: the settings already in use are kept.
    KeptLastBest,
    /// Neither the new best nor the settings in use pass the safety rules.
    NoneSafe,
    /// Legacy saved-state value; no longer emitted because unprofitable/unsafe settings cannot trade.
    KeptUnsafe,
}

// NOTE(agents): User design (2026-10-03): paper always trades the best known settings. The
//               search's best (challenger) replaces the settings in use (champion) only if it
//               scores strictly higher on the SAME latest data; otherwise the champion stays.
//               The profitability and safety rules are not optional: settings that lose,
//               liquidate or exceed the drawdown cap over the 14 days are never used,
//               champion included. The
//               walk-forward verdict is reported as information, not a trade gate.
/// Objective points (percent) a challenger must beat the champion by to replace it: small
/// differences between two searches are noise, and switching churns the paper account.
pub const SWITCH_MARGIN: f64 = 5.0;

/// Pick between the challenger and the champion by score; `None` scores are
/// unusable. A tie, or a lead within `SWITCH_MARGIN`, keeps the champion.
pub fn pick<P: Clone + PartialEq>(
    challenger: Option<(P, f64)>,
    champion: Option<(P, f64)>,
) -> (Option<P>, Choice) {
    match (challenger, champion) {
        (Some((c, cs)), Some((k, ks))) if c != k && cs > ks + SWITCH_MARGIN => {
            (Some(c), Choice::NewBest)
        }
        (_, Some((k, _))) => (Some(k), Choice::KeptLastBest),
        (Some((c, _)), None) => (Some(c), Choice::NewBest),
        (None, None) => (None, Choice::NoneSafe),
    }
}

/// Minimum closed trades over the 14 days for settings to be usable.
pub const MIN_TRADES: usize = 8;

// NOTE(agents): User rule (2026-10-03): the live search uses ONE 14-day window, never split into
//               in-sample/out-of-sample parts. Usable = positive net return, the two safety rules
//               (no liquidation, drawdown <= MAX_DRAWDOWN_PCT), at least MIN_TRADES trades and no execution
//               error, all over the full 14 days.
fn usable(r: &Metrics) -> bool {
    usable_with(r, MIN_TRADES)
}

/// `usable` with a trade minimum of `min_trades` (research: `SearchSpace::min_trades`).
fn usable_with(r: &Metrics, min_trades: usize) -> bool {
    r.execution_error.is_none()
        && r.return_pct().is_finite()
        && r.return_pct() > 0.0
        && r.liquidations == 0
        && r.max_drawdown_pct <= MAX_DRAWDOWN_PCT
        && r.trades >= min_trades
}

/// Score of `p` for live use: its objective over the full 14 days if usable
/// there (`usable`). `None` = not usable now.
pub fn live_score(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    equity: f64,
    p: &XsParams,
    range: std::ops::Range<usize>,
) -> Option<f64> {
    let r = xs::backtest(m, scores, range, p, equity).metrics();
    usable(&r).then(|| objective(&r))
}

/// Settings with their robust score.
#[derive(Debug, Clone, Serialize)]
pub struct RankedParams {
    pub params: XsParams,
    pub robust_score: f64,
}

/// How many of a search's best settings are kept on record.
pub const TOP_KEPT: usize = 20;

/// Research switch (default off, so the bot behaves as before): when set, a search also returns every usable setting in `SearchReport::usable_all`.
pub static KEEP_ALL_USABLE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// One full-window parameter search.
#[derive(Debug, Clone, Serialize)]
pub struct SearchReport {
    pub first_bar_ts: i64,
    pub last_bar_ts: i64,
    pub start_equity: f64,
    /// Combinations in the grid, and how many were backtested before the deadline.
    pub combos: usize,
    pub evaluated: usize,
    /// Combinations that passed the safety rules and the minimum trade count.
    pub usable: usize,
    pub elapsed_ms: u128,
    /// The chosen settings, with their own metrics below. For `search_live`: the candidate with
    /// the best mean objective over itself and its random perturbations (`search_random`).
    pub params: Option<XsParams>,
    /// That mean objective, and the single highest objective found in the sample. A large gap
    /// means the raw maximum was a lone spike.
    pub robust_score: Option<f64>,
    pub raw_best_score: Option<f64>,
    /// Seed of the random sample (`search_random`); None for fixed grids.
    pub seed: Option<u64>,
    /// SPY trend margin range (%) the search drew from, for a one-trading-day average
    /// (the MARGIN_PCTL band of SPY's own distance above it over the window).
    pub margin_range: Option<(f64, f64)>,
    /// The data analysis behind the chosen settings' stop and take-profit (`analyse_exits`).
    pub exit_analysis: Option<ExitAnalysis>,
    /// The best settings of this search by robust score (at most `TOP_KEPT`), for the record.
    pub top: Vec<RankedParams>,
    /// Research only (`KEEP_ALL_USABLE`): EVERY usable setting the search drew, best objective first; `robust_score` holds the plain objective here.
    pub usable_all: Vec<RankedParams>,
    /// Recorded settings re-tested as candidates, how many were usable now, and whether the winner
    /// is one of them.
    pub recorded_tested: usize,
    pub recorded_usable: usize,
    pub from_record: bool,
    pub metrics: Option<Metrics>,
}

/// Random combinations drawn per live search (user choice 2026-10-04: not the whole grid).
pub const LIVE_SAMPLES: usize = 200_000;
/// Best raw candidates re-tested under perturbation, and perturbations each.
const FINALISTS: usize = 300;
const PERTURBATIONS: usize = 20;
/// Perturbation size: each numeric value is scaled by a random factor within +/-this.
const PERTURB: f64 = 0.15;

/// Seeded splitmix64 stream of uniform numbers in [0, 1).
struct Rng(u64);

impl Rng {
    /// Next uniform random number in [0, 1) (splitmix64).
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform random number in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
    /// True with probability `p`.
    fn chance(&mut self, p: f64) -> bool {
        self.next() < p
    }
    /// Optional percent: off with probability `p_off`, else uniform in [lo, hi], 0.1 resolution.
    fn pct(&mut self, p_off: f64, lo: f64, hi: f64) -> Option<f64> {
        (!self.chance(p_off)).then(|| (self.range(lo, hi) * 10.0).round() / 10.0)
    }
}

// NOTE(agents): User request (2026-10-04): randomise the combination values and test 200k random
//               combinations per run, not the whole grid. Every numeric value is drawn from a
//               continuous range, not from fixed lists. Long only is unchanged. Leverage is FIXED at
//               1x for this bot (user: "no leverage") and never searched or perturbed.
/// Fixed leverage of every live-search combination (no leverage).
const LEVERAGE: f64 = 1.0;

// NOTE(agents): User decision (2026-10-06, "not btc"): an absolute trend buy-zone
//               margin from 1-3% above BTC's average. Measured on BTC's own last 14 days
//               (2026-10-06, 48-240-bar averages), 1% sat at the 92.3rd and 3% at the 99.9th
//               percentile of BTC's distance above its average: "buy only in the strongest ~8%
//               of moments". US index ETFs rarely get 1% above such an average (SPY never did in
//               the first measured window), so the stock search draws the same PERCENTILE band and
//               takes that percentile of the reference's (SPY, see `REFERENCE`) own distance above
//               its average over the search window. Reference data only.
/// Percentile band of the reference's distance above its average the margin is drawn from.
pub const MARGIN_PCTL: (f64, f64) = (92.3, 99.9);

/// The reference market's (SPY) distance above its own simple average over the search window,
/// sorted, for every averaging window the search can draw.
#[derive(Debug, Clone)]
pub struct MarginBand {
    by_bars: std::collections::BTreeMap<usize, Vec<f64>>,
    /// The percentile band the margin is drawn from.
    pub pctl: (f64, f64),
}

impl MarginBand {
    /// Trend-margin band from the reference's own distance above its average (live percentile band).
    pub fn new(m: &Market) -> Result<MarginBand> {
        Self::with_pctl(m, MARGIN_PCTL)
    }

    /// As `new`, drawing from the percentile band `pctl` (research).
    pub fn with_pctl(m: &Market, pctl: (f64, f64)) -> Result<MarginBand> {
        let s = m
            .symbols
            .iter()
            .position(|x| x == super::REFERENCE)
            .ok_or_else(|| anyhow::anyhow!("{} missing from the market", super::REFERENCE))?;
        let closes: Vec<Option<f64>> = m.bars[s].iter().map(|b| b.map(|b| b.close)).collect();
        let mut by_bars = std::collections::BTreeMap::new();
        for n in 1..=day_scaled(288) {
            let mut d: Vec<f64> = (n.saturating_sub(1)..closes.len())
                .filter_map(|t| {
                    let w: Option<Vec<f64>> = closes[t + 1 - n..=t].iter().copied().collect();
                    let w = w?;
                    let avg = w.iter().sum::<f64>() / n as f64;
                    Some((closes[t]? / avg - 1.0) * 100.0)
                })
                .collect();
            if d.len() >= 2 {
                d.sort_by(f64::total_cmp);
                by_bars.insert(n, d);
            }
        }
        anyhow::ensure!(
            !by_bars.is_empty(),
            "not enough {} history for the margin band",
            super::REFERENCE
        );
        Ok(MarginBand { by_bars, pctl })
    }

    /// The `q` percentile (0-100) of the distance above the `bars`-bar average (the nearest
    /// measured window when `bars` is longer than the history allows).
    pub fn margin(&self, bars: usize, q: f64) -> f64 {
        let d = self
            .by_bars
            .range(..=bars)
            .next_back()
            .or_else(|| self.by_bars.iter().next())
            .map(|x| x.1)
            .expect("band is never empty");
        let pos = (q / 100.0).clamp(0.0, 1.0) * (d.len() - 1) as f64;
        let (i, f) = (pos.floor() as usize, pos.fract());
        let v = d[i] + f * (d[(i + 1).min(d.len() - 1)] - d[i]);
        (v * 100.0).round() / 100.0
    }
}

/// How the trend filter's go-flat test is searched (research switch; production: `LIVE_EXIT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ExitVariant {
    /// Flatten as soon as the reference is below the ENTRY margin.
    Baseline,
    /// The search also draws a lower go-flat margin (0 to the entry margin): hysteresis.
    Hysteresis,
    /// No trend go-flat; stops, take-profit and rebalances close positions.
    NoTrendExit,
    /// The search draws the exit style per combination (one of the three above).
    Searched,
}

// NOTE(agents): Measured 2026-10-06 (examples/variant_study.rs, two runs of 20 searches each on the
//               real 14-day market): with the go-flat test at the ENTRY margin (Baseline), entries
//               at a rare reference spike are flattened 0.5-0.75 h later and the profit is one lucky
//               trade (median return without its best trade: -0.96% / -1.03%). Without the trend
//               go-flat (the entry filter stays on): median +3.37% / +2.60%, worst run -0.53% /
//               +0.37%, still positive without the best trade, holds ~3 h. Letting the search choose
//               the exit style did worse (it sometimes picks the broken instant exit).
/// The parts of the search space a research study may vary (production: `LIVE_SPACE`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SearchSpace {
    pub exit: ExitVariant,
    /// Percentile band of the reference's distance above its average the entry margin is drawn from.
    pub margin_pctl: (f64, f64),
    /// Whether entries need the reference's up trend (live: always on).
    pub trend_filter: bool,
    /// Random combinations drawn (live: 200,000).
    pub samples: usize,
    /// Closed trades a setting needs to be usable (live: 8).
    pub min_trades: usize,
    /// Bars at the start of the market used only to warm the indicators up: the backtests score
    /// the bars after them (live: 0, the whole window is scored).
    pub warmup_bars: usize,
    /// Range (bars) the rebalance hold is drawn from (research; live: `LIVE_HOLD`).
    pub hold_bars: (usize, usize),
    /// Signal direction (research): `Some(true)` keeps only flipped draws (buy the highest values:
    /// momentum/follow), `Some(false)` only unflipped (contrarian), `None` both (live). A filtered
    /// space re-draws until the direction matches; `None` draws exactly once, so the live random
    /// stream is unchanged.
    pub direction: Option<bool>,
    /// 1-minute bars per candle of the searched market (1 = 1m): the trading-time ranges (trend
    /// window, drawdown cooldown) are drawn in candles of this size (`space_for`).
    pub candle_bars: usize,
    /// Range the basket size (`top`) is drawn from (live: `LIVE_TOP`).
    pub top: (usize, usize),
    /// Range (1-minute bars of trading time) the trend filter's averaging window is drawn from
    /// (live: `LIVE_TREND_BARS`).
    pub trend_bars: (usize, usize),
    /// Lowest trend margin (%) a draw may use (research; live: None, the band's value as is).
    pub min_margin: Option<f64>,
    /// Percentile band the trend EXIT margin is drawn from, separately from the entry margin (research; live: None = the exit
    /// uses the entry margin, as before). Lets the entry margin be widened while the exits stay exactly as they were.
    pub exit_pctl: Option<(f64, f64)>,
    /// Optional entry confirmations / trend options switched on (research; live: `Gates::OFF`).
    pub gates: Gates,
    /// Restrict the ranking signal to one family by name, e.g. "CalmDip" (research; live: None = all eight).
    pub sig: Option<&'static str>,
}

/// The live basket-size range.
pub const LIVE_TOP: (usize, usize) = (1, 1);
/// The live trend-window range: 0.5-2.5 trading days of bars.
pub const LIVE_TREND_BARS: (usize, usize) = (day_scaled(48), day_scaled(240));

/// The live rebalance hold range: 1-2 trading days of bars.
pub const LIVE_HOLD: (usize, usize) = (day_scaled(96), day_scaled(192));

/// `bars` 1-minute bars of trading time in candles of `k` bars (rounded, at least one).
pub const fn per_candle(bars: usize, k: usize) -> usize {
    let c = (bars + k / 2) / k;
    if c == 0 {
        1
    } else {
        c
    }
}

// NOTE(agents): Candle intervals (user request 2026-10-06). The search ranges that are TRADING
//               TIME by design (hold 1-2 sessions, trend window 0.5-2.5 sessions, drawdown
//               cooldown) keep that time on longer candles; in raw bar counts a one-session candle
//               would hold 26-52 sessions, longer than its whole test window, so nothing could
//               ever pass the trade minimum. Indicator windows inside the engine count candles,
//               like any chart indicator. 1m: exactly LIVE_SPACE.
/// The live search space on candles of `interval`.
pub const fn space_for(interval: super::Interval) -> SearchSpace {
    let k = interval.bars();
    SearchSpace {
        hold_bars: (per_candle(LIVE_HOLD.0, k), per_candle(LIVE_HOLD.1, k)),
        candle_bars: k,
        ..LIVE_SPACE
    }
}

/// The exit variant the live search uses.
pub const LIVE_EXIT: ExitVariant = ExitVariant::NoTrendExit;

/// Optional entry confirmations and trend options the search can switch ON (research; live: all off). Each one only ADDS a
/// restriction on top of the reference trend test, except the trend type and sampling step, which change how the trend is read.
/// When one is on, the search draws its strength from a range (never a fixed value).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Gates {
    /// Reference money-flow (Chaikin) confirmation.
    pub cmf: bool,
    /// Reference above its volume-weighted average price.
    pub vwap: bool,
    /// Reference on-balance volume rising.
    pub obv: bool,
    /// Share of the stock universe above its own average at least this high.
    pub breadth: bool,
    /// Trend quality: efficiency ratio of the reference at least this high.
    pub er: bool,
    /// Trend strength: ADX of the reference at least this high.
    pub adx: bool,
    /// No entry right after a sharp fall of the reference (crash trigger).
    pub crash: bool,
    /// Per stock: skip falling knives.
    pub knife: bool,
    /// The trend is sampled every few candles (a coarser trend interval).
    pub tstep: bool,
    /// One trend-average type for every draw, or `kind_any` to search over all five.
    pub kind: Option<xs::TrendKind>,
    pub kind_any: bool,
    /// A data-driven per-stock entry gate whose feature, direction, window and threshold the search draws: one feature, or any.
    pub gate: Option<xs::GateFeature>,
    pub gate_any: bool,
    /// Graduated sizing (entries only): rank grading and/or trend grading, strengths drawn by the search.
    pub grade_rank: bool,
    pub grade_trend: bool,
    /// Risk-based sizing and the three-tier drawdown protocol (see `Risk::grade_risk` / `grade_dd`).
    pub grade_risk: bool,
    pub grade_dd: bool,
    /// Loss-streak pause (see `Risk::loss_pause`): entries stop for a drawn number of hours after a drawn streak of losing trades.
    pub loss_pause: bool,
    /// Rolling-loss stop (see `Risk::roll_stop`): entries stop while the closed trades of a drawn window lost more than a drawn percentage.
    pub roll_stop: bool,
    /// Research: machine-learning entry gate whose percentile the search draws (see `Risk::ml_gate`).
    pub ml_gate: bool,
    /// DCA research (user 2026-10-09): how the add-once-on-a-dip entry (`Risk::add_pct`) is drawn: (share of draws with no add,
    /// lowest %, highest % against the entry). None = the live draw (half the draws, 5-20%). Entry side only; exits unchanged.
    pub dca: Option<(f64, f64, f64)>,
    /// Fine-tuning research (user 2026-10-09): the ENTRY trend margin (% SPY must be above its average to enter) is drawn from
    /// this range instead of the live percentile band. The go-flat (exit) margin stays exactly what the live draw gave it
    /// (`trend_exit_margin_pct` is pinned to the original value), so exits are unchanged. None = live.
    pub margin: Option<(f64, f64)>,
}

impl Gates {
    pub const OFF: Gates = Gates {
        cmf: false,
        vwap: false,
        obv: false,
        breadth: false,
        er: false,
        adx: false,
        crash: false,
        knife: false,
        tstep: false,
        kind: None,
        kind_any: false,
        gate: None,
        gate_any: false,
        grade_rank: false,
        grade_trend: false,
        grade_risk: false,
        grade_dd: false,
        loss_pause: false,
        roll_stop: false,
        ml_gate: false,
        dca: None,
        margin: None,
    };
}

/// Switch the space's optional gates on for one draw (draws random values only for the gates that are on, so the live stream is unchanged).
fn apply_gates(p: &mut XsParams, rng: &mut Rng, space: &SearchSpace) {
    let g = space.gates;
    if let Some((off, lo, hi)) = g.dca {
        p.risk.add_pct = rng.pct(off, lo, hi);
    }
    if let Some((lo, hi)) = g.margin {
        // keep the exit's margin exactly as drawn, then draw only the entry margin from the tuned range
        if p.risk.trend_exit_margin_pct.is_none() {
            p.risk.trend_exit_margin_pct = p.risk.trend_margin_pct;
        }
        p.risk.trend_margin_pct = Some((rng.range(lo, hi) * 100.0).round() / 100.0);
    }
    let k = space.candle_bars;
    let per = |n: usize| per_candle(day_scaled(n), k).max(2) as f64;
    if g.cmf {
        p.risk.cmf_bars = Some(rng.range(per(24), per(240)).round() as usize);
        p.risk.cmf_min = Some((rng.range(0.0, 0.2) * 100.0).round() / 100.0);
    }
    if g.vwap {
        p.risk.vwap_bars = Some(rng.range(per(24), per(240)).round() as usize);
    }
    if g.obv {
        p.risk.obv_bars = Some(rng.range(per(24), per(240)).round() as usize);
    }
    if g.breadth {
        p.risk.breadth_min_pct = Some(rng.range(30.0, 70.0).round());
    }
    if g.er {
        p.risk.er_min = Some((rng.range(0.1, 0.5) * 100.0).round() / 100.0);
    }
    if g.adx {
        p.risk.adx_min = Some(rng.range(10.0, 30.0).round());
    }
    if g.crash {
        p.risk.crash_pct = Some((rng.range(1.0, 6.0) * 10.0).round() / 10.0);
        p.risk.crash_bars = Some(rng.range(2.0, 8.0).round() as usize);
    }
    if g.knife {
        p.risk.knife_drop_pct = Some((rng.range(1.0, 4.0) * 10.0).round() / 10.0);
    }
    if g.tstep {
        p.risk.entry_step = Some(rng.range(1.0, 4.0).round() as usize);
    }
    if let Some(kind) = g.kind {
        p.risk.entry_kind = Some(kind);
    } else if g.kind_any {
        use xs::TrendKind::*;
        p.risk.entry_kind =
            Some([Sma, Ema, Hma, Kama, Donchian][((rng.next() * 5.0) as usize).min(4)]);
    }
    if g.grade_rank {
        p.risk.grade_rank = Some((rng.range(0.1, 0.8) * 100.0).round() / 100.0);
    }
    if g.grade_trend {
        p.risk.grade_trend = Some((rng.range(0.1, 1.0) * 100.0).round() / 100.0);
    }
    if g.grade_risk {
        p.risk.grade_risk = Some((rng.range(0.2, 3.0) * 20.0).round() / 20.0);
    }
    if g.grade_dd {
        let t1 = (rng.range(3.0, 8.0) * 2.0).round() / 2.0;
        let t2 = (t1 + rng.range(5.0, 12.0)).round();
        p.risk.grade_dd = Some((t1, t2, (rng.range(0.3, 0.7) * 20.0).round() / 20.0));
    }
    if g.loss_pause {
        p.risk.loss_pause = Some((
            rng.range(2.0, 4.0).round() as usize,
            rng.range(5.0, 40.0).round(),
        ));
    }
    if g.roll_stop {
        p.risk.roll_stop = Some((
            (rng.range(1.5, 5.0) * 2.0).round() / 2.0,
            (rng.range(120.0, 480.0) / 24.0).round() * 24.0,
        ));
    }
    if g.ml_gate {
        p.risk.ml_gate = Some(rng.range(10.0, 60.0).round());
    }
    if g.gate.is_some() || g.gate_any {
        use xs::GateFeature::*;
        let feature = g
            .gate
            .unwrap_or_else(|| [Ret, DistAvg, VolRatio, Atr][((rng.next() * 4.0) as usize).min(3)]);
        let threshold = match feature {
            Ret | DistAvg => (rng.range(-3.0, 3.0) * 100.0).round() / 100.0,
            VolRatio => (rng.range(0.5, 3.0) * 100.0).round() / 100.0,
            Atr => (rng.range(0.3, 3.0) * 100.0).round() / 100.0,
            // the measured range of SPY's slope is about -3% to +4% (10th to 90th percentile)
            RefSlope => (rng.range(-1.0, 4.5) * 100.0).round() / 100.0,
            RefVol => (rng.range(0.9, 2.0) * 100.0).round() / 100.0,
            // in the stock's ATRs: roughly the 10th to 90th percentile of return-minus-SPY, like `Ret`
            RelStrength => (rng.range(-3.0, 3.0) * 100.0).round() / 100.0,
        };
        let bars = rng.range(per(24), per(240)).round() as usize;
        let skip_above = rng.chance(0.5) || feature == RefVol; // only "stand aside when volatility expands" makes sense
        p.risk.entry_gate = Some(xs::EntryGate {
            feature,
            bars,
            skip_above,
            threshold,
        });
    }
}

/// The search space the live search uses.
pub const LIVE_SPACE: SearchSpace = SearchSpace {
    exit: LIVE_EXIT,
    margin_pctl: MARGIN_PCTL,
    trend_filter: true,
    samples: LIVE_SAMPLES,
    min_trades: MIN_TRADES,
    warmup_bars: 0,
    hold_bars: LIVE_HOLD,
    direction: None,
    candle_bars: 1,
    top: LIVE_TOP,
    trend_bars: LIVE_TREND_BARS,
    min_margin: None,
    exit_pctl: None,
    gates: Gates::OFF,
    sig: None,
};

/// One random combination drawn from `space` (the draw `search_random` makes).
fn random_params_space(rng: &mut Rng, band: &MarginBand, space: &SearchSpace) -> XsParams {
    loop {
        let mut p = random_params_s(rng, band, space);
        if !space.trend_filter {
            p.regime = Regime::Off;
            p.risk.trend_exit = false;
        }
        if space.direction.is_none_or(|d| p.flip == d)
            && space.sig.is_none_or(|n| format!("{:?}", p.signal) == n)
        {
            return p;
        }
    }
}

/// One random combination for `v` (see `ExitVariant`).
#[cfg(test)]
fn random_params_v(
    rng: &mut Rng,
    band: &MarginBand,
    v: ExitVariant,
    hold: (usize, usize),
    k: usize,
) -> XsParams {
    random_params_s(
        rng,
        band,
        &SearchSpace {
            exit: v,
            hold_bars: hold,
            candle_bars: k,
            ..LIVE_SPACE
        },
    )
}

/// One random combination of `space`'s ranges and exit variant (see `ExitVariant`).
fn random_params_s(rng: &mut Rng, band: &MarginBand, space: &SearchSpace) -> XsParams {
    let mut p = random_params_h(rng, band, space);
    let v = match space.exit {
        ExitVariant::Searched => [
            ExitVariant::Baseline,
            ExitVariant::Hysteresis,
            ExitVariant::NoTrendExit,
        ][((rng.next() * 3.0) as usize).min(2)],
        other => other,
    };
    match v {
        ExitVariant::Baseline => {}
        ExitVariant::Hysteresis => {
            let entry = p.risk.trend_margin_pct.unwrap_or(0.0);
            p.risk.trend_exit_margin_pct =
                Some((rng.range(0.0, entry.max(0.0)) * 100.0).round() / 100.0);
        }
        ExitVariant::NoTrendExit => p.risk.trend_exit = false,
        ExitVariant::Searched => unreachable!("resolved above"),
    }
    p
}

/// One random long-only combination (`band`: the margin percentile band, see `MarginBand`).
fn random_params(rng: &mut Rng, band: &MarginBand) -> XsParams {
    random_params_h(rng, band, &LIVE_SPACE)
}

/// As `random_params`, drawing from `space`'s hold, basket and trend ranges.
fn random_params_h(rng: &mut Rng, band: &MarginBand, space: &SearchSpace) -> XsParams {
    let (hold, k) = (space.hold_bars, space.candle_bars);
    let variants = signal_variants();
    let (signal, lookback, flip) = variants[(rng.next() * variants.len() as f64) as usize];
    // NOTE(agents): Loss-first search space (user, 2026-10-04: "focus on reducing losses"). From
    //               docs/validation.md (param_effects): short holds, small baskets and a weak/absent
    //               trend margin lose the most, so the ranges start where the losses shrink: hold 24h-48h,
    //               basket 3-5, trend buy-zone margin 1-3% (on SPY: the same percentile band, see MarginBand), and always at least one trailing exit.
    //               These are RANGES the search draws from, not fixed values.
    let trail_on = rng.chance(0.7);
    let trend_bars = rng
        .range(
            per_candle(space.trend_bars.0, k) as f64,
            per_candle(space.trend_bars.1, k) as f64,
        )
        .round() as usize;
    let mut out = XsParams {
        signal,
        flip,
        lookback,
        hold: rng.range(hold.0 as f64, hold.1 as f64).round() as usize,
        top: space.top.0 + (rng.next() * (space.top.1 + 1 - space.top.0) as f64) as usize,
        gross_leverage: LEVERAGE,
        stop_pct: rng.pct(0.25, 3.0, 25.0),
        risk: xs::Risk {
            close_stop_pct: rng.pct(0.5, 5.0, 20.0),
            take_profit_pct: rng.pct(0.25, 3.0, 50.0),
            add_pct: rng.pct(0.5, 5.0, 20.0),
            breaker_pct: rng.pct(0.5, 8.0, 25.0),
            vol_scaled: rng.chance(0.5),
            short_stop_pct: None,
            trail_pct: trail_on.then(|| (rng.range(0.8, 3.0) * 10.0).round() / 10.0),
            breakeven_pct: rng.pct(0.7, 1.0, 6.0),
            resume_pause_bars: (!rng.chance(0.4)).then(|| {
                rng.range(
                    per_candle(day_scaled(8), k) as f64,
                    per_candle(day_scaled(192), k) as f64,
                )
                .round() as usize
            }),
            atr_stop_mult: rng.pct(0.5, 0.5, 6.0),
            atr_tp_mult: rng.pct(0.5, 0.5, 8.0),
            atr_trail_mult: (!trail_on).then(|| (rng.range(0.5, 2.0) * 10.0).round() / 10.0),
            trend_bars: Some(trend_bars),
            trend_margin_pct: Some({
                let m = band.margin(trend_bars, rng.range(band.pctl.0, band.pctl.1));
                space.min_margin.map_or(m, |lo| m.max(lo))
            }),
            trend_exit: true,
            trend_step: None,
            crash_pct: None,
            crash_bars: None,
            cmf_bars: None,
            cmf_min: None,
            vwap_bars: None,
            obv_bars: None,
            breadth_min_pct: None,
            trend_kind: xs::TrendKind::Sma,
            entry_kind: None,
            entry_step: None,
            entry_gate: None,
            grade_rank: None,
            grade_trend: None,
            grade_risk: None,
            grade_dd: None,
            loss_pause: None,
            roll_stop: None,
            ml_gate: None,
            er_min: None,
            adx_min: None,
            trend_exit_margin_pct: space
                .exit_pctl
                .map(|q| band.margin(trend_bars, rng.range(q.0, q.1))),
            knife_drop_pct: None,
            zone_dispersion_pct: None,
            zone_low_volume_ratio: None,
            zone_laggard_pct: None,
            zone_rally_pct: None,
            zone_spy_pullback_pct: None,
            zone_spy_slope_pct: None,
        },
        // User rule (2026-10-04): long only and never long in a down trend, so the SPY trend
        // filter is always on. Shorting / no-filter modes exist in the engine but were tested as
        // losing and are NOT applied (user decision).
        long_only: true,
        regime: Regime::MarketTrend,
    };
    apply_gates(&mut out, rng, space);
    out
}

/// `n` random long-only combinations from the live search space (for research tools).
pub fn sample_params(seed: u64, n: usize, band: &MarginBand) -> Vec<XsParams> {
    let mut rng = Rng(seed);
    (0..n).map(|_| random_params(&mut rng, band)).collect()
}

/// The first `n` random combinations `search_random` draws for `space` with `seed` (the same
/// draw, in the same order; research tools). Does not run the search.
pub fn sample_space(seed: u64, n: usize, band: &MarginBand, space: SearchSpace) -> Vec<XsParams> {
    let mut rng = Rng(seed);
    (0..n)
        .map(|_| random_params_space(&mut rng, band, &space))
        .collect()
}

/// A nearby combination: every numeric value scaled by a random factor within +/-`PERTURB`;
/// signal, direction, on/off switches and which rules are enabled stay as they are.
fn perturb(p: &XsParams, rng: &mut Rng, band: &MarginBand) -> XsParams {
    let mut f = || 1.0 + PERTURB * (2.0 * rng.next() - 1.0);
    let mut q = p.clone();
    q.hold = ((p.hold as f64 * f()).round() as usize).clamp(1, day_scaled(288));
    let mut pct = |v: Option<f64>| v.map(|x| ((x * f()).max(1.0) * 10.0).round() / 10.0);
    q.stop_pct = pct(p.stop_pct);
    q.risk.close_stop_pct = pct(p.risk.close_stop_pct);
    q.risk.take_profit_pct = pct(p.risk.take_profit_pct);
    q.risk.add_pct = pct(p.risk.add_pct);
    q.risk.breaker_pct = pct(p.risk.breaker_pct);
    q.risk.trail_pct = pct(p.risk.trail_pct);
    q.risk.breakeven_pct = pct(p.risk.breakeven_pct);
    let mut mult = |v: Option<f64>| v.map(|x| ((x * f()).max(0.2) * 10.0).round() / 10.0);
    q.risk.atr_stop_mult = mult(p.risk.atr_stop_mult);
    q.risk.atr_tp_mult = mult(p.risk.atr_tp_mult);
    q.risk.atr_trail_mult = mult(p.risk.atr_trail_mult);
    q.risk.trend_bars = p
        .risk
        .trend_bars
        .map(|b| ((b as f64 * f()).round() as usize).clamp(1, day_scaled(288)));
    // A searched go-flat margin (hysteresis) moves with the entry margin and never exceeds it.
    q.risk.trend_exit_margin_pct = p.risk.trend_exit_margin_pct.map(|x| {
        let entry = q
            .risk
            .trend_margin_pct
            .or(p.risk.trend_margin_pct)
            .unwrap_or(x);
        ((x * f()).clamp(0.0, entry) * 100.0).round() / 100.0
    });
    // The margin's floor is the band's lower end for the (perturbed) averaging window.
    let floor = band.margin(q.risk.trend_bars.unwrap_or(xs::REGIME_BARS), band.pctl.0);
    q.risk.trend_margin_pct = p
        .risk
        .trend_margin_pct
        .map(|x| ((x * f()).max(floor) * 100.0).round() / 100.0);
    if let (Some(x), Some(e)) = (q.risk.trend_exit_margin_pct, q.risk.trend_margin_pct) {
        q.risk.trend_exit_margin_pct = Some(x.min(e));
    }
    q.risk.resume_pause_bars = p
        .risk
        .resume_pause_bars
        .map(|b| ((b as f64 * f()).round() as usize).clamp(1, day_scaled(288)));
    q
}

/// What the chosen settings' trades did in the market, and the stop/take-profit it implies.
#[derive(Debug, Clone, Serialize)]
pub struct ExitAnalysis {
    /// Trades of the no-stop, no-take-profit run that the excursions come from.
    pub trades: usize,
    /// Maximum adverse excursion (% below entry) of trades that ended profitable: median / 90th.
    pub winner_mae_p50: f64,
    pub winner_mae_p90: f64,
    /// Maximum favourable excursion (% above entry) of all trades: median / 90th.
    pub mfe_p50: f64,
    pub mfe_p90: f64,
    /// Candidate stops and take-profits derived from those distributions (None = off).
    pub stops: Vec<Option<f64>>,
    pub take_profits: Vec<Option<f64>>,
    /// Chosen pair after 3x3 smoothing over the candidate grid.
    pub stop_pct: Option<f64>,
    pub take_profit_pct: Option<f64>,
    /// Raw objective of every (stop, take-profit) cell, row = stop, column = take-profit;
    /// None = unusable.
    pub grid: Vec<Vec<Option<f64>>>,
}

/// Value at quantile `q` of a sorted list (0 when empty).
fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// (maximum adverse %, maximum favourable %, final return %) of each long trade, from the
/// traded candles between entry and exit. Trades whose symbol or bars are missing are skipped.
fn excursions(m: &Market, trades: &[Trade]) -> Vec<(f64, f64, f64)> {
    let mut out = Vec::new();
    for t in trades {
        let (Some(s), Ok(e), Ok(x)) = (
            m.symbols.iter().position(|n| *n == t.symbol),
            m.ts.binary_search(&t.entry_ts),
            m.ts.binary_search(&t.exit_ts),
        ) else {
            continue;
        };
        if t.entry <= 0.0 || x < e {
            continue;
        }
        let (mut low, mut high) = (f64::INFINITY, f64::NEG_INFINITY);
        for bar in m.bars[s][e..=x].iter().flatten() {
            low = low.min(bar.low);
            high = high.max(bar.high);
        }
        if low.is_finite() && high.is_finite() {
            out.push((
                ((t.entry - low) / t.entry * 100.0).max(0.0),
                ((high - t.entry) / t.entry * 100.0).max(0.0),
                (t.exit - t.entry) / t.entry * 100.0,
            ));
        }
    }
    out
}

/// Distinct, ascending candidate levels (0.1% resolution) from the given percentiles of a sorted
/// sample, scaled by `scale` and clamped to `[lo, hi]`; `None` (off) always first.
fn candidates(sorted: &[f64], qs: &[f64], scale: f64, lo: f64, hi: f64) -> Vec<Option<f64>> {
    let mut v: Vec<f64> = qs
        .iter()
        .map(|&q| ((percentile(sorted, q) * scale).clamp(lo, hi) * 10.0).round() / 10.0)
        .collect();
    v.dedup();
    let mut out = vec![None];
    out.extend(v.into_iter().map(Some));
    out
}

// NOTE(agents): User request (2026-10-04): stops and take-profits must come from analysing the
//               data. For each finalist, a baseline run WITHOUT stop or take-profit gives its real
//               trades; from the traded candles between each entry and exit we measure the maximum
//               adverse excursion (MAE) and maximum favourable excursion (MFE). Candidate stops are
//               just beyond what profitable trades usually endure (winner-MAE percentiles x1.05);
//               candidate take-profits are levels that a given share of trades actually reached
//               (MFE percentiles). Every (stop, take-profit) pair is then backtested in full and
//               the pair is chosen by a 3x3-smoothed objective, so one lucky cell cannot win.
//               The analysis runs whatever the trade count (user, 2026-10-04); fixed ladders are used
//               only when the baseline has no trade at all or no profitable one. Still in-sample.
const STOP_QS: [f64; 6] = [0.5, 0.6, 0.7, 0.8, 0.9, 0.95];
const TP_QS: [f64; 7] = [0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];

/// `p` with data-derived stop and take-profit, and the analysis behind them.
fn analyse_exits(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    equity: f64,
    p: &XsParams,
    range: std::ops::Range<usize>,
    deadline: Instant,
) -> Result<(XsParams, ExitAnalysis)> {
    let gov = governor::global();
    let mut base = p.clone();
    base.stop_pct = None;
    base.risk.take_profit_pct = None;
    let pf = xs::backtest(m, scores, range.clone(), &base, equity);
    let ex = excursions(m, &pf.trades);
    let mut mae_win: Vec<f64> = ex.iter().filter(|e| e.2 > 0.0).map(|e| e.0).collect();
    let mut mfe: Vec<f64> = ex.iter().map(|e| e.1).collect();
    mae_win.sort_by(f64::total_cmp);
    mfe.sort_by(f64::total_cmp);
    let (stops, tps) = if !ex.is_empty() && !mae_win.is_empty() {
        (
            candidates(&mae_win, &STOP_QS, 1.05, 1.0, 40.0),
            candidates(&mfe, &TP_QS, 1.0, 1.0, 100.0),
        )
    } else {
        (
            [None, Some(5.0), Some(10.0), Some(20.0)].to_vec(),
            [None, Some(5.0), Some(10.0), Some(40.0)].to_vec(),
        )
    };
    let mut grid = vec![vec![None; tps.len()]; stops.len()];
    for (i, &stop) in stops.iter().enumerate() {
        for (j, &tp) in tps.iter().enumerate() {
            gov.checkpoint(deadline)?;
            let mut q = p.clone();
            q.stop_pct = stop;
            q.risk.take_profit_pct = tp;
            let r = xs::backtest(m, scores, range.clone(), &q, equity).metrics();
            grid[i][j] = usable(&r).then(|| objective(&r));
        }
    }
    let (si, ti) = best_cell(&grid).unwrap_or_else(|| {
        // Nothing usable: keep the sampled values.
        let pos = |v: &[Option<f64>], x: Option<f64>| v.iter().position(|c| *c == x).unwrap_or(0);
        (pos(&stops, p.stop_pct), pos(&tps, p.risk.take_profit_pct))
    });
    let mut chosen = p.clone();
    if grid
        .get(si)
        .and_then(|r| r.get(ti))
        .is_some_and(|c| c.is_some())
    {
        chosen.stop_pct = stops[si];
        chosen.risk.take_profit_pct = tps[ti];
    }
    let analysis = ExitAnalysis {
        trades: ex.len(),
        winner_mae_p50: percentile(&mae_win, 0.5),
        winner_mae_p90: percentile(&mae_win, 0.9),
        mfe_p50: percentile(&mfe, 0.5),
        mfe_p90: percentile(&mfe, 0.9),
        stop_pct: chosen.stop_pct,
        take_profit_pct: chosen.risk.take_profit_pct,
        stops,
        take_profits: tps,
        grid,
    };
    Ok((chosen, analysis))
}

/// The usable cell with the best 3x3 mean (itself and up to 8 neighbours; an unusable
/// neighbour counts as the worst usable cell). `None` when nothing is usable.
fn best_cell(grid: &[Vec<Option<f64>>]) -> Option<(usize, usize)> {
    let worst = grid
        .iter()
        .flatten()
        .flatten()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let mut best: Option<((usize, usize), f64)> = None;
    for (i, row) in grid.iter().enumerate() {
        for (j, cell) in row.iter().enumerate() {
            let Some(v) = cell else { continue };
            let (mut sum, mut n) = (0.0, 0.0);
            for (a, r) in grid
                .iter()
                .enumerate()
                .take(i + 2)
                .skip(i.saturating_sub(1))
            {
                for (b, c) in r.iter().enumerate().take(j + 2).skip(j.saturating_sub(1)) {
                    sum += if (a, b) == (i, j) {
                        *v
                    } else {
                        c.unwrap_or(worst)
                    };
                    n += 1.0;
                }
            }
            let mean = sum / n;
            if best.is_none_or(|x| mean > x.1) {
                best = Some(((i, j), mean));
            }
        }
    }
    best.map(|b| b.0)
}

/// The live search: `LIVE_SAMPLES` random combinations over the one 14-day window, then the
/// best `FINALISTS` are re-tested under `PERTURBATIONS` random perturbations each and the one
/// with the best MEAN objective wins (an unusable perturbation counts as the worst usable
/// sampled score). A lone spike whose neighbours fail loses to settings that hold up nearby.
/// Still in-sample: this reduces selection noise, it does not prove an edge.
pub fn search_live(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    equity: f64,
    deadline: Instant,
    seed: u64,
    recorded: &[XsParams],
) -> Result<SearchReport> {
    if m.ts.len() != BARS {
        bail!(
            "search needs exactly {BARS} bars ({TEST_SESSIONS} sessions), got {}",
            m.ts.len()
        );
    }
    search_random(m, scores, equity, deadline, seed, recorded, LIVE_SPACE)
}

/// Random search: draws `space.samples` settings, backtests each, ranks usable ones by latest-window P&L (shorter hold on ties); recorded settings are re-tested too.
pub fn search_random(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    equity: f64,
    deadline: Instant,
    seed: u64,
    recorded: &[XsParams],
    space: SearchSpace,
) -> Result<SearchReport> {
    let samples = space.samples;
    let ok = |r: &Metrics| usable_with(r, space.min_trades);
    m.validate()?;
    let range = space.warmup_bars..m.ts.len();
    // NOTE(agents): Calibrated on the searched bars only: warm-up bars (older history allowed
    //               for the 4h / one-session intervals) feed indicators, never the calibration.
    anyhow::ensure!(
        range.len() >= 2,
        "search range of {} bars is too short",
        range.len()
    );
    let band = MarginBand::with_pctl(&m.slice(range.clone()), space.margin_pctl)?;
    let started = Instant::now();
    let gov = governor::global();
    let mut rng = Rng(seed);
    let mut evaluated = 0;
    // (objective, params) of every usable sampled combination.
    let mut found: Vec<(f64, XsParams)> = Vec::new();
    // Re-test the recorded winners first. This keeps proven parameters in the
    // evaluation path before spending the search budget on random candidates.
    let recorded_usable: Vec<XsParams> = recorded
        .iter()
        .filter(|p| p.long_only)
        .filter(|p| ok(&xs::backtest(m, scores, range.clone(), p, equity).metrics()))
        .cloned()
        .collect();
    let recorded_tested = recorded.len();
    let recorded_usable_n = recorded_usable.len();
    for _ in 0..samples {
        gov.checkpoint(deadline)?;
        evaluated += 1;
        let p = random_params_space(&mut rng, &band, &space);
        let r = xs::backtest(m, scores, range.clone(), &p, equity).metrics();
        if ok(&r) {
            found.push((objective(&r), p));
        }
    }
    let usable_n = found.len();
    let mut report = SearchReport {
        first_bar_ts: m.ts[range.start],
        last_bar_ts: m.ts[m.ts.len() - 1],
        start_equity: equity,
        combos: samples,
        evaluated,
        usable: usable_n,
        elapsed_ms: 0,
        params: None,
        robust_score: None,
        raw_best_score: None,
        seed: Some(seed),
        margin_range: Some((
            band.margin(xs::REGIME_BARS, band.pctl.0),
            band.margin(xs::REGIME_BARS, band.pctl.1),
        )),
        exit_analysis: None,
        top: Vec::new(),
        usable_all: Vec::new(),
        recorded_tested: 0,
        recorded_usable: 0,
        from_record: false,
        metrics: None,
    };
    report.recorded_tested = recorded_tested;
    report.recorded_usable = recorded_usable_n;
    if found.is_empty() && recorded_usable.is_empty() {
        report.elapsed_ms = started.elapsed().as_millis();
        return Ok(report);
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0));
    if KEEP_ALL_USABLE.load(std::sync::atomic::Ordering::Relaxed) {
        report.usable_all = found
            .iter()
            .map(|(o, p)| RankedParams {
                params: p.clone(),
                robust_score: *o,
            })
            .collect();
    }
    let worst = found.last().map_or(-100.0, |f| f.0);
    report.raw_best_score = found.first().map(|f| f.0);
    // Robust score of settings `p`: mean of the WORSE half of its own result and its perturbations
    // (a lower-tail mean): lucky settings whose neighbours are mostly bad rank low.
    let robust = |p: &XsParams, rng: &mut Rng| -> Result<f64> {
        let own = xs::backtest(m, scores, range.clone(), p, equity).metrics();
        let mut vals = vec![if ok(&own) { objective(&own) } else { worst }];
        for _ in 0..PERTURBATIONS {
            gov.checkpoint(deadline)?;
            let q = perturb(p, rng, &band);
            let r = xs::backtest(m, scores, range.clone(), &q, equity).metrics();
            vals.push(if ok(&r) { objective(&r) } else { worst });
        }
        vals.sort_by(f64::total_cmp);
        let half = &vals[..vals.len().div_ceil(2)];
        Ok(half.iter().sum::<f64>() / half.len() as f64)
    };
    // (robust score, params, exit analysis, from the record)
    let mut ranked: Vec<(f64, XsParams, Option<ExitAnalysis>, bool)> = Vec::new();
    for (_, sampled) in found.iter().take(FINALISTS) {
        let (p, analysis) = analyse_exits(m, scores, equity, sampled, range.clone(), deadline)?;
        let mean = robust(&p, &mut rng)?;
        ranked.push((mean, p, Some(analysis), false));
    }
    for p in &recorded_usable {
        if ranked.iter().any(|r| &r.1 == p) {
            continue;
        }
        let mean = robust(p, &mut rng)?;
        ranked.push((mean, p.clone(), None, true));
    }
    // The owner-selected winner is the highest latest-window P&L among safe
    // candidates. When P&L is effectively tied, prefer the shorter holding
    // period. Robust score remains displayed as stability evidence, but it
    // must not override the requested P&L ranking.
    ranked.sort_by(|a, b| {
        let pa = objective(&xs::backtest(m, scores, range.clone(), &a.1, equity).metrics());
        let pb = objective(&xs::backtest(m, scores, range.clone(), &b.1, equity).metrics());
        pb.total_cmp(&pa)
            .then_with(|| a.1.hold.cmp(&b.1.hold))
            .then_with(|| b.0.total_cmp(&a.0))
    });
    report.top = ranked
        .iter()
        .take(TOP_KEPT)
        .map(|r| RankedParams {
            params: r.1.clone(),
            robust_score: r.0,
        })
        .collect();
    if let Some((mean, p, analysis, from_record)) = ranked.into_iter().next() {
        report.metrics = Some(xs::backtest(m, scores, range.clone(), &p, equity).metrics());
        report.robust_score = Some(mean);
        report.params = Some(p);
        report.exit_analysis = analysis;
        report.from_record = from_record;
    }
    report.elapsed_ms = started.elapsed().as_millis();
    Ok(report)
}

// NOTE(agents): User request (2026-10-04): once the search has found optimal settings, they get a
//               walk-forward before being finalised for paper trading at a later stage. Nothing here
//               trades. Two parts, both on data the settings were NOT chosen on:
//               (1) out-of-sample windows: research windows that end before the search window
//               starts (each a full 14 days, no split); (2) a forward track: the same settings
//               re-run over the bars that arrived AFTER they were chosen, updated every bar.
//               Verdict rule (fixed in advance): eligible only if every out-of-sample window and the
//               forward track (at least `FORWARD_MIN_BARS` bars, 3 days) are profitable after costs,
//               have no liquidation and stay within the drawdown safety limit.
/// Minimum bars of forward track (3 trading days) before a verdict can pass.
pub const FORWARD_MIN_BARS: usize = day_scaled(288);

/// Result of running fixed settings over one stretch of unseen data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowResult {
    pub label: String,
    pub bars: usize,
    pub return_pct: f64,
    pub max_drawdown_pct: f64,
    pub trades: usize,
    pub liquidations: usize,
    pub profit_factor: f64,
    /// Equal-weight buy-and-hold of the same symbols over the same bars, for context.
    pub buy_hold_pct: f64,
    /// The closed trades behind these numbers (newest last, at most `MAX_TRADE_ROWS`).
    pub trade_list: Vec<TradeRow>,
}

/// Most trades kept per window result (the console shows which token traded and when).
const MAX_TRADE_ROWS: usize = 200;

/// One closed trade, for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRow {
    pub symbol: String,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub entry: f64,
    pub exit: f64,
    pub pnl: f64,
    /// pnl / margin posted, in %.
    pub return_pct: f64,
    pub leverage: f64,
    pub reason: String,
}

/// Settings found by a search, with their walk-forward evidence so far.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub params: XsParams,
    /// Last bar of the window the settings were chosen on (forward track starts after it).
    pub chosen_bar_ts: i64,
    pub in_sample_return_pct: f64,
    /// The full in-sample result (with its trades) the two numbers above come from.
    pub in_sample: WindowResult,
    pub in_sample_drawdown_pct: f64,
    pub oos: Vec<WindowResult>,
    pub forward: Option<WindowResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub eligible: bool,
    pub reasons: Vec<String>,
}

/// Average buy-and-hold return (%) of all symbols over `range`.
fn buy_hold_pct(m: &Market, range: std::ops::Range<usize>) -> f64 {
    let rs: Vec<f64> = m
        .bars
        .iter()
        .filter_map(|row| {
            let slice = row.get(range.clone())?;
            let a = slice.iter().flatten().next()?.close;
            let b = slice.iter().rev().flatten().next()?.close;
            (a > 0.0).then(|| (b / a - 1.0) * 100.0)
        })
        .collect();
    rs.iter().sum::<f64>() / rs.len().max(1) as f64
}

/// Run `p` over `range` of `m` with the production engine and costs.
pub fn evaluate_window(
    label: &str,
    m: &Market,
    scores: &[Vec<Option<Score>>],
    equity: f64,
    p: &XsParams,
    range: std::ops::Range<usize>,
) -> WindowResult {
    let bars = range.len();
    let bh = buy_hold_pct(m, range.clone());
    let pf = xs::backtest(m, scores, range, p, equity);
    let r = pf.metrics();
    let skip = pf.trades.len().saturating_sub(MAX_TRADE_ROWS);
    let trade_list = pf
        .trades
        .iter()
        .skip(skip)
        .map(|t| TradeRow {
            symbol: t.symbol.clone(),
            entry_ts: t.entry_ts,
            exit_ts: t.exit_ts,
            entry: t.entry,
            exit: t.exit,
            pnl: t.pnl,
            return_pct: t.r * 100.0,
            leverage: t.leverage,
            reason: t.reason.clone(),
        })
        .collect();
    WindowResult {
        label: label.into(),
        bars,
        return_pct: r.return_pct(),
        max_drawdown_pct: r.max_drawdown_pct,
        trades: r.trades,
        liquidations: r.liquidations,
        profit_factor: r.profit_factor(),
        buy_hold_pct: bh,
        trade_list,
    }
}

/// Whether a window result passes: positive return, no liquidation, drawdown within the limit.
fn passes(w: &WindowResult) -> bool {
    w.return_pct > 0.0 && w.liquidations == 0 && w.max_drawdown_pct <= MAX_DRAWDOWN_PCT
}

/// The walk-forward verdict for a candidate (rule stated above; never an automatic trade).
pub fn verdict(c: &Candidate) -> Verdict {
    // The candidate verdict uses the forward track (plus any out-of-sample windows supplied);
    // the service's walk-forward test is reported separately.
    let mut reasons = Vec::new();
    for w in &c.oos {
        if !passes(w) {
            reasons.push(format!(
                "{}: {:+.1}% return, {:.1}% drawdown, {} liquidations",
                w.label, w.return_pct, w.max_drawdown_pct, w.liquidations
            ));
        }
    }
    match &c.forward {
        None => reasons.push("forward track has not started (no bars after the choice yet)".into()),
        Some(f) if f.bars < FORWARD_MIN_BARS => reasons.push(format!(
            "forward track has {} of {FORWARD_MIN_BARS} bars ({:+.1}% so far)",
            f.bars, f.return_pct
        )),
        Some(f) if !passes(f) => reasons.push(format!(
            "forward: {:+.1}% return, {:.1}% drawdown, {} liquidations",
            f.return_pct, f.max_drawdown_pct, f.liquidations
        )),
        Some(_) => {}
    }
    Verdict {
        eligible: reasons.is_empty(),
        reasons,
    }
}

/// The settings paper trades this bar: the search's best (`challenger`) or
/// the settings in use (`champion`), each re-scored on the current market.
pub fn choose_live(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    equity: f64,
    challenger: Option<&XsParams>,
    champion: Option<&XsParams>,
    range: std::ops::Range<usize>,
) -> (Option<XsParams>, Choice) {
    let score = |p: Option<&XsParams>| {
        p.and_then(|p| live_score(m, scores, equity, p, range.clone()).map(|s| (p.clone(), s)))
    };
    pick(score(challenger), score(champion))
}

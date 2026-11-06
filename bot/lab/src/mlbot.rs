//! The candle model, inspired by the bot's own backtester (owner 2026-10-13: "the backtest works really well, so it should inspire the model").
//! What the bot's engine does, and what the model now shares with it:
//!   * it only ever ranks the TOP 20 symbols by trailing one-session dollar volume (its screener universe) -> the model is trained and judged on those only;
//!   * its screener scores (`engine/scores.rs`) are the features its backtests rank by -> ported here exactly: multi-window volatility (1,2,4,16,26 bars,
//!     weights 5:4:3:2:1, each scaled by 1/sqrt(window)), price action (distance from EMA 8/21/55, weights 3:2:1, divided by the volatility), volume z-score
//!     against the previous 20 bars, RSI14 (Wilder), the one-session return, and their percentiles inside the universe, rank value, trend score;
//!   * its entries are gated by SPY's trend (close against its moving average) -> SPY trend distances are features.
//!
//! The turnover of a bar is approximated by close x volume (the lab's store has no turnover column).

use crate::candles::Bar;
use crate::mlcandle::Sample;
use std::collections::BTreeMap;

pub const UNIVERSE: usize = 20;
const DAY_BARS: usize = 26;
const WARMUP: usize = 69; // the bot's: max(DAY_BARS, 55) + 14

pub const EXTRA: [&str; 17] = ["bot_vol", "bot_pa", "bot_volz", "bot_rsi", "bot_ret24", "in_top20", "bot_volatility_score", "bot_volume_score", "bot_pa_strength",
    "bot_price_action_score", "bot_rank_value", "bot_trend_score", "bot_return_score", "bot_rank", "spy_dist_ma26", "spy_dist_ma130", "spy_ret26"];

#[derive(Clone, Copy, Default)]
struct Raw { vol: f64, pa: f64, vol_z: f64, rsi: f64, turnover: f64, ret24: f64 }

/// The bot's per-symbol raw features at every bar index (None during warm-up). Only bars up to the index are used.
fn raw_features(bars: &[Bar]) -> Vec<Option<Raw>> {
    const WINDOWS: [usize; 5] = [1, 2, 4, 16, DAY_BARS];
    const WW: [f64; 5] = [5.0, 4.0, 3.0, 2.0, 1.0];
    const EMAS: [usize; 3] = [8, 21, 55];
    const EW: [f64; 3] = [3.0, 2.0, 1.0];
    let n = bars.len();
    let mut out = vec![None; n];
    if n == 0 { return out; }
    let mut emas = [bars[0].close; 3];
    let (mut gain, mut loss) = (0.0, 0.0);
    for t in 1..n {
        let b = bars[t];
        for (i, p) in EMAS.iter().enumerate() { let k = 2.0 / (*p as f64 + 1.0); emas[i] = b.close * k + emas[i] * (1.0 - k); }
        let d = b.close - bars[t - 1].close;
        gain = (gain * 13.0 + d.max(0.0)) / 14.0;
        loss = (loss * 13.0 + (-d).max(0.0)) / 14.0;
        if t + 1 < WARMUP { continue; }
        let mut vol = 0.0;
        for (i, k) in WINDOWS.iter().enumerate() {
            let w = &bars[t + 1 - k..=t];
            let hi = w.iter().map(|x| x.high).fold(f64::MIN, f64::max);
            let lo = w.iter().map(|x| x.low).fold(f64::MAX, f64::min);
            vol += WW[i] * (hi - lo) / bars[t - k].close * 100.0 / (*k as f64).sqrt();
        }
        vol /= WW.iter().sum::<f64>();
        let pa = (0..3).map(|i| EW[i] * (b.close / emas[i] - 1.0) * 100.0).sum::<f64>() / EW.iter().sum::<f64>();
        let pa = if vol > 0.0 { pa / vol } else { 0.0 };
        let prior: Vec<f64> = bars[t - 20..t].iter().map(|x| x.volume).collect();
        let mean = prior.iter().sum::<f64>() / 20.0;
        let sd = (prior.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / 20.0).sqrt();
        let vol_z = if sd > 0.0 { (b.volume - mean) / sd } else { 0.0 };
        let rsi = if loss == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + gain / loss) };
        let turnover = bars[t + 1 - DAY_BARS..=t].iter().map(|x| x.close * x.volume).sum::<f64>();
        out[t] = Some(Raw { vol, pa, vol_z, rsi, turnover, ret24: b.close / bars[t - DAY_BARS].close - 1.0 });
    }
    out
}

/// Percentile (0-100) of `x` within the sorted `all`, as the bot computes it.
fn percentile(sorted: &[f64], x: f64) -> f64 {
    let le = sorted.partition_point(|v| *v <= x);
    if sorted.len() <= 1 { 50.0 } else { (le as f64 - 1.0) / (sorted.len() as f64 - 1.0) * 100.0 }
}

/// The base samples plus the bot-inspired columns. `series` must be the map the samples were built from (same symbol order).
pub fn extend(series: &BTreeMap<String, Vec<Bar>>, base: &[Sample]) -> Vec<Sample> {
    let raws: Vec<Vec<Option<Raw>>> = series.values().map(|b| raw_features(b)).collect();
    let index: Vec<BTreeMap<i64, usize>> = series.values().map(|b| b.iter().enumerate().map(|(i, x)| (x.ts, i)).collect()).collect();
    let spy = series.iter().position(|(k, _)| k == "SPY");
    let spy_feat = |ts: i64| -> [f64; 3] {
        let Some(si) = spy else { return [0.0; 3] };
        let bars: &Vec<Bar> = series.values().nth(si).expect("spy");
        let Some(&i) = index[si].get(&ts) else { return [0.0; 3] };
        let c = bars[i].close;
        let ma = |n: usize| if i + 1 >= n { bars[i + 1 - n..=i].iter().map(|b| b.close).sum::<f64>() / n as f64 } else { c };
        [c / ma(26) - 1.0, c / ma(130) - 1.0, if i >= 26 { c / bars[i - 26].close - 1.0 } else { 0.0 }]
    };
    let mut out = Vec::with_capacity(base.len());
    let mut i = 0;
    while i < base.len() {
        let mut j = i; while j < base.len() && base[j].ts == base[i].ts { j += 1; }
        let ts = base[i].ts;
        // the universe at this bar: all symbols with raw features, the 20 with the largest one-session turnover (the bot ranks only these)
        let mut members: Vec<(usize, Raw)> = (0..series.len()).filter_map(|s| index[s].get(&ts).and_then(|&k| raws[s][k]).map(|r| (s, r))).collect();
        members.retain(|(s, _)| Some(*s) != spy);
        members.sort_by(|a, b| b.1.turnover.total_cmp(&a.1.turnover));
        members.truncate(UNIVERSE);
        let sorted = |f: &dyn Fn(&Raw) -> f64| { let mut v: Vec<f64> = members.iter().map(|(_, r)| f(r)).collect(); v.sort_by(f64::total_cmp); v };
        let (vols, zs, strengths, pas, rets) = (sorted(&|r| r.vol), sorted(&|r| r.vol_z), sorted(&|r| r.pa.abs()), sorted(&|r| r.pa), sorted(&|r| r.ret24));
        let mut scored: Vec<(usize, [f64; 8])> = members.iter().map(|(s, r)| {
            let (vs, zs_, ps) = (percentile(&vols, r.vol), percentile(&zs, r.vol_z), percentile(&strengths, r.pa.abs()));
            let rv = (vs + zs_ + ps) / 3.0;
            let pas_ = percentile(&pas, r.pa);
            (*s, [vs, zs_, ps, pas_, rv, (50.0 + (pas_ - 50.0) * rv / 100.0).clamp(1.0, 100.0), percentile(&rets, r.ret24), 0.0])
        }).collect();
        scored.sort_by(|a, b| b.1[4].total_cmp(&a.1[4]));
        let nsc = scored.len().max(2) as f64;
        for (pos, (_, v)) in scored.iter_mut().enumerate() { v[7] = pos as f64 / (nsc - 1.0); }
        let by_sym: BTreeMap<usize, [f64; 8]> = scored.into_iter().collect();
        let sf = spy_feat(ts);
        for s in &base[i..j] {
            let mut x = s.x.clone();
            let r = index[s.sym].get(&ts).and_then(|&k| raws[s.sym][k]).unwrap_or_default();
            let sc = by_sym.get(&s.sym);
            x.extend([r.vol, r.pa, r.vol_z, r.rsi, r.ret24, if sc.is_some() { 1.0 } else { 0.0 }]);
            let v = sc.copied().unwrap_or([50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 50.0, 0.5]);
            x.extend([v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]]);
            x.extend(sf);
            out.push(Sample { x, ..s.clone() });
        }
        i = j;
    }
    out
}

/// Market-regime interactions (the best model of `examples/candle_models.rs`, 2026-10-13): the stock's own signals scaled by SPY's trend. Samples without the
/// bot columns (tests) pass through unchanged.
pub fn interactions(s: &[Sample]) -> Vec<Sample> {
    let nb = crate::mlcandle::FEATURES.len();
    let find = |n: &str| crate::mlcandle::FEATURES.iter().position(|f| *f == n).expect("feature");
    let (j_ret26, j_vol) = (find("ret_26"), find("vol_26"));
    let (j_rsi, j_spy) = (nb + 3, nb + 14);
    s.iter().map(|x| {
        if x.x.len() < nb + EXTRA.len() { return x.clone(); }
        let mut v = x.x.clone();
        let spy = x.x[j_spy];
        v.extend([x.x[j_ret26] * spy, x.x[j_vol] * spy, (x.x[j_rsi] - 50.0) / 50.0 * spy, x.x[j_ret26].abs() * x.x[j_vol]]);
        Sample { x: v, ..x.clone() }
    }).collect()
}

/// Only the samples whose symbol is in the bot's top-20 universe at that bar (the only ones the bot's engine ranks).
pub fn in_universe(s: &[Sample], base_features: usize) -> Vec<Sample> { s.iter().filter(|x| x.x[base_features + 5] > 0.5).cloned().collect() }

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(n: usize, f: impl Fn(usize) -> f64, vol: impl Fn(usize) -> f64) -> Vec<Bar> {
        (0..n).map(|i| { let c = f(i); Bar { ts: 1_775_000_000_000 + i as i64 * 900_000, open: c, high: c * 1.002, low: c * 0.998, close: c, volume: vol(i) } }).collect()
    }

    #[test]
    fn raw_features_are_causal_and_start_after_the_bots_warm_up() {
        let a = bars(300, |i| 100.0 + (i as f64 * 0.2).sin() * 3.0, |i| 1000.0 + (i % 7) as f64 * 50.0);
        let mut b = a.clone();
        for x in b.iter_mut().skip(250) { x.close *= 2.0; x.high *= 2.0; x.low *= 2.0; x.volume *= 5.0; }
        let (ra, rb) = (raw_features(&a), raw_features(&b));
        assert!(ra[..WARMUP - 1].iter().all(Option::is_none) && ra[WARMUP - 1].is_some());
        for t in 0..250 { assert_eq!(ra[t].map(|r| (r.vol, r.pa, r.vol_z, r.rsi, r.ret24)), rb[t].map(|r| (r.vol, r.pa, r.vol_z, r.rsi, r.ret24)), "bar {t} changed when only later bars changed"); }
    }

    #[test]
    fn rsi_is_100_on_a_pure_rise_and_low_on_a_fall_and_percentiles_match_the_bots_definition() {
        let up = raw_features(&bars(120, |i| 100.0 + i as f64, |_| 1000.0));
        assert!((up[100].unwrap().rsi - 100.0).abs() < 1e-9);
        let down = raw_features(&bars(120, |i| 300.0 - i as f64, |_| 1000.0));
        assert!(down[100].unwrap().rsi < 1.0);
        let s = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!((percentile(&s, 1.0), percentile(&s, 3.0), percentile(&s, 5.0)), (0.0, 50.0, 100.0));
        assert_eq!(percentile(&[7.0], 7.0), 50.0);
    }

    #[test]
    fn only_the_twenty_most_liquid_names_are_in_the_universe_and_features_are_appended() {
        use std::collections::BTreeMap;
        let mut m: BTreeMap<String, Vec<Bar>> = BTreeMap::new();
        for s in 0..30 { m.insert(format!("S{s:02}"), bars(400, |i| 50.0 + s as f64 + (i as f64 * 0.1 + s as f64).sin(), |_| 1000.0 * (s + 1) as f64)); }
        m.insert("SPY".into(), bars(400, |i| 400.0 + i as f64 * 0.1, |_| 1e6));
        let base = crate::mlcandle::build(&m, 4);
        assert!(!base.is_empty());
        let ext = extend(&m, &base);
        assert_eq!(ext.len(), base.len());
        let nb = crate::mlcandle::FEATURES.len();
        assert!(ext.iter().all(|s| s.x.len() == nb + EXTRA.len() && s.x.iter().all(|v| v.is_finite())));
        let one_moment: Vec<&Sample> = ext.iter().filter(|s| s.ts == ext[ext.len() / 2].ts).collect();
        assert_eq!(one_moment.iter().filter(|s| s.x[nb + 5] > 0.5).count(), UNIVERSE, "exactly the top 20");
        // the most liquid names (largest volume x price) are the members: S10..S29
        assert!(one_moment.iter().filter(|s| s.x[nb + 5] > 0.5).all(|s| s.sym >= 10 && s.sym < 30), "members are the 20 most liquid");
        // SPY (the reference) is never a ranked stock
        assert!(ext.iter().all(|s| s.sym != 30));
        assert!(in_universe(&ext, nb).iter().all(|s| s.x[nb + 5] > 0.5));
    }
}

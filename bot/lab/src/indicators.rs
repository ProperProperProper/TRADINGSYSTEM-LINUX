//! Technical indicators and candle-pattern features, computed in-house from the lab's own 15-minute bars (owner 2026-10-13: "analyse the candles itself, indicators
//! itself"). Every value at bar `i` uses bars up to and including `i` only (a test rewrites the future and checks nothing earlier moves); every value is scale-free
//! (ratios, fractions, bounded oscillators), so one network can read all stocks. `compute` returns one row of `N` values per bar; warm-up values are 0.

use crate::candles::Bar;
use crate::mlcandle::et_offset_ms;

pub const NAMES: [&str; 34] = [
    "rsi14", "macd_line", "macd_hist", "boll_pctb", "boll_bandwidth", "atr14_pct", "stoch_k14", "stoch_d", "cci20", "adx14", "plus_di_minus_di", "obv_slope20", "mfi14", "cmf20",
    "vwap_dist", "donchian_pos20", "roc5", "roc26", "aroon_osc25", "ema8_21", "ema21_55", "keltner_pos", "body_frac", "upper_wick", "lower_wick", "engulfing", "doji", "inside_bar",
    "gap_prev_close", "volume_z20", "range_expansion", "tod_sin", "tod_cos", "up_streak",
];
pub const N: usize = NAMES.len();

fn ema(prev: f64, x: f64, n: usize) -> f64 { let k = 2.0 / (n as f64 + 1.0); x * k + prev * (1.0 - k) }

pub fn compute(bars: &[Bar]) -> Vec<[f64; N]> {
    let n = bars.len();
    let mut out = vec![[0.0; N]; n];
    if n < 2 { return out; }
    // running state
    let (mut e8, mut e12, mut e21, mut e26, mut e55) = (bars[0].close, bars[0].close, bars[0].close, bars[0].close, bars[0].close);
    let (mut macd_sig, mut obv) = (0.0f64, 0.0f64);
    let (mut avg_gain, mut avg_loss, mut atr, mut sm_tr, mut sm_pdm, mut sm_mdm, mut adx) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let mut obv_hist = vec![0.0; n];
    let (mut sess, mut cum_pv, mut cum_v) = (i64::MIN, 0.0f64, 0.0f64);
    let mut k_hist = vec![0.5; n];
    let mut streak = 0.0f64;
    for i in 0..n {
        let b = bars[i];
        let prev = if i > 0 { bars[i - 1] } else { b };
        // exponential averages
        e8 = ema(e8, b.close, 8); e12 = ema(e12, b.close, 12); e21 = ema(e21, b.close, 21); e26 = ema(e26, b.close, 26); e55 = ema(e55, b.close, 55);
        let macd = e12 - e26; macd_sig = ema(macd_sig, macd, 9);
        // Wilder smoothing: RSI, ATR, ADX
        let d = b.close - prev.close;
        avg_gain = (avg_gain * 13.0 + d.max(0.0)) / 14.0; avg_loss = (avg_loss * 13.0 + (-d).max(0.0)) / 14.0;
        let tr = (b.high - b.low).max((b.high - prev.close).abs()).max((b.low - prev.close).abs());
        atr = (atr * 13.0 + tr) / 14.0;
        let (up, dn) = (b.high - prev.high, prev.low - b.low);
        let (pdm, mdm) = (if up > dn && up > 0.0 { up } else { 0.0 }, if dn > up && dn > 0.0 { dn } else { 0.0 });
        sm_tr = (sm_tr * 13.0 + tr) / 14.0; sm_pdm = (sm_pdm * 13.0 + pdm) / 14.0; sm_mdm = (sm_mdm * 13.0 + mdm) / 14.0;
        let (pdi, mdi) = if sm_tr > 0.0 { (100.0 * sm_pdm / sm_tr, 100.0 * sm_mdm / sm_tr) } else { (0.0, 0.0) };
        let dx = if pdi + mdi > 0.0 { 100.0 * (pdi - mdi).abs() / (pdi + mdi) } else { 0.0 };
        adx = (adx * 13.0 + dx) / 14.0;
        obv += (b.close - prev.close).signum() * b.volume * if i > 0 { 1.0 } else { 0.0 };
        obv_hist[i] = obv;
        // session VWAP (resets each New York day)
        let s = (b.ts + et_offset_ms(b.ts)).div_euclid(86_400_000);
        if s != sess { sess = s; cum_pv = 0.0; cum_v = 0.0; }
        let tp = (b.high + b.low + b.close) / 3.0;
        cum_pv += tp * b.volume; cum_v += b.volume;
        streak = if d > 0.0 { streak.max(0.0) + 1.0 } else if d < 0.0 { streak.min(0.0) - 1.0 } else { 0.0 };
        let r = &mut out[i];
        if i < 55 { // warm-up: the exponential and Wilder averages have not settled
            r[29] = 0.0;
            continue;
        }
        let win = |k: usize| &bars[i + 1 - k..=i];
        // oscillators
        r[0] = if avg_loss == 0.0 { 0.5 } else { (100.0 - 100.0 / (1.0 + avg_gain / avg_loss)) / 100.0 - 0.5 };
        r[1] = macd / b.close * 100.0;
        r[2] = (macd - macd_sig) / b.close * 100.0;
        let w20 = win(20);
        let sma = w20.iter().map(|x| x.close).sum::<f64>() / 20.0;
        let sd = (w20.iter().map(|x| (x.close - sma).powi(2)).sum::<f64>() / 20.0).sqrt();
        r[3] = if sd > 0.0 { ((b.close - sma) / (2.0 * sd)).clamp(-2.0, 2.0) } else { 0.0 };
        r[4] = 4.0 * sd / sma;
        r[5] = atr / b.close * 100.0;
        let (hh14, ll14) = win(14).iter().fold((f64::MIN, f64::MAX), |a, x| (a.0.max(x.high), a.1.min(x.low)));
        let k = if hh14 > ll14 { (b.close - ll14) / (hh14 - ll14) } else { 0.5 };
        k_hist[i] = k;
        r[6] = k - 0.5;
        r[7] = (k_hist[i] + k_hist[i - 1] + k_hist[i - 2]) / 3.0 - 0.5;
        let tps: Vec<f64> = w20.iter().map(|x| (x.high + x.low + x.close) / 3.0).collect();
        let tp_m = tps.iter().sum::<f64>() / 20.0;
        let mad = tps.iter().map(|x| (x - tp_m).abs()).sum::<f64>() / 20.0;
        r[8] = if mad > 0.0 { ((tp - tp_m) / (0.015 * mad) / 100.0).clamp(-3.0, 3.0) } else { 0.0 };
        r[9] = adx / 100.0;
        r[10] = (pdi - mdi) / 100.0;
        let vol20: f64 = w20.iter().map(|x| x.volume).sum();
        r[11] = if vol20 > 0.0 { (obv_hist[i] - obv_hist[i - 20]) / vol20 } else { 0.0 };
        // money flow index
        let (mut pos, mut neg) = (0.0, 0.0);
        for j in i + 1 - 14..=i { let t = (bars[j].high + bars[j].low + bars[j].close) / 3.0; let tp_prev = (bars[j - 1].high + bars[j - 1].low + bars[j - 1].close) / 3.0; let flow = t * bars[j].volume; if t > tp_prev { pos += flow; } else if t < tp_prev { neg += flow; } }
        r[12] = if pos + neg > 0.0 { pos / (pos + neg) - 0.5 } else { 0.0 };
        // Chaikin money flow
        let (mut mf, mut vv) = (0.0, 0.0);
        for x in w20 { let rng = x.high - x.low; if rng > 0.0 { mf += ((x.close - x.low) - (x.high - x.close)) / rng * x.volume; } vv += x.volume; }
        r[13] = if vv > 0.0 { mf / vv } else { 0.0 };
        r[14] = if cum_v > 0.0 { (b.close / (cum_pv / cum_v) - 1.0) * 100.0 } else { 0.0 };
        let (hh20, ll20) = w20.iter().fold((f64::MIN, f64::MAX), |a, x| (a.0.max(x.high), a.1.min(x.low)));
        r[15] = if hh20 > ll20 { (b.close - ll20) / (hh20 - ll20) - 0.5 } else { 0.0 };
        r[16] = (b.close / bars[i - 5].close).ln() * 100.0;
        r[17] = (b.close / bars[i - 26].close).ln() * 100.0;
        let w26 = win(26);
        let (hi_at, lo_at) = (w26.iter().enumerate().max_by(|a, c| a.1.high.total_cmp(&c.1.high)).map(|x| x.0).unwrap_or(0), w26.iter().enumerate().min_by(|a, c| a.1.low.total_cmp(&c.1.low)).map(|x| x.0).unwrap_or(0));
        r[18] = (hi_at as f64 - lo_at as f64) / 25.0;
        r[19] = (e8 / e21).ln() * 100.0;
        r[20] = (e21 / e55).ln() * 100.0;
        r[21] = if atr > 0.0 { ((b.close - e21) / (2.0 * atr)).clamp(-4.0, 4.0) } else { 0.0 };
        // the candle itself
        let rng = b.high - b.low;
        if rng > 0.0 {
            r[22] = (b.close - b.open) / rng;
            r[23] = (b.high - b.open.max(b.close)) / rng;
            r[24] = (b.open.min(b.close) - b.low) / rng;
            r[26] = if (b.close - b.open).abs() / rng < 0.1 { 1.0 } else { 0.0 };
        }
        let (pb_hi, pb_lo) = (prev.open.max(prev.close), prev.open.min(prev.close));
        let (b_hi, b_lo) = (b.open.max(b.close), b.open.min(b.close));
        r[25] = if b.close > b.open && prev.close < prev.open && b_hi >= pb_hi && b_lo <= pb_lo { 1.0 } else if b.close < b.open && prev.close > prev.open && b_hi >= pb_hi && b_lo <= pb_lo { -1.0 } else { 0.0 };
        r[27] = if b.high < prev.high && b.low > prev.low { 1.0 } else { 0.0 };
        r[28] = (b.open / prev.close - 1.0) * 100.0;
        let vm = w20.iter().map(|x| x.volume).sum::<f64>() / 20.0;
        let vs = (w20.iter().map(|x| (x.volume - vm).powi(2)).sum::<f64>() / 20.0).sqrt();
        r[29] = if vs > 0.0 { ((b.volume - vm) / vs).clamp(-5.0, 5.0) } else { 0.0 };
        let mean_rng = w20.iter().map(|x| x.high - x.low).sum::<f64>() / 20.0;
        r[30] = if mean_rng > 0.0 { (rng / mean_rng).min(6.0) } else { 1.0 };
        // time of day: a smooth clock for the 26 bars of the session
        let local = b.ts + et_offset_ms(b.ts);
        let minutes = (local.rem_euclid(86_400_000) / 60_000) as f64 - 570.0; // minutes since 09:30
        let ang = (minutes / 390.0).clamp(0.0, 1.0) * std::f64::consts::PI;
        r[31] = ang.sin(); r[32] = ang.cos();
        r[33] = streak.clamp(-8.0, 8.0) / 8.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    fn bars_from(closes: &[f64], vol: f64) -> Vec<Bar> {
        let t0 = 1_775_000_000_000i64 / DAY * DAY + 13 * 3_600_000 + 30 * 60_000;
        let mut prev = closes[0];
        closes.iter().enumerate().map(|(i, c)| { let o = prev; prev = *c; Bar { ts: t0 + (i as i64 / 26) * DAY + (i as i64 % 26) * 900_000, open: o, high: o.max(*c) * 1.001, low: o.min(*c) * 0.999, close: *c, volume: vol } }).collect()
    }
    fn idx(name: &str) -> usize { NAMES.iter().position(|n| *n == name).unwrap() }

    #[test]
    fn a_steady_rise_gives_the_textbook_readings() {
        let closes: Vec<f64> = (0..200).map(|i| 100.0 * 1.001f64.powi(i)).collect();
        let r = compute(&bars_from(&closes, 1000.0));
        let last = r[199];
        assert!((last[idx("rsi14")] - 0.5).abs() < 1e-9, "RSI of a pure rise is 100 (+0.5 after centring): {}", last[idx("rsi14")]);
        assert!(last[idx("macd_line")] > 0.0 && last[idx("ema8_21")] > 0.0 && last[idx("ema21_55")] > 0.0, "an uptrend: fast averages above slow");
        assert!(last[idx("plus_di_minus_di")] > 0.0 && last[idx("adx14")] > 0.2, "directional strength");
        assert!(last[idx("stoch_k14")] > 0.3, "closing near the top of the 14-bar range");
        assert!(last[idx("obv_slope20")] > 0.9, "every bar closed up: OBV rose by almost the whole 20-bar volume: {}", last[idx("obv_slope20")]);
        assert!(last[idx("donchian_pos20")] > 0.3 && last[idx("aroon_osc25")] > 0.5);
        assert!(r[10].iter().all(|v| *v == 0.0), "warm-up rows are zero");
    }

    #[test]
    fn a_fall_gives_the_opposite_readings_and_candle_patterns_are_recognised() {
        let closes: Vec<f64> = (0..200).map(|i| 100.0 * 0.999f64.powi(i)).collect();
        let r = compute(&bars_from(&closes, 1000.0));
        assert!((r[199][idx("rsi14")] + 0.5).abs() < 1e-9 && r[199][idx("macd_line")] < 0.0 && r[199][idx("obv_slope20")] < -0.9);
        // a bullish engulfing bar after a bearish one, and an inside bar
        let mut b = bars_from(&vec![100.0; 80], 1000.0);
        b[78] = Bar { open: 101.0, high: 101.5, low: 99.5, close: 100.0, ..b[78] }; // bearish
        b[79] = Bar { open: 99.8, high: 101.8, low: 99.7, close: 101.6, ..b[79] };  // bullish, engulfs the body of bar 78
        let r2 = compute(&b);
        assert_eq!(r2[79][idx("engulfing")], 1.0);
        assert!(r2[79][idx("body_frac")] > 0.8 && r2[79][idx("lower_wick")] < 0.1);
        let mut c = bars_from(&vec![100.0; 80], 1000.0);
        c[78] = Bar { high: 102.0, low: 98.0, ..c[78] }; c[79] = Bar { open: 100.1, high: 101.0, low: 99.0, close: 100.2, ..c[79] };
        assert_eq!(compute(&c)[79][idx("inside_bar")], 1.0);
    }

    #[test]
    fn every_value_is_bounded_finite_and_never_looks_ahead() {
        let mut s = 9u64; let mut rnd = move || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let mut p = 100.0; let closes: Vec<f64> = (0..400).map(|_| { p *= 1.0 + rnd() * 0.01; p }).collect();
        let a = bars_from(&closes, 1000.0);
        let ra = compute(&a);
        assert!(ra.iter().all(|row| row.iter().all(|v| v.is_finite())));
        let mut b = a.clone();
        for x in b.iter_mut().skip(300) { x.close *= 3.0; x.high *= 3.0; x.low *= 3.0; x.open *= 3.0; x.volume *= 9.0; }
        let rb = compute(&b);
        for i in 0..300 { assert_eq!(ra[i], rb[i], "bar {i} changed when only later bars changed"); }
    }

    #[test]
    fn the_session_vwap_resets_each_day_and_the_clock_runs_from_open_to_close() {
        let closes: Vec<f64> = (0..130).map(|i| 100.0 + (i / 26) as f64 * 5.0).collect();
        let r = compute(&bars_from(&closes, 1000.0));
        assert!(r[60][idx("vwap_dist")].abs() < 0.5, "inside day 3 the price sits near that day's own VWAP, not near the average of all days: {}", r[60][idx("vwap_dist")]);
        let first_bar_of_day = 78; // 3 x 26
        assert!(r[first_bar_of_day][idx("tod_sin")] < 0.1 && r[first_bar_of_day][idx("tod_cos")] > 0.9, "the open");
        assert!(r[first_bar_of_day + 25][idx("tod_cos")] < -0.9, "the close");
    }
}

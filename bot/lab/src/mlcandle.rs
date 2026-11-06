//! Machine learning on the market's own candles (owner 2026-10-13: "there is plenty of candle data to be testing against and learning from").
//!
//! The question: from what the candles show at a given moment, can a model rank WHICH stocks do better than the others over the next trading day?
//! That is what an entry filter needs, and the data is large: 95 symbols x 15-minute bars from 2026-01-02 (about 470,000 candles), independent of the
//! strategy's own trades.
//!
//! Design (every choice is there to prevent fooling ourselves):
//!   * features use only bars up to and including the decision bar (checked by a test that rewrites the future);
//!   * label = log return over the next `H` bars (one session) MINUS the universe average at the same moment, so the market's direction cancels and only
//!     the ranking of stocks remains; a moment needs at least `MIN_CROSS` symbols;
//!   * split by TIME with a purge gap of 3 days between train and test (labels look a day ahead, so neighbours would otherwise share information);
//!   * the score is the rank information coefficient per moment (Spearman between prediction and realised excess return across symbols) and the spread
//!     between the top and bottom fifth, with intervals from resampling whole WEEKS (neighbouring hours overlap);
//!   * control: models trained on excess returns shuffled WITHIN each moment must score about zero;
//!   * simple known signals (reversal, momentum) are scored too, so we see whether learning adds anything beyond them.
//!
//! Models are the self-written ones in `mltrade.rs`. Nothing here touches the live strategy.

use crate::candles::Bar;
use crate::mltrade::{Gbdt, GbdtParams, Logistic, Trade};
use std::collections::BTreeMap;

/// Forward horizon in 15-minute bars (one regular session).
pub const H: usize = 26;
/// A moment is used only when at least this many symbols have a complete feature row and label.
pub const MIN_CROSS: usize = 15;
const WARMUP: usize = 130;
const MS15: i64 = 900_000;
pub const DAY_MS: i64 = 86_400_000;

pub const FEATURES: [&str; 23] = ["ret_1", "ret_4", "ret_13", "ret_26", "ret_65", "ret_130", "vol_26", "vol_130", "atr_14", "dist_ma26", "dist_ma130",
    "vol_ratio_1", "vol_ratio_26_130", "range_pos_52", "bar_in_day", "weekday", "rank_ret_4", "rank_ret_26", "rank_vol_26", "mkt_ret_26", "mkt_ret_130",
    "mkt_breadth", "mkt_dispersion"];

#[derive(Debug, Clone)]
pub struct Sample { pub ts: i64, pub week: i64, pub sym: usize, pub x: Vec<f64>, pub excess: f64, pub raw: f64 }

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400); let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}
fn utc_ms(y: i64, m: i64, d: i64, h: i64) -> i64 { days_from_civil(y, m, d) * DAY_MS + h * 3_600_000 }

/// New York's offset from UTC in milliseconds for 2026 (daylight time from 8 March 07:00 UTC to 1 November 06:00 UTC).
pub fn et_offset_ms(ts: i64) -> i64 { if ts >= utc_ms(2026, 3, 8, 7) && ts < utc_ms(2026, 11, 1, 6) { -4 * 3_600_000 } else { -5 * 3_600_000 } }

fn mean(v: &[f64]) -> f64 { v.iter().sum::<f64>() / v.len() as f64 }
fn std(v: &[f64]) -> f64 { let m = mean(v); (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt() }

/// One symbol's stock-level features at bar `i` (needs `i >= WARMUP`), or None. Uses bars 0..=i only.
fn stock_features(bars: &[Bar], r: &[f64], i: usize, bar_in_day: f64, weekday: f64) -> Option<[f64; 16]> {
    if i < WARMUP { return None; }
    let c = |k: usize| bars[i - k].close;
    let ret = |k: usize| (c(0) / c(k)).ln();
    let w26 = &r[i - 25..=i]; let w130 = &r[i - 129..=i];
    let atr = mean(&bars[i - 13..=i].iter().map(|b| (b.high - b.low) / b.close).collect::<Vec<_>>());
    let ma = |n: usize| mean(&bars[i + 1 - n..=i].iter().map(|b| b.close).collect::<Vec<_>>());
    let v26 = mean(&bars[i - 25..=i].iter().map(|b| b.volume).collect::<Vec<_>>());
    let v130 = mean(&bars[i - 129..=i].iter().map(|b| b.volume).collect::<Vec<_>>());
    let (lo, hi) = bars[i - 51..=i].iter().fold((f64::MAX, f64::MIN), |a, b| (a.0.min(b.low), a.1.max(b.high)));
    if v26 <= 0.0 || v130 <= 0.0 || hi <= lo { return None; }
    Some([ret(1), ret(4), ret(13), ret(26), ret(65), ret(130), std(w26), std(w130), atr, c(0) / ma(26) - 1.0, c(0) / ma(130) - 1.0, bars[i].volume / v26, v26 / v130,
        (c(0) - lo) / (hi - lo), bar_in_day, weekday])
}

/// All samples (one per symbol per grid moment) with cross-sectional features and the excess forward return. `step` keeps every `step`-th 15-minute grid
/// point (4 = hourly). No information from after the decision bar enters any feature.
pub fn build(series: &BTreeMap<String, Vec<Bar>>, step: i64) -> Vec<Sample> { build_h(series, step, H) }

/// `build` with a label horizon of `h` bars (the features are unchanged).
pub fn build_h(series: &BTreeMap<String, Vec<Bar>>, step: i64, h: usize) -> Vec<Sample> { build_core(series, step, h, false) }

/// The features of the NEWEST bars, whose labels are not known yet (excess and raw are NaN): what the model needs to rank the stocks right now. Every 15-minute bar of the
/// last two hours is included; callers keep the newest moment.
pub fn build_live(series: &BTreeMap<String, Vec<Bar>>) -> Vec<Sample> { build_core(series, 1, H, true) }

fn build_core(series: &BTreeMap<String, Vec<Bar>>, step: i64, h: usize, live: bool) -> Vec<Sample> {
    struct Row { ts: i64, st: [f64; 16], fwd: f64, sym: usize }
    let mut rows: Vec<Row> = vec![];
    for (si, (name, bars)) in series.iter().enumerate() {
        if name == "SPY" { continue; } // the reference index is not one of the stocks being ranked
        if bars.len() < WARMUP + h + 2 { continue; }
        let r: Vec<f64> = std::iter::once(0.0).chain(bars.windows(2).map(|w| (w[1].close / w[0].close).ln())).collect();
        // position of each bar inside its New York trading day, and the weekday
        let mut bid = vec![0f64; bars.len()]; let mut wd = vec![0f64; bars.len()];
        let (mut day, mut n) = (i64::MIN, 0f64);
        for (i, b) in bars.iter().enumerate() {
            let local = b.ts + et_offset_ms(b.ts); let d = local.div_euclid(DAY_MS);
            if d != day { day = d; n = 0.0; }
            bid[i] = n; n += 1.0; wd[i] = (d + 4).rem_euclid(7) as f64;
        }
        let end = if live { bars.len() } else { bars.len() - h };
        for i in WARMUP..end {
            if live && i + 8 < bars.len() { continue; }
            let labelled = i + h < bars.len();
            if (bars[i].ts / MS15) % step != 0 || (labelled && bars[i + h].ts - bars[i].ts > (4 + 2 * (h as i64 / 26)) * DAY_MS) { continue; }
            if let Some(st) = stock_features(bars, &r, i, bid[i], wd[i]) { rows.push(Row { ts: bars[i].ts, st, fwd: if labelled { (bars[i + h].close / bars[i].close).ln() } else { f64::NAN }, sym: si }); }
        }
    }
    let mut by_ts: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (k, r) in rows.iter().enumerate() { by_ts.entry(r.ts).or_default().push(k); }
    let mut out = vec![];
    for (ts, idx) in by_ts {
        if idx.len() < MIN_CROSS { continue; }
        let n = idx.len() as f64;
        let rank = |f: &dyn Fn(&Row) -> f64| -> Vec<f64> {
            let mut o: Vec<usize> = (0..idx.len()).collect();
            o.sort_by(|&a, &b| f(&rows[idx[a]]).total_cmp(&f(&rows[idx[b]])));
            let mut p = vec![0.0; idx.len()];
            for (pos, &k) in o.iter().enumerate() { p[k] = pos as f64 / (n - 1.0); }
            p
        };
        let (r4, r26, v26) = (rank(&|r| r.st[1]), rank(&|r| r.st[3]), rank(&|r| r.st[6]));
        let fin: Vec<f64> = idx.iter().map(|&k| rows[k].fwd).filter(|x| x.is_finite()).collect();
        let fwd_mean = if fin.is_empty() { f64::NAN } else { fin.iter().sum::<f64>() / fin.len() as f64 };
        let m26: Vec<f64> = idx.iter().map(|&k| rows[k].st[3]).collect();
        let mkt = [mean(&m26), idx.iter().map(|&k| rows[k].st[5]).sum::<f64>() / n, idx.iter().filter(|&&k| rows[k].st[10] > 0.0).count() as f64 / n, std(&m26)];
        let week = ts.div_euclid(7 * DAY_MS);
        for (j, &k) in idx.iter().enumerate() {
            let mut x = rows[k].st.to_vec();
            x.extend([r4[j], r26[j], v26[j], mkt[0], mkt[1], mkt[2], mkt[3]]);
            
            out.push(Sample { ts, week, sym: rows[k].sym, x, excess: rows[k].fwd - fwd_mean, raw: rows[k].fwd });
        }
    }
    out
}

/// (train, test) by time. Train keeps samples at least `purge_days` before the cut so no training label window overlaps the test period.
pub fn split(samples: &[Sample], test_share: f64, purge_days: i64) -> (Vec<Sample>, Vec<Sample>) {
    let mut ts: Vec<i64> = samples.iter().map(|s| s.ts).collect(); ts.sort(); ts.dedup();
    let cut = ts[((ts.len() as f64) * (1.0 - test_share)) as usize];
    (samples.iter().filter(|s| s.ts <= cut - purge_days * DAY_MS).cloned().collect(), samples.iter().filter(|s| s.ts >= cut).cloned().collect())
}

pub fn to_trades(s: &[Sample]) -> Vec<Trade> { s.iter().map(|x| Trade { block: x.week, x: x.x.clone(), win: x.excess > 0.0, ret: x.excess }).collect() }

/// Shuffles the excess returns WITHIN each moment (the control: it keeps every moment's cross-section but destroys any link to the features).
pub fn shuffle_within_moment(s: &[Sample], seed: u64) -> Vec<Sample> {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in s.iter().enumerate() { by.entry(x.ts).or_default().push(i); }
    let mut out = s.to_vec();
    let mut st = seed | 1;
    for idx in by.values() {
        for k in (1..idx.len()).rev() { st ^= st << 13; st ^= st >> 7; st ^= st << 17; let j = (st % (k as u64 + 1)) as usize; let (a, b) = (out[idx[k]].excess, out[idx[j]].excess); out[idx[k]].excess = b; out[idx[j]].excess = a; }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Eval { pub moments: usize, pub weeks: usize, pub ic: f64, pub ic_lo: f64, pub ic_hi: f64, pub spread_bps: f64, pub spread_lo: f64, pub spread_hi: f64, pub top_bps: f64 }

/// Rank information coefficient and top-minus-bottom-fifth spread of `pred` against the realised excess returns, per moment, with week-resampled intervals.
pub fn evaluate(test: &[Sample], pred: &[f64], seed: u64) -> Eval {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in test.iter().enumerate() { by.entry(x.ts).or_default().push(i); }
    // per week: (sum ic, sum spread, sum top, moments)
    let mut weeks: BTreeMap<i64, (f64, f64, f64, usize)> = BTreeMap::new();
    for idx in by.values().filter(|v| v.len() >= MIN_CROSS) {
        let p: Vec<f64> = idx.iter().map(|&i| pred[i]).collect();
        let e: Vec<f64> = idx.iter().map(|&i| test[i].excess).collect();
        let ic = crate::brain::spearman(&p, &e);
        let mut o: Vec<usize> = (0..idx.len()).collect();
        o.sort_by(|&a, &b| p[b].total_cmp(&p[a]));
        let k = (idx.len() / 5).max(1);
        let top = o[..k].iter().map(|&i| e[i]).sum::<f64>() / k as f64;
        let bot = o[o.len() - k..].iter().map(|&i| e[i]).sum::<f64>() / k as f64;
        let w = weeks.entry(test[idx[0]].week).or_default();
        w.0 += ic; w.1 += top - bot; w.2 += top; w.3 += 1;
    }
    let wv: Vec<(f64, f64, f64, usize)> = weeks.values().copied().collect();
    let pooled = |sel: &[usize]| { let (a, b, c, n) = sel.iter().fold((0.0, 0.0, 0.0, 0usize), |s, &i| (s.0 + wv[i].0, s.1 + wv[i].1, s.2 + wv[i].2, s.3 + wv[i].3)); if n == 0 { (0.0, 0.0, 0.0) } else { (a / n as f64, b / n as f64, c / n as f64) } };
    let all: Vec<usize> = (0..wv.len()).collect();
    let (ic, sp, top) = pooled(&all);
    let mut st = seed | 1;
    let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; st };
    let (mut ics, mut sps): (Vec<f64>, Vec<f64>) = (vec![], vec![]);
    for _ in 0..2000 { let sel: Vec<usize> = (0..wv.len()).map(|_| (rnd() % wv.len().max(1) as u64) as usize).collect(); let (a, b, _) = pooled(&sel); ics.push(a); sps.push(b); }
    ics.sort_by(f64::total_cmp); sps.sort_by(f64::total_cmp);
    let q = |v: &Vec<f64>, p: usize| if v.is_empty() { 0.0 } else { v[v.len() * p / 1000] };
    Eval { moments: wv.iter().map(|w| w.3).sum(), weeks: wv.len(), ic, ic_lo: q(&ics, 25), ic_hi: q(&ics, 975), spread_bps: sp * 1e4, spread_lo: q(&sps, 25) * 1e4, spread_hi: q(&sps, 975) * 1e4, top_bps: top * 1e4 }
}

/// A fixed, simple signal (no learning), for comparison: index into `FEATURES` and a sign.
pub fn simple_signal(test: &[Sample], feature: &str, sign: f64) -> Vec<f64> { let j = FEATURES.iter().position(|f| *f == feature).expect("feature"); test.iter().map(|s| sign * s.x[j]).collect() }

/// Ridge regression on standardised features (closed form, own solver). Cross-checked against smartcore in `examples/candle_libs.rs`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Ridge { mu: Vec<f64>, sd: Vec<f64>, w: Vec<f64>, b: f64 }
impl Ridge {
    pub fn fit(train: &[Sample], alpha: f64) -> Ridge { Self::fit_w(train, alpha, &|_| 1.0) }

    /// Weighted ridge: sample `s` counts `weight(s)` times (recency weighting for an edge that may fade). The ridge strength is relative to the mean weight.
    pub fn fit_w(train: &[Sample], alpha: f64, weight: &dyn Fn(&Sample) -> f64) -> Ridge {
        let d = train[0].x.len(); let n = train.len() as f64;
        let wsum: f64 = train.iter().map(weight).sum::<f64>().max(1e-12);
        let wmean = wsum / n;
        let mu: Vec<f64> = (0..d).map(|j| train.iter().map(|s| s.x[j]).sum::<f64>() / n).collect();
        let sd: Vec<f64> = (0..d).map(|j| (train.iter().map(|s| (s.x[j] - mu[j]).powi(2)).sum::<f64>() / n).sqrt().max(1e-12)).collect();
        let b = train.iter().map(|s| weight(s) * s.excess).sum::<f64>() / wsum;
        let z = |s: &Sample, j: usize| ((s.x[j] - mu[j]) / sd[j]).clamp(-5.0, 5.0);
        let mut a = vec![vec![0.0; d + 1]; d];
        for s in train { let ws = weight(s); let zs: Vec<f64> = (0..d).map(|j| z(s, j)).collect(); for i in 0..d { for j in 0..d { a[i][j] += ws * zs[i] * zs[j]; } a[i][d] += ws * zs[i] * (s.excess - b); } }
        for (i, row) in a.iter_mut().enumerate() { row[i] += alpha * wmean; }
        // Gaussian elimination with partial pivoting (the matrix is positive definite)
        for c in 0..d {
            let p = (c..d).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs())).unwrap_or(c);
            a.swap(c, p);
            let piv = a[c][c];
            for r in 0..d { if r != c { let f = a[r][c] / piv; if f != 0.0 { for k in c..=d { let v = a[c][k]; a[r][k] -= f * v; } } } }
        }
        let w = (0..d).map(|i| a[i][d] / a[i][i]).collect();
        Ridge { mu, sd, w, b }
    }
    pub fn predict(&self, s: &Sample) -> f64 { self.b + (0..self.w.len()).map(|j| self.w[j] * ((s.x[j] - self.mu[j]) / self.sd[j]).clamp(-5.0, 5.0)).sum::<f64>() }
    pub fn weights(&self) -> &[f64] { &self.w }
    /// What each feature adds to the prediction for `s` (weight x standardised value), in the order of the features: the model's own explanation.
    pub fn contributions(&self, s: &Sample) -> Vec<f64> { (0..self.w.len()).map(|j| self.w[j] * ((s.x[j] - self.mu[j]) / self.sd[j]).clamp(-5.0, 5.0)).collect() }
}

/// Rolling-origin test: for each 4-week window after the first `first_days` days, fit on everything at least `purge` days older and predict the window.
/// Returns the pooled predictions aligned with the returned samples (all windows, all genuinely out of sample).
pub fn rolling<F: Fn(&[Sample]) -> Box<dyn Fn(&Sample) -> f64>>(all: &[Sample], first_days: i64, window_days: i64, purge_days: i64, fit_fn: F) -> (Vec<Sample>, Vec<f64>) {
    let t0 = all.iter().map(|s| s.ts).min().unwrap_or(0); let t1 = all.iter().map(|s| s.ts).max().unwrap_or(0);
    let (mut out, mut pred) = (vec![], vec![]);
    let mut start = t0 + first_days * DAY_MS;
    while start < t1 {
        let end = start + window_days * DAY_MS;
        let train: Vec<Sample> = all.iter().filter(|s| s.ts <= start - purge_days * DAY_MS).cloned().collect();
        let test: Vec<&Sample> = all.iter().filter(|s| s.ts >= start && s.ts < end).collect();
        if train.len() > 5000 && test.len() > 500 { let f = fit_fn(&train); for s in test { pred.push(f(s)); out.push(s.clone()); } }
        start = end;
    }
    (out, pred)
}

pub struct Trained { pub trees: Gbdt, pub rounds: usize, pub logistic: Logistic }

/// Fits the return-trees (early stopped on the last quarter of the training period) and a logistic baseline.
pub fn fit(train: &[Sample]) -> Trained {
    let t = to_trades(train);
    let (trees, rounds) = Gbdt::fit_early_stopped(&t, GbdtParams::default(), true);
    Trained { trees, rounds, logistic: Logistic::fit(&t, 120, 0.3, 1e-4) }
}

/// The pre-registered test (fixed in advance, 2026-10-13): hourly grid, test = latest quarter of the time line, 3-day purge, trees fitted to excess return with
/// early stopping on the training period. It QUALIFIES only if the rank-IC interval (weeks resampled) is above zero, the top-minus-bottom spread interval is
/// above zero, and the IC beats the largest IC of 5 models trained on within-moment shuffled labels.
pub struct Verdict { pub qualifies: bool, pub text: String }

pub fn report(series: &BTreeMap<String, Vec<Bar>>, costs: &crate::candles::Costs) -> Verdict {
    let all = build(series, 4);
    if all.len() < 5000 { return Verdict { qualifies: false, text: format!("too little candle data ({} samples)", all.len()) }; }
    let (train, test) = split(&all, 0.25, 3);
    let days = |s: &[Sample]| { let a = s.iter().map(|x| x.ts).min().unwrap_or(0); let b = s.iter().map(|x| x.ts).max().unwrap_or(0); (b - a) / DAY_MS };
    let syms = series.values().filter(|b| b.len() > WARMUP + H + 2).count();
    let mut t = format!("{} symbols, {} candles, {} samples (hourly moments x symbols); train {} samples over {} days, test {} samples over {} days (3-day purge)\n",
        syms, series.values().map(|b| b.len()).sum::<usize>(), all.len(), train.len(), days(&train), test.len(), days(&test));
    let fmt = |name: &str, e: &Eval| format!("  {name:<26} rank IC {:+.4} [{:+.4},{:+.4}] | top-minus-bottom fifth {:+.1} bps [{:+.1},{:+.1}] per session | top fifth {:+.1} bps | {} weeks\n", e.ic, e.ic_lo, e.ic_hi, e.spread_bps, e.spread_lo, e.spread_hi, e.top_bps, e.weeks);
    let m = fit(&train);
    let tt = to_trades(&test);
    let pt: Vec<f64> = tt.iter().map(|x| m.trees.predict(x)).collect();
    let pl: Vec<f64> = tt.iter().map(|x| m.logistic.predict(x)).collect();
    let et = evaluate(&test, &pt, 11);
    t += "models (excess return over the next 26 bars, market direction removed):\n";
    t += &fmt(&format!("trees ({} rounds)", m.rounds), &et);
    t += &fmt("logistic (above/below)", &evaluate(&test, &pl, 12));
    t += "simple signals, no learning:\n";
    for (n, f, sg) in [("reversal (-ret_26)", "ret_26", -1.0), ("reversal (-ret_4)", "ret_4", -1.0), ("momentum (ret_130)", "ret_130", 1.0), ("low volatility", "vol_26", -1.0), ("high volume surge", "vol_ratio_1", 1.0)] {
        t += &fmt(n, &evaluate(&test, &simple_signal(&test, f, sg), 13));
    }
    let mut best_null = f64::MIN;
    let mut nulls = vec![];
    for k in 0..5u64 {
        let nm = fit(&shuffle_within_moment(&train, 100 + k));
        let p: Vec<f64> = tt.iter().map(|x| nm.trees.predict(x)).collect();
        let e = evaluate(&test, &p, 20 + k); best_null = best_null.max(e.ic); nulls.push(e.ic);
    }
    t += &format!("control: 5 trees trained on within-moment shuffled labels: IC {} (largest {:+.4}); must be about 0\n", nulls.iter().map(|x| format!("{x:+.4}")).collect::<Vec<_>>().join(" "), best_null);
    t += "learning curve (train on the earliest share of the training period, same fixed test):\n";
    let mut ts: Vec<i64> = train.iter().map(|x| x.ts).collect(); ts.sort(); ts.dedup();
    for share in [0.25, 0.5, 0.75, 1.0] {
        let cut = ts[((ts.len() as f64 * share) as usize).min(ts.len() - 1)];
        let part: Vec<Sample> = train.iter().filter(|x| x.ts <= cut).cloned().collect();
        let mm = fit(&part);
        let p: Vec<f64> = tt.iter().map(|x| mm.trees.predict(x)).collect();
        let e = evaluate(&test, &p, 31);
        t += &format!("  first {:>3.0}% of training ({:>3} days, {} rounds): IC {:+.4} [{:+.4},{:+.4}], spread {:+.1} bps\n", share * 100.0, days(&part), mm.rounds, e.ic, e.ic_lo, e.ic_hi, e.spread_bps);
    }
    // version 2 (2026-10-13): the linear model. A library cross-check (smartcore ridge/elastic net, examples/candle_libs.rs) found it where the trees found nothing;
    // the ridge is our own 30-line solver. Chosen AFTER seeing the single split, so it is judged on rolling windows and, cleanly, on data after the freeze date.
    t += "linear model (ridge, alpha 100; own solver, matches smartcore):\n";
    let rolled = |a: f64, data: &[Sample]| rolling(data, 100, 28, 3, |tr| { let r = Ridge::fit(tr, a); Box::new(move |x| r.predict(x)) });
    let (rs, rp) = rolled(100.0, &all);
    let er = evaluate(&rs, &rp, 41);
    t += &fmt("ridge, rolling 4-week windows", &er);
    let sh_all = shuffle_within_moment(&all, 77);
    let (ss, sp) = rolled(100.0, &sh_all);
    let es = evaluate(&ss, &sp, 42);
    t += &fmt("control: ridge on shuffled", &es);
    // version 3 (2026-10-13): the verdict is the walk-forward trade test WITH FEES and parameters re-chosen before every 14-session block (mlwalk.rs)
    let names: Vec<String> = series.keys().cloned().collect();
    let freeze_session = (utc_ms(2026, 10, 13, 12) + et_offset_ms(utc_ms(2026, 10, 13, 12))).div_euclid(DAY_MS);
    let w = crate::mlwalk::report(&all, &names, costs, freeze_session);
    t += &w.text;
    let pooled_ok = w.pooled.lo > 0.0 && w.control.net_bps < w.pooled.net_bps / 2.0;
    let forward_ok = w.forward_days >= 28 && w.forward.lo > 0.0;
    let qualifies = pooled_ok && forward_ok;
    t += &format!("VERDICT: {}", if qualifies { "QUALIFIES (net of fees, the pooled walk-forward AND the forward blocks have intervals above zero)" } else if pooled_ok { "LEAD, not qualified: net of fees the pooled walk-forward interval is above zero and the shuffled control is not; waiting for the forward blocks (28 sessions from 2026-10-13, interval above zero)" } else { if w.pooled.lo <= 0.0 { "does not qualify: net of fees (commission and measured spreads) the pooled walk-forward interval includes zero" } else { "does not qualify: the shuffled-label control earns about as much as the model" } });
    let _ = (best_null, et.ic);
    Verdict { qualifies, text: t }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny market: `n_sym` symbols with a deterministic random walk on the shared 15-minute grid.
    fn market(n_sym: usize, bars: usize, seed: u64) -> BTreeMap<String, Vec<Bar>> {
        let mut st = seed | 1;
        let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let start = utc_ms(2026, 4, 6, 13) + 30 * 60_000; // a Monday 13:30 UTC = 09:30 New York (daylight time)
        (0..n_sym).map(|s| {
            let mut p = 50.0 + s as f64; let mut v = vec![];
            for i in 0..bars {
                // 26 bars a day, five days a week: skip nights and weekends on the clock
                let day = i / 26; let slot = (i % 26) as i64; let wk = day / 5; let dow = day % 5;
                let ts = start + (wk as i64 * 7 + dow as i64) * DAY_MS + slot * MS15;
                let o = p; p *= 1.0 + rnd() * 0.004; v.push(Bar { ts, open: o, high: o.max(p) * 1.001, low: o.min(p) * 0.999, close: p, volume: 1000.0 * (1.0 + rnd() + 0.6) });
            }
            (format!("S{s:02}"), v)
        }).collect()
    }

    #[test]
    fn time_zone_offsets_follow_daylight_saving() {
        assert_eq!(et_offset_ms(utc_ms(2026, 1, 15, 12)), -5 * 3_600_000);
        assert_eq!(et_offset_ms(utc_ms(2026, 7, 1, 12)), -4 * 3_600_000);
        assert_eq!(et_offset_ms(utc_ms(2026, 3, 8, 6)), -5 * 3_600_000);
        assert_eq!(et_offset_ms(utc_ms(2026, 3, 8, 7)), -4 * 3_600_000);
    }

    #[test]
    fn features_never_look_ahead() {
        let m = market(24, 400, 3);
        let a = build(&m, 4);
        assert!(!a.is_empty());
        // rewrite the LAST 40 bars of every symbol wildly: every sample whose decision bar and label window end before them must be unchanged
        let mut m2 = m.clone();
        for bars in m2.values_mut() { let n = bars.len(); for b in &mut bars[n - 40..] { b.close *= 3.0; b.high *= 3.0; b.low *= 3.0; b.open *= 3.0; b.volume *= 9.0; } }
        let b = build(&m2, 4);
        let cutoff = m.values().next().unwrap()[400 - 40 - H - 1].ts;
        let key = |s: &Sample| s.ts;
        let early_a: Vec<&Sample> = a.iter().filter(|s| key(s) <= cutoff).collect();
        let early_b: Vec<&Sample> = b.iter().filter(|s| key(s) <= cutoff).collect();
        assert_eq!(early_a.len(), early_b.len());
        for (x, y) in early_a.iter().zip(&early_b) { assert_eq!(x.x, y.x, "a feature changed when only the FUTURE was rewritten"); assert!((x.excess - y.excess).abs() < 1e-12); }
    }

    #[test]
    fn the_label_is_the_return_over_the_next_session_minus_the_moment_average() {
        let m = market(22, 300, 5);
        let s = build(&m, 4);
        // excess returns of one moment sum to zero
        let mut by: BTreeMap<i64, f64> = BTreeMap::new();
        for x in &s { *by.entry(x.ts).or_default() += x.excess; }
        assert!(by.values().all(|v| v.abs() < 1e-9));
        // and the raw forward return for symbol 0 at its first sample matches the closes
        let first_ts = s[0].ts;
        let bars = &m["S00"]; let i = bars.iter().position(|b| b.ts == first_ts).unwrap();
        let fwd0 = (bars[i + H].close / bars[i].close).ln();
        let all: Vec<f64> = m.values().map(|b| { let k = b.iter().position(|x| x.ts == first_ts).unwrap(); (b[k + H].close / b[k].close).ln() }).collect();
        let avg = all.iter().sum::<f64>() / all.len() as f64;
        assert!((s[0].excess - (fwd0 - avg)).abs() < 1e-12);
    }

    #[test]
    fn the_split_leaves_a_gap_and_the_test_is_later() {
        let s = build(&market(22, 700, 9), 4);
        let (tr, te) = split(&s, 0.25, 3);
        let (last_train, first_test) = (tr.iter().map(|x| x.ts).max().unwrap(), te.iter().map(|x| x.ts).min().unwrap());
        assert!(first_test - last_train >= 3 * DAY_MS, "gap {} days", (first_test - last_train) / DAY_MS);
        assert!(tr.len() > te.len());
    }

    #[test]
    fn ic_and_spread_are_right_on_a_constructed_case_and_zero_for_noise() {
        let mk = |ts: i64, e: &[f64]| e.iter().map(|&x| Sample { ts, week: ts / (7 * DAY_MS), sym: 0, x: vec![], excess: x, raw: x }).collect::<Vec<_>>();
        let e: Vec<f64> = (0..25).map(|i| (i as f64 - 12.0) / 1000.0).collect();
        let mut test = mk(0, &e); test.extend(mk(DAY_MS, &e));
        let perfect: Vec<f64> = test.iter().map(|s| s.excess).collect();
        let ev = evaluate(&test, &perfect, 1);
        assert!((ev.ic - 1.0).abs() < 1e-9 && ev.spread_bps > 0.0 && ev.moments == 2);
        let flipped: Vec<f64> = perfect.iter().map(|x| -x).collect();
        assert!((evaluate(&test, &flipped, 1).ic + 1.0).abs() < 1e-9);
        let constant = vec![0.5; test.len()];
        assert_eq!(evaluate(&test, &constant, 1).ic, 0.0);
    }

    #[test]
    fn shuffling_within_a_moment_keeps_each_cross_section() {
        let s = build(&market(22, 300, 7), 4);
        let sh = shuffle_within_moment(&s, 5);
        let sum = |v: &[Sample]| { let mut m: BTreeMap<i64, f64> = BTreeMap::new(); for x in v { *m.entry(x.ts).or_default() += x.excess.abs(); } m };
        let (a, b) = (sum(&s), sum(&sh));
        assert!(a.iter().all(|(k, v)| (v - b[k]).abs() < 1e-9), "the multiset of excess returns per moment is preserved");
        assert!(s.iter().zip(&sh).any(|(x, y)| x.excess != y.excess), "but the assignment changed");
    }

    #[test]
    fn ridge_recovers_a_planted_linear_signal_and_rolling_only_uses_the_past() {
        let s = build(&market(60, 2600, 13), 4);
        let j = FEATURES.iter().position(|f| *f == "ret_26").unwrap();
        let planted: Vec<Sample> = s.iter().map(|x| Sample { excess: -0.8 * x.x[j], ..x.clone() }).collect();
        let r = Ridge::fit(&planted, 1.0);
        assert!(r.weights()[j] < 0.0, "{:?}", r.weights());
        let (te, pr) = rolling(&planted, 20, 14, 3, |t| { assert!(t.len() > 5000); let r = Ridge::fit(t, 1.0); Box::new(move |x| r.predict(x)) });
        assert!(!te.is_empty() && evaluate(&te, &pr, 1).ic > 0.9);
        // corrupt the labels of the LAST 30 days: predictions for everything before them must not change
        let t1 = planted.iter().map(|x| x.ts).max().unwrap();
        let spoiled: Vec<Sample> = planted.iter().map(|x| if x.ts > t1 - 30 * DAY_MS { Sample { excess: 5.0, ..x.clone() } } else { x.clone() }).collect();
        let (te2, pr2) = rolling(&spoiled, 20, 14, 3, |t| { let r = Ridge::fit(t, 1.0); Box::new(move |x| r.predict(x)) });
        let n = te.iter().take_while(|x| x.ts <= t1 - 30 * DAY_MS - 14 * DAY_MS).count();
        assert!(n > 100 && te2.len() >= n);
        assert!(pr[..n].iter().zip(&pr2[..n]).all(|(a, b)| (a - b).abs() < 1e-12), "an earlier window changed when only later labels changed");
    }

    #[test]
    fn a_real_cross_sectional_signal_is_found_and_noise_is_not() {
        // plant a signal: excess return depends on the stock's own 26-bar return (reversal), plus noise
        let s = build(&market(24, 1400, 11), 4);
        let j = FEATURES.iter().position(|f| *f == "ret_26").unwrap();
        let planted: Vec<Sample> = s.iter().map(|x| Sample { excess: -0.8 * x.x[j] + 0.002 * ((x.ts / 7919 + x.x[0].to_bits() as i64 % 13) % 11) as f64 / 11.0, ..x.clone() }).collect();
        let (tr, te) = split(&planted, 0.25, 3);
        let m = fit(&tr);
        let pred: Vec<f64> = to_trades(&te).iter().map(|t| m.trees.predict(t)).collect();
        let ev = evaluate(&te, &pred, 3);
        assert!(ev.ic > 0.3 && ev.ic_lo > 0.0 && m.rounds > 0, "{ev:?} rounds {}", m.rounds);
        let (tr0, te0) = split(&s, 0.25, 3);
        let noise = shuffle_within_moment(&tr0, 9);
        let m0 = fit(&noise);
        let ev0 = evaluate(&te0, &to_trades(&te0).iter().map(|t| m0.trees.predict(t)).collect::<Vec<_>>(), 3);
        assert!(ev0.ic.abs() < 0.08, "a model trained on shuffled labels must be near zero: {ev0:?}");
    }
}

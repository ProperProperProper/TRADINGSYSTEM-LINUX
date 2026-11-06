//! A2.4: the numeric learner as a signal candidate. Causal features per symbol and 1-minute candle (cross-sectionally ranked) predict the next session's
//! return relative to the market. For each search period the model is trained only on candles BEFORE the period (with a purge so no target reaches into it)
//! and judged on the period itself: rank correlation (IC) of prediction and outcome per candle, against single-feature baselines and against a null control
//! (the same model trained on targets shuffled within each candle). The verdict is "no evidence" unless it clearly beats both.
use anyhow::Result;
use std::ops::Range;

use crate::gbdt::{self, Params};
use crate::market::Built;

/// Candles of history a row needs before it (the longest feature window).
pub const LOOKBACK: usize = 20;
/// Candles ahead the target looks: one session of 1-minute candles (26).
pub const HORIZON: usize = 26;
pub const FEATURES: [&str; 9] = [
    "ret_1",
    "ret_2",
    "ret_4",
    "ret_8",
    "vol_8",
    "range",
    "vol_z",
    "dist_ema8",
    "log_turnover",
];

/// Feature rows (cross-sectionally ranked to [0,1] at each candle) and targets (the next session's return minus the cross-section's mean).
#[derive(Debug, Default)]
pub struct Rows {
    pub candle: Vec<usize>,
    pub x: Vec<Vec<f64>>,
    /// Relative forward return, clipped at the cross-section's 1st and 99th percentile (what the model trains on).
    pub y: Vec<f64>,
    /// The same without clipping (what the IC is measured against).
    pub y_raw: Vec<f64>,
}

fn ranks01(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0;
        for &k in &idx[i..=j] {
            r[k] = avg / (v.len().max(2) - 1) as f64;
        }
        i = j + 1;
    }
    r
}

/// Spearman rank correlation.
pub fn spearman(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.len() < 3 {
        return None;
    }
    let (ra, rb) = (ranks01(a), ranks01(b));
    let n = a.len() as f64;
    let (ma, mb) = (ra.iter().sum::<f64>() / n, rb.iter().sum::<f64>() / n);
    let cov: f64 = ra.iter().zip(&rb).map(|(x, y)| (x - ma) * (y - mb)).sum();
    let (va, vb): (f64, f64) = (
        ra.iter().map(|x| (x - ma).powi(2)).sum(),
        rb.iter().map(|y| (y - mb).powi(2)).sum(),
    );
    (va > 0.0 && vb > 0.0).then(|| cov / (va * vb).sqrt())
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// The raw (not yet ranked) feature vector of symbol `s` at candle `t`, from bars at or before `t` only. None when a bar in the window is missing or a
/// value is not finite.
pub fn feature_vector(m: &moomoo_bot::engine::Market, s: usize, t: usize) -> Option<Vec<f64>> {
    if t < LOOKBACK || s >= m.bars.len() || t >= m.ts.len() {
        return None;
    }
    let bars = &m.bars[s];
    if t >= bars.len() {
        return None;
    }
    let w: Option<Vec<_>> = (t - LOOKBACK..=t).map(|k| bars[k]).collect();
    let w = w?;
    let at = |k: usize| w[k - (t - LOOKBACK)]; // bar at candle k
    let close = |k: usize| at(k).close;
    if (t - LOOKBACK..=t).any(|k| close(k).is_nan() || close(k) <= 0.0) {
        return None;
    }
    let rets: Vec<f64> = (t - 7..=t).map(|k| close(k) / close(k - 1) - 1.0).collect();
    let mean = rets.iter().sum::<f64>() / rets.len() as f64;
    let vol8 = (rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rets.len() as f64).sqrt();
    let logv: Vec<f64> = (t - LOOKBACK..t)
        .map(|k| (at(k).volume + 1.0).ln())
        .collect();
    let lm = logv.iter().sum::<f64>() / logv.len() as f64;
    let lsd = (logv.iter().map(|v| (v - lm).powi(2)).sum::<f64>() / logv.len() as f64).sqrt();
    let vol_z = if lsd > 0.0 {
        ((at(t).volume + 1.0).ln() - lm) / lsd
    } else {
        0.0
    };
    let mut ema = close(t - LOOKBACK);
    for k in t - LOOKBACK + 1..=t {
        ema += (close(k) - ema) * 2.0 / 9.0;
    }
    let f = vec![
        close(t) / close(t - 1) - 1.0,
        close(t) / close(t - 2) - 1.0,
        close(t) / close(t - 4) - 1.0,
        close(t) / close(t - 8) - 1.0,
        vol8,
        (at(t).high - at(t).low) / close(t),
        vol_z,
        close(t) / ema - 1.0,
        (at(t).turnover + 1.0).ln(),
    ];
    f.iter().all(|v| v.is_finite()).then_some(f)
}

/// Cross-sectionally ranks each feature column to [0,1].
fn rank_columns(feats: &[Vec<f64>]) -> Vec<Vec<f64>> {
    (0..FEATURES.len())
        .map(|c| ranks01(&feats.iter().map(|r| r[c]).collect::<Vec<_>>()))
        .collect()
}

/// Builds every row whose features and target exist, for candles `LOOKBACK..len - HORIZON`.
pub fn build_rows(b: &Built) -> Rows {
    let m = &b.market;
    let mut out = Rows::default();
    for t in LOOKBACK..m.ts.len().saturating_sub(HORIZON) {
        let mut feats: Vec<Vec<f64>> = Vec::new();
        let mut target: Vec<f64> = Vec::new();
        for s in 0..m.symbols.len() {
            let bars = &m.bars[s];
            // the whole window up to the target must exist and be valid (as before)
            let window: Option<Vec<_>> = (t - LOOKBACK..=t + HORIZON).map(|k| bars[k]).collect();
            let Some(w) = window else { continue };
            if w.iter().any(|x| x.close.is_nan() || x.close <= 0.0) {
                continue;
            }
            let Some(f) = feature_vector(m, s, t) else {
                continue;
            };
            feats.push(f);
            target.push(w[LOOKBACK + HORIZON].close / w[LOOKBACK].close - 1.0);
        }
        if feats.len() < 50 {
            continue;
        }
        let n = feats.len() as f64;
        let mkt = target.iter().sum::<f64>() / n;
        let rel: Vec<f64> = target.iter().map(|r| r - mkt).collect();
        let mut sorted = rel.clone();
        sorted.sort_by(f64::total_cmp);
        let (lo, hi) = (quantile(&sorted, 0.01), quantile(&sorted, 0.99));
        let cols = rank_columns(&feats);
        for i in 0..feats.len() {
            out.candle.push(t);
            out.x.push(cols.iter().map(|c| c[i]).collect());
            out.y.push(rel[i].clamp(lo, hi));
            out.y_raw.push(rel[i]);
        }
    }
    out
}

/// The ML score of every symbol at every candle, for the strategy search's ML gate. The model is refitted at each of `starts` (candle indices) on rows
/// whose targets are fully in the past (`candle + HORIZON < start`) and scores the candles from that start to the next one, so no score ever uses a
/// future price. Candles before the first start get None. Returns `scores[symbol][candle]`.
#[allow(clippy::needless_range_loop)] // `t` is a candle number used for both the bars and the output column
pub fn ml_scores(
    b: &Built,
    rows: &Rows,
    starts: &[usize],
    trainer: Trainer,
) -> Vec<Vec<Option<f64>>> {
    let m = &b.market;
    let mut out = vec![vec![None; m.ts.len()]; m.symbols.len()];
    let mut starts: Vec<usize> = starts.to_vec();
    starts.sort_unstable();
    starts.dedup();
    for (k, &start) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(m.ts.len());
        let train: Vec<usize> = (0..rows.candle.len())
            .filter(|&i| rows.candle[i] + HORIZON < start)
            .collect();
        if train.len() < 1000 {
            continue;
        }
        let tx: Vec<Vec<f64>> = train.iter().map(|&i| rows.x[i].clone()).collect();
        let ty: Vec<f64> = train.iter().map(|&i| rows.y[i]).collect();
        let Some(model) = trainer(&tx, &ty) else {
            continue;
        };
        for t in start..end.min(m.ts.len()) {
            let have: Vec<(usize, Vec<f64>)> = (0..m.symbols.len())
                .filter_map(|s| feature_vector(m, s, t).map(|f| (s, f)))
                .collect();
            if have.len() < 50 {
                continue;
            }
            let cols = rank_columns(&have.iter().map(|x| x.1.clone()).collect::<Vec<_>>());
            for (i, (s, _)) in have.iter().enumerate() {
                let x: Vec<f64> = cols.iter().map(|c| c[i]).collect();
                out[*s][t] = Some(model(&x));
            }
        }
    }
    out
}

/// One period's judgement.
#[derive(Debug, Clone)]
pub struct PeriodResult {
    /// Which learner produced it: "gbdt" or "mlp".
    pub model: &'static str,
    pub range: Range<usize>,
    pub train_rows: usize,
    pub test_candles: usize,
    pub ic_mean: f64,
    /// t-statistic of the per-candle ICs, using every `HORIZON`-th candle so targets do not overlap.
    pub ic_t: f64,
    /// Mean return of the top tenth by prediction minus the bottom tenth, in percent.
    pub spread_pct: f64,
    pub best_baseline: (&'static str, f64),
    pub null_mean: f64,
    pub null_sd: f64,
}

impl PeriodResult {
    /// Positive, significant, better than the best single feature and clearly above the null.
    pub fn has_signal(&self) -> bool {
        self.ic_mean > 0.0
            && self.ic_t >= 2.0
            && self.ic_mean > self.best_baseline.1.abs()
            && self.ic_mean > self.null_mean + 2.0 * self.null_sd
    }
}

struct Scored {
    ic_by_candle: Vec<(usize, f64)>,
    spread: f64,
}

fn score_model(rows: &Rows, test: &[usize], predict: &dyn Fn(&[f64]) -> f64) -> Scored {
    let mut by_candle: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for &i in test {
        by_candle.entry(rows.candle[i]).or_default().push(i);
    }
    let (mut ics, mut spreads) = (Vec::new(), Vec::new());
    for (t, idx) in by_candle {
        let pred: Vec<f64> = idx.iter().map(|&i| predict(&rows.x[i])).collect();
        let actual: Vec<f64> = idx.iter().map(|&i| rows.y_raw[i]).collect();
        if let Some(ic) = spearman(&pred, &actual) {
            ics.push((t, ic));
        }
        let mut order: Vec<usize> = (0..idx.len()).collect();
        order.sort_by(|&a, &b| pred[a].total_cmp(&pred[b]));
        let k = (idx.len() / 10).max(1);
        let mean = |s: &[usize]| s.iter().map(|&j| actual[j]).sum::<f64>() / s.len() as f64;
        spreads.push((mean(&order[order.len() - k..]) - mean(&order[..k])) * 100.0);
    }
    Scored {
        ic_by_candle: ics,
        spread: spreads.iter().sum::<f64>() / spreads.len().max(1) as f64,
    }
}

fn mean_and_t(ics: &[(usize, f64)]) -> (f64, f64) {
    let mean = ics.iter().map(|x| x.1).sum::<f64>() / ics.len().max(1) as f64;
    let sub: Vec<f64> = ics.iter().step_by(HORIZON).map(|x| x.1).collect();
    if sub.len() < 3 {
        return (mean, 0.0);
    }
    let sm = sub.iter().sum::<f64>() / sub.len() as f64;
    let sd = (sub.iter().map(|v| (v - sm).powi(2)).sum::<f64>() / (sub.len() - 1) as f64).sqrt();
    (
        mean,
        if sd > 0.0 {
            sm / (sd / (sub.len() as f64).sqrt())
        } else {
            0.0
        },
    )
}

/// Shuffles targets within each candle (seeded), keeping features: the no-signal control.
fn shuffled_targets(rows: &Rows, train: &[usize], seed: u64) -> Vec<f64> {
    let mut s = seed | 1;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut by_candle: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for (pos, &i) in train.iter().enumerate() {
        by_candle.entry(rows.candle[i]).or_default().push(pos);
    }
    let mut y: Vec<f64> = train.iter().map(|&i| rows.y[i]).collect();
    for positions in by_candle.values() {
        let vals: Vec<f64> = positions.iter().map(|&p| y[p]).collect();
        let mut order: Vec<usize> = (0..vals.len()).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, (next() % (i as u64 + 1)) as usize);
        }
        for (k, &p) in positions.iter().enumerate() {
            y[p] = vals[order[k]];
        }
    }
    y
}

/// A fitted model: features in, score out.
pub type Predictor = Box<dyn Fn(&[f64]) -> f64>;

/// Fits a model on (features, targets); None when it cannot (too little data).
pub type Trainer<'a> = &'a dyn Fn(&[Vec<f64>], &[f64]) -> Option<Predictor>;

/// ThomasAi's gradient-boosted trees.
pub fn gbdt_trainer(params: Params) -> impl Fn(&[Vec<f64>], &[f64]) -> Option<Predictor> {
    move |x, y| {
        gbdt::train(x, y, params).map(|m| Box::new(move |v: &[f64]| m.predict(v)) as Predictor)
    }
}

/// The bot's own small neural network (`moomoo_bot::engine::tensor::Mlp`, a bounded MLP the bot keeps for research), judged by the same test as the trees.
pub fn mlp_trainer(
    seed: u64,
    hidden: usize,
    epochs: usize,
    lr: f32,
) -> impl Fn(&[Vec<f64>], &[f64]) -> Option<Predictor> {
    use moomoo_bot::engine::tensor::{Mlp, Tensor2};
    move |x, y| {
        let rows: Vec<Vec<f32>> = x
            .iter()
            .map(|r| r.iter().map(|v| *v as f32).collect())
            .collect();
        let y32: Vec<f32> = y.iter().map(|v| *v as f32).collect();
        let mut m = Mlp::new(x.first()?.len(), hidden, seed).ok()?;
        m.fit(&Tensor2::from_rows(rows).ok()?, &y32, epochs, lr)
            .ok()?;
        Some(Box::new(move |v: &[f64]| {
            let v32: Vec<f32> = v.iter().map(|a| *a as f32).collect();
            m.predict(&v32).unwrap_or(0.0) as f64
        }) as Predictor)
    }
}

/// The bot's network with its settings CHOSEN, not fixed: hidden size, epochs and learning rate come from a small enumerated grid, picked by rank
/// correlation on the most recent fifth of the training rows (rows are ordered by candle) and then refitted on all of them. The test period is never used.
pub fn mlp_tuned_trainer(seed: u64) -> impl Fn(&[Vec<f64>], &[f64]) -> Option<Predictor> {
    move |x, y| {
        let n = x.len();
        if n < 500 {
            return None;
        }
        let cut = n * 4 / 5;
        let mut best: Option<((usize, usize, f32), f64)> = None;
        for hidden in [8, 32] {
            for epochs in [10, 30] {
                for lr in [0.01_f32, 0.03] {
                    let Some(m) = mlp_trainer(seed, hidden, epochs, lr)(&x[..cut], &y[..cut])
                    else {
                        continue;
                    };
                    let pred: Vec<f64> = x[cut..].iter().map(|r| m(r)).collect();
                    if let Some(ic) = spearman(&pred, &y[cut..])
                        && best.is_none_or(|b| ic > b.1)
                    {
                        best = Some(((hidden, epochs, lr), ic));
                    }
                }
            }
        }
        let ((h, e, lr), _) = best?;
        mlp_trainer(seed, h, e, lr)(x, y)
    }
}

/// Judges the gradient-boosted learner on `period` (kept for the existing callers).
pub fn evaluate_period(
    rows: &Rows,
    period: Range<usize>,
    params: Params,
    null_runs: usize,
) -> Result<Option<PeriodResult>> {
    evaluate_with(rows, period, "gbdt", &gbdt_trainer(params), null_runs)
}

/// Judges any learner on `period` (candle range): trains on candles before it, tests inside it, compares with the best single feature and with the same
/// learner trained on shuffled targets.
pub fn evaluate_with(
    rows: &Rows,
    period: Range<usize>,
    model_name: &'static str,
    trainer: Trainer,
    null_runs: usize,
) -> Result<Option<PeriodResult>> {
    let train: Vec<usize> = (0..rows.candle.len())
        .filter(|&i| rows.candle[i] + HORIZON < period.start)
        .collect();
    let test: Vec<usize> = (0..rows.candle.len())
        .filter(|&i| {
            period.contains(&rows.candle[i]) && rows.candle[i] + HORIZON < period.end + HORIZON
        })
        .collect();
    if train.len() < 1000 || test.is_empty() {
        return Ok(None);
    }
    let tx: Vec<Vec<f64>> = train.iter().map(|&i| rows.x[i].clone()).collect();
    let ty: Vec<f64> = train.iter().map(|&i| rows.y[i]).collect();
    let Some(model) = trainer(&tx, &ty) else {
        return Ok(None);
    };
    let scored = score_model(rows, &test, &|x| model(x));
    let (ic_mean, ic_t) = mean_and_t(&scored.ic_by_candle);
    // single-feature baselines, signed as they are (the best |IC| is the bar to beat)
    let mut best = ("none", 0.0_f64);
    for (c, name) in FEATURES.iter().enumerate() {
        let (m, _) = mean_and_t(&score_model(rows, &test, &|x| x[c]).ic_by_candle);
        if m.abs() > best.1.abs() {
            best = (name, m);
        }
    }
    let mut nulls = Vec::new();
    for r in 0..null_runs {
        let y = shuffled_targets(rows, &train, 1000 + r as u64);
        if let Some(nm) = trainer(&tx, &y) {
            nulls.push(mean_and_t(&score_model(rows, &test, &|x| nm(x)).ic_by_candle).0);
        }
    }
    let nmean = nulls.iter().sum::<f64>() / nulls.len().max(1) as f64;
    let nsd = (nulls.iter().map(|v| (v - nmean).powi(2)).sum::<f64>()
        / nulls.len().max(2).saturating_sub(1) as f64)
        .sqrt();
    Ok(Some(PeriodResult {
        model: model_name,
        range: period,
        train_rows: train.len(),
        test_candles: scored.ic_by_candle.len(),
        ic_mean,
        ic_t,
        spread_pct: scored.spread,
        best_baseline: best,
        null_mean: nmean,
        null_sd: nsd,
    }))
}

/// The learner's periods: the same 14-session grid as the search, anchored at the first scored session of the current window.
/// Returns (period start open time in ms, candle range).
pub fn periods(
    b: &Built,
    calendar: &[(i64, i64)],
    first_scored: usize,
    candles_per_session: usize,
) -> Vec<(i64, Range<usize>)> {
    use thomas_core::config::{PERIOD_SESSIONS, PERIOD_STEP_SESSIONS, grid_starts};
    if candles_per_session == 0
        || b.market.ts.is_empty()
        || first_scored >= b.market.ts.len()
        || b.market.ts.len() < candles_per_session
    {
        return Vec::new();
    }
    let opens: Vec<i64> = calendar.iter().map(|s| s.0).collect();
    let first = first_scored.div_ceil(candles_per_session) * candles_per_session;
    let Some(first_wall) = b.market.wall.get(first) else {
        return Vec::new();
    };
    let Some(last_wall) = b.market.wall.get(b.market.ts.len() - candles_per_session) else {
        return Vec::new();
    };
    let (Ok(span_first), Ok(last)) = (
        opens.binary_search(first_wall),
        opens.binary_search(last_wall),
    ) else {
        return Vec::new();
    };
    grid_starts(
        span_first,
        span_first,
        last + 1,
        PERIOD_SESSIONS,
        PERIOD_STEP_SESSIONS,
    )
    .into_iter()
    .filter_map(|s| {
        let start = b.market.wall.binary_search(&opens[s]).ok()?;
        Some((
            opens[s],
            start..start + PERIOD_SESSIONS * candles_per_session,
        ))
    })
    .collect()
}

/// Stores one period's judgement (replacing an earlier one for the same period).
pub fn record(
    conn: &rusqlite::Connection,
    start_ms: i64,
    r: &PeriodResult,
    now_ms: i64,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO learner_periods(period_start_ms, train_rows, test_candles, ic_mean, ic_t, spread_pct, best_baseline, best_baseline_ic, null_mean, null_sd, has_signal, computed_ms, model) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![
            start_ms,
            r.train_rows as i64,
            r.test_candles as i64,
            r.ic_mean,
            r.ic_t,
            r.spread_pct,
            r.best_baseline.0,
            r.best_baseline.1,
            r.null_mean,
            r.null_sd,
            r.has_signal(),
            now_ms,
            r.model
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spearman_is_one_for_monotone_minus_one_for_reversed_and_none_for_constant() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((spearman(&a, &[10.0, 20.0, 35.0, 36.0, 90.0]).unwrap() - 1.0).abs() < 1e-12);
        assert!((spearman(&a, &[5.0, 4.0, 3.0, 2.0, 1.0]).unwrap() + 1.0).abs() < 1e-12);
        assert!(spearman(&a, &[1.0; 5]).is_none());
        assert!(spearman(&a, &[1.0, 2.0]).is_none());
    }

    #[test]
    fn feature_vector_rejects_missing_symbol_or_candle_instead_of_panicking() {
        let m = moomoo_bot::engine::Market::default();
        assert!(feature_vector(&m, 0, LOOKBACK).is_none());
    }

    #[test]
    fn ranks_are_in_unit_interval_with_ties_averaged() {
        assert_eq!(ranks01(&[10.0, 30.0, 20.0]), vec![0.0, 1.0, 0.5]);
        assert_eq!(ranks01(&[1.0, 1.0, 3.0]), vec![0.25, 0.25, 1.0]);
    }

    #[test]
    fn a_planted_interaction_is_found_and_pure_noise_is_not() {
        // 28 trainable candles beyond the target horizon, then six horizons tested (the t-statistic uses one candle per horizon), x 120 symbols; in the planted case features 0 and 1 together predict the target
        let mut s = 7_u64;
        let mut rnd = move || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        for planted in [true, false] {
            let mut rows = Rows::default();
            for t in 0..(HORIZON + 28 + 6 * HORIZON + 2) {
                for _ in 0..120 {
                    let x: Vec<f64> = (0..FEATURES.len()).map(|_| rnd()).collect();
                    let noise = rnd() - 0.5;
                    let y = if planted {
                        // an interaction no single feature shows: up when features 0 and 1 are on the same side of the middle
                        (if (x[0] > 0.5) == (x[1] > 0.5) {
                            0.3
                        } else {
                            -0.3
                        }) + noise
                    } else {
                        noise
                    };
                    rows.candle.push(t);
                    rows.x.push(x);
                    rows.y.push(y);
                    rows.y_raw.push(y);
                }
            }
            let r = evaluate_period(
                &rows,
                (HORIZON + 28)..(HORIZON + 28 + 6 * HORIZON),
                Params {
                    trees: 60,
                    ..Params::default()
                },
                3,
            )
            .unwrap()
            .unwrap();
            if planted {
                assert!(
                    r.ic_mean > 0.3 && r.has_signal(),
                    "planted interaction must be found and must beat every single feature: {r:?}"
                );
            } else {
                assert!(
                    r.ic_mean.abs() < 0.1 && !r.has_signal(),
                    "noise must not look like signal: {r:?}"
                );
            }
        }
    }

    #[test]
    fn the_bots_neural_net_is_judged_by_the_same_test_it_finds_a_planted_sum_and_not_noise() {
        let mut st = 11_u64;
        let mut rnd = move || {
            st = st
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (st >> 11) as f64 / (1u64 << 53) as f64
        };
        for planted in [true, false] {
            let mut rows = Rows::default();
            for t in 0..(HORIZON + 28 + 6 * HORIZON + 2) {
                for _ in 0..120 {
                    let x: Vec<f64> = (0..FEATURES.len()).map(|_| rnd()).collect();
                    let noise = rnd() - 0.5;
                    // a sum of two features: each alone shows part of it, together they show all of it
                    let y = if planted {
                        (x[0] + x[1] - 1.0) + 0.3 * noise
                    } else {
                        noise
                    };
                    rows.candle.push(t);
                    rows.x.push(x);
                    rows.y.push(y);
                    rows.y_raw.push(y);
                }
            }
            let r = evaluate_with(
                &rows,
                (HORIZON + 28)..(HORIZON + 28 + 6 * HORIZON),
                "mlp",
                &mlp_trainer(7, 16, 30, 0.02),
                3,
            )
            .unwrap()
            .unwrap();
            assert_eq!(r.model, "mlp");
            if planted {
                assert!(
                    r.ic_mean > 0.5 && r.ic_mean > r.best_baseline.1.abs(),
                    "the net must find the sum and beat each single feature: {r:?}"
                );
            } else {
                assert!(
                    r.ic_mean.abs() < 0.1 && !r.has_signal(),
                    "noise must not look like signal: {r:?}"
                );
            }
        }
    }
}

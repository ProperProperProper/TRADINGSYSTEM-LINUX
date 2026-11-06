//! Version 2 of the candle machine learning ("as advanced as possible", owner 2026-10-13). Version 1 (`mlcandle.rs`) stays untouched: its ridge model is the one
//! FROZEN on 2026-10-13 for the forward test. Everything here is judged the same strict way (rolling windows, every prediction out of sample, whole-week
//! resampling) and ALL variants are reported, not only the best, because every extra variant is another chance to look good by luck.
//!
//! Added over v1:
//!   * 20 more features: cross-sectional ranks of more signals, returns relative to the market, volatility-scaled returns, return since the day's open;
//!   * rank-normalised labels for training (robust to outliers; scoring always uses the raw excess return);
//!   * ridge strength chosen by validation on the last quarter of the TRAINING period only;
//!   * trees on the rank label with early stopping, and an ensemble (per-moment standardised average of ridge and trees);
//!   * a cost-aware long-only portfolio test, which is how the bot would actually use a ranking: each day at 10:00 New York, buy the top names, hold one session.

use crate::candles::Bar;
use crate::mlcandle::{evaluate, rolling, to_trades, Eval, Ridge, Sample, FEATURES};
use crate::mltrade::{Gbdt, GbdtParams};
use std::collections::BTreeMap;

pub const EXTRA: [&str; 20] = ["rk_ret_1", "rk_ret_13", "rk_ret_65", "rk_ret_130", "rk_dist_ma26", "rk_dist_ma130", "rk_range_pos", "rk_vol_ratio_1", "rk_atr", "rk_vol_ratio_26_130",
    "res_ret_26", "res_ret_130", "norm_ret_4", "norm_ret_26", "norm_ret_130", "rk_norm_ret_26", "rk_res_ret_26", "since_open", "rk_since_open", "abs_since_open"];

fn idx(name: &str) -> usize { FEATURES.iter().position(|f| *f == name).expect("feature") }

/// Return since the day's first bar for the sample's decision bar, per (symbol, ts). The v1 samples do not carry the symbol, so it is rebuilt from the bars.
fn since_open(series: &BTreeMap<String, Vec<Bar>>) -> BTreeMap<(usize, i64), f64> {
    let mut out = BTreeMap::new();
    for (si, bars) in series.values().enumerate() {
        let (mut day, mut open) = (i64::MIN, 0.0);
        for b in bars {
            let d = (b.ts + crate::mlcandle::et_offset_ms(b.ts)).div_euclid(86_400_000);
            if d != day { day = d; open = b.open; }
            out.insert((si, b.ts), (b.close / open).ln());
        }
    }
    out
}

/// Version-1 samples (23 features) plus the 20 extra ones. `series` must be the same map the samples were built from.
/// Samples of one moment are consecutive in `build`'s output, in symbol order of the moment; the symbol is recovered by matching the moment's cross-section.
pub fn extend(series: &BTreeMap<String, Vec<Bar>>, base: &[Sample]) -> Vec<Sample> {
    let so = since_open(series);
    let mut out = Vec::with_capacity(base.len());
    let mut i = 0;
    while i < base.len() {
        let mut j = i; while j < base.len() && base[j].ts == base[i].ts { j += 1; }
        let group = &base[i..j];
        let syms: Vec<usize> = group.iter().map(|s| s.sym).collect();
        let n = group.len() as f64;
        let rank = |f: &dyn Fn(usize) -> f64| -> Vec<f64> {
            let mut o: Vec<usize> = (0..group.len()).collect();
            o.sort_by(|&a, &b| f(a).total_cmp(&f(b)));
            let mut p = vec![0.0; group.len()];
            for (pos, &k) in o.iter().enumerate() { p[k] = pos as f64 / (n - 1.0); }
            p
        };
        let g = |k: usize, name: &str| group[k].x[idx(name)];
        let sqrt26 = 26f64.sqrt(); let sqrt130 = 130f64.sqrt();
        let n26: Vec<f64> = (0..group.len()).map(|k| g(k, "ret_26") / (g(k, "vol_26") * sqrt26).max(1e-9)).collect();
        let r26: Vec<f64> = (0..group.len()).map(|k| g(k, "ret_26") - g(k, "mkt_ret_26")).collect();
        let so_v: Vec<f64> = syms.iter().map(|s| so.get(&(*s, group[0].ts)).copied().unwrap_or(0.0)).collect();
        let ranks: Vec<Vec<f64>> = ["ret_1", "ret_13", "ret_65", "ret_130", "dist_ma26", "dist_ma130", "range_pos_52", "vol_ratio_1", "atr_14", "vol_ratio_26_130"].iter().map(|f| rank(&|k| g(k, f))).collect();
        let (rk_n26, rk_r26, rk_so) = (rank(&|k| n26[k]), rank(&|k| r26[k]), rank(&|k| so_v[k]));
        for k in 0..group.len() {
            let mut x = group[k].x.clone();
            for r in &ranks { x.push(r[k]); }
            x.push(r26[k]);
            x.push(g(k, "ret_130") - g(k, "mkt_ret_130"));
            x.push(g(k, "ret_4") / (g(k, "vol_26") * 2.0).max(1e-9));
            x.push(n26[k]);
            x.push(g(k, "ret_130") / (g(k, "vol_130") * sqrt130).max(1e-9));
            x.push(rk_n26[k]); x.push(rk_r26[k]);
            x.push(so_v[k]); x.push(rk_so[k]); x.push(so_v[k].abs());
            out.push(Sample { x, ..group[k].clone() });
        }
        i = j;
    }
    out
}

/// Cross-sectional rank of the raw excess return within each moment, centred (-0.5..0.5): the training label of the rank models.
pub fn rank_labels(s: &[Sample]) -> Vec<Sample> {
    let mut out = s.to_vec();
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in s.iter().enumerate() { by.entry(x.ts).or_default().push(i); }
    for v in by.values() {
        let mut o = v.clone(); o.sort_by(|&a, &b| s[a].excess.total_cmp(&s[b].excess));
        for (pos, &k) in o.iter().enumerate() { out[k].excess = pos as f64 / (o.len().max(2) - 1) as f64 - 0.5; }
    }
    out
}

fn zscore_by_moment(s: &[Sample], p: &[f64]) -> Vec<f64> {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in s.iter().enumerate() { by.entry(x.ts).or_default().push(i); }
    let mut out = vec![0.0; p.len()];
    for v in by.values() {
        let m = v.iter().map(|&i| p[i]).sum::<f64>() / v.len() as f64;
        let sd = (v.iter().map(|&i| (p[i] - m).powi(2)).sum::<f64>() / v.len() as f64).sqrt().max(1e-12);
        for &i in v { out[i] = (p[i] - m) / sd; }
    }
    out
}

/// Ridge with its strength chosen on the last quarter of the training period (never on the test windows).
pub fn ridge_tuned(train: &[Sample]) -> Ridge {
    let mut ts: Vec<i64> = train.iter().map(|s| s.ts).collect(); ts.sort(); ts.dedup();
    let cut = ts[ts.len() * 3 / 4];
    let (fit, val): (Vec<Sample>, Vec<Sample>) = (train.iter().filter(|s| s.ts < cut - 3 * 86_400_000).cloned().collect(), train.iter().filter(|s| s.ts >= cut).cloned().collect());
    if fit.len() < 2000 || val.len() < 500 { return Ridge::fit(train, 100.0); }
    let mut best = (f64::MIN, 100.0);
    for a in [10.0, 100.0, 1000.0, 10_000.0, 100_000.0] {
        let r = Ridge::fit(&fit, a);
        let ic = evaluate(&val, &val.iter().map(|s| r.predict(s)).collect::<Vec<_>>(), 5).ic;
        if ic > best.0 { best = (ic, a); }
    }
    Ridge::fit(train, best.1)
}

pub fn trees_on_rank(train: &[Sample]) -> Gbdt {
    let rl = rank_labels(train);
    let t = to_trades(&rl);
    Gbdt::fit_early_stopped(&t, GbdtParams { min_leaf: 800, depth: 3, lr: 0.05, ..GbdtParams::default() }, true).0
}

type Predictor = Box<dyn Fn(&[Sample]) -> Vec<f64>>;
/// A model variant: fits on the training samples and returns a function that scores a whole set of samples (moments need scoring together for the ensemble).
pub type Variant = (&'static str, Box<dyn Fn(&[Sample]) -> Predictor>);

pub fn variants() -> Vec<Variant> {
    let nbase = FEATURES.len();
    let cut_v1 = move |s: &[Sample]| -> Vec<Sample> { s.iter().map(|x| Sample { x: x.x[..nbase].to_vec(), ..x.clone() }).collect() };
    vec![
        ("A ridge v1 features (the frozen model)", Box::new(move |tr| { let r = Ridge::fit(&cut_v1(tr), 100.0); Box::new(move |s| cut_v1(s).iter().map(|x| r.predict(x)).collect()) })),
        ("B ridge v2 features", Box::new(|tr| { let r = Ridge::fit(tr, 100.0); Box::new(move |s| s.iter().map(|x| r.predict(x)).collect()) })),
        ("C ridge v2, rank label, tuned strength", Box::new(|tr| { let r = ridge_tuned(&rank_labels(tr)); Box::new(move |s| s.iter().map(|x| r.predict(x)).collect()) })),
        ("D trees v2, rank label", Box::new(|tr| { let g = trees_on_rank(tr); Box::new(move |s| to_trades(s).iter().map(|t| g.predict(t)).collect()) })),
        ("E ensemble of C and D", Box::new(|tr| {
            let r = ridge_tuned(&rank_labels(tr)); let g = trees_on_rank(tr);
            Box::new(move |s| {
                let a = zscore_by_moment(s, &s.iter().map(|x| r.predict(x)).collect::<Vec<_>>());
                let b = zscore_by_moment(s, &to_trades(s).iter().map(|t| g.predict(t)).collect::<Vec<_>>());
                a.iter().zip(&b).map(|(x, y)| 0.5 * (x + y)).collect()
            })
        })),
    ]
}

/// Rolling-origin predictions of a variant (4-week windows after the first 100 days, 3-day purge).
pub fn rolled(all: &[Sample], v: &Variant) -> (Vec<Sample>, Vec<f64>) {
    // `rolling` wants a per-sample scorer; scoring a window as a whole is needed for the ensemble, so score by calling the set-scorer on the window once.
    let t0 = all.iter().map(|s| s.ts).min().unwrap_or(0); let t1 = all.iter().map(|s| s.ts).max().unwrap_or(0);
    let day = 86_400_000i64;
    let (mut out, mut pred) = (vec![], vec![]);
    let mut start = t0 + 100 * day;
    while start < t1 {
        let end = start + 28 * day;
        let train: Vec<Sample> = all.iter().filter(|s| s.ts <= start - 3 * day).cloned().collect();
        let test: Vec<Sample> = all.iter().filter(|s| s.ts >= start && s.ts < end).cloned().collect();
        if train.len() > 5000 && test.len() > 500 { let p = (v.1)(&train)(&test); out.extend(test); pred.extend(p); }
        start = end;
    }
    let _ = rolling::<fn(&[Sample]) -> Box<dyn Fn(&Sample) -> f64>>;
    (out, pred)
}

#[derive(Debug, Clone)]
pub struct Portfolio { pub days: usize, pub gross_bps: f64, pub net_bps: f64, pub net_lo: f64, pub net_hi: f64, pub sharpe: f64, pub hit: f64 }

/// Long-only portfolio test: once a day at 10:00 New York (decision bar 2 of the session), hold the `k` best-ranked names for one session. Excess return over the
/// equal-weight average of all names, minus `cost_bps` per side (full turnover assumed, so 2 x cost per day). Interval by resampling whole weeks.
pub fn portfolio(test: &[Sample], pred: &[f64], k: usize, cost_bps: f64) -> Portfolio {
    let bi = idx("bar_in_day");
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in test.iter().enumerate() { if (x.x[bi] - 2.0).abs() < 1e-9 { by.entry(x.ts).or_default().push(i); } }
    let mut days: Vec<(i64, f64)> = vec![]; // (week, gross excess)
    for v in by.values().filter(|v| v.len() >= crate::mlcandle::MIN_CROSS) {
        let mut o = v.clone(); o.sort_by(|&a, &b| pred[b].total_cmp(&pred[a]));
        days.push((test[v[0]].week, o[..k.min(o.len())].iter().map(|&i| test[i].excess).sum::<f64>() / k.min(o.len()) as f64));
    }
    if days.is_empty() { return Portfolio { days: 0, gross_bps: 0.0, net_bps: 0.0, net_lo: 0.0, net_hi: 0.0, sharpe: 0.0, hit: 0.0 }; }
    let cost = 2.0 * cost_bps * 1e-4;
    let net: Vec<f64> = days.iter().map(|d| d.1 - cost).collect();
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let sd = (net.iter().map(|x| (x - mean(&net)).powi(2)).sum::<f64>() / net.len() as f64).sqrt().max(1e-12);
    let mut weeks: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for (d, n) in days.iter().zip(&net) { weeks.entry(d.0).or_default().push(*n); }
    let wv: Vec<&Vec<f64>> = weeks.values().collect();
    let mut st = 12345u64;
    let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; st };
    let mut means: Vec<f64> = (0..2000).map(|_| { let (mut s, mut c) = (0.0, 0usize); for _ in 0..wv.len() { let w = wv[(rnd() % wv.len() as u64) as usize]; s += w.iter().sum::<f64>(); c += w.len(); } s / c.max(1) as f64 }).collect();
    means.sort_by(f64::total_cmp);
    Portfolio { days: days.len(), gross_bps: mean(&days.iter().map(|d| d.1).collect::<Vec<_>>()) * 1e4, net_bps: mean(&net) * 1e4, net_lo: means[50] * 1e4, net_hi: means[1950] * 1e4,
        sharpe: mean(&net) / sd * 252f64.sqrt(), hit: net.iter().filter(|x| **x > 0.0).count() as f64 / net.len() as f64 }
}

pub fn fmt_eval(name: &str, e: &Eval) -> String { format!("  {name:<42} IC {:+.4} [{:+.4},{:+.4}] | spread {:+.1} bps [{:+.1},{:+.1}] | {} wk\n", e.ic, e.ic_lo, e.ic_hi, e.spread_bps, e.spread_lo, e.spread_hi, e.weeks) }

/// The whole v2 experiment on the candle store: every variant on rolling windows, a shuffled-label control for the best one, and the cost-aware portfolio.
pub fn report(series: &BTreeMap<String, Vec<Bar>>) -> String {
    let base = crate::mlcandle::build(series, 4);
    if base.len() < 5000 { return "too little candle data".into(); }
    let all = extend(series, &base);
    let mut t = format!("v2: {} features ({} v1 + {} new), {} samples; {} variants tried (all reported)\n", all[0].x.len(), FEATURES.len(), EXTRA.len(), all.len(), variants().len());
    let mut best: (f64, usize) = (f64::MIN, 0);
    let vs = variants();
    let mut results = vec![];
    for (i, v) in vs.iter().enumerate() {
        let (s, p) = rolled(&all, v);
        let e = evaluate(&s, &p, 51);
        t += &fmt_eval(v.0, &e);
        if e.ic > best.0 { best = (e.ic, i); }
        results.push((s, p));
    }
    t += "long-only portfolio, 10:00 New York each day, hold one session, 5 bps per side (all variants):\n";
    for (i, v) in vs.iter().enumerate() {
        for k in [5usize, 19] {
            let p = portfolio(&results[i].0, &results[i].1, k, 5.0);
            t += &format!("  {:<42} top {k:<2}: gross {:+.1} bps/day, NET {:+.1} bps/day [{:+.1},{:+.1}], Sharpe {:+.2}, up days {:.0}%, {} days\n", v.0, p.gross_bps, p.net_bps, p.net_lo, p.net_hi, p.sharpe, p.hit * 100.0, p.days);
        }
    }
    let shuffled = crate::mlcandle::shuffle_within_moment(&all, 91);
    let (s, p) = rolled(&shuffled, &vs[best.1]);
    t += &fmt_eval(&format!("control: best variant ({}) on shuffled labels", vs[best.1].0.split(' ').next().unwrap_or("")), &evaluate(&s, &p, 52));
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(n_sym: usize, bars: usize, seed: u64) -> BTreeMap<String, Vec<Bar>> {
        let mut st = seed | 1;
        let mut rnd = move || { st ^= st << 13; st ^= st >> 7; st ^= st << 17; (st >> 11) as f64 / (1u64 << 53) as f64 - 0.5 };
        let start = 1_775_000_000_000i64 / 86_400_000 * 86_400_000 + 13 * 3_600_000 + 30 * 60_000;
        (0..n_sym).map(|s| {
            let mut p = 50.0 + s as f64; let mut v = vec![];
            for i in 0..bars {
                let day = i / 26; let slot = (i % 26) as i64; let wk = day / 5; let dow = day % 5;
                let ts = start + (wk as i64 * 7 + dow as i64) * 86_400_000 + slot * 900_000;
                let o = p; p *= 1.0 + rnd() * 0.004; v.push(Bar { ts, open: o, high: o.max(p) * 1.001, low: o.min(p) * 0.999, close: p, volume: 1000.0 * (1.0 + rnd() + 0.6) });
            }
            (format!("S{s:02}"), v)
        }).collect()
    }

    #[test]
    fn extension_adds_the_extra_features_and_recovers_every_symbol() {
        let m = market(24, 500, 3);
        let base = crate::mlcandle::build(&m, 4);
        let ext = extend(&m, &base);
        assert_eq!(ext.len(), base.len());
        assert!(ext.iter().all(|s| s.x.len() == FEATURES.len() + EXTRA.len() && s.x.iter().all(|v| v.is_finite())));
        // since_open of the first bar of a day is the open-to-close return of that bar, and ranks are in [0, 1]
        let so = FEATURES.len() + EXTRA.iter().position(|f| *f == "since_open").unwrap();
        let rk = FEATURES.len() + EXTRA.iter().position(|f| *f == "rk_since_open").unwrap();
        assert!(ext.iter().all(|s| (0.0..=1.0).contains(&s.x[rk])));
        // the symbol with the highest since_open at a moment has rank 1
        let ts = ext[0].ts; let g: Vec<&Sample> = ext.iter().filter(|s| s.ts == ts).collect();
        let hi = g.iter().max_by(|a, b| a.x[so].total_cmp(&b.x[so])).unwrap();
        assert!((hi.x[rk] - 1.0).abs() < 1e-9);
        // the label and time stamp are untouched
        assert!(base.iter().zip(&ext).all(|(a, b)| a.ts == b.ts && a.excess == b.excess));
    }

    #[test]
    fn rank_labels_are_centred_per_moment_and_keep_the_order() {
        let mk = |e: f64| Sample { ts: 0, week: 0, sym: 0, x: vec![], excess: e, raw: e };
        let s = vec![mk(0.5), mk(-0.2), mk(0.1), mk(9.0)];
        let r = rank_labels(&s);
        for (a, b) in r.iter().map(|x| x.excess).zip([1.0 / 6.0, -0.5, -1.0 / 6.0, 0.5]) { assert!((a - b).abs() < 1e-12); }
    }

    #[test]
    fn the_portfolio_earns_the_top_names_excess_minus_costs() {
        let bi = idx("bar_in_day");
        let mut x = vec![0.0; FEATURES.len()]; x[bi] = 2.0;
        let s: Vec<Sample> = (0..25).map(|i| Sample { ts: 1000, week: 0, sym: i, x: x.clone(), excess: (i as f64 - 12.0) / 1000.0, raw: (i as f64 - 12.0) / 1000.0 }).collect();
        let perfect: Vec<f64> = s.iter().map(|v| v.excess).collect();
        let p = portfolio(&s, &perfect, 5, 5.0);
        // top 5 of a perfect ranking: excess 0.008..0.012, mean 0.010 = 100 bps gross; cost 2 x 5 = 10 bps
        assert!((p.gross_bps - 100.0).abs() < 1e-6 && (p.net_bps - 90.0).abs() < 1e-6 && p.days == 1, "{p:?}");
        let flipped: Vec<f64> = perfect.iter().map(|v| -v).collect();
        assert!(portfolio(&s, &flipped, 5, 5.0).net_bps < 0.0);
    }

    #[test]
    fn a_planted_signal_is_found_by_the_ensemble_and_not_by_a_shuffled_label() {
        let m = market(60, 2600, 17);
        let base = crate::mlcandle::build(&m, 4);
        let all = extend(&m, &base);
        let j = idx("ret_26");
        let planted: Vec<Sample> = all.iter().map(|x| Sample { excess: -0.8 * x.x[j], ..x.clone() }).collect();
        let vs = variants();
        let (s, p) = rolled(&planted, &vs[4]);
        assert!(evaluate(&s, &p, 1).ic > 0.5, "the ensemble must find a planted reversal");
        let (s0, p0) = rolled(&crate::mlcandle::shuffle_within_moment(&all, 5), &vs[1]);
        assert!(evaluate(&s0, &p0, 1).ic.abs() < 0.08);
    }
}

//! Try different models and label horizons on the bot's top-20 universe, learn which work, report everything.
//! Clean data (SPY holdings, splits dropped), bot score features, every prediction out of sample in rolling 28-day windows.
//! `cargo run --release --example candle_models`.
use qwen_lab::mlcandle::{evaluate, to_trades, Ridge, Sample, FEATURES};
use qwen_lab::mltrade::{Gbdt, GbdtParams, Logistic};
use qwen_lab::{candles, mlbot, mlcandle as m, mlcandle2 as m2};
use std::collections::BTreeMap;

/// Re-centres the label inside the top-20 universe (the bot only chooses among those), and keeps those samples.
fn universe_only(s: &[Sample], nb: usize) -> Vec<Sample> {
    let top = mlbot::in_universe(s, nb);
    let mut by: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    for x in &top { let e = by.entry(x.ts).or_default(); e.0 += x.raw; e.1 += 1; }
    top.into_iter().filter(|x| by[&x.ts].1 >= 15).map(|x| { let mu = by[&x.ts].0 / by[&x.ts].1 as f64; Sample { excess: x.raw - mu, ..x } }).collect()
}

type Scorer = Box<dyn Fn(&[Sample]) -> Vec<f64>>;
type Fitter = Box<dyn Fn(&[Sample]) -> Scorer>;
fn rolled(all: &[Sample], fit: &dyn Fn(&[Sample]) -> Scorer) -> (Vec<Sample>, Vec<f64>) {
    let day = 86_400_000i64;
    let t0 = all.iter().map(|s| s.ts).min().unwrap(); let t1 = all.iter().map(|s| s.ts).max().unwrap();
    let (mut out, mut pred) = (vec![], vec![]);
    let mut start = t0 + 100 * day;
    while start < t1 {
        let end = start + 28 * day;
        let train: Vec<Sample> = all.iter().filter(|s| s.ts <= start - 5 * day).cloned().collect(); // 5-day purge covers the longest horizon
        let test: Vec<Sample> = all.iter().filter(|s| s.ts >= start && s.ts < end).cloned().collect();
        if train.len() > 3000 && test.len() > 400 { let p = fit(&train)(&test); out.extend(test); pred.extend(p); }
        start = end;
    }
    (out, pred)
}

fn main() -> anyhow::Result<()> {
    let db = rusqlite::Connection::open(format!("{}/Projects/qwen-lab-stocks/data/candles.sqlite3", std::env::var("HOME").unwrap_or_default()))?;
    let (series, _) = candles::prepare(&candles::load(&db)?, &candles::load_universe(&db));
    let nb = FEATURES.len();
    println!("{:<8} {:<34} {:>9} {:>20}   {:>10} {:>20}", "horizon", "model", "IC(own h)", "interval", "spread bps", "interval");
    for h in [26usize, 52, 130] {
        let all = universe_only(&mlbot::extend(&series, &m::build_h(&series, 4, h)), nb);
        // interactions: the stock's own signals scaled by the market state (SPY trend), the kind of regime dependence a linear model cannot see alone
        let inter = |s: &[Sample]| -> Vec<Sample> { mlbot::interactions(s) };
        let models: Vec<(&str, Fitter)> = vec![
            ("ridge (strength tuned)", Box::new(|tr| { let r = m2::ridge_tuned(tr); Box::new(move |s| s.iter().map(|x| r.predict(x)).collect()) })),
            ("ridge + market interactions", Box::new(move |tr| { let r = m2::ridge_tuned(&inter(tr)); Box::new(move |s| inter(s).iter().map(|x| r.predict(x)).collect()) })),
            ("logistic (above/below average)", Box::new(|tr| { let l = Logistic::fit(&to_trades(tr), 120, 0.3, 1e-4); Box::new(move |s| to_trades(s).iter().map(|t| l.predict(t)).collect()) })),
            ("trees on the return", Box::new(|tr| { let g = Gbdt::fit_early_stopped(&to_trades(tr), GbdtParams { min_leaf: 600, ..GbdtParams::default() }, true).0; Box::new(move |s| to_trades(s).iter().map(|t| g.predict(t)).collect()) })),
            ("trees on the rank label", Box::new(|tr| { let g = m2::trees_on_rank(tr); Box::new(move |s| to_trades(s).iter().map(|t| g.predict(t)).collect()) })),
            ("ensemble ridge + trees(rank)", Box::new(|tr| {
                let r = m2::ridge_tuned(tr); let g = m2::trees_on_rank(tr);
                Box::new(move |s| {
                    let z = |p: Vec<f64>| { let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new(); for (i, x) in s.iter().enumerate() { by.entry(x.ts).or_default().push(i); } let mut o = vec![0.0; p.len()]; for v in by.values() { let mu = v.iter().map(|&i| p[i]).sum::<f64>() / v.len() as f64; let sd = (v.iter().map(|&i| (p[i] - mu).powi(2)).sum::<f64>() / v.len() as f64).sqrt().max(1e-12); for &i in v { o[i] = (p[i] - mu) / sd; } } o };
                    let a = z(s.iter().map(|x| r.predict(x)).collect()); let b = z(to_trades(s).iter().map(|t| g.predict(t)).collect());
                    a.iter().zip(&b).map(|(x, y)| 0.5 * (x + y)).collect()
                })
            })),
        ];
        for (name, fit) in &models {
            let (s, p) = rolled(&all, fit.as_ref());
            let e = evaluate(&s, &p, 3);
            println!("{:<8} {:<34} {:>+9.4} [{:>+8.4},{:>+8.4}]   {:>+10.1} [{:>+7.1},{:>+7.1}]", format!("{h} bars"), name, e.ic, e.ic_lo, e.ic_hi, e.spread_bps, e.spread_lo, e.spread_hi);
        }
    }
    let _ = Ridge::fit;
    Ok(())
}

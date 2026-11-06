//! Do mature ML libraries find skill in the candles that the self-written trees missed? Same split, features, labels and scoring as `mlcandle::report`.
//! `cargo run --release --example candle_libs`.
use qwen_lab::{candles, mlcandle as m};
use smartcore::ensemble::random_forest_regressor::{RandomForestRegressor, RandomForestRegressorParameters};
use smartcore::linalg::basic::matrix::DenseMatrix;
use smartcore::linear::{elastic_net::{ElasticNet, ElasticNetParameters}, ridge_regression::{RidgeRegression, RidgeRegressionParameters}};

fn matrix(s: &[m::Sample]) -> DenseMatrix<f64> { DenseMatrix::from_2d_vec(&s.iter().map(|x| x.x.clone()).collect::<Vec<_>>()).unwrap() }
fn show(name: &str, e: &m::Eval) { println!("  {name:<28} rank IC {:+.4} [{:+.4},{:+.4}] | spread {:+.1} bps [{:+.1},{:+.1}]", e.ic, e.ic_lo, e.ic_hi, e.spread_bps, e.spread_lo, e.spread_hi); }

fn main() -> anyhow::Result<()> {
    let path = format!("{}/Projects/qwen-lab-stocks/data/candles.sqlite3", std::env::var("HOME").unwrap_or_default());
    let series = candles::load(&rusqlite::Connection::open(path)?)?;
    let all = m::build(&series, 4);
    let (train, test) = m::split(&all, 0.25, 3);
    // standardise with the TRAINING mean/sd only
    let d = m::FEATURES.len();
    let mu: Vec<f64> = (0..d).map(|j| train.iter().map(|s| s.x[j]).sum::<f64>() / train.len() as f64).collect();
    let sd: Vec<f64> = (0..d).map(|j| (train.iter().map(|s| (s.x[j] - mu[j]).powi(2)).sum::<f64>() / train.len() as f64).sqrt().max(1e-12)).collect();
    let z = |s: &[m::Sample]| -> Vec<m::Sample> { s.iter().map(|x| m::Sample { x: x.x.iter().enumerate().map(|(j, v)| ((v - mu[j]) / sd[j]).clamp(-5.0, 5.0)).collect(), ..x.clone() }).collect() };
    let (tr, te) = (z(&train), z(&test));
    let ytr: Vec<f64> = tr.iter().map(|s| s.excess * 100.0).collect();
    let (xtr, xte) = (matrix(&tr), matrix(&te));
    println!("{} train / {} test samples", tr.len(), te.len());
    let ev = |p: Vec<f64>| m::evaluate(&te, &p, 7);
    for (a, l1) in [(1e-3, 0.5), (1e-2, 0.5), (1e-1, 0.5)] {
        let mdl = ElasticNet::fit(&xtr, &ytr, ElasticNetParameters::default().with_alpha(a).with_l1_ratio(l1)).unwrap();
        show(&format!("elastic net a={a} l1={l1}"), &ev(mdl.predict(&xte).unwrap()));
    }
    for a in [1.0, 100.0, 10000.0] {
        let mdl = RidgeRegression::fit(&xtr, &ytr, RidgeRegressionParameters::default().with_alpha(a)).unwrap();
        show(&format!("ridge a={a}"), &ev(mdl.predict(&xte).unwrap()));
    }
    for (trees, depth, leaf) in [(100usize, 6u16, 200usize), (100, 10, 100)] {
        let p = RandomForestRegressorParameters::default().with_n_trees(trees).with_max_depth(depth).with_min_samples_leaf(leaf).with_m(5);
        let mdl = RandomForestRegressor::fit(&xtr, &ytr, p).unwrap();
        show(&format!("random forest {trees}x d{depth} leaf{leaf}"), &ev(mdl.predict(&xte).unwrap()));
    }
    let mine = m::Ridge::fit(&train, 100.0);
    show("OWN ridge a=100 on raw test", &m::evaluate(&test, &test.iter().map(|s| mine.predict(s)).collect::<Vec<_>>(), 7));
    println!("  own ridge weights: {}", m::FEATURES.iter().zip(mine.weights()).map(|(f, w)| format!("{f}={:+.2e}", w)).collect::<Vec<_>>().join(" "));
    println!("rolling-origin (first 100 days, then 4-week windows, 3-day purge, every prediction out of sample):");
    for (name, a) in [("ridge a=100", 100.0), ("ridge a=10000", 10000.0)] {
        let (s, p) = m::rolling(&all, 100, 28, 3, |t| { let r = m::Ridge::fit(t, a); Box::new(move |x| r.predict(x)) });
        show(name, &m::evaluate(&s, &p, 9));
    }
    let (s, p) = m::rolling(&all, 100, 28, 3, |t| { let g = m::fit(t); Box::new(move |x| g.trees.predict(&qwen_lab::mltrade::Trade { block: 0, x: x.x.clone(), win: false, ret: 0.0 })) });
    show("trees (own), rolling", &m::evaluate(&s, &p, 9));
    let (s, _) = m::rolling(&all, 100, 28, 3, |_| Box::new(|_| 0.0));
    println!("  rolling covers {} samples", s.len());
    println!("self-written reference:");
    let r = m::report(&series, &candles::Costs { commission: 1.0, half: Default::default(), default_half: 0.0003 }); println!("{}", r.text.lines().filter(|l| l.trim_start().starts_with("trees") || l.trim_start().starts_with("logistic")).collect::<Vec<_>>().join("\n"));
    Ok(())
}

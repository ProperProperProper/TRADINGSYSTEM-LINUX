//! Reproducible comparison of proposal models on the lab's REAL history (no synthetic data), walk-forward in time order:
//! `cargo run --release --example brain_eval -- <copy of memory.sqlite3>`. Fits on the training half only (h1, nh1, worst block); the lab's holdout
//! verdict is used for EVALUATION only (does the top quarter pass it more often than chance?). Results and conclusions: docs/BRAIN.md.
use qwen_lab::brain::*;
use std::collections::BTreeSet;

fn ridge_predict(train: &[Obs], x: &BTreeSet<String>, lambda: f64, fam_w: f64) -> f64 {
    let mut toks: Vec<String> = train.iter().flat_map(|o| o.options.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect();
    toks.sort();
    let p = toks.len();
    let mean = train.iter().map(|o| o.y).sum::<f64>() / train.len() as f64;
    let idx = |t: &String| toks.iter().position(|u| u == t);
    let mut a = vec![0.0; p * p]; let mut b = vec![0.0; p];
    for o in train {
        let ids: Vec<usize> = o.options.iter().filter_map(&idx).collect();
        for &i in &ids { b[i] += o.y - mean; for &j in &ids { a[i * p + j] += 1.0; } }
    }
    for i in 0..p { a[i * p + i] += lambda; }
    let _ = fam_w;
    let l = match cholesky(&a, p) { Some(l) => l, None => return mean };
    // solve
    let mut y = vec![0.0; p];
    for i in 0..p { let mut s = b[i]; for k in 0..i { s -= l[i * p + k] * y[k]; } y[i] = s / l[i * p + i]; }
    let mut e = vec![0.0; p];
    for i in (0..p).rev() { let mut s = y[i]; for k in i + 1..p { s -= l[k * p + i] * e[k]; } e[i] = s / l[i * p + i]; }
    mean + x.iter().filter_map(&idx).map(|i| e[i]).sum::<f64>()
}

fn ridge_pooled(train: &[Obs], x: &BTreeSet<String>, lambda: f64) -> f64 {
    let toks: Vec<String> = train.iter().flat_map(|o| o.options.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect();
    let p = toks.len();
    let mean = train.iter().map(|o| o.y).sum::<f64>() / train.len() as f64;
    let idx = |t: &String| toks.iter().position(|u| u == t);
    let mut a = vec![0.0; p * p]; let mut b = vec![0.0; p];
    for o in train {
        let ids: Vec<usize> = o.options.iter().filter_map(&idx).collect();
        let w = 1.0 / ids.len().max(1) as f64;
        for &i in &ids { b[i] += w * (o.y - mean); for &j in &ids { a[i * p + j] += w * w; } }
    }
    for i in 0..p { a[i * p + i] += lambda; }
    let l = match cholesky(&a, p) { Some(l) => l, None => return mean };
    let mut y = vec![0.0; p];
    for i in 0..p { let mut s = b[i]; for k in 0..i { s -= l[i * p + k] * y[k]; } y[i] = s / l[i * p + i]; }
    let mut e = vec![0.0; p];
    for i in (0..p).rev() { let mut s = y[i]; for k in i + 1..p { s -= l[k * p + i] * e[k]; } e[i] = s / l[i * p + i]; }
    let ids: Vec<usize> = x.iter().filter_map(&idx).collect();
    mean + ids.iter().map(|&i| e[i]).sum::<f64>() / ids.len().max(1) as f64
}

fn eval(obs: &[Obs], start: usize, step: usize, f: &dyn Fn(&[Obs], &BTreeSet<String>) -> f64) -> (f64, f64, usize) {
    let (mut p, mut act) = (vec![], vec![]);
    let mut t = start;
    while t + 1 < obs.len() { let end = (t + step).min(obs.len()); for o in &obs[t..end] { p.push(f(&obs[..t], &o.options)); act.push(o.y); } t = end; }
    let mut i: Vec<usize> = (0..p.len()).collect(); i.sort_by(|&a, &b| p[b].total_cmp(&p[a]));
    let k = (p.len() / 4).max(1);
    (spearman(&p, &act), i[..k].iter().map(|&j| act[j]).sum::<f64>() / k as f64, p.len())
}

/// share of the top quarter (by prediction) that passes the lab's holdout verdict on cut A, versus the share among all later variants
fn pass_rate(obs: &[Obs], pass: &[bool], start: usize, step: usize, f: &dyn Fn(&[Obs], &BTreeSet<String>) -> f64) -> (f64, f64, usize) {
    let (mut p, mut ok) = (vec![], vec![]);
    let mut t = start;
    while t + 1 < obs.len() { let end = (t + step).min(obs.len()); for (o, pa) in obs[t..end].iter().zip(&pass[t..end]) { p.push(f(&obs[..t], &o.options)); ok.push(*pa); } t = end; }
    let mut i: Vec<usize> = (0..p.len()).collect(); i.sort_by(|&a, &b| p[b].total_cmp(&p[a]));
    let k = (p.len() / 4).max(1);
    (i[..k].iter().filter(|&&j| ok[j]).count() as f64 / k as f64, ok.iter().filter(|x| **x).count() as f64 / ok.len() as f64, k)
}

type Predictor = dyn Fn(&[Obs], &BTreeSet<String>) -> f64;
type Target = dyn Fn(&Rw) -> f64;
type Rw = (String, f64, Option<f64>, f64, f64, f64, Option<f64>);

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("memory db copy");
    let mem = rusqlite::Connection::open(db)?;
    let obs = load(&mem)?;
    println!("{} variants", obs.len());
    let base = obs.iter().map(|o| o.y).sum::<f64>() / obs.len() as f64;
    println!("plain average of all: {base:+.3}");
    println!("{:<28} {:>8} {:>8} {:>8} {:>8}", "model", "rho@60", "top@60", "rho@100", "top@100");
    let row = |name: &str, f: &dyn Fn(&[Obs], &BTreeSet<String>) -> f64| {
        let (r1, t1, _) = eval(&obs, 60, 5, f); let (r2, t2, _) = eval(&obs, 100, 5, f);
        println!("{name:<28} {r1:>8.3} {t1:>+8.3} {r2:>8.3} {t2:>+8.3}");
    };
    row("additive mean (baseline)", &|tr, x| additive_predict(tr, x));
    for lam in [0.5, 1.0, 2.0, 4.0, 8.0] { row(&format!("ridge additive lambda={lam}"), &move |tr, x| ridge_predict(tr, x, lam, 0.0)); }
    for lam in [0.02, 0.1, 0.5, 1.0, 2.0] { row(&format!("ridge POOLED lambda={lam}"), &move |tr, x| ridge_pooled(tr, x, lam)); }
    row("GP (ML-tuned)", &|tr, x| Model::fit(tr, tune(tr)).map_or(0.0, |m| m.predict(x).0));
    row("GP fixed default", &|tr, x| Model::fit(tr, Hyper::default()).map_or(0.0, |m| m.predict(x).0));
    row("GP additive-only (c=0,b=0)", &|tr, x| Model::fit(tr, Hyper { b: 0.0, c: 0.0, ..Hyper::default() }).map_or(0.0, |m| m.predict(x).0));
    // does the top quarter pass the HOLDOUT more often than chance? (holdout used for evaluation only, never for training)
    let lb = qwen_lab::strategy::leaderboard(&mem)?;
    let status_of = |o: &Obs| { let key = qwen_lab::strategy::key(&o.options.iter().cloned().collect::<Vec<_>>()); lb.iter().find(|r| r["interval"] == "5h" && r["tokens"] == key.as_str()).map(|r| r["status"].as_str().unwrap_or("").to_string()) };
    let pass: Vec<bool> = obs.iter().map(|o| status_of(o).is_some_and(|s| s != "failed-holdout" && s != "untested")).collect();
    println!("\nholdout pass rate overall: {}/{} = {:.1}%", pass.iter().filter(|x| **x).count(), pass.len(), 100.0 * pass.iter().filter(|x| **x).count() as f64 / pass.len() as f64);
    println!("{:<28} {:>14} {:>14}", "model", "top-quarter @60", "top-quarter @100");
    let m: Vec<(&str, Box<Predictor>)> = vec![
        ("additive mean (baseline)", Box::new(additive_predict)),
        ("ridge POOLED lambda=2", Box::new(|tr, x| ridge_pooled(tr, x, 2.0))),
        ("GP fixed default", Box::new(|tr, x| Model::fit(tr, Hyper::default()).map_or(0.0, |m| m.predict(x).0))),
        ("random (no skill)", Box::new(|_tr, x| { let mut h = 1469598103934665603u64; for t in x { for b in t.bytes() { h = (h ^ b as u64).wrapping_mul(1099511628211); } } (h >> 11) as f64 })),
    ];
    for (name, f) in &m {
        let (a, base_a, k1) = pass_rate(&obs, &pass, 60, 5, f.as_ref()); let (b, base_b, k2) = pass_rate(&obs, &pass, 100, 5, f.as_ref());
        println!("{name:<28} {:>5.1}% (n={k1}, base {:.1}%) {:>5.1}% (n={k2}, base {:.1}%)", 100.0 * a, 100.0 * base_a, 100.0 * b, 100.0 * base_b);
    }

    // ---- overfit-aware TRAINING-HALF targets (no holdout numbers used for fitting)
    let mut st = mem.prepare("SELECT tokens, h1, nh1, worst, pos, trades, win1 FROM strategy_runs WHERE regime = ?1 AND interval = '5h' AND cut = 'A' AND tokens != 'baseline' ORDER BY ts, id")?;
    let regime = qwen_lab::strategy::current_regime(&mem);
    let rows: Vec<Rw> = st.query_map([regime], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?.collect::<Result<_, _>>()?;
    assert_eq!(rows.len(), obs.len());
    let targets: Vec<(&str, Box<Target>)> = vec![
        ("h1 (clean training gain)", Box::new(|r| r.1)),
        ("nh1 (noisy training gain)", Box::new(|r| r.2.unwrap_or(r.1 - 2.0))),
        ("min(h1, nh1)", Box::new(|r| r.1.min(r.2.unwrap_or(r.1 - 2.0)))),
        ("min(h1,nh1) + worst/4", Box::new(|r| r.1.min(r.2.unwrap_or(r.1 - 2.0)) + r.3 / 4.0)),
        ("blocks positive %", Box::new(|r| r.4 / 20.0)),
    ];
    println!("\nrank by an overfit-aware TRAINING-half target; does the top quarter pass the HOLDOUT? (target -> additive-mean model)");
    println!("{:<28} {:>26} {:>26}", "target", "top-quarter pass @60", "top-quarter pass @100");
    for (name, tf) in &targets {
        let o2: Vec<Obs> = obs.iter().zip(&rows).map(|(o, r)| Obs { options: o.options.clone(), y: tf(r) }).collect();
        let f = |tr: &[Obs], x: &BTreeSet<String>| additive_predict(tr, x);
        let (a, ba, k1) = pass_rate(&o2, &pass, 60, 5, &f); let (b, bb, k2) = pass_rate(&o2, &pass, 100, 5, &f);
        println!("{name:<28} {:>5.1}% (n={k1}, base {:.1}%) {:>5.1}% (n={k2}, base {:.1}%)", 100.0 * a, 100.0 * ba, 100.0 * b, 100.0 * bb);
    }
    // does a variant's own noisy/worst profile (training half) separate passers from failers at all?
    let (mut pa, mut fa) = (vec![], vec![]);
    for (r, ok) in rows.iter().zip(&pass) { let v = (r.1.min(r.2.unwrap_or(r.1 - 2.0)), r.3, r.4); if *ok { pa.push(v) } else { fa.push(v) } }
    let avg = |v: &Vec<(f64, f64, f64)>, f: &dyn Fn(&(f64, f64, f64)) -> f64| v.iter().map(f).sum::<f64>() / v.len().max(1) as f64;
    println!("\nmean of passers (n={}) vs failers (n={}): min(h1,nh1) {:+.2} vs {:+.2} | worst block {:+.1} vs {:+.1} | blocks positive {:.0}% vs {:.0}%",
        pa.len(), fa.len(), avg(&pa, &|t| t.0), avg(&fa, &|t| t.0), avg(&pa, &|t| t.1), avg(&fa, &|t| t.1), avg(&pa, &|t| t.2), avg(&fa, &|t| t.2));
    Ok(())
}

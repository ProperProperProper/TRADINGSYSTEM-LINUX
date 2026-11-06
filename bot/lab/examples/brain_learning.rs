//! Does the model LEARN? Learning curves on the lab's real data: the same model trained on more and more history, judged each time on the same
//! fixed later data it never saw. `cargo run --release --example brain_learning -- <copy of memory.sqlite3> <copy of data.sqlite3>`.
use qwen_lab::brain::{additive_predict, load, spearman, tune, ucb_score, Model, Obs};
use qwen_lab::mltrade::{self, auc, judge, quantile, relative_to_block, split_by_block, Gbdt, GbdtParams, Logistic, Trade};

fn logloss(p: &[f64], y: &[bool]) -> f64 { -p.iter().zip(y).map(|(p, y)| { let p = p.clamp(1e-6, 1.0 - 1e-6); if *y { p.ln() } else { (1.0 - p).ln() } }).sum::<f64>() / p.len() as f64 }

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mem = rusqlite::Connection::open_with_flags(&args[0], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let data = rusqlite::Connection::open_with_flags(&args[1], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    // ---- 1. choosing variants: train on the first k tested variants, test on the LAST 30 (never trained on)
    let obs = load(&mem)?;
    let n = obs.len();
    let test = &obs[n - 30..];
    let mean_all = |t: &[Obs]| t.iter().map(|o| o.y).sum::<f64>() / t.len() as f64;
    println!("=== 1. variants: {} tested, test = the latest 30 (their mean training gain {:+.2}); error of predicting each one's gain", n, mean_all(test));
    println!("{:>6}  {:>10} {:>10} {:>10} | {:>10} {:>10} {:>10}   (MAE: lower is better; 'mean' = always guess the average; rank corr: higher is better)", "k", "MAE mean", "MAE add.", "MAE GP", "rho add.", "rho GP", "rho ucb");
    for k in [10, 20, 40, 60, 80, 100, 120, 140, n - 30] {
        let tr = &obs[..k];
        let gp = Model::fit(tr, tune(tr));
        let (mut e_mean, mut e_add, mut e_gp) = (0.0, 0.0, 0.0);
        let (mut p_add, mut p_gp, mut p_ucb, mut act) = (vec![], vec![], vec![], vec![]);
        let m = mean_all(tr);
        for o in test {
            let a = additive_predict(tr, &o.options);
            let g = gp.as_ref().map_or(m, |g| g.predict(&o.options).0);
            e_mean += (o.y - m).abs(); e_add += (o.y - a).abs(); e_gp += (o.y - g).abs();
            p_add.push(a); p_gp.push(g); p_ucb.push(ucb_score(tr, &o.options, 1.0).0 + ucb_score(tr, &o.options, 1.0).1); act.push(o.y);
        }
        let c = test.len() as f64;
        println!("{k:>6}  {:>10.3} {:>10.3} {:>10.3} | {:>10.3} {:>10.3} {:>10.3}", e_mean / c, e_add / c, e_gp / c, spearman(&p_add, &act), spearman(&p_gp, &act), spearman(&p_ucb, &act));
    }

    // ---- 2. trades: train on the first m blocks of the training period, test on the SAME later 23 blocks
    let trades = relative_to_block(&mltrade::load(&data)?);
    let (train_all, test_t) = split_by_block(&trades, 1.0 / 3.0);
    let mut blocks: Vec<i64> = train_all.iter().map(|t| t.block).collect(); blocks.sort(); blocks.dedup();
    let label: Vec<bool> = test_t.iter().map(|t| t.win).collect();
    println!("\n=== 2. trades (outcomes relative to their block): train on the first m of {} training blocks, test always on the same {} later blocks ({} trades)", blocks.len(), { let mut b: Vec<i64> = test_t.iter().map(|t| t.block).collect(); b.sort(); b.dedup(); b.len() }, test_t.len());
    println!("{:>6} {:>8}  {:>12} {:>14} {:>16}", "blocks", "trades", "AUC (trees)", "AUC (logistic)", "gate gain (pts/trade, keep top 50%)");
    for m in [3usize, 6, 12, 24, 36, blocks.len()] {
        let cut = blocks[m.min(blocks.len()) - 1];
        let tr: Vec<Trade> = train_all.iter().filter(|t| t.block <= cut).cloned().collect();
        let gb = Gbdt::fit(&tr, GbdtParams::default());
        let lg = Logistic::fit(&tr, 150, 0.3, 1e-4);
        let a_gb = auc(&test_t.iter().map(|t| gb.predict(t)).collect::<Vec<_>>(), &label);
        let a_lg = auc(&test_t.iter().map(|t| lg.predict(t)).collect::<Vec<_>>(), &label);
        let gr = Gbdt::fit_with(&tr, GbdtParams::default(), true);
        let thr = quantile(&tr.iter().map(|t| gr.predict(t)).collect::<Vec<_>>(), 0.5);
        let v = judge("x", &test_t, &test_t.iter().map(|t| gr.predict(t)).collect::<Vec<_>>(), thr, 5);
        println!("{m:>6} {:>8}  {a_gb:>12.4} {a_lg:>14.4} {:>10.2} [{:+.2}, {:+.2}]", tr.len(), 100.0 * (v.ret_kept - v.ret_all), 100.0 * v.diff_lo, 100.0 * v.diff_hi);
    }

    // ---- 3. boosting: the model fits the training data more with every round -- does that carry over to unseen blocks?
    println!("\n=== 3. boosting rounds (win/lose trees, relative outcomes): log-loss falls = it is learning the training data; is the test log-loss falling too?");
    println!("{:>7}  {:>14} {:>14}   (a coin flip scores 0.693)", "rounds", "train log-loss", "test log-loss");
    let (es, es_rounds) = Gbdt::fit_early_stopped(&train_all, GbdtParams::default(), false);
    let ytr: Vec<bool> = train_all.iter().map(|t| t.win).collect();
    for r in [0usize, 10, 25, 50, 100, 150, 300] {
        let m = Gbdt::fit(&train_all, GbdtParams { rounds: r, ..GbdtParams::default() });
        let ptr: Vec<f64> = train_all.iter().map(|t| m.predict(t)).collect();
        let pte: Vec<f64> = test_t.iter().map(|t| m.predict(t)).collect();
        println!("{r:>7}  {:>14.4} {:>14.4}", logloss(&ptr, &ytr), logloss(&pte, &label));
    }
    let (ptr, pte): (Vec<f64>, Vec<f64>) = (train_all.iter().map(|t| es.predict(t)).collect(), test_t.iter().map(|t| es.predict(t)).collect());
    println!("  early stopping (chosen on the last quarter of the TRAINING period) picked {es_rounds} rounds: train {:.4}, test {:.4}", logloss(&ptr, &ytr), logloss(&pte, &label));
    Ok(())
}

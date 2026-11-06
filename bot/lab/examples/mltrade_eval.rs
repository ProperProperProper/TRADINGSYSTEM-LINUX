//! Does the bot's own entry data predict winning trades? Walk-forward test of the self-written models on the REAL trades:
//! `cargo run --release --example mltrade_eval -- <copy of data.sqlite3>`. Train = earlier two thirds of the 14-session blocks, test = the later third,
//! judged once. Results and conclusions: docs/BRAIN.md.
use qwen_lab::mltrade::*;

fn run(title: &str, trades: &[Trade]) {
    println!("\n=== {title}");
    let (train, test) = split_by_block(trades, 1.0 / 3.0);
    run_models(&train, &test);
}

fn run_models(train: &[Trade], test: &[Trade]) {
    let show = |v: &Verdict| println!("{:<27} AUC {:.4} | keeps {:>4.0}% | win rate {:.1}% -> {:.1}% | mean return {:+.2}% -> {:+.2}% | gain {:+.2} [{:+.2}, {:+.2}] | better in {}/{} blocks",
        v.name, v.auc, 100.0 * v.keep_share, 100.0 * v.win_all, 100.0 * v.win_kept, 100.0 * v.ret_all, 100.0 * v.ret_kept, 100.0 * (v.ret_kept - v.ret_all), 100.0 * v.diff_lo, 100.0 * v.diff_hi, v.blocks_better, v.blocks);
    let sc = |f: &dyn Fn(&Trade) -> f64, t: &[Trade]| t.iter().map(f).collect::<Vec<_>>();
    let lg = Logistic::fit(train, 200, 0.3, 1e-4);
    let gb = Gbdt::fit(train, GbdtParams::default());
    for keep in [0.5, 0.7] {
        let thr = quantile(&sc(&|t| lg.predict(t), train), 1.0 - keep);
        show(&judge(&format!("logistic, keep top {:.0}%", keep * 100.0), test, &sc(&|t| lg.predict(t), test), thr, 11));
        let thr = quantile(&sc(&|t| gb.predict(t), train), 1.0 - keep);
        show(&judge(&format!("boosted trees, keep top {:.0}%", keep * 100.0), test, &sc(&|t| gb.predict(t), test), thr, 11));
    }
    // trees trained on the return itself (size of gains and losses counts)
    let gr = Gbdt::fit_with(train, GbdtParams::default(), true);
    for keep in [0.5, 0.7] {
        let thr = quantile(&sc(&|t| gr.predict(t), train), 1.0 - keep);
        show(&judge(&format!("return-trees, keep top {:.0}%", keep * 100.0), test, &sc(&|t| gr.predict(t), test), thr, 11));
    }
    let mut nulls: Vec<f64> = (0..10).map(|k| {
        let m = Gbdt::fit(&shuffled(train, 50 + k), GbdtParams::default());
        auc(&test.iter().map(|t| m.predict(t)).collect::<Vec<_>>(), &test.iter().map(|t| t.win).collect::<Vec<_>>())
    }).collect();
    nulls.sort_by(f64::total_cmp);
    println!("CONTROL (10 models trained on shuffled outcomes): AUC mean {:.3}, range {:.3} .. {:.3}", nulls.iter().sum::<f64>() / 10.0, nulls[0], nulls[9]);
}

fn main() -> anyhow::Result<()> {
    let db = std::env::args().nth(1).expect("copy of data.sqlite3");
    let data = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let trades = load(&data)?;
    println!("{} long trades of the unselected backtest runs", trades.len());
    run("1. does an entry's conditions predict that the trade wins? (raw outcomes)", &trades);
    run("2. do they predict which entries beat the OTHER entries of the same block? (outcomes relative to the block)", &relative_to_block(&trades));
    Ok(())
}

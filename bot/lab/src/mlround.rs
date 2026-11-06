//! The daily improvement round (owner 2026-10-13: "learn and improve"): recency schemes against the fading edge, candidate features each added alone, and
//! meta-labelling on the bot's own trades with a tail label -- all against the trial ledger, with PAIRED, multiple-testing-adjusted intervals.
//! Nothing is adopted unless the paired difference is above zero at the adjusted level. Called by `qwen-lab candles improve` and by the statistician daily.

use crate::mlcandle::{Sample, FEATURES};
use crate::mltrade::{
    self, judge, quantile, relative_to_block, BaggedGbdt, Gbdt, GbdtParams, Logistic, Trade,
};
use crate::{candles, mlbot, mlcandle as m, mlfeat, mlimprove as imp};
use rusqlite::Connection;
use std::cell::RefCell;
use std::collections::BTreeMap;

macro_rules! say { ($o:expr, $($a:tt)*) => {{ let mut s = $o.borrow_mut(); s.push_str(&format!($($a)*)); s.push('\n'); }}; }

pub struct Round {
    pub text: String,
    pub adopted_scheme: String,
    pub adopted_features: Vec<String>,
}
const DAY: i64 = 86_400_000;

fn universe_only(s: &[Sample], nb: usize) -> Vec<Sample> {
    let top = mlbot::in_universe(s, nb);
    let mut by: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    for x in &top {
        let e = by.entry(x.ts).or_default();
        e.0 += x.raw;
        e.1 += 1;
    }
    top.into_iter()
        .filter(|x| by[&x.ts].1 >= 15)
        .map(|x| {
            let mu = by[&x.ts].0 / by[&x.ts].1 as f64;
            Sample {
                excess: x.raw - mu,
                ..x
            }
        })
        .collect()
}

/// Rolling windows (28 days, first after 100): `fit(train, window_start)` returns a scorer for the window.
type Scorer = Box<dyn Fn(&[Sample]) -> Vec<f64>>;
fn rolled(
    all: &[Sample],
    cols: &[usize],
    fit: &dyn Fn(&[Sample], i64) -> Scorer,
) -> (Vec<Sample>, Vec<f64>) {
    let pick = |s: &[Sample]| -> Vec<Sample> {
        s.iter()
            .map(|x| Sample {
                x: cols.iter().map(|&j| x.x[j]).collect(),
                ..x.clone()
            })
            .collect()
    };
    let t0 = all.iter().map(|s| s.ts).min().unwrap();
    let t1 = all.iter().map(|s| s.ts).max().unwrap();
    let (mut out, mut pred) = (vec![], vec![]);
    let mut start = t0 + 100 * DAY;
    while start < t1 {
        let end = start + 28 * DAY;
        let train: Vec<Sample> = pick(
            &all.iter()
                .filter(|s| s.ts <= start - 3 * DAY)
                .cloned()
                .collect::<Vec<_>>(),
        );
        let test_full: Vec<Sample> = all
            .iter()
            .filter(|s| s.ts >= start && s.ts < end)
            .cloned()
            .collect();
        if train.len() > 3000 && test_full.len() > 400 {
            let p = fit(&train, start - 3 * DAY)(&pick(&test_full));
            out.extend(test_full);
            pred.extend(p);
        }
        start = end;
    }
    (out, pred)
}

fn z_by_moment(s: &[Sample], p: &[f64]) -> Vec<f64> {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, x) in s.iter().enumerate() {
        by.entry(x.ts).or_default().push(i);
    }
    let mut o = vec![0.0; p.len()];
    for v in by.values() {
        let mu = v.iter().map(|&i| p[i]).sum::<f64>() / v.len() as f64;
        let sd = (v.iter().map(|&i| (p[i] - mu).powi(2)).sum::<f64>() / v.len() as f64)
            .sqrt()
            .max(1e-12);
        for &i in v {
            o[i] = (p[i] - mu) / sd;
        }
    }
    o
}

pub fn run(candle_db: &std::path::Path, data_db: &std::path::Path) -> anyhow::Result<Round> {
    let out = RefCell::new(String::new());
    let db = Connection::open(candle_db)?;
    imp::init(&db)?;
    let (series, _) = candles::prepare(&candles::load(&db)?, &candles::load_universe(&db));
    let nb = FEATURES.len();
    let nbot = nb + mlbot::EXTRA.len();
    // hourly samples with bot features, the 7 candidate features, restricted to the bot's top-20 universe, then the SPY-regime interactions last
    let raw = mlfeat::extend(&series, &mlbot::extend(&series, &m::build(&series, 4)));
    let all = mlbot::interactions(&universe_only(&raw, nb));
    let ninter = all[0].x.len() - nbot - mlfeat::NAMES.len();
    let base_cols: Vec<usize> = (0..nbot)
        .chain(nbot + mlfeat::NAMES.len()..all[0].x.len())
        .collect();
    say!(
        out,
        "{} top-20 samples, {} base features ({} interactions), trials so far {}",
        all.len(),
        base_cols.len(),
        ninter,
        imp::count(&db)
    );
    let show = |tag: &str,
                name: &str,
                s: &[Sample],
                p: &[f64],
                base: Option<&[f64]>,
                trials: usize| {
        let a = imp::adjusted(s, p, trials, 7);
        let paired = base.map(|b| imp::paired_ic(s, b, p, trials, 9));
        match paired {
            Some((d, lo, hi)) => say!(out, "  {tag:<10} {name:<34} IC {:+.4} [{:+.4},{:+.4}] | paired vs base {:+.4} [{:+.4},{:+.4}] {}", a[0], a[1], a[2], d, lo, hi, if lo > 0.0 { "IMPROVES" } else { "no evidence" }),
            None => say!(out, "  {tag:<10} {name:<34} IC {:+.4} [{:+.4},{:+.4}] spread {:+.1} bps [{:+.1},{:+.1}]  (adjusted for {trials} trials)", a[0], a[1], a[2], a[3], a[4], a[5]),
        }
        paired
    };

    // ---- #3: the edge may fade -> recency schemes (paired against the expanding window)
    say!(
        out,
        "\n== #3 recency schemes (the edge looked stronger in the first half) =="
    );
    let ridge_scheme = |w: Option<i64>, h: Option<f64>| {
        move |tr: &[Sample], end: i64| -> Scorer {
            let r = crate::mlscores::tuned_ridge(tr, end, w, h);
            Box::new(move |s: &[Sample]| s.iter().map(|x| r.predict(x)).collect())
        }
    };
    let (te, base_p) = rolled(&all, &base_cols, &ridge_scheme(None, None));
    let t = imp::count(&db) + 1;
    show("expanding", "all history (the base)", &te, &base_p, None, t);
    let mut adopted_scheme = "expanding".to_string();
    let schemes: Vec<(&str, Box<dyn Fn(&[Sample], i64) -> Scorer>)> = vec![
        ("rolling 85 days", Box::new(ridge_scheme(Some(85), None))),
        ("rolling 126 days", Box::new(ridge_scheme(Some(126), None))),
        (
            "half-life 42 days",
            Box::new(ridge_scheme(None, Some(42.0))),
        ),
        (
            "half-life 84 days",
            Box::new(ridge_scheme(None, Some(84.0))),
        ),
        (
            "bag: expanding+126d+hl42",
            Box::new(|tr: &[Sample], end: i64| -> Scorer {
                let rs = [
                    crate::mlscores::tuned_ridge(tr, end, None, None),
                    crate::mlscores::tuned_ridge(tr, end, Some(126), None),
                    crate::mlscores::tuned_ridge(tr, end, None, Some(42.0)),
                ];
                Box::new(move |s: &[Sample]| {
                    let ps: Vec<Vec<f64>> = rs
                        .iter()
                        .map(|r| {
                            z_by_moment(s, &s.iter().map(|x| r.predict(x)).collect::<Vec<_>>())
                        })
                        .collect();
                    (0..s.len())
                        .map(|i| ps.iter().map(|p| p[i]).sum::<f64>() / 3.0)
                        .collect()
                })
            }),
        ),
    ];
    for (name, fit) in &schemes {
        let (s, p) = rolled(&all, &base_cols, fit.as_ref());
        let trials = imp::count(&db) + 1;
        let r = show("scheme", name, &s, &p, Some(&base_p), trials);
        let ok = r.is_some_and(|x| x.1 > 0.0);
        let a = imp::adjusted(&s, &p, trials, 7);
        imp::record(&db, "scheme", name, "ic", a[0], a[1], a[2], ok)?;
        if ok {
            adopted_scheme = name.to_string();
        }
    }

    // ---- #5: candidate features, each ALONE on top of the base
    say!(
        out,
        "\n== #5 candidate features, each added alone (paired against the base) =="
    );
    let mut adopted_feats: Vec<&str> = vec![];
    for (k, name) in mlfeat::NAMES.iter().enumerate() {
        let mut cols = base_cols.clone();
        cols.push(nbot + k);
        let (s, p) = rolled(&all, &cols, &ridge_scheme(None, None));
        let trials = imp::count(&db) + 1;
        let r = show("feature", name, &s, &p, Some(&base_p), trials);
        let ok = r.is_some_and(|x| x.1 > 0.0);
        let a = imp::adjusted(&s, &p, trials, 7);
        imp::record(&db, "feature", name, "ic", a[0], a[1], a[2], ok)?;
        if ok {
            adopted_feats.push(name);
        }
    }

    // ---- #2: meta-labelling on the bot's own backtest trades (tail label)
    say!(
        out,
        "\n== #2 meta-labelling: the bot's own entries, candle context at the decision bar =="
    );
    let all1 = mlbot::interactions(&mlbot::extend(&series, &m::build(&series, 1)));
    let names: Vec<&String> = series.keys().collect();
    let sym_idx: BTreeMap<&str, usize> = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_str(), i))
        .collect();
    let index: std::collections::HashMap<(usize, i64), usize> = all1
        .iter()
        .enumerate()
        .map(|(i, s)| ((s.sym, s.ts), i))
        .collect();
    let nctx = nbot + ninter;
    let lookup = |sym: &str, ts: i64| -> Option<Vec<f64>> {
        let s = *sym_idx.get(sym)?;
        index.get(&(s, ts)).map(|&i| all1[i].x[..nctx].to_vec())
    };
    let data = Connection::open_with_flags(data_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    for (label, with_tf) in [
        ("candle context", false),
        ("candle context + the bot's 18 trade features", true),
    ] {
        let trades = crate::mlmeta::trades_with_context(&data, &lookup, with_tf)?;
        if trades.len() < 3000 {
            say!(out, "  {label}: only {} trades with context", trades.len());
            continue;
        }
        let mut blocks: Vec<i64> = trades.iter().map(|t| t.block).collect();
        blocks.sort();
        blocks.dedup();
        say!(
            out,
            "  {label}: {} trades, {} blocks",
            trades.len(),
            blocks.len()
        );
        let rel = relative_to_block(&trades);
        let tail_q = quantile(&trades.iter().map(|t| t.ret).collect::<Vec<_>>(), 0.2); // the worst 20% of trades are "the tail" (data-driven)
        let mut pool: Vec<(Trade, f64)> = vec![]; // (test trade, score - threshold) pooled over folds, per model
        let models = [
            "logistic win",
            "trees on return",
            "block-bagged feature-subspace trees",
            "logistic TAIL (worst 20%)",
            "control: logistic on shuffled",
        ];
        let mut pools: Vec<Vec<(Trade, f64)>> = vec![vec![]; models.len()];
        for fold in 0..3 {
            let lo = blocks[blocks.len() * (2 + fold) / 5];
            let hi = if fold == 2 {
                i64::MAX
            } else {
                blocks[blocks.len() * (3 + fold) / 5]
            };
            let prev = blocks
                .iter()
                .rev()
                .find(|b| **b < lo)
                .copied()
                .unwrap_or(lo); // one block purge
            let train: Vec<Trade> = rel.iter().filter(|t| t.block < prev).cloned().collect();
            let test: Vec<Trade> = rel
                .iter()
                .filter(|t| t.block >= lo && t.block < hi)
                .cloned()
                .collect();
            if train.len() < 2000 || test.len() < 500 {
                continue;
            }
            let tail_train: Vec<Trade> = train
                .iter()
                .zip(trades.iter().filter(|t| t.block < prev))
                .map(|(r, a)| Trade {
                    win: a.ret > tail_q,
                    ..r.clone()
                })
                .collect();
            let lg = Logistic::fit(&train, 120, 0.3, 1e-4);
            let (gb, _) = Gbdt::fit_early_stopped(
                &train,
                GbdtParams {
                    min_leaf: 300,
                    ..GbdtParams::default()
                },
                true,
            );
            let bag = BaggedGbdt::fit(&train, 7, 101 + fold as u64, true);
            let lt = Logistic::fit(&tail_train, 120, 0.3, 1e-4);
            let ls = Logistic::fit(&mltrade::shuffled(&train, 17), 120, 0.3, 1e-4);
            let scorers: Vec<Box<dyn Fn(&Trade) -> f64>> = vec![
                Box::new(|t| lg.predict(t)),
                Box::new(|t| gb.predict(t)),
                Box::new(|t| bag.predict(t)),
                Box::new(|t| lt.predict(t)),
                Box::new(|t| ls.predict(t)),
            ];
            for (k, sc) in scorers.iter().enumerate() {
                let thr = quantile(&train.iter().map(&sc).collect::<Vec<_>>(), 0.35); // veto the worst 35% by training scores
                for t in &test {
                    pools[k].push((t.clone(), sc(t) - thr));
                }
            }
        }
        for (k, name) in models.iter().enumerate() {
            let (tests, scores): (Vec<Trade>, Vec<f64>) = pools[k].iter().cloned().unzip();
            if tests.is_empty() {
                continue;
            }
            let v = judge(name, &tests, &scores, 0.0, 5);
            say!(out, "    {name:<32} kept {:.0}% | gain per trade {:+.3} pts [{:+.3},{:+.3}] (95%) | better in {}/{} blocks | AUC {:.3}", v.keep_share * 100.0, (v.ret_kept - v.ret_all) * 100.0, v.diff_lo * 100.0, v.diff_hi * 100.0, v.blocks_better, v.blocks, v.auc);
            imp::record(
                &db,
                "meta",
                &format!("{label}: {name}"),
                "gain_pts",
                (v.ret_kept - v.ret_all) * 100.0,
                v.diff_lo * 100.0,
                v.diff_hi * 100.0,
                false,
            )?;
        }
        let _ = &mut pool;
    }
    say!(out, "\nadopted scheme: {adopted_scheme}; adopted features: {adopted_feats:?}; trials in the ledger: {}", imp::count(&db));
    Ok(Round {
        text: out.into_inner(),
        adopted_scheme,
        adopted_features: adopted_feats.iter().map(|s| s.to_string()).collect(),
    })
}

/// The standing experiments that keep the model learning when no new data arrives (24x7): training windows and recency half-lives beyond the five schemes of the
/// daily round. Each is a ledger trial (paired against the expanding base at the adjusted level); the least recently run is repeated when everything has run,
/// so the same question is re-asked as the data grows. Returns a report line.
pub fn explore_next(
    candle_db: &std::path::Path,
    data_db: &std::path::Path,
) -> anyhow::Result<String> {
    let _ = data_db;
    let db = Connection::open(candle_db)?;
    imp::init(&db)?;
    let mut specs: Vec<(String, Option<i64>, Option<f64>)> = vec![];
    for w in [63, 105, 147, 168, 210] {
        specs.push((format!("rolling {w} days"), Some(w), None));
    }
    for h in [21.0, 63.0, 126.0, 168.0] {
        specs.push((format!("half-life {h:.0} days"), None, Some(h)));
    }
    let done: BTreeMap<String, i64> = {
        let mut st = db.prepare("SELECT name, ts FROM ml_trials WHERE family = 'explore'")?;
        let rows = st
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .flatten()
            .collect();
        rows
    };
    // the catalogue first; then NEW experiments are generated without end (random windows and half-lives not tried yet), so the model keeps learning from the EXISTING data without
    // waiting for new bars; only when every generated name has been used is the one run longest ago repeated
    // (the data is built first: a generated training window longer than the history would be the full-history run under another name and count as a needless extra trial)
    let (series, _) = candles::prepare(&candles::load(&db)?, &candles::load_universe(&db));
    let nb = FEATURES.len();
    let nbot = nb + mlbot::EXTRA.len();
    let raw = mlfeat::extend(&series, &mlbot::extend(&series, &m::build(&series, 4)));
    let all = mlbot::interactions(&universe_only(&raw, nb));
    if all.len() < 5000 {
        return Ok("explore: too little data".into());
    }
    let span_days = (all.iter().map(|s| s.ts).max().unwrap_or(0)
        - all.iter().map(|s| s.ts).min().unwrap_or(0))
        / 86_400_000;
    let max_window = (span_days - 35).max(40);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64);
    let mut rng = crate::automl::Rng::new(nanos ^ std::process::id() as u64);
    let fresh = specs
        .iter()
        .find(|s| !done.contains_key(&s.0))
        .cloned()
        .or_else(|| {
            (0..400).find_map(|_| {
                if rng.unit() < 0.5 {
                    let w = (35 + 7 * (rng.bits() % 40) as i64).min(max_window);
                    let n = format!("rolling {w} days");
                    (!done.contains_key(&n)).then_some((n, Some(w), None))
                } else {
                    let h = 10.0 + 7.0 * (rng.bits() % 40) as f64;
                    let n = format!("half-life {h:.0} days");
                    (!done.contains_key(&n)).then_some((n, None, Some(h)))
                }
            })
        });
    let Some((name, window, hl)) = fresh.or_else(|| {
        specs
            .iter()
            .min_by_key(|s| done.get(&s.0).copied().unwrap_or(0))
            .cloned()
    }) else {
        return Ok("no experiments defined".into());
    };
    let base_cols: Vec<usize> = (0..nbot)
        .chain(nbot + mlfeat::NAMES.len()..all[0].x.len())
        .collect();
    let scheme = |w: Option<i64>, h: Option<f64>| {
        move |tr: &[Sample], end: i64| -> Scorer {
            let r = crate::mlscores::tuned_ridge(tr, end, w, h);
            Box::new(move |s: &[Sample]| s.iter().map(|x| r.predict(x)).collect())
        }
    };
    let (_, base_p) = rolled(&all, &base_cols, &scheme(None, None));
    let (s, p) = rolled(&all, &base_cols, &scheme(window, hl));
    let trials = imp::count(&db) + usize::from(!done.contains_key(&name));
    let a = imp::adjusted(&s, &p, trials, 7);
    let (d, lo, hi) = imp::paired_ic(&s, &base_p, &p, trials, 9);
    let adopted = lo > 0.0;
    imp::record(&db, "explore", &name, "ic", a[0], a[1], a[2], adopted)?;
    Ok(format!("explore {name}: IC {:+.4}, paired vs the expanding base {d:+.4} [{lo:+.4},{hi:+.4}] at {trials} trials -> {}", a[0], if adopted { "ADOPTED" } else { "no evidence" }))
}

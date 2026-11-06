//! Machine learning on the bot's real trades (owner 2026-10-13: "use machine learning with the self-created model, for trading").
//!
//! Question: do the conditions at the moment of entry (stock and market state, 19 features recorded for every backtest trade) predict whether the
//! trade makes money -- beyond what chance gives? Everything is self-written Rust (no ML library): a logistic regression baseline and gradient-boosted
//! decision trees (second-order boosting, histogram splits). The test follows the lab's own discipline:
//!   * data: long trades of the unselected backtest runs (`random` and `oos` roles), 2026 blocks only;
//!   * split by TIME: the earlier two thirds of the 14-session blocks train, the later third is judged once and never used for fitting or choices;
//!   * many runs replay the same blocks with different settings, so trades are NOT independent: uncertainty comes from resampling whole blocks;
//!   * a control model trained on shuffled labels must score about 0.5, or the pipeline leaks.
//!
//! This module only measures skill. A model with real, block-consistent skill could become a research-only entry gate that the lab then validates like
//! any other option; nothing here touches the live strategy.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;

pub const FEATURES: [&str; 18] = [
    "f_c_atr14_pct",
    "f_c_dist_24h_avg_pct",
    "f_c_ret_1h_pct",
    "f_c_ret_24h_pct",
    "f_c_ret_24h_vs_median",
    "f_c_ret_4h_pct",
    "f_c_signal",
    "f_c_turnover_24h",
    "f_c_volume_ratio_1h",
    "f_et_minute",
    "f_m_breadth_pct",
    "f_m_dispersion_24h_pct",
    "f_m_median_return_24h_pct",
    "f_m_ref_return_4h_pct",
    "f_m_ref_slope_pct",
    "f_m_trend_dist_pct",
    "f_m_trend_passes",
    "f_weekday",
];

/// One trade: its block (window start), entry features (NaN = missing), outcome and return on margin.
#[derive(Debug, Clone)]
pub struct Trade {
    pub block: i64,
    pub x: Vec<f64>,
    pub win: bool,
    pub ret: f64,
}

/// Long trades of the unselected runs with their entry features, oldest block first.
pub fn load(data: &Connection) -> Result<Vec<Trade>> {
    let cols: Vec<&str> = FEATURES.to_vec();
    let sql = format!("SELECT r.window_start, t.pnl, t.ret_margin, {} FROM trades t JOIN runs r ON r.run_id = t.run_id JOIN trade_features f ON f.run_id = t.run_id AND f.idx = t.idx \
        WHERE r.role IN ('random', 'oos') AND t.side = 'long' ORDER BY r.window_start, t.entry_ts", cols.iter().map(|c| format!("f.{c}")).collect::<Vec<_>>().join(", "));
    let mut st = data.prepare(&sql)?;
    let n = cols.len();
    let rows = st
        .query_map([], |r| {
            let mut x = Vec::with_capacity(n);
            for i in 0..n {
                x.push(r.get::<_, Option<f64>>(3 + i)?.unwrap_or(f64::NAN));
            }
            Ok(Trade {
                block: r.get(0)?,
                win: r.get::<_, f64>(1)? > 0.0,
                ret: r.get(2)?,
                x,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Splits trades by block time: (train, test) where the test is the latest `test_share` of the distinct blocks.
pub fn split_by_block(trades: &[Trade], test_share: f64) -> (Vec<Trade>, Vec<Trade>) {
    let mut blocks: Vec<i64> = trades.iter().map(|t| t.block).collect();
    blocks.sort();
    blocks.dedup();
    let cut = blocks[((blocks.len() as f64) * (1.0 - test_share)).floor() as usize];
    (
        trades.iter().filter(|t| t.block < cut).cloned().collect(),
        trades.iter().filter(|t| t.block >= cut).cloned().collect(),
    )
}

/// Median of the non-missing values per feature, for filling gaps.
pub fn medians(train: &[Trade]) -> Vec<f64> {
    (0..train[0].x.len())
        .map(|j| {
            let mut v: Vec<f64> = train
                .iter()
                .map(|t| t.x[j])
                .filter(|x| x.is_finite())
                .collect();
            v.sort_by(f64::total_cmp);
            if v.is_empty() {
                0.0
            } else {
                v[v.len() / 2]
            }
        })
        .collect()
}

fn filled(t: &Trade, med: &[f64]) -> Vec<f64> {
    t.x.iter()
        .zip(med)
        .map(|(x, m)| if x.is_finite() { *x } else { *m })
        .collect()
}

// ------------------------------------------------------------------ metrics
/// Area under the ROC curve (Mann-Whitney, average ranks for ties).
pub fn auc(score: &[f64], label: &[bool]) -> f64 {
    let mut idx: Vec<usize> = (0..score.len()).collect();
    idx.sort_by(|&a, &b| score[a].total_cmp(&score[b]));
    let (mut rank_sum, mut i) = (0.0, 0usize);
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && score[idx[j + 1]] == score[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for &k in &idx[i..=j] {
            if label[k] {
                rank_sum += avg;
            }
        }
        i = j + 1;
    }
    let pos = label.iter().filter(|x| **x).count() as f64;
    let neg = label.len() as f64 - pos;
    if pos == 0.0 || neg == 0.0 {
        0.5
    } else {
        (rank_sum - pos * (pos + 1.0) / 2.0) / (pos * neg)
    }
}

// ------------------------------------------------------------------ logistic regression (baseline)
pub struct Logistic {
    mean: Vec<f64>,
    sd: Vec<f64>,
    w: Vec<f64>,
    b: f64,
    med: Vec<f64>,
}

impl Logistic {
    pub fn fit(train: &[Trade], epochs: usize, lr: f64, l2: f64) -> Logistic {
        let med = medians(train);
        let d = med.len();
        let xs: Vec<Vec<f64>> = train.iter().map(|t| filled(t, &med)).collect();
        let n = xs.len() as f64;
        let mean: Vec<f64> = (0..d)
            .map(|j| xs.iter().map(|x| x[j]).sum::<f64>() / n)
            .collect();
        let sd: Vec<f64> = (0..d)
            .map(|j| {
                (xs.iter().map(|x| (x[j] - mean[j]).powi(2)).sum::<f64>() / n)
                    .sqrt()
                    .max(1e-9)
            })
            .collect();
        let z: Vec<Vec<f64>> = xs
            .iter()
            .map(|x| {
                (0..d)
                    .map(|j| ((x[j] - mean[j]) / sd[j]).clamp(-5.0, 5.0))
                    .collect()
            })
            .collect();
        let (mut w, mut b) = (vec![0.0; d], 0.0);
        for _ in 0..epochs {
            let (mut gw, mut gb) = (vec![0.0; d], 0.0);
            for (zi, t) in z.iter().zip(train) {
                let p =
                    1.0 / (1.0 + (-(b + zi.iter().zip(&w).map(|(a, c)| a * c).sum::<f64>())).exp());
                let e = p - if t.win { 1.0 } else { 0.0 };
                for j in 0..d {
                    gw[j] += e * zi[j];
                }
                gb += e;
            }
            for j in 0..d {
                w[j] -= lr * (gw[j] / n + l2 * w[j]);
            }
            b -= lr * gb / n;
        }
        Logistic {
            mean,
            sd,
            w,
            b,
            med,
        }
    }
    pub fn predict(&self, t: &Trade) -> f64 {
        let x = filled(t, &self.med);
        let s = self.b
            + (0..x.len())
                .map(|j| self.w[j] * ((x[j] - self.mean[j]) / self.sd[j]).clamp(-5.0, 5.0))
                .sum::<f64>();
        1.0 / (1.0 + (-s).exp())
    }
}

// ------------------------------------------------------------------ gradient-boosted trees
#[derive(Debug, Clone)]
enum Node {
    Leaf(f64),
    Split {
        feat: usize,
        bin: usize,
        left: Box<Node>,
        right: Box<Node>,
    },
}

pub struct Gbdt {
    med: Vec<f64>,
    cuts: Vec<Vec<f64>>,
    base: f64,
    trees: Vec<Node>,
    lr: f64,
    squared: bool,
}

/// A CPU-cheap, sklearn-style ensemble: each member sees a deterministic bootstrap of whole
/// time blocks and a random feature subspace. Resampling blocks (not individual trades) keeps
/// shared market outcomes together; averaging reduces the variance of a single boosted tree.
pub struct BaggedGbdt {
    members: Vec<(Gbdt, Vec<bool>)>,
}

fn masked(t: &Trade, keep: &[bool]) -> Trade {
    let mut out = t.clone();
    for (x, yes) in out.x.iter_mut().zip(keep) {
        if !yes {
            *x = f64::NAN;
        }
    }
    out
}

impl BaggedGbdt {
    pub fn fit(train: &[Trade], members: usize, seed: u64, squared: bool) -> BaggedGbdt {
        let mut blocks: Vec<i64> = train.iter().map(|t| t.block).collect();
        blocks.sort();
        blocks.dedup();
        let d = train.first().map_or(0, |t| t.x.len());
        let mut state = seed | 1;
        let mut rnd = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let members = (0..members.max(1))
            .map(|_| {
                let mut keep: Vec<bool> = (0..d).map(|_| rnd() % 10 < 7).collect();
                if !keep.iter().any(|x| *x) && d > 0 {
                    keep[(rnd() as usize) % d] = true;
                }
                let sampled: Vec<i64> = (0..blocks.len())
                    .map(|_| blocks[(rnd() as usize) % blocks.len()])
                    .collect();
                let mut boot = Vec::new();
                for block in sampled {
                    boot.extend(
                        train
                            .iter()
                            .filter(|t| t.block == block)
                            .map(|t| masked(t, &keep)),
                    );
                }
                let model = Gbdt::fit_with(
                    &boot,
                    GbdtParams {
                        rounds: 50,
                        depth: 2,
                        ..GbdtParams::default()
                    },
                    squared,
                );
                (model, keep)
            })
            .collect();
        BaggedGbdt { members }
    }

    pub fn predict(&self, t: &Trade) -> f64 {
        self.members
            .iter()
            .map(|(model, keep)| model.predict(&masked(t, keep)))
            .sum::<f64>()
            / self.members.len() as f64
    }
}

#[derive(Clone, Copy)]
pub struct GbdtParams {
    pub rounds: usize,
    pub depth: usize,
    pub lr: f64,
    pub min_leaf: usize,
    pub lambda: f64,
    pub bins: usize,
}
impl Default for GbdtParams {
    fn default() -> Self {
        GbdtParams {
            rounds: 150,
            depth: 3,
            lr: 0.05,
            min_leaf: 300,
            lambda: 10.0,
            bins: 32,
        }
    }
}

fn bin_of(cuts: &[f64], x: f64) -> usize {
    cuts.partition_point(|c| *c <= x)
}

#[allow(clippy::too_many_arguments)]
fn build(
    binned: &[Vec<u8>],
    g: &[f64],
    h: &[f64],
    idx: &[usize],
    depth: usize,
    p: &GbdtParams,
    nbins: &[usize],
) -> Node {
    let (gs, hs): (f64, f64) = idx
        .iter()
        .fold((0.0, 0.0), |a, &i| (a.0 + g[i], a.1 + h[i]));
    let leaf = || Node::Leaf(-gs / (hs + p.lambda));
    if depth == 0 || idx.len() < 2 * p.min_leaf {
        return leaf();
    }
    let mut best: Option<(f64, usize, usize)> = None;
    let parent = gs * gs / (hs + p.lambda);
    for f in 0..binned.len() {
        let nb = nbins[f];
        let (mut hg, mut hh, mut hc) = (vec![0.0; nb], vec![0.0; nb], vec![0usize; nb]);
        for &i in idx {
            let b = binned[f][i] as usize;
            hg[b] += g[i];
            hh[b] += h[i];
            hc[b] += 1;
        }
        let (mut lg, mut lh, mut lc) = (0.0, 0.0, 0usize);
        for b in 0..nb.saturating_sub(1) {
            lg += hg[b];
            lh += hh[b];
            lc += hc[b];
            if lc < p.min_leaf || idx.len() - lc < p.min_leaf {
                continue;
            }
            let gain =
                lg * lg / (lh + p.lambda) + (gs - lg).powi(2) / (hs - lh + p.lambda) - parent;
            if gain > best.map_or(1e-9, |x| x.0) {
                best = Some((gain, f, b));
            }
        }
    }
    let Some((_, feat, bin)) = best else {
        return leaf();
    };
    let (l, r): (Vec<usize>, Vec<usize>) = idx
        .iter()
        .partition(|&&i| (binned[feat][i] as usize) <= bin);
    Node::Split {
        feat,
        bin,
        left: Box::new(build(binned, g, h, &l, depth - 1, p, nbins)),
        right: Box::new(build(binned, g, h, &r, depth - 1, p, nbins)),
    }
}

fn walk(n: &Node, bins: &[usize]) -> f64 {
    match n {
        Node::Leaf(v) => *v,
        Node::Split {
            feat,
            bin,
            left,
            right,
        } => {
            if bins[*feat] <= *bin {
                walk(left, bins)
            } else {
                walk(right, bins)
            }
        }
    }
}

impl Gbdt {
    pub fn fit(train: &[Trade], p: GbdtParams) -> Gbdt {
        Self::fit_with(train, p, false)
    }

    /// `squared = true` learns the (winsorised) return itself instead of win/lose, so the size of gains and losses counts.
    pub fn fit_with(train: &[Trade], p: GbdtParams, squared: bool) -> Gbdt {
        let med = medians(train);
        let d = med.len();
        let xs: Vec<Vec<f64>> = train.iter().map(|t| filled(t, &med)).collect();
        let cuts: Vec<Vec<f64>> = (0..d)
            .map(|j| {
                let mut v: Vec<f64> = xs.iter().map(|x| x[j]).collect();
                v.sort_by(f64::total_cmp);
                let mut c: Vec<f64> = (1..p.bins).map(|k| v[v.len() * k / p.bins]).collect();
                c.dedup();
                c
            })
            .collect();
        let nbins: Vec<usize> = cuts.iter().map(|c| c.len() + 1).collect();
        let binned: Vec<Vec<u8>> = (0..d)
            .map(|j| xs.iter().map(|x| bin_of(&cuts[j], x[j]) as u8).collect())
            .collect();
        let (lo, hi) = {
            let r: Vec<f64> = train.iter().map(|t| t.ret).collect();
            (quantile(&r, 0.01), quantile(&r, 0.99))
        };
        let y: Vec<f64> = train
            .iter()
            .map(|t| {
                if squared {
                    t.ret.clamp(lo, hi)
                } else if t.win {
                    1.0
                } else {
                    0.0
                }
            })
            .collect();
        let base = if squared {
            y.iter().sum::<f64>() / y.len() as f64
        } else {
            let prior = (y.iter().sum::<f64>() / y.len() as f64).clamp(1e-3, 1.0 - 1e-3);
            (prior / (1.0 - prior)).ln()
        };
        let mut score = vec![base; y.len()];
        let all: Vec<usize> = (0..y.len()).collect();
        let mut trees = vec![];
        for _ in 0..p.rounds {
            let (g, h): (Vec<f64>, Vec<f64>) = if squared {
                (
                    score.iter().zip(&y).map(|(s, y)| s - y).collect(),
                    vec![1.0; y.len()],
                )
            } else {
                let pr: Vec<f64> = score.iter().map(|s| 1.0 / (1.0 + (-s).exp())).collect();
                (
                    pr.iter().zip(&y).map(|(p, y)| p - y).collect(),
                    pr.iter().map(|p| (p * (1.0 - p)).max(1e-6)).collect(),
                )
            };
            let tree = build(&binned, &g, &h, &all, p.depth, &p, &nbins);
            for (i, s) in score.iter_mut().enumerate() {
                *s += p.lr
                    * walk(
                        &tree,
                        &(0..d).map(|j| binned[j][i] as usize).collect::<Vec<_>>(),
                    );
            }
            trees.push(tree);
        }
        Gbdt {
            med,
            cuts,
            base,
            trees,
            lr: p.lr,
            squared,
        }
    }
    pub fn predict(&self, t: &Trade) -> f64 {
        self.predict_rounds(t, self.trees.len())
    }

    /// Prediction using only the first `rounds` boosting rounds (for choosing how long to train).
    pub fn predict_rounds(&self, t: &Trade, rounds: usize) -> f64 {
        let x = filled(t, &self.med);
        let bins: Vec<usize> = (0..x.len()).map(|j| bin_of(&self.cuts[j], x[j])).collect();
        let s = self.base
            + self
                .trees
                .iter()
                .take(rounds)
                .map(|tr| self.lr * walk(tr, &bins))
                .sum::<f64>();
        if self.squared {
            s
        } else {
            1.0 / (1.0 + (-s).exp())
        }
    }

    /// Early stopping: trains on the earlier 75% of the training blocks, measures the loss on the last 25% (still the TRAINING period, never the test blocks)
    /// after 0, 5, 10, 25, 50, 100 and 150 rounds, picks the best, and refits on all the training data with that many rounds. Returns (model, rounds).
    pub fn fit_early_stopped(train: &[Trade], p: GbdtParams, squared: bool) -> (Gbdt, usize) {
        let mut blocks: Vec<i64> = train.iter().map(|t| t.block).collect();
        blocks.sort();
        blocks.dedup();
        if blocks.len() < 8 {
            return (Self::fit_with(train, p, squared), p.rounds);
        }
        let cut = blocks[blocks.len() * 3 / 4];
        let (fit, val): (Vec<Trade>, Vec<Trade>) = (
            train.iter().filter(|t| t.block < cut).cloned().collect(),
            train.iter().filter(|t| t.block >= cut).cloned().collect(),
        );
        let max = 150usize;
        let m = Self::fit_with(&fit, GbdtParams { rounds: max, ..p }, squared);
        let (lo, hi) = {
            let r: Vec<f64> = fit.iter().map(|t| t.ret).collect();
            (quantile(&r, 0.01), quantile(&r, 0.99))
        };
        let loss = |rounds: usize| -> f64 {
            val.iter()
                .map(|t| {
                    let pr = m.predict_rounds(t, rounds);
                    if squared {
                        (pr - t.ret.clamp(lo, hi)).powi(2)
                    } else {
                        let pr = pr.clamp(1e-6, 1.0 - 1e-6);
                        if t.win {
                            -pr.ln()
                        } else {
                            -(1.0 - pr).ln()
                        }
                    }
                })
                .sum::<f64>()
                / val.len() as f64
        };
        let best = [0usize, 5, 10, 25, 50, 100, 150]
            .into_iter()
            .min_by(|a, b| loss(*a).total_cmp(&loss(*b)))
            .unwrap_or(0);
        (
            Self::fit_with(train, GbdtParams { rounds: best, ..p }, squared),
            best,
        )
    }
}

// ------------------------------------------------------------------ evaluation
#[derive(Debug, Clone)]
pub struct Verdict {
    pub name: String,
    pub auc: f64,
    pub keep_share: f64,
    pub win_all: f64,
    pub win_kept: f64,
    pub ret_all: f64,
    pub ret_kept: f64,
    pub diff_lo: f64,
    pub diff_hi: f64,
    pub blocks_better: usize,
    pub blocks: usize,
}

/// Judges one set of test predictions: AUC, and the effect of keeping only trades with a score at or above `threshold` (chosen on TRAINING data).
/// The interval for (mean return kept - mean return all) resamples whole blocks, because trades in one block share the market.
pub fn judge(name: &str, test: &[Trade], score: &[f64], threshold: f64, seed: u64) -> Verdict {
    let label: Vec<bool> = test.iter().map(|t| t.win).collect();
    let kept: Vec<bool> = score.iter().map(|s| *s >= threshold).collect();
    let mean = |it: &mut dyn Iterator<Item = f64>| {
        let (s, n) = it.fold((0.0, 0usize), |a, x| (a.0 + x, a.1 + 1));
        if n == 0 {
            0.0
        } else {
            s / n as f64
        }
    };
    let win = |f: &dyn Fn(usize) -> bool| {
        mean(
            &mut (0..test.len())
                .filter(|&i| f(i))
                .map(|i| if test[i].win { 1.0 } else { 0.0 }),
        )
    };
    let ret = |f: &dyn Fn(usize) -> bool| {
        mean(&mut (0..test.len()).filter(|&i| f(i)).map(|i| test[i].ret))
    };
    // per block: (sum ret all, n all, sum ret kept, n kept)
    let mut per: BTreeMap<i64, (f64, usize, f64, usize)> = BTreeMap::new();
    for (i, t) in test.iter().enumerate() {
        let e = per.entry(t.block).or_default();
        e.0 += t.ret;
        e.1 += 1;
        if kept[i] {
            e.2 += t.ret;
            e.3 += 1;
        }
    }
    let blocks: Vec<(f64, usize, f64, usize)> = per.values().copied().collect();
    let diff = |sel: &[usize]| {
        let (a, n, k, m) = sel.iter().fold((0.0, 0usize, 0.0, 0usize), |s, &i| {
            (
                s.0 + blocks[i].0,
                s.1 + blocks[i].1,
                s.2 + blocks[i].2,
                s.3 + blocks[i].3,
            )
        });
        if n == 0 || m == 0 {
            0.0
        } else {
            k / m as f64 - a / n as f64
        }
    };
    let mut st = seed | 1;
    let mut rnd = move || {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        st
    };
    let mut ds: Vec<f64> = (0..2000)
        .map(|_| {
            let sel: Vec<usize> = (0..blocks.len())
                .map(|_| (rnd() % blocks.len() as u64) as usize)
                .collect();
            diff(&sel)
        })
        .collect();
    ds.sort_by(f64::total_cmp);
    let better = blocks
        .iter()
        .filter(|b| b.3 > 0 && b.1 > 0 && b.2 / b.3 as f64 > b.0 / b.1 as f64)
        .count();
    Verdict {
        name: name.to_string(),
        auc: auc(score, &label),
        keep_share: kept.iter().filter(|k| **k).count() as f64 / kept.len() as f64,
        win_all: win(&|_| true),
        win_kept: win(&|i| kept[i]),
        ret_all: ret(&|_| true),
        ret_kept: ret(&|i| kept[i]),
        diff_lo: ds[ds.len() * 25 / 1000],
        diff_hi: ds[ds.len() * 975 / 1000],
        blocks_better: better,
        blocks: blocks.len(),
    }
}

/// Re-expresses every trade relative to its own block: `ret` becomes the excess over the block's mean return and `win` means "better than the block's median
/// trade". Whole blocks win or lose together with the market; this asks the question a gate really answers, which entries are weaker than the others
/// taken in the same period.
pub fn relative_to_block(trades: &[Trade]) -> Vec<Trade> {
    let mut by: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for t in trades {
        by.entry(t.block).or_default().push(t.ret);
    }
    let stat: BTreeMap<i64, (f64, f64)> = by
        .into_iter()
        .map(|(b, mut v)| {
            v.sort_by(f64::total_cmp);
            (b, (v.iter().sum::<f64>() / v.len() as f64, v[v.len() / 2]))
        })
        .collect();
    trades
        .iter()
        .map(|t| {
            let (mean, med) = stat[&t.block];
            Trade {
                block: t.block,
                x: t.x.clone(),
                win: t.ret > med,
                ret: t.ret - mean,
            }
        })
        .collect()
}

/// Deterministic label shuffle (the leakage control): the same trades with outcomes assigned at random.
pub fn shuffled(train: &[Trade], seed: u64) -> Vec<Trade> {
    let mut v = train.to_vec();
    let mut st = seed | 1;
    for i in (1..v.len()).rev() {
        st ^= st << 13;
        st ^= st >> 7;
        st ^= st << 17;
        let j = (st % (i as u64 + 1)) as usize;
        let (a, b) = (v[i].win, v[j].win);
        v[i].win = b;
        v[j].win = a;
    }
    v
}

/// The PRE-REGISTERED test, fixed in advance so it cannot be tuned to look good: return-regression trees (default parameters) trained on the earlier two
/// thirds of the blocks, outcomes relative to their own block, keep the top half by predicted return, judged on the later third. It qualifies as a
/// candidate entry gate only when ALL hold: the 95% block-bootstrap interval of the gain is above zero, the gate is better in at least 60% of the test
/// blocks, and the AUC of the win/lose ranking beats the best of `nulls` models trained on shuffled outcomes. Re-run as blocks accumulate (power grows).
#[derive(Debug, Clone)]
pub struct Watch {
    pub trades: usize,
    pub train_blocks: usize,
    pub test_blocks: usize,
    pub verdict: Verdict,
    pub null_max: f64,
    pub qualifies: bool,
    pub rounds: usize,
}

pub fn watch(trades: &[Trade], nulls: usize) -> Option<Watch> {
    if trades.len() < 5000 {
        return None;
    }
    let rel = relative_to_block(trades);
    let (train, test) = split_by_block(&rel, 1.0 / 3.0);
    if train.is_empty() || test.is_empty() {
        return None;
    }
    let blocks = |t: &[Trade]| {
        let mut b: Vec<i64> = t.iter().map(|x| x.block).collect();
        b.sort();
        b.dedup();
        b.len()
    };
    // version 2 of the pre-registered test (2026-10-13): the learning curve showed the trees memorise the training blocks (test log-loss rose with every
    // round), so the number of rounds is now chosen by early stopping on the last quarter of the TRAINING period; nothing else changed
    let (m, rounds) = Gbdt::fit_early_stopped(&train, GbdtParams::default(), true);
    let thr = quantile(&train.iter().map(|t| m.predict(t)).collect::<Vec<_>>(), 0.5);
    let score: Vec<f64> = test.iter().map(|t| m.predict(t)).collect();
    let v = judge("pre-registered return-trees", &test, &score, thr, 11);
    let label: Vec<bool> = test.iter().map(|t| t.win).collect();
    let null_max = (0..nulls as u64)
        .map(|k| {
            let n = Gbdt::fit(&shuffled(&train, 50 + k), GbdtParams::default());
            auc(
                &test.iter().map(|t| n.predict(t)).collect::<Vec<_>>(),
                &label,
            )
        })
        .fold(0.0, f64::max);
    let qualifies = v.diff_lo > 0.0 && v.blocks_better * 10 >= v.blocks * 6 && v.auc > null_max;
    Some(Watch {
        trades: trades.len(),
        train_blocks: blocks(&train),
        test_blocks: blocks(&test),
        verdict: v,
        null_max,
        qualifies,
        rounds,
    })
}

impl Watch {
    pub fn summary(&self) -> String {
        let v = &self.verdict;
        format!("{} trades, {} train / {} test blocks, {} boosting rounds (early stopping): gate gain {:+.2} points per trade [{:+.2}, {:+.2}], better in {}/{} blocks, AUC {:.3} vs best shuffled {:.3} -> {}",
            self.trades, self.train_blocks, self.test_blocks, self.rounds, 100.0 * (v.ret_kept - v.ret_all), 100.0 * v.diff_lo, 100.0 * v.diff_hi, v.blocks_better, v.blocks, v.auc, self.null_max,
            if self.qualifies { "QUALIFIES as a candidate entry gate" } else { "does not qualify (no reliable edge)" })
    }
}

pub fn quantile(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[((s.len() - 1) as f64 * q) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny market where one feature really predicts wins (and another is noise); blocks are time-ordered.
    fn world(n: usize, signal: f64) -> Vec<Trade> {
        let mut st = 12345u64;
        let mut rnd = move || {
            st ^= st << 13;
            st ^= st >> 7;
            st ^= st << 17;
            (st >> 11) as f64 / (1u64 << 53) as f64
        };
        (0..n)
            .map(|i| {
                let (a, b) = (rnd() * 2.0 - 1.0, rnd() * 2.0 - 1.0);
                let p = 0.5 + signal * a;
                let win = rnd() < p;
                Trade {
                    block: (i / (n / 12).max(1)) as i64,
                    x: vec![a, b, f64::NAN],
                    win,
                    ret: if win { 1.0 } else { -1.0 },
                }
            })
            .collect()
    }

    #[test]
    fn relative_labels_remove_the_block_effect() {
        // block 0 is a bull block (everything wins), block 1 a bear block: relative to its own block, the better half wins in both
        let mk = |b: i64, r: f64| Trade {
            block: b,
            x: vec![r],
            win: r > 0.0,
            ret: r,
        };
        let t = vec![
            mk(0, 0.10),
            mk(0, 0.08),
            mk(0, 0.06),
            mk(1, -0.02),
            mk(1, -0.04),
            mk(1, -0.06),
        ];
        let r = relative_to_block(&t);
        assert_eq!(
            r.iter().map(|x| x.win).collect::<Vec<_>>(),
            vec![true, false, false, true, false, false]
        );
        for b in [0, 1] {
            assert!(
                r.iter()
                    .filter(|x| x.block == b)
                    .map(|x| x.ret)
                    .sum::<f64>()
                    .abs()
                    < 1e-12,
                "excess returns sum to zero inside a block"
            );
        }
    }

    #[test]
    fn the_regression_trees_learn_a_return_signal() {
        // returns depend on feature 0 (and not on win/lose labels alone): the squared-loss trees must rank test trades by it
        let mut t = world(6000, 0.0);
        for tr in t.iter_mut() {
            tr.ret = 0.5 * tr.x[0] + 0.05 * tr.x[1];
            tr.win = tr.ret > 0.0;
        }
        let (tr, te) = split_by_block(&t, 1.0 / 3.0);
        let m = Gbdt::fit_with(
            &tr,
            GbdtParams {
                rounds: 60,
                min_leaf: 50,
                ..GbdtParams::default()
            },
            true,
        );
        let pred: Vec<f64> = te.iter().map(|x| m.predict(x)).collect();
        let truth: Vec<f64> = te.iter().map(|x| x.ret).collect();
        let n = pred.len() as f64;
        let (mp, mt) = (pred.iter().sum::<f64>() / n, truth.iter().sum::<f64>() / n);
        let cov: f64 = pred
            .iter()
            .zip(&truth)
            .map(|(a, b)| (a - mp) * (b - mt))
            .sum();
        let corr = cov
            / (pred.iter().map(|a| (a - mp).powi(2)).sum::<f64>().sqrt()
                * truth.iter().map(|b| (b - mt).powi(2)).sum::<f64>().sqrt());
        assert!(corr > 0.9, "correlation {corr:.3}");
    }

    #[test]
    fn the_preregistered_watch_qualifies_real_signal_and_rejects_noise() {
        // 40 blocks of trades; ret depends on feature 0 only when there is signal
        let mk = |signal: f64| {
            let mut t = world(24000, 0.0);
            for (i, tr) in t.iter_mut().enumerate() {
                let noise = ((i * 7919 % 1000) as f64 / 1000.0 - 0.5) * 0.8;
                tr.ret = signal * tr.x[0] + noise;
                tr.win = tr.ret > 0.0;
            }
            t
        };
        let real = watch(&mk(0.6), 4).unwrap();
        assert!(real.qualifies, "{}", real.summary());
        let none = watch(&mk(0.0), 4).unwrap();
        assert!(!none.qualifies, "{}", none.summary());
        assert!(
            watch(&mk(0.6)[..100], 2).is_none(),
            "too few trades to judge"
        );
    }

    #[test]
    fn early_stopping_stops_early_on_noise_and_trains_longer_on_signal() {
        let noise = world(9000, 0.0);
        let (tr, _) = split_by_block(&noise, 1.0 / 3.0);
        let (_, r_noise) = Gbdt::fit_early_stopped(
            &tr,
            GbdtParams {
                min_leaf: 50,
                ..GbdtParams::default()
            },
            false,
        );
        let signal = world(9000, 0.45);
        let (tr2, te2) = split_by_block(&signal, 1.0 / 3.0);
        let (m, r_signal) = Gbdt::fit_early_stopped(
            &tr2,
            GbdtParams {
                min_leaf: 50,
                ..GbdtParams::default()
            },
            false,
        );
        assert!(
            r_noise <= 25,
            "on pure noise it must stop early, got {r_noise} rounds"
        );
        assert!(
            r_signal >= 10 && r_signal > r_noise,
            "with a real signal it should keep learning: {r_signal} vs {r_noise}"
        );
        let a = auc(
            &te2.iter().map(|x| m.predict(x)).collect::<Vec<_>>(),
            &te2.iter().map(|x| x.win).collect::<Vec<_>>(),
        );
        assert!(a > 0.62, "{a}");
    }

    #[test]
    fn auc_known_values() {
        assert!((auc(&[0.1, 0.4, 0.35, 0.8], &[false, false, true, true]) - 0.75).abs() < 1e-12);
        assert!(
            (auc(&[1.0, 1.0, 1.0, 1.0], &[true, false, true, false]) - 0.5).abs() < 1e-12,
            "ties give 0.5"
        );
        assert_eq!(auc(&[1.0, 2.0], &[true, true]), 0.5);
    }

    #[test]
    fn the_split_is_by_time_and_never_mixes_blocks() {
        let t = world(1200, 0.0);
        let (tr, te) = split_by_block(&t, 1.0 / 3.0);
        let (max_tr, min_te) = (
            tr.iter().map(|x| x.block).max().unwrap(),
            te.iter().map(|x| x.block).min().unwrap(),
        );
        assert!(
            max_tr < min_te,
            "train blocks {max_tr} must all be earlier than test blocks {min_te}"
        );
        assert_eq!(tr.len() + te.len(), t.len());
    }

    #[test]
    fn both_models_find_real_signal_and_the_shuffle_control_finds_none() {
        let t = world(6000, 0.35);
        let (tr, te) = split_by_block(&t, 1.0 / 3.0);
        let label: Vec<bool> = te.iter().map(|x| x.win).collect();
        let lg = Logistic::fit(&tr, 150, 0.5, 1e-4);
        let gb = Gbdt::fit(
            &tr,
            GbdtParams {
                rounds: 60,
                min_leaf: 50,
                ..GbdtParams::default()
            },
        );
        let a_lg = auc(
            &te.iter().map(|x| lg.predict(x)).collect::<Vec<_>>(),
            &label,
        );
        let a_gb = auc(
            &te.iter().map(|x| gb.predict(x)).collect::<Vec<_>>(),
            &label,
        );
        assert!(
            a_lg > 0.62 && a_gb > 0.62,
            "logistic {a_lg:.3}, trees {a_gb:.3}"
        );
        // control: a model trained on shuffled outcomes is a RANDOM model. One of them may score 0.4 or 0.6 on a test set that does hold signal (its random
        // weights can point either way), but the average over many shuffles must be 0.5, or the pipeline leaks the test labels.
        let aucs: Vec<f64> = (0..10)
            .map(|k| {
                let m = Gbdt::fit(
                    &shuffled(&tr, 100 + k),
                    GbdtParams {
                        rounds: 40,
                        min_leaf: 50,
                        ..GbdtParams::default()
                    },
                );
                auc(&te.iter().map(|x| m.predict(x)).collect::<Vec<_>>(), &label)
            })
            .collect();
        let mean = aucs.iter().sum::<f64>() / aucs.len() as f64;
        assert!(
            (mean - 0.5).abs() < 0.06,
            "average AUC of shuffled-label models must be about 0.5, got {mean:.3} from {aucs:?}"
        );
        assert!(
            aucs.iter().any(|a| *a < 0.5) && aucs.iter().any(|a| *a > 0.5),
            "random models land on both sides of 0.5: {aucs:?}"
        );
    }

    #[test]
    fn block_bagged_feature_subspace_ensemble_finds_signal() {
        let t = world(9000, 0.4);
        let (tr, te) = split_by_block(&t, 1.0 / 3.0);
        let model = BaggedGbdt::fit(&tr, 5, 73, false);
        let score: Vec<f64> = te.iter().map(|x| model.predict(x)).collect();
        let labels: Vec<bool> = te.iter().map(|x| x.win).collect();
        assert!(auc(&score, &labels) > 0.6);
    }

    #[test]
    fn keeping_high_scores_helps_when_the_signal_is_real_and_the_interval_excludes_zero() {
        let t = world(12000, 0.4);
        let (tr, te) = split_by_block(&t, 1.0 / 3.0);
        let gb = Gbdt::fit(
            &tr,
            GbdtParams {
                rounds: 60,
                min_leaf: 50,
                ..GbdtParams::default()
            },
        );
        let thr = quantile(&tr.iter().map(|x| gb.predict(x)).collect::<Vec<_>>(), 0.5);
        let v = judge(
            "gbdt",
            &te,
            &te.iter().map(|x| gb.predict(x)).collect::<Vec<_>>(),
            thr,
            3,
        );
        assert!(v.ret_kept > v.ret_all && v.diff_lo > 0.0, "{v:?}");
        assert!(v.keep_share > 0.3 && v.keep_share < 0.7, "{v:?}");
    }

    #[test]
    fn pure_noise_gives_no_significant_gate() {
        let t = world(12000, 0.0);
        let (tr, te) = split_by_block(&t, 1.0 / 3.0);
        let lg = Logistic::fit(&tr, 100, 0.5, 1e-4);
        let thr = quantile(&tr.iter().map(|x| lg.predict(x)).collect::<Vec<_>>(), 0.5);
        let v = judge(
            "lg",
            &te,
            &te.iter().map(|x| lg.predict(x)).collect::<Vec<_>>(),
            thr,
            3,
        );
        assert!(
            v.diff_lo <= 0.0 && v.diff_hi >= 0.0,
            "an interval for pure noise must include zero: {v:?}"
        );
    }
}

//! Deep learning for the trading model, written from scratch on the lab's own autodiff engine (owner 2026-10-13: "make it as advanced as possible", "a proper AI model
//! for trading"). Two architectures:
//!   * `Res`: a residual network (pre-layer-norm blocks, GELU, dropout) on the static features (the lab's candle features, the bot's score components, SPY-regime terms);
//!   * `Transformer`: self-attention over the last `seq_len` bars of the stock's own candle and INDICATOR channels (`indicators.rs`), combined with the static features -
//!     it reads the candles themselves and learns its own patterns.
//!
//! Both are trained with a loss that optimises the thing the bot uses, the CROSS-SECTIONAL rank correlation within each moment (1 - Pearson per moment, plus a small
//! squared-error anchor), by AdamW with warm-up and a cosine schedule, global gradient clipping, dropout, data-parallel threads, and early stopping on the latest fifth of
//! the training period. Deterministic for a seed. Whether they help is decided by the trial ledger (paired, multiple-testing-adjusted), never assumed.

use crate::autodiff::{Mat, Tape, V};
use crate::candles::Bar;
use crate::indicators;
use crate::mlcandle::Sample;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Kind { Res, Transformer }

/// One candidate architecture + training recipe (the unit the automatic search mutates).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Genome {
    pub kind: Kind, pub width: usize, pub blocks: usize, pub heads: usize, pub dropout: f64, pub lr: f64, pub weight_decay: f64,
    pub epochs: usize, pub seq_len: usize, pub moments_per_batch: usize, pub mse_weight: f64,
}

impl Genome {
    pub fn label(&self) -> String {
        match self.kind {
            Kind::Res => format!("ResNet w{} x{} drop{:.3} lr{:.6} wd{:.5} ep{} batch{} mse{:.3}", self.width, self.blocks, self.dropout, self.lr, self.weight_decay, self.epochs, self.moments_per_batch, self.mse_weight),
            Kind::Transformer => format!("Transformer d{} h{} x{} L{} drop{:.3} lr{:.6} wd{:.5} ep{} batch{} mse{:.3}", self.width, self.heads, self.blocks, self.seq_len, self.dropout, self.lr, self.weight_decay, self.epochs, self.moments_per_batch, self.mse_weight),
        }
    }
}

struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng { Rng(seed.wrapping_mul(2).wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1) }
    fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 }
    fn unit(&mut self) -> f64 { (self.next() >> 11) as f64 / (1u64 << 53) as f64 }
}

fn glorot(rng: &mut Rng, r: usize, c: usize) -> Mat { let a = (6.0 / (r + c) as f64).sqrt(); Mat::from(r, c, (0..r * c).map(|_| (rng.unit() * 2.0 - 1.0) * a).collect()) }
fn ones(c: usize) -> Mat { Mat::from(1, c, vec![1.0; c]) }
fn zeros(r: usize, c: usize) -> Mat { Mat::zeros(r, c) }

// ------------------------------------------------------------------ data
/// One training/evaluation row: the moment, the standardised-later static features, the candle+indicator window (f32 to save memory), and the label.
#[derive(Debug, Clone)]
pub struct Row { pub sym: usize, pub ts: i64, pub stat: Vec<f64>, pub seq: Vec<f32>, pub y: f64 }

pub const SEQ_F: usize = indicators::N + 1; // the indicator channels plus the one-bar log return

/// Rows for `samples` (the universe samples of `mlcandle::build` + extensions): static features as given, and a window of `seq_len` bars of indicator channels ending
/// at the sample's decision bar, from the stock's own candles.
pub fn build_rows(series: &BTreeMap<String, Vec<Bar>>, samples: &[Sample], seq_len: usize) -> Vec<Row> {
    let names: Vec<&String> = series.keys().collect();
    let ind: Vec<Vec<[f64; indicators::N]>> = series.values().map(|b| indicators::compute(b)).collect();
    let bars: Vec<&Vec<Bar>> = series.values().collect();
    samples.iter().filter_map(|s| {
        let _ = names[s.sym];
        let b = bars[s.sym];
        let i = b.binary_search_by_key(&s.ts, |x| x.ts).ok()?;
        if i + 1 < seq_len + 1 { return None; }
        let mut seq = Vec::with_capacity(seq_len * SEQ_F);
        for k in i + 1 - seq_len..=i {
            seq.extend(ind[s.sym][k].iter().map(|v| *v as f32));
            seq.push(((b[k].close / b[k - 1].close).ln() * 100.0) as f32);
        }
        Some(Row { sym: s.sym, ts: s.ts, stat: s.x.clone(), seq, y: s.excess })
    }).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Norm { stat_mu: Vec<f64>, stat_sd: Vec<f64>, seq_mu: Vec<f64>, seq_sd: Vec<f64> }

fn fit_norm(rows: &[&Row], seq_len: usize) -> Norm {
    let d = rows[0].stat.len(); let n = rows.len() as f64;
    let stat_mu: Vec<f64> = (0..d).map(|j| rows.iter().map(|r| r.stat[j]).sum::<f64>() / n).collect();
    let stat_sd: Vec<f64> = (0..d).map(|j| (rows.iter().map(|r| (r.stat[j] - stat_mu[j]).powi(2)).sum::<f64>() / n).sqrt().max(1e-9)).collect();
    let (mut seq_mu, mut seq_sd) = (vec![0.0; SEQ_F], vec![1.0; SEQ_F]);
    if rows[0].seq.len() == seq_len * SEQ_F {
        let cnt = (rows.len() * seq_len) as f64;
        for f in 0..SEQ_F { let mut s = 0.0; for r in rows { for t in 0..seq_len { s += r.seq[t * SEQ_F + f] as f64; } } seq_mu[f] = s / cnt; }
        for f in 0..SEQ_F { let mut s = 0.0; for r in rows { for t in 0..seq_len { s += (r.seq[t * SEQ_F + f] as f64 - seq_mu[f]).powi(2); } } seq_sd[f] = (s / cnt).sqrt().max(1e-9); }
    }
    Norm { stat_mu, stat_sd, seq_mu, seq_sd }
}

// ------------------------------------------------------------------ parameters and forward passes
pub fn init_params(g: &Genome, d: usize, seed: u64) -> Vec<Mat> {
    let mut rng = Rng::new(seed);
    let (w, mut p) = (g.width, vec![]);
    match g.kind {
        Kind::Res => {
            p.push(glorot(&mut rng, d, w)); p.push(zeros(1, w));
            for _ in 0..g.blocks { p.push(ones(w)); p.push(zeros(1, w)); p.push(glorot(&mut rng, w, 2 * w)); p.push(zeros(1, 2 * w)); p.push(glorot(&mut rng, 2 * w, w)); p.push(zeros(1, w)); }
            p.push(ones(w)); p.push(zeros(1, w)); p.push(glorot(&mut rng, w, 1)); p.push(zeros(1, 1));
        }
        Kind::Transformer => {
            p.push(glorot(&mut rng, SEQ_F, w)); p.push(zeros(1, w)); p.push(Mat::from(g.seq_len, w, (0..g.seq_len * w).map(|_| (rng.unit() - 0.5) * 0.1).collect()));
            for _ in 0..g.blocks {
                p.push(ones(w)); p.push(zeros(1, w));
                for _ in 0..4 { p.push(glorot(&mut rng, w, w)); }
                p.push(zeros(1, w));
                p.push(ones(w)); p.push(zeros(1, w));
                p.push(glorot(&mut rng, w, 2 * w)); p.push(zeros(1, 2 * w)); p.push(glorot(&mut rng, 2 * w, w)); p.push(zeros(1, w));
            }
            p.push(ones(w)); p.push(zeros(1, w));
            p.push(glorot(&mut rng, d, w)); p.push(zeros(1, w));
            p.push(glorot(&mut rng, 3 * w, w)); p.push(zeros(1, w)); p.push(glorot(&mut rng, w, 1)); p.push(zeros(1, 1));
        }
    }
    p
}

struct Cur(usize);
impl Cur { fn n(&mut self) -> usize { self.0 += 1; self.0 - 1 } }

fn keep_mask(rng: &mut Rng, n: usize, p: f64) -> Vec<bool> { (0..n).map(|_| rng.unit() >= p).collect() }

/// Residual network on a batch matrix (n x d) -> (n x 1).
fn forward_res(t: &mut Tape, params: &[Mat], g: &Genome, x: V, mut drop: Option<&mut Rng>) -> V {
    let mut c = Cur(0);
    let leaf = |t: &mut Tape, i: usize| t.param(params[i].clone(), i);
    let (w0, b0) = (leaf(t, c.n()), leaf(t, c.n()));
    let h0 = t.matmul(x, w0); let mut h = t.add_row(h0, b0);
    for _ in 0..g.blocks {
        let (g1, b1, w1, bb1, w2, bb2) = (leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()));
        let z = t.layer_norm(h, g1, b1);
        let z = t.matmul(z, w1); let z = t.add_row(z, bb1); let mut z = t.gelu(z);
        if let (Some(r), true) = (drop.as_deref_mut(), g.dropout > 0.0) { let n = t.val(z).d.len(); let k = keep_mask(r, n, g.dropout); z = t.dropout(z, &k, g.dropout); }
        let z = t.matmul(z, w2); let z = t.add_row(z, bb2);
        h = t.add(h, z);
    }
    let (gf, bf, wh, bh) = (leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()));
    let z = t.layer_norm(h, gf, bf); let z = t.matmul(z, wh); t.add_row(z, bh)
}

/// Transformer on ONE sample: the sequence (L x F) and the static row (1 x d) -> (1 x 1).
fn forward_tr(t: &mut Tape, params: &[Mat], g: &Genome, seq: V, stat: V, mut drop: Option<&mut Rng>) -> V {
    let mut c = Cur(0);
    let leaf = |t: &mut Tape, i: usize| t.param(params[i].clone(), i);
    let d = g.width; let dk = d / g.heads;
    let (we, be, pos) = (leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()));
    let x0 = t.matmul(seq, we); let x0 = t.add_row(x0, be); let mut x = t.add(x0, pos);
    for _ in 0..g.blocks {
        let (g1, b1, wq, wk, wv, wo, bo, g2, b2, f1, fb1, f2, fb2) = (leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()));
        let z = t.layer_norm(x, g1, b1);
        let (q, k, v) = (t.matmul(z, wq), t.matmul(z, wk), t.matmul(z, wv));
        let mut heads = vec![];
        for h in 0..g.heads {
            let (qh, kh, vh) = (t.slice_cols(q, h * dk, dk), t.slice_cols(k, h * dk, dk), t.slice_cols(v, h * dk, dk));
            let s = t.matmul_bt(qh, kh); let s = t.scale(s, 1.0 / (dk as f64).sqrt()); let a = t.softmax_rows(s);
            heads.push(t.matmul(a, vh));
        }
        let ctx = if heads.len() == 1 { heads[0] } else { t.concat_cols(&heads) };
        let o = t.matmul(ctx, wo); let o = t.add_row(o, bo);
        x = t.add(x, o);
        let z = t.layer_norm(x, g2, b2);
        let z = t.matmul(z, f1); let z = t.add_row(z, fb1); let mut z = t.gelu(z);
        if let (Some(r), true) = (drop.as_deref_mut(), g.dropout > 0.0) { let n = t.val(z).d.len(); let kp = keep_mask(r, n, g.dropout); z = t.dropout(z, &kp, g.dropout); }
        let z = t.matmul(z, f2); let z = t.add_row(z, fb2);
        x = t.add(x, z);
    }
    let (gf, bf, ws, bs, h1, hb1, h2, hb2) = (leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()), leaf(t, c.n()));
    let x = t.layer_norm(x, gf, bf);
    let (mean, last) = (t.mean_rows(x), t.row(x, g.seq_len - 1));
    let s = t.matmul(stat, ws); let s = t.add_row(s, bs); let s = t.gelu(s);
    let cat = t.concat_cols(&[mean, last, s]);
    let z = t.matmul(cat, h1); let z = t.add_row(z, hb1); let mut z = t.gelu(z);
    if let (Some(r), true) = (drop, g.dropout > 0.0) { let n = t.val(z).d.len(); let kp = keep_mask(r, n, g.dropout); z = t.dropout(z, &kp, g.dropout); }
    let z = t.matmul(z, h2); t.add_row(z, hb2)
}

fn stat_mat(r: &Row, n: &Norm) -> Mat { Mat::from(1, r.stat.len(), r.stat.iter().enumerate().map(|(j, v)| ((v - n.stat_mu[j]) / n.stat_sd[j]).clamp(-5.0, 5.0)).collect()) }
fn seq_mat(r: &Row, n: &Norm, l: usize) -> Mat { Mat::from(l, SEQ_F, (0..l * SEQ_F).map(|k| ((r.seq[k] as f64 - n.seq_mu[k % SEQ_F]) / n.seq_sd[k % SEQ_F]).clamp(-6.0, 6.0)).collect()) }

/// Forward over the rows of ONE group (a moment, or any set), returning the predictions (n x 1) node.
fn forward_group(t: &mut Tape, params: &[Mat], g: &Genome, norm: &Norm, rows: &[&Row], drop: Option<&mut Rng>) -> V {
    match g.kind {
        Kind::Res => {
            let d = rows[0].stat.len();
            let mut data = Vec::with_capacity(rows.len() * d);
            for r in rows { data.extend(stat_mat(r, norm).d); }
            let x = t.input(Mat::from(rows.len(), d, data));
            forward_res(t, params, g, x, drop)
        }
        Kind::Transformer => {
            let mut drop = drop;
            let outs: Vec<V> = rows.iter().map(|r| { let s = t.input(seq_mat(r, norm, g.seq_len)); let st = t.input(stat_mat(r, norm)); forward_tr(t, params, g, s, st, drop.as_deref_mut()) }).collect();
            t.concat_rows(&outs)
        }
    }
}

// ------------------------------------------------------------------ training
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepModel { pub genome: Genome, pub params: Vec<Mat>, pub norm: Norm, pub val_ic: f64, pub epochs_run: usize }

fn moments(rows: &[&Row]) -> Vec<Vec<usize>> {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() { by.entry(r.ts).or_default().push(i); }
    by.into_values().filter(|v| v.len() >= 8).collect()
}

fn ic_of(model: &DeepModel, rows: &[&Row]) -> f64 {
    let pred = predict_refs(model, rows);
    let ms = moments(rows);
    if ms.is_empty() { return 0.0; }
    ms.iter().map(|idx| crate::brain::spearman(&idx.iter().map(|&i| pred[i]).collect::<Vec<_>>(), &idx.iter().map(|&i| rows[i].y).collect::<Vec<_>>())).sum::<f64>() / ms.len() as f64
}

/// Loss and parameter gradients of one shard of moments.
fn shard_grads(params: &[Mat], g: &Genome, norm: &Norm, rows: &[&Row], ms: &[&Vec<usize>], seed: u64) -> (f64, Vec<Mat>) {
    let mut t = Tape::new();
    let mut rng = Rng::new(seed);
    let mut total: Option<V> = None;
    for idx in ms {
        let group: Vec<&Row> = idx.iter().map(|&i| rows[i]).collect();
        let y: Vec<f64> = group.iter().map(|r| r.y).collect();
        let mu = y.iter().sum::<f64>() / y.len() as f64;
        let sd = (y.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / y.len() as f64).sqrt().max(1e-9);
        let ys: Vec<f64> = y.iter().map(|v| (v - mu) / sd).collect();
        let p = forward_group(&mut t, params, g, norm, &group, Some(&mut rng));
        let ic = t.pearson_loss(p, &ys);
        let l = if g.mse_weight > 0.0 { let m = t.mse(p, &ys); let m = t.scale(m, g.mse_weight); t.add(ic, m) } else { ic };
        total = Some(match total { Some(a) => t.add(a, l), None => l });
    }
    let Some(total) = total else { return (0.0, params.iter().map(|m| Mat::zeros(m.r, m.c)).collect()) };
    let loss = t.val(total).d[0];
    t.backward(total);
    let mut grads: Vec<Mat> = params.iter().map(|m| Mat::zeros(m.r, m.c)).collect();
    for (i, gm) in t.param_grads() { for (a, b) in grads[i].d.iter_mut().zip(&gm.d) { *a += b; } }
    (loss, grads)
}

/// Trains one model. `threads` shards each batch of moments across threads (data parallel, gradients summed). Returns the best-by-validation parameters.
pub fn train(rows: &[&Row], g: &Genome, seed: u64, threads: usize) -> DeepModel {
    let mut sorted: Vec<&Row> = rows.to_vec(); sorted.sort_by_key(|r| r.ts);
    let split = sorted.len() * 4 / 5;
    // the validation moments start 1 session after the last training moment (a purge, so labels do not overlap)
    let cut_ts = sorted[split].ts;
    let train_rows: Vec<&Row> = sorted.iter().filter(|r| r.ts < cut_ts - 86_400_000).copied().collect();
    let val_rows: Vec<&Row> = sorted.iter().filter(|r| r.ts >= cut_ts).copied().collect();
    let norm = fit_norm(&train_rows, g.seq_len);
    let mut params = init_params(g, train_rows[0].stat.len(), seed);
    let (mut m, mut v): (Vec<Mat>, Vec<Mat>) = (params.iter().map(|p| Mat::zeros(p.r, p.c)).collect(), params.iter().map(|p| Mat::zeros(p.r, p.c)).collect());
    let ms = moments(&train_rows);
    let steps_per_epoch = ms.len().div_ceil(g.moments_per_batch).max(1);
    let total_steps = (g.epochs * steps_per_epoch) as f64;
    let warm = (steps_per_epoch as f64).max(5.0);
    let (mut best, mut best_params, mut best_epoch, mut step) = (f64::MIN, params.clone(), 0usize, 0usize);
    let mut rng = Rng::new(seed ^ 0xABCD);
    let mut order: Vec<usize> = (0..ms.len()).collect();
    let mut stale = 0;
    for epoch in 0..g.epochs {
        for i in (1..order.len()).rev() { let j = (rng.next() % (i as u64 + 1)) as usize; order.swap(i, j); }
        for batch in order.chunks(g.moments_per_batch) {
            let lr = if (step as f64) < warm { g.lr * (step as f64 + 1.0) / warm } else { g.lr * (0.1 + 0.9 * 0.5 * (1.0 + (std::f64::consts::PI * (step as f64 - warm) / (total_steps - warm).max(1.0)).cos())) };
            let groups: Vec<&Vec<usize>> = batch.iter().map(|&i| &ms[i]).collect();
            let shards: Vec<Vec<&Vec<usize>>> = (0..threads.max(1)).map(|t| groups.iter().skip(t).step_by(threads.max(1)).copied().collect()).filter(|s: &Vec<&Vec<usize>>| !s.is_empty()).collect();
            let results: Vec<(f64, Vec<Mat>)> = std::thread::scope(|sc| {
                let handles: Vec<_> = shards.iter().enumerate().map(|(k, s)| { let (params, norm, train_rows, g) = (&params, &norm, &train_rows, g); let seed = seed.wrapping_add((step * 131 + k) as u64); sc.spawn(move || shard_grads(params, g, norm, train_rows, s, seed)) }).collect();
                handles.into_iter().map(|h| h.join().expect("shard")).collect()
            });
            // sum the shards, divide by the number of moments (the mean loss), clip the global norm
            let mut grads: Vec<Mat> = params.iter().map(|p| Mat::zeros(p.r, p.c)).collect();
            for (_, gs) in &results { for (a, b) in grads.iter_mut().zip(gs) { for (x, y) in a.d.iter_mut().zip(&b.d) { *x += y / groups.len() as f64; } } }
            let norm2: f64 = grads.iter().map(|gm| gm.d.iter().map(|x| x * x).sum::<f64>()).sum::<f64>().sqrt();
            let clip = if norm2 > 1.0 { 1.0 / norm2 } else { 1.0 };
            step += 1;
            let (b1, b2, eps) = (0.9f64, 0.999f64, 1e-8);
            for i in 0..params.len() {
                let decay = if params[i].r > 1 { g.weight_decay } else { 0.0 };
                for k in 0..params[i].d.len() {
                    let gk = grads[i].d[k] * clip;
                    m[i].d[k] = b1 * m[i].d[k] + (1.0 - b1) * gk; v[i].d[k] = b2 * v[i].d[k] + (1.0 - b2) * gk * gk;
                    let (mh, vh) = (m[i].d[k] / (1.0 - b1.powi(step as i32)), v[i].d[k] / (1.0 - b2.powi(step as i32)));
                    params[i].d[k] -= lr * (mh / (vh.sqrt() + eps) + decay * params[i].d[k]);
                }
            }
        }
        let probe = DeepModel { genome: *g, params: params.clone(), norm: norm.clone(), val_ic: 0.0, epochs_run: epoch + 1 };
        let ic = ic_of(&probe, &val_rows);
        if ic > best { best = ic; best_params = params.clone(); best_epoch = epoch + 1; stale = 0; } else { stale += 1; if stale >= 4 { break; } }
    }
    DeepModel { genome: *g, params: best_params, norm, val_ic: best, epochs_run: best_epoch }
}

fn predict_refs(model: &DeepModel, rows: &[&Row]) -> Vec<f64> {
    let g = &model.genome;
    match g.kind {
        Kind::Res => { let mut out = vec![]; for chunk in rows.chunks(256) { let mut t = Tape::new(); let p = forward_group(&mut t, &model.params, g, &model.norm, chunk, None); out.extend(t.val(p).d.clone()); } out }
        Kind::Transformer => rows.iter().map(|r| { let mut t = Tape::new(); let p = forward_group(&mut t, &model.params, g, &model.norm, &[r], None); t.val(p).d[0] }).collect(),
    }
}
pub fn predict(model: &DeepModel, rows: &[Row]) -> Vec<f64> { predict_refs(model, &rows.iter().collect::<Vec<_>>()) }

#[cfg(test)]
mod tests {
    use super::*;

    fn gauss(rng: &mut Rng) -> f64 { let (a, b) = (rng.unit().max(1e-12), rng.unit()); (-2.0 * a.ln()).sqrt() * (2.0 * std::f64::consts::PI * b).cos() }

    /// `moments` moments of 12 stocks: static x (4 features), label = sign(x0*x1) + noise (invisible to any linear model); sequences are noise unless `seq_rule`.
    fn make(moments: usize, seed: u64, seq_len: usize, seq_rule: bool) -> Vec<Row> {
        let mut rng = Rng::new(seed);
        let mut rows = vec![];
        for m in 0..moments { for _ in 0..12 {
            let stat: Vec<f64> = (0..4).map(|_| gauss(&mut rng)).collect();
            let seq: Vec<f32> = (0..seq_len * SEQ_F).map(|_| gauss(&mut rng) as f32).collect();
            let y = if seq_rule { seq[(seq_len - 3) * SEQ_F] as f64 - seq[(seq_len - 1) * SEQ_F] as f64 + 0.3 * gauss(&mut rng) } else { (if stat[0] * stat[1] > 0.0 { 1.0 } else { -1.0 }) + 0.2 * gauss(&mut rng) };
            rows.push(Row { sym: 0, ts: m as i64 * 86_400_000, stat, seq, y });
        } }
        rows
    }
    fn genome(kind: Kind) -> Genome { Genome { kind, width: 16, blocks: 2, heads: 2, dropout: 0.0, lr: 0.01, weight_decay: 0.0, epochs: 40, seq_len: 6, moments_per_batch: 8, mse_weight: 0.1 } }
    fn test_ic(model: &DeepModel, rows: &[Row]) -> f64 { ic_of(model, &rows.iter().collect::<Vec<_>>()) }

    #[test]
    fn genome_label_distinguishes_batch_size_and_learning_rate() {
        let a = genome(Kind::Res);
        assert_ne!(a.label(), Genome { moments_per_batch: 12, ..a }.label());
        assert_ne!(a.label(), Genome { lr: a.lr + 0.00001, ..a }.label());
    }

    #[test]
    fn the_residual_network_learns_a_nonlinear_cross_sectional_rule_a_linear_model_cannot() {
        let (train, test) = (make(90, 1, 6, false), make(40, 2, 6, false));
        let refs: Vec<&Row> = train.iter().collect();
        let model = train_model(&refs, &genome(Kind::Res), 3);
        let ic = test_ic(&model, &test);
        assert!(ic > 0.5, "held-out rank IC of the residual network: {ic}");
        // a ridge on the same static features finds nothing
        let samples: Vec<Sample> = train.iter().map(|r| Sample { ts: r.ts, week: 0, sym: 0, x: r.stat.clone(), excess: r.y, raw: r.y }).collect();
        let ridge = crate::mlcandle::Ridge::fit(&samples, 1.0);
        let preds: Vec<f64> = test.iter().map(|r| ridge.predict(&Sample { ts: r.ts, week: 0, sym: 0, x: r.stat.clone(), excess: 0.0, raw: 0.0 })).collect();
        let ms = moments(&test.iter().collect::<Vec<_>>());
        let lin = ms.iter().map(|idx| crate::brain::spearman(&idx.iter().map(|&i| preds[i]).collect::<Vec<_>>(), &idx.iter().map(|&i| test[i].y).collect::<Vec<_>>())).sum::<f64>() / ms.len() as f64;
        assert!(lin.abs() < 0.15, "the linear model must not: {lin}");
    }

    fn train_model(rows: &[&Row], g: &Genome, seed: u64) -> DeepModel { train(rows, g, seed, 2) }

    #[test]
    fn the_transformer_reads_the_candle_sequence_and_finds_a_pattern_in_time() {
        // the label is (value 3 bars ago) - (latest value) of one channel: only a model that looks at the sequence can find it
        let (train, test) = (make(80, 5, 6, true), make(30, 6, 6, true));
        let mut g = genome(Kind::Transformer);
        g.width = 8; g.blocks = 1; g.epochs = 45; g.lr = 0.01;
        let model = train_model(&train.iter().collect::<Vec<_>>(), &g, 7);
        let ic = test_ic(&model, &test);
        assert!(ic > 0.4, "held-out rank IC of the Transformer: {ic} (epochs {})", model.epochs_run);
    }

    #[test]
    fn it_is_deterministic_serialisable_and_thread_count_does_not_change_the_gradients() {
        let rows = make(20, 9, 6, false);
        let refs: Vec<&Row> = rows.iter().collect();
        let g = Genome { epochs: 3, ..genome(Kind::Res) };
        let (a, b) = (train(&refs, &g, 4, 1), train(&refs, &g, 4, 1));
        assert_eq!(predict(&a, &rows[..5]), predict(&b, &rows[..5]), "same seed, same network");
        let back: DeepModel = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(predict(&back, &rows[..5]), predict(&a, &rows[..5]), "saved and loaded networks predict identically");
        // gradients of the same moments computed in one shard and in two shards add up to the same thing
        let norm = fit_norm(&refs, 6);
        let params = init_params(&g, 4, 11);
        let ms = moments(&refs);
        let all: Vec<&Vec<usize>> = ms.iter().take(4).collect();
        let (l1, g1) = shard_grads(&params, &g, &norm, &refs, &all, 5);
        let (l2a, g2a) = shard_grads(&params, &g, &norm, &refs, &all[..2], 5);
        let (l2b, g2b) = shard_grads(&params, &g, &norm, &refs, &all[2..], 5);
        assert!((l1 - (l2a + l2b)).abs() < 1e-9);
        for ((x, y), z) in g1.iter().zip(&g2a).zip(&g2b) { for k in 0..x.d.len() { assert!((x.d[k] - (y.d[k] + z.d[k])).abs() < 1e-9); } }
    }
}

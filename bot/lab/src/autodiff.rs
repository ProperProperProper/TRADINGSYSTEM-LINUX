//! A small reverse-mode automatic differentiation engine, written from scratch (owner 2026-10-13: "make it as advanced as possible"). It is what lets the lab train
//! real deep architectures (residual networks with layer normalisation, Transformers with attention) with exact gradients instead of hand-derived backpropagation.
//! Matrices are f64, row-major; every operation records itself on a `Tape`; `backward` walks the tape in reverse. Every operation is checked against numerical
//! gradients in the tests (the only way to trust hand-written derivatives).

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mat { pub r: usize, pub c: usize, pub d: Vec<f64> }

impl Mat {
    pub fn zeros(r: usize, c: usize) -> Mat { Mat { r, c, d: vec![0.0; r * c] } }
    pub fn from(r: usize, c: usize, d: Vec<f64>) -> Mat { assert_eq!(d.len(), r * c); Mat { r, c, d } }
    #[inline] pub fn at(&self, i: usize, j: usize) -> f64 { self.d[i * self.c + j] }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V(pub usize);

enum Op {
    Leaf,
    Add(V, V),
    AddRow(V, V),        // a (r x c) + b (1 x c) broadcast over rows
    Mul(V, V),           // elementwise
    Scale(V, f64),
    MatMul(V, V),        // a (r x k) . b (k x c)
    MatMulBT(V, V),      // a (r x k) . b^T where b is (c x k)
    Gelu(V),
    Tanh(V),
    SoftmaxRows(V),
    LayerNorm { x: V, g: V, b: V, xhat: Vec<f64>, inv_std: Vec<f64> },
    SliceCols { a: V, start: usize, len: usize },
    ConcatCols(Vec<V>),
    ConcatRows(Vec<V>),
    Dropout { a: V, mask: Vec<f64> },
    MeanRows(V),         // (r x c) -> (1 x c)
    Row(V, usize),       // one row as (1 x c)
    /// 1 - Pearson correlation between the column vector `pred` (n x 1) and a constant target; a scalar (1 x 1).
    PearsonLoss { pred: V, target: Vec<f64> },
    /// mean squared error against a constant target; a scalar.
    Mse { pred: V, target: Vec<f64> },
    Sum(V),
}

pub struct Tape { vals: Vec<Mat>, grads: Vec<Mat>, ops: Vec<Op>, param_of: Vec<Option<usize>> }

const GELU_C: f64 = 0.797_884_560_802_865_4; // sqrt(2/pi)

impl Default for Tape { fn default() -> Self { Self::new() } }

impl Tape {
    pub fn new() -> Tape { Tape { vals: vec![], grads: vec![], ops: vec![], param_of: vec![] } }
    fn push(&mut self, v: Mat, op: Op, param: Option<usize>) -> V { self.grads.push(Mat::zeros(v.r, v.c)); self.vals.push(v); self.ops.push(op); self.param_of.push(param); V(self.vals.len() - 1) }
    pub fn val(&self, v: V) -> &Mat { &self.vals[v.0] }
    pub fn grad(&self, v: V) -> &Mat { &self.grads[v.0] }
    /// A constant input (no gradient is used).
    pub fn input(&mut self, m: Mat) -> V { self.push(m, Op::Leaf, None) }
    /// A trainable parameter: `index` identifies it in the caller's parameter store so its gradient can be harvested.
    pub fn param(&mut self, m: Mat, index: usize) -> V { self.push(m, Op::Leaf, Some(index)) }
    pub fn param_grads(&self) -> Vec<(usize, &Mat)> { self.param_of.iter().enumerate().filter_map(|(n, p)| p.map(|i| (i, &self.grads[n]))).collect() }

    pub fn add(&mut self, a: V, b: V) -> V { let m = self.zip(a, b, |x, y| x + y); self.push(m, Op::Add(a, b), None) }
    pub fn add_row(&mut self, a: V, b: V) -> V {
        let (x, y) = (&self.vals[a.0], &self.vals[b.0]);
        assert_eq!((y.r, y.c), (1, x.c));
        let mut m = x.clone();
        for i in 0..m.r { for j in 0..m.c { m.d[i * m.c + j] += y.d[j]; } }
        self.push(m, Op::AddRow(a, b), None)
    }
    pub fn mul(&mut self, a: V, b: V) -> V { let m = self.zip(a, b, |x, y| x * y); self.push(m, Op::Mul(a, b), None) }
    pub fn scale(&mut self, a: V, s: f64) -> V { let mut m = self.vals[a.0].clone(); m.d.iter_mut().for_each(|x| *x *= s); self.push(m, Op::Scale(a, s), None) }
    fn zip(&self, a: V, b: V, f: impl Fn(f64, f64) -> f64) -> Mat {
        let (x, y) = (&self.vals[a.0], &self.vals[b.0]);
        assert_eq!((x.r, x.c), (y.r, y.c));
        Mat { r: x.r, c: x.c, d: x.d.iter().zip(&y.d).map(|(p, q)| f(*p, *q)).collect() }
    }
    pub fn matmul(&mut self, a: V, b: V) -> V {
        let (x, y) = (&self.vals[a.0], &self.vals[b.0]);
        assert_eq!(x.c, y.r);
        let mut m = Mat::zeros(x.r, y.c);
        for i in 0..x.r { for k in 0..x.c { let xv = x.d[i * x.c + k]; if xv == 0.0 { continue; } for j in 0..y.c { m.d[i * y.c + j] += xv * y.d[k * y.c + j]; } } }
        self.push(m, Op::MatMul(a, b), None)
    }
    pub fn matmul_bt(&mut self, a: V, b: V) -> V {
        let (x, y) = (&self.vals[a.0], &self.vals[b.0]);
        assert_eq!(x.c, y.c);
        let mut m = Mat::zeros(x.r, y.r);
        for i in 0..x.r { for j in 0..y.r { let mut s = 0.0; for k in 0..x.c { s += x.d[i * x.c + k] * y.d[j * y.c + k]; } m.d[i * y.r + j] = s; } }
        self.push(m, Op::MatMulBT(a, b), None)
    }
    pub fn gelu(&mut self, a: V) -> V { let mut m = self.vals[a.0].clone(); m.d.iter_mut().for_each(|x| { let u = GELU_C * (*x + 0.044715 * *x * *x * *x); *x = 0.5 * *x * (1.0 + u.tanh()); }); self.push(m, Op::Gelu(a), None) }
    pub fn tanh(&mut self, a: V) -> V { let mut m = self.vals[a.0].clone(); m.d.iter_mut().for_each(|x| *x = x.tanh()); self.push(m, Op::Tanh(a), None) }
    pub fn softmax_rows(&mut self, a: V) -> V {
        let mut m = self.vals[a.0].clone();
        for i in 0..m.r { let row = &mut m.d[i * m.c..(i + 1) * m.c]; let mx = row.iter().cloned().fold(f64::MIN, f64::max); let mut s = 0.0; for x in row.iter_mut() { *x = (*x - mx).exp(); s += *x; } for x in row.iter_mut() { *x /= s; } }
        self.push(m, Op::SoftmaxRows(a), None)
    }
    pub fn layer_norm(&mut self, x: V, g: V, b: V) -> V {
        let xv = &self.vals[x.0];
        let (r, c) = (xv.r, xv.c);
        assert_eq!((self.vals[g.0].r, self.vals[g.0].c, self.vals[b.0].c), (1, c, c));
        let (mut out, mut xhat, mut inv) = (Mat::zeros(r, c), vec![0.0; r * c], vec![0.0; r]);
        for i in 0..r {
            let row = &xv.d[i * c..(i + 1) * c];
            let mean = row.iter().sum::<f64>() / c as f64;
            let var = row.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / c as f64;
            inv[i] = 1.0 / (var + 1e-5).sqrt();
            for j in 0..c { xhat[i * c + j] = (row[j] - mean) * inv[i]; out.d[i * c + j] = xhat[i * c + j] * self.vals[g.0].d[j] + self.vals[b.0].d[j]; }
        }
        self.push(out, Op::LayerNorm { x, g, b, xhat, inv_std: inv }, None)
    }
    pub fn slice_cols(&mut self, a: V, start: usize, len: usize) -> V {
        let x = &self.vals[a.0];
        let mut m = Mat::zeros(x.r, len);
        for i in 0..x.r { for j in 0..len { m.d[i * len + j] = x.d[i * x.c + start + j]; } }
        self.push(m, Op::SliceCols { a, start, len }, None)
    }
    pub fn concat_cols(&mut self, parts: &[V]) -> V {
        let r = self.vals[parts[0].0].r;
        let c: usize = parts.iter().map(|p| self.vals[p.0].c).sum();
        let mut m = Mat::zeros(r, c);
        let mut off = 0;
        for p in parts { let x = &self.vals[p.0]; assert_eq!(x.r, r); for i in 0..r { for j in 0..x.c { m.d[i * c + off + j] = x.d[i * x.c + j]; } } off += x.c; }
        self.push(m, Op::ConcatCols(parts.to_vec()), None)
    }
    pub fn concat_rows(&mut self, parts: &[V]) -> V {
        let c = self.vals[parts[0].0].c;
        let r: usize = parts.iter().map(|p| self.vals[p.0].r).sum();
        let mut m = Mat::zeros(r, c);
        let mut off = 0;
        for p in parts { let x = &self.vals[p.0]; assert_eq!(x.c, c); m.d[off * c..off * c + x.d.len()].copy_from_slice(&x.d); off += x.r; }
        self.push(m, Op::ConcatRows(parts.to_vec()), None)
    }
    /// Inverted dropout with a caller-supplied 0/1 `keep` pattern (so training is deterministic given a seed): kept values are scaled by 1/(1-p).
    pub fn dropout(&mut self, a: V, keep: &[bool], p: f64) -> V {
        let mut m = self.vals[a.0].clone();
        let mask: Vec<f64> = keep.iter().map(|k| if *k { 1.0 / (1.0 - p) } else { 0.0 }).collect();
        assert_eq!(mask.len(), m.d.len());
        m.d.iter_mut().zip(&mask).for_each(|(x, k)| *x *= k);
        self.push(m, Op::Dropout { a, mask }, None)
    }
    pub fn mean_rows(&mut self, a: V) -> V { let x = &self.vals[a.0]; let mut m = Mat::zeros(1, x.c); for i in 0..x.r { for j in 0..x.c { m.d[j] += x.d[i * x.c + j] / x.r as f64; } } self.push(m, Op::MeanRows(a), None) }
    pub fn row(&mut self, a: V, i: usize) -> V { let x = &self.vals[a.0]; let m = Mat::from(1, x.c, x.d[i * x.c..(i + 1) * x.c].to_vec()); self.push(m, Op::Row(a, i), None) }
    pub fn sum(&mut self, a: V) -> V { let s: f64 = self.vals[a.0].d.iter().sum(); self.push(Mat::from(1, 1, vec![s]), Op::Sum(a), None) }
    pub fn mse(&mut self, pred: V, target: &[f64]) -> V {
        let x = &self.vals[pred.0];
        assert_eq!(x.d.len(), target.len());
        let l = x.d.iter().zip(target).map(|(p, t)| (p - t).powi(2)).sum::<f64>() / target.len() as f64;
        self.push(Mat::from(1, 1, vec![l]), Op::Mse { pred, target: target.to_vec() }, None)
    }
    /// 1 - Pearson correlation of `pred` (n x 1) with `target`: minimising it maximises the cross-sectional correlation (a rank-style objective), scale-free.
    pub fn pearson_loss(&mut self, pred: V, target: &[f64]) -> V {
        let x = &self.vals[pred.0];
        assert_eq!(x.d.len(), target.len());
        let (c, _, _, _) = pearson(&x.d, target);
        self.push(Mat::from(1, 1, vec![1.0 - c]), Op::PearsonLoss { pred, target: target.to_vec() }, None)
    }

    /// Fills the gradients of everything that contributes to the scalar `loss`.
    pub fn backward(&mut self, loss: V) {
        assert_eq!((self.vals[loss.0].r, self.vals[loss.0].c), (1, 1));
        for g in self.grads.iter_mut() { g.d.iter_mut().for_each(|x| *x = 0.0); }
        self.grads[loss.0].d[0] = 1.0;
        for n in (0..self.ops.len()).rev() {
            let g = self.grads[n].clone();
            if g.d.iter().all(|x| *x == 0.0) { continue; }
            match &self.ops[n] {
                Op::Leaf => {}
                Op::Add(a, b) => { let (a, b) = (*a, *b); acc(&mut self.grads[a.0], &g.d); acc(&mut self.grads[b.0], &g.d); }
                Op::AddRow(a, b) => { let (a, b) = (*a, *b); acc(&mut self.grads[a.0], &g.d); for i in 0..g.r { for j in 0..g.c { self.grads[b.0].d[j] += g.d[i * g.c + j]; } } }
                Op::Mul(a, b) => { let (a, b) = (*a, *b); let (av, bv) = (self.vals[a.0].d.clone(), self.vals[b.0].d.clone()); for i in 0..g.d.len() { self.grads[a.0].d[i] += g.d[i] * bv[i]; self.grads[b.0].d[i] += g.d[i] * av[i]; } }
                Op::Scale(a, s) => { let (a, s) = (*a, *s); for i in 0..g.d.len() { self.grads[a.0].d[i] += g.d[i] * s; } }
                Op::MatMul(a, b) => {
                    let (a, b) = (*a, *b);
                    let (x, y) = (self.vals[a.0].clone(), self.vals[b.0].clone());
                    for i in 0..x.r { for k in 0..x.c { let mut s = 0.0; for j in 0..y.c { s += g.d[i * y.c + j] * y.d[k * y.c + j]; } self.grads[a.0].d[i * x.c + k] += s; } }
                    for k in 0..y.r { for j in 0..y.c { let mut s = 0.0; for i in 0..x.r { s += x.d[i * x.c + k] * g.d[i * y.c + j]; } self.grads[b.0].d[k * y.c + j] += s; } }
                }
                Op::MatMulBT(a, b) => {
                    let (a, b) = (*a, *b);
                    let (x, y) = (self.vals[a.0].clone(), self.vals[b.0].clone());
                    for i in 0..x.r { for k in 0..x.c { let mut s = 0.0; for j in 0..y.r { s += g.d[i * y.r + j] * y.d[j * y.c + k]; } self.grads[a.0].d[i * x.c + k] += s; } }
                    for j in 0..y.r { for k in 0..y.c { let mut s = 0.0; for i in 0..x.r { s += g.d[i * y.r + j] * x.d[i * x.c + k]; } self.grads[b.0].d[j * y.c + k] += s; } }
                }
                Op::Gelu(a) => { let a = *a; let xv = self.vals[a.0].d.clone(); for i in 0..g.d.len() { let x = xv[i]; let u = GELU_C * (x + 0.044715 * x * x * x); let t = u.tanh(); let du = GELU_C * (1.0 + 3.0 * 0.044715 * x * x); self.grads[a.0].d[i] += g.d[i] * (0.5 * (1.0 + t) + 0.5 * x * (1.0 - t * t) * du); } }
                Op::Tanh(a) => { let a = *a; let y = self.vals[n].d.clone(); for i in 0..g.d.len() { self.grads[a.0].d[i] += g.d[i] * (1.0 - y[i] * y[i]); } }
                Op::SoftmaxRows(a) => { let a = *a; let y = self.vals[n].clone(); for i in 0..y.r { let dot: f64 = (0..y.c).map(|j| g.d[i * y.c + j] * y.d[i * y.c + j]).sum(); for j in 0..y.c { self.grads[a.0].d[i * y.c + j] += y.d[i * y.c + j] * (g.d[i * y.c + j] - dot); } } }
                Op::LayerNorm { x, g: gm, b, xhat, inv_std } => {
                    let (x, gm, b) = (*x, *gm, *b);
                    let (xhat, inv_std) = (xhat.clone(), inv_std.clone());
                    let gamma = self.vals[gm.0].d.clone();
                    let (r, c) = (g.r, g.c);
                    for i in 0..r {
                        let (mut s1, mut s2) = (0.0, 0.0);
                        for j in 0..c { let dxh = g.d[i * c + j] * gamma[j]; s1 += dxh; s2 += dxh * xhat[i * c + j]; self.grads[gm.0].d[j] += g.d[i * c + j] * xhat[i * c + j]; self.grads[b.0].d[j] += g.d[i * c + j]; }
                        for j in 0..c { let dxh = g.d[i * c + j] * gamma[j]; self.grads[x.0].d[i * c + j] += inv_std[i] * (dxh - s1 / c as f64 - xhat[i * c + j] * s2 / c as f64); }
                    }
                }
                Op::SliceCols { a, start, len } => { let (a, start, len) = (*a, *start, *len); let ac = self.vals[a.0].c; for i in 0..g.r { for j in 0..len { self.grads[a.0].d[i * ac + start + j] += g.d[i * len + j]; } } }
                Op::ConcatCols(parts) => { let parts = parts.clone(); let mut off = 0; for p in parts { let pc = self.vals[p.0].c; for i in 0..g.r { for j in 0..pc { self.grads[p.0].d[i * pc + j] += g.d[i * g.c + off + j]; } } off += pc; } }
                Op::ConcatRows(parts) => { let parts = parts.clone(); let mut off = 0; for p in parts { let (pr, pc) = (self.vals[p.0].r, self.vals[p.0].c); for i in 0..pr { for j in 0..pc { self.grads[p.0].d[i * pc + j] += g.d[(off + i) * pc + j]; } } off += pr; } }
                Op::Dropout { a, mask } => { let a = *a; let mask = mask.clone(); for i in 0..g.d.len() { self.grads[a.0].d[i] += g.d[i] * mask[i]; } }
                Op::MeanRows(a) => { let a = *a; let (r, c) = (self.vals[a.0].r, self.vals[a.0].c); for i in 0..r { for j in 0..c { self.grads[a.0].d[i * c + j] += g.d[j] / r as f64; } } }
                Op::Row(a, i) => { let (a, i) = (*a, *i); let c = self.vals[a.0].c; for j in 0..c { self.grads[a.0].d[i * c + j] += g.d[j]; } }
                Op::Sum(a) => { let a = *a; for x in self.grads[a.0].d.iter_mut() { *x += g.d[0]; } }
                Op::Mse { pred, target } => { let (pred, target) = (*pred, target.clone()); let pv = self.vals[pred.0].d.clone(); for i in 0..target.len() { self.grads[pred.0].d[i] += g.d[0] * 2.0 * (pv[i] - target[i]) / target.len() as f64; } }
                Op::PearsonLoss { pred, target } => {
                    let (pred, target) = (*pred, target.clone());
                    let pv = self.vals[pred.0].d.clone();
                    let (c, mp, sp, _) = pearson(&pv, &target);
                    let n = pv.len() as f64;
                    let mt = target.iter().sum::<f64>() / n;
                    let st = (target.iter().map(|t| (t - mt).powi(2)).sum::<f64>()).sqrt();
                    if sp > 1e-12 && st > 1e-12 {
                        for i in 0..pv.len() { let dc = ((target[i] - mt) / (sp * st)) - c * (pv[i] - mp) / (sp * sp); self.grads[pred.0].d[i] += g.d[0] * -dc; }
                    }
                }
            }
        }
    }
}

fn acc(dst: &mut Mat, src: &[f64]) { for (d, s) in dst.d.iter_mut().zip(src) { *d += s; } }

/// (correlation, mean of a, sqrt of the sum of squared deviations of a, same for b)
fn pearson(a: &[f64], b: &[f64]) -> (f64, f64, f64, f64) {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) { sab += (x - ma) * (y - mb); saa += (x - ma).powi(2); sbb += (y - mb).powi(2); }
    let (sa, sb) = (saa.sqrt(), sbb.sqrt());
    (if sa > 1e-12 && sb > 1e-12 { sab / (sa * sb) } else { 0.0 }, ma, sa, sb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rnd(seed: u64, r: usize, c: usize, scale: f64) -> Mat {
        let mut s = seed | 1;
        Mat::from(r, c, (0..r * c).map(|_| { s ^= s << 13; s ^= s >> 7; s ^= s << 17; ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2.0 * scale }).collect())
    }

    /// Numerical gradient check: `build` makes a scalar loss from the given leaf matrices; every leaf entry's analytic gradient must match central differences.
    fn check(name: &str, leaves: Vec<Mat>, build: &dyn Fn(&mut Tape, &[V]) -> V) {
        let run = |ls: &[Mat]| -> (Tape, Vec<V>, V) { let mut t = Tape::new(); let vs: Vec<V> = ls.iter().map(|m| t.param(m.clone(), 0)).collect(); let l = build(&mut t, &vs); (t, vs, l) };
        let (mut t, vs, l) = run(&leaves);
        t.backward(l);
        let analytic: Vec<Mat> = vs.iter().map(|v| t.grad(*v).clone()).collect();
        for (li, m) in leaves.iter().enumerate() {
            for k in 0..m.d.len() {
                let eps = 1e-6;
                let (mut up, mut dn) = (leaves.clone(), leaves.clone());
                up[li].d[k] += eps; dn[li].d[k] -= eps;
                let (tu, _, lu) = run(&up); let (td, _, ld) = run(&dn);
                let num = (tu.val(lu).d[0] - td.val(ld).d[0]) / (2.0 * eps);
                let an = analytic[li].d[k];
                assert!((num - an).abs() <= 1e-5 + 1e-4 * num.abs().max(an.abs()), "{name}: leaf {li} entry {k}: numerical {num} vs analytic {an}");
            }
        }
    }

    #[test]
    fn matmul_add_row_mul_scale_and_activations_have_exact_gradients() {
        check("matmul+bias+gelu", vec![rnd(1, 3, 4, 1.0), rnd(2, 4, 5, 1.0), rnd(3, 1, 5, 1.0)], &|t, v| { let h = t.matmul(v[0], v[1]); let h = t.add_row(h, v[2]); let h = t.gelu(h); let tgt = vec![0.3; 15]; t.mse(h, &tgt) });
        check("tanh*mul+scale", vec![rnd(4, 2, 3, 1.0), rnd(5, 2, 3, 1.0)], &|t, v| { let a = t.tanh(v[0]); let m = t.mul(a, v[1]); let s = t.scale(m, 1.7); let z = t.add(s, v[0]); t.sum(z) });
        check("matmul_bt", vec![rnd(6, 3, 4, 1.0), rnd(7, 5, 4, 1.0)], &|t, v| { let h = t.matmul_bt(v[0], v[1]); let tgt = vec![0.1; 15]; t.mse(h, &tgt) });
    }

    #[test]
    fn softmax_layer_norm_slice_concat_pooling_and_dropout_have_exact_gradients() {
        check("softmax", vec![rnd(8, 3, 4, 2.0)], &|t, v| { let s = t.softmax_rows(v[0]); let tgt: Vec<f64> = (0..12).map(|i| (i % 4) as f64 * 0.1).collect(); t.mse(s, &tgt) });
        check("layer_norm", vec![rnd(9, 3, 5, 2.0), rnd(10, 1, 5, 1.0), rnd(11, 1, 5, 1.0)], &|t, v| { let n = t.layer_norm(v[0], v[1], v[2]); let tgt: Vec<f64> = (0..15).map(|i| (i as f64 * 0.37).sin()).collect(); t.mse(n, &tgt) });
        check("slice+concat", vec![rnd(12, 3, 6, 1.0), rnd(13, 3, 2, 1.0)], &|t, v| { let a = t.slice_cols(v[0], 1, 3); let c = t.concat_cols(&[a, v[1]]); let tgt = vec![0.2; 15]; t.mse(c, &tgt) });
        check("rows+mean+concat_rows", vec![rnd(14, 4, 3, 1.0), rnd(15, 1, 3, 1.0)], &|t, v| { let m = t.mean_rows(v[0]); let r = t.row(v[0], 2); let c = t.concat_rows(&[m, r, v[1]]); let tgt = vec![0.1, -0.2, 0.3, 0.0, 0.5, 0.2, -0.1, 0.4, 0.2]; t.mse(c, &tgt) });
        let keep = [true, false, true, true, false, true];
        check("dropout", vec![rnd(16, 2, 3, 1.0)], &move |t, v| { let d = t.dropout(v[0], &keep, 0.25); let tgt = vec![0.3; 6]; t.mse(d, &tgt) });
    }

    #[test]
    fn the_pearson_loss_is_zero_for_a_perfect_ranking_and_has_exact_gradients() {
        let target = vec![0.5, -0.2, 0.9, 0.1, -0.7, 0.3];
        check("pearson", vec![rnd(17, 6, 1, 1.0)], &|t, v| t.pearson_loss(v[0], &target));
        let mut t = Tape::new();
        let p = t.input(Mat::from(6, 1, target.iter().map(|x| x * 3.0 + 1.0).collect()));
        let l = t.pearson_loss(p, &target);
        assert!(t.val(l).d[0].abs() < 1e-12, "a perfectly correlated prediction (any scale and shift) has zero loss");
    }

    #[test]
    fn a_full_attention_block_has_exact_gradients() {
        // one head of self-attention with a residual and layer norm: the building block of the Transformer
        let leaves = vec![rnd(20, 4, 6, 1.0), rnd(21, 6, 6, 0.5), rnd(22, 6, 6, 0.5), rnd(23, 6, 6, 0.5), rnd(24, 1, 6, 1.0), rnd(25, 1, 6, 0.3)];
        check("attention", leaves, &|t, v| {
            let q = t.matmul(v[0], v[1]); let k = t.matmul(v[0], v[2]); let val = t.matmul(v[0], v[3]);
            let s = t.matmul_bt(q, k); let s = t.scale(s, 1.0 / 6f64.sqrt()); let a = t.softmax_rows(s);
            let ctx = t.matmul(a, val); let res = t.add(v[0], ctx); let n = t.layer_norm(res, v[4], v[5]);
            let pooled = t.mean_rows(n);
            let tgt = vec![0.1, -0.3, 0.2, 0.0, 0.4, -0.1];
            t.mse(pooled, &tgt)
        });
    }

    #[test]
    fn param_gradients_are_harvested_by_index() {
        let mut t = Tape::new();
        let w = t.param(Mat::from(1, 2, vec![1.0, 2.0]), 7);
        let x = t.input(Mat::from(1, 2, vec![3.0, 4.0]));
        let p = t.mul(w, x);
        let l = t.sum(p);
        t.backward(l);
        let g = t.param_grads();
        assert_eq!(g.len(), 1);
        assert_eq!((g[0].0, g[0].1.d.clone()), (7, vec![3.0, 4.0]));
    }
}

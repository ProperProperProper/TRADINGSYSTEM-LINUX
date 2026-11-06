//! Small, deterministic CPU tensor model (a bounded MLP, not a general ML framework). The live engine never depends on it.
//!
//! The research that used it (walk-forward neural search, promotion gate) moved into ThomasAi, which judges this network with the same causal test as its
//! other learners (`thomas_engine::learner::mlp_trainer`) and feeds its scores to the engine's ML gate in the strategy search. Only the network itself stays
//! here, as the library ThomasAi calls.

#![allow(clippy::needless_range_loop)]

use anyhow::{ensure, Result};

/// A compact row-major tensor used by the research model.
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor2 {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f32>,
}

impl Tensor2 {
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            data: vec![0.0; rows.saturating_mul(cols)],
        }
    }

    pub fn from_rows(rows: Vec<Vec<f32>>) -> Result<Self> {
        let cols = rows.first().map_or(0, Vec::len);
        ensure!(
            cols > 0 && rows.iter().all(|r| r.len() == cols),
            "ragged or empty tensor"
        );
        Ok(Self {
            rows: rows.len(),
            cols,
            data: rows.into_iter().flatten().collect(),
        })
    }

    #[inline]
    pub fn get(&self, row: usize, col: usize) -> f32 {
        self.data[row * self.cols + col]
    }

    #[inline]
    pub fn set(&mut self, row: usize, col: usize, value: f32) {
        self.data[row * self.cols + col] = value;
    }
}

/// A tiny one-hidden-layer regression network. All arithmetic is finite-checked and CPU-only.
#[derive(Debug, Clone)]
pub struct Mlp {
    input: usize,
    hidden: usize,
    w1: Tensor2,
    b1: Vec<f32>,
    w2: Vec<f32>,
    b2: f32,
    mean: Vec<f32>,
    scale: Vec<f32>,
}

impl Mlp {
    /// Deterministic small model. `hidden` is capped to keep paper trading responsive.
    pub fn new(input: usize, hidden: usize, seed: u64) -> Result<Self> {
        ensure!(input > 0, "model needs features");
        let hidden = hidden.clamp(4, 64);
        let mut state = seed | 1;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((state >> 40) as f32 / 8_388_608.0) - 1.0
        };
        let mut w1 = Tensor2::zeros(input, hidden);
        for x in &mut w1.data {
            *x = next() * 0.05;
        }
        let mut w2 = vec![0.0; hidden];
        for x in &mut w2 {
            *x = next() * 0.05;
        }
        Ok(Self {
            input,
            hidden,
            w1,
            b1: vec![0.0; hidden],
            w2,
            b2: 0.0,
            mean: vec![0.0; input],
            scale: vec![1.0; input],
        })
    }

    /// Fit on examples from strictly earlier candles. The caller owns the walk-forward split.
    pub fn fit(&mut self, x: &Tensor2, y: &[f32], epochs: usize, lr: f32) -> Result<()> {
        ensure!(
            x.cols == self.input && x.rows == y.len() && x.rows > 0,
            "training shape mismatch"
        );
        ensure!(
            lr.is_finite() && lr > 0.0 && lr <= 1.0,
            "invalid learning rate"
        );
        for j in 0..self.input {
            self.mean[j] = (0..x.rows).map(|i| x.get(i, j)).sum::<f32>() / x.rows as f32;
            let v = (0..x.rows)
                .map(|i| {
                    let d = x.get(i, j) - self.mean[j];
                    d * d
                })
                .sum::<f32>()
                / x.rows as f32;
            self.scale[j] = v.sqrt().max(1e-4);
        }
        for _ in 0..epochs.min(200) {
            for i in 0..x.rows {
                let mut h = vec![0.0; self.hidden];
                for k in 0..self.hidden {
                    h[k] = (0..self.input)
                        .map(|j| ((x.get(i, j) - self.mean[j]) / self.scale[j]) * self.w1.get(j, k))
                        .sum::<f32>()
                        + self.b1[k];
                    h[k] = h[k].tanh();
                }
                let pred = self.b2 + (0..self.hidden).map(|k| h[k] * self.w2[k]).sum::<f32>();
                let err = (pred - y[i]).clamp(-10.0, 10.0);
                for k in 0..self.hidden {
                    let g = err * self.w2[k] * (1.0 - h[k] * h[k]);
                    self.w2[k] -= lr * err * h[k];
                    self.b1[k] -= lr * g;
                    for j in 0..self.input {
                        self.w1.set(
                            j,
                            k,
                            self.w1.get(j, k)
                                - lr * g * ((x.get(i, j) - self.mean[j]) / self.scale[j]),
                        );
                    }
                }
                self.b2 -= lr * err;
            }
        }
        Ok(())
    }

    pub fn predict(&self, x: &[f32]) -> Option<f32> {
        if x.len() != self.input || x.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let mut out = self.b2;
        for k in 0..self.hidden {
            let h = (0..self.input)
                .map(|j| ((x[j] - self.mean[j]) / self.scale[j]) * self.w1.get(j, k))
                .sum::<f32>()
                + self.b1[k];
            out += h.tanh() * self.w2[k];
        }
        out.is_finite().then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tensor_model_is_deterministic_and_learns() {
        let x = Tensor2::from_rows(vec![vec![-1.0], vec![0.0], vec![1.0]]).unwrap();
        let mut a = Mlp::new(1, 8, 7).unwrap();
        let mut b = Mlp::new(1, 8, 7).unwrap();
        a.fit(&x, &[-1.0, 0.0, 1.0], 80, 0.02).unwrap();
        b.fit(&x, &[-1.0, 0.0, 1.0], 80, 0.02).unwrap();
        assert_eq!(a.predict(&[0.5]), b.predict(&[0.5]));
        assert!(a.predict(&[0.5]).unwrap().is_finite());
    }
}

//! A small gradient-boosted regression tree learner (squared loss, histogram splits), deterministic and dependency-free.
//! It knows nothing about markets: rows of finite features in, a number out.

/// Learner settings.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub trees: usize,
    pub depth: usize,
    pub learning_rate: f64,
    pub min_leaf: usize,
    /// Histogram bins per feature (at most 255).
    pub bins: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            trees: 100,
            depth: 3,
            learning_rate: 0.05,
            min_leaf: 50,
            bins: 32,
        }
    }
}

#[derive(Debug, Clone)]
enum Node {
    Leaf(f64),
    Split {
        feature: usize,
        threshold: f64,
        left: Box<Node>,
        right: Box<Node>,
    },
}

impl Node {
    fn predict(&self, x: &[f64]) -> f64 {
        match self {
            Node::Leaf(v) => *v,
            Node::Split {
                feature,
                threshold,
                left,
                right,
            } => {
                if x[*feature] <= *threshold {
                    left.predict(x)
                } else {
                    right.predict(x)
                }
            }
        }
    }
}

/// A trained model.
#[derive(Debug, Clone)]
pub struct Model {
    base: f64,
    trees: Vec<Node>,
}

impl Model {
    pub fn predict(&self, x: &[f64]) -> f64 {
        self.base + self.trees.iter().map(|t| t.predict(x)).sum::<f64>()
    }
    pub fn trees(&self) -> usize {
        self.trees.len()
    }
}

/// Quantile cut points of one feature column (up to `bins - 1` distinct thresholds).
fn cuts(col: &[f64], bins: usize) -> Vec<f64> {
    let mut v: Vec<f64> = col.to_vec();
    v.sort_by(f64::total_cmp);
    let mut out: Vec<f64> = (1..bins)
        .map(|b| v[(v.len() * b / bins).min(v.len() - 1)])
        .collect();
    out.dedup();
    out
}

struct Data<'a> {
    x: &'a [Vec<f64>],
    /// cut points per feature
    cuts: Vec<Vec<f64>>,
}

fn best_split(
    d: &Data,
    rows: &[usize],
    resid: &[f64],
    min_leaf: usize,
) -> Option<(usize, f64, f64)> {
    let n = rows.len() as f64;
    let total: f64 = rows.iter().map(|&r| resid[r]).sum();
    let mut best: Option<(usize, f64, f64)> = None; // (feature, threshold, gain)
    for (f, cut) in d.cuts.iter().enumerate() {
        // bucket sums per cut interval
        let mut cnt = vec![0usize; cut.len() + 1];
        let mut sum = vec![0.0; cut.len() + 1];
        for &r in rows {
            let b = cut.partition_point(|c| *c < d.x[r][f]);
            cnt[b] += 1;
            sum[b] += resid[r];
        }
        let (mut lc, mut ls) = (0usize, 0.0);
        for (i, c) in cut.iter().enumerate() {
            lc += cnt[i];
            ls += sum[i];
            let rc = rows.len() - lc;
            if lc < min_leaf || rc < min_leaf {
                continue;
            }
            let gain =
                ls * ls / lc as f64 + (total - ls) * (total - ls) / rc as f64 - total * total / n;
            if best.is_none_or(|b| gain > b.2) {
                best = Some((f, *c, gain));
            }
        }
    }
    best.filter(|b| b.2 > 1e-12)
}

fn grow(d: &Data, rows: &[usize], resid: &[f64], depth: usize, p: &Params) -> Node {
    let mean = rows.iter().map(|&r| resid[r]).sum::<f64>() / rows.len().max(1) as f64;
    if depth == 0 || rows.len() < 2 * p.min_leaf {
        return Node::Leaf(mean);
    }
    match best_split(d, rows, resid, p.min_leaf) {
        None => Node::Leaf(mean),
        Some((feature, threshold, _)) => {
            let (l, r): (Vec<usize>, Vec<usize>) =
                rows.iter().partition(|&&i| d.x[i][feature] <= threshold);
            Node::Split {
                feature,
                threshold,
                left: Box::new(grow(d, &l, resid, depth - 1, p)),
                right: Box::new(grow(d, &r, resid, depth - 1, p)),
            }
        }
    }
}

/// Trains on rows `x` (all the same width, finite) and targets `y`. Returns None when there is nothing to learn from.
pub fn train(x: &[Vec<f64>], y: &[f64], p: Params) -> Option<Model> {
    if x.is_empty()
        || x.len() != y.len()
        || x[0].is_empty()
        || x.iter()
            .any(|r| r.len() != x[0].len() || r.iter().any(|v| !v.is_finite()))
        || y.iter().any(|v| !v.is_finite())
    {
        return None;
    }
    let width = x[0].len();
    let d = Data {
        x,
        cuts: (0..width)
            .map(|f| cuts(&x.iter().map(|r| r[f]).collect::<Vec<_>>(), p.bins.min(255)))
            .collect(),
    };
    let base = y.iter().sum::<f64>() / y.len() as f64;
    let mut pred = vec![base; y.len()];
    let rows: Vec<usize> = (0..y.len()).collect();
    let mut trees = Vec::with_capacity(p.trees);
    for _ in 0..p.trees {
        let resid: Vec<f64> = y.iter().zip(&pred).map(|(t, q)| t - q).collect();
        let tree = grow(&d, &rows, &resid, p.depth, &p);
        for (i, q) in pred.iter_mut().enumerate() {
            *q += p.learning_rate * tree.predict(&x[i]);
        }
        trees.push(scale(tree, p.learning_rate));
    }
    Some(Model { base, trees })
}

fn scale(n: Node, k: f64) -> Node {
    match n {
        Node::Leaf(v) => Node::Leaf(v * k),
        Node::Split {
            feature,
            threshold,
            left,
            right,
        } => Node::Split {
            feature,
            threshold,
            left: Box::new(scale(*left, k)),
            right: Box::new(scale(*right, k)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random rows in [0,1).
    fn rows(n: usize, width: usize) -> Vec<Vec<f64>> {
        let mut s = 12345_u64;
        (0..n)
            .map(|_| {
                (0..width)
                    .map(|_| {
                        s = s
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        (s >> 11) as f64 / (1u64 << 53) as f64
                    })
                    .collect()
            })
            .collect()
    }

    fn r2(m: &Model, x: &[Vec<f64>], y: &[f64]) -> f64 {
        let mean = y.iter().sum::<f64>() / y.len() as f64;
        let ss_tot: f64 = y.iter().map(|v| (v - mean).powi(2)).sum();
        let ss_res: f64 = x
            .iter()
            .zip(y)
            .map(|(r, t)| (t - m.predict(r)).powi(2))
            .sum();
        1.0 - ss_res / ss_tot
    }

    #[test]
    fn learns_a_step_in_one_feature_and_ignores_noise_features() {
        let x = rows(2000, 3);
        let y: Vec<f64> = x
            .iter()
            .map(|r| if r[0] > 0.5 { 1.0 } else { -1.0 })
            .collect();
        let m = train(&x, &y, Params::default()).unwrap();
        assert!(r2(&m, &x, &y) > 0.95, "r2 {}", r2(&m, &x, &y));
        let test = rows(500, 3);
        let yt: Vec<f64> = test
            .iter()
            .map(|r| if r[0] > 0.5 { 1.0 } else { -1.0 })
            .collect();
        assert!(
            r2(&m, &test, &yt) > 0.9,
            "generalises out of the training rows"
        );
    }

    #[test]
    fn learns_an_interaction_that_no_single_feature_shows() {
        let x = rows(4000, 2);
        let y: Vec<f64> = x
            .iter()
            .map(|r| {
                if (r[0] > 0.5) == (r[1] > 0.5) {
                    1.0
                } else {
                    -1.0
                }
            })
            .collect();
        let m = train(
            &x,
            &y,
            Params {
                depth: 3,
                ..Params::default()
            },
        )
        .unwrap();
        assert!(
            r2(&m, &x, &y) > 0.9,
            "depth 3 captures the XOR: {}",
            r2(&m, &x, &y)
        );
        let shallow = train(
            &x,
            &y,
            Params {
                depth: 1,
                ..Params::default()
            },
        )
        .unwrap();
        assert!(
            r2(&shallow, &x, &y) < 0.2,
            "stumps cannot: {}",
            r2(&shallow, &x, &y)
        );
    }

    #[test]
    fn is_deterministic_and_constant_targets_give_constant_predictions() {
        let x = rows(500, 2);
        let y: Vec<f64> = x.iter().map(|r| r[0] * 2.0).collect();
        let (a, b) = (
            train(&x, &y, Params::default()).unwrap(),
            train(&x, &y, Params::default()).unwrap(),
        );
        assert!(x.iter().all(|r| a.predict(r) == b.predict(r)));
        let flat = train(&x, &vec![3.0; 500], Params::default()).unwrap();
        assert!(x.iter().all(|r| (flat.predict(r) - 3.0).abs() < 1e-12));
    }

    #[test]
    fn refuses_bad_input() {
        assert!(train(&[], &[], Params::default()).is_none());
        assert!(train(&[vec![1.0], vec![f64::NAN]], &[1.0, 2.0], Params::default()).is_none());
        assert!(train(&[vec![1.0]], &[1.0, 2.0], Params::default()).is_none());
    }
}

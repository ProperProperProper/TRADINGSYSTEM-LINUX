# `lab/src/stats.rs`

> Statistics the rule engine relies on. Pure functions, no I/O, deterministic given a seed.
> The language model never computes any of these numbers.

15 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `mean`

`lab/src/stats.rs:10`

```rust
pub fn mean(x: &[f64]) -> f64
```

Mean (NaN when empty).

## `variance`

`lab/src/stats.rs:15`

```rust
pub fn variance(x: &[f64]) -> f64
```

Sample variance (NaN with fewer than 2 values).

## `std_dev`

`lab/src/stats.rs:24`

```rust
pub fn std_dev(x: &[f64]) -> f64
```

Sample standard deviation.

## `normal_cdf`

`lab/src/stats.rs:29`

```rust
pub fn normal_cdf(x: f64) -> f64
```

Standard normal CDF (Abramowitz-Stegun erf approximation, abs error < 1.5e-7).

## `normal_inv`

`lab/src/stats.rs:37`

```rust
pub fn normal_inv(p: f64) -> f64
```

Inverse standard normal CDF (Acklam's algorithm, relative error ~1e-9).

## `kurtosis`

`lab/src/stats.rs:58`

```rust
pub fn kurtosis(x: &[f64]) -> f64
```

Non-excess kurtosis (3 for a normal distribution).

## `group_diff`

`lab/src/stats.rs:74`

```rust
pub fn group_diff(obs: &[Obs]) -> f64
```

Mean of group 1 minus mean of group 0 (NaN when a group is empty).

## `effective_n`

`lab/src/stats.rs:83`

```rust
pub fn effective_n(obs: &[Obs]) -> usize
```

Number of independent clusters that contain at least one observation (the effective sample size).

## `cluster_bootstrap_ci`

`lab/src/stats.rs:91`

```rust
pub fn cluster_bootstrap_ci(obs: &[Obs], n_boot: usize, seed: u64) -> (f64, f64)
```

Block (cluster) bootstrap of the group difference: whole days are resampled, so correlation within a day (many coins on the same market move) is preserved. Returns the 95% percentile interval.

## `placebo_p`

`lab/src/stats.rs:117`

```rust
pub fn placebo_p(obs: &[Obs], n_perm: usize, seed: u64) -> f64
```

Placebo test: shuffle group labels *within* each cluster (this keeps the day structure and the group sizes per day) and ask how often noise produces a difference as large as the observed one. Two-sided.

## `bh_qvalues`

`lab/src/stats.rs:146`

```rust
pub fn bh_qvalues(p: &[f64]) -> Vec<f64>
```

Benjamini-Hochberg adjusted q-values (false-discovery-rate control), in the original order.

## `ranks`

`lab/src/stats.rs:161`

```rust
fn ranks(x: &[f64]) -> Vec<f64>
```

Ranks of `x` starting at 1, ties averaged.

## `spearman`

`lab/src/stats.rs:177`

```rust
pub fn spearman(x: &[f64], y: &[f64]) -> f64
```

Spearman rank correlation (the information coefficient used throughout quantitative finance).

## `quantile`

`lab/src/stats.rs:191`

```rust
pub fn quantile(x: &[f64], q: f64) -> f64
```

Quantile with linear interpolation; `q` in [0, 1]. Input need not be sorted.

## `global_placebo_p`

`lab/src/stats.rs:203`

```rust
pub fn global_placebo_p(obs: &[Obs], n_perm: usize, seed: u64) -> f64
```

Placebo for observations that are each their own cluster (for example one run = one observation): shuffle the group labels globally, keeping the group sizes.

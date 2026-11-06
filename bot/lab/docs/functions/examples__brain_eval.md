# `examples/brain_eval.rs`

> Reproducible comparison of proposal models on the lab's REAL history (no synthetic data), walk-forward in time order:
> `cargo run --release --example brain_eval -- <copy of memory.sqlite3>`. Fits on the training half only (h1, nh1, worst block); the lab's holdout
> verdict is used for EVALUATION only (does the top quarter pass it more often than chance?). Results and conclusions: docs/BRAIN.md.

5 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `ridge_predict`

`examples/brain_eval.rs:7`

```rust
fn ridge_predict(train: &[Obs], x: &BTreeSet<String>, lambda: f64, fam_w: f64) -> f64
```

_No comment in the code; the signature above is the description._

## `ridge_pooled`

`examples/brain_eval.rs:29`

```rust
fn ridge_pooled(train: &[Obs], x: &BTreeSet<String>, lambda: f64) -> f64
```

_No comment in the code; the signature above is the description._

## `eval`

`examples/brain_eval.rs:50`

```rust
fn eval(obs: &[Obs], start: usize, step: usize, f: &dyn Fn(&[Obs], &BTreeSet<String>) -> f64) -> (f64, f64, usize)
```

_No comment in the code; the signature above is the description._

## `pass_rate`

`examples/brain_eval.rs:60`

```rust
fn pass_rate(obs: &[Obs], pass: &[bool], start: usize, step: usize, f: &dyn Fn(&[Obs], &BTreeSet<String>) -> f64) -> (f64, f64, usize)
```

share of the top quarter (by prediction) that passes the lab's holdout verdict on cut A, versus the share among all later variants

## `main`

`examples/brain_eval.rs:73`

```rust
fn main() -> anyhow::Result<()>
```

_No comment in the code; the signature above is the description._

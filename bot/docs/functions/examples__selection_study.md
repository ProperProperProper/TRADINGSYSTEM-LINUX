# `examples/selection_study.rs`

> Does ANY way of picking settings from the backtest carry forward? For every 14-session look-forward block
> (search window = the 14 sessions before, forward = the next 14, as in lab_export), draw N random settings,
> backtest each in-sample and forward, then score selection RULES by the forward return of the setting they pick.
> Also reports the rank correlation between in-sample and forward return across the draws, and the bot's own
> search winner on the same blocks. Stored candles only; no orders.
>
> cargo run --release --example selection_study -- <interval key> [draws=400] [offsets=0,5,10]

4 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `rank`

`examples/selection_study.rs:32`

```rust
fn rank(v: &[f64]) -> Vec<f64>
```

Ranks of `v` (0 = smallest), for rank correlation.

## `spearman`

`examples/selection_study.rs:43`

```rust
fn spearman(a: &[f64], b: &[f64]) -> f64
```

Spearman rank correlation between `a` and `b` (0 when either has no spread).

## `pick`

`examples/selection_study.rs:57`

```rust
fn pick<F: Fn(&M) -> Option<f64>>(ms: &[M], score: F) -> Option<f64>
```

The forward return of the candidate with the highest `score` (None when no candidate has a score).

## `main`

`examples/selection_study.rs:63`

```rust
async fn main() -> Result<()>
```

Selection study: tests whether any in-sample selection rule predicts the next 14 sessions' return, on look-forward blocks.

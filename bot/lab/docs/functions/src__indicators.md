# `src/indicators.rs`

> Technical indicators and candle-pattern features, computed in-house from the lab's own 15-minute bars (owner 2026-10-13: "analyse the candles itself, indicators
> itself"). Every value at bar `i` uses bars up to and including `i` only (a test rewrites the future and checks nothing earlier moves); every value is scale-free
> (ratios, fractions, bounded oscillators), so one network can read all stocks. `compute` returns one row of `N` values per bar; warm-up values are 0.

2 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `ema`

`src/indicators.rs:15`

```rust
fn ema(prev: f64, x: f64, n: usize) -> f64
```

_No comment in the code; the signature above is the description._

## `compute`

`src/indicators.rs:17`

```rust
pub fn compute(bars: &[Bar]) -> Vec<[f64; N]>
```

_No comment in the code; the signature above is the description._

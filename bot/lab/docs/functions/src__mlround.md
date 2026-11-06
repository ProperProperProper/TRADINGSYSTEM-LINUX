# `src/mlround.rs`

> The daily improvement round (owner 2026-10-13: "learn and improve"): recency schemes against the fading edge, candidate features each added alone, and
> meta-labelling on the bot's own trades with a tail label -- all against the trial ledger, with PAIRED, multiple-testing-adjusted intervals.
> Nothing is adopted unless the paired difference is above zero at the adjusted level. Called by `qwen-lab candles improve` and by the statistician daily.

5 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `universe_only`

`src/mlround.rs:17`

```rust
fn universe_only(s: &[Sample], nb: usize) -> Vec<Sample>
```

_No comment in the code; the signature above is the description._

## `rolled`

`src/mlround.rs:26`

```rust
fn rolled(all: &[Sample], cols: &[usize], fit: &dyn Fn(&[Sample], i64) -> Scorer) -> (Vec<Sample>, Vec<f64>)
```

_No comment in the code; the signature above is the description._

## `z_by_moment`

`src/mlround.rs:41`

```rust
fn z_by_moment(s: &[Sample], p: &[f64]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `run`

`src/mlround.rs:49`

```rust
pub fn run(candle_db: &std::path::Path, data_db: &std::path::Path) -> anyhow::Result<Round>
```

_No comment in the code; the signature above is the description._

## `explore_next`

`src/mlround.rs:164`

```rust
pub fn explore_next(candle_db: &std::path::Path, data_db: &std::path::Path) -> anyhow::Result<String>
```

The standing experiments that keep the model learning when no new data arrives (24x7): training windows and recency half-lives beyond the five schemes of the daily round. Each is a ledger trial (paired against the expanding base at the adjusted level); the least recently run is repeated when everything has run, so the same question is re-asked as the data grows. Returns a report line.

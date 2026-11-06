# `lab/examples/candle_models.rs`

> Try different models and label horizons on the bot's top-20 universe, learn which work, report everything.
> Clean data (SPY holdings, splits dropped), bot score features, every prediction out of sample in rolling 28-day windows.
> `cargo run --release --example candle_models`.

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `universe_only`

`lab/examples/candle_models.rs:10`

```rust
fn universe_only(s: &[Sample], nb: usize) -> Vec<Sample>
```

Re-centres the label inside the top-20 universe (the bot only chooses among those), and keeps those samples.

## `rolled`

`lab/examples/candle_models.rs:19`

```rust
fn rolled(all: &[Sample], fit: &dyn Fn(&[Sample]) -> Scorer) -> (Vec<Sample>, Vec<f64>)
```

_No comment in the code; the signature above is the description._

## `main`

`lab/examples/candle_models.rs:34`

```rust
fn main() -> anyhow::Result<()>
```

_No comment in the code; the signature above is the description._

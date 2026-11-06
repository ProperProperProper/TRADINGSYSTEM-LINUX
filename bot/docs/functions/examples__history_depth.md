# `examples/history_depth.rs`

> How far back moomoo's 15m candle history reaches for one symbol already in this week's quota (a re-pull costs no quota).
> Read-only: pulls candles, stores nothing, places no orders.
>
> cargo run --release --example history_depth -- SPY 2023-01-03

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/history_depth.rs:11`

```rust
async fn main() -> Result<()>
```

Pulls `symbol`'s 15m candles from the given date and prints the first and last candle and the count.

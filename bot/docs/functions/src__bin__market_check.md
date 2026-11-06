# `src/bin/market_check.rs`

> Which markets OpenD serves quotes for: stock lookup (static info) and a quote snapshot for an
> ASX stock (BHP) and a US stock (AAPL). Read-only; uses no historical-candle quota.

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`src/bin/market_check.rs:9`

```rust
async fn main() -> Result<()>
```

Probe: asks OpenD for static info and a quote of one US and one ASX stock to show which markets this login can see.

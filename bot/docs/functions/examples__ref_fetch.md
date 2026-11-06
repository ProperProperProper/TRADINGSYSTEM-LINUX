# `examples/ref_fetch.rs`

> Add one symbol's full 15m history (from the clock epoch) to target/oos-cache/history.json, e.g. a new trend
> reference. Costs ONE candle-quota unit if the symbol is not already pulled in the current 7-day window; the
> bot's store is not touched.
>
> cargo run --release --example ref_fetch -- SPY

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/ref_fetch.rs:12`

```rust
async fn main() -> Result<()>
```

Pulls one symbol's full candle history into the research cache (for a reference or a missing symbol), respecting the quota.

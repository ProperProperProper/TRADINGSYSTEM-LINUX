# `examples/history_fetch.rs`

> Research data: the longest 15m history moomoo gives (from the clock epoch) for symbols ALREADY
> pulled inside the current 7-day candle-quota window, so it costs no quota (re-pulling a symbol
> in the window is free). Any symbol not in moomoo's pulled list is skipped, and the run stops if
> the used quota moves. Written to target/oos-cache/history.json for examples/oos_study.rs; the
> bot's store is not touched. Paced well below moomoo's request limit (the live bot shares it).
>
> cargo run --release --example history_fetch

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/history_fetch.rs:16`

```rust
async fn main() -> Result<()>
```

Pulls the full 15m candle history (from the clock epoch) for every symbol already in the store, within the candle quota, into the research cache.

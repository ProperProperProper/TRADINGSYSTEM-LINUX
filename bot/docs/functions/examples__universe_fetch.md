# `examples/universe_fetch.rs`

> Fill target/oos-cache/history.json with the 15m history (from the research start, `common::RESEARCH_FROM`) of SPY's holdings that
> the research cache lacks or holds only from a later start, in weight order, within the candle quota: symbols already pulled this week are free, a new one costs one unit and is only
> taken while more than RESERVE units remain (the live bot keeps its own headroom). Delisting or otherwise ineligible securities
> are skipped BEFORE any quota is spent. Run it daily: quota frees up as old pulls expire.
>
> cargo run --release --example universe_fetch

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/universe_fetch.rs:20`

```rust
async fn main() -> Result<()>
```

Fills the research cache with the 15m history of SPY's holdings it lacks, heaviest first, paced and within the quota reserve.

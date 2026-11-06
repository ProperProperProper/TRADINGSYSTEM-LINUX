# `examples/variant_study.rs`

> Which search-space VARIANT wins out of sample? For every 14-session look-forward block the bot's own search
> (with the variant's ranges: hold, basket size, trend band, signal direction, fee gate, keep-on-rebalance) picks
> settings on the 14 sessions before; those settings, fixed, trade the next 14. Settings still come from the search;
> only the ranges differ. Stored candles only.
>
> STUDY_EQUITY=5000 cargo run --release --example variant_study -- <interval key> <variant,variant,...> [offsets=0,5,10]

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `variant_universe`

`examples/variant_study.rs:29`

```rust
fn variant_universe(name: &str) -> usize
```

How many of the most liquid names (trailing dollar volume, causal) may enter, from a `univ<N>` token (live: 20).

## `variant`

`examples/variant_study.rs:34`

```rust
fn variant(name: &str, interval: Interval) -> Result<SearchSpace>
```

The search space of variant `name` (a '+'-joined list of tokens) on `interval`, and the research switches it turns on.

## `main`

`examples/variant_study.rs:100`

```rust
async fn main() -> Result<()>
```

Variant study: scores each variant on 14-session look-forward blocks (both halves, noisy tape, compounded `chained` return).

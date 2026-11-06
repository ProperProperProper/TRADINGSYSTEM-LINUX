# `src/mlbot.rs`

> The candle model, inspired by the bot's own backtester (owner 2026-10-13: "the backtest works really well, so it should inspire the model").
> What the bot's engine does, and what the model now shares with it:
> * it only ever ranks the TOP 20 symbols by trailing one-session dollar volume (its screener universe) -> the model is trained and judged on those only;
> * its screener scores (`engine/scores.rs`) are the features its backtests rank by -> ported here exactly: multi-window volatility (1,2,4,16,26 bars,
> weights 5:4:3:2:1, each scaled by 1/sqrt(window)), price action (distance from EMA 8/21/55, weights 3:2:1, divided by the volatility), volume z-score
> against the previous 20 bars, RSI14 (Wilder), the one-session return, and their percentiles inside the universe, rank value, trend score;
> * its entries are gated by SPY's trend (close against its moving average) -> SPY trend distances are features.
>
> The turnover of a bar is approximated by close x volume (the lab's store has no turnover column).

5 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `raw_features`

`src/mlbot.rs:26`

```rust
fn raw_features(bars: &[Bar]) -> Vec<Option<Raw>>
```

The bot's per-symbol raw features at every bar index (None during warm-up). Only bars up to the index are used.

## `percentile`

`src/mlbot.rs:65`

```rust
fn percentile(sorted: &[f64], x: f64) -> f64
```

Percentile (0-100) of `x` within the sorted `all`, as the bot computes it.

## `extend`

`src/mlbot.rs:71`

```rust
pub fn extend(series: &BTreeMap<String, Vec<Bar>>, base: &[Sample]) -> Vec<Sample>
```

The base samples plus the bot-inspired columns. `series` must be the map the samples were built from (same symbol order).

## `interactions`

`src/mlbot.rs:123`

```rust
pub fn interactions(s: &[Sample]) -> Vec<Sample>
```

Market-regime interactions (the best model of `examples/candle_models.rs`, 2026-10-13): the stock's own signals scaled by SPY's trend. Samples without the bot columns (tests) pass through unchanged.

## `in_universe`

`src/mlbot.rs:138`

```rust
pub fn in_universe(s: &[Sample], base_features: usize) -> Vec<Sample>
```

Only the samples whose symbol is in the bot's top-20 universe at that bar (the only ones the bot's engine ranks).

# `lab/src/mlmeta.rs`

> Meta-labelling (owner 2026-10-13, item 2): train on the bot's OWN backtest trades. The bot's strategy decides to enter; the model learns, from the candle
> context at the decision bar, which of those entries do badly, so it can veto them. Two labels are tried: the return itself, and a TAIL label (the trade
> loses more than `tail`), because a veto exists to avoid the bad outcomes, not to rank the good ones.
> The candle context is the lab's features at the decision bar (the latest bar OPEN at least 15 minutes before the trade's recorded decision time, so it is
> strictly past even if the recorded time is a bar close).

2 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `trades_with_context`

`lab/src/mlmeta.rs:13`

```rust
pub fn trades_with_context(data: &Connection, lookup: &dyn Fn(&str, i64) -> Option<Vec<f64>>, with_trade_features: bool) -> rusqlite::Result<Vec<Trade>>
```

The bot's long backtest trades of the unselected runs (random/oos) with the candle features at their decision bar. `lookup(symbol, ts_ms)` returns the feature vector of a bar. Rows without candle context are dropped. `block` = the run window start (trades of one block share the market).

## `with_tail_label`

`lab/src/mlmeta.rs:38`

```rust
pub fn with_tail_label(t: &[Trade], tail: f64) -> Vec<Trade>
```

Tail label: `win` = the trade did NOT lose more than `tail` (so a classifier's score is the probability of avoiding the tail).

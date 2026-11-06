# `src/engine/btarchive.rs`

> Backtest archive for the research lab (Qwen Lab): every backtest run the service makes, with its
> individual trades, in `backtest_archive.sqlite3` beside the main store. The lab reads it strictly
> read-only and learns from it; the bot never reads it back.
>
> Tables: `bt_runs` (one row per run), `bt_trades` (the closed trades of a run), `bt_daily`
> (equity per day). Selection rule the lab relies on: runs of source `oos` (walk-forward days on
> data the settings never saw) and `random` (settings drawn at random) keep their trades whether or
> not the run did well, so they are an UNSELECTED sample; runs of source `chosen` (the search's
> winners, in-sample) keep trades only when the run passed the success filter. Timestamps are
> milliseconds. Stock bot: long only, cash only, so leverage is 1 and funding is 0.

8 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `open`

`src/engine/btarchive.rs:83`  (method / nested)

```rust
pub fn open(path: &Path, engine_commit: &str) -> Result<BtArchive>
```

Opens (or creates) the backtest archive database in WAL mode, labelled with the engine commit and cost model.

## `with_cost_model`

`src/engine/btarchive.rs:96`  (method / nested)

```rust
pub fn with_cost_model(mut self, label: &str) -> BtArchive
```

Label this archive's runs with another cost-model string, so the lab never mixes regimes (history exports built on today's stock pool use their own).

## `record`

`src/engine/btarchive.rs:103`  (method / nested)

```rust
pub fn record(&self, service_run: i64, now_ms: i64, run: &BtRun) -> Result<bool>
```

Store one run with its trades and daily rows in one transaction. A run already stored (same settings, source, window and start) is left alone. Returns whether it was new.

## `entry_features`

`src/engine/btarchive.rs:154`

```rust
pub fn entry_features(m: &Market, closes: &[i64], sc: &[Vec<Option<Score>>], p: &XsParams, t: &Trade, cache: &mut std::collections::HashMap<usize, serde_json::Value>) -> serde_json::Value
```

The market and stock state at the bar whose close DECIDED this entry (closed bars only), as the research lab's features: the stock's own signal, returns, distance from its 24h average, ATR and volume; the reference ETF's trend test; market-wide dispersion and breadth. Uses the decision bar the engine recorded on the trade (a wick-guard delay does not move it); a trade without one falls back to its entry bar minus two.

## `clock_wall`

`src/engine/btarchive.rs:175`

```rust
pub fn clock_wall(m: &Market, closes: &[i64], c: i64) -> Option<i64>
```

A trading-clock time (`Market::ts`) as real UTC ms: the open of the bar it names, or the close of the last bar when it is the end-of-test marker one bar past the data.

## `wall_trade`

`src/engine/btarchive.rs:184`

```rust
pub fn wall_trade(m: &Market, closes: &[i64], t: &Trade) -> Option<Trade>
```

The trade with its clock times (entry, exit, deciding bar) converted to real UTC ms, for the research archive; None when a time cannot be mapped (such a trade is left out, never guessed).

## `day_tokens`

`src/engine/btarchive.rs:196`

```rust
pub fn day_tokens(trades: &[(Trade, serde_json::Value)]) -> Vec<(String, f64)>
```

Symbols traded with their summed pnl, best first.

## `params_value`

`src/engine/btarchive.rs:207`

```rust
pub fn params_value<P: Serialize>(p: &P) -> serde_json::Value
```

A run's parameters as JSON for the archive.

# `src/mlscores.rs`

> The candle model as a signal for the BOT'S OWN backtester (owner 2026-10-13: "does it work like the bot back tester??? it should").
> Writes, for every 15-minute bar, the model's score for the next session's relative return, in the file the bot's research engine reads (`Signal::Ml`,
> `variant_study ... sig:Ml`). The bot's engine then does everything else itself: top-20 universe, trend filter, entries, exits, sizing, its measured fees
> and spreads, 14-session walk-forward blocks and the per-block parameter search.
>
> The features are the lab's 23 plus the bot's own score components (`mlbot.rs`); training and scoring cover the bot's top-20 liquid universe only.
> Causality: the model is refitted at the start of every 14-session block on samples that ended at least `PURGE` sessions earlier (their one-session labels
> are then complete), with the ridge strength chosen on the last quarter of that training period. Every score in a block comes from that one model.

8 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `session_of`

`src/mlscores.rs:19`

```rust
fn session_of(ts: i64) -> i64
```

_No comment in the code; the signature above is the description._

## `parse`

`src/mlscores.rs:28`  (method / nested)

```rust
pub fn parse(name: &str) -> Scheme
```

Parses a scheme name written by the improvement round ("rolling 126 days", "half-life 42 days"); anything else (expanding, bags) is the default.

## `from_ledger`

`src/mlscores.rs:35`  (method / nested)

```rust
pub fn from_ledger(db: &rusqlite::Connection) -> Scheme
```

The best ADOPTED scheme in the trial ledger (highest IC), or the default.

## `tuned_ridge`

`src/mlscores.rs:42`

```rust
pub fn tuned_ridge(train: &[Sample], end: i64, window_days: Option<i64>, half_life_days: Option<f64>) -> crate::mlcandle::Ridge
```

Ridge with the strength chosen on the last quarter of the training period, optionally recency-weighted / windowed.

## `score_samples`

`src/mlscores.rs:61`

```rust
pub fn score_samples(all: &[Sample], member: &dyn Fn(&Sample) -> bool, scheme: Scheme) -> Vec<Option<f64>>
```

Scores for every sample of `all` (same order); None before the first fit.

## `table`

`src/mlscores.rs:83`

```rust
pub fn table(series: &BTreeMap<String, Vec<Bar>>, scheme: Scheme) -> BTreeMap<String, Vec<(i64, f64)>>
```

`{"SYMBOL": [[bar open ms, score], ...]}` for the bot's engine.

## `write`

`src/mlscores.rs:93`

```rust
pub fn write(series: &BTreeMap<String, Vec<Bar>>, path: &std::path::Path, scheme: Scheme) -> anyhow::Result<(usize, usize)>
```

_No comment in the code; the signature above is the description._

## `latest_view`

`src/mlscores.rs:107`

```rust
pub fn latest_view(path: &std::path::Path, symbols: &[String]) -> Option<(i64, Vec<(String, f64)>)>
```

Where the model ranks `symbols` at the newest scored bar in the score file: (bar open ms, [(symbol, percentile 0-100 among all scored symbols at that bar)]). None when the file is missing or empty. This is what the bot's backtester reads, so it is the model's actual view.

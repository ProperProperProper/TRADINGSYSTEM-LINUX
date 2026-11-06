# `examples/lab_export.rs`

> History export for the research lab (the research lab): rolling walk-forward backtests over every stored
> session, written to `backtest_archive.sqlite3` with each trade's entry-state features. No new
> candle pulls, no orders; the live service's rows are left alone (runs are keyed by settings,
> source, window and start, so re-running adds only what is new).
>
> Training data is cut into 14-session look-forward blocks (never one parameter set over a longer window, as the
> live windows of the longer intervals use). For every block, with settings chosen on the 14 sessions before it:
> - `oos`: the full search picks settings on the window ENDING before the block, champion/
> challenger against the previous block's settings, and those settings, held fixed, trade the
> block's 14 sessions from the run's start capital (`LAB_CAPITALS`) (unselected: kept whatever the result);
> - `random`: RANDOM settings drawn from the same space trade the same block from the same start capital (the unselected baseline that measures the selection effect);
> - `chosen`: the search winner on its own selection window (in-sample; trades kept only when it
> did well, the selection-biased population).
>
> Caveat, labelled in the cost-model string: the universe is TODAY's pool of liquid stocks applied
> to past sessions, so it carries survivorship/selection bias the live runs do not have.
>
> cargo run --release --example lab_export -- <interval key|all> [seeds=1] [random per session=8] [random-only]
>
> `random-only` skips the searches and only adds more unselected random-settings baselines (the lab
> needs enough unique entries per week; this strategy trades rarely).

4 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `with_features`

`examples/lab_export.rs:40`

```rust
fn with_features(m: &Market, closes: &[i64], sc: &[Vec<Option<scores::Score>>], p: &XsParams, trades: &[Trade]) -> Vec<(Trade, serde_json::Value)>
```

Trades with their entry features, times converted to real UTC; trades that cannot be mapped are left out.

## `profit_factor`

`examples/lab_export.rs:46`

```rust
fn profit_factor(trades: &[(Trade, serde_json::Value)]) -> f64
```

Profit factor of a trade list: gross profit over gross loss (infinite with no loss, 0 with no trades).

## `export`

`examples/lab_export.rs:64`

```rust
fn export(arch: &BtArchive, big: &Big, seed: u64, random_n: usize, random_only: bool, offsets: &[usize], start: f64, suffix: &str) -> Result<(usize, usize)>
```

Rolling look-forward blocks for one interval. For every block start (sessions `offset`, `offset + 14`, ...): - `oos`: the full search picks settings on the 14 sessions BEFORE the block (champion/challenger against the settings of the previous block), and those settings, held fixed, trade the next 14 sessions from the run's start capital; - `random`: random settings from the same space trade the same 14 sessions (the unselected baseline); - `chosen`: the search winner on its own 14-session selection window (in-sample, selection-biased).  Several `offsets` stagger the blocks so every part of the history is covered by differently-aligned blocks.

## `main`

`examples/lab_export.rs:166`

```rust
async fn main() -> Result<()>
```

Writes the research archive: rolling 14-session look-forward blocks (oos, random and chosen runs with entry features) for the lab to learn from.

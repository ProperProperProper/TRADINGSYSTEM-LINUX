# `src/mlwalk.rs`

> Walk-forward trading test WITH FEES and with new parameters chosen for every 14-session block (owner 2026-10-13).
>
> The procedure, for each test block of 14 trading sessions (the lab's block length):
> 1. the parameters are chosen from data BEFORE the block only: for every candidate (ridge strength, number of positions, entry time, minimum edge) the
> strategy is replayed on the three preceding 14-session blocks, each predicted by a model trained only on data older than that block, and the
> candidate with the best net return after fees is picked;
> 2. the model is refitted on everything older than the block (2 sessions of purge, the label looks a session ahead) with the chosen ridge strength;
> 3. the block is traded with the chosen parameters and nothing else is allowed to change. Block results are pooled.
>
> Fees are the bot's measured ones (`candles::Costs`): commission per order (about 1 USD) divided by the position size, plus the symbol's half-spread, on every
> buy and every sell; a name kept from one day to the next costs nothing. Position size = account / number of positions (5,000 USD).
> The return is the stock's own return over the next session (not an excess over the universe); a day without a trade earns 0 (cash).
> A control runs the identical procedure with models trained on labels shuffled within each moment (the returns being earned stay real).

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `grid`

`src/mlwalk.rs:28`

```rust
pub fn grid() -> Vec<Params>
```

_No comment in the code; the signature above is the description._

## `session_of`

`src/mlwalk.rs:34`

```rust
fn session_of(ts: i64) -> i64
```

_No comment in the code; the signature above is the description._

## `simulate`

`src/mlwalk.rs:40`

```rust
pub fn simulate(s: &[Sample], pred: &[f64], p: &Params, costs: &Costs, names: &[String]) -> Vec<Day>
```

Replays the daily strategy over the given samples (real returns in `raw`), scored by `pred` (aligned with `s`).

## `mean`

`src/mlwalk.rs:68`

```rust
fn mean(v: &[f64]) -> f64
```

_No comment in the code; the signature above is the description._

## `walk_forward`

`src/mlwalk.rs:71`

```rust
pub fn walk_forward(real: &[Sample], learn: &[Sample], costs: &Costs, names: &[String], fixed: Option<Params>) -> Vec<BlockResult>
```

`real` carries the true returns; `learn` is the same samples (same order) with the labels the models are trained on (identical to `real` except in the control).

## `summarize`

`src/mlwalk.rs:123`

```rust
pub fn summarize(days: &[Day]) -> Summary
```

_No comment in the code; the signature above is the description._

## `fmt`

`src/mlwalk.rs:140`

```rust
fn fmt(name: &str, s: &Summary) -> String
```

_No comment in the code; the signature above is the description._

## `report`

`src/mlwalk.rs:148`

```rust
pub fn report(all: &[Sample], names: &[String], costs: &Costs, freeze_session: i64) -> WalkReport
```

The full test. `freeze` is the session (New York date index) before which the procedure's design was chosen: blocks starting on or after it are the forward test.

## `date`

`src/mlwalk.rs:174`

```rust
pub fn date(session: i64) -> String
```

_No comment in the code; the signature above is the description._

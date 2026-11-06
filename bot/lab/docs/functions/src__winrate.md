# `src/winrate.rs`

> Why runs win or lose (user 2026-10-09: "anything under 60% win rate is a loser, anything above is a winner; it must learn why").
>
> Unit: one backtest run = one set of settings traded unchanged for one 14-session block, from the bot's archive (random and
> out-of-sample runs only: unselected, so no search bias), at the bot's capital, with at least `MIN_TRADES` closed trades.
> A run is a WINNER when at least 60% of its closed trades made money, else a LOSER.
>
> Candidate reasons, per run:
> - `setting`: every numeric or yes/no field of the settings, and each category (signal, regime, trend line kind ...);
> - `entry`: the average entry conditions of its trades (stock and market state at the decision: returns, distance from
> average, volume, SPY trend, breadth, time of day ...);
> - `exit mix` (how, not why): the share of its trades closed by each exit reason.
>
> Each reason is learned on the EARLIER two thirds of the blocks (direction and split point at the training median) and judged
> only on the LATER third: winner share on the good side minus the bad side, with a 95% interval from resampling whole blocks
> (trades in one block share the market, so they are not independent), and false-discovery control over all reasons.
> A reason is `confirmed` only when the later blocks agree. Nothing here is typed by the model; the model reads the result.

8 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`src/winrate.rs:63`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the findings tables if missing.

## `flatten`

`src/winrate.rs:75`

```rust
fn flatten(prefix: &str, v: &Value, out: &mut BTreeMap<String, (String, f64)>)
```

Flattens settings JSON into named numbers: numbers as they are, yes/no as 1/0, text as one category flag `name=value`.

## `load`

`src/winrate.rs:99`

```rust
fn load(data: &Connection, capital: f64) -> Result<Vec<RunObs>>
```

Loads the runs of one capital with their label and candidate reasons.

## `auc`

`src/winrate.rs:162`

```rust
fn auc(xs: &[(f64, bool)]) -> f64
```

Area under the ROC curve of `x` for telling winners from losers (0.5 = no information), ties counted half.

## `split_delta`

`src/winrate.rs:193`

```rust
fn split_delta(obs: &[(f64, bool, usize)], split: f64, direction: i8) -> Option<(f64, f64, f64)>
```

Winner share on the good side of `split` minus the bad side, for `direction` (+1: above is good).

## `analyse`

`src/winrate.rs:203`

```rust
pub fn analyse(mem: &Connection, data: &Connection, capital: f64) -> Result<(Value, Vec<Finding>)>
```

Learns every reason on the earlier blocks and judges it on the later ones; stores the findings and a summary. Returns them.

## `sentence`

`src/winrate.rs:292`

```rust
pub fn sentence(f: &Finding) -> String
```

One plain sentence for a finding, from its numbers only.

## `board_text`

`src/winrate.rs:303`

```rust
pub fn board_text(mem: &Connection, limit: usize) -> Result<String>
```

The stored findings as whiteboard text for the model: what makes winners and losers, confirmed first.

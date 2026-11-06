# `src/announce.rs`

> Tells the owner (Discord, via the already-approved `qwen-notify`) when the lab finds BETTER parameters -- in the backtest search or in the
> AI learning (owner 2026-10-13: "it should update and post when better params are found, whether in backtest or ai learning").
>
> Four detectors, each remembered in the memory database table `announced` so nothing is posted twice and a restart never re-posts:
> 1. a new best validated parameter set (holdout return up by at least `MIN_GAIN` points, or a new leader)
> 2. a variant that newly reaches `validated` (passes the holdout on cut A and the alternate cut B)
> 3. a new winning FORWARD run (unseen data) at or above the highlight threshold, with the lab's own explanation and its settings
> 4. a GRPO training run whose adapter beats the base model (or was promoted)
>
> The first scan only RECORDS what already exists (plus one summary post), so switching this on does not flood the channel.
> Numbers come straight from the lab's records; nothing is estimated here. Account details cannot appear: the messages hold strategy
> settings and percentages only, and `qwen-notify` redacts account-like tokens anyway.

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`src/announce.rs:22`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `get`

`src/announce.rs:27`

```rust
fn get(mem: &Connection, key: &str) -> Result<Option<f64>>
```

_No comment in the code; the signature above is the description._

## `put`

`src/announce.rs:30`

```rust
fn put(mem: &Connection, key: &str, value: f64) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `improved`

`src/announce.rs:36`

```rust
pub fn improved(prev: Option<f64>, now: f64) -> bool
```

True when `now` is a real improvement over the last announced value (or there is none yet).

## `f`

`src/announce.rs:38`

```rust
fn f(v: &Value, k: &str) -> f64
```

_No comment in the code; the signature above is the description._

## `pct`

`src/announce.rs:39`

```rust
fn pct(v: &Value, k: &str) -> String
```

_No comment in the code; the signature above is the description._

## `describe`

`src/announce.rs:42`

```rust
pub fn describe(row: &Value) -> String
```

One leaderboard row as an announcement body (the "parameters" are the option tokens, e.g. `kind:kama+univ60`).

## `post`

`src/announce.rs:50`

```rust
fn post(title: &str, body: &str)
```

_No comment in the code; the signature above is the description._

## `scan`

`src/announce.rs:53`

```rust
pub fn scan(mem: &Connection) -> Result<Vec<String>>
```

Runs all detectors; returns what was announced (also printed by the loop).

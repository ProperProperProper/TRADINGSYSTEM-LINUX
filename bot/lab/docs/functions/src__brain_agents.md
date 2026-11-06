# `src/brain_agents.rs`

> Six agents that DO things (owner 2026-10-13: "the local model needs agents which do things ... build it all ... make sure it learns 24x7").
> They join the six reporting roles in `brain_roles` and take turns in the same always-on process (a role every 5 minutes, so each runs hourly, day and night).
> Every action goes through code that checks it, is logged as a knowledge-store fact, and can be undone by deleting a plain file. None touches the live strategy,
> an account, an order or an exit.
>
> data          keeps the candle store current and clean; watches gaps, stale symbols and moomoo's candle quota (it never spends the quota)
> steward       owns the ML model: re-scores when new bars arrive, watches the last sessions' out-of-sample IC, switches the model off (file ML_GATE_OFF) when
> it is significantly negative and back on when it recovers
> drift         compares the paper bot's real fills with the candles: observed slippage against the cost table, and how the model rated the entries it took
> risk          watches the paper account against the owner's rules (drawdown cap, worst-block rule, positions) and alerts early
> promotion     prepares an evidence packet for every variant that passes the lab's full validation, and lists the ones that need a code review
> experimenter  finds where a lead loses (block by block against the baseline) and keeps the standing ML experiments running when no new data arrives

19 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `home`

`src/brain_agents.rs:24`

```rust
fn home() -> String
```

_No comment in the code; the signature above is the description._

## `bot_db`

`src/brain_agents.rs:25`

```rust
fn bot_db() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `scores_path`

`src/brain_agents.rs:26`

```rust
pub fn scores_path() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `ro`

`src/brain_agents.rs:27`

```rust
fn ro(p: &std::path::Path) -> rusqlite::Result<Connection>
```

_No comment in the code; the signature above is the description._

## `run`

`src/brain_agents.rs:29`

```rust
pub fn run(ctx: &Ctx, role: &str) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `alert`

`src/brain_agents.rs:36`

```rust
fn alert(ctx: &Ctx, role: &str, subject: &str, title: &str, text: &str)
```

_No comment in the code; the signature above is the description._

## `coverage`

`src/brain_agents.rs:45`

```rust
pub fn coverage(series: &BTreeMap<String, Vec<candles::Bar>>, symbol: &str, n: usize) -> Option<f64>
```

Sessions are the New York dates of the bars; coverage = bars present / bars expected (26 a session) over the last `n` sessions of the reference symbol.

Notes from the code: ------------------------------------------------------------------ data

## `data`

`src/brain_agents.rs:54`

```rust
pub fn data(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `recent_ic`

`src/brain_agents.rs:85`

```rust
pub fn recent_ic(series: &BTreeMap<String, Vec<candles::Bar>>, table: &BTreeMap<String, Vec<(i64, f64)>>, days: usize) -> Option<(f64, f64, usize)>
```

Mean and standard error of the daily rank IC of `table` (scores at bars) against the realised excess returns of the last `days` sessions with complete labels.

Notes from the code: ------------------------------------------------------------------ steward

## `gate_decision`

`src/brain_agents.rs:108`

```rust
pub fn gate_decision(currently_off: bool, mean: f64, se: f64, n: usize) -> Option<bool>
```

The switch decision with hysteresis: off when the mean is significantly negative (95%), back on once it is above zero. None = keep the state.

## `steward`

`src/brain_agents.rs:115`

```rust
pub fn steward(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `drift`

`src/brain_agents.rs:193`

```rust
pub fn drift(ctx: &Ctx) -> Result<Vec<String>>
```

Notes from the code: ------------------------------------------------------------------ drift

## `risk`

`src/brain_agents.rs:228`

```rust
pub fn risk(ctx: &Ctx) -> Result<Vec<String>>
```

Notes from the code: ------------------------------------------------------------------ risk

## `promotion`

`src/brain_agents.rs:253`

```rust
pub fn promotion(ctx: &Ctx) -> Result<Vec<String>>
```

Notes from the code: ------------------------------------------------------------------ promotion

## `lost_blocks`

`src/brain_agents.rs:280`

```rust
pub fn lost_blocks(mem: &Connection, tokens: &str) -> Result<(usize, Vec<(i64, f64, f64)>)>
```

Blocks (cut A, 5h) where `tokens` lost to the baseline by more than 2 points: (block start s, variant ret, baseline ret).

Notes from the code: ------------------------------------------------------------------ experimenter

## `experimenter`

`src/brain_agents.rs:288`

```rust
pub fn experimenter(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `latest_fact`

`src/brain_agents.rs:320`

```rust
fn latest_fact(mem: &Connection, role: &str, kind: &str) -> Option<String>
```

Notes from the code: ------------------------------------------------------------------ the daily digest

## `last_commit`

`src/brain_agents.rs:324`

```rust
fn last_commit(repo: &str) -> String
```

_No comment in the code; the signature above is the description._

## `digest`

`src/brain_agents.rs:331`

```rust
pub fn digest(ctx: &Ctx) -> Result<Option<String>>
```

One short status for the owner's Discord every day, built only from the knowledge store and git (no new computation): the model and its recent out-of-sample IC, the daily ML verdicts, the standing experiments, the data and the paper account, and each repo's latest commit. Returns the posted text.

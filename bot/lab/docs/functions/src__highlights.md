# `src/highlights.rs`

> Winning runs worth a human's attention: any backtest block of 14 sessions that returned 15% or more (user rule). For each one the lab
> records the exact settings and trades, works out WHY from the data (which stocks and entry conditions it favoured, how concentrated
> the profit was, how the random settings of the same window did: a luck check), has the local model write the explanation with every
> percentage checked against those facts, and REMEMBERS it: the finding is stored in the memory database, shown to the model on its
> whiteboard, and written to `HIGHLIGHTS.md` in the lab root.
>
> Nothing here is invented: every figure comes from the imported runs and trades; when the model's text fails the checks, a plain
> sentence built by code from the same facts is stored instead.

6 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`src/highlights.rs:21`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the highlights table if missing.

## `facts`

`src/highlights.rs:32`

```rust
fn facts(data: &Connection, run_id: &str, label: &str, w_start: i64, ret: f64) -> Result<Value>
```

The facts about one winning run, computed from the stored runs and trades.

## `code_why`

`src/highlights.rs:74`

```rust
pub(crate) fn code_why(f: &Value, forward: bool) -> String
```

A plain sentence from the facts alone (used when the model's text fails its checks, or the model is unavailable).

## `scan`

`src/highlights.rs:95`

```rust
pub fn scan(ctx: &Ctx) -> Result<Vec<String>>
```

Find, explain and remember every new run at or above the threshold; (re)write `HIGHLIGHTS.md`. Returns what happened.

## `write_report`

`src/highlights.rs:121`

```rust
pub fn write_report(ctx: &Ctx) -> Result<()>
```

`HIGHLIGHTS.md` in the lab root: every remembered winning run, forward results first.

## `board_lines`

`src/highlights.rs:142`

```rust
pub fn board_lines(mem: &Connection) -> Result<String>
```

What the model remembers: the newest highlights with their explanation, for its whiteboard.

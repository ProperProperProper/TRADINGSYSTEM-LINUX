# `src/tick.rs`

> The scheduled hourly tick. One idempotent, resumable, single-instance pass that imports the real archives (the bot's paper record and backtest
> archive), refreshes the win/lose analysis and the promotion request, writes the report, backs up, and checks the size limit. Every step is
> guarded, time-boxed and logged; a failing step never stops the next one; with no data it does nothing harmful and says so.

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `live`

`src/tick.rs:22`  (method / nested)

```rust
pub fn live(_cfg: &Config) -> Self
```

Live probes of this machine: load, cores, free memory.

## `get`

`src/tick.rs:50`  (method / nested)

```rust
pub fn get(&self, name: &str) -> Option<&Step>
```

The step called `name`.

## `text`

`src/tick.rs:52`  (method / nested)

```rust
pub fn text(&self) -> String
```

The tick report as text, one line per step.

## `pid_alive`

`src/tick.rs:64`

```rust
fn pid_alive(pid: u32) -> bool
```

Whether a process with this id exists (signal 0 checks without sending anything).

## `counter`

`src/tick.rs:69`

```rust
fn counter(mem: &rusqlite::Connection, name: &str) -> Option<i64>
```

A named counter from the memory database, if set.

## `set_counter`

`src/tick.rs:74`

```rust
fn set_counter(mem: &rusqlite::Connection, name: &str, v: i64)
```

Sets a named counter in the memory database.

## `auto_full`

`src/tick.rs:79`

```rust
pub fn auto_full(cfg: &Config, mem: &rusqlite::Connection, now: i64, local_hour: u32) -> bool
```

Decide light or full for a scheduler invocation: full once a day at the configured hour, otherwise light.

## `tick`

`src/tick.rs:88`

```rust
pub fn tick(ctx: &Ctx, opts: &TickOptions, probes: &Probes) -> Result<TickReport>
```

One scheduled tick: guards, then imports and the light or full learning steps; writes a tick log.

## `finish`

`src/tick.rs:194`

```rust
fn finish(cfg: &Config, mut rep: TickReport) -> Result<TickReport>
```

Writes the tick log and deletes tick logs older than 30 days.

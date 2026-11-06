# `lab/src/btarchive.rs`

> The bot's backtest archive as the main real input (spec 4.4). Read-only, incremental, idempotent import of
> `bt_runs`, `bt_trades` (with the market state at each entry decision) and `bt_daily` into the lab's `runs`,
> `trades`, `trade_features` and `daily` tables. Features are flattened into the DYNAMIC feature registry, so fields the
> bot adds later appear automatically. Weekly blocks cut by entry time become the lab's independent windows.
>
> The archive stores trades only for runs that passed the bot's success filter, and the search runs are the best of
> 100,000 in-sample. Every imported trade-level run is therefore flagged (`selection`), which caps confidence and
> requires the out-of-sample (`oos`) check for a lesson (rules.rs).

13 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `counter`

`lab/src/btarchive.rs:43`

```rust
fn counter(mem: &Connection, name: &str) -> Result<Option<i64>>
```

Reads a named counter from the memory database (None if never set).

## `set_counter`

`lab/src/btarchive.rs:48`

```rust
fn set_counter(mem: &Connection, name: &str, v: i64) -> Result<()>
```

Sets a named counter in the memory database.

## `check_schema`

`lab/src/btarchive.rs:54`

```rust
fn check_schema(c: &Connection) -> std::result::Result<(), String>
```

Checks the bot's archive has every required table and column; returns what is missing.

## `has_col`

`lab/src/btarchive.rs:65`

```rust
fn has_col(c: &Connection, table: &str, col: &str) -> bool
```

Whether `table` has a column named `col`.

## `block_label`

`lab/src/btarchive.rs:73`

```rust
pub fn block_label(ts_secs: i64, block_secs: i64) -> String
```

Block label of a time (e.g. "b2931"): which block of `block_secs` seconds it falls in.

## `snake`

`lab/src/btarchive.rs:76`

```rust
fn snake(key: &str) -> String
```

Turn a JSON key into a valid feature-name fragment.

## `num`

`lab/src/btarchive.rs:85`

```rust
fn num(v: &Value) -> Option<f64>
```

A JSON number (or boolean as 1/0) as a finite f64.

## `flatten_features`

`lab/src/btarchive.rs:95`

```rust
pub fn flatten_features(v: &Value) -> BTreeMap<String, f64>
```

Flatten the bot's per-trade `features` JSON into named numbers. Known fields get stable names; any other numeric leaf under `coin`, `trend` or `zones` becomes `c_*` / `m_*` automatically. Missing or null values are simply absent.

## `import`

`lab/src/btarchive.rs:147`

```rust
pub fn import(cfg: &Config, mem: &Connection, data: &Connection) -> Result<BtReport>
```

Imports new runs, trades and daily rows from the bot's backtest archive (read-only) into the lab's data database.

## `reconcile`

`lab/src/btarchive.rs:293`

```rust
fn reconcile(mem: &Connection, src: &Connection, tx: &Connection, epoch: i64, imported_trades_upto: i64) -> Result<()>
```

Fill in details for rows imported earlier (for example by an older build of the lab) that the bot has since added to the same archive rows: commit and cost model, tweak tag, the true selection status of out-of-sample/random runs, and decision times. Never overwrites a value that is already set, and never touches rows the archive does not know.

## `current`

`lab/src/btarchive.rs:336`

```rust
pub fn current(data: &Connection) -> Result<BtReport>
```

Report of what the lab already holds from the archive, without importing.

## `finish`

`lab/src/btarchive.rs:341`

```rust
fn finish(data: &Connection, mut rep: BtReport) -> Result<BtReport>
```

Fills the report's totals (runs, runs with trades, oos runs, trades) from the data database.

## `import_best_effort`

`lab/src/btarchive.rs:350`

```rust
pub fn import_best_effort(cfg: &Config, mem: &Connection, data: &Connection) -> BtReport
```

Import that never fails: an error becomes a report with source "error" and the message.

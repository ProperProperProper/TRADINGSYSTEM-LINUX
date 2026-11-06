# `lab/src/paper_moomoo.rs`

> Forward record from the moomoo US-stock bot (paper account, USD, long only, whole shares, no funding).
>
> The moomoo bot keeps one SQLite store (`moomoo.sqlite3`). This reads it strictly read-only and copies its paper
> tables into the same lab tables the crypto importer fills, so everything downstream (sufficiency ladder, prompt
> block, search-decay check, dashboard) is shared. Mapping: leverage is 1, funding is 0, and the return per trade is
> `pnl / (entry * qty)` (the stock bot has no margin). Timestamps are milliseconds, as in the lab.

4 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `columns`

`lab/src/paper_moomoo.rs:25`

```rust
fn columns(c: &Connection, table: &str) -> Result<Vec<String>>
```

Column names of `table`.

## `check_schema`

`lab/src/paper_moomoo.rs:32`

```rust
fn check_schema(c: &Connection) -> std::result::Result<(), String>
```

Checks the moomoo store has every required table and column; returns what is missing.

## `params_id`

`lab/src/paper_moomoo.rs:44`

```rust
fn params_id(json: &str) -> i64
```

Stable positive id for a parameter set: the first 6 bytes of its hash (the stock store has no params table).

## `import`

`lab/src/paper_moomoo.rs:49`

```rust
pub fn import(cfg: &Config, mem: &Connection, data: &Connection, now_ms: i64, mut rep: PaperReport) -> Result<PaperReport>
```

Imports the moomoo bot's paper record (equity, trades, searches) read-only into the lab's paper tables.

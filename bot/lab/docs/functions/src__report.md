# `src/report.rs`

> The lab's daily report: data coverage, the paper record, the backtest archive by population, operations and alerts. Numbers first.

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `one`

`src/report.rs:10`

```rust
fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T
```

First column of the first row of `sql` (panics on error; report building only).

## `build`

`src/report.rs:13`

```rust
pub fn build(cfg: &Config, mem: &Connection, data: &Connection) -> Result<String>
```

Builds the lab's markdown report: data coverage, paper record, backtest archive by population, operations and alerts.

## `write`

`src/report.rs:71`

```rust
pub fn write(cfg: &Config, mem: &Connection, data: &Connection) -> Result<std::path::PathBuf>
```

Writes today's report to the reports folder.

# `src/contract.rs`

> The export contract (spec section 4): file formats, manifest, and validation of one run before it is ingested.

5 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `trades_header`

`src/contract.rs:83`

```rust
pub fn trades_header(features: &[&str]) -> Vec<String>
```

Column names of a trades CSV: the fixed columns plus one `f_<name>` per feature.

## `scrub`

`src/contract.rs:94`

```rust
pub fn scrub(text: &str) -> Result<()>
```

Refuses text that contains a forbidden field (credentials, balances) as a JSON key or CSV column.

## `parse_trades`

`src/contract.rs:105`

```rust
pub fn parse_trades(text: &str) -> Result<(Vec<TradeRow>, bool)>
```

Parses a trades CSV into rows; features come from its `f_*` columns. Also returns true (degraded) when the file has no feature columns.

## `parse_daily`

`src/contract.rs:139`

```rust
pub fn parse_daily(text: &str) -> Result<Vec<DailyRow>>
```

Parses a daily CSV (day, equity, pnl), rejecting non-finite numbers.

## `validate`

`src/contract.rs:152`

```rust
pub fn validate(h: &RunHeader, trades: &[TradeRow], daily: &[DailyRow]) -> Result<()>
```

Quality gate (spec 4.1). Err = reject the whole run (quarantine).

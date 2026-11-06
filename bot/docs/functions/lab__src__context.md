# `lab/src/context.rs`

> The lab's shared context (config and the two databases) and the data windows. Weekly blocks of the bot's backtest archive are windows only when COMPLETE.

2 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `windows_info`

`lab/src/context.rs:26`

```rust
pub fn windows_info(data: &Connection) -> Result<Vec<(String, bool)>>
```

Every window the lab may use, with whether it is COMPLETE. A window with enough trades that is still growing, or that the bot's runs cover only partly (the archive started part-way through that week), is PROVISIONAL: it is used for exploration and re-assessed as it grows, but a claim whose TEST windows include one can never be called replicated (veto `V_PROVISIONAL`).

## `windows`

`lab/src/context.rs:65`

```rust
pub fn windows(data: &Connection) -> Result<Vec<String>>
```

Complete windows only (what lessons and replication rest on).

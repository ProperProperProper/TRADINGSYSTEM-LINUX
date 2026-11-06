# `src/bin/bot/main.rs`

> moomoo mean-reversion bot for US stocks, long only, 5h candles,
> settings from a random search, decisions executed as REAL moomoo paper
> orders (US SIMULATE account only).
>
> bot serve        paper-trading service + console on 127.0.0.1:8789

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`src/bin/bot/main.rs:14`

```rust
async fn main() -> Result<()>
```

Entry point: `bot serve` runs the trading service from the runtime folder.

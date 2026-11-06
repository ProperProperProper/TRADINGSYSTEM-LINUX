# `src/bin/probe.rs`

> Answers, from moomoo OpenD itself, what the bot can use: login state, quote rights, the
> historical-candle quota, the trading accounts (paper and real) with their markets, paper
> balances, and whether 15-minute candles come back for a US stock (AAPL) and an ASX stock (BHP).
> Read-only: places no orders. Uses 2 of the 7-day historical-candle quota (AAPL, BHP), and
> none if they were already pulled within 7 days.
>
> cargo run --release --bin probe            (OpenD must be running and logged in)
>
> Env: `OPEND=<host:port>` (default 127.0.0.1:11111).

2 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `market_name`

`src/bin/probe.rs:15`

```rust
fn market_name(m: i32) -> &'static str
```

Human name of a moomoo market code.

## `main`

`src/bin/probe.rs:31`

```rust
async fn main() -> Result<()>
```

Read-only probe of OpenD: login state, accounts, funds, candle quota and a sample candle request.

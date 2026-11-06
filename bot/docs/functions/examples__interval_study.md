# `examples/interval_study.rs`

> Release check for every candle interval (`Interval::ALL`: 15m, 1h, 2h, 3h, 4h, 5h, one session) on REAL moomoo data: today's
> universe (SPY's holdings, NYSE/Nasdaq, nothing delisted), rules from real snapshot quotes and the
> bot's stored real fees (commission excluded when none exist), each interval built exactly like
> the service does (15m candles aggregated per session, 1h / 4h / one session with warm-up candles
> before their 21-session window), then one full search per interval. Prints what the service's
> interval table will show. Never places orders. Respects the 7-day candle quota (a symbol not
> pulled in the window is skipped once only the reserve is left).
>
> cargo run --release --example interval_study

2 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `stored_fees`

`examples/interval_study.rs:18`

```rust
fn stored_fees() -> Vec<(f64, f64)>
```

Real fee observations (notional, fee) from the bot's store, if it exists.

## `main`

`examples/interval_study.rs:28`

```rust
async fn main() -> Result<()>
```

Release check of every candle interval on real moomoo data: builds each interval like the service and runs one full search per interval.

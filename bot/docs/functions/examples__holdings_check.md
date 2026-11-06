# `examples/holdings_check.rs`

> Keep only the SPY holdings moomoo can trade: reads a holdings CSV (from ops/spy_holdings_xlsx.py), asks moomoo for the
> static information of every ticker in paced batches, and writes
> - `data/spy_holdings.csv`: the holdings that are eligible (NYSE/Nasdaq, not delisting, known to moomoo), the file's own
> rank, name and weight kept;
> - `data/spy_removed.csv`: the holdings left out, with moomoo's reason.
>
> The bot's universe is exactly `data/spy_holdings.csv`. Refuses to write when moomoo answered for too few tickers.
>
> cargo run --release --example holdings_check -- <candidate.csv>

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/holdings_check.rs:15`

```rust
async fn main() -> Result<()>
```

Keeps only the SPY holdings moomoo can trade: checks every ticker's static info and writes data/spy_holdings.csv and data/spy_removed.csv.

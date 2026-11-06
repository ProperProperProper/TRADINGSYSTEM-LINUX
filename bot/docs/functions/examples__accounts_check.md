# `examples/accounts_check.rs`

> Read-only: which moomoo PAPER accounts OpenD reports and their balances. Real accounts are only counted,
> never queried. No candle requests (no quota), no orders.
>
> cargo run --release --example accounts_check

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/accounts_check.rs:11`

```rust
async fn main() -> Result<()>
```

Lists the accounts OpenD reports and prints the PAPER accounts' funds; real accounts are counted but never queried.

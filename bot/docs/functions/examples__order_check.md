# `examples/order_check.rs`

> Verifies, on the PAPER account only and through the bot's own `Gateway` code path
> (`place_market`, `orders`, `account`, `order_fees`), what the bot relies on for execution: a
> market order placed with TrdEnv SIMULATE fills and the order list reports its fill quantity and
> average price. Trd_GetOrderFee is tried too (moomoo answers "Paper trading is not supported"),
> so each order's fee is measured as the paper cash change around its fill minus the fill value
> (`derived_fee`) and stored as a real fee observation in the bot's store. Buys `QTY` share(s) of
> `SYMBOL` and sells them again. The gateway refuses anything but the US SIMULATE account.
>
> cargo run --release --example order_check -- SYMBOL [QTY]     (US market must be open)

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/order_check.rs:17`

```rust
async fn main() -> Result<()>
```

Buys and sells one small lot on the PAPER account and prints the real fills, fees and cash change (end-to-end order check).

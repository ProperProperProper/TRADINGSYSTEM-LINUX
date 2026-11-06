# `examples/fill_probe.rs`

> How does the moomoo PAPER simulator fill orders of increasing size? For one very liquid stock: read the quote, BUY at market, wait for the
> fill, then SELL the same quantity straight back, and record fill price against the quote and the fee (from the cash change). PAPER account only
> (the gateway refuses anything else); needs the US market open. The results are appended to data/fill_probe.json: real measurements that the
> liquidity model is calibrated on, instead of the top-of-book snapshot it used.
>
> cargo run --release --example fill_probe -- [SYMBOL=AAPL] [notionals=300,30000,150000,500000]

2 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `fill`

`examples/fill_probe.rs:13`

```rust
async fn fill(gw: &Gateway, symbol: &str, side: i32, qty: f64) -> Result<(f64, f64, f64, f64)>
```

Places one paper market order, waits up to 60 s for it to fill completely, and returns (filled qty, average price, fee derived from the cash change, cash after).

## `main`

`examples/fill_probe.rs:35`

```rust
async fn main() -> Result<()>
```

Fill probe: buys then sells a ladder of order sizes on the PAPER account and records fill completeness and slippage in data/fill_probe.json.

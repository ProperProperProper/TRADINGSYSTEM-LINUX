# `examples/fractional_check.rs`

> Does moomoo's US PAPER (SIMULATE) account accept a fractional share quantity over the API?
> Places ONE limit buy of `QTY` (default 0.5) share of `SYMBOL` at half the current bid (so it
> cannot fill), reads the order back, then cancels it. Paper account only (the gateway refuses
> anything else). Leaves no position behind.
>
> cargo run --release --example fractional_check -- [SYMBOL] [QTY]

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/fractional_check.rs:13`

```rust
async fn main() -> Result<()>
```

Checks whether moomoo paper accepts a fractional-share order, with a limit price far below the bid so it cannot fill; cancels it afterwards.

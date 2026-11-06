# `examples/universe_probe.rs`

> Read-only: does moomoo publish S&P 500 / index membership as a "plate" (a sector or index set)? Lists the US plates whose
> name looks like an S&P / index set and, for a chosen plate, its members with the delisting flag. No candle requests, no orders.
>
> cargo run --release --example universe_probe                      # list matching plates
> cargo run --release --example universe_probe -- <plate code>      # members of a plate, e.g. US.BK1001

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`examples/universe_probe.rs:12`

```rust
async fn main() -> Result<()>
```

Probe: lists a moomoo plate's members (with a code argument) or the available plates; read-only.

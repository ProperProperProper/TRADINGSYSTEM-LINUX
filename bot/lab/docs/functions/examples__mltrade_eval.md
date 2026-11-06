# `examples/mltrade_eval.rs`

> Does the bot's own entry data predict winning trades? Walk-forward test of the self-written models on the REAL trades:
> `cargo run --release --example mltrade_eval -- <copy of data.sqlite3>`. Train = earlier two thirds of the 14-session blocks, test = the later third,
> judged once. Results and conclusions: docs/BRAIN.md.

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `run`

`examples/mltrade_eval.rs:6`

```rust
fn run(title: &str, trades: &[Trade])
```

_No comment in the code; the signature above is the description._

## `run_models`

`examples/mltrade_eval.rs:12`

```rust
fn run_models(train: &[Trade], test: &[Trade])
```

_No comment in the code; the signature above is the description._

## `main`

`examples/mltrade_eval.rs:38`

```rust
fn main() -> anyhow::Result<()>
```

_No comment in the code; the signature above is the description._

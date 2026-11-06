# `lab/examples/brain_learning.rs`

> Does the model LEARN? Learning curves on the lab's real data: the same model trained on more and more history, judged each time on the same
> fixed later data it never saw. `cargo run --release --example brain_learning -- <copy of memory.sqlite3> <copy of data.sqlite3>`.

2 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `logloss`

`lab/examples/brain_learning.rs:6`

```rust
fn logloss(p: &[f64], y: &[bool]) -> f64
```

_No comment in the code; the signature above is the description._

## `main`

`lab/examples/brain_learning.rs:8`

```rust
fn main() -> anyhow::Result<()>
```

_No comment in the code; the signature above is the description._

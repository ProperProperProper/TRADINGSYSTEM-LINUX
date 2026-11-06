# `src/bin/qwen-notify.rs`

> `qwen-notify [--title T] [MESSAGE]` -- posts a change notification to the owner's Discord (message from stdin when omitted).
> Exit 0 when sent, skipped as a duplicate, or not configured (so shell scripts can call it without `|| true`); 1 only on a send error.

1 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `main`

`src/bin/qwen-notify.rs:5`

```rust
fn main()
```

_No comment in the code; the signature above is the description._

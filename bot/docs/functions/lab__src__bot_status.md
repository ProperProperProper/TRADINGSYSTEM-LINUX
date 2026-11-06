# `lab/src/bot_status.rs`

> The two JSON summaries inside the bot's check.sh (formerly inline Python): `qwen-tools bot-status` (stdin = the console's /api/status) and
> `qwen-tools bot-winners` (stdin = /api/winners). The lab's `bot_status` tool and the scheduled checks call check.sh, which pipes into these.

6 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `py_str`

`lab/src/bot_status.rs:6`

```rust
fn py_str(v: Option<&Value>) -> String
```

_No comment in the code; the signature above is the description._

## `truthy`

`lab/src/bot_status.rs:9`

```rust
fn truthy(v: Option<&Value>) -> bool
```

_No comment in the code; the signature above is the description._

## `num`

`lab/src/bot_status.rs:10`

```rust
fn num(v: Option<&Value>) -> f64
```

_No comment in the code; the signature above is the description._

## `status`

`lab/src/bot_status.rs:12`

```rust
pub fn status(input: &str) -> Vec<String>
```

_No comment in the code; the signature above is the description._

## `winners`

`lab/src/bot_status.rs:23`

```rust
pub fn winners(input: &str) -> Option<String>
```

_No comment in the code; the signature above is the description._

## `main`

`lab/src/bot_status.rs:31`

```rust
pub fn main(which: &str) -> i32
```

_No comment in the code; the signature above is the description._

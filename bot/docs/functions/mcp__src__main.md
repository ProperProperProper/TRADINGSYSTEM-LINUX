# `mcp/src/main.rs`

> MCP server (stdio, JSON-RPC 2.0) giving Claude Code COMPACT views of the moomoo paper bot, so
> checks cost a few lines instead of raw logs and JSON. Read-only: never changes the bot, never
> talks to OpenD, never places orders.
>
> Tools: moomoo_check (health = the repo's check.sh), moomoo_results (interval winners and
> out-of-sample tests), moomoo_orders (real paper orders, holdings, recent paper trades).

8 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `get`

`mcp/src/main.rs:15`

```rust
fn get(path: &str) -> Result<Value, String>
```

GET a console API path; Err when the console does not answer.

## `utc`

`mcp/src/main.rs:26`

```rust
fn utc(ms: &Value) -> String
```

_No comment in the code; the signature above is the description._

## `pct`

`mcp/src/main.rs:33`

```rust
fn pct(v: &Value) -> String
```

_No comment in the code; the signature above is the description._

## `check`

`mcp/src/main.rs:37`

```rust
fn check(minutes: i64) -> (String, bool)
```

_No comment in the code; the signature above is the description._

## `results`

`mcp/src/main.rs:45`

```rust
fn results() -> (String, bool)
```

_No comment in the code; the signature above is the description._

## `orders`

`mcp/src/main.rs:63`

```rust
fn orders() -> (String, bool)
```

_No comment in the code; the signature above is the description._

## `tools`

`mcp/src/main.rs:88`

```rust
fn tools() -> Value
```

_No comment in the code; the signature above is the description._

## `main`

`mcp/src/main.rs:96`

```rust
fn main()
```

_No comment in the code; the signature above is the description._

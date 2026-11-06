# `src/action_items.rs`

> ACTION ITEMS for the quant supervisor : every verdict and open problem the lab dashboard shows, from
> its JSON snapshot on stdin. Used by supervisor/quant_supervisor.sh; prints one "- item" per line.

7 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `w`

`src/action_items.rs:7`

```rust
fn w<'a>(o: &'a Value, k: &str) -> Option<&'a Value>
```

First value stored under key `k` anywhere in nested objects (depth-first, objects only, like the original). A JSON null counts as absent.

## `truthy`

`src/action_items.rs:12`

```rust
fn truthy(v: &Value) -> bool
```

_No comment in the code; the signature above is the description._

## `wo`

`src/action_items.rs:16`

```rust
fn wo(d: &Value, k: &str, default: Value) -> Value
```

`w(d, k) or default`

## `s`

`src/action_items.rs:17`

```rust
fn s(v: &Value) -> &str
```

_No comment in the code; the signature above is the description._

## `clip`

`src/action_items.rs:18`

```rust
fn clip(t: &str, n: usize) -> String
```

_No comment in the code; the signature above is the description._

## `items`

`src/action_items.rs:20`

```rust
pub fn items(d: &Value) -> Vec<String>
```

_No comment in the code; the signature above is the description._

## `main`

`src/action_items.rs:43`

```rust
pub fn main()
```

_No comment in the code; the signature above is the description._

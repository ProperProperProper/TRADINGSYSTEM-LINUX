# `src/engine/metrics.rs`

> Closed-trade records and performance metrics shared by every engine (costs
> come from each symbol's moomoo rules, `rules::Rules`).

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `net`

`src/engine/metrics.rs:47`  (method / nested)

```rust
pub fn net(&self) -> f64
```

Net result in USD: end equity plus open unrealized PnL minus start equity.

## `return_pct`

`src/engine/metrics.rs:51`  (method / nested)

```rust
pub fn return_pct(&self) -> f64
```

Net result as % of the start equity (0 with no start equity).

## `profit_factor`

`src/engine/metrics.rs:59`  (method / nested)

```rust
pub fn profit_factor(&self) -> f64
```

Gross profit over gross loss (infinite with no loss, 0 with no trades).

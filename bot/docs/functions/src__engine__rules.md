# `src/engine/rules.rs`

> Per-symbol trading rules, all taken from moomoo, never assumed:
> * order rules from the static security info (lot size = whole shares),
> * this account's own commission for each order size, from the real fees of its paper orders
> (Trd_GetOrderFee; excluded until a real fee exists),
> * the cost of a market order of a given size: the measured top-of-book bid/ask walked
> like a book, or, under the liquidity model, half-spread plus square-root impact on real ADV,
> * the leverage cap (1x: the user's no-leverage rule for this bot).
>
> moomoo publishes no historical books, so backtests use the most recent measurement
> (`book_ts` says when); the live service re-measures every hour. A symbol without rules is
> not traded, and an order larger than the measured depth is not filled.

13 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `order_cap_usd`

`src/engine/rules.rs:30`

```rust
fn order_cap_usd() -> Option<f64>
```

The research-only per-order USD cap, when set.

## `liquidity_model_on`

`src/engine/rules.rs:38`

```rust
pub fn liquidity_model_on() -> bool
```

Whether the liquidity model (ADV participation cap and square-root impact) is on.

## `book_points`

`src/engine/rules.rs:47`

```rust
pub fn book_points() -> impl Iterator<Item = f64>
```

Order sizes (USD notional) at which the book cost and the commission are measured: 10, 20, 40, ... (the measurements end where the data ends).

Notes from the code: Note: No fixed upper size (user rule: no fixed parameters). The ladder doubles from 10 USD; measurement stops where the data stops it: the quote's displayed depth (book cost) or the largest order with a real fee from this account (commission).

## `order_qty`

`src/engine/rules.rs:80`  (method / nested)

```rust
pub fn order_qty(&self, qty: f64, price: f64) -> Option<f64>
```

Quantity moomoo accepts for an order of about `qty` at `price`: rounded down to the size increment; None below the minimum size or order value.

## `order_notional_cap`

`src/engine/rules.rs:103`  (method / nested)

```rust
pub fn order_notional_cap(&self, reference: f64) -> Option<f64>
```

Leave half of the measured book capacity as a closing-liquidity reserve. Large balances keep unallocated cash instead of assuming infinite depth.

Notes from the code: Note: Half the measured depth is kept free so the position can be closed; don't raise it to fit bigger orders. Large balances keep cash instead.

## `book_notional_cap`

`src/engine/rules.rs:109`  (method / nested)

```rust
fn book_notional_cap(&self, reference: f64) -> Option<f64>
```

Largest order notional for this symbol: 0.2% of ADV under the liquidity model, else half the displayed book depth.

## `book_cost`

`src/engine/rules.rs:122`  (method / nested)

```rust
pub fn book_cost(&self, notional: f64, buy: bool) -> Option<f64>
```

Market-order cost (fraction of price) for `notional` USD, buying or selling, interpolated between measured sizes; None if the measured book was too thin for that size.

Notes from the code: Note: Beyond measured depth this returns None (no fill). Never extrapolate: the old linear extrapolation produced negative exit prices.

## `fee`

`src/engine/rules.rs:161`  (method / nested)

```rust
pub fn fee(&self, notional: f64) -> Option<f64>
```

This account's commission (USD) for an order of `notional` USD, interpolated between the measured sizes (the measured minimum below the smallest); None when unmeasured.

Notes from the code: Note: US stock fees have per-share parts and per-order minimums, so the fee is a function of the order size, never a flat rate. Beyond the largest measured size: None.

## `sized_notional`

`src/engine/rules.rs:187`  (method / nested)

```rust
pub fn sized_notional(&self, budget: f64, leverage: f64, cost_mult: f64, cap: f64) -> Option<f64>
```

The order notional whose margin plus commission (× `cost_mult`) uses `budget` at `leverage`, capped at `cap`; None when the commission is unmeasured at that size.

## `liquidation_price`

`src/engine/rules.rs:211`  (method / nested)

```rust
pub fn liquidation_price( &self, side: super::Side, qty: f64, entry: f64, margin: f64, _fee_mult: f64, ) -> Option<f64>
```

Price at which a position's loss uses up its posted margin. At 1x (cash) a long's is zero: it cannot be liquidated.

## `leverage_allowed`

`src/engine/rules.rs:232`  (method / nested)

```rust
pub fn leverage_allowed(&self, value: f64, leverage: f64) -> bool
```

Whether `leverage` is allowed for an order of `value` (finite and within the symbol's maximum).

## `walk_book`

`src/engine/rules.rs:268`

```rust
pub fn walk_book(levels: &[(f64, f64)], mid: f64, notional: f64) -> Option<f64>
```

Cost of a market order of `notional` USD against one side of the book (`levels`: (price, size) best first), as a fraction of `mid`; None when the levels cannot fill it.

## `measure_book`

`src/engine/rules.rs:294`

```rust
pub fn measure_book(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> Vec<(f64, f64, f64)>
```

Book cost at every book_points() size the book can fill.

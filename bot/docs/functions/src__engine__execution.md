# `src/engine/execution.rs`

> Real moomoo paper orders for the engine's decisions, and reconciliation of the engine's
> paper account with what moomoo reports.
>
> The engine (`xs::XsPortfolio`) decides exactly as in the source and simulates every fill at a
> candle open, which it can only do after that candle has CLOSED. Live, the order has to go out
> AT the open. So, at each open of the paper interval:
>
> 1. `preview`: a copy of the paper portfolio steps one candle whose prices are the current real
> quote mid (open = high = low = close). What that copy executes at the open (exits, entries,
> adds) is exactly what the engine's own code decides at this open given today's price; those
> become MARKET orders (sells first). The copy is discarded; nothing of it is recorded.
> 2. When the candle has closed, the engine steps it with the real candle (as in the source).
> `Ledger::link_step` ties the engine's fills to the orders, and `Ledger::recompute`
> overwrites every simulated fill with moomoo's real fill quantity, average price and fee
> (equity adjusted by the difference). A fill the engine simulated without a real order is
> removed (and logged); a real fill the engine did not make is sold at the next open and its
> real result booked as a reconciliation trade; an engine exit that has no real sell yet
> (an intrabar stop, a failed order) is sold at the next open.
>
> Nothing is invented: until moomoo reports a fill or fee, the engine's simulated value stays
> (marked pending); an order that never fills contributes nothing.

23 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `is_final`

`src/engine/execution.rs:82`  (method / nested)

```rust
pub fn is_final(&self) -> bool
```

No further change can happen (refused, or a final moomoo state).

## `filled`

`src/engine/execution.rs:86`  (method / nested)

```rust
pub fn filled(&self) -> f64
```

Quantity filled so far (0 before any state).

## `fill`

`src/engine/execution.rs:90`  (method / nested)

```rust
pub fn fill(&self) -> (f64, f64)
```

(filled qty, filled value).

## `real_fee`

`src/engine/execution.rs:97`  (method / nested)

```rust
pub fn real_fee(&self) -> Option<f64>
```

The real fee once known (Some(0) when moomoo reports none: excluded).

## `same`

`src/engine/execution.rs:151`

```rust
fn same(a: f64, b: f64) -> bool
```

Whether two floats are equal within a relative 1e-9.

## `preview_market`

`src/engine/execution.rs:157`

```rust
fn preview_market(m: &Market, sc: &[Vec<Option<Score>>], next_ts: i64, next_wall: i64, mids: &HashMap<String, f64>) -> Result<(Market, Vec<Vec<Option<Score>>>)>
```

The market plus one candle at clock `next_ts` (wall `next_wall`) priced at each symbol's real quote mid (None without a quote), and scores extended with None. For the preview only.

## `new_id`

`src/engine/execution.rs:176`  (method / nested)

```rust
fn new_id(&mut self) -> u64
```

Next holding id.

## `holding`

`src/engine/execution.rs:182`  (method / nested)

```rust
pub fn holding(&self, id: u64) -> Option<&Holding>
```

The holding with id `id`.

## `open_holding`

`src/engine/execution.rs:187`  (method / nested)

```rust
fn open_holding(&self, symbol: &str, entry_ts: i64) -> Option<u64>
```

The open holding of engine position `p`.

## `preview`

`src/engine/execution.rs:193`  (method / nested)

```rust
pub fn preview(&self, pf: &XsPortfolio, m: &Market, sc: &[Vec<Option<Score>>], p: &XsParams, next_ts: i64, next_wall: i64, mids: &HashMap<String, f64>) -> Result<Vec<Planned>>
```

Orders that implement the engine's executions at the open `next_ts` (see the module doc).

## `owed_sells`

`src/engine/execution.rs:242`  (method / nested)

```rust
pub fn owed_sells(&self, due_ts: i64, can_sell: &HashMap<String, f64>) -> Vec<Planned>
```

Sells still owed at this open: shares of holdings the engine has closed (or never held) that moomoo still holds, with no live sell order. `can_sell`: moomoo's sellable quantity.

## `record`

`src/engine/execution.rs:280`  (method / nested)

```rust
pub fn record(&mut self, p: &Planned, placed_ms: i64, placed: Result<(u64, String), String>) -> u64
```

Record an order placed (or refused) for `p`.

## `update_state`

`src/engine/execution.rs:308`  (method / nested)

```rust
pub fn update_state(&mut self, s: &OrderState) -> bool
```

moomoo's latest state of an order.

## `update_fee`

`src/engine/execution.rs:319`  (method / nested)

```rust
pub fn update_fee(&mut self, f: &OrderFee) -> bool
```

Stores the real fee of the order it belongs to; true when it changed.

## `open_order_ids`

`src/engine/execution.rs:331`  (method / nested)

```rust
pub fn open_order_ids(&self) -> Vec<u64>
```

Orders moomoo may still change (ids).

## `fee_missing`

`src/engine/execution.rs:336`  (method / nested)

```rust
pub fn fee_missing(&self) -> Vec<String>
```

Filled orders whose fee is still unknown (ids_ex).

## `link_step`

`src/engine/execution.rs:347`  (method / nested)

```rust
pub fn link_step(&mut self, pf: &mut XsPortfolio, before: &[XsPosition], trades_before: usize, due_ts: Option<i64>) -> Vec<String>
```

Tie the engine's executions at `due_ts` (one `advance` of the paper account) to orders: `before` = positions before the advance, `trades_before` = trades before it. Divergences are turned into the standard shapes (see the module doc). Returns log lines.

## `unlinked`

`src/engine/execution.rs:478`  (method / nested)

```rust
fn unlinked(&self, i: usize) -> bool
```

True when order `i` is not tied to an engine execution.

## `attach`

`src/engine/execution.rs:490`  (method / nested)

```rust
fn attach(&mut self, hid: u64, symbol: &str, leg: LegKind, due_ts: i64)
```

Give the unlinked order for (`symbol`, `leg`, `due_ts`) to holding `hid`.

## `recompute`

`src/engine/execution.rs:508`  (method / nested)

```rust
pub fn recompute(&mut self, pf: &mut XsPortfolio) -> Vec<String>
```

Book every holding's real executions into the engine portfolio (see the module doc). Returns log lines for changes.

## `book_residual`

`src/engine/execution.rs:655`  (method / nested)

```rust
fn book_residual(&mut self, hi: usize, orders: &[LiveOrder], pf: &mut XsPortfolio) -> Option<String>
```

A residual holding is booked once all its shares are sold: a reconciliation trade with the real buy and sell prices and fees.

## `bot_shares`

`src/engine/execution.rs:707`  (method / nested)

```rust
pub fn bot_shares(&self) -> HashMap<String, f64>
```

Shares the engine's holdings account for at moomoo, per symbol (real filled buys minus sells of this bot's orders; simulated pending legs excluded).

## `compact`

`src/engine/execution.rs:719`  (method / nested)

```rust
pub fn compact(&mut self, keep: usize)
```

Drop settled holdings and their final orders older than the last `keep` (the store keeps the full history); trade indices of kept holdings stay valid.

# `src/engine/store.rs`

> The permanent SQLite store (user rule 2026-10-06: store everything, including candle data;
> only cull candle data once the database reaches 10 GB). It lives in the archive folder beside
> the runtime folder, which deploy resets never touch.
>
> Tables: `candles` (every fetched 15-minute traded candle), `rules` (each measurement),
> `accounts` and `broker_positions` (each moomoo paper account read), `universe` (each screen),
> `orders` (every real paper order state seen), `order_fees` (moomoo's fees per order), `quota`
> (each candle-quota read), `paper` (the engine paper account per bar), `paper_trades`, `events`
> (every engine decision, denied entry, gate, exclusion and reconciliation), `market` (per-bar
> market context), `searches`.

23 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `open`

`src/engine/store.rs:53`  (method / nested)

```rust
pub fn open(path: &Path) -> Result<Store>
```

Opens (or creates) the store with the default size limit.

## `open_with_limit`

`src/engine/store.rs:58`  (method / nested)

```rust
pub fn open_with_limit(path: &Path, limit: u64) -> Result<Store>
```

Opens (or creates) the store with size limit `limit` bytes; creates every table.

## `conn`

`src/engine/store.rs:100`  (method / nested)

```rust
fn conn(&self) -> std::sync::MutexGuard<'_, Connection>
```

The store's connection (locked).

## `size_bytes`

`src/engine/store.rs:105`  (method / nested)

```rust
pub fn size_bytes(&self) -> Result<u64>
```

Used bytes (allocated pages minus free pages).

## `enforce_limit`

`src/engine/store.rs:117`  (method / nested)

```rust
pub fn enforce_limit(&self) -> Result<usize>
```

Cull the oldest candles while the store is at or above its limit; rows removed.

Notes from the code: Note: The ONLY deletion in the store (user rule): at the 10 GB limit the oldest candle rows go, a slice at a time, until the store is below the limit again. Nothing else is ever deleted.

## `candles`

`src/engine/store.rs:138`  (method / nested)

```rust
pub fn candles(&self, symbol: &str, kind: &str, bars: &std::collections::BTreeMap<i64, Bar>, now: i64) -> Result<()>
```

Saves candles of `symbol` (replacing any with the same time).

## `rules`

`src/engine/store.rs:154`  (method / nested)

```rust
pub fn rules(&self, symbol: &str, r: &Rules, now: i64) -> Result<()>
```

Saves one rules measurement of `symbol`.

## `latest_rules`

`src/engine/store.rs:164`  (method / nested)

```rust
pub fn latest_rules(&self) -> Result<std::collections::HashMap<String, Rules>>
```

The latest stored rules measurement of every symbol (real past measurements, each with its own `book_ts`), for a restart to continue from.

## `account`

`src/engine/store.rs:179`  (method / nested)

```rust
pub fn account(&self, a: &super::moomoo::Account, held: &[super::moomoo::Held]) -> Result<()>
```

Saves one account read and the positions held at that moment.

## `universe`

`src/engine/store.rs:197`  (method / nested)

```rust
pub fn universe(&self, ts: i64, list: &[super::moomoo::Listing]) -> Result<()>
```

Saves the universe at `ts` with each listing's rank.

## `order`

`src/engine/store.rs:211`  (method / nested)

```rust
pub fn order(&self, ts: i64, order_id: u64, json: &str) -> Result<()>
```

One observed state of a real paper order (any JSON the service keeps for it).

## `order_fee`

`src/engine/store.rs:218`  (method / nested)

```rust
pub fn order_fee(&self, ts: i64, f: &super::moomoo::OrderFee, symbol: &str, side: i32, qty: f64, price: f64) -> Result<()>
```

moomoo's fee for one filled order (kept once per order).

## `fee_points`

`src/engine/store.rs:227`  (method / nested)

```rust
pub fn fee_points(&self) -> Result<Vec<(String, f64, f64)>>
```

Every real fee observation: (symbol, notional USD, total fee USD), oldest first.

## `quota`

`src/engine/store.rs:235`  (method / nested)

```rust
pub fn quota(&self, q: &super::moomoo::QuotaState) -> Result<()>
```

Saves one candle-quota read.

## `start_run`

`src/engine/store.rs:244`  (method / nested)

```rust
pub fn start_run(&self, run_id: i64, note: &str) -> Result<()>
```

Records the start of a service run.

## `paper`

`src/engine/store.rs:255`  (method / nested)

```rust
pub fn paper(&self, p: &PaperPoint, trades: &[Trade], wall: impl Fn(i64) -> Option<i64>, features: impl Fn(&Trade) -> serde_json::Value) -> Result<()>
```

One bar of the paper account plus its closed trades (`wall` maps engine clock times). `features` gives the research lab's entry-state blob for a trade; it is called only for trades not stored yet and kept in the trade's json (with `decided_wall_ms`, the deciding bar in wall time).

## `liquidity`

`src/engine/store.rs:294`  (method / nested)

```rust
pub fn liquidity(&self, symbol: &str) -> Option<(f64, f64)>
```

Average daily dollar turnover and daily volatility of `symbol` from its stored real candles (see `moomoo::liquidity_from_bars`).

## `events`

`src/engine/store.rs:306`  (method / nested)

```rust
pub fn events(&self, run_id: i64, events: &[EngineEvent], wall: impl Fn(i64) -> Option<i64>) -> Result<()>
```

Saves engine events of run `run_id` (clock times mapped to wall times).

## `market`

`src/engine/store.rs:320`  (method / nested)

```rust
pub fn market(&self, run_id: i64, wall_ms: i64, json: &str) -> Result<()>
```

Saves the market context of one bar.

## `search`

`src/engine/store.rs:329`  (method / nested)

```rust
pub fn search(&self, run_id: i64, finished_ms: i64, json: &str) -> Result<()>
```

Saves one finished search.

## `equity_curve_all`

`src/engine/store.rs:339`  (method / nested)

```rust
pub fn equity_curve_all(&self) -> Result<Vec<(i64, f64)>>
```

The paper account's equity over ALL runs (every service start is a run): the paper state carries over restarts, so its chart must too (it showed only the current run, which a restart resets).

## `trades_all`

`src/engine/store.rs:347`  (method / nested)

```rust
pub fn trades_all(&self) -> Result<Vec<TradeRow>>
```

Closed paper trades of ALL runs, oldest exit first.

## `candle_count`

`src/engine/store.rs:355`  (method / nested)

```rust
pub fn candle_count(&self) -> Result<i64>
```

Number of stored candles.

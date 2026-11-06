# `examples/common/mod.rs`

> Shared by the research examples: the stored candles, moomoo's session calendar and one interval's
> whole stored span as a market.

11 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `research_from`

`examples/common/mod.rs:18`

```rust
pub fn research_from() -> Result<chrono::NaiveDate>
```

The research history's first day: `RESEARCH_FROM`, or the later `RESEARCH_FROM` environment value (never before 2026-01-02).

## `store`

`examples/common/mod.rs:27`

```rust
pub fn store() -> Result<Connection>
```

Opens the bot's permanent store (candles, rules, paper state) read-only for a research tool.

## `sessions`

`examples/common/mod.rs:34`

```rust
pub async fn sessions() -> Result<Sessions>
```

moomoo's session calendar from the research start (cached per start date; calendar requests use no candle quota).

## `load_data`

`examples/common/mod.rs:49`

```rust
pub fn load_data() -> Result<Vec<SymbolData>>
```

Stored 15m candles and the latest stored rules of every symbol.

## `big_market`

`examples/common/mod.rs:129`

```rust
pub fn big_market(cal: &Sessions, data: &[SymbolData], interval: Interval) -> Result<Big>
```

Builds the research market for `interval` from all stored history (from SPY's first candle), dropping symbols whose data fails validation.

## `fmt`

`examples/common/mod.rs:154`

```rust
pub fn fmt(ms: i64) -> String
```

Formats a wall time (ms) as US/Eastern date and time.

## `liquidity_default`

`examples/common/mod.rs:161`

```rust
pub fn liquidity_default() -> bool
```

The research tools model what the bot will do: the real-volume liquidity model is ON unless STUDY_LIQUIDITY=0 (the old one-moment top-of-book snapshot capped every order at a few thousand USD). Call once at the start of an example's main.

## `apply_order_cap`

`examples/common/mod.rs:167`

```rust
pub fn apply_order_cap()
```

`STUDY_ORDER_CAP=<usd>`: ceiling on one entry order's notional (research only). Also lifts the live service's CPU governor for this research process, so its threads can use every core.

## `now_ms`

`examples/common/mod.rs:175`

```rust
pub fn now_ms() -> i64
```

Current wall time in milliseconds since the Unix epoch.

## `jitter`

`examples/common/mod.rs:181`

```rust
pub fn jitter(m: &Market, sigma: f64, seed: u64) -> Market
```

Execution/price noise for stress tests: every bar's open and close are multiplied by exp(sigma * z) (independent, no drift), highs and lows widened to still cover them. Traded and mark candles get the same factors. Deterministic in `seed`.

## `capital`

`examples/common/mod.rs:213`

```rust
pub fn capital() -> f64
```

The USD the bot trades with, as the live service sizes it: the paper account's latest AVAILABLE cash in the store (the research tools must answer for the same size the bot really trades, since market impact grows with order size). Falls back to START_CAPITAL when the store has no balance. Override with STUDY_EQUITY.

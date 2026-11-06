# `src/engine/mod.rs`

> The trading engine for US stocks on moomoo, on closed 15-minute regular-session bars (aggregated to
> supported live intervals aggregated from 15m bars): a cross-sectional mean-reversion engine on a trading-time clock (see `Market::ts`)
> with an SPY trend filter (`REFERENCE`). Data and execution come from moomoo OpenD only.
>
> * `moomoo` — real moomoo data through OpenD: trading sessions, closed traded 15m candles,
> the screener universe, quotes, the paper account, real paper orders and their fees.
> * `rules` — per-symbol rules: lot size, real paper fees, quoted spread cost.
> * `scores` — the screener, computed causally from closed bars.
> * `xs` — the strategy: cross-sectional ranking, long only, settings from
> `walkforward::search_live` (a random search), SPY trend filter and drawdown rules.
> * `walkforward` — the full-window random parameter search and the champion/challenger rule.
> * `metrics` — trade records and performance metrics.
> * `governor`, `supervisor` — CPU cap, task restarts.
>
> Decisions are made at a bar's close and filled at a later open (or at a level inside a
> later bar, adverse first): no lookahead, no repainting; the same engine drives backtests,
> the parameter search and paper trading.

16 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `runtime_dir`

`src/engine/mod.rs:41`

```rust
pub fn runtime_dir() -> std::path::PathBuf
```

Runtime data folder (bars, rules, paper account, logs). It lives outside ~/Documents: macOS privacy protection blocks launchd jobs there.

## `bars`

`src/engine/mod.rs:110`  (method / nested)

```rust
pub const fn bars(self) -> usize
```

15-minute bars per candle (a session's last candle may be shorter).

## `label`

`src/engine/mod.rs:128`  (method / nested)

```rust
pub const fn label(self) -> &'static str
```

Human label of the interval (e.g. "5h", "1 session").

## `key`

`src/engine/mod.rs:146`  (method / nested)

```rust
pub const fn key(self) -> &'static str
```

Short key (file names, API query `?i=`).

## `from_key`

`src/engine/mod.rs:164`  (method / nested)

```rust
pub fn from_key(k: &str) -> Option<Interval>
```

The interval whose key (e.g. "5h") is `k`.

## `test_sessions`

`src/engine/mod.rs:169`  (method / nested)

```rust
pub const fn test_sessions(self) -> usize
```

Sessions of the test window.

## `per_session`

`src/engine/mod.rs:177`  (method / nested)

```rust
pub const fn per_session(self) -> usize
```

Candles in a full regular session.

## `test_candles`

`src/engine/mod.rs:182`  (method / nested)

```rust
pub const fn test_candles(self) -> usize
```

Candles in the test window (15m: `BARS`).

## `warmup_candles`

`src/engine/mod.rs:188`  (method / nested)

```rust
pub const fn warmup_candles(self) -> usize
```

Candles before the test window that only warm the indicators up (none for 15m, whose warm-up stays inside the window as before).

## `day_scaled`

`src/engine/mod.rs:197`

```rust
pub const fn day_scaled(crypto_bars: usize) -> usize
```

A window of `crypto_bars` 15-minute bars of 24/7 time, in trading-time bars (96 = one trading day).

## `validate`

`src/engine/mod.rs:262`  (method / nested)

```rust
pub fn validate(&self) -> anyhow::Result<()>
```

Reject incomplete or malformed real data before evaluating a strategy.

## `validate_symbol`

`src/engine/mod.rs:290`  (method / nested)

```rust
pub fn validate_symbol(&self, s: usize) -> anyhow::Result<()>
```

One symbol's rows: complete traded and mark candles after listing, sane OHLC, ascending finite funding.

Notes from the code: Note: Per-symbol on purpose: the service lets an incomplete coin sit out a bar instead of failing the whole market.

## `rules`

`src/engine/mod.rs:338`  (method / nested)

```rust
pub fn rules(&self, sym: usize) -> Option<&rules::Rules>
```

The symbol's rules; None means the symbol must not be traded.

## `slice`

`src/engine/mod.rs:343`  (method / nested)

```rust
pub fn slice(&self, r: std::ops::Range<usize>) -> Market
```

The bars in `r` (all symbols, rules and listing data kept).

## `concat`

`src/engine/mod.rs:360`  (method / nested)

```rust
pub fn concat(parts: &[Market]) -> anyhow::Result<Market>
```

Join back-to-back markets into one continuous timeline (union of symbols; a symbol missing from one part has no bars there). Indicators computed on the result carry their history across the joins, as they do live.

## `sign`

`src/engine/mod.rs:442`  (method / nested)

```rust
pub fn sign(self) -> f64
```

+1 for long, -1 for short.

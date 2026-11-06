# `src/engine/scores.rs`

> Screener scores per symbol per closed 15m bar. Everything at bar t uses
> bars <= t only. Cross-sectional parts compare a symbol with the other
> symbols in the universe at the SAME bar.
>
> * Volatility: high-low range as % over the last 1, 2, 4, 16, DAY_BARS bars
> (15m, 30m, 1h, 4h, one trading day), each scaled by 1/sqrt(bars) so windows are
> comparable, averaged with weights 5:4:3:2:1 (recent intervals weigh more).
> * Price action (signed): distance of close from EMA 8/21/55 in %, weights
> 3:2:1 (recent pricing weighs more), divided by the 15m-equivalent volatility.
> * Volume: Z-score of the bar's volume vs the previous 20 bars.
> * Universe at t: the top `universe` symbols by rolling one-trading-day dollar volume at t.
> * volatility_score / volume_score / pa_strength: percentiles within the universe.
> * rank_value = mean of those three; rank = position (1 = highest).
> * price_action_score = percentile of the signed price action (0 bearish .. 100 bullish).
> * return_score = percentile of the one-trading-day return (0 biggest loser .. 100 biggest gainer).
> * trend_score = 50 + (price_action_score - 50) * rank_value/100, clamped to 1..100:
> strongly directional only when the pair is also highly ranked.

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `raw_features`

`src/engine/scores.rs:71`

```rust
fn raw_features(bars: &[Option<Bar>]) -> Vec<Option<Raw>>
```

Per-symbol raw features; None until WARMUP contiguous bars exist.

Notes from the code: Note: Strictly causal: bar t uses bars <= t only, and a gap restarts the warm-up. Tests check future-bar invariance; keep them passing for any new feature.

## `bbw`

`src/engine/scores.rs:150`

```rust
fn bbw(closes: &[f64]) -> Option<f64>
```

Bollinger band width over `closes`: 4 * population sd / mean.

## `sigmoid100`

`src/engine/scores.rs:158`

```rust
fn sigmoid100(x: f64) -> f64
```

Logistic function scaled to 0..100.

## `pulse_scores`

`src/engine/scores.rs:163`

```rust
fn pulse_scores( bars: &[Option<Bar>], t: usize, run_start: usize, vol_z: f64, ) -> Option<(f64, f64)>
```

(long, short) Pulse scores at bar t using bars <= t only.

## `percentile`

`src/engine/scores.rs:202`

```rust
fn percentile(sorted: &[f64], x: f64) -> f64
```

Percentile (0-100) of `x` within `all` (share of values <= x).

## `compute`

`src/engine/scores.rs:213`

```rust
pub fn compute(m: &Market, universe: usize) -> Vec<Vec<Option<Score>>>
```

Scores `[symbol][bar]`, None outside the universe or during warmup. Only symbols with moomoo rules (tradeable now) can join the universe.

## `compute_with`

`src/engine/scores.rs:222`

```rust
pub fn compute_with(m: &Market, universe: usize, tradeable_only: bool) -> Vec<Vec<Option<Score>>>
```

`compute`, optionally ranking every symbol that traded at the time whether or not it has rules today: research on the universe as it was then, free of today's survivors (`examples/signal_ic.rs`).

Notes from the code: Note: tradeable_only = false is for research only (signal_ic on broad historical data). The live bot must use `compute`, which only ranks symbols with moomoo rules.

## `load`

`src/engine/scores.rs:328`  (method / nested)

```rust
fn load() -> HashMap<String, Vec<(i64, f32)>>
```

_No comment in the code; the signature above is the description._

## `lookup`

`src/engine/scores.rs:340`  (method / nested)

```rust
pub fn lookup(symbol: &str, wall_ms: i64) -> Option<f64>
```

The score of `symbol` for the bar that opened at `wall_ms`, if the research switch is on and the file has it.

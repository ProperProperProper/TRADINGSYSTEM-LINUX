# `src/engine/walkforward.rs`

> The parameter search over closed 15m regular-session bars of US stocks and ETFs, plus the
> strategy grids. Day-long windows are mapped to
> trading time (one trading day = DAY_BARS) and leverage fixed at 1x.
>
> User rule (2026-10-03): ONE window (the latest 14 sessions, BARS bars), never split into in-sample and out-of-sample parts. `search_live` backtests 200,000
> random combinations over the whole window; usable settings pass the two safety rules
> (no liquidation, drawdown <= 25%) with at least `MIN_TRADES` trades, and the
> best by `objective` challenges the settings paper is using (`choose_live`).
> The honest check without splitting a window is chronological: choose on one
> 14-day window, trade the next, never-seen one (dashboard chart, research).

37 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `objective`

`src/engine/walkforward.rs:30`

```rust
fn objective(m: &Metrics) -> f64
```

Search objective: return % minus half the max drawdown %.

## `signal_variants`

`src/engine/walkforward.rs:429`

```rust
fn signal_variants() -> Vec<(Signal, usize, bool)>
```

(signal, lookback, flip) for every signal: unflipped buys the lowest values, flipped the highest; `Return` at each of its lookbacks. 18 variants.

## `lookbacks`

`src/engine/walkforward.rs:455`

```rust
fn lookbacks(signal: Signal) -> &'static [usize]
```

Lookbacks searched: only `Return` uses one.

## `pick`

`src/engine/walkforward.rs:489`

```rust
pub fn pick<P: Clone + PartialEq>( challenger: Option<(P, f64)>, champion: Option<(P, f64)>, ) -> (Option<P>, Choice)
```

Pick between the challenger and the champion by score; `None` scores are unusable. A tie, or a lead within `SWITCH_MARGIN`, keeps the champion.

## `usable`

`src/engine/walkforward.rs:510`

```rust
fn usable(r: &Metrics) -> bool
```

Notes from the code: Note: User rule (2026-10-03): the live search uses ONE 14-day window, never split into in-sample/out-of-sample parts. Usable = the two safety rules (no liquidation, drawdown <= MAX_DRAWDOWN_PCT) plus at least MIN_TRADES trades and no execution error, all over the full 14 days.

## `usable_with`

`src/engine/walkforward.rs:515`

```rust
fn usable_with(r: &Metrics, min_trades: usize) -> bool
```

`usable` with a trade minimum of `min_trades` (research: `SearchSpace::min_trades`).

## `live_score`

`src/engine/walkforward.rs:524`

```rust
pub fn live_score( m: &Market, scores: &[Vec<Option<Score>>], equity: f64, p: &XsParams, range: std::ops::Range<usize>, ) -> Option<f64>
```

Score of `p` for live use: its objective over the full 14 days if usable there (`usable`). `None` = not usable now.

## `next`

`src/engine/walkforward.rs:594`  (method / nested)

```rust
fn next(&mut self) -> f64
```

Next uniform random number in [0, 1) (splitmix64).

## `range`

`src/engine/walkforward.rs:603`  (method / nested)

```rust
fn range(&mut self, lo: f64, hi: f64) -> f64
```

Uniform random number in [lo, hi).

## `chance`

`src/engine/walkforward.rs:607`  (method / nested)

```rust
fn chance(&mut self, p: f64) -> bool
```

True with probability `p`.

## `pct`

`src/engine/walkforward.rs:611`  (method / nested)

```rust
fn pct(&mut self, p_off: f64, lo: f64, hi: f64) -> Option<f64>
```

Optional percent: off with probability `p_off`, else uniform in [lo, hi], 0.1 resolution.

## `new`

`src/engine/walkforward.rs:645`  (method / nested)

```rust
pub fn new(m: &Market) -> Result<MarginBand>
```

Trend-margin band from the reference's own distance above its average (live percentile band).

## `with_pctl`

`src/engine/walkforward.rs:650`  (method / nested)

```rust
pub fn with_pctl(m: &Market, pctl: (f64, f64)) -> Result<MarginBand>
```

As `new`, drawing from the percentile band `pctl` (research).

## `margin`

`src/engine/walkforward.rs:678`  (method / nested)

```rust
pub fn margin(&self, bars: usize, q: f64) -> f64
```

The `q` percentile (0-100) of the distance above the `bars`-bar average (the nearest measured window when `bars` is longer than the history allows).

## `per_candle`

`src/engine/walkforward.rs:763`

```rust
pub const fn per_candle(bars: usize, k: usize) -> usize
```

`bars` 15-minute bars of trading time in candles of `k` bars (rounded, at least one).

## `space_for`

`src/engine/walkforward.rs:779`

```rust
pub const fn space_for(interval: super::Interval) -> SearchSpace
```

The live search space on candles of `interval`.

Notes from the code: Note: Candle intervals (user request 2026-10-06). The search ranges that are TRADING TIME by design (hold 1-2 sessions, trend window 0.5-2.5 sessions, drawdown cooldown) keep that time on longer candles; in raw bar counts a one-session candle would hold 26-52 sessions, longer than its whole test window, so nothing could ever pass the trade minimum. Indicator windows inside the engine count candles, like any chart indicator. 15m: exactly LIVE_SPACE.

## `apply_gates`

`src/engine/walkforward.rs:846`

```rust
fn apply_gates(p: &mut XsParams, rng: &mut Rng, space: &SearchSpace)
```

Switch the space's optional gates on for one draw (draws random values only for the gates that are on, so the live stream is unchanged).

## `random_params_space`

`src/engine/walkforward.rs:956`

```rust
fn random_params_space(rng: &mut Rng, band: &MarginBand, space: &SearchSpace) -> XsParams
```

One random combination drawn from `space` (the draw `search_random` makes).

## `random_params_s`

`src/engine/walkforward.rs:976`

```rust
fn random_params_s(rng: &mut Rng, band: &MarginBand, space: &SearchSpace) -> XsParams
```

One random combination of `space`'s ranges and exit variant (see `ExitVariant`).

## `random_params`

`src/engine/walkforward.rs:996`

```rust
fn random_params(rng: &mut Rng, band: &MarginBand) -> XsParams
```

One random long-only combination (`band`: the margin percentile band, see `MarginBand`).

## `random_params_h`

`src/engine/walkforward.rs:1001`

```rust
fn random_params_h(rng: &mut Rng, band: &MarginBand, space: &SearchSpace) -> XsParams
```

As `random_params`, drawing from `space`'s hold, basket and trend ranges.

## `sample_params`

`src/engine/walkforward.rs:1085`

```rust
pub fn sample_params(seed: u64, n: usize, band: &MarginBand) -> Vec<XsParams>
```

`n` random long-only combinations from the live search space (for research tools).

## `sample_space`

`src/engine/walkforward.rs:1092`

```rust
pub fn sample_space(seed: u64, n: usize, band: &MarginBand, space: SearchSpace) -> Vec<XsParams>
```

The first `n` random combinations `search_random` draws for `space` with `seed` (the same draw, in the same order; research tools). Does not run the search.

## `perturb`

`src/engine/walkforward.rs:1099`

```rust
fn perturb(p: &XsParams, rng: &mut Rng, band: &MarginBand) -> XsParams
```

A nearby combination: every numeric value scaled by a random factor within +/-`PERTURB`; signal, direction, on/off switches and which rules are enabled stay as they are.

## `percentile`

`src/engine/walkforward.rs:1163`

```rust
fn percentile(sorted: &[f64], q: f64) -> f64
```

Value at quantile `q` of a sorted list (0 when empty).

## `excursions`

`src/engine/walkforward.rs:1172`

```rust
fn excursions(m: &Market, trades: &[Trade]) -> Vec<(f64, f64, f64)>
```

(maximum adverse %, maximum favourable %, final return %) of each long trade, from the traded candles between entry and exit. Trades whose symbol or bars are missing are skipped.

## `candidates`

`src/engine/walkforward.rs:1203`

```rust
fn candidates(sorted: &[f64], qs: &[f64], scale: f64, lo: f64, hi: f64) -> Vec<Option<f64>>
```

Distinct, ascending candidate levels (0.1% resolution) from the given percentiles of a sorted sample, scaled by `scale` and clamped to `[lo, hi]`; `None` (off) always first.

## `analyse_exits`

`src/engine/walkforward.rs:1228`

```rust
fn analyse_exits( m: &Market, scores: &[Vec<Option<Score>>], equity: f64, p: &XsParams, range: std::ops::Range<usize>, deadline: Instant, ) -> Result<(XsParams, ExitAnalysis)>
```

`p` with data-derived stop and take-profit, and the analysis behind them.

## `best_cell`

`src/engine/walkforward.rs:1299`

```rust
fn best_cell(grid: &[Vec<Option<f64>>]) -> Option<(usize, usize)>
```

The usable cell with the best 3x3 mean (itself and up to 8 neighbours; an unusable neighbour counts as the worst usable cell). `None` when nothing is usable.

## `search_live`

`src/engine/walkforward.rs:1340`

```rust
pub fn search_live( m: &Market, scores: &[Vec<Option<Score>>], equity: f64, deadline: Instant, seed: u64, recorded: &[XsParams], ) -> Result<SearchReport>
```

The live search: `LIVE_SAMPLES` random combinations over the one 14-day window, then the best `FINALISTS` are re-tested under `PERTURBATIONS` random perturbations each and the one with the best MEAN objective wins (an unusable perturbation counts as the worst usable sampled score). A lone spike whose neighbours fail loses to settings that hold up nearby. Still in-sample: this reduces selection noise, it does not prove an edge.

## `search_random`

`src/engine/walkforward.rs:1355`

```rust
pub fn search_random( m: &Market, scores: &[Vec<Option<Score>>], equity: f64, deadline: Instant, seed: u64, recorded: &[XsParams], space: SearchSpace, ) -> Result<SearchReport>
```

Random search: draws `space.samples` settings, backtests each, ranks usable ones by robust score; recorded settings are re-tested too.

## `buy_hold_pct`

`src/engine/walkforward.rs:1543`

```rust
fn buy_hold_pct(m: &Market, range: std::ops::Range<usize>) -> f64
```

Average buy-and-hold return (%) of all symbols over `range`.

## `evaluate_window`

`src/engine/walkforward.rs:1558`

```rust
pub fn evaluate_window( label: &str, m: &Market, scores: &[Vec<Option<Score>>], equity: f64, p: &XsParams, range: std::ops::Range<usize>, ) -> WindowResult
```

Run `p` over `range` of `m` with the production engine and costs.

## `passes`

`src/engine/walkforward.rs:1601`

```rust
fn passes(w: &WindowResult) -> bool
```

Whether a window result passes: positive return, no liquidation, drawdown within the limit.

## `verdict`

`src/engine/walkforward.rs:1606`

```rust
pub fn verdict(c: &Candidate) -> Verdict
```

The walk-forward verdict for a candidate (rule stated above; never an automatic trade).

## `choose_live`

`src/engine/walkforward.rs:1638`

```rust
pub fn choose_live( m: &Market, scores: &[Vec<Option<Score>>], equity: f64, challenger: Option<&XsParams>, champion: Option<&XsParams>, range: std::ops::Range<usize>, ) -> (Option<XsParams>, Choice)
```

The settings paper trades this bar: the search's best (`challenger`) or the settings in use (`champion`), each re-scored on the current market.

## `keep_recent_best`

`src/engine/walkforward.rs:1653`

```rust
pub fn keep_recent_best( picked: (Option<XsParams>, Choice), champion: Option<&XsParams>, ) -> (Option<XsParams>, Choice)
```

User rule (2026-10-04): when no settings are usable, still use the recent best ones.

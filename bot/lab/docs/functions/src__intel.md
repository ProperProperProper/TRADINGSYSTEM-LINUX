# `src/intel.rs`

> Trading intelligence beyond the ranking (owner 2026-10-13: "I want it to be super trading intelligence"): the pieces a trader needs around a stock ranking.
> * a RISK model: forecasts SPY's volatility over the next session (a HAR-style ridge on log realised volatilities) and says whether it beats the naive "tomorrow
> looks like today" forecast, on rolling out-of-sample windows;
> * a DIRECTION model: probability that SPY rises over the next session, with an honest out-of-sample AUC interval (it is expected to show NO skill; the report says so);
> * a DECISION engine (`decide`): regime, stance, which names are worth trading AFTER the bot's measured costs, position sizes scaled by risk, and a confidence level taken
> from the model's own live record. It abstains when the edge is below costs or the model's recent record is significantly bad.
>
> Advisory only: nothing here places an order or touches the live strategy.

11 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `rv`

`src/intel.rs:25`

```rust
fn rv(r: &[f64]) -> f64
```

_No comment in the code; the signature above is the description._

## `observations`

`src/intel.rs:28`

```rust
pub fn observations(spy: &[Bar], live: bool) -> Vec<Obs>
```

SPY's state at every hourly bar with the outcomes of the next `H` bars (training / evaluation).

## `rolling_vol`

`src/intel.rs:52`

```rust
fn rolling_vol(obs: &[Obs]) -> Option<(Vec<f64>, Vec<f64>, Vec<f64>, Vec<i64>, Vec<i64>)>
```

The rolling-origin forecasts behind [`test_volatility`]: (model, naive, realised, week, time) per test observation, all in log volatility; None when too few.

## `test_volatility`

`src/intel.rs:77`

```rust
pub fn test_volatility(obs: &[Obs]) -> Option<RiskReport>
```

Rolling-origin test of the volatility forecast against the naive forecast (next volatility = today's, scaled by the training mean ratio). Gain = reduction in squared error, interval by resampling whole weeks.

## `volatility_track`

`src/intel.rs:89`

```rust
pub fn volatility_track(obs: &[Obs]) -> Vec<serde_json::Value>
```

SPY volatility (percent over the next session): the model's forecast, the naive forecast and what was realised, averaged per day, out of sample (the chart on the dashboard).

## `test_direction`

`src/intel.rs:100`

```rust
pub fn test_direction(obs: &[Obs]) -> Option<DirectionReport>
```

Rolling-origin test of "will SPY be higher after the next session": AUC with a week-resampled interval. No skill = an interval around 0.5.

## `forecast_now`

`src/intel.rs:129`

```rust
pub fn forecast_now(obs_labelled: &[Obs], live: &Obs) -> Option<(f64, f64)>
```

A volatility forecast for the newest bar (log realised volatility over the next session, as a percentile of the forecasts made over the history).

## `round_trip_bps`

`src/intel.rs:150`

```rust
pub fn round_trip_bps(costs: &Costs, symbol: &str, size: f64) -> f64
```

Round-trip cost of one position in bps: two commissions over the position size plus two half-spreads.

## `assess`

`src/intel.rs:157`

```rust
pub fn assess(picks: &[Pick], costs: &Costs, account: f64, positions: usize, spy_trend_up: bool, vol_pct: Option<f64>, record: &Scoreboard, recent_ic: Option<(f64, f64, usize)>, scale: &BTreeMap<String, f64>) -> Assessment
```

The bot holds `positions` positions at a time and ANY symbol whose signal fires first takes a slot (owner), so there is no portfolio to pick. For every stock the model ranks, this answers: if its signal fires, take it or skip it? Take = the market regime allows entries, the model's recent record is not significantly bad, and the calibrated expected return beats the stock's measured round-trip cost at the position size. Size = a slot (account / positions), halved in a high-volatility regime and scaled down for stocks whose own tail is wilder (`scale`, from `portfolio::slot_scale`). Trust comes from the model's live record.

## `briefing`

`src/intel.rs:180`

```rust
pub fn briefing(root: &std::path::Path, db: &rusqlite::Connection, series: &BTreeMap<String, Vec<Bar>>) -> anyhow::Result<String>
```

The full trading briefing for the newest bar: regime, risk forecast, the model's ranking after costs, the decision and the confidence, as plain text. Also runs and records the two honest tests (volatility forecast and direction) in the trial ledger.

## `gate_on_trades`

`src/intel.rs:230`

```rust
pub fn gate_on_trades(db: &rusqlite::Connection, data: &rusqlite::Connection) -> anyhow::Result<String>
```

The model as a filter on the bot's REAL entries: every backtest entry (the unselected runs) is looked up in the walk-forward score file at the bar before its decision (a score from a model fitted on older data), given its percentile among the stocks scored at that bar, and the trades the model would have skipped (percentile below 20 / 35 / 50) are compared with all trades by block-resampled interval. This is the question the bot's rule poses: a signal fired, does the model say take it?

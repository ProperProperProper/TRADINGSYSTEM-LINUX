# `src/mlcandle2.rs`

> Version 2 of the candle machine learning ("as advanced as possible", owner 2026-10-13). Version 1 (`mlcandle.rs`) stays untouched: its ridge model is the one
> FROZEN on 2026-10-13 for the forward test. Everything here is judged the same strict way (rolling windows, every prediction out of sample, whole-week
> resampling) and ALL variants are reported, not only the best, because every extra variant is another chance to look good by luck.
>
> Added over v1:
> * 20 more features: cross-sectional ranks of more signals, returns relative to the market, volatility-scaled returns, return since the day's open;
> * rank-normalised labels for training (robust to outliers; scoring always uses the raw excess return);
> * ridge strength chosen by validation on the last quarter of the TRAINING period only;
> * trees on the rank label with early stopping, and an ensemble (per-moment standardised average of ridge and trees);
> * a cost-aware long-only portfolio test, which is how the bot would actually use a ranking: each day at 10:00 New York, buy the top names, hold one session.

12 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `idx`

`src/mlcandle2.rs:20`

```rust
fn idx(name: &str) -> usize
```

_No comment in the code; the signature above is the description._

## `since_open`

`src/mlcandle2.rs:23`

```rust
fn since_open(series: &BTreeMap<String, Vec<Bar>>) -> BTreeMap<(usize, i64), f64>
```

Return since the day's first bar for the sample's decision bar, per (symbol, ts). The v1 samples do not carry the symbol, so it is rebuilt from the bars.

## `extend`

`src/mlcandle2.rs:38`

```rust
pub fn extend(series: &BTreeMap<String, Vec<Bar>>, base: &[Sample]) -> Vec<Sample>
```

Version-1 samples (23 features) plus the 20 extra ones. `series` must be the same map the samples were built from. Samples of one moment are consecutive in `build`'s output, in symbol order of the moment; the symbol is recovered by matching the moment's cross-section.

## `rank_labels`

`src/mlcandle2.rs:79`

```rust
pub fn rank_labels(s: &[Sample]) -> Vec<Sample>
```

Cross-sectional rank of the raw excess return within each moment, centred (-0.5..0.5): the training label of the rank models.

## `zscore_by_moment`

`src/mlcandle2.rs:90`

```rust
fn zscore_by_moment(s: &[Sample], p: &[f64]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `ridge_tuned`

`src/mlcandle2.rs:103`

```rust
pub fn ridge_tuned(train: &[Sample]) -> Ridge
```

Ridge with its strength chosen on the last quarter of the training period (never on the test windows).

## `trees_on_rank`

`src/mlcandle2.rs:117`

```rust
pub fn trees_on_rank(train: &[Sample]) -> Gbdt
```

_No comment in the code; the signature above is the description._

## `variants`

`src/mlcandle2.rs:127`

```rust
pub fn variants() -> Vec<Variant>
```

_No comment in the code; the signature above is the description._

## `rolled`

`src/mlcandle2.rs:147`

```rust
pub fn rolled(all: &[Sample], v: &Variant) -> (Vec<Sample>, Vec<f64>)
```

Rolling-origin predictions of a variant (4-week windows after the first 100 days, 3-day purge).

## `portfolio`

`src/mlcandle2.rs:169`

```rust
pub fn portfolio(test: &[Sample], pred: &[f64], k: usize, cost_bps: f64) -> Portfolio
```

Long-only portfolio test: once a day at 10:00 New York (decision bar 2 of the session), hold the `k` best-ranked names for one session. Excess return over the equal-weight average of all names, minus `cost_bps` per side (full turnover assumed, so 2 x cost per day). Interval by resampling whole weeks.

## `fmt_eval`

`src/mlcandle2.rs:194`

```rust
pub fn fmt_eval(name: &str, e: &Eval) -> String
```

_No comment in the code; the signature above is the description._

## `report`

`src/mlcandle2.rs:197`

```rust
pub fn report(series: &BTreeMap<String, Vec<Bar>>) -> String
```

The whole v2 experiment on the candle store: every variant on rolling windows, a shuffled-label control for the best one, and the cost-aware portfolio.

# `lab/src/mlcandle.rs`

> Machine learning on the market's own candles (owner 2026-10-13: "there is plenty of candle data to be testing against and learning from").
>
> The question: from what the candles show at a given moment, can a model rank WHICH stocks do better than the others over the next trading day?
> That is what an entry filter needs, and the data is large: 95 symbols x 15-minute bars from 2026-01-02 (about 470,000 candles), independent of the
> strategy's own trades.
>
> Design (every choice is there to prevent fooling ourselves):
> * features use only bars up to and including the decision bar (checked by a test that rewrites the future);
> * label = log return over the next `H` bars (one session) MINUS the universe average at the same moment, so the market's direction cancels and only
> the ranking of stocks remains; a moment needs at least `MIN_CROSS` symbols;
> * split by TIME with a purge gap of 3 days between train and test (labels look a day ahead, so neighbours would otherwise share information);
> * the score is the rank information coefficient per moment (Spearman between prediction and realised excess return across symbols) and the spread
> between the top and bottom fifth, with intervals from resampling whole WEEKS (neighbouring hours overlap);
> * control: models trained on excess returns shuffled WITHIN each moment must score about zero;
> * simple known signals (reversal, momentum) are scored too, so we see whether learning adds anything beyond them.
>
> Models are the self-written ones in `mltrade.rs`. Nothing here touches the live strategy.

23 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `days_from_civil`

`lab/src/mlcandle.rs:38`

```rust
fn days_from_civil(y: i64, m: i64, d: i64) -> i64
```

_No comment in the code; the signature above is the description._

## `utc_ms`

`lab/src/mlcandle.rs:44`

```rust
fn utc_ms(y: i64, m: i64, d: i64, h: i64) -> i64
```

_No comment in the code; the signature above is the description._

## `et_offset_ms`

`lab/src/mlcandle.rs:47`

```rust
pub fn et_offset_ms(ts: i64) -> i64
```

New York's offset from UTC in milliseconds for 2026 (daylight time from 8 March 07:00 UTC to 1 November 06:00 UTC).

## `mean`

`lab/src/mlcandle.rs:49`

```rust
fn mean(v: &[f64]) -> f64
```

_No comment in the code; the signature above is the description._

## `std`

`lab/src/mlcandle.rs:50`

```rust
fn std(v: &[f64]) -> f64
```

_No comment in the code; the signature above is the description._

## `stock_features`

`lab/src/mlcandle.rs:53`

```rust
fn stock_features(bars: &[Bar], r: &[f64], i: usize, bar_in_day: f64, weekday: f64) -> Option<[f64; 16]>
```

One symbol's stock-level features at bar `i` (needs `i >= WARMUP`), or None. Uses bars 0..=i only.

## `build`

`lab/src/mlcandle.rs:70`

```rust
pub fn build(series: &BTreeMap<String, Vec<Bar>>, step: i64) -> Vec<Sample>
```

All samples (one per symbol per grid moment) with cross-sectional features and the excess forward return. `step` keeps every `step`-th 15-minute grid point (4 = hourly). No information from after the decision bar enters any feature.

## `build_h`

`lab/src/mlcandle.rs:73`

```rust
pub fn build_h(series: &BTreeMap<String, Vec<Bar>>, step: i64, h: usize) -> Vec<Sample>
```

`build` with a label horizon of `h` bars (the features are unchanged).

## `build_live`

`lab/src/mlcandle.rs:77`

```rust
pub fn build_live(series: &BTreeMap<String, Vec<Bar>>) -> Vec<Sample>
```

The features of the NEWEST bars, whose labels are not known yet (excess and raw are NaN): what the model needs to rank the stocks right now. Every 15-minute bar of the last two hours is included; callers keep the newest moment.

## `build_core`

`lab/src/mlcandle.rs:79`

```rust
fn build_core(series: &BTreeMap<String, Vec<Bar>>, step: i64, h: usize, live: bool) -> Vec<Sample>
```

_No comment in the code; the signature above is the description._

## `split`

`lab/src/mlcandle.rs:132`

```rust
pub fn split(samples: &[Sample], test_share: f64, purge_days: i64) -> (Vec<Sample>, Vec<Sample>)
```

(train, test) by time. Train keeps samples at least `purge_days` before the cut so no training label window overlaps the test period.

## `to_trades`

`lab/src/mlcandle.rs:138`

```rust
pub fn to_trades(s: &[Sample]) -> Vec<Trade>
```

_No comment in the code; the signature above is the description._

## `shuffle_within_moment`

`lab/src/mlcandle.rs:141`

```rust
pub fn shuffle_within_moment(s: &[Sample], seed: u64) -> Vec<Sample>
```

Shuffles the excess returns WITHIN each moment (the control: it keeps every moment's cross-section but destroys any link to the features).

## `evaluate`

`lab/src/mlcandle.rs:156`

```rust
pub fn evaluate(test: &[Sample], pred: &[f64], seed: u64) -> Eval
```

Rank information coefficient and top-minus-bottom-fifth spread of `pred` against the realised excess returns, per moment, with week-resampled intervals.

## `simple_signal`

`lab/src/mlcandle.rs:187`

```rust
pub fn simple_signal(test: &[Sample], feature: &str, sign: f64) -> Vec<f64>
```

A fixed, simple signal (no learning), for comparison: index into `FEATURES` and a sign.

## `fit`

`lab/src/mlcandle.rs:193`  (method / nested)

```rust
pub fn fit(train: &[Sample], alpha: f64) -> Ridge
```

_No comment in the code; the signature above is the description._

## `fit_w`

`lab/src/mlcandle.rs:196`  (method / nested)

```rust
pub fn fit_w(train: &[Sample], alpha: f64, weight: &dyn Fn(&Sample) -> f64) -> Ridge
```

Weighted ridge: sample `s` counts `weight(s)` times (recency weighting for an edge that may fade). The ridge strength is relative to the mean weight.

## `predict`

`lab/src/mlcandle.rs:217`  (method / nested)

```rust
pub fn predict(&self, s: &Sample) -> f64
```

_No comment in the code; the signature above is the description._

## `weights`

`lab/src/mlcandle.rs:218`  (method / nested)

```rust
pub fn weights(&self) -> &[f64]
```

_No comment in the code; the signature above is the description._

## `contributions`

`lab/src/mlcandle.rs:220`  (method / nested)

```rust
pub fn contributions(&self, s: &Sample) -> Vec<f64>
```

What each feature adds to the prediction for `s` (weight x standardised value), in the order of the features: the model's own explanation.

## `rolling`

`lab/src/mlcandle.rs:225`

```rust
pub fn rolling<F: Fn(&[Sample]) -> Box<dyn Fn(&Sample) -> f64>>(all: &[Sample], first_days: i64, window_days: i64, purge_days: i64, fit_fn: F) -> (Vec<Sample>, Vec<f64>)
```

Rolling-origin test: for each 4-week window after the first `first_days` days, fit on everything at least `purge` days older and predict the window. Returns the pooled predictions aligned with the returned samples (all windows, all genuinely out of sample).

## `fit`

`lab/src/mlcandle.rs:242`

```rust
pub fn fit(train: &[Sample]) -> Trained
```

Fits the return-trees (early stopped on the last quarter of the training period) and a logistic baseline.

## `report`

`lab/src/mlcandle.rs:253`

```rust
pub fn report(series: &BTreeMap<String, Vec<Bar>>, costs: &crate::candles::Costs) -> Verdict
```

_No comment in the code; the signature above is the description._

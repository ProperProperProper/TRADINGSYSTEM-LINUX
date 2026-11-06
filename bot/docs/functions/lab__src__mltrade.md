# `lab/src/mltrade.rs`

> Machine learning on the bot's real trades (owner 2026-10-13: "use machine learning with the self-created model, for trading").
>
> Question: do the conditions at the moment of entry (stock and market state, 19 features recorded for every backtest trade) predict whether the
> trade makes money -- beyond what chance gives? Everything is self-written Rust (no ML library): a logistic regression baseline and gradient-boosted
> decision trees (second-order boosting, histogram splits). The test follows the lab's own discipline:
> * data: long trades of the unselected backtest runs (`random` and `oos` roles), 2026 blocks only;
> * split by TIME: the earlier two thirds of the 14-session blocks train, the later third is judged once and never used for fitting or choices;
> * many runs replay the same blocks with different settings, so trades are NOT independent: uncertainty comes from resampling whole blocks;
> * a control model trained on shuffled labels must score about 0.5, or the pipeline leaks.
>
> This module only measures skill. A model with real, block-consistent skill could become a research-only entry gate that the lab then validates like
> any other option; nothing here touches the live strategy.

21 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `load`

`lab/src/mltrade.rs:27`

```rust
pub fn load(data: &Connection) -> Result<Vec<Trade>>
```

Long trades of the unselected runs with their entry features, oldest block first.

## `split_by_block`

`lab/src/mltrade.rs:42`

```rust
pub fn split_by_block(trades: &[Trade], test_share: f64) -> (Vec<Trade>, Vec<Trade>)
```

Splits trades by block time: (train, test) where the test is the latest `test_share` of the distinct blocks.

## `medians`

`lab/src/mltrade.rs:50`

```rust
pub fn medians(train: &[Trade]) -> Vec<f64>
```

Median of the non-missing values per feature, for filling gaps.

## `filled`

`lab/src/mltrade.rs:58`

```rust
fn filled(t: &Trade, med: &[f64]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `auc`

`lab/src/mltrade.rs:62`

```rust
pub fn auc(score: &[f64], label: &[bool]) -> f64
```

Area under the ROC curve (Mann-Whitney, average ranks for ties).

Notes from the code: ------------------------------------------------------------------ metrics

## `fit`

`lab/src/mltrade.rs:82`  (method / nested)

```rust
pub fn fit(train: &[Trade], epochs: usize, lr: f64, l2: f64) -> Logistic
```

_No comment in the code; the signature above is the description._

## `predict`

`lab/src/mltrade.rs:104`  (method / nested)

```rust
pub fn predict(&self, t: &Trade) -> f64
```

_No comment in the code; the signature above is the description._

## `bin_of`

`lab/src/mltrade.rs:121`

```rust
fn bin_of(cuts: &[f64], x: f64) -> usize
```

_No comment in the code; the signature above is the description._

## `build`

`lab/src/mltrade.rs:124`

```rust
fn build(binned: &[Vec<u8>], g: &[f64], h: &[f64], idx: &[usize], depth: usize, p: &GbdtParams, nbins: &[usize]) -> Node
```

_No comment in the code; the signature above is the description._

## `walk`

`lab/src/mltrade.rs:147`

```rust
fn walk(n: &Node, bins: &[usize]) -> f64
```

_No comment in the code; the signature above is the description._

## `fit`

`lab/src/mltrade.rs:152`  (method / nested)

```rust
pub fn fit(train: &[Trade], p: GbdtParams) -> Gbdt
```

_No comment in the code; the signature above is the description._

## `fit_with`

`lab/src/mltrade.rs:155`  (method / nested)

```rust
pub fn fit_with(train: &[Trade], p: GbdtParams, squared: bool) -> Gbdt
```

`squared = true` learns the (winsorised) return itself instead of win/lose, so the size of gains and losses counts.

## `predict`

`lab/src/mltrade.rs:187`  (method / nested)

```rust
pub fn predict(&self, t: &Trade) -> f64
```

_No comment in the code; the signature above is the description._

## `predict_rounds`

`lab/src/mltrade.rs:190`  (method / nested)

```rust
pub fn predict_rounds(&self, t: &Trade, rounds: usize) -> f64
```

Prediction using only the first `rounds` boosting rounds (for choosing how long to train).

## `fit_early_stopped`

`lab/src/mltrade.rs:199`  (method / nested)

```rust
pub fn fit_early_stopped(train: &[Trade], p: GbdtParams, squared: bool) -> (Gbdt, usize)
```

Early stopping: trains on the earlier 75% of the training blocks, measures the loss on the last 25% (still the TRAINING period, never the test blocks) after 0, 5, 10, 25, 50, 100 and 150 rounds, picks the best, and refits on all the training data with that many rounds. Returns (model, rounds).

## `judge`

`lab/src/mltrade.rs:226`

```rust
pub fn judge(name: &str, test: &[Trade], score: &[f64], threshold: f64, seed: u64) -> Verdict
```

Judges one set of test predictions: AUC, and the effect of keeping only trades with a score at or above `threshold` (chosen on TRAINING data). The interval for (mean return kept - mean return all) resamples whole blocks, because trades in one block share the market.

## `relative_to_block`

`lab/src/mltrade.rs:249`

```rust
pub fn relative_to_block(trades: &[Trade]) -> Vec<Trade>
```

Re-expresses every trade relative to its own block: `ret` becomes the excess over the block's mean return and `win` means "better than the block's median trade". Whole blocks win or lose together with the market; this asks the question a gate really answers, which entries are weaker than the others taken in the same period.

## `shuffled`

`lab/src/mltrade.rs:257`

```rust
pub fn shuffled(train: &[Trade], seed: u64) -> Vec<Trade>
```

Deterministic label shuffle (the leakage control): the same trades with outcomes assigned at random.

## `watch`

`lab/src/mltrade.rs:271`

```rust
pub fn watch(trades: &[Trade], nulls: usize) -> Option<Watch>
```

_No comment in the code; the signature above is the description._

## `summary`

`lab/src/mltrade.rs:293`  (method / nested)

```rust
pub fn summary(&self) -> String
```

_No comment in the code; the signature above is the description._

## `quantile`

`lab/src/mltrade.rs:301`

```rust
pub fn quantile(v: &[f64], q: f64) -> f64
```

_No comment in the code; the signature above is the description._

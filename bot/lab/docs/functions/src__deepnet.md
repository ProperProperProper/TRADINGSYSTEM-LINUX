# `src/deepnet.rs`

> Deep learning for the trading model, written from scratch on the lab's own autodiff engine (owner 2026-10-13: "make it as advanced as possible", "a proper AI model
> for trading"). Two architectures:
> * `Res`: a residual network (pre-layer-norm blocks, GELU, dropout) on the static features (the lab's candle features, the bot's score components, SPY-regime terms);
> * `Transformer`: self-attention over the last `seq_len` bars of the stock's own candle and INDICATOR channels (`indicators.rs`), combined with the static features -
> it reads the candles themselves and learns its own patterns.
>
> Both are trained with a loss that optimises the thing the bot uses, the CROSS-SECTIONAL rank correlation within each moment (1 - Pearson per moment, plus a small
> squared-error anchor), by AdamW with warm-up and a cosine schedule, global gradient clipping, dropout, data-parallel threads, and early stopping on the latest fifth of
> the training period. Deterministic for a seed. Whether they help is decided by the trial ledger (paired, multiple-testing-adjusted), never assumed.

22 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `label`

`src/deepnet.rs:29`  (method / nested)

```rust
pub fn label(&self) -> String
```

_No comment in the code; the signature above is the description._

## `new`

`src/deepnet.rs:39`  (method / nested)

```rust
fn new(seed: u64) -> Rng
```

_No comment in the code; the signature above is the description._

## `next`

`src/deepnet.rs:40`  (method / nested)

```rust
fn next(&mut self) -> u64
```

_No comment in the code; the signature above is the description._

## `unit`

`src/deepnet.rs:41`  (method / nested)

```rust
fn unit(&mut self) -> f64
```

_No comment in the code; the signature above is the description._

## `glorot`

`src/deepnet.rs:44`

```rust
fn glorot(rng: &mut Rng, r: usize, c: usize) -> Mat
```

_No comment in the code; the signature above is the description._

## `ones`

`src/deepnet.rs:45`

```rust
fn ones(c: usize) -> Mat
```

_No comment in the code; the signature above is the description._

## `zeros`

`src/deepnet.rs:46`

```rust
fn zeros(r: usize, c: usize) -> Mat
```

_No comment in the code; the signature above is the description._

## `build_rows`

`src/deepnet.rs:57`

```rust
pub fn build_rows(series: &BTreeMap<String, Vec<Bar>>, samples: &[Sample], seq_len: usize) -> Vec<Row>
```

Rows for `samples` (the universe samples of `mlcandle::build` + extensions): static features as given, and a window of `seq_len` bars of indicator channels ending at the sample's decision bar, from the stock's own candles.

## `fit_norm`

`src/deepnet.rs:78`

```rust
fn fit_norm(rows: &[&Row], seq_len: usize) -> Norm
```

_No comment in the code; the signature above is the description._

## `init_params`

`src/deepnet.rs:92`

```rust
pub fn init_params(g: &Genome, d: usize, seed: u64) -> Vec<Mat>
```

Notes from the code: ------------------------------------------------------------------ parameters and forward passes

## `keep_mask`

`src/deepnet.rs:121`

```rust
fn keep_mask(rng: &mut Rng, n: usize, p: f64) -> Vec<bool>
```

_No comment in the code; the signature above is the description._

## `forward_res`

`src/deepnet.rs:124`

```rust
fn forward_res(t: &mut Tape, params: &[Mat], g: &Genome, x: V, mut drop: Option<&mut Rng>) -> V
```

Residual network on a batch matrix (n x d) -> (n x 1).

## `forward_tr`

`src/deepnet.rs:142`

```rust
fn forward_tr(t: &mut Tape, params: &[Mat], g: &Genome, seq: V, stat: V, mut drop: Option<&mut Rng>) -> V
```

Transformer on ONE sample: the sequence (L x F) and the static row (1 x d) -> (1 x 1).

## `stat_mat`

`src/deepnet.rs:177`

```rust
fn stat_mat(r: &Row, n: &Norm) -> Mat
```

_No comment in the code; the signature above is the description._

## `seq_mat`

`src/deepnet.rs:178`

```rust
fn seq_mat(r: &Row, n: &Norm, l: usize) -> Mat
```

_No comment in the code; the signature above is the description._

## `forward_group`

`src/deepnet.rs:181`

```rust
fn forward_group(t: &mut Tape, params: &[Mat], g: &Genome, norm: &Norm, rows: &[&Row], drop: Option<&mut Rng>) -> V
```

Forward over the rows of ONE group (a moment, or any set), returning the predictions (n x 1) node.

## `moments`

`src/deepnet.rs:202`

```rust
fn moments(rows: &[&Row]) -> Vec<Vec<usize>>
```

_No comment in the code; the signature above is the description._

## `ic_of`

`src/deepnet.rs:208`

```rust
fn ic_of(model: &DeepModel, rows: &[&Row]) -> f64
```

_No comment in the code; the signature above is the description._

## `shard_grads`

`src/deepnet.rs:216`

```rust
fn shard_grads(params: &[Mat], g: &Genome, norm: &Norm, rows: &[&Row], ms: &[&Vec<usize>], seed: u64) -> (f64, Vec<Mat>)
```

Loss and parameter gradients of one shard of moments.

## `train`

`src/deepnet.rs:240`

```rust
pub fn train(rows: &[&Row], g: &Genome, seed: u64, threads: usize) -> DeepModel
```

Trains one model. `threads` shards each batch of moments across threads (data parallel, gradients summed). Returns the best-by-validation parameters.

## `predict_refs`

`src/deepnet.rs:292`

```rust
fn predict_refs(model: &DeepModel, rows: &[&Row]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `predict`

`src/deepnet.rs:299`

```rust
pub fn predict(model: &DeepModel, rows: &[Row]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

# `lab/src/autodiff.rs`

> A small reverse-mode automatic differentiation engine, written from scratch (owner 2026-10-13: "make it as advanced as possible"). It is what lets the lab train
> real deep architectures (residual networks with layer normalisation, Transformers with attention) with exact gradients instead of hand-derived backpropagation.
> Matrices are f64, row-major; every operation records itself on a `Tape`; `backward` walks the tape in reverse. Every operation is checked against numerical
> gradients in the tests (the only way to trust hand-written derivatives).

32 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `zeros`

`lab/src/autodiff.rs:10`  (method / nested)

```rust
pub fn zeros(r: usize, c: usize) -> Mat
```

_No comment in the code; the signature above is the description._

## `from`

`lab/src/autodiff.rs:11`  (method / nested)

```rust
pub fn from(r: usize, c: usize, d: Vec<f64>) -> Mat
```

_No comment in the code; the signature above is the description._

## `new`

`lab/src/autodiff.rs:50`  (method / nested)

```rust
pub fn new() -> Tape
```

_No comment in the code; the signature above is the description._

## `push`

`lab/src/autodiff.rs:51`  (method / nested)

```rust
fn push(&mut self, v: Mat, op: Op, param: Option<usize>) -> V
```

_No comment in the code; the signature above is the description._

## `val`

`lab/src/autodiff.rs:52`  (method / nested)

```rust
pub fn val(&self, v: V) -> &Mat
```

_No comment in the code; the signature above is the description._

## `grad`

`lab/src/autodiff.rs:53`  (method / nested)

```rust
pub fn grad(&self, v: V) -> &Mat
```

_No comment in the code; the signature above is the description._

## `input`

`lab/src/autodiff.rs:55`  (method / nested)

```rust
pub fn input(&mut self, m: Mat) -> V
```

A constant input (no gradient is used).

## `param`

`lab/src/autodiff.rs:57`  (method / nested)

```rust
pub fn param(&mut self, m: Mat, index: usize) -> V
```

A trainable parameter: `index` identifies it in the caller's parameter store so its gradient can be harvested.

## `param_grads`

`lab/src/autodiff.rs:58`  (method / nested)

```rust
pub fn param_grads(&self) -> Vec<(usize, &Mat)>
```

_No comment in the code; the signature above is the description._

## `add`

`lab/src/autodiff.rs:60`  (method / nested)

```rust
pub fn add(&mut self, a: V, b: V) -> V
```

_No comment in the code; the signature above is the description._

## `add_row`

`lab/src/autodiff.rs:61`  (method / nested)

```rust
pub fn add_row(&mut self, a: V, b: V) -> V
```

_No comment in the code; the signature above is the description._

## `mul`

`lab/src/autodiff.rs:68`  (method / nested)

```rust
pub fn mul(&mut self, a: V, b: V) -> V
```

_No comment in the code; the signature above is the description._

## `scale`

`lab/src/autodiff.rs:69`  (method / nested)

```rust
pub fn scale(&mut self, a: V, s: f64) -> V
```

_No comment in the code; the signature above is the description._

## `zip`

`lab/src/autodiff.rs:70`  (method / nested)

```rust
fn zip(&self, a: V, b: V, f: impl Fn(f64, f64) -> f64) -> Mat
```

_No comment in the code; the signature above is the description._

## `matmul`

`lab/src/autodiff.rs:75`  (method / nested)

```rust
pub fn matmul(&mut self, a: V, b: V) -> V
```

_No comment in the code; the signature above is the description._

## `matmul_bt`

`lab/src/autodiff.rs:82`  (method / nested)

```rust
pub fn matmul_bt(&mut self, a: V, b: V) -> V
```

_No comment in the code; the signature above is the description._

## `gelu`

`lab/src/autodiff.rs:89`  (method / nested)

```rust
pub fn gelu(&mut self, a: V) -> V
```

_No comment in the code; the signature above is the description._

## `tanh`

`lab/src/autodiff.rs:90`  (method / nested)

```rust
pub fn tanh(&mut self, a: V) -> V
```

_No comment in the code; the signature above is the description._

## `softmax_rows`

`lab/src/autodiff.rs:91`  (method / nested)

```rust
pub fn softmax_rows(&mut self, a: V) -> V
```

_No comment in the code; the signature above is the description._

## `layer_norm`

`lab/src/autodiff.rs:96`  (method / nested)

```rust
pub fn layer_norm(&mut self, x: V, g: V, b: V) -> V
```

_No comment in the code; the signature above is the description._

## `slice_cols`

`lab/src/autodiff.rs:110`  (method / nested)

```rust
pub fn slice_cols(&mut self, a: V, start: usize, len: usize) -> V
```

_No comment in the code; the signature above is the description._

## `concat_cols`

`lab/src/autodiff.rs:116`  (method / nested)

```rust
pub fn concat_cols(&mut self, parts: &[V]) -> V
```

_No comment in the code; the signature above is the description._

## `concat_rows`

`lab/src/autodiff.rs:124`  (method / nested)

```rust
pub fn concat_rows(&mut self, parts: &[V]) -> V
```

_No comment in the code; the signature above is the description._

## `dropout`

`lab/src/autodiff.rs:133`  (method / nested)

```rust
pub fn dropout(&mut self, a: V, keep: &[bool], p: f64) -> V
```

Inverted dropout with a caller-supplied 0/1 `keep` pattern (so training is deterministic given a seed): kept values are scaled by 1/(1-p).

## `mean_rows`

`lab/src/autodiff.rs:140`  (method / nested)

```rust
pub fn mean_rows(&mut self, a: V) -> V
```

_No comment in the code; the signature above is the description._

## `row`

`lab/src/autodiff.rs:141`  (method / nested)

```rust
pub fn row(&mut self, a: V, i: usize) -> V
```

_No comment in the code; the signature above is the description._

## `sum`

`lab/src/autodiff.rs:142`  (method / nested)

```rust
pub fn sum(&mut self, a: V) -> V
```

_No comment in the code; the signature above is the description._

## `mse`

`lab/src/autodiff.rs:143`  (method / nested)

```rust
pub fn mse(&mut self, pred: V, target: &[f64]) -> V
```

_No comment in the code; the signature above is the description._

## `pearson_loss`

`lab/src/autodiff.rs:150`  (method / nested)

```rust
pub fn pearson_loss(&mut self, pred: V, target: &[f64]) -> V
```

1 - Pearson correlation of `pred` (n x 1) with `target`: minimising it maximises the cross-sectional correlation (a rank-style objective), scale-free.

## `backward`

`lab/src/autodiff.rs:158`  (method / nested)

```rust
pub fn backward(&mut self, loss: V)
```

Fills the gradients of everything that contributes to the scalar `loss`.

## `acc`

`lab/src/autodiff.rs:221`

```rust
fn acc(dst: &mut Mat, src: &[f64])
```

_No comment in the code; the signature above is the description._

## `pearson`

`lab/src/autodiff.rs:224`

```rust
fn pearson(a: &[f64], b: &[f64]) -> (f64, f64, f64, f64)
```

(correlation, mean of a, sqrt of the sum of squared deviations of a, same for b)

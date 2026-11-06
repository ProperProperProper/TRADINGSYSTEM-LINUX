# `src/quant.rs`

> Quant review of the strategy tests (owner, 2026-10-11: "make this think like a quant trader and carefully analyse and learn
> from and apply things testing"). Computed by code from the per-block results (strategy_blocks), never by a model.
>
> What a quant asks of every test, and what the averages on the dashboard did not answer:
> - Is the gain real? Variant and current strategy are compared block by block on the SAME blocks (paired): mean difference,
> its t statistic, a bootstrap 95% interval and the share of blocks that were better.
> - Does it survive selection? ~200 variants were tried; the best of N noisy tries looks good by chance. A gain counts only if
> its t beats the expected maximum of N null t values (sqrt(2 ln N)) and its Benjamini-Hochberg q is below 0.10.
> - Is it worth the risk? Return per block divided by its volatility (a per-block Sharpe), worst block, profit factor.
> - Is it overfitted? How much of the training-half gain is left on the unseen half, per option family.
> - Which options carry edge? Every option's paired gain over all variants that used it.
>
> The view is computed for one half of the blocks: TRAINING (first half: what the agents and the planner may use, so choosing
> what to test never leaks into the verdict) or UNSEEN (second half: the human dashboard). The verdict rules themselves are
> unchanged (strategy::considerably_better and the full validation decide).

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `mean_sd`

`src/quant.rs:51`

```rust
fn mean_sd(x: &[f64]) -> (f64, f64)
```

Mean and sample standard deviation.

## `p_one_sided`

`src/quant.rs:60`

```rust
fn p_one_sided(t: f64) -> f64
```

One-sided p value of a t statistic (normal approximation; blocks are few, so it is a screen, not a proof).

## `bootstrap_ci`

`src/quant.rs:65`

```rust
fn bootstrap_ci(x: &[f64]) -> (f64, f64)
```

Bootstrap 95% interval of the mean (fixed seed: the same data gives the same interval).

## `blocks`

`src/quant.rs:75`

```rust
fn blocks(mem: &Connection, iv: &str, tokens: &str) -> Vec<(i64, f64, i64, i64)>
```

Per-block returns (and trades, wins) of a variant on cut A, in block order.

## `half`

`src/quant.rs:82`

```rust
fn half<T: Clone>(b: &[T], h: Half) -> Vec<T>
```

The chosen half of a block list.

## `paired_all`

`src/quant.rs:88`

```rust
pub fn paired_all(mem: &Connection, iv: &str, h: Half) -> Result<Vec<Paired>>
```

Paired statistics of every tested variant on interval `iv` (with Benjamini-Hochberg q values over all of them).

## `option_edges`

`src/quant.rs:134`

```rust
pub fn option_edges(ps: &[Paired]) -> Vec<(String, usize, f64, f64)>
```

Every option's paired gain over the variants that used it: (option, variants, mean of their mean gains, t over variants).

## `review`

`src/quant.rs:151`

```rust
pub fn review(mem: &Connection, h: Half) -> Result<(String, Value)>
```

The quant review as text and JSON for one half (see the module docs).

## `decay`

`src/quant.rs:192`

```rust
pub fn decay(mem: &Connection) -> Result<Vec<(String, usize, f64, f64)>>
```

Overfitting decay per option family: for variants with per-block results, training-half gain vs unseen-half gain (paired, against the current strategy). A family whose training gains vanish on unseen blocks is fitting noise. Human view only.

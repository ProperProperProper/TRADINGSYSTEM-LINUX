# `lab/src/mlimprove.rs`

> Stricter statistics for improving the candle model (owner 2026-10-13, "implement 2, 3, 4, 5, 6"):
> * a TRIAL LEDGER: every model, feature or scheme tried is recorded, and every interval is widened for the number of trials so far
> (Bonferroni: the tail probability is 0.025 / trials), because the more things are tried the more likely one looks good by luck;
> * PAIRED comparisons: a change is judged by the week-by-week difference against the model it modifies on the SAME test windows
> (much tighter than comparing two separate intervals);
> * a change is only ADOPTED when the paired difference is above zero at the adjusted level.

8 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`lab/src/mlimprove.rs:12`

```rust
pub fn init(db: &Connection) -> rusqlite::Result<()>
```

_No comment in the code; the signature above is the description._

## `record`

`lab/src/mlimprove.rs:18`

```rust
pub fn record(db: &Connection, family: &str, name: &str, metric: &str, value: f64, lo: f64, hi: f64, adopted: bool) -> rusqlite::Result<()>
```

Records (or updates) one trial. A trial counts once however many times it is re-run.

## `count`

`lab/src/mlimprove.rs:26`

```rust
pub fn count(db: &Connection) -> usize
```

_No comment in the code; the signature above is the description._

## `weekly`

`lab/src/mlimprove.rs:29`

```rust
pub fn weekly(test: &[Sample], pred: &[f64]) -> BTreeMap<i64, (f64, f64, usize)>
```

Per-week sums of the per-moment rank IC and top-minus-bottom-fifth spread: (ic sum, spread sum, moments).

## `interval`

`lab/src/mlimprove.rs:48`

```rust
pub fn interval(stat: &[(f64, usize)], tail: f64, seed: u64) -> (f64, f64, f64)
```

Interval of a statistic by resampling whole weeks. `tail` is the probability in each tail (0.025 = 95%). Returns (value, lo, hi).

## `pick`

`lab/src/mlimprove.rs:61`

```rust
fn pick(w: &BTreeMap<i64, (f64, f64, usize)>, spread: bool) -> Vec<(f64, usize)>
```

_No comment in the code; the signature above is the description._

## `adjusted`

`lab/src/mlimprove.rs:64`

```rust
pub fn adjusted(test: &[Sample], pred: &[f64], trials: usize, seed: u64) -> [f64; 6]
```

IC and spread intervals at the ADJUSTED level for `trials` trials so far: (ic, lo, hi, spread bps, lo, hi).

## `paired_ic`

`lab/src/mlimprove.rs:74`

```rust
pub fn paired_ic(test: &[Sample], base: &[f64], new: &[f64], trials: usize, seed: u64) -> (f64, f64, f64)
```

Paired difference (new - base) of the weekly IC on the same test set, at the adjusted level: (diff, lo, hi). Moments are matched by position, so both predictions must be for the same `test`.

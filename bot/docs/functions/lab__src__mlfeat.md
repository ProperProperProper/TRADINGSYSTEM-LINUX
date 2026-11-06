# `lab/src/mlfeat.rs`

> Candidate extra features for the candle model (owner 2026-10-13, item 5). Each is added ALONE to the base model and kept only if the paired,
> multiple-testing-adjusted test says it helps (`mlimprove`). All use bars up to the decision bar only.
> gap          today's open against the previous session's close (earnings and news days show up as gaps)
> gap_abs      the size of that gap
> fh_vol       volume of today's first hour against its 20-session norm (1 until the first hour is done)
> vol_profile  volume so far today against the 20-session norm at the same time of day
> beta         the stock's beta to SPY over the previous 20 sessions (15-minute returns)
> resid_26     its one-session return minus beta x SPY's (what the market does not explain)
> peer_rel_26  its one-session return minus the average of its 5 most correlated peers' (relative strength inside its cluster)

3 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `session_of`

`lab/src/mlfeat.rs:18`

```rust
fn session_of(ts: i64) -> i64
```

_No comment in the code; the signature above is the description._

## `layout`

`lab/src/mlfeat.rs:21`

```rust
fn layout(bars: &[Bar]) -> Vec<(i64, usize, usize)>
```

Per bar: (session id, index inside the session, session's first bar index).

## `extend`

`lab/src/mlfeat.rs:31`

```rust
pub fn extend(series: &BTreeMap<String, Vec<Bar>>, base: &[Sample]) -> Vec<Sample>
```

_No comment in the code; the signature above is the description._

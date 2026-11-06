# `examples/oos_study.rs`

> Out-of-sample study on the candles already in the bot's store (no new candle pulls, no orders).
>
> Rolling walk-forward, exactly as paper runs it: before each traded session the full search
> picks settings on that interval's fixed window ENDING before the session (15m: the last 14
> the latest 14 sessions for every interval, with only indicator warm-up candles before that window), champion/challenger against
> the settings in use (`choose_live`, recorded best settings re-tested), and ONE continuous
> portfolio trades the unseen session. Several seeds; every variant runs on the same data and seeds.
> The one-session interval cannot roll (its 69 warm-up + 21 test sessions use the whole store), so
> it is tested inside its final window like the service's walk-forward, with more unseen sessions.
>
> cargo run --release --example oos_study -- <interval key> <variant[,variant..]> [seeds] [unseen sessions]

5 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `variant`

`examples/oos_study.rs:32`

```rust
fn variant(name: &str, interval: Interval) -> Result<Variant>
```

Variant names are '+'-joined tokens, e.g. `keep+top13`.

## `run`

`examples/oos_study.rs:86`

```rust
fn run(big: &Big, v: &Variant, seed: u64, unseen: usize, in_window: bool) -> Result<RunResult>
```

Rolling walk-forward of `v` with `seed` over the last `unseen` sessions of `big`.

## `med`

`examples/oos_study.rs:162`

```rust
fn med(v: &mut [f64]) -> f64
```

Median of `v` (sorts it in place; NaN when empty).

## `table`

`examples/oos_study.rs:176`

```rust
fn table(lines: &str) -> Result<()>
```

The aggregate table of JSON result lines (one per variant and seed), variants in first-seen order.

## `main`

`examples/oos_study.rs:207`

```rust
async fn main() -> Result<()>
```

Out-of-sample study: runs one variant on unseen sessions (`run`) or prints a summary table of collected results (`table`).

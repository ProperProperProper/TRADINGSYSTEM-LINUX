# `src/portfolio.rs`

> Mean-CVaR optimisation and risk-based slot sizing (the idea behind NVIDIA's cuFOLIO example, owner 2026-10-13: "recreate some of the useful things"), in-house.
> `mean_cvar` chooses weights that minimise the expected loss in the worst tail of REAL historical joint scenarios minus a reward for the model's expected return, under
> constraints (no shorting, a cap per position, a minimum invested share, cash allowed); projected subgradient descent on the CPU. NOTE: the bot does not pick a portfolio.
> It holds 2 positions at a time and ANY symbol whose signal fires first takes a slot (owner), so the part that applies is `slot_scale`: how large a triggered stock's
> position should be, from the tail of its OWN real one-session returns.

5 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `cvar`

`src/portfolio.rs:13`

```rust
pub fn cvar(scen: &[Vec<f64>], w: &[f64], alpha: f64) -> f64
```

CVaR at level `alpha` of the portfolio's one-session return: the average of the worst (1 - alpha) share of the scenarios, as a positive LOSS.

## `project`

`src/portfolio.rs:20`

```rust
fn project(w: &mut [f64], wmax: f64, min_invest: f64, budget: f64)
```

_No comment in the code; the signature above is the description._

## `mean_cvar`

`src/portfolio.rs:37`

```rust
pub fn mean_cvar(scen: &[Vec<f64>], mu: &[f64], alpha: f64, lambda: f64, wmax: f64, min_invest: f64, budget: f64) -> Opt
```

minimise CVaR_alpha(w) - lambda * mu.w  subject to 0 <= w_i <= wmax and min_invest <= sum w <= budget (the rest is cash). The bot always deploys its slots, so the comparison arms use min_invest = 1; without a minimum the risk-only optimum is trivially all cash.

## `scenarios`

`src/portfolio.rs:61`

```rust
pub fn scenarios(history: &BTreeMap<i64, BTreeMap<String, f64>>, symbols: &[String]) -> Vec<Vec<f64>>
```

Real joint scenarios: for `symbols`, the one-session returns at every moment in `history` where all of them exist, each column centred on its own mean.

## `slot_scale`

`src/portfolio.rs:70`

```rust
pub fn slot_scale(own_returns: &[f64], reference_cvar: f64) -> f64
```

Position size multiplier (0.5-1.0) for a stock that just triggered a signal: the slot is full size when its own tail (CVaR 90% of one-session returns) is no worse than the reference tail (the median stock's), and shrinks in proportion to how much wilder it is, never below half.

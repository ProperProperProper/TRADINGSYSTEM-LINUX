# `src/engine/live_options.rs`

> THE LIVE STRATEGY'S ENTRY OPTIONS: research options that passed the lab's full validation and were promoted into the live
> strategy (owner 2026-10-10: "if it passes always go live with the strat; update backtester and paper"; "claude makes code
> changes, not anything else"; "always update the strat in repo and put comments on what is different and why").
>
> How a promotion happens: the lab's code (qwen-lab `promote`) finds a variant that passes the full validation (every block
> offset) and writes a promotion request; the hourly promotion job (ops/promote.sh) starts a Claude Code session, only while the
> US market is closed (never disrupting paper trading), which changes [`LIVE`] below WITH A COMMENT (what differs from the
> previous strategy, the validation numbers, why), adds a line to docs/STRATEGY.md, runs the tests, deploys and pushes.
>
> Everything uses [`space`]: the live search (and therefore the paper account, which trades the search's settings), the
> console's stability and walk-forward tests, and the backtester (`examples/variant_study`), so what is measured and what trades
> is always the same strategy. Exits are never touched: only entry options can be promoted ([`is_live_token`]).

4 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `is_live_token`

`src/engine/live_options.rs:53`

```rust
pub fn is_live_token(tok: &str) -> bool
```

Whether `tok` may be part of the live strategy: entry-side options only (gates, confirmations, trend kind, graduated sizing, entry band, basket size, DCA and tuned entry ranges). Never a signal, direction, exit, universe or research switch.

## `apply_token`

`src/engine/live_options.rs:63`

```rust
pub fn apply_token(space: &mut SearchSpace, tok: &str) -> Result<()>
```

Applies one entry option to a search space (the one mapping shared by the live search and the backtester).

## `space`

`src/engine/live_options.rs:141`

```rust
pub fn space(interval: Interval, live: &LiveStrategy) -> (SearchSpace, Vec<String>)
```

The search space of `interval` under `live` (the plain `space_for` when its options do not apply), and the options used.

## `params_fit`

`src/engine/live_options.rs:156`

```rust
pub fn params_fit(p: &XsParams, space: &SearchSpace) -> bool
```

Whether recorded settings fit `space`: every entry option the space switches on must be present in the settings (settings recorded before a promotion lack the new gate and would trade the old strategy, so they are not re-tested).

# `lab/src/promote.rs`

> Promotion requests (owner 2026-10-10: "if it passes always go live with the strat"; "claude makes code changes, not anything
> else"; "make sure it doesn't disrupt paper trading live"). Code, no model; runs in every hourly tick (`qwen-lab promote`).
>
> A variant qualifies when it is "considerably better" on cuts A and B (`strategy::considerably_better`: validated, same candles
> as the current strategy, unseen-block return >= +1.0 point per block on both cuts, win rate not lower, noisy tape positive)
> AND passes the full validation (`strategy::full_validation`: all five block cuts measured, at most one failing), on the live
> interval, with entry options only. Whoever proposed it (code or a Hermes agent): the owner said not to wait for approval.
>
> The lab never changes the bot. It writes ONE request at a time to `<lab>/promotions/pending/<variant>.json` with the evidence;
> the bot repo's hourly promotion job (ops/promote.sh) runs a Claude Code session only while the US market is closed, which
> changes the live strategy constant in code with comments (what differs, the numbers, why), tests, deploys and pushes, then
> reports back (`qwen-lab promotion-status`). Requests and their outcome are kept in the `promotions` table.

6 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `is_live_token`

`lab/src/promote.rs:22`

```rust
pub fn is_live_token(tok: &str) -> bool
```

Entry options the bot's live strategy accepts (mirror of moomoo-bot `engine::live_options::is_live_token`).

## `init`

`lab/src/promote.rs:32`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the promotions table if missing.

## `qualifying`

`lab/src/promote.rs:41`

```rust
pub fn qualifying(mem: &Connection) -> Result<(Vec<Value>, Vec<String>)>
```

Variants that qualify for the live strategy now (best first), with the reason any considerably-better variant does not.

## `run`

`lab/src/promote.rs:67`

```rust
pub fn run(mem: &Connection, lab_root: &std::path::Path) -> Result<Vec<String>>
```

One promotion pass: writes a request for the best qualifying variant unless a request is already pending. Returns notes.

## `set_status`

`lab/src/promote.rs:112`

```rust
pub fn set_status(mem: &Connection, lab_root: &std::path::Path, variant: &str, status: &str, detail: &str) -> Result<()>
```

Records the outcome reported by the bot's promotion job ("live" with the commit, or "failed" with the reason), and moves the request out of pending.

## `list`

`lab/src/promote.rs:127`

```rust
pub fn list(mem: &Connection, limit: usize) -> Vec<Value>
```

Every promotion (newest first), for the dashboard.

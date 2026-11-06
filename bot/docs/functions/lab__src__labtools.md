# `lab/src/labtools.rs`

> The lab's tools, used by the brain (src/brain_roles.rs, src/brain.rs) and for the known-numbers check (src/facts.rs). The brain acts only through them,
> as authors `brain-<role>`; code validates every request and judges every result:
>
> - `lab_queue_test`: ask the strategy loop to test a variant (validated against the owner's rules; tested within 1-2 rounds),
> - `lab_note` / `lab_inbox`: notes between the roles (shared `agent_messages` board),
> - `lab_experiments`, `lab_winrate`, `lab_rules`, `lab_research_search`, `lab_record_idea`: results, win/lose findings, rules, papers.
>
> Nothing here can change the bot, the data or any account.

17 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `similar_tested`

`lab/src/labtools.rs:18`

```rust
fn similar_tested(mem: &Connection, options: &[String], n: usize) -> String
```

Up to `n` tested 5h variants that share an option family with `options`: status, the code's reason when it failed, and the TRAINING-half gain per block and win rate (never the unseen half).

## `paper_decisions`

`lab/src/labtools.rs:32`

```rust
fn paper_decisions(ctx: &Ctx, n: usize) -> Result<String>
```

The bot's newest paper decisions and closed paper trades, from its archive (read-only).

## `init`

`lab/src/labtools.rs:50`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the test-request queue and the reward events if missing (the tables were called hermes_queue and hermes_events until 2026-10-13).

## `call`

`lab/src/labtools.rs:74`

```rust
pub fn call(ctx: &Ctx, name: &str, args: &Value) -> Result<String>
```

Runs a lab tool.

## `event`

`lab/src/labtools.rs:268`

```rust
fn event(mem: &Connection, author: &str, kind: &str, points: f64, detail: &str) -> Result<()>
```

Records a reward event for an agent.

## `outcome_points`

`lab/src/labtools.rs:275`

```rust
fn outcome_points(result: Option<&Value>) -> Option<(f64, String)>
```

Points for one tested result under REWARD_RULES: (points, label); None while untested.

## `scoreboard`

`lab/src/labtools.rs:292`

```rust
pub fn scoreboard(mem: &Connection) -> Result<Vec<Value>>
```

The reward scoreboard: the brain's roles, best first, with the reasons for each point.

## `untested`

`lab/src/labtools.rs:331`

```rust
pub fn untested(mem: &Connection, iv: &str, n: usize) -> Result<Vec<String>>
```

Untested variants of the current strategy on `iv` (cut A, current regime, nothing queued for it): single options first, then pairs that add one more option to the best-performing tested singles; families ordered by their record (validated or cut-A passes first, then average training return). Valid under the owner's rules only.

## `ensure_agent`

`lab/src/labtools.rs:363`

```rust
fn ensure_agent(name: &str) -> Result<()>
```

Refuses names that are not one of the brain's agents (brain-<role>).

## `options_text`

`lab/src/labtools.rs:368`

```rust
fn options_text(ctx: &Ctx) -> String
```

The options a test may use (the lab's vocabulary plus tuned ranges) and the candle sizes.

## `experiments`

`lab/src/labtools.rs:380`

```rust
fn experiments(mem: &Connection, n: usize) -> Result<String>
```

Tested variants for the brain: status and TRAINING-half numbers only.

## `train_worst`

`lab/src/labtools.rs:407`

```rust
fn train_worst(mem: &Connection, iv: &str, tokens: &str) -> Option<f64>
```

The worst block of the TRAINING half (the first half of a variant's cut-A blocks), if its blocks are recorded.

## `verdict`

`lab/src/labtools.rs:413`

```rust
fn verdict(r: &Value) -> Option<String>
```

The code's reason a variant failed (cut B's verdict for a failure on cut B), without numbers from the unseen blocks.

## `queue_test`

`lab/src/labtools.rs:419`

```rust
fn queue_test(ctx: &Ctx, args: &Value) -> Result<String>
```

Validates and queues a test request from a brain role.

## `take_queue`

`lab/src/labtools.rs:456`

```rust
pub fn take_queue(mem: &Connection, n: usize) -> Result<Vec<(String, String, String)>>
```

Up to `n` queued requests for this round (marked picked), oldest first; requests that became invalid or already tested are closed.

## `queue_with_results`

`lab/src/labtools.rs:491`

```rust
pub fn queue_with_results(mem: &Connection, leaderboard: &[Value], limit: usize) -> Result<Vec<Value>>
```

The test requests for the dashboard, newest first, with the code's verdict when tested.

## `reports`

`lab/src/labtools.rs:502`

```rust
pub fn reports(root: &std::path::Path, limit: usize) -> Vec<Value>
```

Latest role reports for the dashboard (brain/reports/*.md: written by the brain, and before 2026-10-13 by the retired Hermes agents; the folder name is historical).

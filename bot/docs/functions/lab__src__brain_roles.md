# `lab/src/brain_roles.rs`

> The brain's team: twelve roles done by code and the small models in `brain.rs` and `brain_agents.rs` (owner 2026-10-13: a narrow Rust model for
> this bot; no language model). Every role works through the lab's tools (`lab_queue_test`, `lab_note`, `lab_new_results`, `lab_record_idea`,
> `lab_whiteboard_edit`, src/labtools.rs), as author `brain-<role>`, so the lab's validation, honesty gate, scoreboard and dashboards judge every
> role by what code measures. Nothing a role writes contains a number it did not copy from a tool.
>
> | role | what it does |
> |---|---|
> | strategist | queues untested variants under rotating policies (`brain::cycle`), keeps the shared plan true, reads its inbox |
> | tuner | queues tuned search ranges (`tune:margin`, `tune:dca`) from where the most profitable training runs set them (`strategy::tuning_ranges`) |
> | researcher | rotates through market-research topics, searches the literature (guarded), records ideas mapped to the options that implement them |
> | auditor | reviews new results for fragility / too few trades and notes the strategist |
> | statistician | passes on the quant verdict, flags validated/candidate results whose gain is not consistent across blocks |
> | execution | reads the bot's real paper decisions, reports what blocks entries (quoting the reason exactly) and queues one test that addresses it |
>
> `run_role` also writes a report to `brain/reports/` and a line to `logs/brain.log`; `serve` runs the roles in rotation as an always-on process.
> Knowledge (every note, idea, finding, plan) is kept in the `brain_facts` table, which also prevents repeating a note within a day.

30 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `agent`

`lab/src/brain_roles.rs:28`

```rust
pub fn agent(role: &str) -> String
```

_No comment in the code; the signature above is the description._

## `init_store`

`lab/src/brain_roles.rs:31`

```rust
pub fn init_store(mem: &Connection) -> Result<()>
```

Notes from the code: ------------------------------------------------------------------ knowledge store (everything on disk)

## `recent`

`lab/src/brain_roles.rs:38`

```rust
pub fn recent(mem: &Connection, role: &str, kind: &str, subject: &str, secs: i64) -> Result<bool>
```

Has this role already recorded this kind of fact about this subject within the last `secs` seconds?

## `remember`

`lab/src/brain_roles.rs:42`

```rust
pub fn remember(mem: &Connection, role: &str, kind: &str, subject: &str, text: &str, source: &str) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `lessons`

`lab/src/brain_roles.rs:48`

```rust
pub fn lessons(mem: &Connection, words: &[String], limit: usize) -> Result<Vec<String>>
```

Stored lessons and skills whose text contains all the given words (case-insensitive), newest first.

## `tool`

`lab/src/brain_roles.rs:60`

```rust
pub(crate) fn tool(ctx: &Ctx, name: &str, args: Value) -> Result<String>
```

_No comment in the code; the signature above is the description._

## `note`

`lab/src/brain_roles.rs:64`

```rust
pub(crate) fn note(ctx: &Ctx, from: &str, to: &str, subject: &str, text: &str, plain: &str) -> Result<Option<String>>
```

Leaves a note through `lab_note` (at most once per `subject` per day). `plain` is the same finding without numbers, used when the honesty gate cannot verify a number right now. Returns a line for the report, or None when it was a repeat.

## `num_after`

`lab/src/brain_roles.rs:77`

```rust
fn num_after(s: &str, marker: &str) -> Option<f64>
```

_No comment in the code; the signature above is the description._

## `parse_new_results`

`lab/src/brain_roles.rs:84`

```rust
pub fn parse_new_results(text: &str) -> Vec<NewResult>
```

Parses `lab_new_results`: `  [status] 5h|key: training +2.31%/block, noisy +1.20%, win rate 46%, 9.0 trades/block`.

## `red_flags`

`lab/src/brain_roles.rs:99`

```rust
pub fn red_flags(r: &NewResult) -> Vec<(String, String)>
```

Red flags the auditor reports for one result: (text with the lab's own numbers, the same without numbers).

## `parse_decisions`

`lab/src/brain_roles.rs:115`

```rust
pub fn parse_decisions(text: &str) -> Vec<Decision>
```

Parses `lab_paper_decisions`: `  2026-10-13: <reason> | funded 2 of 2 | gates {...} | ABBV rejected: why; ACN rejected: why`.

## `dominant_rejection`

`lab/src/brain_roles.rs:125`

```rust
pub fn dominant_rejection(d: &Decision) -> Option<(String, usize)>
```

The most common rejection reason in a decision and how many symbols it covers.

## `parse_papers`

`lab/src/brain_roles.rs:135`

```rust
pub fn parse_papers(text: &str) -> Vec<Paper>
```

Parses `lab_research_search`: `- Title (published) https://...` (withheld results are skipped).

## `mentions`

`lab/src/brain_roles.rs:151`

```rust
pub fn mentions(p: &Paper, keywords: &[&str]) -> bool
```

Does the paper itself mention the topic? (a search hit that never uses the topic's own terms is not evidence about the option)

## `queue`

`lab/src/brain_roles.rs:180`

```rust
pub(crate) fn queue(ctx: &Ctx, role: &str, options: &[String], why: &str) -> (bool, String)
```

_No comment in the code; the signature above is the description._

## `log_proposal`

`lab/src/brain_roles.rs:187`

```rust
pub(crate) fn log_proposal(mem: &Connection, policy: &str, role: &str, key: &str, queued: bool, note: &str) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `ml_lead`

`lab/src/brain_roles.rs:197`

```rust
fn ml_lead(ctx: &Ctx) -> Result<Vec<String>>
```

The ML model's lead, acted on (owner 2026-10-13: "finish it all"): the machine-learning entry gate `mlgate` (its percentile searched by the bot's own per-block search) and its pairing with the validated options get tested through the same validated queue as every other variant, once each. Only when the bot's score file exists; the lab's judging (cuts A-E, noisy tape, holdout) decides, not this rule.

Notes from the code: ------------------------------------------------------------------ the roles

## `strategist`

`lab/src/brain_roles.rs:220`

```rust
pub fn strategist(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `tuner`

`lab/src/brain_roles.rs:243`

```rust
pub fn tuner(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `researcher`

`lab/src/brain_roles.rs:260`

```rust
pub fn researcher(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `auditor`

`lab/src/brain_roles.rs:285`

```rust
pub fn auditor(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `ml_watch`

`lab/src/brain_roles.rs:302`

```rust
pub fn ml_watch(ctx: &Ctx) -> Result<Option<String>>
```

The pre-registered machine-learning test on the bot's real trades (src/mltrade.rs), at most once a day; Discord hears about it the first time and whenever the verdict changes.

## `candle_watch`

`lab/src/brain_roles.rs:316`

```rust
pub fn candle_watch(ctx: &Ctx) -> Result<Option<String>>
```

Daily pre-registered test on the market's own candles (`mlcandle::report`): refreshes the lab's candle store, runs the fixed test, and tells Discord the first time and whenever the verdict flips. The full report is written to logs/candle-ml.txt; the knowledge store keeps a one-line summary.

## `candle_improve`

`lab/src/brain_roles.rs:345`

```rust
pub fn candle_improve(ctx: &Ctx) -> Result<Option<String>>
```

The daily improvement round on the candle model (`mlround::run`): recency schemes, extra features and meta-labelling against the trial ledger. The report goes to logs/candle-improve.txt; Discord hears only when something is ADOPTED (paired difference above zero at the multiple-testing-adjusted level).

## `statistician`

`lab/src/brain_roles.rs:362`

```rust
pub fn statistician(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `execution`

`lab/src/brain_roles.rs:391`

```rust
pub fn execution(ctx: &Ctx) -> Result<Vec<String>>
```

_No comment in the code; the signature above is the description._

## `stamp`

`lab/src/brain_roles.rs:434`

```rust
fn stamp() -> String
```

Notes from the code: ------------------------------------------------------------------ running roles

## `run_role`

`lab/src/brain_roles.rs:437`

```rust
pub fn run_role(ctx: &Ctx, role: &str) -> Vec<String>
```

Runs one role, writes its report (`brain/reports/<stamp>-brain-<role>.md`) and a line to `logs/brain.log`; a failing role is reported, never fatal.

## `next_role`

`lab/src/brain_roles.rs:462`

```rust
pub fn next_role(mem: &Connection) -> Result<&'static str>
```

The next role in rotation (state in the `counters` table, so a restart continues where it stopped).

## `serve`

`lab/src/brain_roles.rs:469`

```rust
pub fn serve(ctx: &Ctx, every_secs: u64) -> !
```

Always-on process: one role every `every_secs`, in rotation, so each role runs about every `6 * every_secs`.

# `src/strategy.rs`

> Strategy-improvement loop with a persistent WHITEBOARD: the local model learns from backtest results, proposes new entry
> options, and keeps its own notes; deterministic code scores everything and decides what is a winner.
>
> GOAL (user): the best ENTRIES for wins, with the current EXIT strategy left exactly as it is (it works). So the vocabulary holds
> only entry options (the bot applies them to entries only; its exit test never reads them) and nothing that changes how a
> position is closed (hold length, keep-on-rebalance, the trend horizon and margin used by the exit).
>
> A variant is a set of tokens that widen what the bot's own random search may draw (an indicator confirmation, a trend type, a
> data-driven per-stock gate, a signal family, the number of liquid names allowed to enter, ...). Settings still come from the
> search, never fixed values. The bot-side tool (`variant_study` in the moomoo bot) searches on the 14 sessions before each
> look-forward block and trades the next 14 with the winner, also replaying them on a noisy tape with 1.5x costs.
>
> The model's memory is a small persistent whiteboard (tables `whiteboard`, `whiteboard_log` in the lab's memory database):
> - FACTS are written by code from the results (leaders, the average effect of each option, dead ends). The model never edits them
> and only ever sees the TRAINING half of the blocks: the second half is a holdout that only the verdict reads.
> - BELIEFS and PLAN are the model's: it adds, updates and retires short notes through validated operations. A belief must cite
> results that exist, and every percentage it writes must match them; instruction-like text is refused; the board is capped, so
> it stays small and stable.
> - Everything persists, every edit is logged, and the loop can run forever (`improve --forever`) and resume after a restart.

52 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`src/strategy.rs:100`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the strategy tables (runs, whiteboard, blocks, origins) and migrates older layouts.

## `parse`

`src/strategy.rs:173`

```rust
pub fn parse(out: &str) -> Result<(i64, f64, Vec<Row>)>
```

Parse `variant_study` output: (blocks, reference buy&hold mean, rows).

## `header_iv_eq`

`src/strategy.rs:197`

```rust
fn header_iv_eq(out: &str) -> Result<(String, f64)>
```

Interval key and start capital from a study output's header line ("5h start capital 999991: 19 blocks, ...").

## `verdict`

`src/strategy.rs:209`

```rust
pub fn verdict(r: &Row, baseline: Option<&Row>) -> (bool, String)
```

Code decides, from the holdout half: a variant is a WINNER for an interval and capital only if it makes money in BOTH halves and in the holdout beats the baseline, holds up in at least 40% of blocks, never loses more than 8% in a block, trades enough to be real, and still makes money on the noisy tape (jittered prices, costs x1.5) in both halves.

## `key`

`src/strategy.rs:248`

```rust
pub fn key(tokens: &[String]) -> String
```

Canonical key of a token set (sorted, `baseline` when empty).

## `tune_token`

`src/strategy.rs:260`

```rust
pub fn tune_token(t: &str) -> Option<(&str, f64, f64)>
```

A fine-tuning token `tune:margin=<lo>-<hi>` (entry trend margin %, 0-10) or `tune:dca=<lo>-<hi>` (add on a dip, 0-40 %).

## `valid`

`src/strategy.rs:270`

```rust
pub fn valid(tokens: &[String]) -> bool
```

Tokens from the vocabulary (or valid fine-tuning tokens) only, at most `MAX_OPTIONS`, at most one per exclusive group (a `tune:dca` range counts in the DCA family, one `tune:margin` at a time).

## `why_invalid`

`src/strategy.rs:285`

```rust
pub fn why_invalid(tokens: &[String]) -> Option<String>
```

Why `tokens` is not a valid variant, in words an agent can act on (None when valid).

## `store`

`src/strategy.rs:317`

```rust
fn store(mem: &Connection, now: i64, iv: &str, eq: f64, cut: &str, blocks: i64, spy_bh: f64, r: &Row, regime: &str) -> Result<()>
```

Stores one variant's result for an interval, capital and block cut (replacing an earlier one).

## `regime_of`

`src/strategy.rs:327`

```rust
pub fn regime_of(out: &str) -> String
```

The live strategy a study ran under, from its "live <options>" line ('' = the plain strategy; older output has no line).

## `current_regime`

`src/strategy.rs:332`

```rust
pub fn current_regime(mem: &Connection) -> String
```

The current live strategy as far as the results know: the regime of the newest current-strategy (baseline) result.

## `import_output_cut`

`src/strategy.rs:344`

```rust
pub fn import_output_cut(mem: &Connection, out: &str, cut: &str) -> Result<usize>
```

Same, for a study run on block cut `cut` ("A" offsets 0,5,10 or "B" offsets 2,7,12); lines before the header (progress messages) are skipped.

## `store_blocks`

`src/strategy.rs:359`

```rust
pub fn store_blocks(mem: &Connection, out: &str, iv: &str, eq: f64, cut: &str, now: i64) -> Result<usize>
```

Stores the per-block results printed by variant_study ("blocks <variant> <start>/<ret>/<trades>/<wins>;...") for charts.

## `best_ai_runs`

`src/strategy.rs:384`

```rust
pub fn best_ai_runs(mem: &Connection, capital: f64) -> Result<Vec<Value>>
```

The best AI-proposed runs block by block, for the dashboard's charts: the brain's best variant (largest unseen-block gain over the current strategy on cut A, any status) and the validated variant with the largest gain (whoever proposed it). Each: per-block returns next to the current strategy's on the same blocks (matched by block start), the split between the training half (seen by agents) and the holdout half, and compounded USD curves.

## `blocks_of`

`src/strategy.rs:431`

```rust
pub fn blocks_of(mem: &Connection, iv: &str, tokens: &str, cut: &str) -> Vec<(i64, f64, i64, i64, bool)>
```

Per-block results of one variant (cut A by default) for charts: (start, return %, trades, wins, chain).

## `all_runs`

`src/strategy.rs:440`

```rust
fn all_runs(mem: &Connection) -> Result<Vec<RunRow>>
```

Every stored strategy result as (tokens, interval, capital, cut, row).

## `status`

`src/strategy.rs:456`

```rust
fn status(runs: &[RunRow], iv: &str, eq: f64, tokens: &str) -> &'static str
```

`validated` (passes on cut A and the alternate cut B), `candidate` (passes A, B pending), `failed-holdout`, `failed-alt-cut`.

## `render_board`

`src/strategy.rs:469`

```rust
pub fn render_board(mem: &Connection) -> Result<String>
```

The model's whiteboard text: fixed rules, code-written facts (training half only), then the model's own beliefs and plan.

## `percent_numbers`

`src/strategy.rs:555`

```rust
pub(crate) fn percent_numbers(text: &str) -> Vec<f64>
```

Percentages written in a text ("1.5%", "+0.30%"), as absolute numbers.

## `clean_text`

`src/strategy.rs:577`

```rust
pub(crate) fn clean_text(text: &str, min: usize, max: usize) -> Result<String>
```

A note must be short plain text that looks like a finding, never an instruction to the model that will read it later.

## `claims_ok`

`src/strategy.rs:590`

```rust
fn claims_ok(text: &str, cited: &[Row]) -> Result<()>
```

Every percentage in `text` must match a number of the cited training-half results (within 0.06 points).

## `log_op`

`src/strategy.rs:605`

```rust
fn log_op(mem: &Connection, now: i64, op: &str, detail: &Value, accepted: bool, reason: &str) -> Result<()>
```

Logs a whiteboard edit and whether it was accepted.

## `evidence_rows`

`src/strategy.rs:611`

```rust
fn evidence_rows(mem: &Connection, keys: &[String]) -> Result<Vec<Row>>
```

The cited results, `interval|tokens`, as rows of cut A (all must exist).

## `apply_ops`

`src/strategy.rs:623`

```rust
pub fn apply_ops(mem: &Connection, ops: &[Value], now: i64) -> Vec<String>
```

Apply the model's whiteboard operations; each one is validated and logged, a bad one is refused and explained.

## `account_capital`

`src/strategy.rs:693`

```rust
fn account_capital(cfg: &crate::config::StrategyConfig) -> Option<f64>
```

The paper account's latest available cash from the bot's store (read-only), floored like the bot does.

## `capitals`

`src/strategy.rs:700`

```rust
fn capitals(cfg: &crate::config::StrategyConfig) -> Vec<f64>
```

Every capital scale the loop tests at (the configured ones, then the account's own).

## `run_study`

`src/strategy.rs:713`

```rust
fn run_study(cfg: &crate::config::StrategyConfig, iv: &str, names: &str, offsets: Option<&str>, capital: f64) -> Result<String>
```

Run the bot-side study for one interval at one capital. `offsets` picks the block alignment (cut A default `0,5,10`, cut B `2,7,12`).

## `baseline_stale`

`src/strategy.rs:739`

```rust
fn baseline_stale(mem: &Connection, iv: &str, capital: f64, cut: &str, now: i64) -> bool
```

Whether the stored baseline of (interval, capital, cut) is missing, older than a day, or lacks the trade win rate.

## `parse_origin`

`src/strategy.rs:751`

```rust
fn parse_origin(line: &str) -> Option<(String, String, String, String)>
```

Which brain role (or code path) proposed a variant, parsed from the strategy loop's own lines: "<brain-role> asked to test iv|k", "research idea queued for testing: iv|k", "planner picks iv|k", "explorer chooses iv|k", "interval sweep: baseline on iv". The first proposer keeps the credit.

## `record_origins`

`src/strategy.rs:779`

```rust
pub fn record_origins(mem: &Connection, ts: i64, lines: &[String]) -> Result<usize>
```

Records the origins found in `lines` (first proposer wins); returns how many were new.

## `backfill_origins`

`src/strategy.rs:793`

```rust
pub fn backfill_origins(mem: &Connection, log: &str) -> Result<usize>
```

One-time backfill of origins from the strategy loop's logs ("[ts] round N:" headers carry the time).

## `origins_from_logs`

`src/strategy.rs:809`

```rust
pub fn origins_from_logs(ctx: &Ctx) -> Result<usize>
```

Backfills origins from every improve*.log in the lab's log folder; returns how many were recorded.

## `agent_results`

`src/strategy.rs:823`

```rust
pub fn agent_results(mem: &Connection, capital: f64, limit: usize) -> Result<Value>
```

What the AI agents (and the code paths) achieved, for the dashboards: per source the number proposed and tested, the code's verdicts, the best result on the unseen half against the current strategy, and the newest tested proposals. Human view: holdout numbers are shown (the agents themselves never see them).

## `stale_winners`

`src/strategy.rs:895`

```rust
pub fn stale_winners(mem: &Connection, capital: f64) -> Result<Vec<(String, String)>>
```

Validated and candidate variants whose cut-A result is older than the current baseline's (measured on different data) or lacks the trade win rate: they are re-tested so every winner is compared with the baseline on the same candles (the dashboard alert relies on this).

## `considerably_better`

`src/strategy.rs:915`

```rust
pub fn considerably_better(mem: &Connection) -> Result<(Vec<Value>, Vec<String>)>
```

The dashboard's "considerably better strategy" alert, decided by code only. A variant qualifies when ALL hold on BOTH block cuts: status validated; measured on the same candles as the current baseline (not older than it); holdout return per block at least 1.0 point above the baseline's; holdout trade win rate recorded and not below the baseline's; noisy holdout positive. Returns (qualifying variants, validated variants still waiting for a re-test on current data).

## `tuning_stats`

`src/strategy.rs:959`

```rust
pub fn tuning_stats(data: &Connection, capital: f64) -> Result<TuningStats>
```

_No comment in the code; the signature above is the description._

## `tuning_facts`

`src/strategy.rs:981`

```rust
pub fn tuning_facts(data: &Connection, capital: f64) -> Result<String>
```

_No comment in the code; the signature above is the description._

## `tuning_ranges`

`src/strategy.rs:996`

```rust
pub fn tuning_ranges(data: &Connection, capital: f64) -> Result<Vec<(&'static str, f64, f64)>>
```

Tuned search ranges chosen by code (no model): where the most profitable runs set the margin / DCA depth (their 25%-75% band), kept only when that band clearly differs from all runs (median shifted by more than a quarter of the all-runs spread). Returns (what, lo, hi).

## `band_if_different`

`src/strategy.rs:1006`

```rust
pub fn band_if_different(top: &[f64], all: &[f64], max: f64) -> Option<(f64, f64)>
```

The top runs' 25%-75% band (rounded to 0.1, inside 0..=max) when their median differs clearly from all runs; None otherwise.

## `training_spent`

`src/strategy.rs:1025`

```rust
fn training_spent(five: &[&Value], opt: &str) -> bool
```

Tried in at least SPENT_MIN_TRIED 5h variants without one beating the current strategy on the TRAINING half (h1)? Then more combinations of it are not worth the test budget and the planner stops pairing it.

## `entry_gating`

`src/strategy.rs:1041`

```rust
pub fn entry_gating(mem: &Connection, half: crate::quant::Half) -> Result<(String, Vec<(String, String)>)>
```

Does entry gating need improving, and how (owner 2026-10-11: "it needs to learn if entry gating needs to be improved")? Computed by code from the results and the win/lose analysis: for every gate option, how its tested variants did against the current strategy on unseen cut-A blocks (count, beat it, average and best gain, why the best is not live); for every CONFIRMED entry finding, the options that act on it and whether they are tested; a verdict; and the untested tests that would apply a confirmed finding (the planner runs them first; the agents see them). Returns (text, suggested tests). `half`: Unseen for people (the dashboard, Claude), Training for the brain. On 2026-10-12 this table gave the agents the unseen-half gains (e.g. "best gate result ... +2.03"), against the training-half-only rule; the statuses and verdict reasons (no numbers) are the same in both views.

## `planner`

`src/strategy.rs:1133`

```rust
fn planner(mem: &Connection, cfg: &crate::config::StrategyConfig, n: usize) -> Result<Vec<(String, String)>>
```

The planner's picks for this round (greedy forward selection, code not model): every option alone first (priority options first), then pairs of the best singles. Ranking uses only what the model may see too: the TRAINING half (h1), the noisy training half and the training-half win rate, never the holdout, so choosing what to test cannot leak into the verdict. Singles clearly worse than the baseline on the training half are not paired. Nothing already measured is repeated.

## `run_jobs`

`src/strategy.rs:1173`

```rust
fn run_jobs(cfg: &crate::config::StrategyConfig, jobs: Vec<(String, String, Option<&'static str>, f64)>) -> Vec<Result<String>>
```

Run several (interval, names, offsets) jobs, at most `max_parallel` at a time.

## `fallback`

`src/strategy.rs:1193`

```rust
fn fallback(mem: &Connection, cfg: &crate::config::StrategyConfig, want: usize, attempt: u64) -> Result<Vec<(String, String)>>
```

Untried experiments chosen by code, so exploration never stalls when the model repeats itself: single options first, then pairs of the options that did best on the training half.

## `next_proposal_attempt`

`src/strategy.rs:1232`

```rust
pub fn next_proposal_attempt(c: &Connection) -> Result<i64>
```

A counter that rises on every round (it varies which untested variants the planner picks first).

## `round`

`src/strategy.rs:1238`

```rust
pub fn round(ctx: &Ctx, attempt: u64) -> Result<Vec<String>>
```

One round: queued requests, re-tests, the interval sweep and the planner pick variants, the bot-side tool scores, winners are re-checked on differently cut blocks.

## `verdict_rule`

`src/strategy.rs:1429`

```rust
pub fn verdict_rule(r: &Row, b: &Row, rule: BlockRule) -> (bool, String)
```

`verdict` with the block-loss rule swapped for `rule` (every other criterion unchanged).

## `rule_study`

`src/strategy.rs:1442`

```rust
pub fn rule_study(ctx: &Ctx, run: bool) -> Result<String>
```

TEST of the block-loss rule (owner 2026-10-12: "you need to test and validate not just change"): every 5h variant at 5,000 USD judged under both rules on every block cut. With `run`, the variants that pass cut A under the relative rule get the missing cuts (B, C, D, E) backtested first, so each candidate has the full 5-cut validation. Changes no rule and no status.

## `full_validation`

`src/strategy.rs:1510`

```rust
pub fn full_validation(runs: &[RunRow], iv: &str, eq: f64, tokens: &str) -> Value
```

A variant's verdict on every block cut (A, B and the extra cuts C, D, E), each against the current strategy on the same blocks: {"passed", "measured", "complete" (all five measured), "robust" (complete and at most one cut failed), "cuts": [...]}.

## `improve_forever`

`src/strategy.rs:1525`

```rust
pub fn improve_forever(ctx: &Ctx) -> Result<()>
```

Run forever on the existing data: round after round, resumable (everything is in the database), never faster than `pause_secs`.

## `leaderboard`

`src/strategy.rs:1593`

```rust
pub fn leaderboard(mem: &Connection) -> Result<Vec<Value>>
```

Every tested variant with both block cuts, its verdict against the baseline and its status (validated = passed cut A and cut B; candidate = passed cut A, cut B not run yet), best first: validated, then candidates, then the rest by holdout return.

## `winners`

`src/strategy.rs:1617`

```rust
pub fn winners(mem: &Connection) -> Result<Vec<String>>
```

Every stored result that passes the verdict, with its status, for the report.

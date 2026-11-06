# `lab/src/brain.rs`

> The lab's own small model (owner 2026-10-13: "it must be narrow and specific to this bot, trading and learning ... and it can use machine
> learning too"). A Gaussian-process surrogate over OPTION SETS: it learns from every variant the lab has already tested which option
> combinations tend to have a high training-half edge, says how sure it is, and proposes the untested valid combination with the best
> optimistic estimate (upper confidence bound) -- standard Bayesian optimisation over a small discrete space.
>
> Why a Gaussian process and not a neural network: the lab has only a few hundred tested variants (~240 at 5h). A network would memorise
> them; a GP with a combinatorial kernel is the right size, gives an honest uncertainty, and trains in milliseconds on the CPU.
>
> Hard rules kept: it learns from the TRAINING half only (`h1`), never the holdout the lab uses to judge variants; its requests go through
> the normal validated queue, which still tests them on both cuts; its `why` text contains no numbers it was not shown.
>
> ROLES (what the Hermes agents did, done by code + this model; see docs/BRAIN.md for the parity table): `cycle` is the strategist's job -- pick
> untested variants and queue them through the same validated queue (`lab_queue_test`, author `brain-strategist`), so the lab's tests, scoreboard and
> checks treat them exactly like Hermes requests. Three policies take turns, least-used first, and every proposal is logged with its policy: an
> optimistic-bonus rule (UCB), the additive average (the one model that learns), and a RANDOM control. `report` compares them (and Hermes) on real outcomes, so the model is only
> trusted if it beats chance.
>
> Kernel: k(A,B) = a*|A∩B| + b*|fam(A)∩fam(B)| + c*[A==B and |A|==2] + d     (options shared, exclusive families shared, identical pair, bias)

27 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `family`

`lab/src/brain.rs:32`

```rust
fn family(t: &str) -> Option<usize>
```

_No comment in the code; the signature above is the description._

## `fams`

`lab/src/brain.rs:33`

```rust
fn fams(s: &BTreeSet<String>) -> BTreeSet<usize>
```

_No comment in the code; the signature above is the description._

## `kernel`

`lab/src/brain.rs:35`

```rust
pub fn kernel(h: &Hyper, x: &BTreeSet<String>, z: &BTreeSet<String>) -> f64
```

_No comment in the code; the signature above is the description._

## `cholesky`

`lab/src/brain.rs:43`

```rust
pub fn cholesky(a: &[f64], n: usize) -> Option<Vec<f64>>
```

Lower-triangular Cholesky factor of a symmetric positive-definite matrix (row-major n x n); None if it is not positive definite.

## `solve_lower`

`lab/src/brain.rs:54`

```rust
fn solve_lower(l: &[f64], n: usize, b: &[f64]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `solve_upper_t`

`lab/src/brain.rs:59`

```rust
fn solve_upper_t(l: &[f64], n: usize, b: &[f64]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `fit`

`lab/src/brain.rs:69`  (method / nested)

```rust
pub fn fit(obs: &[Obs], h: Hyper) -> Option<Model>
```

_No comment in the code; the signature above is the description._

## `predict`

`lab/src/brain.rs:85`  (method / nested)

```rust
pub fn predict(&self, x: &BTreeSet<String>) -> (f64, f64)
```

(predicted gain, standard deviation) for a set of options, in the original units.

## `tune`

`lab/src/brain.rs:96`

```rust
pub fn tune(obs: &[Obs]) -> Hyper
```

Picks hyper-parameters by coordinate search on the log marginal likelihood (training data only).

## `ranks`

`lab/src/brain.rs:114`

```rust
fn ranks(v: &[f64]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `spearman`

`lab/src/brain.rs:128`

```rust
pub fn spearman(a: &[f64], b: &[f64]) -> f64
```

Spearman rank correlation (average ranks for ties); 0 for fewer than 3 points or no spread.

## `additive_predict`

`lab/src/brain.rs:139`

```rust
pub fn additive_predict(train: &[Obs], x: &BTreeSet<String>) -> f64
```

The baseline the surrogate must beat: the plain additive rule "average training gain of the earlier variants that used each option".

## `walk_forward`

`lab/src/brain.rs:153`

```rust
pub fn walk_forward(obs: &[Obs], start: usize, step: usize) -> WalkForward
```

Walk-forward test in time order: fit on the first `t` variants, predict the next `step`, slide. Reports rank correlation and the mean actual gain of the top quarter by prediction, for the surrogate and for the additive baseline, against the plain average (no skill).

## `load`

`lab/src/brain.rs:170`

```rust
pub fn load(mem: &Connection) -> Result<Vec<Obs>>
```

Every 5h variant at cut A, oldest first, with its training-half gain per block (`h1`). Only the regime the live strategy currently runs.

## `candidates`

`lab/src/brain.rs:179`

```rust
pub fn candidates() -> Vec<BTreeSet<String>>
```

Every valid 1- and 2-option combination from the vocabulary.

## `propose`

`lab/src/brain.rs:194`

```rust
pub fn propose(obs: &[Obs], tested: &BTreeSet<String>, n: usize, kappa: f64) -> Vec<(BTreeSet<String>, f64, f64)>
```

The `n` untested valid combinations with the best upper confidence bound (mean + kappa*sd), at most one per leading option so the batch is varied.

## `report_eval`

`lab/src/brain.rs:210`

```rust
pub fn report_eval(obs: &[Obs]) -> String
```

Human-readable walk-forward report on the lab's real history (training-half targets only).

## `init_tables`

`lab/src/brain.rs:227`

```rust
pub fn init_tables(mem: &Connection) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `ucb_score`

`lab/src/brain.rs:233`

```rust
pub fn ucb_score(train: &[Obs], x: &BTreeSet<String>, kappa: f64) -> (f64, f64)
```

Optimistic-bonus rule: the additive average of past training gains plus a bonus that is larger for options tested few times.

## `xorshift`

`lab/src/brain.rs:241`

```rust
fn xorshift(state: &mut u64) -> u64
```

_No comment in the code; the signature above is the description._

## `pick`

`lab/src/brain.rs:244`

```rust
pub fn pick(policy: &str, obs: &[Obs], avoid: &BTreeSet<String>, n: usize, seed: u64) -> Vec<(BTreeSet<String>, f64, f64)>
```

The next `n` variants under one policy, never one in `avoid` (tested or already queued); at most one per leading option so a batch is varied.

## `pick_heuristic`

`lab/src/brain.rs:267`

```rust
pub fn pick_heuristic(mem: &Connection, avoid: &BTreeSet<String>, n: usize) -> Vec<(BTreeSet<String>, f64, f64)>
```

The lab's own ranked list of untested variants (`lab_untested`, the list the Hermes strategist picks from), top first, one per leading option.

## `pick_build`

`lab/src/brain.rs:283`

```rust
pub fn pick_build(mem: &Connection, obs: &[Obs], avoid: &BTreeSet<String>, n: usize) -> Vec<(BTreeSet<String>, f64, f64)>
```

Policy `build` (the mechanism that made the old Hermes strategist better than chance, found by replaying the real history in time order, `examples/brain_build.rs`: variants containing an option of an EARLIER validated/candidate variant passed the holdout 8 of 28 = 29%, the rest 15 of 168 = 9%). Untested valid combinations that contain such an option, best average past training gain first, one per leading option. Empty until something validates.

## `next_policy`

`lab/src/brain.rs:299`

```rust
pub fn next_policy(mem: &Connection) -> Result<&'static str>
```

The policy that has proposed least so far (ties in `POLICIES` order), so the three arms stay balanced.

## `why_text`

`lab/src/brain.rs:312`

```rust
fn why_text(policy: &str) -> &'static str
```

_No comment in the code; the signature above is the description._

## `cycle`

`lab/src/brain.rs:323`

```rust
pub fn cycle(ctx: &crate::context::Ctx, n: usize) -> Result<Vec<String>>
```

The strategist's job: queue up to `n` untested variants under the next policy. Returns one line per request.

## `report`

`lab/src/brain.rs:352`

```rust
pub fn report(mem: &Connection) -> Result<String>
```

Head-to-head table on real outcomes: per brain policy, how many requests were tested and how many passed the lab's verdict.

# `lab/src/automl.rs`

> The self-improving trainer (owner 2026-10-13: "the model needs to train 24x7", "it needs to self improve 24x7"). A worker that never stops searches for better deep
> architectures with an evolutionary loop (the genetic-algorithm idea of *The Nature of Code*, applied to the model itself): a population of genomes (architecture +
> training recipe) is kept in SQLite; each turn the worker picks a parent from the best ones (tournament), mutates or crosses it, trains and evaluates the child on real
> out-of-sample windows, records the result in the trial ledger (paired against the ridge, adjusted for the number of trials), and keeps the best. A genome is a CHAMPION
> only when its paired gain over the ridge is above zero at the adjusted level; the champion is then retrained on all data and saved for live blending. It learns from the
> historical candles, the indicators computed from them, and (via `mlmeta`) the bot's backtest trades. Resource-guarded and niced; stops on the plain switch files.

34 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `new`

`lab/src/automl.rs:20`  (method / nested)

```rust
pub fn new(seed: u64) -> Rng
```

_No comment in the code; the signature above is the description._

## `bits`

`lab/src/automl.rs:21`  (method / nested)

```rust
pub fn bits(&mut self) -> u64
```

_No comment in the code; the signature above is the description._

## `unit`

`lab/src/automl.rs:22`  (method / nested)

```rust
pub fn unit(&mut self) -> f64
```

_No comment in the code; the signature above is the description._

## `pick`

`lab/src/automl.rs:23`  (method / nested)

```rust
fn pick<T: Copy>(&mut self, v: &[T]) -> T
```

_No comment in the code; the signature above is the description._

## `log_uniform`

`lab/src/automl.rs:24`  (method / nested)

```rust
fn log_uniform(&mut self, lo: f64, hi: f64) -> f64
```

_No comment in the code; the signature above is the description._

## `clamp_genome`

`lab/src/automl.rs:28`

```rust
pub fn clamp_genome(mut g: Genome) -> Genome
```

Keeps a genome inside what the CPU can train: valid head counts, bounded Transformer size.

## `random_genome`

`lab/src/automl.rs:39`

```rust
fn random_genome(r: &mut Rng) -> Genome
```

_No comment in the code; the signature above is the description._

## `mutate`

`lab/src/automl.rs:46`

```rust
pub fn mutate(g: &Genome, r: &mut Rng) -> Genome
```

Changes 1-3 genes (the mutation step of the genetic algorithm).

## `crossover`

`lab/src/automl.rs:65`

```rust
pub fn crossover(a: &Genome, b: &Genome, r: &mut Rng) -> Genome
```

Crossover: each gene from one of two parents.

## `init`

`lab/src/automl.rs:74`

```rust
pub fn init(db: &Connection) -> rusqlite::Result<()>
```

Notes from the code: ------------------------------------------------------------------ the population (everything on disk)

## `next_genome`

`lab/src/automl.rs:79`

```rust
pub fn next_genome(db: &Connection, seed: u64) -> Result<(Genome, Option<i64>)>
```

_No comment in the code; the signature above is the description._

## `load_rows`

`lab/src/automl.rs:102`

```rust
pub fn load_rows(candle_db: &Path) -> Result<(Vec<Row>, i64)>
```

The rows of the bot's top-20 universe (hourly), with the static features and a window of indicator channels, oldest first, and the newest bar's time.

Notes from the code: ------------------------------------------------------------------ data and evaluation

## `with_seq_len`

`lab/src/automl.rs:113`

```rust
fn with_seq_len(r: &Row, l: usize) -> Row
```

_No comment in the code; the signature above is the description._

## `evaluate`

`lab/src/automl.rs:123`

```rust
pub fn evaluate(rows: &[Row], g: &Genome, windows: usize, trials: usize, threads: usize, seed: u64) -> Option<Eval>
```

Rolling out-of-sample test: the last `windows` blocks of 28 days, each predicted by a model trained only on data at least 3 days older. The deep model is compared with the ridge on the same windows (paired, week-resampled, widened for `trials`).

## `bag_dir`

`lab/src/automl.rs:151`

```rust
fn bag_dir(root: &Path) -> std::path::PathBuf
```

_No comment in the code; the signature above is the description._

## `staging_dir`

`lab/src/automl.rs:152`

```rust
fn staging_dir(root: &Path) -> std::path::PathBuf
```

_No comment in the code; the signature above is the description._

## `meta_path`

`lab/src/automl.rs:153`

```rust
fn meta_path(root: &Path) -> std::path::PathBuf
```

_No comment in the code; the signature above is the description._

## `write_atomic`

`lab/src/automl.rs:159`

```rust
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `indices`

`lab/src/automl.rs:167`

```rust
fn indices(dir: &Path) -> Vec<usize>
```

The numeric file indices present in a bag directory, ascending.

## `dir_models`

`lab/src/automl.rs:172`

```rust
fn dir_models(dir: &Path) -> Vec<DeepModel>
```

_No comment in the code; the signature above is the description._

## `add_to_dir`

`lab/src/automl.rs:173`

```rust
fn add_to_dir(dir: &Path, model: &DeepModel) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `bag_size`

`lab/src/automl.rs:175`

```rust
pub fn bag_size(root: &Path) -> usize
```

_No comment in the code; the signature above is the description._

## `add_to_bag`

`lab/src/automl.rs:177`

```rust
pub fn add_to_bag(root: &Path, model: &DeepModel, trained_through_ms: i64) -> Result<()>
```

Adds a model to the LIVE bag (the first build of a champion, or growing it on the existing data).

## `clear_champion`

`lab/src/automl.rs:181`

```rust
pub fn clear_champion(root: &Path)
```

_No comment in the code; the signature above is the description._

## `load_champions`

`lab/src/automl.rs:183`

```rust
pub fn load_champions(root: &Path) -> Vec<DeepModel>
```

All readable models of the live champion bag (empty when there is no champion); a truncated or foreign file is skipped.

## `champion_trained_through`

`lab/src/automl.rs:184`

```rust
fn champion_trained_through(root: &Path) -> Option<i64>
```

_No comment in the code; the signature above is the description._

## `grow_staging`

`lab/src/automl.rs:187`

```rust
fn grow_staging(root: &Path, model: &DeepModel, trained_through_ms: i64) -> Result<bool>
```

Adds one seed to the staging bag; when it is complete it replaces the live bag in one step and the meta moves to the new data. Returns true when the swap happened.

## `tend_champion`

`lab/src/automl.rs:201`

```rust
fn tend_champion(db: &Connection, root: &Path, w: &Worker, threads: usize) -> Result<Option<String>>
```

Keeps the champion alive: retrains it on all data as soon as a new session of bars has arrived, and every hour re-tests its genome on fresh out-of-sample windows; if the paired gain over the ridge is no longer above zero at the adjusted level it is DEMOTED (the file is removed, the live ranking goes back to the ridge alone).

## `load_too_high`

`lab/src/automl.rs:244`

```rust
pub fn load_too_high(load: f64, cores: f64) -> bool
```

The trainer stands down above 1.5 load per core (it is also niced to the lowest priority and uses fewer threads while the US market is open).

## `threads_now`

`lab/src/automl.rs:247`

```rust
pub fn threads_now(configured: usize) -> usize
```

Threads for the next step: two while the US market is open (13:00-21:00 UTC) so the live bot keeps its headroom, the configured number otherwise.

## `blocked`

`lab/src/automl.rs:250`

```rust
pub fn blocked(root: &Path) -> Option<String>
```

Why the trainer should not run right now (None = go). The trainer is for the quiet hours of the machine, never at the cost of the bot or the Mac.

## `step`

`lab/src/automl.rs:261`

```rust
pub fn step(candle_db: &Path, root: &Path, w: &mut Option<Worker>, threads: usize) -> Result<String>
```

One iteration: refresh the data if there are newer bars, pick the next genome, train and evaluate it, record it, and keep the champion. Returns a report line.

## `run_forever`

`lab/src/automl.rs:295`

```rust
pub fn run_forever(candle_db: &Path, root: &Path, logs: &Path, threads: usize) -> !
```

The worker: forever, one genome after another, whatever the hour; it only pauses for the switch files and an overloaded machine (re-checked every minute). Everything it learns is on disk (the population table, the trial ledger, the champion file), so a restart loses nothing.

## `status`

`lab/src/automl.rs:322`

```rust
pub fn status(db: &Connection) -> Result<String>
```

The population's state as text: how many genomes tried, the best by paired gain over the ridge, the champion.

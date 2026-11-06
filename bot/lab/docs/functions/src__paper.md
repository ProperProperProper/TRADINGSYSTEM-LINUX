# `src/paper.rs`

> The paper-trading archive as a forward record (spec 4.3). The bot appends everything its PAPER account does to a
> separate SQLite file; this module reads it strictly read-only, copies it incrementally into `data.sqlite3`, and
> tells the rest of the lab how much to trust it. The archive can be empty or thin, so every use goes through a
> sufficiency ladder (absent / empty / thin / usable / rich).
>
> Amounts: both the absolute paper amounts (USDT, exactly as the bot recorded them) and the same figures as percent of
> each run's starting equity are stored, at the user's instruction (2026-10-05). Credentials are never stored, and
> text fields still pass the privacy scrub.

14 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `counter`

`src/paper.rs:41`

```rust
pub(crate) fn counter(mem: &Connection, name: &str) -> Result<Option<i64>>
```

Reads a named counter from the memory database (None if never set).

## `set_counter`

`src/paper.rs:46`

```rust
pub(crate) fn set_counter(mem: &Connection, name: &str, v: i64) -> Result<()>
```

Sets a named counter in the memory database.

## `columns`

`src/paper.rs:52`

```rust
fn columns(c: &Connection, table: &str) -> Result<Vec<String>>
```

Column names of `table`.

## `check_schema`

`src/paper.rs:68`

```rust
fn check_schema(c: &Connection) -> std::result::Result<(), String>
```

Checks the paper archive has every required table and column; returns what is missing.

## `clean`

`src/paper.rs:80`

```rust
pub(crate) fn clean(text: &str) -> Result<String>
```

Cleans a text field from the archive (privacy scrub, no control characters, at most 200 characters).

## `scrub_json`

`src/paper.rs:87`

```rust
pub(crate) fn scrub_json(json: &str) -> Result<()>
```

Refuses JSON that contains a forbidden field.

## `pct`

`src/paper.rs:92`

```rust
pub(crate) fn pct(x: f64, base: Option<f64>) -> Option<f64>
```

`x` as a percentage of `base` (None without a positive base).

## `import`

`src/paper.rs:95`

```rust
pub fn import(cfg: &Config, mem: &Connection, data: &Connection, now_ms: i64) -> Result<PaperReport>
```

Imports the bot's paper-trading archive (read-only); routes to the moomoo importer for stock labs.

## `current`

`src/paper.rs:226`

```rust
pub fn current(cfg: &Config, data: &Connection, now_ms: i64) -> Result<PaperReport>
```

Current counts and sufficiency state from what the lab already holds (read-only; no import).

## `finish`

`src/paper.rs:231`

```rust
pub(crate) fn finish(cfg: &Config, data: &Connection, mut rep: PaperReport, now_ms: i64) -> Result<PaperReport>
```

Fill in counts and the sufficiency state from what the lab holds (so a missing source still reports honestly).

## `mean_ci`

`src/paper.rs:246`

```rust
fn mean_ci(values: &[(i64, f64)], n_boot: usize, seed: u64) -> (f64, f64, f64)
```

Mean of values and a day-clustered bootstrap 95% interval (NaN interval with fewer than 5 days).

## `prompt_block`

`src/paper.rs:265`

```rust
pub fn prompt_block(cfg: &Config, data: &Connection, rep: &PaperReport) -> Result<String>
```

The PAPER RECORD block placed in model prompts: built from aggregates only, never raw rows or account amounts.

## `search_decay`

`src/paper.rs:313`

```rust
pub fn search_decay(data: &Connection, min_pairs: usize, n_perm: usize) -> Result<SearchDecay>
```

Does the in-sample score of the settings a search chose predict the forward paper return until the next search? Needs at least `min_pairs` (search, forward-return) pairs; otherwise it says so and stops.

## `import_best_effort`

`src/paper.rs:344`

```rust
pub fn import_best_effort(cfg: &Config, mem: &Connection, data: &Connection) -> PaperReport
```

Convenience for callers that must never fail a research cycle because of the archive.

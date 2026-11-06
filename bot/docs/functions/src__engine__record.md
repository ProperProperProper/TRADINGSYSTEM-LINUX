# `src/engine/record.rs`

> Persistent record of the best parameters the search has found (user request 2026-10-04).
>
> The market cache and every other runtime file are wiped on each start/deploy (fresh-data rule),
> but PARAMETERS are the product of the search, not market history: the top settings of every
> search are kept in their own SQLite file outside the runtime folder so that (1) the most recent
> best settings can still be used when a search finds nothing usable, (2) every recorded setting is
> re-tested on each new search window as a candidate, and (3) each recorded setting accumulates a
> track record on every window it has been tested on. Only parameters and their test results are
> stored: no market data, no account data.

8 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `open`

`src/engine/record.rs:41`  (method / nested)

```rust
pub fn open(path: &Path) -> Result<Self>
```

Opens (or creates) a parameter record database.

## `id_of`

`src/engine/record.rs:72`  (method / nested)

```rust
fn id_of(&self, p: &XsParams, now_ms: i64, score: f64) -> Result<i64>
```

Id of settings `p` in the record, inserting them or updating last-seen time and best score.

## `save_top`

`src/engine/record.rs:85`  (method / nested)

```rust
pub fn save_top( &self, top: &[(XsParams, f64)], chosen: Option<&XsParams>, now_ms: i64, ) -> Result<()>
```

Record a search's top settings (with their robust scores) and the one chosen.

## `load_params`

`src/engine/record.rs:109`  (method / nested)

```rust
pub fn load_params(&self, limit: usize) -> Result<Vec<XsParams>>
```

Recorded settings, best first (by best score, then most recently seen), at most `limit`.

## `last_chosen`

`src/engine/record.rs:125`  (method / nested)

```rust
pub fn last_chosen(&self) -> Result<Option<XsParams>>
```

The most recently chosen settings: the "recent best" to use when nothing else is available.

## `add_eval`

`src/engine/record.rs:139`  (method / nested)

```rust
pub fn add_eval( &self, p: &XsParams, now_ms: i64, label: &str, return_pct: f64, max_drawdown_pct: f64, trades: usize, liquidations: usize, ) -> Result<()>
```

Store one test result of a recorded setting on a window.

## `count`

`src/engine/record.rs:163`  (method / nested)

```rust
pub fn count(&self) -> Result<i64>
```

Number of distinct settings in the record.

## `summary`

`src/engine/record.rs:170`  (method / nested)

```rust
pub fn summary(&self, limit: usize) -> Result<Vec<RecordRow>>
```

The best-tracked settings (by mean return over their stored evaluations), at most `limit`.

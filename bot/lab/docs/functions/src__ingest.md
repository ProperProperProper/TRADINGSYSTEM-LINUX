# `src/ingest.rs`

> Ingest: manifest-driven, hash-verified, idempotent loading of export files into `data.sqlite3`.
> Bad runs are quarantined with a reason; nothing is ever overwritten silently.

5 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `ingest`

`src/ingest.rs:30`

```rust
pub fn ingest(cfg: &Config, mem: &Connection, data: &mut Connection) -> Result<IngestReport>
```

Imports every complete run in the exports folder (header, trades, daily), checking hashes and the privacy scrub; bad runs are quarantined.

## `run_id_of`

`src/ingest.rs:77`

```rust
fn run_id_of(file: &str) -> Option<String>
```

Run id from an export file name (run_<id>_trades.csv, _daily.csv or .json).

## `log`

`src/ingest.rs:84`

```rust
fn log(mem: &Connection, sha: &str, file: &str, run_id: &str, status: &str, reason: Option<&str>) -> Result<()>
```

Records one file's ingest outcome (status and reason) in the memory log.

## `quarantine`

`src/ingest.rs:93`

```rust
fn quarantine(cfg: &Config, dir: &Path, run_id: &str, files: &[&ManifestEntry], reason: &str) -> Result<()>
```

Moves a run's files into quarantine/<run_id> with the reason.

## `load_run`

`src/ingest.rs:106`

```rust
fn load_run(cfg: &Config, data: &mut Connection, run_id: &str, h: &ManifestEntry, t: &ManifestEntry, d: &ManifestEntry, dir: &Path, batch: &mut Option<i64>, batch_ts: i64) -> Result<usize>
```

Loads one run into the data database after checking every file's sha256 and scrubbing for forbidden fields.

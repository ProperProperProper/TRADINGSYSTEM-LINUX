# `lab/src/config.rs`

> `lab.toml`: every threshold and budget lives here and is snapshotted into the memory database per run.

21 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `default`

`lab/src/config.rs:48`  (method / nested)

```rust
fn default() -> Self
```

Default strategy-loop settings: the bot's variant_study binary, a 5,000 USD capital, all research intervals, 4 variants per round.

## `default`

`lab/src/config.rs:72`  (method / nested)

```rust
fn default() -> Self
```

Default dashboard port 8790.

## `default`

`lab/src/config.rs:89`  (method / nested)

```rust
fn default() -> Self
```

Default schedule: a full tick at 03:00 at most every 20 hours, with load and memory guards.

## `default`

`lab/src/config.rs:102`  (method / nested)

```rust
fn default() -> Self
```

Default backtest-archive path: the SPY bot's archive.

## `default`

`lab/src/config.rs:125`  (method / nested)

```rust
fn default() -> Self
```

Default paper-archive settings: the SPY bot's store (moomoo, USD).

## `default`

`lab/src/config.rs:160`  (method / nested)

```rust
fn default() -> Self
```

Default lab configuration rooted at ~/Projects/qwen-lab in real mode.

## `default`

`lab/src/config.rs:177`  (method / nested)

```rust
fn default() -> Self
```

Default database size limits: cull data above 10 GB down to 8 GB, alert on memory above 2 GB.

## `default`

`lab/src/config.rs:189`  (method / nested)

```rust
fn default() -> Self
```

Default windows: 7-day blocks, three needed.

## `dirs_home`

`lab/src/config.rs:193`

```rust
fn dirs_home() -> PathBuf
```

The user's home folder (current folder if HOME is unset).

## `load`

`lab/src/config.rs:199`  (method / nested)

```rust
pub fn load(path: &Path) -> Result<Self>
```

Loads lab.toml; refuses anything but real mode and a cull target below the limit.

## `for_root`

`lab/src/config.rs:210`  (method / nested)

```rust
pub fn for_root(root: &Path, mode: &str) -> Self
```

Configuration rooted at `root` in `mode`, everything else default.

## `exports_dir`

`lab/src/config.rs:215`  (method / nested)

```rust
pub fn exports_dir(&self) -> PathBuf
```

Folder the bot's exports are read from.

## `quarantine_dir`

`lab/src/config.rs:217`  (method / nested)

```rust
pub fn quarantine_dir(&self) -> PathBuf
```

Folder where rejected imports are kept with their reason.

## `requests_dir`

`lab/src/config.rs:219`  (method / nested)

```rust
pub fn requests_dir(&self) -> PathBuf
```

Folder for requests to the bot session.

## `reports_dir`

`lab/src/config.rs:221`  (method / nested)

```rust
pub fn reports_dir(&self) -> PathBuf
```

Folder for daily reports.

## `logs_dir`

`lab/src/config.rs:223`  (method / nested)

```rust
pub fn logs_dir(&self) -> PathBuf
```

Folder for logs.

## `data_db`

`lab/src/config.rs:225`  (method / nested)

```rust
pub fn data_db(&self) -> PathBuf
```

Path of the data database (imported runs and trades).

## `candles_db`

`lab/src/config.rs:226`  (method / nested)

```rust
pub fn candles_db(&self) -> PathBuf
```

_No comment in the code; the signature above is the description._

## `memory_db`

`lab/src/config.rs:228`  (method / nested)

```rust
pub fn memory_db(&self) -> PathBuf
```

Path of the memory database (persistent learning).

## `backup_dir`

`lab/src/config.rs:230`  (method / nested)

```rust
pub fn backup_dir(&self) -> PathBuf
```

Folder for memory backups.

## `stop_file`

`lab/src/config.rs:232`  (method / nested)

```rust
pub fn stop_file(&self) -> PathBuf
```

Path of the kill-switch file: while it exists, cycles refuse to run.

# `src/schedule.rs`

> The launchd job that runs the learning tick on a schedule (spec 21.7). It runs a stable, ad-hoc-signed copy of the lab
> binary at low priority every hour (`tick --auto` decides light or full), logs under the lab folder, and talks only to
> localhost. Removing it is `schedule uninstall`, or the STOP file, or `launchctl bootout`.

22 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `plist_path`

`src/schedule.rs:14`

```rust
pub fn plist_path() -> PathBuf
```

Path of the tick job's launchd plist.

## `esc`

`src/schedule.rs:19`

```rust
fn esc(s: &str) -> String
```

Escapes text for an XML plist.

## `plist`

`src/schedule.rs:22`

```rust
pub fn plist(cfg: &Config, binary: &Path, config_path: &Path) -> String
```

The tick job's launchd plist (hourly `tick --auto`).

## `uid`

`src/schedule.rs:48`

```rust
fn uid() -> Result<String>
```

The current user's numeric id.

## `install_binary`

`src/schedule.rs:54`

```rust
fn install_binary(cfg: &Config) -> Result<(PathBuf, bool)>
```

Copy the running binary to the stable path (atomically, so a job that is running right now is not disturbed) and re-sign it.

## `dash_plist_path`

`src/schedule.rs:72`

```rust
pub fn dash_plist_path() -> PathBuf
```

Path of the dashboard job's launchd plist.

## `dash_plist`

`src/schedule.rs:77`

```rust
pub fn dash_plist(cfg: &Config, binary: &Path, config_path: &Path) -> String
```

The dashboard job's launchd plist (always on).

## `install_dash`

`src/schedule.rs:102`

```rust
pub fn install_dash(cfg: &Config, config_path: &Path) -> Result<String>
```

Install the read-only monitoring page as a background service (localhost only, always on).

## `uninstall_dash`

`src/schedule.rs:114`

```rust
pub fn uninstall_dash() -> Result<String>
```

Stops and removes the dashboard job.

## `dash_status`

`src/schedule.rs:123`

```rust
pub fn dash_status() -> String
```

Whether the dashboard job is loaded.

## `install`

`src/schedule.rs:131`

```rust
pub fn install(cfg: &Config, config_path: &Path) -> Result<String>
```

Installs the binary copy and the hourly tick job, and loads it.

## `uninstall`

`src/schedule.rs:144`

```rust
pub fn uninstall() -> Result<String>
```

Stops and removes the tick job.

## `status`

`src/schedule.rs:154`

```rust
pub fn status() -> String
```

The tick job's state, run count and last exit code.

## `bootstrap`

`src/schedule.rs:168`

```rust
fn bootstrap(domain: &str, path: &std::path::Path) -> Result<()>
```

`launchctl bootstrap`, retried: right after a bootout launchd can refuse with "Input/output error" (error 5) until the old job is gone (seen 2026-10-10, which left the dashboard down).

## `brain_plist_path`

`src/schedule.rs:184`

```rust
pub fn brain_plist_path() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `brain_plist`

`src/schedule.rs:189`

```rust
pub fn brain_plist(cfg: &Config, binary: &Path, config_path: &Path) -> String
```

The always-on brain: one role of the team every few minutes, in rotation (see `brain_roles::serve`). Low priority, restarts if it exits.

## `install_brain`

`src/schedule.rs:216`

```rust
pub fn install_brain(cfg: &Config, config_path: &Path) -> Result<String>
```

Installs and starts the brain server (re-registers the job so launchd accepts the freshly signed binary).

## `uninstall_brain`

`src/schedule.rs:228`

```rust
pub fn uninstall_brain() -> Result<String>
```

Stops and removes the brain server job.

## `trainer_plist_path`

`src/schedule.rs:235`

```rust
pub fn trainer_plist_path() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `trainer_plist`

`src/schedule.rs:240`

```rust
pub fn trainer_plist(cfg: &Config, binary: &Path, config_path: &Path) -> String
```

The deep-learning trainer, always on (`trainer`): trains and evaluates architectures around the clock at the lowest priority; restarts if it exits.

## `install_trainer`

`src/schedule.rs:267`

```rust
pub fn install_trainer(cfg: &Config, config_path: &Path) -> Result<String>
```

_No comment in the code; the signature above is the description._

## `uninstall_trainer`

`src/schedule.rs:277`

```rust
pub fn uninstall_trainer() -> Result<String>
```

_No comment in the code; the signature above is the description._

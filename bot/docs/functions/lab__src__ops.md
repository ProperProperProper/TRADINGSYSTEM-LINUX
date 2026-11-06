# `lab/src/ops.rs`

> Resource safety for the 16 GB Mac, in Rust (replaces the zsh scripts resource_watch/heal/watchdog/train_guard/health; owner 2026-10-13:
> "everything is rust only"). See docs/RESOURCE_SAFETY.md for the incident this exists for. The decisions are pure functions with unit
> tests; the commands around them shell out only to macOS tools (sysctl, launchctl, top, memory_pressure, pgrep, curl, nc, date, claude).
>
> Commands (`qwen-ops <cmd>`): `watch` (every 5 min: sample, alert, then self-heal), `watchdog` (every 10 min: starts one
> headless Claude fixer when a problem persists), `health` (one-shot report).

34 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `home`

`lab/src/ops.rs:16`

```rust
pub fn home() -> PathBuf
```

Notes from the code: ---------------------------------------------------------------- paths and small helpers

## `lab`

`lab/src/ops.rs:17`

```rust
pub fn lab() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `logs`

`lab/src/ops.rs:18`

```rust
fn logs() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `state`

`lab/src/ops.rs:19`

```rust
fn state() -> PathBuf
```

_No comment in the code; the signature above is the description._

## `uid`

`lab/src/ops.rs:20`

```rust
fn uid() -> String
```

_No comment in the code; the signature above is the description._

## `gui`

`lab/src/ops.rs:21`

```rust
fn gui() -> String
```

_No comment in the code; the signature above is the description._

## `epoch`

`lab/src/ops.rs:22`

```rust
fn epoch() -> u64
```

_No comment in the code; the signature above is the description._

## `out`

`lab/src/ops.rs:25`

```rust
pub fn out(cmd: &str, args: &[&str]) -> String
```

stdout of a command, trimmed; empty on any failure.

## `ok`

`lab/src/ops.rs:28`

```rust
fn ok(cmd: &str, args: &[&str]) -> bool
```

_No comment in the code; the signature above is the description._

## `date_fmt`

`lab/src/ops.rs:29`

```rust
fn date_fmt(fmt: &str, back: Option<&str>) -> String
```

_No comment in the code; the signature above is the description._

## `now_str`

`lab/src/ops.rs:32`

```rust
fn now_str() -> String
```

_No comment in the code; the signature above is the description._

## `append`

`lab/src/ops.rs:33`

```rust
fn append(path: &Path, line: &str)
```

_No comment in the code; the signature above is the description._

## `discord`

`lab/src/ops.rs:37`

```rust
pub fn discord(title: &str, body: &str)
```

Posts to Discord through the separately installed, already-approved binary (this one never touches the network itself).

## `parse_load`

`lab/src/ops.rs:44`

```rust
pub fn parse_load(s: &str) -> f64
```

`{ 2.38 2.27 2.30 }` -> 2.38

Notes from the code: ---------------------------------------------------------------- parsing (pure, tested)

## `parse_memfree`

`lab/src/ops.rs:46`

```rust
pub fn parse_memfree(s: &str) -> Option<u32>
```

`System-wide memory free percentage: 47%` -> 47

## `parse_swap_mb`

`lab/src/ops.rs:50`

```rust
pub fn parse_swap_mb(s: &str) -> u64
```

`total = 2048.00M  used = 1016.69M  free = 1031.31M` -> 1016 (MB used)

## `launchd_pid`

`lab/src/ops.rs:55`

```rust
pub fn launchd_pid(list: &str, label: &str) -> Option<String>
```

pid of a launchd job from `launchctl list` (`Some("-")` = loaded but not running, None = not loaded)

## `launchd_status`

`lab/src/ops.rs:60`

```rust
pub fn launchd_status(list: &str, label: &str) -> Option<i32>
```

last exit status of a launchd job from `launchctl list` (second column)

## `needs_reregister`

`lab/src/ops.rs:65`

```rust
pub fn needs_reregister(status: Option<i32>) -> bool
```

EX_CONFIG (78): launchd refuses to spawn the job. Seen after a binary was re-signed (stale launch constraints); a plain kickstart cannot fix it, the job must be unloaded and loaded again.

## `line`

`lab/src/ops.rs:73`  (method / nested)

```rust
pub fn line(&self, ts: &str) -> String
```

_No comment in the code; the signature above is the description._

## `alerts`

`lab/src/ops.rs:76`  (method / nested)

```rust
pub fn alerts(&self) -> Vec<String>
```

_No comment in the code; the signature above is the description._

## `since`

`lab/src/ops.rs:90`

```rust
fn since(key: &str) -> u64
```

Notes from the code: ---------------------------------------------------------------- state: "how long since I last did X"

## `mark`

`lab/src/ops.rs:91`

```rust
fn mark(key: &str)
```

_No comment in the code; the signature above is the description._

## `read_num`

`lab/src/ops.rs:92`

```rust
fn read_num(key: &str) -> Option<u64>
```

_No comment in the code; the signature above is the description._

## `write_num`

`lab/src/ops.rs:93`

```rust
fn write_num(key: &str, v: u64)
```

_No comment in the code; the signature above is the description._

## `act`

`lab/src/ops.rs:95`

```rust
fn act(msg: &str)
```

_No comment in the code; the signature above is the description._

## `kick`

`lab/src/ops.rs:99`

```rust
fn kick(label: &str)
```

_No comment in the code; the signature above is the description._

## `disk_free_gb`

`lab/src/ops.rs:101`

```rust
fn disk_free_gb() -> u64
```

_No comment in the code; the signature above is the description._

## `take_sample`

`lab/src/ops.rs:103`

```rust
pub fn take_sample() -> Sample
```

_No comment in the code; the signature above is the description._

## `trim_log`

`lab/src/ops.rs:112`

```rust
fn trim_log(p: &Path, keep: usize)
```

_No comment in the code; the signature above is the description._

## `watch`

`lab/src/ops.rs:117`

```rust
pub fn watch()
```

Notes from the code: ---------------------------------------------------------------- watch = sample + alerts + heal

## `heal`

`lab/src/ops.rs:127`

```rust
fn heal()
```

_No comment in the code; the signature above is the description._

## `watchdog`

`lab/src/ops.rs:156`

```rust
pub fn watchdog()
```

Notes from the code: ---------------------------------------------------------------- watchdog: free while healthy; one Claude fixer when trouble persists

## `health`

`lab/src/ops.rs:195`

```rust
pub fn health()
```

Notes from the code: ---------------------------------------------------------------- health: one-shot report

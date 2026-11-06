# `lab/src/sysinfo.rs`

> Small machine probes for the scheduler's guards (load, free memory, local model server). All read-only.

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `run`

`lab/src/sysinfo.rs:6`

```rust
fn run(cmd: &str, args: &[&str]) -> Option<String>
```

Output of a command when it succeeds.

## `load1`

`lab/src/sysinfo.rs:12`

```rust
pub fn load1() -> Option<f64>
```

1-minute load average.

## `ncpu`

`lab/src/sysinfo.rs:18`

```rust
pub fn ncpu() -> usize
```

Number of CPU cores (4 if unknown).

## `free_gb`

`lab/src/sysinfo.rs:23`

```rust
pub fn free_gb() -> Option<f64>
```

Memory that can be given to a new process without swapping: free + inactive + speculative pages, in GB.

## `disk_free_gb`

`lab/src/sysinfo.rs:29`

```rust
pub fn disk_free_gb(path: &std::path::Path) -> Option<f64>
```

Free space on the drive holding `path`, in GB (`df -k`; tested in tests::df_is_parsed).

## `parse_df`

`lab/src/sysinfo.rs:35`

```rust
pub fn parse_df(text: &str) -> Option<f64>
```

Available space in GB from `df -k` output (the 4th column of the data line).

## `parse_vm_stat`

`lab/src/sysinfo.rs:41`

```rust
pub fn parse_vm_stat(text: &str) -> Option<f64>
```

Free + inactive + speculative memory in GB from `vm_stat` output.

## `date_str`

`lab/src/sysinfo.rs:50`

```rust
pub fn date_str(ts: i64) -> String
```

Calendar date (local) of a unix time, e.g. 2026-11-08.

## `local_hour`

`lab/src/sysinfo.rs:55`

```rust
pub fn local_hour() -> u32
```

Local hour of day (0-23).

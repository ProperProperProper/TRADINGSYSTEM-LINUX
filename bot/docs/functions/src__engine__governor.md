# `src/engine/governor.rs`

> Process-wide CPU governor. A sampler thread measures this process's real
> CPU time (all threads, 100% = one full core) and adjusts a pause fraction;
> heavy work calls `checkpoint()` in its inner loop, which sleeps in
> proportion so the whole bot stays at or below CPU_TARGET_PCT. Trading loops
> never call checkpoint, so they are never slowed. `checkpoint` also enforces
> a deadline so a stuck job aborts instead of hanging.

9 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `fmt`

`src/engine/governor.rs:27`  (method / nested)

```rust
fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result
```

Display text of a missed search deadline.

## `load`

`src/engine/governor.rs:34`

```rust
fn load(a: &AtomicU64) -> f64
```

Reads an f64 stored as bits in an atomic.

## `store`

`src/engine/governor.rs:38`

```rust
fn store(a: &AtomicU64, v: f64)
```

Stores an f64 as bits in an atomic.

## `process_cpu_seconds`

`src/engine/governor.rs:43`

```rust
pub fn process_cpu_seconds() -> f64
```

User + system CPU seconds consumed by this process so far.

## `cpu_pct`

`src/engine/governor.rs:60`  (method / nested)

```rust
pub fn cpu_pct(&self) -> f64
```

Process CPU use (%) at the last measurement.

## `pause_fraction`

`src/engine/governor.rs:65`  (method / nested)

```rust
pub fn pause_fraction(&self) -> f64
```

Share of time the governor currently pauses work to hold the CPU target.

## `update`

`src/engine/governor.rs:70`  (method / nested)

```rust
fn update(&self, cpu: f64)
```

Integral controller: raise the pause fraction while above target, lower it below.

## `checkpoint`

`src/engine/governor.rs:78`  (method / nested)

```rust
pub fn checkpoint(&self, deadline: Instant) -> Result<(), DeadlineExceeded>
```

Call often from CPU-heavy loops (never from the trading loop).

## `global`

`src/engine/governor.rs:105`

```rust
pub fn global() -> &'static Governor
```

The global governor; starts its sampler thread on first use.

# `src/cull.rs`

> The data-size cull (spec section 6). `data.sqlite3` is bounded: at the limit, whole runs of the lowest-value
> ingest batches are deleted, oldest first, but only after their statistics have been distilled into
> `memory.sqlite3` and read back. Every cull is logged; dry-run changes nothing.

4 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `free_bytes`

`src/cull.rs:27`

```rust
fn free_bytes(data: &Connection) -> Result<u64>
```

Free space inside the data database (free pages times page size).

## `distil_batch`

`src/cull.rs:34`

```rust
fn distil_batch(mem: &Connection, data: &Connection, batch: i64) -> Result<()>
```

Statistics preserved before raw rows go (spec section 6): per-feature quintile tables on de-duplicated entries.

## `needs_cull`

`src/cull.rs:77`

```rust
pub fn needs_cull(cfg: &Config) -> bool
```

Whether the data database has reached its size limit.

## `cull`

`src/cull.rs:80`

```rust
pub fn cull(cfg: &Config, mem: &Connection, data: &Connection, dry_run: bool, force: bool) -> Result<CullReport>
```

Deletes the oldest raw batches (keeping the newest batches) until the data database is under its target, after distilling their statistics.

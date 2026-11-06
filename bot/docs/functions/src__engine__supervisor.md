# `src/engine/supervisor.rs`

> Keeps every background task running. A task that returns an error, panics,
> or stops sending heartbeats for longer than its `stall_after` is logged,
> aborted and restarted with exponential backoff (5s doubling to 5 min).

4 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `beat`

`src/engine/supervisor.rs:39`  (method / nested)

```rust
pub fn beat(&self)
```

Marks the task alive now.

## `snapshot`

`src/engine/supervisor.rs:54`  (method / nested)

```rust
pub fn snapshot(&self) -> BTreeMap<String, TaskHealth>
```

Restarts, last error and seconds since the last heartbeat of every task.

## `with`

`src/engine/supervisor.rs:74`  (method / nested)

```rust
fn with<R>(&self, name: &str, f: impl FnOnce(&mut Entry) -> R) -> R
```

Runs `f` on the task's entry, creating it if missing.

## `spawn`

`src/engine/supervisor.rs:91`  (method / nested)

```rust
pub fn spawn<F, Fut>(&self, name: &str, stall_after: Duration, make_task: F) where F: Fn(Heartbeat) -> Fut + Send + Sync + 'static, Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
```

Spawn `make_task` under supervision. The task must call `hb.beat()` at least every `stall_after`, or it is treated as hung and restarted.

Notes from the code: Note: A task cannot beat while it awaits a spawn_blocking job, so its stall limit must exceed that job's deadline: search_task uses SEARCH_DEADLINE + 10 min (service.rs), or a long search gets killed mid-run. bar_task no longer runs the search.

# `lab/src/dash.rs`

> The monitoring dashboard: a small READ-ONLY web page served on 127.0.0.1, separate from the bot. It shows what the lab is doing,
> what real data it has, how close it is to a lesson, and whether anything needs attention. It opens the lab databases read-only,
> has no endpoint that changes anything, accepts only GET, and answers only to localhost host names. The page is fully
> self-contained (no external scripts, fonts or images).

18 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `open_ro`

`lab/src/dash.rs:19`

```rust
fn open_ro(path: &std::path::Path) -> Result<Connection>
```

Opens a database read-only with a 5 s busy timeout.

## `q1`

`lab/src/dash.rs:26`

```rust
fn q1<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> Option<T>
```

First column of the first row of `sql`, if any.

## `counter`

`lab/src/dash.rs:29`

```rust
fn counter(mem: &Connection, name: &str) -> Option<i64>
```

A named counter from the memory database, if set.

## `ticks`

`lab/src/dash.rs:34`

```rust
fn ticks(cfg: &Config, limit: usize) -> Vec<Value>
```

Recent tick logs, newest first: when each ran, light or full, and what each step reported.

## `ensure_schema`

`lab/src/dash.rs:75`

```rust
pub fn ensure_schema(cfg: &Config) -> Result<()>
```

Creates every lab table that is missing (CREATE TABLE IF NOT EXISTS; never changes data).

## `page_id`

`lab/src/dash.rs:89`

```rust
fn page_id() -> String
```

Identifies the dashboard page this server ships (a hash of the page); an open browser tab reloads itself when it changes, so a dashboard update shows without a manual refresh.

## `snapshot`

`lab/src/dash.rs:97`

```rust
pub fn snapshot(cfg: &Config) -> Result<Value>
```

Everything the dashboard shows, as one JSON snapshot (inputs, PnL, hypotheses, lessons, health, alerts).

## `respond`

`lab/src/dash.rs:301`

```rust
fn respond(stream: &mut TcpStream, code: u16, reason: &str, ctype: &str, body: &[u8], head_only: bool)
```

Writes an HTTP response with security headers and no caching.

## `handle`

`lab/src/dash.rs:310`

```rust
fn handle(cfg: &Config, port: u16, cache: &Mutex<Cache>, mut stream: TcpStream)
```

Handles one HTTP request: serves the page, the JSON snapshot (cached briefly) or 404; GET and HEAD only.

## `team`

`lab/src/dash.rs:366`

```rust
fn team(cfg: &Config, now: i64) -> Value
```

The org chart's live part: when each Claude job and service last wrote its log. Ages are seconds before `now`; a missing log is null (never a made-up time).

## `block_compare`

`lab/src/dash.rs:382`

```rust
fn block_compare(mem: &Connection) -> Value
```

Per-block returns of the current strategy and of the validated variants (cuts A and B), for the backtest charts: each series is the blocks in time order (start, return %, trades, wins, chain flag).

## `human_age`

`lab/src/dash.rs:395`

```rust
fn human_age(s: i64) -> String
```

"45 min", "3 h", "2 d".

## `freshness`

`lab/src/dash.rs:401`

```rust
fn freshness(cfg: &Config, mem: &Connection, now: i64) -> Vec<Value>
```

Each data source the dashboard shows: newest update (unix time, from the source itself), the longest gap allowed, and whether it is overdue. Market-hours sources (the bot's paper account and decisions) allow a weekend.

## `ai_inputs`

`lab/src/dash.rs:426`

```rust
fn ai_inputs(cfg: &Config, mem: &Connection) -> Value
```

Everything the dashboard shows about the lab's own models and the paper account, built from files and the databases only (nothing is computed here): the packaged model (versions, calibration, the steward's reading, the live ranking and decision, the scoreboard), the deep-learning trainer's search, the trial ledger, each brain role's latest run, and the paper account with its positions, trades and the bot's latest decision.

## `live_log`

`lab/src/dash.rs:488`

```rust
fn live_log(cfg: &Config, name: &str) -> Option<std::path::PathBuf>
```

_No comment in the code; the signature above is the description._

## `tail`

`lab/src/dash.rs:500`

```rust
fn tail(f: &std::path::Path, n: usize) -> Vec<String>
```

The last `n` lines of `f` (reads at most the last 256 KB).

## `serve_listener`

`lab/src/dash.rs:513`

```rust
pub fn serve_listener(cfg: Arc<Config>, listener: TcpListener, max_requests: Option<usize>) -> Result<()>
```

Serve until `max_requests` connections have been handled (None = forever). The listener is bound to localhost by the caller.

## `serve`

`lab/src/dash.rs:530`

```rust
pub fn serve(cfg: Config) -> Result<()>
```

Serves the dashboard on 127.0.0.1 at the configured port until stopped.

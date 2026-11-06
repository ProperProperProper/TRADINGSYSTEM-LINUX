# `src/warnings_ledger.rs`

> Warnings ledger (Rust port of the bot's ops/warnings.py). Owner, 2026-10-11: "make sure all warnings are captured and fixed and marked as
> fixed"; "i want thorough logging"; "i want claude cli to check these logs and fix".
>
> Every log of the bot and the lab is scanned incrementally, and both dashboards (lab :8790, bot console :8789) are checked
> the way a viewer sees them (leaked bug text, failed tick steps, health, not answering). Logs are read from byte offsets kept in the database,
> so nothing is read twice or skipped. Each warning or error line is reduced to a signature (time stamps, numbers, ids and ticker symbols
> removed), so one recurring problem is one entry with a count, first and last seen. Silent jobs are warnings too: a log that should be written
> on a schedule and is not ("STALE"). An entry stays open until it is marked:
> fixed    <id> --commit <sha> --note "...": the commit must exist in the bot or lab repo (checked). If the same warning appears again more than
> GRACE after the fix, code REOPENS it (the fix did not hold) and counts the reopen.
> expected <id> --note "...": a known, harmless condition; a reason is required.
> The ledger is SQLite on disk (~/Library/Application Support/ClaudeMoomooBot/warnings.sqlite3); `report` writes docs/WARNINGS.md; account-like
> numbers and secrets are redacted. The database schema and the signature rules are unchanged from the Python version, so the existing ledger
> keeps working.
>
> usage: qwen-tools warnings scan | list [--all] | show <id> | fixed <id> --commit <sha> --note <text> | expected <id> --note <text> | report <out.md> | counts

31 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `build`

`src/warnings_ledger.rs:33`

```rust
fn build(pat: &str) -> Regex
```

Compiled once, with a generous backtracking limit (the default limit made the lookahead patterns fail on very long log lines).

## `rx`

`src/warnings_ledger.rs:34`

```rust
fn rx(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex
```

_No comment in the code; the signature above is the description._

## `hit`

`src/warnings_ledger.rs:37`

```rust
fn hit() -> &'static Regex
```

_No comment in the code; the signature above is the description._

## `not`

`src/warnings_ledger.rs:38`

```rust
fn not() -> &'static Regex
```

_No comment in the code; the signature above is the description._

## `errorish`

`src/warnings_ledger.rs:39`

```rust
fn errorish() -> &'static Regex
```

_No comment in the code; the signature above is the description._

## `ts_re`

`src/warnings_ledger.rs:40`

```rust
fn ts_re() -> &'static Regex
```

_No comment in the code; the signature above is the description._

## `dash_bad`

`src/warnings_ledger.rs:41`

```rust
fn dash_bad() -> &'static Regex
```

_No comment in the code; the signature above is the description._

## `paths`

`src/warnings_ledger.rs:44`

```rust
fn paths() -> Paths
```

_No comment in the code; the signature above is the description._

## `con`

`src/warnings_ledger.rs:50`

```rust
fn con(p: &Paths) -> Result<Connection>
```

_No comment in the code; the signature above is the description._

## `clip`

`src/warnings_ledger.rs:64`

```rust
fn clip(s: &str, n: usize) -> String
```

_No comment in the code; the signature above is the description._

## `sub`

`src/warnings_ledger.rs:65`

```rust
fn sub(pat: &str, rep: &str, s: &str) -> String
```

_No comment in the code; the signature above is the description._

## `signature`

`src/warnings_ledger.rs:68`

```rust
pub fn signature(line: &str) -> String
```

One recurring problem = one signature: time stamps, numbers, ids, temp paths and ticker symbols removed.

## `line_ts`

`src/warnings_ledger.rs:81`

```rust
pub fn line_ts(line: &str) -> Option<i64>
```

Unix time of the time stamp at the start of the line (UTC when it ends in Z, local otherwise); None without one. Dates inside a message are not times, so only the first 40 characters are looked at.

## `splitlines`

`src/warnings_ledger.rs:92`

```rust
pub fn splitlines(s: &str) -> Vec<&str>
```

Python `str.splitlines` (also splits on \r, \x0b, \x0c, \x1c-\x1e, \x85, \u2028, \u2029).

## `record`

`src/warnings_ledger.rs:106`

```rust
pub fn record(c: &Connection, sig: &str, source: &str, log: &str, severity: &str, sample: &str, ts: i64, now: i64) -> Result<()>
```

Adds one occurrence; a fixed warning seen again after its fix (+GRACE) is reopened.

## `relpath`

`src/warnings_ledger.rs:122`

```rust
fn relpath(path: &Path, home: &Path) -> String
```

_No comment in the code; the signature above is the description._

## `fmt`

`src/warnings_ledger.rs:125`

```rust
pub fn fmt(ts: i64) -> String
```

Local date and time ("before the ledger" for a backlog line without a time stamp).

## `mtime`

`src/warnings_ledger.rs:130`

```rust
fn mtime(p: &Path) -> Option<f64>
```

_No comment in the code; the signature above is the description._

## `now_i`

`src/warnings_ledger.rs:132`

```rust
fn now_i() -> i64
```

_No comment in the code; the signature above is the description._

## `scan`

`src/warnings_ledger.rs:135`

```rust
pub fn scan() -> Result<String>
```

Reads every log from where the last scan stopped, records warnings, and flags silent jobs.

## `fetch`

`src/warnings_ledger.rs:187`

```rust
fn fetch(url: &str) -> std::result::Result<String, (String, String)>
```

_No comment in the code; the signature above is the description._

## `walk`

`src/warnings_ledger.rs:196`

```rust
fn walk(c: &Connection, v: &Value, path: &str, name: &str, where_: &str, now: i64, seen: &mut HashSet<String>) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `dashboards`

`src/warnings_ledger.rs:211`

```rust
fn dashboards(c: &Connection, now: i64) -> Result<()>
```

The dashboards are checked the way a viewer sees them (owner 2026-10-11: "why are you not monitoring and fixing shit like this from dashboard"): each must answer; no text may contain a leaked bug; the lab's newest tick steps must not have failed; its health numbers must be sane.

## `builds_current`

`src/warnings_ledger.rs:246`

```rust
fn builds_current(c: &Connection, p: &Paths, now: i64) -> Result<()>
```

Nothing stale may run or be shown: each installed program must be at least as new as the newest code commit for it.

## `git_has`

`src/warnings_ledger.rs:264`

```rust
fn git_has(p: &Paths, sha: &str) -> bool
```

True when sha is a commit in the bot or the lab repo.

## `flag`

`src/warnings_ledger.rs:268`

```rust
fn flag<'a>(args: &'a [String], name: &str) -> Result<Option<&'a str>>
```

_No comment in the code; the signature above is the description._

## `mark`

`src/warnings_ledger.rs:273`

```rust
pub fn mark(wid: i64, status: &str, args: &[String]) -> Result<String>
```

Marks an entry fixed (a real commit required) or expected (a reason required).

## `redact`

`src/warnings_ledger.rs:287`

```rust
pub fn redact(t: &str) -> String
```

Account-like numbers and secrets removed for publishing.

## `listing`

`src/warnings_ledger.rs:293`

```rust
pub fn listing(all_rows: bool) -> Result<Vec<String>>
```

Prints open and reopened entries (all with --all), most severe and newest first.

## `report`

`src/warnings_ledger.rs:307`

```rust
pub fn report(out: &str) -> Result<String>
```

_No comment in the code; the signature above is the description._

## `main`

`src/warnings_ledger.rs:333`

```rust
pub fn main(a: &[String]) -> i32
```

_No comment in the code; the signature above is the description._

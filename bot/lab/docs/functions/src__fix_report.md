# `src/fix_report.rs`

> Code checks for the daily fix report (Rust port of the bot's ops/fix_report_check.py; used by ops/fix_report.sh).
>
> check <commits.md> <report.md>     exit 0 when every number and every commit id in the report appears in the commits (hard rule: never invent
> data or results); otherwise prints what is unverified, exit 1
> fallback <commits.md> <report.md>  writes the report from the commit messages as they are (used when the check fails)
> secrets <report.md>                exit 1 when the report looks like it contains an account number, key or password
> index <docs/fixes dir>             rewrites docs/fixes/README.md, the list of all reports, newest first

14 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `big`

`src/fix_report.rs:15`

```rust
fn big(p: &str) -> Regex
```

_No comment in the code; the signature above is the description._

## `res`

`src/fix_report.rs:16`

```rust
fn res() -> Res
```

_No comment in the code; the signature above is the description._

## `norm`

`src/fix_report.rs:25`

```rust
fn norm(n: &str) -> String
```

_No comment in the code; the signature above is the description._

## `all`

`src/fix_report.rs:26`

```rust
fn all(re: &Regex, text: &str) -> Vec<String>
```

_No comment in the code; the signature above is the description._

## `numbers`

`src/fix_report.rs:27`

```rust
fn numbers(r: &Res, text: &str) -> BTreeSet<String>
```

_No comment in the code; the signature above is the description._

## `py_list`

`src/fix_report.rs:28`

```rust
fn py_list(v: &[String]) -> String
```

_No comment in the code; the signature above is the description._

## `check`

`src/fix_report.rs:30`

```rust
pub fn check(commits: &str, report: &str) -> Result<i32>
```

_No comment in the code; the signature above is the description._

## `blocks`

`src/fix_report.rs:48`

```rust
fn blocks(src: &str) -> Vec<&str>
```

Splits a text into the blocks that start at a line beginning with `### ` (text before the first one is dropped, as the original did).

## `fallback`

`src/fix_report.rs:55`

```rust
pub fn fallback(commits: &str, report: &str) -> Result<i32>
```

_No comment in the code; the signature above is the description._

## `find_secret`

`src/fix_report.rs:74`

```rust
pub fn find_secret(text: &str) -> Option<String>
```

The leftmost "possible secret" in `text`, as the original regex found it: a key word followed by a value, an account id, or a bare 12+ digit number that is not glued to a word/hyphen, not a YYYYMMDDHHMM stamp (backup file names) and not followed by a word character. Written without lookarounds (linear time): the lookaround version hit the backtracking limit on large reports.

## `secrets`

`src/fix_report.rs:97`

```rust
pub fn secrets(report: &str) -> Result<i32>
```

_No comment in the code; the signature above is the description._

## `index`

`src/fix_report.rs:106`

```rust
pub fn index(dir: &str) -> Result<i32>
```

_No comment in the code; the signature above is the description._

## `filter_commits`

`src/fix_report.rs:122`

```rust
pub fn filter_commits(text: &str) -> String
```

Drops the bot's own "Sync lab/ from qwen-lab" and "Daily fix report" commits from the collected commit list (formerly inline Python in ops/fix_report.sh). A commit block is `### repo sha (date)` + its message; the title is the message's first line.

## `main`

`src/fix_report.rs:136`

```rust
pub fn main(args: &[String]) -> i32
```

_No comment in the code; the signature above is the description._

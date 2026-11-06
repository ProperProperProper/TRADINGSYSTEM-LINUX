# `src/guard.rs`

> Security guard for every piece of text that comes from outside the lab's own code before a model reads it: paper titles and
> abstracts (arXiv / OpenAlex), notes between the brain's roles, their test-request reasons, whiteboard edits and research ideas, and files scanned by hand.
> Owner's rule (2026-10-10): never take in downloaded knowledge without inspecting it for prompt injection or bad-actor content.
>
> What it does:
> - removes invisible and direction-changing characters (zero-width, bidi overrides, Unicode tag characters) and control
> characters, which are used to hide instructions from a human reader;
> - detects injection and bad-actor patterns: attempts to override instructions or change the model's role, fake chat-template
> or tool-call markup, requests for secrets or accounts, shell commands, exfiltration (send/post/upload to a URL), long encoded
> blobs; flagged text is WITHHELD (the model never sees it) and logged in `security_events` (shown on the dashboard);
> - wraps accepted outside text in explicit UNTRUSTED markers so every prompt says it is data, never instructions.
>
> TRUSTED (owner 2026-10-14): text written through the Claude CLI on this machine -- Claude Code sessions, `qwen-lab note`, supervisor /
> log-doctor / fixer output, AGENTS.md -- is the owner's own and is never screened, withheld or quarantined here ([`TRUSTED_AUTHORS`]).
> Everything else (papers, downloaded notes and skills, web text, the brain's own output) is screened.
>
> Deterministic code, no model. False positives only cost one paper or one note; a miss is still limited by what the agents can
> do (no terminal, files or accounts; every test is rule-checked and judged by code).

11 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `is_trusted`

`src/guard.rs:27`

```rust
pub fn is_trusted(source: &str) -> bool
```

Whether `source` (an author name, or a label such as "claude-code note") is a trusted author.

## `hidden`

`src/guard.rs:51`

```rust
fn hidden(c: char) -> bool
```

Characters that hide text from a human reader: zero-width, bidi controls, Unicode tags, BOM.

## `strip`

`src/guard.rs:56`

```rust
pub fn strip(text: &str) -> String
```

The text with hidden and control characters removed (newlines and tabs become spaces).

## `scan`

`src/guard.rs:61`

```rust
pub fn scan(text: &str) -> Vec<&'static str>
```

Findings for `text` (empty = clean): the kind of each pattern found, deduplicated, plus hidden characters and encoded blobs.

## `init`

`src/guard.rs:82`

```rust
pub fn init(mem: &Connection) -> Result<()>
```

Creates the security log if missing.

## `check`

`src/guard.rs:89`

```rust
pub fn check(mem: &Connection, source: &str, text: &str) -> std::result::Result<String, String>
```

Checks outside text from `source`: Ok(cleaned text) when clean, Err(findings) when withheld (logged in `security_events`).

## `count`

`src/guard.rs:107`

```rust
fn count(mem: &Connection, withheld: bool)
```

Counts one screened text (and whether it was withheld) for today, for the dashboard's counter chart.

## `wrap`

`src/guard.rs:114`

```rust
pub fn wrap(label: &str, text: &str) -> String
```

Accepted outside text wrapped so the reading model treats it as data only.

## `scan_paths`

`src/guard.rs:119`

```rust
pub fn scan_paths(mem: &Connection, paths: &[std::path::PathBuf]) -> Vec<(std::path::PathBuf, String)>
```

Scans every text file under `paths` (skills, notes, downloads): returns (file, findings) for each flagged file and logs it.

## `daily_counts`

`src/guard.rs:148`

```rust
pub fn daily_counts(mem: &Connection, days: usize) -> Vec<serde_json::Value>
```

Screened and withheld texts per day (newest last), for the dashboard's counter chart.

## `recent`

`src/guard.rs:156`

```rust
pub fn recent(mem: &Connection, limit: usize) -> Vec<serde_json::Value>
```

The newest security events for the dashboard.

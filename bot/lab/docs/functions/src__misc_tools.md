# `src/misc_tools.rs`

> Small helper commands that used to be inline Python in shell scripts (owner 2026-10-13: "everything is rust only"):
> agent-results  (stdin = dashboard snapshot)   the "Agents: tests and results" block of the quant supervisor's bundle
> market                                        OPEN / CLOSED for the US equity session (New York clock, 09:25-16:05 on weekdays)

19 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `py_float`

`src/misc_tools.rs:10`

```rust
pub fn py_float(f: f64) -> String
```

Notes from the code: ---------------------------------------------------------------- Python-style text for numbers, dicts and JSON (the reports keep their old look)

## `py_quote`

`src/misc_tools.rs:17`

```rust
pub fn py_quote(s: &str) -> String
```

_No comment in the code; the signature above is the description._

## `py_repr`

`src/misc_tools.rs:24`

```rust
pub fn py_repr(v: &Value) -> String
```

Python `repr` of a JSON value (None/True/False, single-quoted strings, `{'k': v}`).

## `py_str_pub`

`src/misc_tools.rs:34`

```rust
pub fn py_str_pub(v: Option<&Value>) -> String
```

Python `str()` of an optional JSON value (strings unquoted, missing = None).

## `py_str`

`src/misc_tools.rs:35`

```rust
fn py_str(v: Option<&Value>) -> String
```

_No comment in the code; the signature above is the description._

## `py_json`

`src/misc_tools.rs:37`

```rust
pub fn py_json(v: &Value) -> String
```

Python `json.dumps` (", " and ": " separators, non-ASCII escaped).

## `w`

`src/misc_tools.rs:52`

```rust
fn w<'a>(o: &'a Value, k: &str) -> Option<&'a Value>
```

_No comment in the code; the signature above is the description._

## `truthy`

`src/misc_tools.rs:57`

```rust
fn truthy(v: &Value) -> bool
```

_No comment in the code; the signature above is the description._

## `agent_results`

`src/misc_tools.rs:59`

```rust
pub fn agent_results(d: &Value) -> Vec<String>
```

_No comment in the code; the signature above is the description._

## `market_state`

`src/misc_tools.rs:73`

```rust
pub fn market_state(dow: u32, hhmm: u32) -> &'static str
```

OPEN / CLOSED from New York's clock (weekday number 1=Mon..7=Sun, HHMM).

## `market_window_now`

`src/misc_tools.rs:77`

```rust
pub fn market_window_now(from: u32, to: u32) -> &'static str
```

OPEN / CLOSED for a custom weekday window given as HHMM bounds (inclusive, to the minute; the Python versions compared to the second, so the closing minute can read OPEN up to 59 s longer).

## `market_now`

`src/misc_tools.rs:83`

```rust
pub fn market_now() -> &'static str
```

_No comment in the code; the signature above is the description._

## `json_len`

`src/misc_tools.rs:91`

```rust
pub fn json_len(path: &str) -> Result<usize>
```

Number of rows in the fill probe's JSON list.

Notes from the code: ---------------------------------------------------------------- small JSON file helpers used by the bot's ops scripts

## `json_get`

`src/misc_tools.rs:93`

```rust
pub fn json_get(path: &str, key: &str) -> Result<String>
```

`data[key]` of a JSON object file as plain text (strings unquoted).

## `json_join`

`src/misc_tools.rs:98`

```rust
pub fn json_join(path: &str, key: &str) -> Result<String>
```

`' '.join(data[key])` for a list of strings.

## `fill_probe_verdict`

`src/misc_tools.rs:103`

```rust
pub fn fill_probe_verdict(path: &str) -> Vec<String>
```

The fill probe's decision from its real measurements only: every size filled completely both ways and the worst slippage against the quote is small.

## `mcp_text`

`src/misc_tools.rs:117`

```rust
pub fn mcp_text(reply_line: &str) -> Result<String>
```

First line of an MCP reply -> `result.content[0].text`.

## `main_mcp_text`

`src/misc_tools.rs:122`

```rust
pub fn main_mcp_text() -> i32
```

_No comment in the code; the signature above is the description._

## `main_agent_results`

`src/misc_tools.rs:128`

```rust
pub fn main_agent_results() -> i32
```

_No comment in the code; the signature above is the description._

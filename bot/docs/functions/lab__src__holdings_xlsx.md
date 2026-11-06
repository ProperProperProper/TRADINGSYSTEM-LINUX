# `lab/src/holdings_xlsx.rs`

> SPY's official daily holdings spreadsheet (State Street) -> holdings CSV (rank,ticker,name,weight_pct). Rust port of the bot's
> ops/spy_holdings_xlsx.py. `qwen-tools spy-holdings-xlsx <holdings-daily-us-en-spy.xlsx> <out.csv>`.
> Rows kept: USD equity lines with a ticker; the cash line (ticker "-") is dropped. Nothing is invented: names and weights are the file's.

7 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `local`

`lab/src/holdings_xlsx.rs:11`

```rust
fn local(name: &[u8]) -> &[u8]
```

_No comment in the code; the signature above is the description._

## `shared_strings`

`lab/src/holdings_xlsx.rs:14`

```rust
fn shared_strings(xml: &[u8]) -> Result<Vec<String>>
```

Text of every `<t>` inside each top-level `<si>` of sharedStrings.xml.

## `sheet_rows`

`lab/src/holdings_xlsx.rs:34`

```rust
pub fn sheet_rows(xml: &[u8], strings: &[String]) -> Result<Vec<BTreeMap<String, String>>>
```

Each row of a worksheet as {column letters: cell text}; `strings` resolves shared-string cells.

## `read_rows`

`lab/src/holdings_xlsx.rs:70`

```rust
fn read_rows(path: &str) -> Result<Vec<BTreeMap<String, String>>>
```

_No comment in the code; the signature above is the description._

## `holdings`

`lab/src/holdings_xlsx.rs:81`

```rust
pub fn holdings(rows: &[BTreeMap<String, String>]) -> Result<(Option<String>, Vec<(f64, String, String)>)>
```

(as-of text, holdings sorted by weight descending) from the spreadsheet rows.

## `convert`

`lab/src/holdings_xlsx.rs:102`

```rust
pub fn convert(src: &str, dst: &str) -> Result<String>
```

_No comment in the code; the signature above is the description._

## `main`

`lab/src/holdings_xlsx.rs:111`

```rust
pub fn main(args: &[String]) -> i32
```

_No comment in the code; the signature above is the description._

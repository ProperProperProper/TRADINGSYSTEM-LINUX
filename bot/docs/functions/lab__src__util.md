# `lab/src/util.rs`

> Small shared helpers: hashing, time, canonical JSON.

7 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `sha256_bytes`

`lab/src/util.rs:9`

```rust
pub fn sha256_bytes(b: &[u8]) -> String
```

SHA-256 of bytes as hex.

## `sha256_str`

`lab/src/util.rs:14`

```rust
pub fn sha256_str(s: &str) -> String
```

SHA-256 of a string as hex.

## `sha256_file`

`lab/src/util.rs:19`

```rust
pub fn sha256_file(p: &Path) -> Result<String>
```

SHA-256 of a file as hex.

## `now_ts`

`lab/src/util.rs:32`

```rust
pub fn now_ts() -> i64
```

Current unix time in seconds.

## `canonical`

`lab/src/util.rs:37`

```rust
pub fn canonical(v: &serde_json::Value) -> String
```

serde_json maps are ordered (BTreeMap) without the `preserve_order` feature, so this is canonical.

## `fmt_g`

`lab/src/util.rs:44`

```rust
pub fn fmt_g(x: f64) -> String
```

Python's `f"{x:g}"`: 6 significant digits, trailing zeros trimmed, exponent form below 1e-4 or from 1e6 (`1.23457e+06`, `1e-05`).

## `utc_minute`

`lab/src/util.rs:71`

```rust
pub fn utc_minute(ts: i64) -> String
```

`YYYY-MM-DD HH:MM UTC` for a Unix timestamp (civil-from-days, no date library).

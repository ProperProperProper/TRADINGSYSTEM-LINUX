# `lab/src/main.rs`

6 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `flag`

`lab/src/main.rs:32`

```rust
fn flag(args: &[String], name: &str) -> Option<String>
```

Value following the command-line flag `name`.

## `has`

`lab/src/main.rs:37`

```rust
fn has(args: &[String], name: &str) -> bool
```

Whether the command line contains `name`.

## `num`

`lab/src/main.rs:40`

```rust
fn num<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> Result<T>
```

Numeric value of flag `name`, or `default` when absent.

## `config_path`

`lab/src/main.rs:45`

```rust
fn config_path(args: &[String]) -> PathBuf
```

Path of lab.toml: `--config <path>` or the default root's lab.toml.

## `open_all`

`lab/src/main.rs:50`

```rust
fn open_all(cfg: &Config) -> Result<(rusqlite::Connection, rusqlite::Connection)>
```

Opens and migrates both databases; refuses to mix real and synthetic data in one lab.

## `main`

`lab/src/main.rs:68`

```rust
fn main() -> Result<()>
```

Command-line entry point: dispatches init, import, explore, confirm, tick, improve, dashboard, MCP and the other commands.

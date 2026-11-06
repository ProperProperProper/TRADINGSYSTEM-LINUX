# `lab/src/db.rs`

> The two SQLite databases: `memory.sqlite3` (small, permanent) and `data.sqlite3` (large, culled).

11 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `block_secs`

`lab/src/db.rs:8`

```rust
pub fn block_secs(c: &Connection) -> i64
```

Block size (seconds) used to cut independent windows; fixed per lab at the first archive import.

## `valid_feature_name`

`lab/src/db.rs:13`

```rust
pub fn valid_feature_name(n: &str) -> bool
```

A feature name is `[a-z][a-z0-9_]{0,40}`: it becomes a column name (`f_<name>`), so it is validated before any SQL.

## `registry`

`lab/src/db.rs:19`

```rust
pub fn registry(c: &Connection) -> Result<Vec<String>>
```

All registered feature names (dynamic: new features appear when the bot starts exporting them).

## `ensure_feature`

`lab/src/db.rs:26`

```rust
pub fn ensure_feature(c: &Connection, name: &str, source: &str) -> Result<()>
```

Register a feature and add its column the first time it is seen. Existing lessons keep pointing at the old name.

## `open`

`lab/src/db.rs:37`

```rust
pub fn open(path: &Path, synchronous_full: bool) -> Result<Connection>
```

Opens (or creates) a SQLite database in WAL mode with foreign keys and a 30 s busy timeout.

## `migrate_memory`

`lab/src/db.rs:50`

```rust
pub fn migrate_memory(conn: &Connection) -> Result<()>
```

Creates or upgrades the memory database schema.

## `columns_of`

`lab/src/db.rs:56`

```rust
fn columns_of(conn: &Connection, table: &str) -> Result<Vec<String>>
```

Column names of `table`.

## `add_column_if_missing`

`lab/src/db.rs:63`

```rust
fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()>
```

Adds `column` to `table` if the table exists and lacks it.

## `migrate_data`

`lab/src/db.rs:73`

```rust
pub fn migrate_data(conn: &Connection) -> Result<bool>
```

Create or upgrade `data.sqlite3`. Returns true when the paper copy tables had an older layout and were rebuilt (they are only a copy of the bot's archive, so the caller resets the import watermarks and the next import repopulates them).

## `set_version`

`lab/src/db.rs:99`

```rust
fn set_version(conn: &Connection, component: &str, version: &str) -> Result<()>
```

Records the schema version of a component.

## `size_bytes`

`lab/src/db.rs:109`

```rust
pub fn size_bytes(path: &Path) -> u64
```

Database size in bytes including the WAL.

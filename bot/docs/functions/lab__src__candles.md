# `lab/src/candles.rs`

> The lab's own copy of the market candles (everything on disk, owner rule): 15-minute bars of every symbol the bot has pulled, merged from the bot's
> research cache (`moomoo-bot/target/oos-cache/history.json`, the longest history moomoo gives, from 2026-01-02; it lives in a build folder that
> `cargo clean` would erase) and the bot's live store (`moomoo.sqlite3`, newer bars). Imported into `<lab>/data/candles.sqlite3`, merged by (symbol, time).
> The machine-learning tests (`mlcandle.rs`) read only from here.

12 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `init`

`lab/src/candles.rs:18`

```rust
pub fn init(db: &Connection) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `valid`

`lab/src/candles.rs:25`

```rust
pub fn valid(b: &Bar) -> bool
```

A bar is usable when its prices are positive and ordered; anything else is dropped (and counted by the importer).

## `import`

`lab/src/candles.rs:28`

```rust
pub fn import(history_json: Option<&Path>, bot_db: Option<&Path>, out: &Connection) -> Result<(usize, usize, usize)>
```

Merges the research cache and the bot's store into `out`. Returns (bars read, bars written, bars dropped as invalid).

## `load`

`lab/src/candles.rs:54`

```rust
pub fn load(db: &Connection) -> Result<BTreeMap<String, Vec<Bar>>>
```

Every symbol's bars, oldest first.

## `import_universe`

`lab/src/candles.rs:66`

```rust
pub fn import_universe(holdings_csv: &Path, out: &Connection) -> Result<usize>
```

The bot's universe: SPY's holdings (its official file, by weight) -- the same list the bot's loader reads. Stored in the lab's candle store.

## `load_universe`

`lab/src/candles.rs:82`

```rust
pub fn load_universe(db: &Connection) -> Vec<String>
```

_No comment in the code; the signature above is the description._

## `prepare`

`lab/src/candles.rs:91`

```rust
pub fn prepare(series: &BTreeMap<String, Vec<Bar>>, universe: &[String]) -> (BTreeMap<String, Vec<Bar>>, Vec<String>)
```

The bot's data hygiene (examples/common/mod.rs there): only the universe plus the SPY reference; candles are unadjusted, so a jump between consecutive bars (open / previous close outside 0.6..1.8) is a split: only the history AFTER a symbol's last jump is kept; symbols with fewer than 520 bars left are dropped. An empty `universe` keeps every symbol. Returns the cleaned series and one line per change.

## `load_prepared`

`lab/src/candles.rs:104`

```rust
pub fn load_prepared(db: &Connection) -> Result<BTreeMap<String, Vec<Bar>>>
```

The cleaned series the machine-learning code uses.

## `half_spread`

`lab/src/candles.rs:112`  (method / nested)

```rust
pub fn half_spread(&self, symbol: &str) -> f64
```

_No comment in the code; the signature above is the description._

## `one_way`

`lab/src/candles.rs:114`  (method / nested)

```rust
pub fn one_way(&self, symbol: &str, size: f64) -> f64
```

Cost of one buy or one sell of `size` USD in `symbol`, as a fraction of the position.

## `import_costs`

`lab/src/candles.rs:118`

```rust
pub fn import_costs(bot_db: &Path, out: &Connection) -> Result<(usize, f64)>
```

Reads the bot's order fees and order books into the lab's store. Returns (symbols with a spread, commission USD).

## `load_costs`

`lab/src/candles.rs:139`

```rust
pub fn load_costs(db: &Connection) -> Costs
```

The stored costs; falls back to the bot's measured $1.00 per order and a 2 bp half-spread only if nothing was imported (and says so through the values).

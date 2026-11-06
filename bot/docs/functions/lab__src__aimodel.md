# `lab/src/aimodel.rs`

> THE MODEL (owner 2026-10-13: "make it a proper AI model for this trading bot and learning").
>
> One packaged, versioned model with the parts a proper model has:
> * an ARTIFACT: the fitted weights, the feature list, the training window and scheme, a calibration table, saved as JSON (`<lab>/model/v<N>.json`);
> * TRAINING (`train`): the bot's top-20 universe, the bot's own score components, SPY-regime interactions, a ridge whose strength is chosen on held-out data;
> * INFERENCE on the live bars (`predict_live`): the newest moment's ranking of the bot's top-20 stocks, each with a score, a percentile, a CALIBRATED expected
> excess return and hit rate (measured on a held-out tail), and the three features that drove it (the model's own explanation);
> * a REGISTRY with a current-version pointer, and MONITORING: every version's live out-of-sample IC after its training end, tracked per session;
> * a LIFECYCLE (`maintain`): a new version every 14 sessions; the previous version is restored if the new one is significantly worse over the same sessions.
>
> Nothing here touches the live strategy, an account or an order: it ranks stocks and reports how well it did.

28 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `feature_names`

`lab/src/aimodel.rs:43`

```rust
pub fn feature_names() -> Vec<String>
```

_No comment in the code; the signature above is the description._

## `scheme_name`

`lab/src/aimodel.rs:48`

```rust
fn scheme_name(s: Scheme) -> String
```

_No comment in the code; the signature above is the description._

## `universe_samples`

`lab/src/aimodel.rs:53`

```rust
fn universe_samples(series: &BTreeMap<String, Vec<Bar>>, labelled: bool, step: i64) -> Vec<Sample>
```

Samples in the bot's top-20 universe with all features (the interactions last). `labelled` = completed labels (training), else the newest unlabelled bars.

## `session`

`lab/src/aimodel.rs:58`

```rust
fn session(ts: i64) -> i64
```

_No comment in the code; the signature above is the description._

## `isotonic`

`lab/src/aimodel.rs:62`

```rust
pub fn isotonic(bins: &mut [Bin])
```

Pool-adjacent-violators: the closest NON-DECREASING sequence to the bin means (weighted by n). A higher percentile can never promise less than a lower one; the noise in a small held-out tail is averaged out instead of being presented as structure. Hit rates are made monotone the same way.

## `pav`

`lab/src/aimodel.rs:63`  (method / nested)

```rust
fn pav(v: &[(f64, f64)]) -> Vec<f64>
```

_No comment in the code; the signature above is the description._

## `calibrate`

`lab/src/aimodel.rs:74`

```rust
pub fn calibrate(test: &[Sample], pred: &[f64]) -> Vec<Bin>
```

Decile calibration of `pred` against the realised excess returns, by within-moment percentile.

## `mean_ic`

`lab/src/aimodel.rs:89`

```rust
fn mean_ic(test: &[Sample], pred: &[f64]) -> f64
```

_No comment in the code; the signature above is the description._

## `train`

`lab/src/aimodel.rs:96`

```rust
pub fn train(series: &BTreeMap<String, Vec<Bar>>, scheme: Scheme, version: u32) -> Result<Artifact>
```

Trains a new version on everything labelled so far.

## `zscore`

`lab/src/aimodel.rs:117`

```rust
pub fn zscore(v: &[f64]) -> Vec<f64>
```

Standardises a set of scores to mean 0 and standard deviation 1 (empty in, empty out).

## `predict`

`lab/src/aimodel.rs:127`  (method / nested)

```rust
pub fn predict(&self, s: &Sample) -> f64
```

The model's score for one sample.

## `predict_live`

`lab/src/aimodel.rs:131`

```rust
pub fn predict_live(series: &BTreeMap<String, Vec<Bar>>, a: &Artifact, deep: &[crate::deepnet::DeepModel]) -> Option<(i64, Vec<Pick>)>
```

Where the artifact ranks the bot's top-20 stocks at the newest moment with enough names: (bar open ms, picks best first).

## `init_ledger`

`lab/src/aimodel.rs:165`

```rust
pub fn init_ledger(db: &rusqlite::Connection) -> rusqlite::Result<()>
```

Notes from the code: ------------------------------------------------------------------ the prediction ledger: every live call is scored against what really happened

## `record_predictions`

`lab/src/aimodel.rs:171`

```rust
pub fn record_predictions(db: &rusqlite::Connection, version: u32, ts: i64, picks: &[Pick]) -> rusqlite::Result<usize>
```

Stores one live ranking (the model's calls, made before the outcome exists). A repeat of the same (version, bar, stock) is ignored.

## `resolve_predictions`

`lab/src/aimodel.rs:179`

```rust
pub fn resolve_predictions(db: &rusqlite::Connection, series: &BTreeMap<String, Vec<Bar>>) -> Result<usize>
```

Fills in the realised excess return (next 26 bars, minus the average of the stocks ranked at that moment) for every stored call whose outcome has arrived.

## `scoreboard`

`lab/src/aimodel.rs:197`

```rust
pub fn scoreboard(db: &rusqlite::Connection, version: Option<u32>) -> Result<Scoreboard>
```

The model's real record: every resolved call. top/bottom = the best and worst 3 of the 20 at each moment.

## `line`

`lab/src/aimodel.rs:225`  (method / nested)

```rust
pub fn line(&self) -> String
```

_No comment in the code; the signature above is the description._

## `dir`

`lab/src/aimodel.rs:232`

```rust
pub fn dir(root: &Path) -> PathBuf
```

Notes from the code: ------------------------------------------------------------------ registry

## `save`

`lab/src/aimodel.rs:233`

```rust
pub fn save(root: &Path, a: &Artifact) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `load`

`lab/src/aimodel.rs:241`

```rust
pub fn load(root: &Path, version: u32) -> Result<Artifact>
```

_No comment in the code; the signature above is the description._

## `versions`

`lab/src/aimodel.rs:242`

```rust
pub fn versions(root: &Path) -> Vec<u32>
```

_No comment in the code; the signature above is the description._

## `current`

`lab/src/aimodel.rs:248`

```rust
pub fn current(root: &Path) -> Option<u32>
```

_No comment in the code; the signature above is the description._

## `set_current`

`lab/src/aimodel.rs:249`

```rust
pub fn set_current(root: &Path, v: u32) -> Result<()>
```

_No comment in the code; the signature above is the description._

## `forward_daily`

`lab/src/aimodel.rs:253`

```rust
pub fn forward_daily(series: &BTreeMap<String, Vec<Bar>>, arts: &[Artifact]) -> BTreeMap<u32, BTreeMap<i64, f64>>
```

Per-session mean rank IC of each artifact on the sessions AFTER its training end (plus a 3-day purge): its real out-of-sample record.

Notes from the code: ------------------------------------------------------------------ monitoring

## `summarize`

`lab/src/aimodel.rs:272`

```rust
pub fn summarize(d: &BTreeMap<i64, f64>) -> Option<(f64, f64, usize)>
```

(mean, standard error, sessions) of a series of daily values.

## `should_roll_back`

`lab/src/aimodel.rs:281`

```rust
pub fn should_roll_back(newer: &BTreeMap<i64, f64>, older: &BTreeMap<i64, f64>) -> bool
```

Restore the older version when the newer one is significantly worse over the sessions both have seen (95%, at least 10 sessions).

## `maintain`

`lab/src/aimodel.rs:288`

```rust
pub fn maintain(root: &Path, series: &BTreeMap<String, Vec<Bar>>, scheme: Scheme) -> Result<Vec<String>>
```

The lifecycle step the steward runs: a new version every `RETRAIN_SESSIONS` sessions, forward records for the latest versions, and the rollback rule. Returns report lines.

## `card`

`lab/src/aimodel.rs:319`

```rust
pub fn card(root: &Path) -> Result<String>
```

The model card as markdown, from the current artifact and the monitor file.

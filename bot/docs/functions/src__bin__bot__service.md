# `src/bin/bot/service.rs`

> Long-only PAPER-trading service for US stocks on moomoo: a cross-sectional mean-reversion
> engine, with every paper decision executed as a REAL moomoo paper
> order (US SIMULATE account only) and the engine's paper account reconciled with moomoo's fills,
> fees and positions (`engine::execution`). It trades a `START_CAPITAL` (5,000 USD) account, never more than the
> available cash (`capital_from`), with 2 open positions on 5h candles.
>
> bar_task (each regular-session 15m close): the universe (every eligible SPY holding from spy_holdings.csv + SPY, the trend
> reference) -> the account,
> closed 15m candles (quota-guarded), hourly rules -> the newest market per interval -> real
> order states and fees -> the paper account (replay, reconcile, decide) -> orders at the next
> open of the paper interval (`place_open`).
> search_task: 200k random combinations for each live interval, champion/challenger; next round 5 min
> after the last finished. stability_task: stability + walk-forward tests while the market is closed.
> Console: 127.0.0.1:8789 (`/`, `/api/status`, `/api/signals`, `/api/paper`, `/api/orders`, ...).

59 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `capital_from`

`src/bin/bot/service.rs:55`

```rust
fn capital_from(account: &Account) -> f64
```

The USD the bot trades with: a `START_CAPITAL` (5000 USD) account (user 2026-10-09), never more than the paper account's AVAILABLE cash (long only, cash only, whole shares). Falls back to `START_CAPITAL` only while the cash is unknown or tiny.

## `affordable_qty`

`src/bin/bot/service.rs:61`

```rust
fn affordable_qty(cash: f64, mid: f64, qty: f64) -> f64
```

Whole shares of `qty` the cash can pay for at `mid` with the availability slack (never negative).

## `range`

`src/bin/bot/service.rs:288`  (method / nested)

```rust
fn range(&self) -> std::ops::Range<usize>
```

Bar indices of this frame's test window (from the test start to the newest bar).

## `get`

`src/bin/bot/service.rs:304`  (method / nested)

```rust
fn get(&self, i: Interval) -> Option<&Frame>
```

The frame of interval `i`, if built.

## `search_summary`

`src/bin/bot/service.rs:325`

```rust
fn search_summary(x: &Search) -> serde_json::Value
```

The console summary of one search (what /api/winners and /api/winner show), also what is saved on disk.

## `load_candidates`

`src/bin/bot/service.rs:340`

```rust
fn load_candidates(dir: &Path) -> BTreeMap<Interval, Vec<Candidate>>
```

Reads `CANDIDATES_FILE` from `dir` (empty when missing or unreadable).

## `load_console_state`

`src/bin/bot/service.rs:348`

```rust
fn load_console_state(dir: &Path) -> SavedConsole
```

Reads `CONSOLE_STATE_FILE` from `dir`.

## `now_ms`

`src/bin/bot/service.rs:363`

```rust
fn now_ms() -> i64
```

Current wall time in milliseconds.

## `event`

`src/bin/bot/service.rs:369`  (method / nested)

```rust
fn event(&self, level: &str, msg: String)
```

Logs a service event and keeps it in the console's recent-events list (newest first, at most 200).

## `exclude`

`src/bin/bot/service.rs:382`  (method / nested)

```rust
fn exclude(&self, symbol: &str, why: String)
```

Record an exclusion (logged once per reason change; shown on the console).

## `include`

`src/bin/bot/service.rs:392`  (method / nested)

```rust
fn include(&self, symbol: &str)
```

Removes `symbol` from the excluded list (its data is available again).

## `live_space`

`src/bin/bot/service.rs:398`  (method / nested)

```rust
fn live_space(&self, interval: Interval) -> SearchSpace
```

The search space of `interval` under the live strategy (`live_options::LIVE`, a code constant changed only by a reviewed promotion commit), used by the live search, the paper account and the console's tests alike.

## `save_console_state`

`src/bin/bot/service.rs:404`  (method / nested)

```rust
fn save_console_state(&self)
```

Writes `CONSOLE_STATE_FILE` (atomically): research and walk-forward results, every interval's latest search summary (a fresh search, else the restored one) and the paper interval choice.

## `save_candidates`

`src/bin/bot/service.rs:426`  (method / nested)

```rust
fn save_candidates(&self)
```

Writes `CANDIDATES_FILE` (atomically).

## `save_paper`

`src/bin/bot/service.rs:437`  (method / nested)

```rust
fn save_paper(&self) -> Result<()>
```

Writes the paper account state to disk (atomic replace), if one exists.

## `sessions`

`src/bin/bot/service.rs:446`  (method / nested)

```rust
async fn sessions(&self) -> Result<Arc<Sessions>>
```

The session calendar, refreshed hourly from moomoo (minus closures found in the candles).

## `listing`

`src/bin/bot/service.rs:460`  (method / nested)

```rust
async fn listing(&self, symbol: &str) -> Result<Listing>
```

Static information of `symbol` (cached; it does not change).

## `quota`

`src/bin/bot/service.rs:470`  (method / nested)

```rust
async fn quota(&self) -> Result<QuotaState>
```

The candle quota now (stored with every read).

## `fetch`

`src/bin/bot/service.rs:488`  (method / nested)

```rust
async fn fetch(&self, symbols: &[String], must: &[String], oldest: i64, sessions: &Sessions, eligible: &[String], hb: &Heartbeat) -> Result<Vec<(SymbolData, Option<String>)>>
```

Closed 15-minute candles of `symbols` from `oldest` (wall) on, with each symbol's listing data and current rules; a symbol that cannot be read comes back with the reason.

Notes from the code: Note: The 1h / 4h / one-session warm-up needs ~90 sessions of 15m bars. They are fetched from moomoo once per symbol; every later bar fetches only the days since the newest cached bar (overlapping, so moomoo's latest values replace older ones) and drops bars older than any window needs. Every fetch is written to the store. A symbol not yet pulled in the 7-day quota window is pulled only while the quota allows (user rule: never exceed it): held symbols and the reference may use the reserve, new candidates may not.

## `wall_fn`

`src/bin/bot/service.rs:549`  (method / nested)

```rust
fn wall_fn(&self, interval: Interval) -> impl Fn(i64) -> Option<i64>
```

Map engine clock times of `interval` to wall times for display.

## `paper_interval`

`src/bin/bot/service.rs:560`  (method / nested)

```rust
fn paper_interval(&self) -> Interval
```

The interval the paper account is on (its state), else the one it should trade.

## `fee_points`

`src/bin/bot/service.rs:571`  (method / nested)

```rust
fn fee_points(&self) -> Vec<(f64, f64)>
```

Real fee observations (notional, fee) from this account's paper orders, all symbols.

## `walls`

`src/bin/bot/service.rs:577`

```rust
fn walls(v: &mut serde_json::Value, wall: &dyn Fn(i64) -> Option<i64>)
```

Replace engine clock timestamps in console JSON by wall times (keys listed here).

## `serve`

`src/bin/bot/service.rs:599`

```rust
pub async fn serve(dir: PathBuf) -> Result<()>
```

Runs the service: connects to OpenD, loads the paper state and archives, then supervises the bar, search, stability, watch and console tasks.

## `winner_curve`

`src/bin/bot/service.rs:753`

```rust
fn winner_curve(f: &Frame, sc: &[Vec<Option<scores::Score>>], p: &XsParams, equity: f64) -> serde_json::Value
```

The search winner's backtest over the frame's test window, candle by candle: the same loop as `xs::backtest` (step every candle, close everything after the last), recording marked equity after each closed candle, and the trades with wall times. Its end equity is exactly the backtest's.

## `span_return`

`src/bin/bot/service.rs:786`

```rust
fn span_return(curve: &serde_json::Value, from: i64) -> Option<f64>
```

Return % of a winner curve from wall time `from` to its end: from the equity at the last point closing at or before `from` (the window start when it starts later).

## `common_span_start`

`src/bin/bot/service.rs:799`

```rust
fn common_span_start(sessions: &Sessions, closed_end: i64) -> Option<i64>
```

Wall open of the oldest of the last `TEST_SESSIONS` sessions fully closed by `closed_end`.

Notes from the code: Note: The span the intervals are compared on is the last TEST_SESSIONS CLOSED sessions (plus whatever of today has closed). The 15m window counts today's forming session as one of its 14, which during the session dropped a whole closed session from the 4h / one-session span and flipped the paper interval intraday (2026-10-07).

## `search_frame`

`src/bin/bot/service.rs:806`

```rust
fn search_frame(f: &Frame, sc: &[Vec<Option<scores::Score>>], equity: f64, deadline: Instant, seed: u64, recorded: &[XsParams], space: SearchSpace) -> Result<SearchReport>
```

The frame's full parameter search (15m: exactly the live search; other intervals the same search space over their test window, warm-up candles excluded).

## `walk_forward_test`

`src/bin/bot/service.rs:841`

```rust
fn walk_forward_test(f: &Frame, start: f64, live: SearchSpace) -> Result<(serde_json::Value, Vec<BtRun>)>
```

The walk-forward test inside the frame's test window (see the note above).

## `drawdown_pct`

`src/bin/bot/service.rs:956`

```rust
fn drawdown_pct(path: &[f64]) -> f64
```

Largest peak-to-trough fall (%) of an equity path.

## `stability`

`src/bin/bot/service.rs:972`

```rust
fn stability(f: &Frame, equity: f64, live: SearchSpace) -> Result<(serde_json::Value, Vec<BtRun>)>
```

Stability tests of one frame: the chosen settings, random settings and the top search results replayed on the test window, also written to the archive.

## `archive_runs`

`src/bin/bot/service.rs:1048`

```rust
fn archive_runs(app: &App, runs: &[BtRun])
```

Write backtest runs to the research archive (a failure is logged, never fatal).

## `stability_task`

`src/bin/bot/service.rs:1059`

```rust
async fn stability_task(app: Arc<App>, hb: Heartbeat) -> Result<()>
```

Notes from the code: Note: Runs only while the US market is closed (user request), once per closed period (and once at start-up if closed).

## `keep_alive_task`

`src/bin/bot/service.rs:1153`

```rust
async fn keep_alive_task(app: Arc<App>, hb: Heartbeat) -> Result<()>
```

Notes from the code: Note: Self-healing connection (user request 2026-10-06). OpenD expects a keep-alive every keep_alive_secs (10 s, proto 1004); a transport failure drops the connection and the next request reconnects. When OpenD stays unreachable for GATEWAY_DEAD_LIMIT, the process saves the paper account and exits; launchd restarts it after ThrottleInterval (startup fails until OpenD is back, so launchd keeps retrying). Nothing is invented meanwhile: the bar task simply cannot read data.

## `gateway_watch`

`src/bin/bot/service.rs:1165`

```rust
async fn gateway_watch(app: Arc<App>, hb: Heartbeat) -> Result<()>
```

Watches OpenD every 30 s; logs loss and recovery, and exits the service after GATEWAY_DEAD_LIMIT so launchd restarts and reconnects it.

## `pool`

`src/bin/bot/service.rs:1189`

```rust
async fn pool(app: &App) -> Result<Vec<Listing>>
```

The universe (SPY's holdings, nothing delisted or unknown to moomoo), refreshed hourly; exclusions recorded with their reason.

## `refresh_orders`

`src/bin/bot/service.rs:1223`

```rust
async fn refresh_orders(app: &App) -> Result<()>
```

Refresh the states and fees of this bot's orders from moomoo and book them (`Ledger::recompute`).

## `maybe_place_open`

`src/bin/bot/service.rs:1290`

```rust
async fn maybe_place_open(app: &Arc<App>, sessions: &Sessions) -> Result<()>
```

Place the orders of the open in `app.open_ctx` once it is open (see `execution`).

## `place_open`

`src/bin/bot/service.rs:1306`

```rust
async fn place_open(app: &Arc<App>, ctx: &OpenCtx) -> Result<()>
```

At an open of the paper interval: previews the engine's executions at live quote mids and places them as paper market orders (sells first).

## `bar_task`

`src/bin/bot/service.rs:1461`

```rust
async fn bar_task(app: Arc<App>, hb: Heartbeat) -> Result<()>
```

The 15m bar loop: after each regular-session close, refreshes universe, account, candles and rules, steps the paper account and records it.

## `legacy_version`

`src/bin/bot/service.rs:2029`

```rust
fn legacy_version() -> u32
```

Paper-state file version written before versions existed (serde default).

## `load_paper`

`src/bin/bot/service.rs:2036`

```rust
fn load_paper(path: &Path) -> Result<Option<PaperState>>
```

Notes from the code: Note: Never silently reset the paper account: a missing file is a first run, anything unreadable or invalid stops the service. To migrate state, write an explicit migration, not a fallback.

## `persist_paper`

`src/bin/bot/service.rs:2064`

```rust
fn persist_paper(path: &Path, state: &PaperState) -> Result<()>
```

Writes the paper state atomically (temp file, fsync, rename, fsync folder) after validating it.

## `build_frame`

`src/bin/bot/service.rs:2085`

```rust
fn build_frame(interval: Interval, sessions: Arc<Sessions>, walls: &[i64], data: &[SymbolData], must_keep: &[String]) -> Result<(Frame, Vec<(String, String)>)>
```

Replay only contiguous unseen bars with the previously known settings. Later balance observations and reports must not resize historical orders. One interval's market from the shared closed 15-minute bars. Symbols with incomplete candles are dropped (returned with the reason) unless listed in `must_keep`, which fails the bar.

Notes from the code: Note: Causality order: (1) replay missed bars with the PREVIOUS settings, (2) apply what was observed now (wallet change, committed margin), (3) install new settings and decide at the latest close. Anything learned now must never change a replayed bar.

## `pick`

`src/bin/bot/service.rs:2100`  (method / nested)

```rust
fn pick<T>(v: &mut Vec<T>, keep: &[bool])
```

Keeps the entries of `v` whose flag in `keep` is true (same order).

## `switch_interval`

`src/bin/bot/service.rs:2128`

```rust
fn switch_interval(app: &App, sessions: &Sessions, m15: &Market, from: Interval, to: Interval, frames: &Frames) -> Result<String>
```

Move the paper account from interval `from` to `to`; returns the log line.

Notes from the code: Note: Interval switch (user request 2026-10-06: "paper chooses the winner"). Open positions are closed at the latest real 15m close (like any exit: commission and half spread), then the flat account moves onto the new interval's clock. Equity, closed trades and drawdown history carry over; nothing is re-simulated.

## `advance_paper`

`src/bin/bot/service.rs:2185`

```rust
fn advance_paper( ps: &mut PaperState, market: &Market, sc: &[Vec<Option<scores::Score>>], next: &XsParams, allowed: bool, ready_ts: i64, ) -> Result<Vec<EngineEvent>>
```

Steps the paper account over the new candles with settings `next`, entries allowed or not, and returns the engine events.

## `mode_text`

`src/bin/bot/service.rs:2282`

```rust
fn mode_text(choice: Option<walkforward::Choice>) -> &'static str
```

Status wording for a search outcome.

## `publish_walk_forward`

`src/bin/bot/service.rs:2295`

```rust
fn publish_walk_forward(app: &App)
```

Publish the walk-forward view of the settings in use (the paper interval's).

## `register_candidate`

`src/bin/bot/service.rs:2309`

```rust
async fn register_candidate(app: &Arc<App>, report: &SearchReport, chosen: Option<XsParams>, f: Frame, equity: f64) -> Result<()>
```

Start the forward track of newly chosen settings (in-sample result on the frame's test window; the forward track is updated with every bar). Same settings are never re-registered.

## `update_forward`

`src/bin/bot/service.rs:2350`

```rust
async fn update_forward(app: &Arc<App>, frames: &Frames) -> Result<()>
```

Re-run every candidate over the candles of its interval that arrived after it was chosen.

## `record_and_test`

`src/bin/bot/service.rs:2386`

```rust
fn record_and_test(app: &App, f: &Frame, equity: f64, report: &SearchReport, chosen: Option<&XsParams>)
```

Saves a search's top settings in the parameter record and re-tests the tracked ones on the newest data.

## `search_seed`

`src/bin/bot/service.rs:2415`

```rust
fn search_seed() -> u64
```

Notes from the code: A fresh random seed per search (wall-clock nanoseconds; not security-sensitive).

## `pick_interval`

`src/bin/bot/service.rs:2427`

```rust
fn pick_interval(searches: &BTreeMap<Interval, Search>, current: Option<Interval>) -> (Option<Interval>, String)
```

The interval the paper should trade after a search round, with the comparison text.

Notes from the code: Note: User request (2026-10-06): "paper chooses the winner". After each search round the interval whose chosen (safe) settings returned most over the span EVERY interval tests (the last 14 sessions) becomes the paper interval; a tie keeps the current one. In-sample like every choice the search makes; the walk-forward and stability tests per interval are reported beside it.

## `pick_among`

`src/bin/bot/service.rs:2432`

```rust
fn pick_among(choices: &[Interval], searches: &BTreeMap<Interval, Search>, current: Option<Interval>) -> (Option<Interval>, String)
```

The best winner among `choices` (the live service only has 3h; the tests use every interval).

## `hold_interval`

`src/bin/bot/service.rs:2455`

```rust
fn hold_interval(in_session: bool, held: Option<Interval>, pick: Option<Interval>, note: String) -> (Option<Interval>, String)
```

The paper interval may change only between sessions (user rule 2026-10-08: as often as needed, never while the market is open). During a session the held interval stays and the comparison is still reported.

## `search_task`

`src/bin/bot/service.rs:2469`

```rust
async fn search_task(app: Arc<App>, hb: Heartbeat) -> Result<()>
```

The parameter search: per interval, `walkforward::LIVE_SAMPLES` random combinations backtested over its full test window, then champion/challenger against that interval's settings in use; then the paper interval is picked. The next round starts `SEARCH_PAUSE` after this one finished.

## `console`

`src/bin/bot/service.rs:2611`

```rust
async fn console(app: Arc<App>, hb: Heartbeat) -> Result<()>
```

Serves the read-only console (HTML page and JSON API) on 127.0.0.1.

## `lab_summary`

`src/bin/bot/service.rs:2780`

```rust
fn lab_summary() -> serde_json::Value
```

The research lab's state for the console (read-only), from the lab dashboard's local snapshot (127.0.0.1:8790): the "considerably better strategy" alert (decided by the lab's code), validated and candidate variants, the agents' reward scoreboard, the Hermes agents' newest test requests, notes between agents and security events. `{"up": false}` when the lab dashboard does not answer.

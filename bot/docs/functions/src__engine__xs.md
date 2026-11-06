# `src/engine/xs.rs`

> Cross-sectional portfolio over the top-20 USDT perpetuals (live: long only).
> Every `hold` bars (aligned to timestamps, so backtest and live rebalance at
> the same closes) rank the universe by a causal `Signal` (screener scores,
> return or settled funding), then hold LONG the `top` lowest and SHORT the
> `top` highest in research's neutral mode. Close-based decisions fill no
> earlier than t+2's open; rebalances close and reopen the basket. Isolated margin per position, real
> funding, optional protective stop, pessimistic intrabar order.

74 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `decision_fill_bound`

`src/engine/xs.rs:47`

```rust
fn decision_fill_bound(bar_ts: i64) -> i64
```

Open time of the first bar a decision taken at the close of the bar opening at `bar_ts` may fill at: the open two bars later.

Notes from the code: Note: Decision latency, identical in backtests, the search, the forward chart and paper (user: fix timing before relying on paper P&L or optimizer results). Bar t is processed after its close, so live a decision taken at that close can first fill at bar t+2's open. Every decision (rebalance, close-based exit, add, breaker or drawdown flatten) is therefore stamped with a bound of its close + one more bar at the moment it is decided. Never stamp already-pending items at later bars: that slides due actions forward forever. Live, `defer_new_decisions(now)` raises bounds to the next open after `now` (nothing can fill at a past open). Resting orders (intrabar stops, liquidation, live WebSocket exits) are not delayed.

## `eff_pct`

`src/engine/xs.rs:67`

```rust
fn eff_pct(fixed: Option<f64>, mult: Option<f64>, atr_pct: f64) -> Option<f64>
```

A distance in %: `mult` ATRs when both the multiple and the coin's ATR are known, else `fixed`.

## `atr_pct`

`src/engine/xs.rs:75`

```rust
pub fn atr_pct(m: &Market, sym: usize, t: usize) -> f64
```

Mean true range of the 14 bars before `t`, in % of the previous close; 0 if any bar is missing.

## `open_is_fair`

`src/engine/xs.rs:98`

```rust
fn open_is_fair(m: &Market, sym: usize, t: usize) -> bool
```

Whether the traded open is within the entry wick guard of the mark open.

## `on`

`src/engine/xs.rs:124`  (method / nested)

```rust
pub fn on(x: &AtomicBool) -> bool
```

Reads a research switch.

## `regime_allows`

`src/engine/xs.rs:227`

```rust
pub fn regime_allows(m: &Market, t: usize, r: Regime) -> bool
```

Whether `r` allows entries at bar t. A missing SPY history blocks entries.

Notes from the code: Note: Missing SPY history returns false (no entries) on purpose: never guess market state. The SPY filter is NOT covered by the CalmDip holdout proof; it is the only market-timing element.

## `regime_allows_with`

`src/engine/xs.rs:232`

```rust
pub fn regime_allows_with(m: &Market, t: usize, r: Regime, bars: usize, margin_pct: f64) -> bool
```

`regime_allows` with an explicit averaging window and up-trend margin (% above the average).

## `pressure_ok`

`src/engine/xs.rs:254`

```rust
fn pressure_ok(m: &Market, t: usize, spy: usize, bars: usize, r: &Risk) -> bool
```

Volume / buy-sell-pressure and breadth confirmations of the trend (all must pass when set).

## `wma`

`src/engine/xs.rs:337`

```rust
fn wma(v: &[f64], n: usize) -> f64
```

Weighted moving average of the last `n` values (newest weighted most).

## `trend_line`

`src/engine/xs.rs:348`

```rust
fn trend_line(kind: TrendKind, closes: &[f64], bars: usize) -> Option<f64>
```

The trend line of `kind` over `closes` (oldest first, at least `bars` long).

## `efficiency_ratio`

`src/engine/xs.rs:401`

```rust
fn efficiency_ratio(closes: &[f64], n: usize) -> Option<f64>
```

Kaufman's efficiency ratio of the last `n` closes: net move / path length (0-1).

## `adx14`

`src/engine/xs.rs:415`

```rust
fn adx14(m: &Market, sym: usize, t: usize) -> Option<f64>
```

Wilder's ADX(14) at bar t from `m`'s bars of `sym`; None without 120 complete bars.

## `market_context`

`src/engine/xs.rs:469`

```rust
pub fn market_context( m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, ) -> serde_json::Value
```

The entry/hold test for these settings at bar t: the SPY average test (sampled every `trend_step` bars) and, if set, the crash trigger. Everything observable about the market at decision bar t, for the research archive: the trend filter's inputs, market-wide state (dispersion, breadth, SPY returns/slope) and every coin's signal value, price, returns, ATR and volume ratio. Closed bars only.

## `trend_snapshot`

`src/engine/xs.rs:537`

```rust
fn trend_snapshot(m: &Market, t: usize, p: &XsParams) -> serde_json::Value
```

SPY trend state for the decision log (what the filter saw).

## `trend_ok`

`src/engine/xs.rs:567`

```rust
fn trend_ok(m: &Market, t: usize, p: &XsParams) -> bool
```

The ENTRY trend test (entry-only options and confirmations apply).

## `trend_ok_margin`

`src/engine/xs.rs:572`

```rust
fn trend_ok_margin(m: &Market, t: usize, p: &XsParams, margin_pct: f64, entry: bool) -> bool
```

The trend test. `entry = false` is the EXIT test ("trend turned down"): it ignores every entry-only option and confirmation.

## `slots`

`src/engine/xs.rs:650`  (method / nested)

```rust
pub fn slots(&self) -> usize
```

Positions held at a full rebalance: `top` longs, plus `top` shorts unless long only.

## `gate_value`

`src/engine/xs.rs:870`

```rust
pub fn gate_value(m: &Market, s: usize, t: usize, g: &EntryGate) -> Option<f64>
```

The gate feature of stock `s` at the decision close `t` (None when it cannot be measured: the gate then fails closed).

## `ref_vol_expansion`

`src/engine/xs.rs:913`

```rust
fn ref_vol_expansion(m: &Market, spy: usize, t: usize) -> Option<f64>
```

SPY's ATR % at decision close `t` over the mean ATR % of the four 14-bar windows before it (None when any is unmeasured).

## `one`

`src/engine/xs.rs:1099`

```rust
fn one() -> f64
```

Serde default: 1.0.

## `yes`

`src/engine/xs.rs:1104`

```rust
fn yes() -> bool
```

Serde default: true.

## `realized_vol`

`src/engine/xs.rs:1110`

```rust
fn realized_vol(m: &Market, sym: usize, t: usize) -> Option<f64>
```

Standard deviation of 15m log returns over the trading day ending at bar `t` (None with fewer than half that many returns).

## `decision_weights`

`src/engine/xs.rs:1127`

```rust
fn decision_weights(m: &Market, t: usize, target: &BTreeMap<usize, Side>) -> BTreeMap<String, f64>
```

Inverse-volatility weights at decision bar t, normalised to mean 1 within each side so both sides keep equal notional; a target without enough history gets weight 1 (equal size).

## `graded_multipliers`

`src/engine/xs.rs:1154`

```rust
fn graded_multipliers(n: usize, grade_rank: Option<f64>, trend_mult: f64) -> Vec<f64>
```

Size multipliers (best-ranked first), all in (0, 1]: rank grading shrinks lower-ranked legs, trend grading scales every leg.

## `afford_floor`

`src/engine/xs.rs:1167`

```rust
fn afford_floor(mult: f64, price: f64, slot: f64) -> f64
```

A leg's multiplier raised so that its budget still buys one whole share (never above 1).

## `effective_stop_pct`

`src/engine/xs.rs:1172`

```rust
fn effective_stop_pct(m: &Market, s: usize, t: usize, p: &XsParams) -> Option<f64>
```

The tightest percentage distance at which a new long would be stopped (hard stop, close stop, ATR stop), None when it has no stop.

## `risk_multiplier`

`src/engine/xs.rs:1182`

```rust
fn risk_multiplier(risk_pct: f64, equity: f64, stop_pct: f64, slot: f64) -> f64
```

Risk-based multiplier of one leg: the slot is reduced so that its stop loses at most `risk_pct` of `equity`; never above 1.

## `loss_pause_active`

`src/engine/xs.rs:1192`

```rust
fn loss_pause_active(trades: &[Trade], rule: Option<(usize, f64)>, now_ms: i64) -> bool
```

Drawdown tier of the new-entry size: Some(multiplier), or None = no new entries (the third tier). Whether the loss-streak pause blocks entries at `now_ms`: the last `streak` closed trades all lost and the last one closed no more than `hours` ago. `trades` are in closing order.

## `roll_stop_active`

`src/engine/xs.rs:1203`

```rust
fn roll_stop_active(trades: &[Trade], rule: Option<(f64, f64)>, equity: f64, now_ms: i64) -> bool
```

Whether the rolling-loss stop blocks entries at `now_ms`: the pnl of trades closed within the last `hours` is below `-pct` % of the equity at the start of that window (`equity` now minus that pnl). `trades` are in closing order.

## `drawdown_multiplier`

`src/engine/xs.rs:1214`

```rust
fn drawdown_multiplier(dd_pct: f64, tiers: (f64, f64, f64)) -> Option<f64>
```

_No comment in the code; the signature above is the description._

## `grade_weights`

`src/engine/xs.rs:1227`

```rust
fn grade_weights(m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, target: &BTreeMap<usize, Side>, slot: f64, equity: f64, dd_mult: f64, weights: &mut BTreeMap<String, f64>)
```

Graduated sizing for the targets of a rebalance decision: multiplies the legs' weights (see `Risk::grade_rank` / `grade_trend`).

## `is_rebalance`

`src/engine/xs.rs:1248`

```rust
fn is_rebalance(ts: i64, hold: usize) -> bool
```

True when bar t's close is a rebalance point (timestamp-aligned).

## `funding_hourly`

`src/engine/xs.rs:1254`

```rust
fn funding_hourly(f: &[(i64, f64)], ts: i64) -> Option<f64>
```

Average hourly rate of the last 3 funding settlements at or before `ts` (intervals differ per symbol: 1h, 4h or 8h, so normalise per hour).

## `signal_value`

`src/engine/xs.rs:1270`

```rust
pub fn signal_value( m: &Market, scores: &[Vec<Option<Score>>], s: usize, t: usize, p: &XsParams, ) -> Option<f64>
```

Ranking value of symbol s at bar t (None = not rankable now).

## `avg_close`

`src/engine/xs.rs:1321`

```rust
fn avg_close(m: &Market, s: usize, end: usize, n: usize) -> Option<f64>
```

Average close of symbol `s` over the `n` bars ending at `end` (None if any bar is missing).

## `zone_market`

`src/engine/xs.rs:1333`

```rust
fn zone_market(m: &Market, t: usize) -> Option<ZoneMarket>
```

Market state for the pullback zone at bar `t`: SPY's and the universe's recent returns.

## `zone_blocks`

`src/engine/xs.rs:1363`

```rust
fn zone_blocks(m: &Market, t: usize, s: usize, r: &Risk, mk: Option<&ZoneMarket>) -> bool
```

True when a loss zone says: do not enter `s` now (see `Risk` zone fields).

## `ml_gate_blocks`

`src/engine/xs.rs:1407`

```rust
fn ml_gate_blocks(scores: &[Vec<Option<Score>>], s: usize, t: usize, pctl: i64) -> bool
```

Research: true when the stock's ML score is below the `pctl`-th percentile of the scored stocks at bar `t` (needs at least 5 scored; no score = not blocked).

## `candidate_value`

`src/engine/xs.rs:1423`

```rust
fn candidate_value( m: &Market, scores: &[Vec<Option<Score>>], s: usize, t: usize, p: &XsParams, budget: Option<(f64, f64)>, mk: Option<&ZoneMarket>, ) -> Result<f64, &'static str>
```

Why `s` can or cannot be held as a long entry at decision bar t.

## `ranked_targets`

`src/engine/xs.rs:1499`

```rust
fn ranked_targets( m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, budget: Option<(f64, f64)>, ) -> BTreeMap<usize, Side>
```

Targets of a rebalance at bar `t`: the best-ranked eligible symbols per side.

## `ranked_targets_notes`

`src/engine/xs.rs:1510`

```rust
fn ranked_targets_notes( m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, budget: Option<(f64, f64)>, mut notes: Option<&mut Vec<CandidateNote>>, ) -> BTreeMap<usize, Side>
```

`ranked_targets`, optionally explaining every coin's fate into `notes`.

## `funded_targets`

`src/engine/xs.rs:1623`

```rust
fn funded_targets( m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, equity: f64, cost_mult: f64, ) -> (BTreeMap<usize, Side>, usize, f64)
```

Reduce the basket until the balance can fund the contracts' actual lot rules.

Notes from the code: Note: Balance-aware sizing for every account size: tries the configured basket, then smaller ones, until each slot passes the real lot, minimum, depth and leverage checks. Returns the slot budget so execution reuses it exactly. User rule (2026-10-03): NEVER halve the budget. Every slot gets the full share of the free balance: no half size after drawdowns, no half slot reserved for an average-down add (an add only fills from free balance that happens to remain).

## `funded_targets_notes`

`src/engine/xs.rs:1648`

```rust
fn funded_targets_notes( m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, equity: f64, cost_mult: f64, ) -> (BTreeMap<usize, Side>, usize, f64, Vec<CandidateNote>)
```

`funded_targets` plus the explanation of every coin's fate at the basket size actually used.

## `new`

`src/engine/xs.rs:1683`  (method / nested)

```rust
pub fn new(equity: f64) -> Self
```

Empty portfolio starting with `equity` USD.

## `enable_events`

`src/engine/xs.rs:1718`  (method / nested)

```rust
pub fn enable_events(&mut self)
```

Start recording decisions, skipped/denied entries and gates.

## `take_events`

`src/engine/xs.rs:1725`  (method / nested)

```rust
pub fn take_events(&mut self) -> Vec<EngineEvent>
```

Take the events recorded since the last call (empty when logging is off).

## `logging`

`src/engine/xs.rs:1730`  (method / nested)

```rust
fn logging(&self) -> bool
```

Whether this portfolio records engine events.

## `log`

`src/engine/xs.rs:1735`  (method / nested)

```rust
fn log( &mut self, ts_ms: i64, kind: &'static str, symbol: Option<&str>, detail: serde_json::Value, )
```

Records one engine event (at most 20,000 per portfolio).

## `with_cost_mult`

`src/engine/xs.rs:1755`  (method / nested)

```rust
pub fn with_cost_mult(mut self, k: f64) -> Self
```

Sets the fee/slippage multiplier (stress tests use 1.5).

## `reindex`

`src/engine/xs.rs:1776`  (method / nested)

```rust
pub fn reindex(&mut self, symbols: &[String])
```

Point positions and pending targets at `symbols` by NAME. The live symbol list is re-fetched every bar, so indices shift when a symbol joins the list; a held position whose symbol is gone fails explicitly: exchange settlement data is required, and a last-mark exit must never be invented.

## `close`

`src/engine/xs.rs:1806`  (method / nested)

```rust
fn close(&mut self, i: usize, ts: i64, price: f64, reason: &str)
```

Close position i at `price` (already including any market-order cost), paying this account's commission for the exit size (rules measured at entry; the entry's rate only if those are missing).

## `held_rules`

`src/engine/xs.rs:1844`  (method / nested)

```rust
fn held_rules<'a>(&'a self, m: &'a Market, i: usize) -> Option<&'a super::rules::Rules>
```

The rules to manage held position `i` with: fresh, else those measured at entry.

Notes from the code: Note: A HELD position must always stay manageable (exits, funding, liquidation) even when the market has no fresh rules for it, e.g. after turnover moved it out of the measured top 20 or a book measurement failed. Fresh rules are used when present; otherwise the rules measured at entry (real, older). New orders (entries, adds) still require fresh rules. The service also measures held symbols every refresh, so the fallback is a last resort.

## `exit_price`

`src/engine/xs.rs:1851`  (method / nested)

```rust
fn exit_price(&self, m: &Market, i: usize, reference: f64) -> Option<f64>
```

A close must fit the measured book. Extrapolated or nonpositive prices invalidate the simulation rather than becoming invented fills.

## `close_market`

`src/engine/xs.rs:1861`  (method / nested)

```rust
fn close_market( &mut self, m: &Market, i: usize, ts: i64, reference: f64, reason: &str, ) -> bool
```

Closes position `i` at the market exit price (fees and book cost); false when no exit price exists.

## `charge_funding`

`src/engine/xs.rs:1885`  (method / nested)

```rust
fn charge_funding(&mut self, m: &Market, i: usize, ts: i64, rate: f64, mark: f64)
```

Funding consumes free cash first, then this position's isolated margin.

Notes from the code: Note: Funding (none for US stocks: always zero) is valued at the MARK price and debited from free balance first, then the isolated margin, which moves the liquidation price. Never value it at traded prices.

## `liquidate`

`src/engine/xs.rs:1910`  (method / nested)

```rust
fn liquidate(&mut self, i: usize, ts: i64)
```

Closes position `i` at its liquidation price; the loss is floored at margin plus fees.

## `live_check`

`src/engine/xs.rs:1930`  (method / nested)

```rust
pub fn live_check( &mut self, marks: &std::collections::HashMap<String, f64>, now_ms: i64, ) -> Vec<String>
```

The live monitor's check on fresh mark prices (`marks`: symbol -> mark). Locks in an exit for every position whose stop the mark has reached and, if marked equity is `DRAWDOWN_STOP_PCT` below its peak, stops out: every position gets an exit and nothing new opens. Returns what it did.

Notes from the code: Note: Only LOCKS IN exits (`live_exit`); it never closes positions or moves cash itself, because funding for the current bar is charged when the bar is stepped. Pass MARK prices only (wick-safe), never last-trade prices.

## `reclock`

`src/engine/xs.rs:2026`  (method / nested)

```rust
pub fn reclock(&mut self, f: impl Fn(i64) -> anyhow::Result<i64>) -> anyhow::Result<()>
```

Cancel queued entries when the report changes; retain risk exits. Move a FLAT portfolio onto another bar clock: every stored clock time is mapped by `f`.

Notes from the code: Note: Interval switch (paper trades the best interval, user request 2026-10-06): the service first closes every position at real prices, then moves the flat portfolio onto the new interval's clock. Closed trades keep their results (times re-mapped for display); pending decisions are dropped, so the new interval's next close decides afresh. Equity, peak and drawdown carry over.

## `install_entry_gate`

`src/engine/xs.rs:2047`  (method / nested)

```rust
pub fn install_entry_gate(&mut self, allowed: bool)
```

Turns entries on or off after a settings change and clears pending targets.

## `available`

`src/engine/xs.rs:2070`  (method / nested)

```rust
pub fn available(&self) -> f64
```

Free balance: equity minus this bot's posted margins and the real account's margin committed elsewhere.

## `skip_entry`

`src/engine/xs.rs:2079`  (method / nested)

```rust
fn skip_entry( &mut self, m: &Market, t: usize, sym: usize, why: &str, extra: serde_json::Value, )
```

Market order at bar t's open spending `budget` USDT on margin + fee, under moomoo lot rules, the measured order-book cost for this size, the account's taker fee and the real margin tier. Not placed when the symbol has no rules, the book is too thin, or the account lacks the cash. Why an entry order was not placed (event log only).

## `open`

`src/engine/xs.rs:2099`  (method / nested)

```rust
fn open(&mut self, m: &Market, t: usize, sym: usize, side: Side, budget: f64, p: &XsParams)
```

Opens a position in `sym` at bar `t`'s open with `budget` (rules, book cost, fees, whole shares); skips with a logged reason when it cannot.

## `add_skipped`

`src/engine/xs.rs:2258`  (method / nested)

```rust
fn add_skipped(&mut self, m: &Market, t: usize, i: usize, why: &str)
```

Add the position's current size again at bar t's open (same rules as `open`); entry, margin, liquidation and stop move to the new average.

## `add`

`src/engine/xs.rs:2271`  (method / nested)

```rust
fn add(&mut self, m: &Market, t: usize, i: usize, p: &XsParams)
```

Adds to position `i` at bar `t`'s open (same size again), if the rules and book allow it.

## `step`

`src/engine/xs.rs:2342`  (method / nested)

```rust
pub fn step(&mut self, m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams)
```

Advances the portfolio by bar `t`: fills pending orders at the open, then exits, stops and the rebalance decision at the close.

## `decide_close`

`src/engine/xs.rs:2865`  (method / nested)

```rust
pub fn decide_close( &mut self, m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, )
```

Queue a decision using only information available at this close.

Notes from the code: Note: Decisions use only data up to bar t and fill at a later open (`defer_new_decisions` live). Targets, parameters and slot budget are frozen here; `install_entry_gate` cancels them if the settings or the gate change.

## `executable_targets`

`src/engine/xs.rs:2995`  (method / nested)

```rust
pub fn executable_targets( &self, m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams, ) -> BTreeMap<usize, Side>
```

The targets the portfolio could fund now at bar `t` (none when stopped out, entries off or the trend filter blocks).

## `defer_new_decisions`

`src/engine/xs.rs:3018`  (method / nested)

```rust
pub fn defer_new_decisions(&mut self, ready_ts: i64, decision_bar_ts: i64)
```

Live reports/data received after a candle closes can first fill at a future opening boundary. Backtests leave this delay unset.

## `validate_state`

`src/engine/xs.rs:3036`  (method / nested)

```rust
pub fn validate_state(&self) -> anyhow::Result<()>
```

Checks every money field is finite and the state is consistent; errors otherwise.

## `close_all`

`src/engine/xs.rs:3098`  (method / nested)

```rust
pub fn close_all(&mut self, m: &Market, ts: i64, reason: &str)
```

Close every position at its last mark (bar close) with its measured order-book cost and fee.

## `unrealized`

`src/engine/xs.rs:3138`  (method / nested)

```rust
pub fn unrealized(&self) -> f64
```

Open unrealized PnL of all positions at their marks.

## `metrics`

`src/engine/xs.rs:3146`  (method / nested)

```rust
pub fn metrics(&self) -> Metrics
```

Performance metrics of this portfolio (equity, trades, drawdown, profit factor).

## `backtest`

`src/engine/xs.rs:3272`

```rust
pub fn backtest( m: &Market, scores: &[Vec<Option<Score>>], range: std::ops::Range<usize>, p: &XsParams, equity: f64, ) -> XsPortfolio
```

Backtest of settings `p` over `range` from `equity` with normal costs.

## `backtest_costs`

`src/engine/xs.rs:3285`

```rust
pub fn backtest_costs( m: &Market, scores: &[Vec<Option<Score>>], range: std::ops::Range<usize>, p: &XsParams, equity: f64, cost_mult: f64, ) -> XsPortfolio
```

Backtest with a fee/slippage multiplier. Positions still open at the end are closed at the last close (with fees), so net and profit factor always come from the same closed trades.

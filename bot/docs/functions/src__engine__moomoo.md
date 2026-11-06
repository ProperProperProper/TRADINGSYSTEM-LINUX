# `src/engine/moomoo.rs`

> Real moomoo data and PAPER trading through the local OpenD gateway (`crate::opend`):
> * the US paper account (found via Trd_GetAccList, never a REAL account), its
> funds, positions, orders and order fees,
> * the universe: SPY's holdings (data/spy_holdings.csv) by weight, NYSE / Nasdaq listings only, nothing delisted
> or unknown to moomoo,
> * the session calendar (Qot_RequestTradeDate, whole and half days) and the trading clock built
> from it (`Sessions`), checked against the reference's candles,
> * closed 15-minute regular-session candles (Qot_RequestHistoryKL, unadjusted), converted from
> moomoo's close-time labels to the engine's open-time convention,
> * per-symbol rules: lot size, quote (bid/ask and sizes from the snapshot), fees of this account's
> real paper orders,
> * the 7-day historical-candle quota (`QuotaState`): a symbol not pulled in the window is pulled
> only while quota remains.
>
> Nothing here invents a value: a request that fails is an error, a symbol with incomplete data
> or rules sits out (and the service logs why).

68 functions (tests excluded). Generated from the source by `qwen-tools gen-function-docs`; regenerate after code changes.

## `load_holdings`

`src/engine/moomoo.rs:39`

```rust
pub fn load_holdings(path: &std::path::Path) -> Option<Vec<String>>
```

Tickers of SPY's holdings by weight, best first, from the installed holdings file (`rank,ticker,name,weight_pct`, `#` comment lines).

## `ms_now`

`src/engine/moomoo.rs:73`

```rust
pub fn ms_now() -> i64
```

Current wall time in milliseconds.

## `select_paper_account`

`src/engine/moomoo.rs:86`

```rust
pub fn select_paper_account(list: &[pb::trd_common::TrdAcc], preferred: Option<u64>) -> Result<u64>
```

The US paper account: exactly one SIMULATE account with US trading rights and a stock sim type.

Notes from the code: Note: User rule (2026-10-06): NEVER send an order or an unlock to a REAL account. The only account this bot trades is the US SIMULATE (paper) account that Trd_GetAccList reports; `guard_order_header` runs before every trading write and refuses anything else. The bot never sends Trd_UnlockTrade (paper needs no unlock).

## `guard_order_header`

`src/engine/moomoo.rs:112`

```rust
pub fn guard_order_header(h: &pb::trd_common::TrdHeader, paper: u64) -> Result<()>
```

Refuse any trading write that is not for the US SIMULATE account `paper`.

## `leveraged_or_inverse`

`src/engine/moomoo.rs:161`

```rust
pub fn leveraged_or_inverse(name: &str) -> bool
```

Whether an ETF's name marks it as leveraged or inverse.

Notes from the code: Note: User rule: NO leverage, long only. Leveraged (2x/3x) and inverse ETFs carry leverage or short exposure inside the fund, so they are never traded. Identified from the fund name (moomoo publishes no leverage flag).

## `ineligible`

`src/engine/moomoo.rs:174`

```rust
pub fn ineligible(b: &pb::qot_common::SecurityStaticBasic) -> Option<String>
```

Why a security may not be a candidate (None = eligible): NYSE / Nasdaq listing, common stock or a non-leveraged, non-inverse fund, not delisting, a positive lot size.

## `of`

`src/engine/moomoo.rs:199`  (method / nested)

```rust
fn of(b: &pb::qot_common::SecurityStaticBasic, avg_turnover: Option<f64>) -> Listing
```

Listing from moomoo's static info.

## `screen`

`src/engine/moomoo.rs:213`

```rust
pub fn screen(rows: &[(String, f64)], info: &HashMap<String, pb::qot_common::SecurityStaticBasic>, cap: usize) -> (Vec<Listing>, Vec<(String, String)>)
```

The candidate pool from screener rows (best first) and their static information: eligible rows in screener order, at most `cap`; the rest with the reason they were left out.

## `quota_allows`

`src/engine/moomoo.rs:248`

```rust
pub fn quota_allows(q: &QuotaState, symbol: &str, reserve: i32) -> bool
```

Whether candles of `symbol` may be requested now. Re-pulling a symbol already used in the 7-day window is free; a new symbol needs remaining quota above `reserve` (0 for symbols that must be read: held positions and the reference).

## `et_ms`

`src/engine/moomoo.rs:268`

```rust
fn et_ms(date: NaiveDate, hm: (u32, u32)) -> Result<i64>
```

Wall time (ms) of `hm` (hour, minute) US/Eastern on `date`.

## `session_bounds`

`src/engine/moomoo.rs:278`

```rust
pub fn session_bounds(date: NaiveDate, kind: i32) -> Result<(i64, i64)>
```

Wall (open, close) of the regular session on `date` of moomoo trade-date type `kind`.

## `candle_open_ms`

`src/engine/moomoo.rs:291`

```rust
pub fn candle_open_ms(time: &str) -> Result<i64>
```

UTC open time (ms) of the 15-minute candle moomoo labels `time` (US/Eastern close time).

Notes from the code: Note: moomoo labels a 15-minute candle with its CLOSE time in US/Eastern ("09:45:00" is the 09:30-09:45 bar; a full session has 26, the last labelled "16:00:00"). The engine keys bars by OPEN time (UTC ms), so the open is the label minus 15 minutes.

## `check_calendar`

`src/engine/moomoo.rs:306`

```rust
pub fn check_calendar(sessions: &Sessions, candles: &BTreeMap<i64, Bar>, from: i64, to: i64) -> (Vec<(i64, i64)>, Vec<String>)
```

Check the calendar against the reference's real candles: every session fully inside [`from`, `to`) must have exactly its 15-minute bars (first at the open, last ending at the close), and no candle may fall outside a session. Returns (sessions with no candles at all: unscheduled closures the calendar does not know, problems).

## `fmt_et`

`src/engine/moomoo.rs:333`

```rust
pub fn fmt_et(ms: i64) -> String
```

Formats a wall time (ms) as "YYYY-MM-DD HH:MM ET".

## `et_date`

`src/engine/moomoo.rs:340`

```rust
pub fn et_date(ms: i64) -> NaiveDate
```

ET date of a wall time.

## `order_final`

`src/engine/moomoo.rs:369`

```rust
pub fn order_final(status: i32) -> bool
```

Trd_Common.OrderStatus values after which an order never changes again.

## `of`

`src/engine/moomoo.rs:375`  (method / nested)

```rust
fn of(o: &pb::trd_common::Order) -> OrderState
```

Order state from moomoo's order record.

## `mid`

`src/engine/moomoo.rs:415`  (method / nested)

```rust
pub fn mid(&self) -> f64
```

Quote midpoint: (bid + ask) / 2.

## `valid`

`src/engine/moomoo.rs:419`  (method / nested)

```rust
pub fn valid(&self) -> bool
```

A two-sided, uncrossed quote with sizes.

## `derived_fee`

`src/engine/moomoo.rs:440`

```rust
pub fn derived_fee(side: i32, value: f64, cash_before: f64, cash_after: f64) -> f64
```

The fee of one fill from the paper account's cash before and after it: a buy costs its value plus fees, a sell brings its value minus fees. `side`: 1 buy, 2 sell.

Notes from the code: Note: moomoo has no pre-trade fee quote, and Trd_GetOrderFee answers "Paper trading is not supported" for the SIMULATE account (verified 2026-10-06). The paper account does charge fees: its cash moves by the fill value plus fees (INTC test orders: buy 3 -> 0.99 USD, sell 3 -> 1.00 USD). Each order's fee is therefore measured as the cash change around its fill minus the fill value (`derived_fee`; orders are placed one at a time for that). The commission ladder is built from those real observations (order notional, fee; buys and sells mixed, the larger fee at equal notionals). Where no real fee exists, the COMMISSION IS EXCLUDED from the simulation (user rule 2026-10-06: exclude, never invent) and labelled so everywhere ("excluded: ..."); real fills still carry their real fees.

## `commission_ladder`

`src/engine/moomoo.rs:455`

```rust
pub fn commission_ladder(points: &[FeePoint]) -> (Vec<(f64, f64)>, String)
```

The commission ladder for the engine from real fee observations: ascending notional, non-decreasing fee; with its source label. No observation: the commission is excluded (zero over every size) and labelled as such.

## `rate_limit`

`src/engine/moomoo.rs:488`

```rust
fn rate_limit(proto: u32) -> usize
```

Requests per 30 seconds this client allows itself per protocol (below moomoo's limits).

## `connect`

`src/engine/moomoo.rs:522`  (method / nested)

```rust
pub async fn connect(client_id: &str) -> Result<Gateway>
```

Connect to OpenD, check it is logged in, and select the US paper account.

## `header`

`src/engine/moomoo.rs:544`  (method / nested)

```rust
fn header(&self) -> pb::trd_common::TrdHeader
```

Trading header for the US SIMULATE (paper) account; every trading request uses it.

## `call`

`src/engine/moomoo.rs:550`  (method / nested)

```rust
pub async fn call<R: crate::opend::Reply>(&self, proto: u32, req: &impl prost::Message) -> Result<R::S2c>
```

One request (rate-limited per protocol); connects on demand. A transport failure drops the connection so the next call reconnects; a refusal from OpenD keeps it.

## `keep_alive`

`src/engine/moomoo.rs:586`  (method / nested)

```rust
pub async fn keep_alive(&self) -> Result<()>
```

The keep-alive OpenD expects every `keep_alive_secs` (proto 1004).

## `keep_alive_secs`

`src/engine/moomoo.rs:593`  (method / nested)

```rust
pub async fn keep_alive_secs(&self) -> Option<i32>
```

OpenD's keep-alive interval (seconds) of the current connection.

## `alive`

`src/engine/moomoo.rs:598`  (method / nested)

```rust
pub async fn alive(&self) -> bool
```

Whether OpenD answers and is logged in for both quotes and trading.

## `account`

`src/engine/moomoo.rs:606`  (method / nested)

```rust
pub async fn account(&self) -> Result<Account>
```

The paper account's funds (total assets, cash, buying power, market value) now.

## `positions`

`src/engine/moomoo.rs:619`  (method / nested)

```rust
pub async fn positions(&self) -> Result<Vec<Held>>
```

The paper account's current positions.

## `trade_dates`

`src/engine/moomoo.rs:648`  (method / nested)

```rust
async fn trade_dates(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<(NaiveDate, i32)>>
```

moomoo's US trade dates from `from` to `to` (ET dates), with their type, in chunks (one request covers at most a year).

## `sessions`

`src/engine/moomoo.rs:671`  (method / nested)

```rust
pub async fn sessions(&self) -> Result<Sessions>
```

Regular sessions from `CLOCK_EPOCH` through the coming weeks, from moomoo's calendar.

## `sessions_from`

`src/engine/moomoo.rs:680`  (method / nested)

```rust
pub async fn sessions_from(&self, from: NaiveDate) -> Result<Sessions>
```

Regular sessions from the first trading day on or after `from` through the coming weeks (research calendars that reach further back than the live clock epoch; the live service always uses `sessions`).

## `universe_from`

`src/engine/moomoo.rs:688`  (method / nested)

```rust
pub async fn universe_from(&self, tickers: &[String], cap: usize) -> Result<(Vec<Listing>, Vec<(String, String)>)>
```

The universe from a ticker list (SPY's holdings, best first): eligible listings in that order, at most `cap`, plus the excluded ones with reasons.

## `static_info`

`src/engine/moomoo.rs:700`  (method / nested)

```rust
pub async fn static_info(&self, symbols: &[String]) -> Result<HashMap<String, pb::qot_common::SecurityStaticBasic>>
```

Static information of `symbols` (US), keyed by symbol.

## `listing`

`src/engine/moomoo.rs:725`  (method / nested)

```rust
pub async fn listing(&self, symbol: &str) -> Result<Listing>
```

Static information of one symbol, which must be an eligible listing.

## `quota`

`src/engine/moomoo.rs:735`  (method / nested)

```rust
pub async fn quota(&self) -> Result<QuotaState>
```

The historical-candle quota now: used, remaining and which stocks were pulled when.

## `bars`

`src/engine/moomoo.rs:753`  (method / nested)

```rust
pub async fn bars(&self, symbol: &str, from: i64, sessions: &Sessions) -> Result<(BTreeMap<i64, Bar>, usize)>
```

Closed 15-minute regular-session candles of `symbol` from the ET date of `from` through today, keyed by wall OPEN time (ms). The candle still forming is dropped; blank candles are missing. Candles outside `sessions` are dropped and counted (second value).

## `quotes`

`src/engine/moomoo.rs:817`  (method / nested)

```rust
pub async fn quotes(&self, symbols: &[String]) -> Result<HashMap<String, Quote>>
```

Snapshot quotes of `symbols` (bid/ask, sizes, last, lot); symbols moomoo omits are absent.

## `place_market`

`src/engine/moomoo.rs:848`  (method / nested)

```rust
pub async fn place_market(&self, symbol: &str, side: i32, qty: f64, remark: &str) -> Result<(u64, String)>
```

Place a MARKET order (day, regular hours) on the paper account. `side`: 1 buy, 2 sell.

## `place_limit_probe`

`src/engine/moomoo.rs:855`  (method / nested)

```rust
pub async fn place_limit_probe(&self, symbol: &str, side: i32, qty: f64, price: f64, remark: &str) -> Result<(u64, String)>
```

Research probe (examples/fractional_check.rs): a LIMIT order on the paper account whose quantity may be fractional. The bot itself never calls this (it trades whole shares).

## `place`

`src/engine/moomoo.rs:861`  (method / nested)

```rust
async fn place(&self, symbol: &str, side: i32, qty: f64, price: Option<f64>, remark: &str) -> Result<(u64, String)>
```

Market order (`price` None) or limit order at `price`, paper account only.

## `cancel`

`src/engine/moomoo.rs:904`  (method / nested)

```rust
pub async fn cancel(&self, order_id: u64) -> Result<()>
```

Cancel an open order on the paper account.

## `orders`

`src/engine/moomoo.rs:940`  (method / nested)

```rust
pub async fn orders(&self, ids: &[u64]) -> Result<Vec<OrderState>>
```

Today's orders of the paper account (moomoo's order list), optionally only `ids`.

## `history_orders`

`src/engine/moomoo.rs:956`  (method / nested)

```rust
pub async fn history_orders(&self, ids: &[u64], from_ms: i64) -> Result<Vec<OrderState>>
```

Orders of earlier days (history order list) from `from_ms` on, optionally only `ids`.

## `order_fees`

`src/engine/moomoo.rs:981`  (method / nested)

```rust
pub async fn order_fees(&self, ids_ex: &[String]) -> Result<Vec<OrderFee>>
```

Fees moomoo charged the orders `ids_ex` (only those it reports).

## `liquidity_from_bars`

`src/engine/moomoo.rs:1004`

```rust
pub fn liquidity_from_bars(bars: &BTreeMap<i64, Bar>, sessions: usize) -> Option<(f64, f64)>
```

Average daily dollar turnover and daily return volatility (a fraction) of a symbol from its REAL 15-minute traded candles: the last `sessions` days that have at least 20 of a session's 26 candles (a half day or a gap is not mistaken for a quiet day). None when fewer than 5 such days.

## `rules_from_quote`

`src/engine/moomoo.rs:1030`

```rust
pub fn rules_from_quote(l: &Listing, q: &Quote, fees: &[FeePoint], now: i64) -> Result<Rules>
```

Rules of `l` from a real quote and this account's real fee observations: whole shares in lots of `lot_size`, the quote's displayed depth, the commission ladder (`commission_ladder`).

## `new`

`src/engine/moomoo.rs:1076`  (method / nested)

```rust
pub fn new(list: Vec<(i64, i64)>) -> Result<Sessions>
```

Session calendar of 15m candles from (open, close) wall pairs.

## `build`

`src/engine/moomoo.rs:1081`  (method / nested)

```rust
fn build(list: Vec<(i64, i64)>, k: i64) -> Result<Sessions>
```

Session calendar for candles of `k` 15m bars; checks sessions are whole bars and do not overlap.

## `without`

`src/engine/moomoo.rs:1096`  (method / nested)

```rust
pub fn without(&self, drop: &[(i64, i64)]) -> Result<Sessions>
```

The calendar without the sessions in `drop` (unscheduled closures found in the candles).

## `list`

`src/engine/moomoo.rs:1101`  (method / nested)

```rust
pub fn list(&self) -> &[(i64, i64)]
```

The sessions (wall open, close).

## `with_interval`

`src/engine/moomoo.rs:1106`  (method / nested)

```rust
pub fn with_interval(&self, interval: crate::engine::Interval) -> Sessions
```

The same calendar with candles of `interval` (aligned to each session's open).

## `step`

`src/engine/moomoo.rs:1112`  (method / nested)

```rust
fn step(&self) -> i64
```

Candle length (ms).

## `candle_end`

`src/engine/moomoo.rs:1117`  (method / nested)

```rust
fn candle_end(&self, open: i64, b: i64) -> i64
```

Wall close of the candle opening at `open` inside session `(a, b)`.

## `clock_at`

`src/engine/moomoo.rs:1123`  (method / nested)

```rust
pub fn clock_at(&self, wall: i64) -> i64
```

Clock time of the candle containing `wall`; between sessions, the next session's first candle (the clock skips the gap). Display/re-mapping only.

## `candle_bounds`

`src/engine/moomoo.rs:1142`  (method / nested)

```rust
pub fn candle_bounds(&self, open: i64) -> Option<(i64, i64)>
```

Wall [open, close) of the candle opening at `open`; None off-grid or outside a session.

## `origin`

`src/engine/moomoo.rs:1149`  (method / nested)

```rust
fn origin(&self) -> i64
```

Clock origin (ms): the epoch session's open.

## `clock`

`src/engine/moomoo.rs:1154`  (method / nested)

```rust
pub fn clock(&self, wall: i64) -> Option<i64>
```

Trading-clock time of the bar (candle) opening at `wall`; None outside a session or off-grid.

## `wall`

`src/engine/moomoo.rs:1162`  (method / nested)

```rust
pub fn wall(&self, clock: i64) -> Option<i64>
```

Wall open time of the bar (candle) at trading-clock time `clock`.

## `sessions_from`

`src/engine/moomoo.rs:1185`  (method / nested)

```rust
pub fn sessions_from(&self, wall: i64, now: i64) -> usize
```

Sessions from the one containing (or following) `wall` through the last one started by `now`.

## `open_session`

`src/engine/moomoo.rs:1190`  (method / nested)

```rust
pub fn open_session(&self, wall: i64) -> Option<(i64, i64)>
```

The session containing `wall`, if the market is open then.

## `next_open`

`src/engine/moomoo.rs:1195`  (method / nested)

```rust
pub fn next_open(&self, wall: i64) -> Option<i64>
```

The next session opening after `wall`.

## `last_closed_bars`

`src/engine/moomoo.rs:1200`  (method / nested)

```rust
pub fn last_closed_bars(&self, now: i64, n: usize) -> Vec<i64>
```

Wall open times of the last `n` closed bars (candles) at `now`, ascending.

## `close_of`

`src/engine/moomoo.rs:1219`  (method / nested)

```rust
pub fn close_of(&self, open: i64) -> Option<i64>
```

Wall close of the bar (candle) opening at `open` (None off-grid).

## `clock_now`

`src/engine/moomoo.rs:1224`  (method / nested)

```rust
pub fn clock_now(&self, now: i64) -> Option<i64>
```

The trading-clock time of the next bar (candle) to open after `now` (for live fill bounds).

## `aggregate`

`src/engine/moomoo.rs:1249`

```rust
fn aggregate(bars: &BTreeMap<i64, Bar>, a: i64, b: i64) -> Option<Bar>
```

One candle [a, b) from closed 15-minute bars: open of the first, extremes of all, close of the last, summed volume and turnover. None unless EVERY 15-minute bar of the candle is present (a gap is never papered over).

## `build_market`

`src/engine/moomoo.rs:1273`

```rust
pub fn build_market(sessions: &Sessions, walls: &[i64], data: &[SymbolData]) -> Result<Market>
```

The market over the bars (candles of `sessions`' interval) opening at `walls` (ascending, all inside sessions), built from closed 15-minute bars.

Notes from the code: Note: moomoo publishes no bid/ask MIDPOINT candles. The engine's mark candles are therefore the same real TRADED candles: stops, valuation and the entry wick guard use traded prices (the wick guard, which compares the traded open with the mark open, can then never fire). Never invented: it is the only real candle series moomoo provides.

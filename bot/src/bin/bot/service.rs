//! Long-only PAPER-trading service for US stocks on moomoo: a cross-sectional mean-reversion
//! engine, with every paper decision executed as a REAL moomoo paper
//! order (US SIMULATE account only) and the engine's paper account reconciled with moomoo's fills,
//! fees and positions (`engine::execution`). It trades a `START_CAPITAL` (5,000 USD) account, never more than the
//! available cash (`capital_from`), with exactly 1 open position on live intervals.
//!
//! bar_task (each regular-session 1m close): the universe (every eligible SPY holding from spy_holdings.csv + SPY, the trend
//! reference) -> the account,
//! closed 1m candles (quota-guarded), hourly rules -> the newest market per interval -> real
//! order states and fees -> the paper account (replay, reconcile, decide) -> orders at the next
//! open of the paper interval (`place_open`).
//! search_task: 200k random combinations for each live interval, champion/challenger; next round 15 min
//! after the last finished. walk_forward_priority_task: causal walk-forward learning continuously
//! from the latest real candles; stability_task: larger closed-market research runs.
//! Console: 127.0.0.1:8789 (`/`, `/api/status`, `/api/signals`, `/api/paper`, `/api/orders`, ...).

use anyhow::{Context, Result};
use log::{error, info, warn};
use moomoo_bot::engine::btarchive::{
    day_tokens, entry_features, params_value, wall_trade, BtArchive, BtRun, Daily,
};
use moomoo_bot::engine::execution::{Ledger, Planned};
use moomoo_bot::engine::live_options;
use moomoo_bot::engine::metrics::Trade;
use moomoo_bot::engine::moomoo::{
    self, fmt_et, Account, Gateway, Held, Listing, QuotaState, Sessions, SymbolData,
};
use moomoo_bot::engine::record::{Record, RecordRow};
use moomoo_bot::engine::rules::Rules;
use moomoo_bot::engine::scores;
use moomoo_bot::engine::store::{PaperPoint, Store};
use moomoo_bot::engine::supervisor::{Health, Heartbeat};
use moomoo_bot::engine::walkforward::{self, Candidate, SearchReport, SearchSpace, Verdict};
use moomoo_bot::engine::xs::{self, EngineEvent, XsParams, XsPortfolio};
use moomoo_bot::engine::{
    governor, Bar, Interval, Market, Side, BARS, BAR_MS, REFERENCE, TEST_SESSIONS,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const PORT: u16 = 8789;
const PAPER_FILE: &str = "paper_xs.json";
/// Console state that would otherwise live only in memory (owner 2026-10-10: everything on disk, nothing only in RAM): the stability
/// and walk-forward results, each interval's latest search (summary and winner curve) and the paper interval choice. Written
/// atomically whenever one changes, loaded at start so a restart shows the last results instead of nothing.
const CONSOLE_STATE_FILE: &str = "console_state.json";
/// Every distinct setting each interval's search chose, with its walk-forward evidence (in memory `candidates`), kept on disk so a
/// restart continues their forward tracks instead of starting over. Written when candidates change, loaded at start.
const CANDIDATES_FILE: &str = "search_candidates.json";
// NOTE(agents): User decision (2026-10-09): trade a 5,000 USD account (was 1,000, then available cash). The moomoo
//               paper account holds ~1,000,000 virtual USD; the bot never sizes from more than START_CAPITAL.
//               The engine's equity starts here and moves only with real fills.
/// The bot's starting capital (USD).
pub use moomoo_bot::engine::START_CAPITAL;

/// The USD the bot trades with: a `START_CAPITAL` (5000 USD) account (user 2026-10-09), never more than the paper account's AVAILABLE
/// cash (long only, cash only, whole shares). Unknown cash disables new sizing; known low cash is used as-is.
fn capital_from(account: &Account) -> f64 {
    let c = account.cash.floor();
    if c.is_finite() && c >= 0.0 {
        c.min(START_CAPITAL)
    } else {
        0.0
    }
}

/// Whole shares of `qty` the cash can pay for at `mid` with the availability slack (never negative).
fn affordable_qty(cash: f64, mid: f64, qty: f64) -> f64 {
    if !(cash.is_finite() && mid > 0.0) {
        return 0.0;
    }
    qty.min((cash / (mid * AVAILABILITY_SLACK)).floor())
        .max(0.0)
}

/// A buy never goes out larger than the cash the account has at that moment (a little slack for price moves and the fee).
const AVAILABILITY_SLACK: f64 = 1.01;
/// Rules (quotes) and the session calendar are re-measured this often; the screener pool too.
const RULES_REFRESH: Duration = Duration::from_secs(3600);
// NOTE(agents): User schedule (2026-10-03): the parameter search runs in its own task and starts
//               again SEARCH_PAUSE after the previous search FINISHED (not on the clock hour, not
//               per candle). The 1m bar loop never waits for it.
/// A search that runs longer is abandoned (the previous result stays in use).
const SEARCH_DEADLINE: Duration = Duration::from_secs(50 * 60);
/// Pause between the end of one search and the start of the next.
const SEARCH_PAUSE: Duration = Duration::from_secs(5 * 60);
/// Retry delay after a failed search.
const SEARCH_RETRY: Duration = Duration::from_secs(5 * 60);
/// The bot exits (launchd restarts it and reconnects) after OpenD has been unreachable this long.
const GATEWAY_DEAD_LIMIT: Duration = Duration::from_secs(5 * 60);
/// OpenD client id of this service.
const CLIENT_ID: &str = "claude-moomoo-bot";
/// How long after a close the bar loop waits for moomoo to publish that bar.
const PUBLISH_WAIT_MS: i64 = 10 * 60_000;
/// Fee requests for a filled order before its fee counts as unavailable (excluded).
const FEE_REQUESTS: u32 = 8;

#[derive(Debug, Clone, Serialize)]
pub struct SignalRow {
    pub symbol: String,
    /// Ranking value of the signal in use (negated when the settings buy the
    /// highest values, so the lowest is always bought first).
    pub signal: f64,
    /// 1 = lowest value (long side).
    pub rank: usize,
    /// Side this symbol would hold at the next rebalance, if any.
    pub target: Option<Side>,
    /// Side held in the paper portfolio now, if any.
    pub held: Option<Side>,
    pub close: f64,
    /// Dollar volume over the last trading day.
    pub turnover_24h: f64,
}

/// The chosen settings' walk-forward evidence and the verdict (research only; never a trade).
#[derive(Debug, Clone, Serialize)]
pub struct WalkForwardView {
    pub candidate: Candidate,
    pub verdict: Verdict,
}

/// The parameter record as shown on the console.
#[derive(Debug, Clone, Serialize)]
pub struct RecordView {
    /// Settings on record, and how many are re-tested as candidates in each search.
    pub count: i64,
    pub tested_per_search: usize,
    pub rows: Vec<RecordRow>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchProgress {
    pub running: bool,
    pub interval: Option<Interval>,
    pub interval_index: usize,
    pub interval_total: usize,
    pub combinations_target: usize,
    pub combinations_evaluated: usize,
    pub completed_intervals: usize,
    pub started_ms: i64,
}

/// Recorded settings re-tested as candidates in every search, and the most evaluated on all windows.
const RECORD_CANDIDATES: usize = 300;
const RECORD_TRACKED: usize = 100;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub strategy: String,
    pub started_ms: i64,
    /// Wall open time of the last processed bar.
    pub last_bar_ts: i64,
    pub next_rebalance_ts: i64,
    pub symbols: usize,
    pub universe: usize,
    pub sync_secs: f64,
    pub report: Option<SearchReport>,
    pub params: Option<XsParams>,
    /// How the settings in use were chosen by the latest search.
    pub choice: Option<walkforward::Choice>,
    /// When the latest parameter search finished (ms; 0 = none yet).
    pub search_finished_ms: i64,
    /// TRADING (new best / kept last best) or NO SAFE SETTINGS.
    pub mode: String,
    /// The candle interval the paper account trades (the best interval's winner).
    pub paper_interval: Option<Interval>,
    /// Why the paper trades that interval (the latest comparison of the interval winners).
    pub interval_note: String,
    /// Walk-forward evidence for the settings in use (in-sample + forward track).
    pub walk_forward: Option<WalkForwardView>,
    /// The persistent record of the best settings ever found and their track record.
    pub record: Option<RecordView>,
    pub paper_equity: f64,
    pub paper_start_equity: f64,
    pub paper_open_positions: usize,
    pub paper_trades: usize,
    pub paper_wins: usize,
    /// Equity change since the start: paper_realized + paper_open.
    pub paper_net: f64,
    /// Cash P&L: closed trades plus fees already paid on open positions.
    pub paper_realized: f64,
    /// Mark-to-market of open positions at the last close (before exit fees).
    pub paper_open: f64,
    pub paper_max_drawdown_pct: f64,
    /// Cooldown expiry; finished exits and valid entry gates are also required.
    pub resume_not_before: Option<i64>,
    pub effective_pairs: usize,
    pub allocation_note: String,
    /// The real moomoo paper account (read every bar; never sized from).
    pub account: Option<Account>,
    /// Positions moomoo reports in the paper account.
    pub broker_positions: Vec<Held>,
    /// Shares this bot's own filled orders account for (per symbol), and mismatches against moomoo.
    pub bot_shares: BTreeMap<String, f64>,
    pub position_mismatches: Vec<String>,
    /// Regular session state from moomoo's calendar.
    pub market_open: bool,
    pub session_close_ms: i64,
    pub next_open_ms: i64,
    /// Calendar check against the reference's candles.
    pub calendar_note: String,
    /// Symbols with complete rules, and when they were last measured.
    pub rules_symbols: usize,
    pub rules_ts: i64,
    /// Where the commission in use comes from (real fees, or excluded).
    pub commission_source: String,
    /// The historical-candle quota (moomoo, distinct stocks per 7 days).
    pub quota: Option<QuotaState>,
    /// Everything left out for missing / ineligible real data, with the reason (symbol -> why).
    pub exclusions: BTreeMap<String, String>,
    /// The next open orders are planned for (wall ms) and what was placed last.
    pub next_order_open_ms: i64,
    pub last_orders_note: String,
    /// State of the market-closed stability test.
    pub history_note: String,
    /// The archive (SQLite) size and its candle rows.
    pub store_bytes: u64,
    pub store_candles: i64,
    pub events: VecDeque<String>,
    pub search_progress: SearchProgress,
}

/// What `place_open` needs to plan the orders of the next open of the paper interval.
#[derive(Clone)]
struct OpenCtx {
    interval: Interval,
    next_ts: i64,
    next_wall: i64,
    market: Arc<Market>,
    sc: Arc<Vec<Vec<Option<scores::Score>>>>,
    params: XsParams,
}

struct App {
    gw: Gateway,
    dir: PathBuf,
    health: Health,
    status: RwLock<Status>,
    /// Permanent SQLite store (archive folder; candles culled only at 10 GB).
    store: Option<Store>,
    /// The live tests' runs (21-session windows, daily walk-forward) and their trades. NOT read by the lab: its training data is
    /// `backtest_archive.sqlite3`, built by examples/lab_export.rs in 14-session look-forward blocks.
    btarchive: Option<BtArchive>,
    /// Persistent record of the best settings per interval (archive folder).
    records: BTreeMap<Interval, Mutex<Option<Record>>>,
    /// This process's run id (its start time, ms) in the store.
    run_id: i64,
    sessions: RwLock<Option<(Instant, Arc<Sessions>)>>,
    /// Sessions removed from moomoo's calendar because the reference has no candles in them.
    closures: Mutex<Vec<(i64, i64)>>,
    /// Measured rules by symbol, and when they were measured.
    rules: Mutex<(Option<Instant>, HashMap<String, Rules>)>,
    /// Static information by symbol.
    listings: Mutex<HashMap<String, Listing>>,
    /// The screener pool and when it was read.
    pool: Mutex<Option<(Instant, Vec<Listing>)>>,
    /// Exclusions with their reason (symbol -> why), shown on the console.
    excluded: Mutex<BTreeMap<String, String>>,
    signals: RwLock<Vec<SignalRow>>,
    paper: RwLock<Option<PaperState>>,
    /// The market-closed stability test per interval (stability_task), newest result.
    research: RwLock<BTreeMap<Interval, serde_json::Value>>,
    /// The market-closed walk-forward test per interval (stability_task), newest result.
    walk_forward: RwLock<BTreeMap<Interval, serde_json::Value>>,
    /// The newest market of every interval and the free balance to size from, for the search.
    frames: RwLock<Option<Frames>>,
    /// Every distinct settings each interval's search has chosen, with its evidence (newest last).
    candidates: RwLock<BTreeMap<Interval, Vec<Candidate>>>,
    /// The latest completed parameter search of each interval.
    searches: RwLock<BTreeMap<Interval, Search>>,
    /// The interval the paper account should trade (best winner of the latest search round);
    /// None until a round has finished or when no interval has profitable safe settings.
    paper_choice: RwLock<Option<Interval>>,
    /// Each interval's latest search as saved on disk (`CONSOLE_STATE_FILE`), shown until a new search finishes.
    restored: RwLock<BTreeMap<Interval, serde_json::Value>>,

    /// Closed 1-minute bars per symbol (deep history fetched once, then only new bars).
    history: Mutex<HashMap<String, History>>,
    /// The next open's order context (placed when it opens).
    open_ctx: Mutex<Option<OpenCtx>>,
    /// Serialises order placement.
    order_lock: tokio::sync::Mutex<()>,
}

/// Cached closed 1-minute bars of one symbol (real moomoo data, also written to the store).
#[derive(Clone)]
struct History {
    trades: BTreeMap<i64, Bar>,
    /// First bar of the deep fetch (a newer listing starts here).
    listing_wall: i64,
}

/// One interval's newest market: its test window, and for 1h / 4h / one session the warm-up
/// candles before it (indicators only).
#[derive(Clone)]
struct Frame {
    interval: Interval,
    market: Arc<Market>,
    /// The calendar with this interval's candles.
    sessions: Arc<Sessions>,
    /// First candle of the test window.
    test_start: usize,
    /// Wall close of every candle.
    closes: Vec<i64>,
}

impl Frame {
    /// Bar indices of this frame's test window (from the test start to the newest bar).
    fn range(&self) -> std::ops::Range<usize> {
        self.test_start..self.market.ts.len()
    }
}

#[derive(Clone)]
struct Frames {
    list: Vec<Frame>,
    /// Free balance the searches size from.
    equity: f64,
    /// Wall open of the span every interval tests (the shortest window: the last 14 sessions).
    common_start: i64,
}

impl Frames {
    /// The frame of interval `i`, if built.
    fn get(&self, i: Interval) -> Option<&Frame> {
        self.list.iter().find(|f| f.interval == i)
    }
}

/// One completed parameter search (full window + champion/challenger).
#[derive(Clone)]
struct Search {
    /// Return % of the chosen settings over the span every interval tests (`Frames::common_start`).
    common_return_pct: Option<f64>,
    /// Test window [open, close) in wall time.
    window: (i64, i64),
    report: SearchReport,
    chosen: Option<XsParams>,
    choice: walkforward::Choice,
    finished_ms: i64,
    /// The chosen settings' 14-session backtest curve (`winner_curve`).
    winner: Option<serde_json::Value>,
}

/// The console summary of one search (what /api/winners and /api/winner show), also what is saved on disk.
fn search_summary(x: &Search) -> serde_json::Value {
    serde_json::json!({"searched_ms": x.finished_ms, "window": x.window, "evaluated": x.report.evaluated, "usable": x.report.usable,
        "choice": format!("{:?}", x.choice), "params": x.chosen, "winner": x.winner, "common_return_pct": x.common_return_pct})
}

/// What `CONSOLE_STATE_FILE` restores at start (empty when missing or unreadable).
#[derive(Default)]
struct SavedConsole {
    research: BTreeMap<Interval, serde_json::Value>,
    walk_forward: BTreeMap<Interval, serde_json::Value>,
    searches: BTreeMap<Interval, serde_json::Value>,
    paper_choice: Option<Interval>,
}

/// Reads `CANDIDATES_FILE` from `dir` (empty when missing or unreadable).
fn load_candidates(dir: &Path) -> BTreeMap<Interval, Vec<Candidate>> {
    std::fs::read(dir.join(CANDIDATES_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<BTreeMap<String, Vec<Candidate>>>(&b).ok())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| Interval::from_key(&k).map(|i| (i, v)))
                .collect()
        })
        .unwrap_or_default()
}

/// Reads `CONSOLE_STATE_FILE` from `dir`.
fn load_console_state(dir: &Path) -> SavedConsole {
    let Ok(text) = std::fs::read_to_string(dir.join(CONSOLE_STATE_FILE)) else {
        return SavedConsole::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return SavedConsole::default();
    };
    let by_key = |o: &serde_json::Value| -> BTreeMap<Interval, serde_json::Value> {
        o.as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Interval::from_key(k).map(|i| (i, v.clone())))
                    .collect()
            })
            .unwrap_or_default()
    };
    SavedConsole {
        research: by_key(&v["research"]),
        walk_forward: by_key(&v["walk_forward"]),
        searches: by_key(&v["searches"]),
        paper_choice: serde_json::from_value(v["paper_choice"].clone())
            .ok()
            .flatten(),
    }
}

/// Current wall time in milliseconds.
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl App {
    /// Logs a service event and keeps it in the console's recent-events list (newest first, at most 200).
    fn event(&self, level: &str, msg: String) {
        match level {
            "ERROR" => error!("{msg}"),
            "WARN" => warn!("{msg}"),
            _ => info!("{msg}"),
        }
        let line = format!(
            "{} {level} {msg}",
            chrono::Utc::now().format("%m-%d %H:%M:%S")
        );
        let mut s = self.status.write().unwrap_or_else(|p| p.into_inner());
        s.events.push_front(line);
        s.events.truncate(200);
    }

    /// Record an exclusion (logged once per reason change; shown on the console).
    fn exclude(&self, symbol: &str, why: String) {
        let mut ex = self.excluded.lock().unwrap_or_else(|e| e.into_inner());
        if ex.get(symbol) != Some(&why) {
            ex.insert(symbol.to_string(), why.clone());
            drop(ex);
            self.event("WARN", format!("EXCLUDED {symbol}: {why}"));
        }
    }

    /// Removes `symbol` from the excluded list (its data is available again).
    fn include(&self, symbol: &str) {
        self.excluded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(symbol);
    }

    /// The search space of `interval` under the live strategy (`live_options::LIVE`, a code constant changed only by a reviewed
    /// promotion commit), used by the live search, the paper account and the console's tests alike.
    fn live_space(&self, interval: Interval) -> SearchSpace {
        live_options::space(interval, &live_options::LIVE).0
    }

    /// Writes `CONSOLE_STATE_FILE` (atomically): research and walk-forward results, every interval's latest search summary (a
    /// fresh search, else the restored one) and the paper interval choice.
    fn save_console_state(&self) {
        let searches: BTreeMap<String, serde_json::Value> = {
            let live = self.searches.read().unwrap_or_else(|e| e.into_inner());
            let old = self.restored.read().unwrap_or_else(|e| e.into_inner());
            Interval::LIVE
                .iter()
                .filter_map(|i| {
                    live.get(i)
                        .map(search_summary)
                        .or_else(|| old.get(i).cloned())
                        .map(|v| (i.key().to_string(), v))
                })
                .collect()
        };
        let key_map = |m: &BTreeMap<Interval, serde_json::Value>| {
            m.iter()
                .map(|(i, v)| (i.key().to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        let state = serde_json::json!({
            "saved_ms": now_ms(),
            "research": key_map(&self.research.read().unwrap_or_else(|e| e.into_inner())),
            "walk_forward": key_map(&self.walk_forward.read().unwrap_or_else(|e| e.into_inner())),
            "searches": searches,
            "paper_choice": *self.paper_choice.read().unwrap_or_else(|e| e.into_inner()),
        });
        let path = self.dir.join(CONSOLE_STATE_FILE);
        let tmp = path.with_extension("json.tmp");
        if let Err(e) =
            std::fs::write(&tmp, state.to_string()).and_then(|_| std::fs::rename(&tmp, &path))
        {
            warn!("console state not saved: {e}");
        }
    }

    /// Writes `CANDIDATES_FILE` (atomically).
    fn save_candidates(&self) {
        let all: BTreeMap<String, Vec<Candidate>> = self
            .candidates
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(i, v)| (i.key().to_string(), v.clone()))
            .collect();
        let path = self.dir.join(CANDIDATES_FILE);
        let tmp = path.with_extension("json.tmp");
        match serde_json::to_vec(&all) {
            Ok(bytes) => {
                if let Err(e) =
                    std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path))
                {
                    warn!("candidates not saved: {e}");
                }
            }
            Err(e) => warn!("candidates not serialised: {e}"),
        }
    }

    /// Writes the paper account state to disk (atomic replace), if one exists.
    fn save_paper(&self) -> Result<()> {
        let guard = self.paper.read().unwrap_or_else(|p| p.into_inner());
        if let Some(ps) = guard.as_ref() {
            persist_paper(&self.dir.join(PAPER_FILE), ps)?;
        }
        Ok(())
    }

    /// The session calendar, refreshed hourly from moomoo (minus closures found in the candles).
    async fn sessions(&self) -> Result<Arc<Sessions>> {
        let cached = self
            .sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some((at, s)) = cached {
            if at.elapsed() < RULES_REFRESH {
                return Ok(s);
            }
        }
        let closures = self
            .closures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let s = Arc::new(self.gw.sessions().await?.without(&closures)?);
        *self.sessions.write().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), s.clone()));
        Ok(s)
    }

    /// Static information of `symbol` (cached; it does not change).
    async fn listing(&self, symbol: &str) -> Result<Listing> {
        if let Some(x) = self
            .listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(symbol)
        {
            return Ok(x.clone());
        }
        let l = self.gw.listing(symbol).await?;
        self.listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(symbol.to_string(), l.clone());
        Ok(l)
    }

    /// Read converted closed 1-minute candles from the converter's single common database.
    /// OpenD is deliberately not used for candle history here: the converter owns Moomoo/Yahoo
    /// fetching, normalization and writes, while this bot owns only its account/order ledger.
    async fn fetch(
        &self,
        symbols: &[String],
        _must: &[String],
        oldest: i64,
        _sessions: &Sessions,
        eligible: &[String],
        hb: &Heartbeat,
    ) -> Result<Vec<(SymbolData, Option<String>)>> {
        let rules = self
            .rules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone();
        let common = self
            .dir
            .parent()
            .and_then(Path::parent)
            .map(|p| p.join("papercompmoomoo/data/common1m.sqlite3"))
            .ok_or_else(|| anyhow::anyhow!("cannot locate common candle database"))?;
        let mut out = Vec::new();
        for sym in symbols {
            hb.beat();
            let common = common.clone();
            let res: Result<SymbolData> = async {
                let db = Connection::open_with_flags(&common, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .with_context(|| format!("open converted candle database {}", common.display()))?;
                let ticker = sym.strip_prefix("US.").unwrap_or(sym);
                let mut query = db.prepare("SELECT ts, open, high, low, close, volume FROM bars WHERE symbol=?1 AND ts>=?2 ORDER BY ts")?;
                let rows = query.query_map(rusqlite::params![ticker, oldest], |row| Ok((row.get::<_, i64>(0)?, Bar { open: row.get(1)?, high: row.get(2)?, low: row.get(3)?, close: row.get(4)?, volume: row.get(5)?, turnover: 0.0 })))?;
                let mut trades = BTreeMap::new();
                for row in rows { let (wall, bar) = row?; if bar.volume > 0.0 { trades.insert(wall, bar); } }
                let listing_wall = *trades.keys().next().ok_or_else(|| anyhow::anyhow!("{sym}: no converted candles in common database"))?;
                let hist = History { trades, listing_wall };
                self.history.lock().unwrap_or_else(|e| e.into_inner()).insert(sym.clone(), hist.clone());
                Ok(SymbolData { symbol: sym.clone(), trades: hist.trades, listing_wall: hist.listing_wall, eligible: eligible.contains(sym), rules: rules.get(sym).cloned() })
            }
            .await;
            match res {
                Ok(d) => out.push((d, None)),
                Err(e) => out.push((
                    SymbolData {
                        symbol: sym.clone(),
                        trades: Default::default(),
                        listing_wall: 0,
                        eligible: false,
                        rules: None,
                    },
                    Some(format!("{e:#}")),
                )),
            }
        }
        Ok(out)
    }

    /// Map engine clock times of `interval` to wall times for display.
    fn wall_fn(&self, interval: Interval) -> impl Fn(i64) -> Option<i64> {
        let s = self
            .sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|x| x.1.with_interval(interval));
        move |c| s.as_ref().and_then(|s| s.wall(c))
    }

    /// The interval the paper account is on (its state), else the one it should trade.
    fn paper_interval(&self) -> Interval {
        self.paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|ps| ps.interval)
            .or(*self.paper_choice.read().unwrap_or_else(|e| e.into_inner()))
            .unwrap_or_default()
    }

    /// Real fee observations (notional, fee) from this account's paper orders, all symbols.
    fn fee_points(&self) -> Vec<(f64, f64)> {
        self.store
            .as_ref()
            .and_then(|s| s.fee_points().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(_, n, f)| (n, f))
            .collect()
    }
}

/// Replace engine clock timestamps in console JSON by wall times (keys listed here).
fn walls(v: &mut serde_json::Value, wall: &dyn Fn(i64) -> Option<i64>) {
    const KEYS: [&str; 8] = [
        "entry_ts",
        "exit_ts",
        "chosen_bar_ts",
        "first_bar_ts",
        "last_bar_ts",
        "decided_ts",
        "ts_ms",
        "resume_not_before",
    ];
    match v {
        serde_json::Value::Object(map) => {
            for (k, x) in map.iter_mut() {
                if KEYS.contains(&k.as_str()) {
                    if let Some(c) = x.as_i64() {
                        *x = wall(c).map_or(serde_json::Value::Null, |w| w.into());
                        continue;
                    }
                }
                walls(x, wall);
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walls(x, wall)),
        _ => {}
    }
}

/// Runs the service: connects to OpenD, loads the paper state and archives, then supervises the bar, search, stability, watch and console tasks.
pub async fn serve(dir: PathBuf) -> Result<()> {
    governor::UNTHROTTLED.store(true, std::sync::atomic::Ordering::Relaxed);
    info!("moomoo mean reversion bot — long only, REAL moomoo paper orders: all live intervals aggregated from 1m regular-session bars, SPY trend filter, top {} most liquid of SPY's top holdings may enter, 200k random combinations per search, a {START_CAPITAL} USD account, CPU throttling disabled",
        walkforward::UNIVERSE);
    governor::global();
    let paper = load_paper(&dir.join(PAPER_FILE))?;
    let health = Health::default();
    let run_id = now_ms();
    // The archive lives beside (not inside) the runtime folder, so deploys never touch it.
    let archive = dir
        .parent()
        .map(|p| p.join("ClaudeMoomooBotArchive"))
        .ok_or_else(|| anyhow::anyhow!("runtime folder has no parent"))?;
    let store = match Store::open(&archive.join("moomoo.sqlite3")) {
        Ok(s) => {
            if let Err(e) = s.start_run(run_id, "service start") {
                warn!("store run record failed: {e:#}");
            }
            info!("store: {}", archive.join("moomoo.sqlite3").display());
            Some(s)
        }
        Err(e) => {
            warn!("store unavailable: {e:#}");
            None
        }
    };
    let btarchive = match BtArchive::open(
        &archive.join("live_backtest_archive.sqlite3"),
        concat!("moomoo-bot-", env!("CARGO_PKG_VERSION")),
    ) {
        Ok(a) => Some(a),
        Err(e) => {
            warn!("backtest archive unavailable: {e:#}");
            None
        }
    };
    let saved_console = load_console_state(&dir);
    let saved_candidates = load_candidates(&dir);
    let records: BTreeMap<Interval, Mutex<Option<Record>>> = Interval::LIVE
        .into_iter()
        .map(|i| {
            let file = format!("param_record_{}.sqlite3", i.key());
            let r = match Record::open(&archive.join(file)) {
                Ok(r) => Some(r),
                Err(e) => {
                    warn!("{} parameter record unavailable: {e:#}", i.label());
                    None
                }
            };
            (i, Mutex::new(r))
        })
        .collect();
    let saved_rules = store
        .as_ref()
        .and_then(|s| s.latest_rules().ok())
        .unwrap_or_default();
    info!(
        "continuing from {} stored rules measurements",
        saved_rules.len()
    );
    // No OpenD, no service: launchd retries (nothing is guessed without real data).
    let gw = Gateway::connect(CLIENT_ID).await?;
    let account = gw.account().await?;
    info!(
        "moomoo US paper account {} (SIMULATE): total assets {:.2} USD, cash {:.2}; the bot trades {:.0} USD of it",
        account.id, account.total_assets, account.cash, capital_from(&account)
    );
    let app = Arc::new(App {
        gw,
        dir,
        health: health.clone(),
        status: RwLock::new(Status {
            strategy: format!("Long only: live intervals built from 1m bars; signal and settings chosen by a search of 200k random combinations on the last {} sessions; exactly 1 open position, no trade below 50% of its slot; two safety rules, SPY trend filter, no leverage; every decision placed as a REAL moomoo paper order (US SIMULATE account), sized as a {START_CAPITAL} USD account (never more than the available cash)", moomoo_bot::engine::TEST_SESSIONS),
            started_ms: now_ms(),
            universe: walkforward::UNIVERSE,
            ..Default::default()
        }),
        store,
        records,
        run_id,
        sessions: RwLock::new(None),
        closures: Mutex::new(Vec::new()),
        // NOTE(agents): Never start from wiped rules (user rule): continue from the last stored
        //               measurement of each symbol; the hourly in-session refresh replaces them.
        rules: Mutex::new((None, saved_rules)),
        listings: Mutex::new(HashMap::new()),
        pool: Mutex::new(None),
        excluded: Mutex::new(BTreeMap::new()),
        signals: RwLock::new(vec![]),
        paper: RwLock::new(paper),
        research: RwLock::new(saved_console.research),
        btarchive,
        walk_forward: RwLock::new(saved_console.walk_forward),
        frames: RwLock::new(None),
        candidates: RwLock::new(saved_candidates),
        searches: RwLock::new(BTreeMap::new()),
        paper_choice: RwLock::new(saved_console.paper_choice),
        restored: RwLock::new(saved_console.searches),

        history: Mutex::new(HashMap::new()),
        open_ctx: Mutex::new(None),
        order_lock: tokio::sync::Mutex::new(()),
    });
    app.status
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .account = Some(account);
    // Candle history is loaded from the converter-owned common database on each bar cycle.
    // The bot archive stores decisions and paper/order state only; it is never a second candle source.

    let a = app.clone();
    health.spawn("bar_task", Duration::from_secs(1200), move |hb| {
        let a = a.clone();
        async move { bar_task(a, hb).await }
    });
    let a = app.clone();
    // Stall limit above SEARCH_DEADLINE: the task cannot beat while a search runs.
    health.spawn(
        "search_task",
        SEARCH_DEADLINE + Duration::from_secs(600),
        move |hb| {
            let a = a.clone();
            async move { search_task(a, hb).await }
        },
    );
    let a = app.clone();
    // Stall limit: a stability test runs 20 full searches without heartbeats.
    health.spawn("stability_task", Duration::from_secs(3 * 3600), move |hb| {
        let a = a.clone();
        async move { stability_task(a, hb).await }
    });
    let a = app.clone();
    health.spawn(
        "walk_forward_priority_task",
        Duration::from_secs(3 * 3600),
        move |hb| {
            let a = a.clone();
            async move { walk_forward_priority_task(a, hb).await }
        },
    );
    let a = app.clone();
    health.spawn("gateway_watch", Duration::from_secs(300), move |hb| {
        let a = a.clone();
        async move { gateway_watch(a, hb).await }
    });
    let a = app.clone();
    health.spawn("keep_alive", Duration::from_secs(300), move |hb| {
        let a = a.clone();
        async move { keep_alive_task(a, hb).await }
    });
    let a = app.clone();
    health.spawn("console", Duration::from_secs(120), move |hb| {
        let a = a.clone();
        async move { console(a, hb).await }
    });

    let wd = health.clone();
    std::thread::Builder::new()
        .name("process-watchdog".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(60));
            for (name, t) in wd.snapshot() {
                if name == "bar_task" && t.seconds_since_heartbeat > 2400 {
                    error!(
                        "[watchdog] {name} silent for {}s; exiting so launchd restarts the service",
                        t.seconds_since_heartbeat
                    );
                    std::process::exit(1);
                }
            }
        })?;

    // Ctrl-C on every OS; SIGTERM (launchd, systemd) on macOS and Linux. Windows has no SIGTERM: Task Scheduler ends the task with a console break.
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    app.save_paper()?;
    info!("shutdown");
    Ok(())
}

/// The search winner's backtest over the frame's test window, candle by candle: the same loop as
/// `xs::backtest` (step every candle, close everything after the last), recording marked equity
/// after each closed candle, and the trades with wall times. Its end equity is exactly the backtest's.
fn winner_curve(
    f: &Frame,
    sc: &[Vec<Option<scores::Score>>],
    p: &XsParams,
    equity: f64,
) -> serde_json::Value {
    let m = &f.market;
    let range = f.range();
    let mut pf = XsPortfolio::new(equity);
    let mut points: Vec<(i64, f64)> = vec![(m.wall[range.start], equity)];
    for t in range.clone() {
        pf.step(m, sc, t, p);
        points.push((f.closes[t], pf.equity + pf.unrealized()));
    }
    let last = m.ts.len() - 1;
    pf.close_all(m, m.ts[last] + BAR_MS, "End of test");
    if let Some(pt) = points.last_mut() {
        pt.1 = pf.equity;
    }
    let wall = |c: i64| match m.ts.binary_search(&c) {
        Ok(i) => Some(m.wall[i]),
        Err(_) => m.ts.binary_search(&(c - BAR_MS)).ok().map(|i| f.closes[i]),
    };
    let trades: Vec<serde_json::Value> = pf
        .trades
        .iter()
        .map(|t| serde_json::json!({"symbol": t.symbol, "entry_ms": wall(t.entry_ts), "exit_ms": wall(t.exit_ts),
            "entry": t.entry, "exit": t.exit, "qty": t.qty, "pnl": t.pnl, "fees": t.fees, "reason": t.reason}))
        .collect();
    let mt = pf.metrics();
    serde_json::json!({"interval": f.interval, "interval_label": f.interval.label(), "sessions": f.interval.test_sessions(),
        "params": p, "start_equity": equity, "end_equity": pf.equity, "return_pct": mt.return_pct(),
        "trades": mt.trades, "wins": mt.wins, "max_drawdown_pct": mt.max_drawdown_pct, "points": points, "trade_list": trades,
        "execution_error": pf.execution_error})
}

/// Return % of a winner curve from wall time `from` to its end: from the equity at the last point
/// closing at or before `from` (the window start when it starts later).
fn span_return(curve: &serde_json::Value, from: i64) -> Option<f64> {
    let pts = curve["points"].as_array()?;
    let pt = |v: &serde_json::Value| Some((v[0].as_i64()?, v[1].as_f64()?));
    let base = pts
        .iter()
        .filter_map(pt)
        .take_while(|(x, _)| *x <= from)
        .last()
        .or_else(|| pts.first().and_then(pt))?
        .1;
    let end = pts.last().and_then(pt)?.1;
    (base > 0.0).then(|| (end / base - 1.0) * 100.0)
}

// NOTE(agents): The span the intervals are compared on is the last TEST_SESSIONS CLOSED sessions
//               (plus whatever of today has closed). The 1m window counts today's forming session
//               as one of its 14, which during the session dropped a whole closed session from the
//               4h / one-session span and flipped the paper interval intraday (2026-10-07).
/// Wall open of the oldest of the last `TEST_SESSIONS` sessions fully closed by `closed_end`.
fn common_span_start(sessions: &Sessions, closed_end: i64) -> Option<i64> {
    let done: Vec<i64> = sessions
        .list()
        .iter()
        .filter(|s| s.1 <= closed_end)
        .map(|s| s.0)
        .collect();
    done.len().checked_sub(TEST_SESSIONS).map(|i| done[i])
}

/// The frame's full parameter search (1m: exactly the live search; other intervals the same
/// search space over their test window, warm-up candles excluded).
fn search_frame(
    f: &Frame,
    sc: &[Vec<Option<scores::Score>>],
    equity: f64,
    deadline: Instant,
    seed: u64,
    recorded: &[XsParams],
    space: SearchSpace,
) -> Result<SearchReport> {
    match f.interval {
        Interval::M1 if space == walkforward::space_for(Interval::M1) => {
            walkforward::search_live(&f.market, sc, equity, deadline, seed, recorded)
        }
        _ => walkforward::search_random(
            &f.market,
            sc,
            equity,
            deadline,
            seed,
            recorded,
            SearchSpace {
                warmup_bars: f.test_start,
                ..space
            },
        ),
    }
}

// NOTE(agents): User rule (2026-10-06): STRICT test windows, live and in every test: 1m the last
//               14 trading sessions; 1h, 4h and one session the last 21 (their older candles only
//               warm indicators up). While the market is closed the bot runs, per interval, a
//               STABILITY test on that window (STABILITY_RUNS independent full searches + a
//               random-settings baseline) and a WALK-FORWARD test INSIDE the same window: for each
//               of the last WF_DAYS sessions, the search picks settings on the sessions before it
//               (1m: the first scores::WARMUP candles only warm the indicators up, user
//               request; the larger time frames warm up on their older candles), and one continuous portfolio trades that day unseen.
/// Independent full searches per market-closed stability test.
const STABILITY_RUNS: usize = 20;
/// Random settings backtested for the baseline.
const BASELINE_SAMPLES: usize = 2000;
/// One unseen walk-forward day, kept for the research archive.
struct BtDay {
    first: usize,
    end: usize,
    first_trade: usize,
    equity_before: f64,
    equity_after: f64,
    /// Range of the equity path (`points`) the day covers.
    points: (usize, usize),
    params: Option<XsParams>,
}

/// Unseen trading days at the end of the window in the walk-forward test.
const WF_DAYS: usize = 3;

/// The walk-forward test inside the frame's test window (see the note above).
fn walk_forward_test(
    f: &Frame,
    start: f64,
    live: SearchSpace,
) -> Result<(serde_json::Value, Vec<BtRun>)> {
    let m = &f.market;
    let n = m.ts.len();
    // Candles that open a session, inside the test window.
    let opens: Vec<usize> = f
        .range()
        .filter(|&t| {
            f.sessions
                .open_session(m.wall[t])
                .is_some_and(|s| s.0 == m.wall[t])
        })
        .collect();
    anyhow::ensure!(
        opens.len() > WF_DAYS,
        "{}: only {} session opens in the window",
        f.interval.label(),
        opens.len()
    );
    let days: Vec<(usize, usize)> = (opens.len() - WF_DAYS..opens.len())
        .map(|k| (opens[k], opens.get(k + 1).copied().unwrap_or(n)))
        .collect();
    let first = days[0].0;
    // 1m warms up inside the window (as before); the larger time frames on their warm-up candles.
    let warmup = if f.test_start > 0 {
        f.test_start
    } else {
        scores::WARMUP
    };
    let sc_full = scores::compute(m, walkforward::UNIVERSE);
    let space = SearchSpace {
        warmup_bars: warmup,
        ..live
    };
    let mut pf = XsPortfolio::new(start);
    let (mut current, mut segments, mut points): (
        Option<XsParams>,
        Vec<serde_json::Value>,
        Vec<(i64, f64)>,
    ) = (None, Vec::new(), vec![(m.wall[first], start)]);
    let mut bt_days: Vec<BtDay> = Vec::new();
    let reference = m.symbols.iter().position(|x| x == REFERENCE);
    for &(a, b) in &days {
        anyhow::ensure!(
            a > warmup + 1,
            "{}: no sessions to choose on before {}",
            f.interval.label(),
            m.wall[a]
        );
        let sel = m.slice(0..a);
        let sc_sel = scores::compute(&sel, walkforward::UNIVERSE);
        let eq = pf.equity + pf.unrealized();
        let rep = walkforward::search_random(
            &sel,
            &sc_sel,
            eq,
            Instant::now() + SEARCH_DEADLINE,
            search_seed(),
            &[],
            space,
        )?;
        let chosen = rep.params.is_some();
        if chosen {
            current = rep.params.clone();
        }
        let before = (pf.equity + pf.unrealized(), pf.trades.len());
        let points_from = points.len();
        if let Some(p) = current.as_ref() {
            for t in a..b {
                pf.step(m, &sc_full, t, p);
                points.push((f.closes[t], pf.equity + pf.unrealized()));
            }
        }
        let after = pf.equity + pf.unrealized();
        bt_days.push(BtDay {
            first: a,
            end: b,
            first_trade: before.1,
            equity_before: before.0,
            equity_after: after,
            points: (points_from, points.len()),
            params: current.clone(),
        });
        let bh = reference
            .and_then(|s| Some((m.bars[s][b - 1]?.close / m.bars[s][a]?.open - 1.0) * 100.0));
        segments.push(serde_json::json!({
            "chosen_on": [m.wall[warmup.min(a - 1)], f.closes[a - 1]],
            "traded": [m.wall[a], f.closes[b - 1]],
            "usable": rep.usable, "new_settings": chosen, "params": current,
            "start_equity": before.0, "end_equity": after, "trades_closed": pf.trades.len() - before.1,
            "reference_bh_pct": bh,
        }));
    }
    let last = n - 1;
    pf.close_all(m, m.ts[last] + BAR_MS, "End of test");
    // NOTE(agents): An incomplete or failed simulation is never a valid result.
    pf.validate_state()?;
    if let Some(pt) = points.last_mut() {
        pt.1 = pf.equity;
    }
    let mt = pf.metrics();
    let bh = reference
        .and_then(|s| Some((m.bars[s][last]?.close / m.bars[s][first]?.open - 1.0) * 100.0));
    // Each unseen day is one out-of-sample run of the settings chosen before it; trades are kept whatever the day's result.
    let n_days = bt_days.len();
    let bt_runs: Vec<BtRun> = bt_days
        .iter()
        .enumerate()
        .filter_map(|(i, d)| {
            let (a, b, t0, e0, (p0, p1)) =
                (&d.first, &d.end, &d.first_trade, &d.equity_before, d.points);
            let params = d.params.as_ref()?;
            let t1 = bt_days
                .get(i + 1)
                .map_or(pf.trades.len(), |x| x.first_trade);
            let e1 = if i + 1 == n_days {
                pf.equity
            } else {
                d.equity_after
            };
            let mut cache = HashMap::new();
            let trades: Vec<(Trade, serde_json::Value)> = pf
                .trades
                .get(*t0..t1)?
                .iter()
                .filter_map(|t| {
                    Some((
                        wall_trade(m, &f.closes, t)?,
                        entry_features(m, &f.closes, &sc_full, params, t, &mut cache),
                    ))
                })
                .collect();
            let day_points: Vec<f64> = points
                .get(p0..p1)
                .unwrap_or(&[])
                .iter()
                .map(|x| x.1)
                .collect();
            let (gp, gl) = trades.iter().fold((0.0, 0.0), |(gp, gl), (t, _)| {
                if t.pnl > 0.0 {
                    (gp + t.pnl, gl)
                } else {
                    (gp, gl - t.pnl)
                }
            });
            let daily = Daily {
                day_ts: m.wall[*a],
                equity: e1,
                pnl: e1 - e0,
                trades: trades.len(),
                wins: trades.iter().filter(|(t, _)| t.pnl > 0.0).count(),
                tokens: day_tokens(&trades),
            };
            Some(BtRun {
                source: "oos",
                window_label: format!("{}-oos", f.interval.key()),
                first_bar_ts: m.wall[*a],
                last_bar_ts: m.wall[*b - 1],
                start_equity: *e0,
                params: params_value(params),
                seed: None,
                robust_score: None,
                return_pct: if *e0 > 0.0 {
                    (e1 / e0 - 1.0) * 100.0
                } else {
                    0.0
                },
                max_drawdown_pct: drawdown_pct(&day_points),
                profit_factor: if gl > 0.0 {
                    gp / gl
                } else if gp > 0.0 {
                    f64::INFINITY
                } else {
                    0.0
                },
                successful: e1 > *e0,
                trades,
                daily: vec![daily],
            })
        })
        .collect();
    let value = (
        serde_json::json!({
            "interval": f.interval, "interval_label": f.interval.label(), "sessions": f.interval.test_sessions(),
            "start_equity": start, "end_equity": pf.equity, "return_pct": (pf.equity / start - 1.0) * 100.0,
            "max_drawdown_pct": mt.max_drawdown_pct, "trades": mt.trades, "wins": mt.wins,
            "reference": REFERENCE, "reference_bh_pct": bh, "days": WF_DAYS, "warmup_bars": warmup,
            "segments": segments, "points": points,
            "assumptions": [
                format!("Inside the live {}-session window only ({} candles): each unseen day's settings were chosen by the full search on the sessions before it; warm-up candles only feed the indicators.", f.interval.test_sessions(), f.interval.label()),
                "One continuous portfolio (positions carried across days, as live); simulation engine, no real orders.".to_string(),
            ],
        }),
        bt_runs,
    );
    Ok(value)
}

/// Largest peak-to-trough fall (%) of an equity path.
fn drawdown_pct(path: &[f64]) -> f64 {
    let (mut peak, mut dd) = (f64::MIN, 0.0_f64);
    for &e in path {
        peak = peak.max(e);
        if peak > 0.0 {
            dd = dd.max((peak - e) / peak * 100.0);
        }
    }
    dd
}

/// The stability test on the frame's test window (see the note above).
/// Random-settings runs whose trades go to the research archive per stability test.
const ARCHIVED_RANDOM_RUNS: usize = 40;

/// Stability tests of one frame: the chosen settings, random settings and the top search results replayed on the test window, also written to the archive.
fn stability(f: &Frame, equity: f64, live: SearchSpace) -> Result<(serde_json::Value, Vec<BtRun>)> {
    let m = &f.market;
    let range = f.range();
    let sc = scores::compute(m, walkforward::UNIVERSE);
    let mut runs = Vec::new();
    let mut bt_runs: Vec<BtRun> = Vec::new();
    let (first_bar_ts, last_bar_ts, window_label) = (
        m.wall[range.start],
        f.closes[range.end - 1],
        format!("{}-{}s", f.interval.key(), f.interval.test_sessions()),
    );
    let archive_run =
        |source: &'static str, p: &XsParams, seed: Option<i64>, robust_score: Option<f64>| {
            let pf = xs::backtest(m, &sc, range.clone(), p, equity);
            let mt = pf.metrics();
            BtRun {
                source,
                window_label: window_label.clone(),
                first_bar_ts,
                last_bar_ts,
                start_equity: equity,
                params: params_value(p),
                seed,
                robust_score,
                return_pct: mt.return_pct(),
                max_drawdown_pct: mt.max_drawdown_pct,
                profit_factor: mt.profit_factor(),
                successful: mt.return_pct() > 0.0 && mt.liquidations == 0,
                trades: {
                    let mut cache = HashMap::new();
                    pf.trades
                        .iter()
                        .filter_map(|t| {
                            Some((
                                wall_trade(m, &f.closes, t)?,
                                entry_features(m, &f.closes, &sc, p, t, &mut cache),
                            ))
                        })
                        .collect()
                },
                daily: Vec::new(),
            }
        };
    for _ in 0..STABILITY_RUNS {
        let seed = search_seed();
        let rep = search_frame(
            f,
            &sc,
            equity,
            Instant::now() + SEARCH_DEADLINE,
            seed,
            &[],
            live,
        )?;
        let (ret, trades, dd, wins) = rep.metrics.as_ref().map_or((None, None, None, None), |x| {
            (
                Some(x.return_pct()),
                Some(x.trades),
                Some(x.max_drawdown_pct),
                Some(x.wins),
            )
        });
        if let Some(p) = rep.params.as_ref() {
            bt_runs.push(archive_run(
                "chosen",
                p,
                Some(seed as i64),
                rep.robust_score,
            ));
        }
        runs.push(serde_json::json!({
            "seed": seed, "usable": rep.usable, "params": rep.params, "robust_score": rep.robust_score,
            "return_pct": ret, "trades": trades, "wins": wins, "max_drawdown_pct": dd,
        }));
    }
    let band = walkforward::MarginBand::new(&m.slice(range.clone()))?;
    // 1m: the baseline as before; other intervals draw from their own search space.
    let draws = match f.interval {
        Interval::M1 => walkforward::sample_params(search_seed(), BASELINE_SAMPLES, &band),
        _ => walkforward::sample_space(search_seed(), BASELINE_SAMPLES, &band, live),
    };
    bt_runs.extend(
        draws
            .iter()
            .take(ARCHIVED_RANDOM_RUNS)
            .map(|p| archive_run("random", p, None, None)),
    );
    let mut rets: Vec<f64> = draws
        .iter()
        .map(|p| {
            xs::backtest(m, &sc, range.clone(), p, equity)
                .metrics()
                .return_pct()
        })
        .collect();
    rets.sort_by(f64::total_cmp);
    let q = |x: f64| rets[((rets.len() - 1) as f64 * x).round() as usize];
    let mut families: HashMap<String, usize> = HashMap::new();
    for r in &runs {
        if let Some(p) = r["params"].as_object() {
            let key = format!(
                "{} {}",
                p["signal"].as_str().unwrap_or("?"),
                if p["flip"].as_bool() == Some(true) {
                    "follow"
                } else {
                    "contrarian"
                }
            );
            *families.entry(key).or_default() += 1;
        }
    }
    let mut fam: Vec<(String, usize)> = families.into_iter().collect();
    fam.sort_by_key(|x| std::cmp::Reverse(x.1));
    let found: Vec<f64> = runs
        .iter()
        .filter_map(|r| r["return_pct"].as_f64())
        .collect();
    let value = serde_json::json!({
        "interval": f.interval, "interval_label": f.interval.label(), "sessions": f.interval.test_sessions(),
        "window": [m.wall[range.start], f.closes[range.end - 1]], "start_equity": equity,
        "runs": runs, "families": fam,
        "runs_with_settings": found.len(), "runs_positive": found.iter().filter(|x| **x > 0.0).count(),
        "baseline": {"samples": rets.len(), "median_pct": q(0.5), "p10_pct": q(0.1), "p90_pct": q(0.9),
                     "positive_share": rets.iter().filter(|x| **x > 0.0).count() as f64 / rets.len() as f64 * 100.0},
        "test_kind": format!("stability_{}_{}s", f.interval.key(), f.interval.test_sessions()),
        "assumptions": [
            format!("All results are on the same last-{}-session window the searches choose on: in-sample, not a forecast.", f.interval.test_sessions()),
            "Uses the simulation engine; no real orders or fills.".to_string(),
        ],
    });
    Ok((value, bt_runs))
}

/// Write backtest runs to the research archive (a failure is logged, never fatal).
fn archive_runs(app: &App, runs: &[BtRun]) {
    let Some(a) = &app.btarchive else { return };
    let fresh = runs
        .iter()
        .filter(|r| {
            a.record(app.run_id, now_ms(), r).unwrap_or_else(|e| {
                warn!("backtest archive write failed: {e:#}");
                false
            })
        })
        .count();
    info!("backtest archive: {fresh} of {} runs new", runs.len());
}

/// Causal walk-forward learning has priority over broad stability searches and runs 24/7.
/// It uses only the latest real candle frames; its output is research metadata until the
/// multi-block, cost/noise and promotion gates approve a winner.
async fn walk_forward_priority_task(app: Arc<App>, hb: Heartbeat) -> Result<()> {
    let mut last_marker: Option<i64> = None;
    loop {
        hb.beat();
        let latest = app.frames.read().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(frames) = latest.filter(|x| x.get(Interval::M1).is_some()) else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };
        let marker = frames
            .get(Interval::M1)
            .and_then(|f| f.market.wall.last().copied())
            .unwrap_or_default();
        if marker == 0 || last_marker == Some(marker) {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        }
        last_marker = Some(marker);
        let equity = frames.equity;
        let mut ran = 0usize;
        for f in frames
            .list
            .clone()
            .into_iter()
            .filter(|f| Interval::LIVE.contains(&f.interval))
        {
            hb.beat();
            let label = f.interval.label();
            app.status
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .history_note = format!("priority walk-forward learning: {label} on real candles");
            let fs = f.clone();
            let live = app.live_space(fs.interval);
            let wf =
                tokio::task::spawn_blocking(move || walk_forward_test(&fs, equity, live)).await?;
            let (mut value, bt) = match wf {
                Ok((v, bt)) => (v, bt),
                Err(e) => (
                    serde_json::json!({"error": format!("{e:#}"), "interval": f.interval}),
                    Vec::new(),
                ),
            };
            archive_runs(&app, &bt);
            value["computed_ms"] = now_ms().into();
            value["priority"] = true.into();
            if let Some(st) = &app.store {
                if let Err(e) = st.search(
                    app.run_id,
                    now_ms(),
                    &serde_json::json!({"walk_forward_priority": &value}).to_string(),
                ) {
                    warn!("store priority walk-forward write failed: {e:#}");
                }
            }
            app.walk_forward
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(f.interval, value.clone());
            app.event("INFO", match value.get("error") {
                Some(e) => format!("WALK-FORWARD PRIORITY {label} failed: {e}"),
                None => format!("WALK-FORWARD PRIORITY {label}: {:+.2}% vs {REFERENCE} {:+.2}%, {} trades; research-only", value["return_pct"].as_f64().unwrap_or(0.0), value["reference_bh_pct"].as_f64().unwrap_or(0.0), value["trades"]),
            });
            ran += 1;
        }
        app.save_console_state();
        app.status
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .history_note =
            format!("priority walk-forward complete: {ran} intervals; next run in 5 minutes");
        tokio::time::sleep(Duration::from_secs(5 * 60)).await;
    }
}

// NOTE(agents): Larger stability runs remain market-closed and once per closed period.
async fn stability_task(app: Arc<App>, hb: Heartbeat) -> Result<()> {
    let mut done_for: Option<i64> = None;
    loop {
        hb.beat();
        tokio::time::sleep(Duration::from_secs(30)).await;
        let sessions = app.sessions().await?;
        let now = now_ms();
        if sessions.open_session(now).is_some() {
            if done_for.is_none() {
                app.status
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .history_note = "waits for the market to close".into();
            }
            continue;
        }
        let Some(&last_close) = sessions.last_closed_bars(now, 1).last() else {
            continue;
        };
        let latest = app.frames.read().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(frames) = latest.filter(|x| {
            x.get(Interval::M1)
                .is_some_and(|f| f.market.wall.last() == Some(&last_close))
        }) else {
            continue;
        };
        if done_for == Some(last_close) {
            continue;
        }
        let equity = frames.equity;
        for f in frames
            .list
            .clone()
            .into_iter()
            .filter(|f| Interval::LIVE.contains(&f.interval))
        {
            hb.beat();
            let (label, sessions_n) = (f.interval.label(), f.interval.test_sessions());
            app.status.write().unwrap_or_else(|e| e.into_inner()).history_note =
                format!("running {label}: {STABILITY_RUNS} independent 200k-combination searches + {BASELINE_SAMPLES} random settings on the last {sessions_n} sessions");
            let started = Instant::now();
            let fs = f.clone();
            let live = app.live_space(fs.interval);
            let result = tokio::task::spawn_blocking(move || stability(&fs, equity, live)).await?;
            let (mut value, bt) = match result {
                Ok((v, bt)) => (v, bt),
                Err(e) => (
                    serde_json::json!({ "error": format!("{e:#}"), "interval": f.interval }),
                    Vec::new(),
                ),
            };
            archive_runs(&app, &bt);
            value["computed_ms"] = now_ms().into();
            value["elapsed_s"] = started.elapsed().as_secs().into();
            if let Some(st) = &app.store {
                if let Err(e) = st.search(
                    app.run_id,
                    now_ms(),
                    &serde_json::json!({"stability": &value}).to_string(),
                ) {
                    warn!("store stability write failed: {e:#}");
                }
            }
            let summary = match value.get("error") {
                Some(e) => format!("STABILITY {label} test failed: {e}"),
                None => format!(
                    "STABILITY {label} (last {sessions_n} sessions): {} of {STABILITY_RUNS} searches found settings, {} positive; random settings median {:+.2}%, {} s",
                    value["runs_with_settings"], value["runs_positive"],
                    value["baseline"]["median_pct"].as_f64().unwrap_or(0.0), value["elapsed_s"]
                ),
            };
            app.research
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(f.interval, value);
            app.save_console_state();
            app.event("INFO", summary);
            hb.beat();
            app.status
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .history_note =
                format!("running {label}: walk-forward inside the window ({WF_DAYS} unseen days)");
            let fs = f.clone();
            let live = app.live_space(fs.interval);
            let wf =
                tokio::task::spawn_blocking(move || walk_forward_test(&fs, equity, live)).await?;
            let (mut wf_value, bt) = match wf {
                Ok((v, bt)) => (v, bt),
                Err(e) => (
                    serde_json::json!({ "error": format!("{e:#}"), "interval": f.interval }),
                    Vec::new(),
                ),
            };
            archive_runs(&app, &bt);
            wf_value["computed_ms"] = now_ms().into();
            if let Some(st) = &app.store {
                if let Err(e) = st.search(
                    app.run_id,
                    now_ms(),
                    &serde_json::json!({"walk_forward": &wf_value}).to_string(),
                ) {
                    warn!("store walk-forward write failed: {e:#}");
                }
            }
            app.event("INFO", match wf_value.get("error") {
                Some(e) => format!("WALK-FORWARD {label} test failed: {e}"),
                None => format!(
                    "WALK-FORWARD {label} (last {WF_DAYS} sessions unseen): {:+.2}% vs {REFERENCE} {:+.2}%, {} trades",
                    wf_value["return_pct"].as_f64().unwrap_or(0.0), wf_value["reference_bh_pct"].as_f64().unwrap_or(0.0), wf_value["trades"]
                ),
            });
            app.walk_forward
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(f.interval, wf_value);
            app.save_console_state();
        }
        app.status
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .history_note = format!("done {}", chrono::Utc::now().format("%m-%d %H:%M UTC"));
        done_for = Some(last_close);
    }
}

// NOTE(agents): Self-healing connection (user request 2026-10-06). OpenD expects a keep-alive
//               every keep_alive_secs (10 s, proto 1004); a transport failure drops the connection
//               and the next request reconnects. When OpenD stays unreachable for
//               GATEWAY_DEAD_LIMIT, the process saves the paper account and exits; launchd restarts
//               it after ThrottleInterval (startup fails until OpenD is back, so launchd keeps
//               retrying). Nothing is invented meanwhile: the bar task simply cannot read data.
async fn keep_alive_task(app: Arc<App>, hb: Heartbeat) -> Result<()> {
    loop {
        hb.beat();
        let secs = app
            .gw
            .keep_alive_secs()
            .await
            .filter(|s| *s > 0)
            .unwrap_or(10) as u64;
        tokio::time::sleep(Duration::from_secs(secs)).await;
        if let Err(e) = app.gw.keep_alive().await {
            warn!("OpenD keep-alive failed: {e:#}");
        }
    }
}

/// Watches OpenD every 30 s; logs loss and recovery, and exits the service after GATEWAY_DEAD_LIMIT so launchd restarts and reconnects it.
async fn gateway_watch(app: Arc<App>, hb: Heartbeat) -> Result<()> {
    let mut dead_since: Option<Instant> = None;
    loop {
        hb.beat();
        tokio::time::sleep(Duration::from_secs(30)).await;
        if app.gw.alive().await {
            if dead_since.take().is_some() {
                app.event("INFO", "OpenD reachable and logged in again".into());
            }
            continue;
        }
        let since = *dead_since.get_or_insert_with(Instant::now);
        app.event(
            "WARN",
            format!(
                "OpenD unreachable or logged out for {} s",
                since.elapsed().as_secs()
            ),
        );
        if since.elapsed() >= GATEWAY_DEAD_LIMIT {
            error!("OpenD unreachable for {} s: exiting so launchd restarts the service and reconnects", since.elapsed().as_secs());
            if let Err(e) = app.save_paper() {
                error!("paper save before restart failed: {e:#}");
            }
            std::process::exit(1);
        }
    }
}

/// The universe (SPY's holdings, nothing delisted or unknown to moomoo), refreshed hourly; exclusions recorded with their reason.
async fn pool(app: &App) -> Result<Vec<Listing>> {
    let cached = app.pool.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some((at, list)) = cached {
        if at.elapsed() < RULES_REFRESH {
            return Ok(list);
        }
    }
    // SPY's holdings only (user 2026-10-09). No file = no universe (never another source).
    let tickers = moomoo::prefer_selection(
        moomoo::load_holdings(&app.dir.join("spy_holdings.csv")).context(
            "spy_holdings.csv missing in the runtime folder: no universe (run ./deploy.sh)",
        )?,
    );
    let (list, excluded) = app.gw.universe_from(&tickers, moomoo::SPY_POOL).await?;
    for (sym, why) in &excluded {
        app.excluded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sym.clone(), format!("SPY holding left out: {why}"));
    }
    for l in &list {
        app.listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(l.symbol.clone(), l.clone());
    }
    if let Some(st) = &app.store {
        st.universe(now_ms(), &list)?;
    }
    app.event(
        "INFO",
        format!(
            "UNIVERSE: {} SPY holdings eligible on moomoo (by weight: {}); {} left out (delisted, not NYSE/Nasdaq or unknown to moomoo; first: {})",
            list.len(),
            list.iter().take(8).map(|l| l.symbol.as_str()).collect::<Vec<_>>().join(", "),
            excluded.len(),
            excluded.first().map_or("-".to_string(), |(s, w)| format!("{s}: {w}"))
        ),
    );
    *app.pool.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// Refresh the states and fees of this bot's orders from moomoo and book them (`Ledger::recompute`).
async fn refresh_orders(app: &App) -> Result<()> {
    let (ids, fee_missing, oldest) = {
        let g = app.paper.read().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            None => return Ok(()),
            Some(ps) => (
                ps.ledger.open_order_ids(),
                ps.ledger.fee_missing(),
                ps.ledger
                    .orders
                    .iter()
                    .filter(|o| !o.is_final())
                    .map(|o| o.placed_ms)
                    .min(),
            ),
        }
    };
    let mut states = Vec::new();
    if !ids.is_empty() {
        states = app.gw.orders(&ids).await?;
        let missing: Vec<u64> = ids
            .iter()
            .copied()
            .filter(|i| !states.iter().any(|s| s.order_id == *i))
            .collect();
        if !missing.is_empty() {
            states.extend(
                app.gw
                    .history_orders(&missing, oldest.unwrap_or(now_ms()) - 86_400_000)
                    .await?,
            );
        }
    }
    // Trd_GetOrderFee refuses paper accounts; fees come from `place_open`'s cash measurement only.
    let fees: Vec<moomoo::OrderFee> = Vec::new();
    let mut lines = Vec::new();
    {
        let mut g = app.paper.write().unwrap_or_else(|e| e.into_inner());
        let Some(ps) = g.as_mut() else { return Ok(()) };
        for s in &states {
            if ps.ledger.update_state(s) {
                if let Some(st) = &app.store {
                    st.order(now_ms(), s.order_id, &serde_json::to_string(s)?)?;
                }
                if moomoo::order_final(s.status) {
                    lines.push(match s.fill_avg_price {
                        Some(p) if s.fill_qty > 0.0 => format!(
                            "FILL {} {} {} @ {p:.4} (order {}, status {})",
                            if s.side == 1 { "BUY" } else { "SELL" },
                            s.fill_qty,
                            s.symbol,
                            s.order_id,
                            s.status
                        ),
                        _ => format!(
                            "ORDER {} {} {} ended without a fill (status {}{})",
                            if s.side == 1 { "BUY" } else { "SELL" },
                            s.qty,
                            s.symbol,
                            s.status,
                            s.last_err
                                .as_ref()
                                .map_or(String::new(), |e| format!(": {e}"))
                        ),
                    });
                }
            }
        }
        for f in &fees {
            if ps.ledger.update_fee(f) {
                if let (Some(st), Some(o)) = (
                    &app.store,
                    ps.ledger
                        .orders
                        .iter()
                        .find(|o| o.order_id_ex.as_deref() == Some(f.order_id_ex.as_str())),
                ) {
                    let (q, v) = o.fill();
                    st.order_fee(
                        now_ms(),
                        f,
                        &o.symbol,
                        o.side,
                        q,
                        if q > 0.0 { v / q } else { 0.0 },
                    )?;
                }
                lines.push(format!(
                    "FEE order {}: {:.2} USD ({})",
                    f.order_id_ex,
                    f.total,
                    f.items
                        .iter()
                        .map(|(t, v)| format!("{t} {v:.2}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        for o in ps.ledger.orders.iter_mut().filter(|o| {
            o.order_id_ex
                .as_ref()
                .is_some_and(|x| fee_missing.contains(x))
                && o.fee.is_none()
        }) {
            o.fee_requests += 1;
            if o.fee_requests >= FEE_REQUESTS && !o.fee_unavailable {
                o.fee_unavailable = true;
                lines.push(format!("EXCLUDED fee of order {} ({} {}): no cash-measured fee after {FEE_REQUESTS} bars (moomoo reports no fees for paper orders); the fee is left out (zero), never invented", o.order_id_ex.clone().unwrap_or_default(), o.symbol, o.qty));
            }
        }
        if !states.is_empty() || !fees.is_empty() || !lines.is_empty() {
            lines.extend(ps.ledger.recompute(&mut ps.portfolio));
            persist_paper(&app.dir.join(PAPER_FILE), ps)?;
        }
    }
    for l in lines {
        app.event(
            if l.starts_with("EXCLUDED") {
                "WARN"
            } else {
                "INFO"
            },
            l,
        );
    }
    Ok(())
}

/// Place the orders of the open in `app.open_ctx` once it is open (see `execution`).
async fn maybe_place_open(app: &Arc<App>, sessions: &Sessions) -> Result<()> {
    let Some(ctx) = app
        .open_ctx
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return Ok(());
    };
    let now = now_ms();
    if now < ctx.next_wall || sessions.open_session(now).is_none() {
        return Ok(());
    }
    let close = sessions
        .with_interval(ctx.interval)
        .close_of(ctx.next_wall)
        .unwrap_or(ctx.next_wall);
    *app.open_ctx.lock().unwrap_or_else(|e| e.into_inner()) = None;
    if now >= close {
        app.event("WARN", format!("ORDERS for the open {} not placed: that candle already closed (the service was not running)", fmt_et(ctx.next_wall)));
        return Ok(());
    }
    place_open(app, &ctx).await
}

/// At an open of the paper interval: previews the engine's executions at live quote mids and places them as paper market orders (sells first).
async fn place_open(app: &Arc<App>, ctx: &OpenCtx) -> Result<()> {
    let _g = app.order_lock.lock().await;
    let quotes = app.gw.quotes(&ctx.market.symbols).await?;
    let mids: HashMap<String, f64> = quotes
        .iter()
        .filter(|(_, q)| q.valid())
        .map(|(s, q)| (s.clone(), q.mid()))
        .collect();
    let held = app.gw.positions().await?;
    let can_sell: HashMap<String, f64> = held
        .iter()
        .map(|h| (h.symbol.clone(), h.can_sell_qty))
        .collect();
    let (mut plan, note) = {
        let g = app.paper.read().unwrap_or_else(|e| e.into_inner());
        let Some(ps) = g.as_ref() else { return Ok(()) };
        if ps.ledger.planned_for == Some(ctx.next_ts) {
            return Ok(());
        }
        let mut note = String::new();
        let mut plan = match ps.ledger.preview(
            &ps.portfolio,
            &ctx.market,
            &ctx.sc,
            &ctx.params,
            ctx.next_ts,
            ctx.next_wall,
            &mids,
        ) {
            Ok(p) => p,
            Err(e) => {
                note = format!("preview failed ({e:#}): no engine orders at this open; the candle's engine fills are reconciled against no real order");
                Vec::new()
            }
        };
        plan.extend(ps.ledger.owed_sells(ctx.next_ts, &can_sell));
        (plan, note)
    };
    // A search can invalidate settings after a decision but before its order reaches
    // the broker. Never send a buy based on an unqualified or superseded winner;
    // existing-position sells must still go through.
    let pending_uses_selected = app
        .paper
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|ps| ps.portfolio.pending_buy_uses(&ctx.params));
    if !paper_entry_eligible(app, ctx.interval, &ctx.params) || !pending_uses_selected {
        plan.retain(|p| p.side == 2);
    }
    // A strategy-cap change can leave more paper positions than the new limit.
    // At the next session open, keep the strongest current holding by marked P&L
    // and sell only weaker excess positions that are at break-even or profitable.
    // Losing excess positions remain managed until they recover or their normal
    // strategy exit fires; the cap must not force a realized loss.
    let excess: Vec<(String, f64)> = {
        let g = app.paper.read().unwrap_or_else(|e| e.into_inner());
        g.as_ref()
            .map(|ps| {
                let mut positions = ps.portfolio.positions.iter().collect::<Vec<_>>();
                positions.sort_by(|a, b| {
                    let pa = (a.mark - a.entry) * a.qty - a.fees;
                    let pb = (b.mark - b.entry) * b.qty - b.fees;
                    pb.total_cmp(&pa)
                });
                positions
                    .into_iter()
                    .skip(1)
                    .filter(|p| (p.mark - p.entry) * p.qty - p.fees >= 0.0)
                    .filter_map(|p| {
                        can_sell
                            .get(&p.symbol)
                            .copied()
                            .map(|q| (p.symbol.clone(), q.min(p.qty).floor()))
                    })
                    .filter(|(_, q)| *q > 0.0)
                    .collect()
            })
            .unwrap_or_default()
    };
    for (symbol, qty) in excess {
        if !plan.iter().any(|p| p.symbol == symbol && p.side == 2) {
            app.event("WARN", format!("POSITION CAP: scheduling sale of excess paper position {symbol} ({qty} shares) at session open"));
            plan.push(Planned {
                symbol,
                side: 2,
                qty,
                holding: 0,
                leg: moomoo_bot::engine::execution::LegKind::Sell,
                due_ts: ctx.next_ts,
                reason: "position cap reduced to one".into(),
            });
        }
    }
    // Strategy SELL decisions for this paper account are owned by the Codex
    // exit worker. Keep the Rust shadow plan for research/accounting, but do
    // not route its independently chosen SELLs to Moomoo. The worker submits
    // only after a fresh Codex decision and broker-side revalidation.
    let shadow_sells = plan.iter().filter(|p| p.side == 2).count();
    if shadow_sells > 0 {
        app.event(
            "INFO",
            format!("CODEX EXIT OWNER: suppressing {shadow_sells} Rust strategy SELL order(s); paper position remains under Codex live management"),
        );
        plan.retain(|p| p.side != 2);
    }
    if !note.is_empty() {
        app.event("ERROR", format!("ORDERS {}: {note}", fmt_et(ctx.next_wall)));
    }
    // A sell never exceeds what moomoo can sell (positions are the truth).
    let mut left = can_sell.clone();
    plan.retain_mut(|p: &mut Planned| {
        if p.side != 2 {
            return true;
        }
        let avail = left.get(&p.symbol).copied().unwrap_or(0.0).floor();
        if avail < p.qty {
            app.event(
                "WARN",
                format!(
                    "RECONCILE {}: engine sells {} but moomoo can sell {avail}; selling {avail}",
                    p.symbol, p.qty
                ),
            );
            p.qty = avail;
        }
        left.insert(p.symbol.clone(), avail - p.qty);
        p.qty > 0.0
    });
    // NOTE(agents): Trd_GetOrderFee refuses paper accounts ("Paper trading is not supported"), so
    //               each order's fee is measured from the paper account's cash around its fill
    //               (`moomoo::derived_fee`). For that, orders go out ONE AT A TIME: place, wait for
    //               the fill (bounded), read the cash; the next order starts from that cash. An
    //               order not final in time keeps its fee unknown (excluded later, never invented).
    let mut placed = Vec::new();
    let mut cash = app.gw.account().await.map(|a| a.cash).ok();
    for planned in &plan {
        let mut p = planned.clone();
        let side = if p.side == 1 { "BUY" } else { "SELL" };
        // Size to availability: a buy is cut to whole shares the account's cash can pay for right now.
        if p.side == 1 {
            if let (Some(c), Some(mid)) = (cash, mids.get(&p.symbol)) {
                let affordable = affordable_qty(c, *mid, p.qty);
                if affordable < p.qty {
                    app.event("WARN", format!("SIZE {} BUY: engine plans {} but {c:.2} USD of cash buys {affordable}; ordering {affordable}", p.symbol, p.qty));
                    p.qty = affordable;
                }
                if p.qty < 1.0 {
                    app.event(
                        "WARN",
                        format!(
                            "ORDER BUY {} skipped: no cash available for one share",
                            p.symbol
                        ),
                    );
                    placed.push((
                        p.clone(),
                        Err("skipped: no available cash".to_string()),
                        None,
                        None,
                    ));
                    continue;
                }
            }
        }
        let r = app
            .gw
            .place_market(
                &p.symbol,
                p.side,
                p.qty,
                &format!("claude-moomoo-bot {}", ctx.interval.key()),
            )
            .await;
        let mut state = None;
        let mut fee = None;
        match &r {
            Ok((id, id_ex)) => {
                app.event(
                    "INFO",
                    format!(
                        "ORDER {side} {} {} market ({}, {} open {}): moomoo order {id}",
                        p.qty,
                        p.symbol,
                        p.reason,
                        ctx.interval.label(),
                        fmt_et(ctx.next_wall)
                    ),
                );
                let started = Instant::now();
                while started.elapsed() < Duration::from_secs(45) {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    if let Ok(v) = app.gw.orders(&[*id]).await {
                        if let Some(o) = v.into_iter().find(|o| o.order_id == *id) {
                            let done = moomoo::order_final(o.status);
                            state = Some(o);
                            if done {
                                break;
                            }
                        }
                    }
                }
                let after = app.gw.account().await.map(|a| a.cash).ok();
                if let (Some(o), Some(before), Some(after)) = (
                    state.as_ref().filter(|o| moomoo::order_final(o.status)),
                    cash,
                    after,
                ) {
                    let value = o.fill_qty * o.fill_avg_price.unwrap_or(0.0);
                    let f = moomoo::derived_fee(p.side, value, before, after);
                    if o.fill_qty > 0.0 && f > -0.005 && f.is_finite() {
                        fee = Some(moomoo::OrderFee {
                            order_id_ex: id_ex.clone(),
                            total: f.max(0.0),
                            items: vec![("derived from paper cash change".into(), f.max(0.0))],
                        });
                    } else if o.fill_qty > 0.0 {
                        app.event("WARN", format!("EXCLUDED fee of order {id}: cash change {:+.4} does not fit the fill value {value:.4}", after - before));
                    }
                }
                cash = after;
            }
            Err(e) => app.event(
                "ERROR",
                format!("ORDER {side} {} {} refused: {e:#}", p.qty, p.symbol),
            ),
        }
        placed.push((p, r.map_err(|e| format!("{e:#}")), state, fee));
    }
    {
        let mut g = app.paper.write().unwrap_or_else(|e| e.into_inner());
        if let Some(ps) = g.as_mut() {
            let mut lines = Vec::new();
            for (p, r, state, fee) in placed.iter() {
                let id = ps.ledger.record(p, now_ms(), r.clone());
                if let Some(st) = state {
                    ps.ledger.update_state(st);
                    if moomoo::order_final(st.status) {
                        lines.push(match st.fill_avg_price {
                            Some(px) if st.fill_qty > 0.0 => format!(
                                "FILL {} {} {} @ {px:.4}{}",
                                if st.side == 1 { "BUY" } else { "SELL" },
                                st.fill_qty,
                                st.symbol,
                                fee.as_ref()
                                    .map_or(" (fee unknown)".to_string(), |f| format!(
                                        ", fee {:.2} USD",
                                        f.total
                                    ))
                            ),
                            _ => format!(
                                "ORDER {} {} ended without a fill (status {}{})",
                                st.symbol,
                                st.qty,
                                st.status,
                                st.last_err
                                    .as_ref()
                                    .map_or(String::new(), |e| format!(": {e}"))
                            ),
                        });
                    }
                }
                if let Some(f) = fee {
                    ps.ledger.update_fee(f);
                    if let (Some(store), Some(st)) = (&app.store, state) {
                        store.order_fee(
                            now_ms(),
                            f,
                            &st.symbol,
                            st.side,
                            st.fill_qty,
                            st.fill_avg_price.unwrap_or(0.0),
                        )?;
                    }
                }
                if let (Some(st), Some(o)) =
                    (&app.store, ps.ledger.orders.iter().find(|o| o.id == id))
                {
                    st.order(
                        now_ms(),
                        o.order_id.unwrap_or(0),
                        &serde_json::to_string(o)?,
                    )?;
                }
            }
            ps.ledger.planned_for = Some(ctx.next_ts);
            persist_paper(&app.dir.join(PAPER_FILE), ps)?;
            drop(g);
            for l in lines {
                app.event("INFO", l);
            }
        }
    }
    app.status
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .last_orders_note = format!(
        "{} open {}: {} order(s) placed{}",
        ctx.interval.label(),
        fmt_et(ctx.next_wall),
        placed.iter().filter(|x| x.1.is_ok()).count(),
        if placed.iter().any(|x| x.1.is_err()) {
            ", some refused (see the log)"
        } else {
            ""
        }
    );
    if !placed.is_empty() {
        let a = app.clone();
        tokio::spawn(async move {
            for secs in [3u64, 10, 30, 60, 120, 300] {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                if let Err(e) = refresh_orders(&a).await {
                    warn!("order refresh failed: {e:#}");
                }
            }
        });
    }
    Ok(())
}

/// The 1m bar loop: after each regular-session close, refreshes universe, account, candles and rules, steps the paper account and records it.
async fn bar_task(app: Arc<App>, hb: Heartbeat) -> Result<()> {
    let mut last_done = 0i64;
    loop {
        hb.beat();
        let sessions = app.sessions().await?;
        let now = now_ms();
        {
            let mut s = app.status.write().unwrap_or_else(|e| e.into_inner());
            let open = sessions.open_session(now);
            s.market_open = open.is_some();
            s.session_close_ms = open.map_or(0, |x| x.1);
            s.next_open_ms = sessions.next_open(now).unwrap_or(0);
        }
        if let Err(e) = maybe_place_open(&app, &sessions).await {
            app.event("ERROR", format!("ORDERS failed: {e:#}"));
        }
        // The newest closed regular-session bar (5 s grace for moomoo to publish it).
        let Some(&closed) = sessions.last_closed_bars(now - 5_000, 1).last() else {
            anyhow::bail!("moomoo calendar has no closed bars");
        };
        if closed <= last_done {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        let t0 = Instant::now();
        let session_open = sessions.open_session(now).is_some();
        let candidates = pool(&app).await?;
        let (held, bot_shares): (Vec<String>, HashMap<String, f64>) = {
            let g = app.paper.read().unwrap_or_else(|e| e.into_inner());
            let shares = g
                .as_ref()
                .map(|ps| ps.ledger.bot_shares())
                .unwrap_or_default();
            let mut h: Vec<String> = g
                .as_ref()
                .map(|ps| {
                    ps.portfolio
                        .positions
                        .iter()
                        .map(|p| p.symbol.clone())
                        .collect()
                })
                .unwrap_or_default();
            h.extend(shares.keys().cloned());
            h.sort();
            h.dedup();
            (h, shares)
        };
        // Rules: measured from real quotes while the session is open (moomoo's pre-market quotes
        // are not the regular session's); hourly in full, a newcomer at once. A failed
        // re-measurement keeps the last real measurement (stamped with `book_ts`).
        let stale = session_open
            && app
                .rules
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .0
                .is_none_or(|t| t.elapsed() >= RULES_REFRESH);
        let mut measured: Vec<Listing> = candidates.clone();
        for h in &held {
            if !measured.iter().any(|l| &l.symbol == h) {
                match app.listing(h).await {
                    Ok(l) => measured.push(l),
                    Err(e) => app.event(
                        "WARN",
                        format!("held symbol {h}: static information unavailable: {e:#}"),
                    ),
                }
            }
        }
        let have = app
            .rules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone();
        let todo: Vec<Listing> = measured
            .iter()
            .filter(|l| stale || (session_open && !have.contains_key(&l.symbol)))
            .cloned()
            .collect();
        if !todo.is_empty() {
            hb.beat();
            let quotes = app
                .gw
                .quotes(&todo.iter().map(|l| l.symbol.clone()).collect::<Vec<_>>())
                .await?;
            let fees = app.fee_points();
            for l in &todo {
                let mut r = quotes
                    .get(&l.symbol)
                    .ok_or_else(|| anyhow::anyhow!("no snapshot from moomoo"))
                    .and_then(|q| moomoo::rules_from_quote(l, q, &fees, now_ms()));
                // the stock's real average daily dollar volume and volatility, for the liquidity model (see rules.rs)
                if let (Ok(rules), Some(st)) = (r.as_mut(), app.store.as_ref()) {
                    if let Some((adv, vol)) = st.liquidity(&l.symbol) {
                        rules.adv_usd = Some(adv);
                        rules.daily_vol = Some(vol);
                    }
                }
                match r {
                    Ok(r) => {
                        if let Some(st) = &app.store {
                            st.rules(&l.symbol, &r, now_ms())?;
                        }
                        app.rules.lock().unwrap_or_else(|e| e.into_inner()).1.insert(l.symbol.clone(), r);
                    }
                    Err(e) if have.contains_key(&l.symbol) => app.event("WARN", format!("rules for {} not re-measured ({e:#}); keeping the last real measurement", l.symbol)),
                    Err(e) => app.exclude(&l.symbol, format!("no rules: {e:#}")),
                }
            }
        }
        {
            let mut guard = app.rules.lock().unwrap_or_else(|e| e.into_inner());
            if stale {
                guard.0 = Some(Instant::now());
            }
            let n = guard.1.len();
            let mut sources: Vec<String> = guard
                .1
                .values()
                .map(|r| r.commission_source.clone())
                .collect();
            sources.sort();
            sources.dedup();
            drop(guard);
            let mut s = app.status.write().unwrap_or_else(|e| e.into_inner());
            s.rules_symbols = n;
            if stale {
                s.rules_ts = now_ms();
            }
            s.commission_source = sources.join(" / ");
        }
        hb.beat();
        let account = app.gw.account().await?;
        // The capital the bot trades and sizes searches with: the account's available cash (see `capital_from`).
        let capital = capital_from(&account);
        // The real-volume liquidity model is OFF until real paper fills have shown that the simulator fills large orders at the quote (ops/fill_probe.sh
        // creates this file only when they did). Re-read every bar so the switch needs no restart.
        {
            use moomoo_bot::engine::rules::{liquidity_model_on, LIQUIDITY_MODEL};
            let want = app.dir.join("liquidity_model.on").exists();
            if want != liquidity_model_on() {
                LIQUIDITY_MODEL.store(want, std::sync::atomic::Ordering::Relaxed);
                app.event("INFO", format!("LIQUIDITY MODEL {}: orders are limited by the stock's real traded volume instead of its displayed top-of-book size", if want { "ON" } else { "OFF" }));
            }
        }
        let positions = app.gw.positions().await?;
        if let Some(st) = &app.store {
            st.account(&account, &positions)?;
        }
        // Positions moomoo holds vs the shares this bot's own fills account for.
        let mut mismatches = Vec::new();
        for (sym, q) in &bot_shares {
            let real = positions
                .iter()
                .find(|h| &h.symbol == sym)
                .map_or(0.0, |h| h.qty);
            if (real - q).abs() > 1e-9 {
                mismatches.push(format!("{sym}: bot orders filled {q}, moomoo holds {real}"));
            }
        }
        for h in positions
            .iter()
            .filter(|h| !bot_shares.contains_key(&h.symbol))
        {
            mismatches.push(format!(
                "{}: moomoo holds {} that no bot order filled (left untouched)",
                h.symbol, h.qty
            ));
        }
        let changed = app
            .status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .position_mismatches
            != mismatches;
        if changed && !mismatches.is_empty() {
            app.event(
                "WARN",
                format!("RECONCILE positions: {}", mismatches.join("; ")),
            );
        }
        let rules_now = app
            .rules
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone();
        // Every candidate with complete rules may enter; the screener picks the top UNIVERSE each bar.
        let eligible: Vec<String> = candidates
            .iter()
            .filter(|l| rules_now.contains_key(&l.symbol))
            .map(|l| l.symbol.clone())
            .collect();
        if eligible.len() < walkforward::UNIVERSE {
            app.event("WARN", format!("only {} of {} candidates have complete rules (rules are measured while the market is open)", eligible.len(), candidates.len()));
        }
        let mut symbols = eligible.clone();
        for s in held.iter().chain(std::iter::once(&REFERENCE.to_string())) {
            if !symbols.contains(s) {
                symbols.push(s.clone());
            }
        }
        symbols.sort();
        // Each interval's candles: the test window plus (1h / 4h / one session) the warm-up before
        // it. The fetch reaches back exactly to the oldest of them, nothing older.
        // The 1m market is always built: it is the base the longer candles are aggregated from and it
        // supplies the symbols, rules and eligibility. Only the LIVE intervals are searched, tested and traded.
        let plans: Vec<(Interval, Arc<Sessions>, Vec<i64>)> = std::iter::once(Interval::M1)
            .chain(Interval::LIVE.into_iter().filter(|i| *i != Interval::M1))
            .map(|i| {
                let s = Arc::new(sessions.with_interval(i));
                let w = s.last_closed_bars(now, i.test_candles() + i.warmup_candles());
                (i, s, w)
            })
            .collect();
        let oldest = plans
            .iter()
            .filter_map(|p| p.2.first().copied())
            .min()
            .ok_or_else(|| anyhow::anyhow!("moomoo calendar has no closed candles"))?;
        let mut must: Vec<String> = held.clone();
        must.push(REFERENCE.to_string());
        let data = app
            .fetch(&symbols, &must, oldest, &sessions, &eligible, &hb)
            .await?;
        let (reference, error) = data
            .iter()
            .find(|(d, _)| d.symbol == REFERENCE)
            .ok_or_else(|| anyhow::anyhow!("{REFERENCE} was not fetched"))?;
        if let Some(e) = error {
            anyhow::bail!("{REFERENCE} data unavailable: {e}");
        }
        // moomoo publishes a candle shortly after it closes: wait (bounded) until the reference has it.
        if !reference.trades.contains_key(&closed) {
            let waited = now_ms() - (closed + BAR_MS);
            anyhow::ensure!(
                waited < PUBLISH_WAIT_MS,
                "moomoo has not published the {REFERENCE} candle closing {} after {} s",
                fmt_et(closed + BAR_MS),
                waited / 1000
            );
            tokio::time::sleep(Duration::from_secs(10)).await;
            continue;
        }
        // The calendar against the reference's real candles (sessions fully inside the data).
        let (closures, problems) =
            moomoo::check_calendar(&sessions, &reference.trades, oldest, closed + BAR_MS);
        if !closures.is_empty() {
            app.event("WARN", format!("CALENDAR: {} session(s) in moomoo's calendar have no {REFERENCE} candles at all (unscheduled closures: {}); removed from the trading clock", closures.len(), closures.iter().map(|c| fmt_et(c.0)).collect::<Vec<_>>().join(", ")));
            app.closures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend(closures);
            *app.sessions.write().unwrap_or_else(|e| e.into_inner()) = None;
            continue;
        }
        let note = if problems.is_empty() {
            format!(
                "moomoo calendar matches the {REFERENCE} candles ({} sessions checked)",
                sessions
                    .list()
                    .iter()
                    .filter(|s| s.0 >= oldest && s.1 <= closed + BAR_MS)
                    .count()
            )
        } else {
            format!("calendar vs {REFERENCE} candles: {}", problems.join("; "))
        };
        let prev_note = std::mem::replace(
            &mut app
                .status
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .calendar_note,
            note.clone(),
        );
        if prev_note != note && !problems.is_empty() {
            app.event("WARN", format!("CALENDAR {note}"));
        }
        let sync_secs = t0.elapsed().as_secs_f64();
        let walls = sessions.last_closed_bars(now, BARS);
        anyhow::ensure!(
            walls.last() == Some(&closed) && walls.len() == BARS,
            "calendar window mismatch"
        );
        let mut failed: Vec<(String, String)> = Vec::new();
        let mut ok: Vec<SymbolData> = Vec::new();
        for (d, e) in data {
            match e {
                Some(e) => failed.push((d.symbol.clone(), e)),
                None => ok.push(d),
            }
        }
        for (s, e) in &failed {
            app.exclude(s, format!("candles unavailable: {e}"));
        }
        for d in &ok {
            if rules_now.contains_key(&d.symbol) || d.symbol == REFERENCE {
                let stale_reason = app
                    .excluded
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&d.symbol)
                    .is_some_and(|w| !w.starts_with("screener"));
                if stale_reason {
                    app.include(&d.symbol);
                }
            }
        }
        // NOTE(agents): Skipping is fine for symbols we don't hold. A HELD symbol with incomplete
        //               data must fail the bar: never mark or close a position on missing prices.
        let paper_on = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|ps| ps.interval);
        let engine_held: Vec<String> = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|ps| {
                ps.portfolio
                    .positions
                    .iter()
                    .map(|p| p.symbol.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut list = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        if let Some((_, e)) = failed.iter().find(|(s, _)| engine_held.contains(s)) {
            anyhow::bail!("held symbol data unavailable: {e}");
        }
        for (i, isess, iwalls) in &plans {
            let strict = *i == Interval::M1 || Some(*i) == paper_on;
            let (frame, bad) = build_frame(
                *i,
                isess.clone(),
                iwalls,
                &ok,
                if strict { &engine_held } else { &[] },
            )?;
            for (sym, e) in bad {
                skipped.push(format!("{} {sym} ({e})", i.label()));
            }
            list.push(frame);
        }
        if !skipped.is_empty() {
            app.event(
                "WARN",
                format!(
                    "EXCLUDED this bar for incomplete real data: {} symbol/interval(s) (first: {})",
                    skipped.len(),
                    skipped[0]
                ),
            );
        }
        let m15 = list[0].market.clone();
        anyhow::ensure!(
            list[0].interval == Interval::M1
                && m15.wall.last() == Some(&closed)
                && m15.ts.len() == BARS,
            "1m window mismatch"
        );
        symbols.retain(|s| m15.symbols.contains(s));
        hb.beat();
        // NOTE(agents): The search sizes from the bot's own paper equity (START_CAPITAL until the
        //               paper account exists), never from the 1,000,000 USD moomoo paper balance.
        let wf_equity = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(capital, |ps| {
                ps.portfolio.equity + ps.portfolio.unrealized()
            })
            .max(0.0);
        let common_start = common_span_start(&sessions, closed + BAR_MS).ok_or_else(|| {
            anyhow::anyhow!("the calendar has fewer than {TEST_SESSIONS} closed sessions")
        })?;
        let frames = Frames {
            list,
            equity: wf_equity,
            common_start,
        };
        *app.frames.write().unwrap_or_else(|e| e.into_inner()) = Some(frames.clone());
        hb.beat();
        last_done = closed;

        // Real order states and fees before the engine books this candle.
        if let Err(e) = refresh_orders(&app).await {
            app.event("WARN", format!("order refresh failed: {e:#}"));
        }

        // NOTE(agents): Paper trades the best profitable, SAFE settings of the interval chosen by the latest
        //               search round (`paper_choice`: best winner over the last 14 sessions). Entries
        //               stop only when no interval has safe settings or before the first round
        //               finishes; held positions are then still managed with the last settings.
        let searches = app
            .searches
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let target = *app.paper_choice.read().unwrap_or_else(|e| e.into_inner());
        let chosen = target
            .and_then(|i| searches.get(&i))
            .and_then(|x| x.chosen.clone());
        let mut paper_events: Vec<EngineEvent> = Vec::new();
        let mut market_ctx: Option<String> = None;
        // A paper account that has never traded and holds nothing is re-based to the configured capital
        // (user 2026-10-09: 5000 USD account). Anything with positions or trades keeps its own history.
        {
            let mut g = app.paper.write().unwrap_or_else(|e| e.into_inner());
            if let Some(ps) = g.as_mut() {
                if ps.portfolio.positions.is_empty()
                    && ps.portfolio.trades.is_empty()
                    && (ps.portfolio.metrics().start_equity - capital).abs() > capital * 0.01
                {
                    ps.portfolio = XsPortfolio::new(capital);
                    ps.ledger.planned_for = None;
                    persist_paper(&app.dir.join(PAPER_FILE), ps)?;
                    drop(g);
                    app.event("INFO", format!("PAPER account re-based to {capital:.0} USD = the available cash (it had no trades and no positions)"));
                }
            }
        }
        // A paper account left on an interval the service no longer runs moves to the
        // live interval at once, but only while it holds nothing: a position is never force-closed for this.
        let flat = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(|ps| ps.portfolio.positions.is_empty());
        let legacy = paper_on.filter(|i| !Interval::LIVE.contains(i) && flat);
        // Search completion may happen during a live session. Switch immediately only at a
        // safe handoff: flat portfolio, no open order plan, and no order currently being placed.
        // This lets a better interval catch the next signal without closing a live position or
        // discarding an already planned order.
        let planned = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|ps| ps.ledger.planned_for);
        let order_plan_pending = app
            .open_ctx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        // A pending buy must not pin a flat account to settings that the latest
        // search has disqualified. The broker-order gate already blocks that buy.
        let current_disqualified = paper_on.is_some_and(|i| {
            searches.get(&i).is_some_and(|s| {
                s.chosen.is_none()
                    || !s
                        .common_return_pct
                        .is_some_and(|r| r.is_finite() && r > 0.0)
            })
        });
        let between_sessions = sessions.open_session(now_ms()).is_none();
        let safe_handoff = can_handoff_interval(
            flat,
            planned.is_some(),
            order_plan_pending,
            current_disqualified,
            between_sessions,
        );
        if safe_handoff {
            if let (Some(from), Some(to)) = (legacy, Interval::LIVE.first().copied()) {
                let line = switch_interval(&app, &sessions, &m15, from, to, &frames)?;
                app.event("INFO", line);
            } else if let (Some(to), Some(from)) = (target, paper_on) {
                if to != from && chosen.is_some() {
                    let line = switch_interval(&app, &sessions, &m15, from, to, &frames)?;
                    if current_disqualified || between_sessions {
                        *app.open_ctx.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    }
                    app.event("INFO", line);
                }
            }
        }
        let use_i = app.paper_interval();
        let f = frames
            .get(use_i)
            .ok_or_else(|| anyhow::anyhow!("no {} market this bar", use_i.label()))?
            .clone();
        let market = f.market.clone();
        let n = market.ts.len();
        let prev_params = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|ps| ps.params.clone());
        let entries_allowed = chosen
            .as_ref()
            .is_some_and(|p| paper_entry_eligible(&app, use_i, p));
        let m2 = market.clone();
        let sc = Arc::new(
            tokio::task::spawn_blocking(move || scores::compute(&m2, walkforward::UNIVERSE))
                .await?,
        );
        let ready = f
            .sessions
            .clock_now(now_ms())
            .ok_or_else(|| anyhow::anyhow!("moomoo calendar has no next session"))?;
        // A longer interval decides only when one of its candles has closed.
        let new_candle = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(|ps| market.ts[n - 1] > ps.last_ts);
        let settings = if entries_allowed {
            chosen.clone()
        } else {
            prev_params
        };
        if let (Some(p), true) = (settings, new_candle) {
            let new_lines = {
                let mut guard = app.paper.write().unwrap_or_else(|e| e.into_inner());
                let mut candidate = guard.clone().unwrap_or_else(|| PaperState {
                    schema_version: 2,
                    interval: use_i,
                    interval_since: None,
                    settlements_seen: Default::default(),
                    portfolio: XsPortfolio::new(capital),
                    last_ts: market.ts[n - 1] - BAR_MS,
                    params: None,
                    last_account_balance: None,
                    ledger: Ledger::default(),
                });
                let ps = &mut candidate;
                let (before_trades, before_pos): (usize, Vec<(String, Side, bool)>) = (
                    ps.portfolio.trades.len(),
                    ps.portfolio
                        .positions
                        .iter()
                        .map(|x| (x.symbol.clone(), x.side, x.added))
                        .collect(),
                );
                let before_positions = ps.portfolio.positions.clone();
                // The symbol list is re-fetched every bar: re-point positions by name.
                ps.portfolio.reindex(&market.symbols);
                paper_events = advance_paper(ps, &market, &sc, &p, entries_allowed, ready)?;
                // Real fills replace the simulated ones (orders placed at this candle's open).
                let mut lines = ps.ledger.link_step(
                    &mut ps.portfolio,
                    &before_positions,
                    before_trades,
                    Some(market.ts[n - 1]),
                );
                lines.extend(ps.ledger.recompute(&mut ps.portfolio));
                ps.ledger.compact(500);
                persist_paper(&app.dir.join(PAPER_FILE), ps)?;
                lines.extend(
                    ps.portfolio.trades[before_trades.min(ps.portfolio.trades.len())..]
                        .iter()
                        .map(|t| {
                            format!(
                                "PAPER {} close {:?} {} ({}) pnl {:+.2} USD, held {} candles",
                                use_i.label(),
                                t.side,
                                t.symbol,
                                t.reason,
                                t.pnl,
                                (t.exit_ts - t.entry_ts) / BAR_MS
                            )
                        }),
                );
                for x in &ps.portfolio.positions {
                    let before = before_pos.iter().find(|b| b.0 == x.symbol && b.1 == x.side);
                    if before.is_some_and(|b| !b.2) && x.added {
                        lines.push(format!("PAPER {} add {:?} {}: closed {}% against, size doubled, average now {}", use_i.label(), x.side, x.symbol, p.risk.add_pct.unwrap_or_default(), x.entry));
                    }
                    if before.is_none() {
                        lines.push(format!(
                            "PAPER {} open {:?} {} {} @ {} (stop {})",
                            use_i.label(),
                            x.side,
                            x.symbol,
                            x.qty,
                            x.entry,
                            x.stop.map_or("-".to_string(), |s| format!("{s:.4}"))
                        ));
                    }
                }
                *guard = Some(candidate);
                lines
            };
            for l in new_lines {
                app.event("INFO", l);
            }
            app.save_paper()?;

            // Signals at the latest candle.
            let t = n - 1;
            let targets = if entries_allowed {
                app.paper
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|ps| ps.portfolio.executable_targets(&market, &sc, t, &p))
                    .unwrap_or_default()
            } else {
                Default::default()
            };
            let held_now: Vec<(usize, Side)> = app
                .paper
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|ps| {
                    ps.portfolio
                        .positions
                        .iter()
                        .map(|x| (x.sym, x.side))
                        .collect()
                })
                .unwrap_or_default();
            let mut rows: Vec<SignalRow> = (0..market.symbols.len())
                .filter_map(|s| {
                    let score = sc[s][t]?;
                    Some(SignalRow {
                        symbol: market.symbols[s].clone(),
                        signal: xs::signal_value(&market, &sc, s, t, &p)?,
                        rank: 0,
                        target: targets.get(&s).copied(),
                        held: held_now.iter().find(|h| h.0 == s).map(|h| h.1),
                        close: market.bars[s][t]?.close,
                        turnover_24h: score.turnover_24h,
                    })
                })
                .collect();
            rows.sort_by(|a, b| a.signal.total_cmp(&b.signal));
            for (i, r) in rows.iter_mut().enumerate() {
                r.rank = i + 1;
            }
            *app.signals.write().unwrap_or_else(|e| e.into_inner()) = rows;
            market_ctx = Some(xs::market_context(&market, &sc, t, &p).to_string());
        } else if new_candle {
            paper_events.push(EngineEvent {
                ts_ms: market.ts[n - 1],
                kind: "no_settings",
                symbol: None,
                detail: serde_json::json!({
                    "reason": "no settings exist yet (the first search round has not produced any): no paper decision this candle",
                }),
            });
        }
        // The orders of the next open of the paper interval (placed when it opens). Set every bar
        // (also after a restart), never twice for the same open.
        let ctx_now = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|ps| {
                let params = ps.params.clone()?;
                (ps.interval == use_i && ps.last_ts == market.ts[n - 1])
                    .then_some((params, ps.ledger.planned_for))
            });
        if let Some((params, planned)) = ctx_now {
            let next_ts = market.ts[n - 1] + BAR_MS;
            if let Some(next_wall) = f.sessions.wall(next_ts) {
                if planned != Some(next_ts) {
                    *app.open_ctx.lock().unwrap_or_else(|e| e.into_inner()) = Some(OpenCtx {
                        interval: use_i,
                        next_ts,
                        next_wall,
                        market: market.clone(),
                        sc: sc.clone(),
                        params,
                    });
                    app.status
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .next_order_open_ms = next_wall;
                }
            }
        }
        if let Err(e) = maybe_place_open(&app, &sessions).await {
            app.event("ERROR", format!("ORDERS failed: {e:#}"));
        }
        let search = searches.get(&use_i).cloned();
        let chosen = search.as_ref().and_then(|x| x.chosen.clone());

        let hold = chosen.as_ref().map_or(16, |p| p.hold) as i64;
        let choice = search.as_ref().map(|x| x.choice);
        // Next rebalance: the next clock time aligned to `hold`, shown in wall time (candle close).
        let paper_last = market.ts[n - 1];
        let next_rebalance = ((paper_last + BAR_MS) / (hold * BAR_MS) + 1) * hold * BAR_MS;
        let wall = app.wall_fn(use_i);
        let paper = app.paper.read().unwrap_or_else(|e| e.into_inner());
        let mut snap: Option<(PaperPoint, Vec<moomoo_bot::engine::metrics::Trade>)> = None;
        let mut paper_params: Option<XsParams> = None;
        let mut s = app.status.write().unwrap_or_else(|e| e.into_inner());
        s.last_bar_ts = closed;
        s.account = Some(account.clone());
        s.broker_positions = positions.clone();
        s.bot_shares = bot_shares.iter().map(|(k, v)| (k.clone(), *v)).collect();
        s.position_mismatches = mismatches;
        s.exclusions = app
            .excluded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        // Shown as the deciding bar's close (wall time).
        s.next_rebalance_ts = wall(next_rebalance - BAR_MS)
            .and_then(|w| f.sessions.close_of(w))
            .unwrap_or(0);
        s.paper_interval = Some(use_i);
        s.symbols = symbols.len();
        s.sync_secs = sync_secs;
        s.choice = choice;
        s.params = chosen.clone();
        s.search_finished_ms = search.as_ref().map_or(0, |x| x.finished_ms);
        if let Some(ps) = paper.as_ref() {
            let m = ps.portfolio.metrics();
            s.paper_equity = m.end_equity + m.open_unrealized;
            s.paper_start_equity = m.start_equity;
            s.paper_open_positions = ps.portfolio.positions.len();
            s.paper_trades = m.trades;
            s.paper_wins = m.wins;
            s.paper_net = m.net();
            s.paper_realized = m.end_equity - m.start_equity;
            s.paper_open = m.open_unrealized;
            s.paper_max_drawdown_pct = m.max_drawdown_pct;
            s.resume_not_before = if ps.portfolio.stopped_out {
                ps.portfolio.resume_not_before.and_then(&wall)
            } else {
                None
            };
            if ps.portfolio.entries_allowed && entries_allowed {
                s.effective_pairs = ps.portfolio.effective_top;
                s.allocation_note = ps.portfolio.allocation_note.clone();
            } else {
                s.effective_pairs = 0;
                s.allocation_note =
                    "no entries: no currently profitable settings pass the safety rules".into();
            }
            paper_params = ps.params.clone();
            let skip = ps.portfolio.trades.len().saturating_sub(500);
            snap = Some((
                PaperPoint {
                    run_id: app.run_id,
                    wall_ms: closed,
                    equity: m.end_equity + m.open_unrealized,
                    start_equity: m.start_equity,
                    realized: m.end_equity - m.start_equity,
                    open_pnl: m.open_unrealized,
                    max_drawdown_pct: m.max_drawdown_pct,
                    open_positions: ps.portfolio.positions.len(),
                    closed_trades: m.trades,
                    wins: m.wins,
                    params_json: serde_json::to_string(&ps.params).unwrap_or_default(),
                    mode: String::new(),
                    positions_json: serde_json::to_string(&ps.portfolio.positions)
                        .unwrap_or_default(),
                },
                ps.portfolio.trades[skip..].to_vec(),
            ));
        }
        let stopped = paper.as_ref().is_some_and(|ps| ps.portfolio.stopped_out);
        s.mode = format!("{}: {}", use_i.label(), match choice {
            _ if stopped && s.paper_open_positions > 0 => "DRAWDOWN EXIT (25%, closing positions before resuming)",
            _ if stopped => "COOLDOWN (25% drawdown exit; re-evaluate after one candle when entry gates pass)",
            _ if !entries_allowed && target.is_some() => "NO ENTRIES (waiting for eligible interval handoff)",
            None => "SEARCHING (first search running)",
            Some(walkforward::Choice::NewBest) => "TRADING (new best settings)",
            Some(walkforward::Choice::KeptLastBest) => "TRADING (kept last best settings)",
            Some(walkforward::Choice::NoneSafe) if s.paper_open_positions > 0 => "NO SAFE SETTINGS (exits pending)",
            Some(walkforward::Choice::NoneSafe) => "NO SAFE SETTINGS (flat)",
            Some(walkforward::Choice::KeptUnsafe) => "TRADING (kept last best; none passed the safety rules this time)",
        });
        let verdict = match search.as_ref().map(|x| &x.report) {
            None => "no search finished yet".to_string(),
            Some(r) => format!(
                "last search: {} of {} combinations, {} usable",
                r.evaluated, r.combos, r.usable
            ),
        };
        let summary = format!(
            "bar {} | {} symbols | sync {:.0}s | {} | {} | paper equity {:.2} of {capital:.0} | moomoo paper account {:.2}",
            fmt_et(closed + BAR_MS),
            symbols.len(),
            sync_secs,
            s.mode,
            verdict,
            s.paper_equity,
            account.total_assets
        );
        s.report = search.map(|x| x.report);
        let mode = s.mode.clone();
        drop(s);
        drop(paper);
        if let Some(st) = &app.store {
            if let Some((mut point, trades)) = snap {
                point.mode = mode;
                let cache = std::cell::RefCell::new(HashMap::new());
                let features = |t: &Trade| match paper_params.as_ref() {
                    Some(p) => {
                        entry_features(&market, &f.closes, &sc, p, t, &mut cache.borrow_mut())
                    }
                    None => serde_json::Value::Null,
                };
                if let Err(e) = st.paper(&point, &trades, &wall, features) {
                    warn!("store paper write failed: {e:#}");
                }
            }
            if let Err(e) = st.events(app.run_id, &paper_events, &wall) {
                warn!("store event write failed: {e:#}");
            }
            if let Some(json) = market_ctx {
                if let Err(e) = st.market(app.run_id, closed, &json) {
                    warn!("store market write failed: {e:#}");
                }
            }
            match st.enforce_limit() {
                Ok(0) => {}
                Ok(n) => app.event(
                    "WARN",
                    format!("store reached 10 GB: culled the {n} oldest candle rows"),
                ),
                Err(e) => warn!("store size check failed: {e:#}"),
            }
            let (bytes, candles) = (st.size_bytes().unwrap_or(0), st.candle_count().unwrap_or(0));
            let mut s = app.status.write().unwrap_or_else(|e| e.into_inner());
            s.store_bytes = bytes;
            s.store_candles = candles;
        }
        app.event("INFO", summary);
        // Forward-track update runs on its own task so the bar loop never waits for it.
        let (a, fr) = (app.clone(), frames.clone());
        tokio::spawn(async move {
            if let Err(e) = update_forward(&a, &fr).await {
                warn!("forward track update failed: {e:#}");
            }
        });
        hb.beat();
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct PaperState {
    #[serde(default = "legacy_version")]
    schema_version: u32,
    /// The candle interval this account trades; every clock time in it is on that interval's
    /// clock (accounts saved before intervals existed are 1m).
    #[serde(default)]
    interval: Interval,
    /// Wall time of the last interval switch (None = never switched since this field existed).
    #[serde(default)]
    interval_since: Option<i64>,
    #[serde(default)]
    settlements_seen: std::collections::BTreeMap<String, f64>,
    portfolio: XsPortfolio,
    last_ts: i64,
    /// Params last used, so open positions stay managed (stops, rebalances)
    /// even on a bar where no params qualify.
    #[serde(default)]
    params: Option<XsParams>,
    /// Real account balance last mirrored; a change is a deposit or withdrawal.
    #[serde(default)]
    last_account_balance: Option<f64>,
    /// Every real moomoo paper order and the engine holding it implements.
    #[serde(default)]
    ledger: Ledger,
}

/// Paper-state file version written before versions existed (serde default).
fn legacy_version() -> u32 {
    1
}

// NOTE(agents): Never silently reset the paper account: a missing file is a first run, anything
//               unreadable or invalid stops the service. To migrate state, write an explicit
//               migration, not a fallback.
fn load_paper(path: &Path) -> Result<Option<PaperState>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut state: PaperState = serde_json::from_str(&text)?;
    anyhow::ensure!(
        state.schema_version <= 2,
        "unsupported paper schema {}",
        state.schema_version
    );
    if state.schema_version < 2 {
        anyhow::ensure!(
            state.portfolio.positions.is_empty() && state.portfolio.trades.is_empty(),
            "legacy paper history needs replay migration; refusing to reset it"
        );
        state.schema_version = 2;
    }
    state.portfolio.validate_state()?;
    anyhow::ensure!(
        state.last_ts >= 0 && state.last_ts % BAR_MS == 0,
        "invalid paper checkpoint"
    );
    Ok(Some(state))
}

/// Writes the paper state atomically (temp file, fsync, rename, fsync folder) after validating it.
fn persist_paper(path: &Path, state: &PaperState) -> Result<()> {
    state.portfolio.validate_state()?;
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    serde_json::to_writer(&mut file, state)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

// NOTE(agents): Causality order: (1) replay missed bars with the PREVIOUS settings, (2) apply what
//               was observed now (wallet change, committed margin), (3) install
//               new settings and decide at the latest close. Anything learned now must never change
//               a replayed bar.
/// Replay only contiguous unseen bars with the previously known settings.
/// Later balance observations and reports must not resize historical orders.
/// One interval's market from the shared closed 1-minute bars. Symbols with incomplete candles
/// are dropped (returned with the reason) unless listed in `must_keep`, which fails the bar.
fn build_frame(
    interval: Interval,
    sessions: Arc<Sessions>,
    walls: &[i64],
    data: &[SymbolData],
    must_keep: &[String],
) -> Result<(Frame, Vec<(String, String)>)> {
    anyhow::ensure!(
        walls.len() >= interval.test_candles(),
        "{}: the calendar has only {} closed candles",
        interval.label(),
        walls.len()
    );
    let mut m = moomoo::build_market(&sessions, walls, data)?;
    let mut bad: Vec<(String, String)> = Vec::new();
    for k in 0..m.symbols.len() {
        if let Err(e) = m.validate_symbol(k) {
            bad.push((m.symbols[k].clone(), format!("{e:#}")));
        }
    }
    if let Some((sym, e)) = bad.iter().find(|(s, _)| must_keep.contains(s)) {
        anyhow::bail!(
            "{}: held symbol {sym} data incomplete: {e}",
            interval.label()
        );
    }
    if !bad.is_empty() {
        let keep: Vec<bool> = m
            .symbols
            .iter()
            .map(|s| bad.iter().all(|(b, _)| b != s))
            .collect();
        /// Keeps the entries of `v` whose flag in `keep` is true (same order).
        fn pick<T>(v: &mut Vec<T>, keep: &[bool]) {
            let mut it = keep.iter();
            v.retain(|_| *it.next().unwrap_or(&false));
        }
        pick(&mut m.symbols, &keep);
        pick(&mut m.listing_times, &keep);
        pick(&mut m.entry_eligible, &keep);
        pick(&mut m.bars, &keep);
        pick(&mut m.marks, &keep);
        pick(&mut m.funding, &keep);
        pick(&mut m.rules, &keep);
    }
    m.validate()?;
    let closes = walls
        .iter()
        .map(|&w| {
            sessions
                .close_of(w)
                .ok_or_else(|| anyhow::anyhow!("candle {w} is off the calendar"))
        })
        .collect::<Result<Vec<i64>>>()?;
    Ok((
        Frame {
            interval,
            market: Arc::new(m),
            sessions,
            test_start: walls.len() - interval.test_candles(),
            closes,
        },
        bad,
    ))
}

// NOTE(agents): Interval switch (user request 2026-10-06: "paper chooses the winner"). Open
//               positions are closed at the latest real 1m close (like any exit: commission and
//               half spread), then the flat account moves onto the new interval's clock. Equity,
//               closed trades and drawdown history carry over; nothing is re-simulated.
/// Move the paper account from interval `from` to `to`; returns the log line.
fn switch_interval(
    app: &App,
    sessions: &Sessions,
    m15: &Market,
    from: Interval,
    to: Interval,
    frames: &Frames,
) -> Result<String> {
    let mut guard = app.paper.write().unwrap_or_else(|e| e.into_inner());
    let Some(ps) = guard.as_ref() else {
        return Ok(String::new());
    };
    let target = frames
        .get(to)
        .ok_or_else(|| anyhow::anyhow!("no {} market", to.label()))?;
    let (old, new) = (sessions.with_interval(from), sessions.with_interval(to));
    let mut c = ps.clone();
    let open = c.portfolio.positions.len();
    let before = (c.portfolio.trades.len(), c.portfolio.equity);
    let before_positions = c.portfolio.positions.clone();
    c.portfolio.reindex(&m15.symbols);
    anyhow::ensure!(
        c.portfolio.positions.len() == open,
        "a held symbol is missing from the 1m market"
    );
    if open > 0 {
        let last = m15.ts.len() - 1;
        c.portfolio.close_all(
            m15,
            m15.ts[last] + BAR_MS,
            &format!("Interval switch {} -> {}", from.label(), to.label()),
        );
        if let Some(e) = &c.portfolio.execution_error {
            anyhow::bail!("interval switch close failed: {e}");
        }
        // The switch's exits are engine exits like any other: their real sells go out at the next
        // open (`Ledger::owed_sells`) and their real prices replace the simulated ones.
        for l in c
            .ledger
            .link_step(&mut c.portfolio, &before_positions, before.0, None)
        {
            log::info!("{l}");
        }
    }
    c.ledger.planned_for = None;
    // The switch closes happened on the 1m clock: keep their real exit times.
    let exit_walls: Vec<i64> = c.portfolio.trades[before.0..]
        .iter()
        .map(|t| {
            sessions
                .wall(t.exit_ts)
                .ok_or_else(|| anyhow::anyhow!("exit {} is off the calendar", t.exit_ts))
        })
        .collect::<Result<_>>()?;
    for (t, &w) in c.portfolio.trades[before.0..].iter_mut().zip(&exit_walls) {
        t.exit_ts = old.clock_at(w);
    }
    c.portfolio.reclock(|t| {
        old.wall(t)
            .map(|w| new.clock_at(w))
            .ok_or_else(|| anyhow::anyhow!("clock {t} is off the {} calendar", from.label()))
    })?;
    for (t, &w) in c.portfolio.trades[before.0..].iter_mut().zip(&exit_walls) {
        t.exit_ts = new.clock_at(w);
    }
    c.portfolio.validate_state()?;
    c.interval = to;
    c.interval_since = Some(now_ms());
    c.params = None;
    c.last_ts = target.market.ts[target.market.ts.len() - 1] - BAR_MS;
    persist_paper(&app.dir.join(PAPER_FILE), &c)?;
    let pnl = c.portfolio.equity - before.1;
    *guard = Some(c);
    Ok(format!(
        "PAPER interval switch {} -> {}: closed {open} position(s) at the latest 1m close ({pnl:+.2} USD); {} settings start at its next candle close",
        from.label(),
        to.label(),
        to.label()
    ))
}

/// Steps the paper account over the new candles with settings `next`, entries allowed or not, and returns the engine events.
fn advance_paper(
    ps: &mut PaperState,
    market: &Market,
    sc: &[Vec<Option<scores::Score>>],
    next: &XsParams,
    allowed: bool,
    ready_ts: i64,
) -> Result<Vec<EngineEvent>> {
    anyhow::ensure!(
        next.long_only,
        "shorting is removed: paper trades long only"
    );
    anyhow::ensure!(!market.ts.is_empty(), "empty paper market");
    anyhow::ensure!(
        market.ts.windows(2).all(|w| w[1] - w[0] == BAR_MS),
        "noncontiguous paper timeline"
    );
    let mut candidate = ps.clone();
    // Record every decision, denial and gate for the research archive.
    candidate.portfolio.enable_events();
    if let Some(previous) = candidate.params.clone() {
        if let Some(&first) = market.ts.iter().find(|&&ts| ts > candidate.last_ts) {
            anyhow::ensure!(first==candidate.last_ts+BAR_MS,"paper checkpoint {} predates available history starting at {first}; recovery replay required",candidate.last_ts);
        }
        for (sym, name) in market.symbols.iter().enumerate() {
            for &(ts, rate) in &market.funding[sym] {
                if ts > candidate.last_ts {
                    continue;
                }
                let owned = candidate
                    .portfolio
                    .positions
                    .iter()
                    .any(|p| p.symbol == *name && p.entry_ts < ts)
                    || candidate
                        .portfolio
                        .trades
                        .iter()
                        .any(|p| p.symbol == *name && p.entry_ts < ts && p.exit_ts >= ts);
                if owned {
                    let key = format!("{name}:{ts}");
                    anyhow::ensure!(
                        candidate.settlements_seen.get(&key) == Some(&rate),
                        "late/revised funding {key}: replay recovery required"
                    );
                }
            }
        }
        for t in 0..market.ts.len() {
            if market.ts[t] > candidate.last_ts {
                let ts = market.ts[t];
                for pos in &candidate.portfolio.positions {
                    for &(f, rate) in &market.funding[pos.sym] {
                        if f == ts {
                            candidate
                                .settlements_seen
                                .insert(format!("{}:{f}", pos.symbol), rate);
                        }
                    }
                }
                candidate.portfolio.step(market, sc, t, &previous);
                candidate.portfolio.validate_state()?;
                for pos in &candidate.portfolio.positions {
                    if let Some(f) = pos.last_funding_ts {
                        if f == ts + BAR_MS {
                            if let Some(&(_, rate)) =
                                market.funding[pos.sym].iter().find(|(time, _)| *time == f)
                            {
                                candidate
                                    .settlements_seen
                                    .insert(format!("{}:{f}", pos.symbol), rate);
                            }
                        }
                    }
                }
                candidate.last_ts = ts;
            }
        }
    }
    // NOTE(agents): No balance mirroring (user decision 2026-10-06): the bot trades its own
    //               START_CAPITAL; the moomoo paper balance is never a deposit or withdrawal here.
    let t = market.ts.len() - 1;
    candidate.last_ts = market.ts[t];
    // Pending decisions may carry frozen settings. Once those settings are no longer
    // the selected winner, cancel their buys; otherwise a losing old setting could
    // enter after a fresh search. Same-setting decisions still survive each search.
    if candidate.params.is_none()
        || candidate.portfolio.entries_allowed != allowed
        || !candidate.portfolio.pending_buy_uses(next)
    {
        candidate.portfolio.install_entry_gate(allowed);
    }
    candidate.params = Some(next.clone());
    candidate.portfolio.decide_close(market, sc, t, next);
    candidate
        .portfolio
        .defer_new_decisions(ready_ts, market.ts[t]);
    let events = candidate.portfolio.take_events();
    *ps = candidate;
    Ok(events)
}

/// Status wording for a search outcome.
fn mode_text(choice: Option<walkforward::Choice>) -> &'static str {
    match choice {
        None => "SEARCHING (first search running)",
        Some(walkforward::Choice::NewBest) => "NEW BEST SETTINGS (backtest)",
        Some(walkforward::Choice::KeptLastBest) => "KEPT LAST BEST SETTINGS (backtest)",
        Some(walkforward::Choice::NoneSafe) => "NO SAFE SETTINGS",
        Some(walkforward::Choice::KeptUnsafe) => {
            "KEPT LAST BEST (nothing passed the safety rules this time)"
        }
    }
}

/// Publish the walk-forward view of the settings in use (the paper interval's).
fn publish_walk_forward(app: &App) {
    let i = app.paper_interval();
    let mut st = app.status.write().unwrap_or_else(|e| e.into_inner());
    let cands = app.candidates.read().unwrap_or_else(|e| e.into_inner());
    st.walk_forward = st.params.as_ref().and_then(|p| {
        cands
            .get(&i)?
            .iter()
            .rev()
            .find(|c| &c.params == p)
            .map(|c| WalkForwardView {
                verdict: walkforward::verdict(c),
                candidate: c.clone(),
            })
    });
}

/// Start the forward track of newly chosen settings (in-sample result on the frame's test window;
/// the forward track is updated with every bar). Same settings are never re-registered.
async fn register_candidate(
    app: &Arc<App>,
    report: &SearchReport,
    chosen: Option<XsParams>,
    f: Frame,
    equity: f64,
) -> Result<()> {
    let Some(p) = chosen else { return Ok(()) };
    if app
        .candidates
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&f.interval)
        .is_some_and(|v| v.iter().any(|c| c.params == p))
    {
        return Ok(());
    }
    let chosen_bar_ts = report.last_bar_ts;
    let interval = f.interval;
    let cand = tokio::task::spawn_blocking(move || {
        let sc = scores::compute(&f.market, walkforward::UNIVERSE);
        let own = walkforward::evaluate_window("in-sample", &f.market, &sc, equity, &p, f.range());
        Candidate {
            params: p,
            chosen_bar_ts,
            in_sample_return_pct: own.return_pct,
            in_sample_drawdown_pct: own.max_drawdown_pct,
            in_sample: own,
            // The candidate view tracks in-sample + forward; the walk-forward test is separate.
            oos: Vec::new(),
            forward: None,
        }
    })
    .await?;
    {
        let mut all = app.candidates.write().unwrap_or_else(|e| e.into_inner());
        let c = all.entry(interval).or_default();
        c.push(cand);
        let excess = c.len().saturating_sub(24);
        c.drain(..excess);
    }
    app.save_candidates();
    publish_walk_forward(app);
    Ok(())
}

/// Re-run every candidate over the candles of its interval that arrived after it was chosen.
async fn update_forward(app: &Arc<App>, frames: &Frames) -> Result<()> {
    let all: BTreeMap<Interval, Vec<Candidate>> = app
        .candidates
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let equity = frames.equity;
    for (interval, cands) in all {
        if cands.is_empty() {
            continue;
        }
        let Some(f) = frames.get(interval).cloned() else {
            continue;
        };
        let results = tokio::task::spawn_blocking(move || {
            let sc = scores::compute(&f.market, walkforward::UNIVERSE);
            let n = f.market.ts.len();
            cands
                .into_iter()
                .map(|c| {
                    let idx = f.market.ts.partition_point(|&t| t <= c.chosen_bar_ts);
                    let fwd = (idx + 1 < n).then(|| {
                        walkforward::evaluate_window(
                            "forward",
                            &f.market,
                            &sc,
                            equity,
                            &c.params,
                            idx..n,
                        )
                    });
                    (c.params, c.chosen_bar_ts, fwd)
                })
                .collect::<Vec<_>>()
        })
        .await?;
        let mut live = app.candidates.write().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = live.get_mut(&interval) {
            for (params, ts, fwd) in results {
                if let Some(c) = v
                    .iter_mut()
                    .find(|c| c.params == params && c.chosen_bar_ts == ts)
                {
                    c.forward = fwd;
                }
            }
        }
    }
    app.save_candidates();
    publish_walk_forward(app);
    Ok(())
}

/// Saves a search's top settings in the parameter record and re-tests the tracked ones on the newest data.
fn record_and_test(
    app: &App,
    f: &Frame,
    equity: f64,
    report: &SearchReport,
    chosen: Option<&XsParams>,
) {
    let now = chrono::Utc::now().timestamp_millis();
    let top: Vec<(XsParams, f64)> = report
        .top
        .iter()
        .map(|r| (r.params.clone(), r.robust_score))
        .collect();
    let Some(slot) = app.records.get(&f.interval) else {
        return;
    };
    let guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    let Some(rec) = guard.as_ref() else { return };
    if let Err(e) = rec.save_top(&top, chosen, now) {
        warn!("parameter record save failed: {e:#}");
        return;
    }
    let tracked = rec.load_params(RECORD_TRACKED).unwrap_or_default();
    let latest_sc = scores::compute(&f.market, walkforward::UNIVERSE);
    let label = format!("latest {} sessions", f.interval.test_sessions());
    for p in &tracked {
        let r = walkforward::evaluate_window(&label, &f.market, &latest_sc, equity, p, f.range());
        let _ = rec.add_eval(
            p,
            now,
            &label,
            r.return_pct,
            r.max_drawdown_pct,
            r.trades,
            r.liquidations,
        );
    }
    let view = RecordView {
        count: rec.count().unwrap_or(0),
        tested_per_search: RECORD_CANDIDATES,
        rows: rec.summary(10).unwrap_or_default(),
    };
    drop(guard);
    if f.interval == app.paper_interval() {
        app.status.write().unwrap_or_else(|e| e.into_inner()).record = Some(view);
    }
}

// A fresh random seed per search (wall-clock nanoseconds; not security-sensitive).
fn search_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
}

// NOTE(agents): User request (2026-10-06): "paper chooses the winner". After each search round the
//               interval whose chosen (safe) settings returned most over the span EVERY interval
//               tests (the last 14 sessions) becomes the paper interval; a tie keeps the current
//               one. In-sample like every choice the search makes; the walk-forward and stability
//               tests per interval are reported beside it.
/// The interval the paper should trade after a search round, with the comparison text.
fn pick_interval(
    searches: &BTreeMap<Interval, Search>,
    current: Option<Interval>,
) -> (Option<Interval>, String) {
    pick_among(&Interval::LIVE, searches, current)
}

/// A fresh search must still endorse the exact settings that would place a buy.
/// This also blocks stale pending orders during a restart or interval switch.
fn paper_entry_eligible(app: &App, interval: Interval, params: &XsParams) -> bool {
    if *app.paper_choice.read().unwrap_or_else(|e| e.into_inner()) != Some(interval) {
        return false;
    }
    app.searches
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&interval)
        .is_some_and(|s| {
            s.chosen.as_ref() == Some(params)
                && s.common_return_pct
                    .is_some_and(|r| r.is_finite() && r > 0.0)
        })
}

/// Flat accounts may discard a stale pending buy when its interval is disqualified
/// or the market is closed and a better winner has been selected.
fn can_handoff_interval(
    flat: bool,
    planned: bool,
    order_pending: bool,
    disqualified: bool,
    between_sessions: bool,
) -> bool {
    flat && (disqualified || between_sessions || (!planned && !order_pending))
}

/// The best winner among `choices` (the live service only has 3h; the tests use every interval).
fn pick_among(
    choices: &[Interval],
    searches: &BTreeMap<Interval, Search>,
    current: Option<Interval>,
) -> (Option<Interval>, String) {
    let mut best: Option<(Interval, f64)> = None;
    let mut parts = Vec::new();
    for &i in choices {
        let r = searches
            .get(&i)
            .filter(|x| x.chosen.is_some())
            .and_then(|x| x.common_return_pct)
            .filter(|r| r.is_finite() && *r > 0.0);
        parts.push(format!(
            "{} {}",
            i.label(),
            r.map_or("no profitable safe settings".to_string(), |r| format!(
                "{r:+.2}%"
            ))
        ));
        if let Some(r) = r {
            let better = match best {
                None => true,
                Some((b, br)) => r > br || (r == br && Some(i) == current && Some(b) != current),
            };
            if better {
                best = Some((i, r));
            }
        }
    }
    let note = format!(
        "winners over the last {TEST_SESSIONS} sessions: {}",
        parts.join(", ")
    );
    (best.map(|b| b.0), note)
}

/// The paper interval may change only between sessions (user rule 2026-10-08: as often as needed,
/// never while the market is open). During a session the held interval stays and the comparison
/// is still reported.
fn hold_interval(
    in_session: bool,
    held: Option<Interval>,
    held_eligible: bool,
    pick: Option<Interval>,
    note: String,
) -> (Option<Interval>, String) {
    match held {
        Some(h) if in_session && held_eligible && pick != Some(h) => (
            Some(h),
            format!(
                "{} kept (the market is open; the interval changes only between sessions); {note}",
                h.label()
            ),
        ),
        _ => (pick, note),
    }
}

/// The parameter search: per interval, `walkforward::LIVE_SAMPLES` random combinations
/// backtested over its full test window, then champion/challenger against that interval's
/// settings in use; then the paper interval is picked. The next round starts `SEARCH_PAUSE`
/// after this one finished.
async fn search_task(app: Arc<App>, hb: Heartbeat) -> Result<()> {
    loop {
        hb.beat();
        let latest = app.frames.read().unwrap_or_else(|e| e.into_inner()).clone();
        // Nothing to search until the universe has measured moomoo rules.
        let Some(frames) = latest.filter(|x| {
            x.get(Interval::M1).is_some_and(|f| {
                (0..f.market.symbols.len())
                    .any(|s| f.market.entry_eligible[s] && f.market.rules[s].is_some())
            })
        }) else {
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        };
        let equity = frames.equity;
        let mut failures = 0;
        let live: Vec<Frame> = frames
            .list
            .iter()
            .filter(|f| Interval::LIVE.contains(&f.interval))
            .cloned()
            .collect();
        for (interval_index, f) in live.clone().into_iter().enumerate() {
            hb.beat();
            let interval = f.interval;
            // The champion: the settings this interval's latest search chose; after a restart, the
            // most recent best settings from its persistent record (user rule: keep the recent best).
            let (recorded, record_champion) = {
                let guard = app
                    .records
                    .get(&interval)
                    .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()));
                match guard.as_ref().and_then(|g| g.as_ref()) {
                    Some(r) => (
                        r.load_params(RECORD_CANDIDATES).unwrap_or_default(),
                        r.last_chosen().unwrap_or(None),
                    ),
                    None => (Vec::new(), None),
                }
            };
            let champion = app
                .searches
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&interval)
                .and_then(|x| x.chosen.clone())
                .or(record_champion);
            let combos = walkforward::LIVE_SAMPLES;
            // the promoted live options (if any): settings recorded before a promotion lack the new gate and are not re-tested,
            // and an old champion that lacks it cannot be kept (it would trade the previous strategy)
            let space = app.live_space(interval);
            let recorded: Vec<XsParams> = recorded
                .into_iter()
                .filter(|p| live_options::params_fit(p, &space))
                .collect();
            let champion = champion.filter(|p| live_options::params_fit(p, &space));
            {
                let mut st = app.status.write().unwrap_or_else(|e| e.into_inner());
                st.search_progress = SearchProgress {
                    running: true,
                    interval: Some(interval),
                    interval_index: interval_index + 1,
                    interval_total: live.len(),
                    combinations_target: walkforward::LIVE_SAMPLES,
                    combinations_evaluated: 0,
                    completed_intervals: interval_index,
                    started_ms: now_ms(),
                };
            }
            let started = Instant::now();
            let fs = f.clone();
            let common_start = frames.common_start;
            let result = tokio::task::spawn_blocking(move || -> Result<_> {
                let sc = scores::compute(&fs.market, walkforward::UNIVERSE);
                let report = search_frame(
                    &fs,
                    &sc,
                    equity,
                    Instant::now() + SEARCH_DEADLINE / 4,
                    search_seed(),
                    &recorded,
                    space,
                )?;
                let (chosen, choice) = walkforward::choose_live(
                    &fs.market,
                    &sc,
                    equity,
                    report.params.as_ref(),
                    champion.as_ref(),
                    fs.range(),
                );
                let winner = chosen.as_ref().map(|p| winner_curve(&fs, &sc, p, equity));
                let common = winner.as_ref().and_then(|w| span_return(w, common_start));
                Ok((report, chosen, choice, winner, common))
            })
            .await?;
            hb.beat();
            match result {
                Ok((report, chosen, choice, winner, common)) => {
                    {
                        let mut st = app.status.write().unwrap_or_else(|e| e.into_inner());
                        st.search_progress.combinations_evaluated = report.evaluated;
                        st.search_progress.completed_intervals = interval_index + 1;
                    }
                    app.event(
                        "INFO",
                        format!(
                            "SEARCH {} ({} sessions): {combos} random combinations, {} evaluations, {} usable in {:.0}s: {:?}{}",
                            interval.label(),
                            interval.test_sessions(),
                            report.evaluated,
                            report.usable,
                            started.elapsed().as_secs_f64(),
                            choice,
                            common.map_or(String::new(), |r| format!("; winner {r:+.2}% over the last {TEST_SESSIONS} sessions"))
                        ),
                    );
                    let finished_ms = chrono::Utc::now().timestamp_millis();
                    if interval == app.paper_interval() {
                        // Display only: the bar loop applies settings at the next close.
                        let mut st = app.status.write().unwrap_or_else(|e| e.into_inner());
                        st.report = Some(report.clone());
                        st.params = chosen.clone();
                        st.choice = Some(choice);
                        st.search_finished_ms = finished_ms;
                        st.mode = format!("{}: {}", interval.label(), mode_text(Some(choice)));
                    }
                    let window = (f.market.wall[f.test_start], f.closes[f.closes.len() - 1]);
                    app.searches
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(
                            interval,
                            Search {
                                common_return_pct: common,
                                window,
                                report: report.clone(),
                                chosen: chosen.clone(),
                                choice,
                                finished_ms,
                                winner,
                            },
                        );
                    app.save_console_state();
                    let json = serde_json::json!({
                        "interval": interval, "choice": format!("{choice:?}"), "chosen": chosen,
                        "usable": report.usable, "evaluated": report.evaluated,
                        "robust_score": report.robust_score, "raw_best_score": report.raw_best_score,
                        "seed": report.seed, "from_record": report.from_record, "common_return_pct": common,
                        "metrics": report.metrics, "top": report.top,
                    })
                    .to_string();
                    if let Some(st) = &app.store {
                        if let Err(e) = st.search(app.run_id, finished_ms, &json) {
                            warn!("store search write failed: {e:#}");
                        }
                    }
                    {
                        // Keep this search's best settings on record, then re-test every recorded
                        // setting on the latest window (detached).
                        let (a, fr, rp, ch) =
                            (app.clone(), f.clone(), report.clone(), chosen.clone());
                        tokio::task::spawn_blocking(move || {
                            record_and_test(&a, &fr, equity, &rp, ch.as_ref())
                        });
                    }
                    if let Err(e) =
                        register_candidate(&app, &report, chosen, f.clone(), equity).await
                    {
                        warn!(
                            "forward track of the chosen {} settings failed: {e:#}",
                            interval.label()
                        );
                    }
                }
                Err(e) => {
                    let mut st = app.status.write().unwrap_or_else(|e| e.into_inner());
                    st.search_progress.completed_intervals = interval_index + 1;
                    failures += 1;
                    app.event(
                        "WARN",
                        format!(
                            "SEARCH {} failed ({e:#}); its previous result stays in use",
                            interval.label()
                        ),
                    );
                }
            }
        }
        {
            let mut st = app.status.write().unwrap_or_else(|e| e.into_inner());
            st.search_progress.running = false;
            st.search_progress.completed_intervals = live.len();
        }
        // Pick the paper interval from this round.
        let current = app
            .paper
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|ps| ps.interval);
        let (pick, note) = pick_interval(
            &app.searches.read().unwrap_or_else(|e| e.into_inner()),
            current,
        );
        // NOTE(agents): Re-picking mid-session flipped the paper interval twice on 2026-10-07
        //               (4h +7.09% vs one session +7.07%), and every switch discards the planned
        //               orders, so the open's entries were lost. User rule (2026-10-08): switch as
        //               often as needed, but only between sessions (never while the market is open).
        let cal = app.sessions().await.ok();
        let now = now_ms();
        let in_session = cal.as_ref().is_some_and(|s| s.open_session(now).is_some());
        let held = current.or(*app.paper_choice.read().unwrap_or_else(|e| e.into_inner()));
        let held_eligible = held.is_some_and(|i| {
            app.searches
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&i)
                .is_some_and(|s| {
                    s.chosen.is_some()
                        && s.common_return_pct
                            .is_some_and(|r| r.is_finite() && r > 0.0)
                })
        });
        let (pick, note) = hold_interval(in_session, held, held_eligible, pick, note);
        *app.paper_choice.write().unwrap_or_else(|e| e.into_inner()) = pick;
        app.save_console_state();
        // Search rounds often finish outside market hours, when the bar loop will
        // not run again until after the next open. Move a flat account now so its
        // first decision uses the selected profitable interval, not the old one.
        if !in_session {
            if let (Some(from), Some(to), Some(sessions), Some(m15)) =
                (current, pick, cal.as_ref(), frames.get(Interval::M1))
            {
                let flat = app
                    .paper
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .is_some_and(|ps| ps.portfolio.positions.is_empty());
                if from != to && flat {
                    match switch_interval(&app, sessions, &m15.market, from, to, &frames) {
                        Ok(line) => {
                            *app.open_ctx.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            app.event("INFO", line);
                            if let Some(search) = app
                                .searches
                                .read()
                                .unwrap_or_else(|e| e.into_inner())
                                .get(&to)
                            {
                                let mut st = app.status.write().unwrap_or_else(|e| e.into_inner());
                                st.paper_interval = Some(to);
                                st.params = search.chosen.clone();
                                st.choice = Some(search.choice);
                                st.report = Some(search.report.clone());
                                st.search_finished_ms = search.finished_ms;
                                st.effective_pairs = 0;
                                st.allocation_note =
                                    "awaiting first candle on the selected interval".into();
                                st.mode = format!(
                                    "{}: AWAITING FIRST CANDLE (profitable safe settings selected)",
                                    to.label()
                                );
                            }
                        }
                        Err(e) => {
                            app.event("WARN", format!("PAPER interval handoff failed: {e:#}"))
                        }
                    }
                }
            }
        }
        app.event(
            "INFO",
            format!(
                "PAPER interval: {} ({note})",
                pick.map_or(
                    "none (no interval has profitable safe settings: no new entries)".to_string(),
                    |i| i.label().to_string()
                )
            ),
        );
        app.status
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .interval_note = note;
        let finished = Instant::now();
        let pause = if failures == live.len() {
            SEARCH_RETRY
        } else {
            SEARCH_PAUSE
        };
        let until = finished + pause;
        while Instant::now() < until {
            hb.beat();
            tokio::time::sleep(
                Duration::from_secs(30).min(until.saturating_duration_since(Instant::now())),
            )
            .await;
        }
    }
}

/// Serves the read-only console (HTML page and JSON API) on 127.0.0.1.
async fn console(app: Arc<App>, hb: Heartbeat) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await?;
    info!("console on http://127.0.0.1:{PORT}");
    loop {
        hb.beat();
        let Ok(Ok((mut sock, _))) =
            tokio::time::timeout(Duration::from_secs(20), listener.accept()).await
        else {
            continue;
        };
        let app = app.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(5), sock.read(&mut buf)).await
            else {
                return;
            };
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            // `?i=<1m|1h|4h|1d>` selects an interval; default: the one the paper trades.
            let interval = path
                .split_once("?i=")
                .and_then(|(_, k)| Interval::from_key(k.split('&').next().unwrap_or("")))
                .unwrap_or_else(|| app.paper_interval());
            let (ctype, body) = if path.starts_with("/api/status") {
                let mut v =
                    serde_json::to_value(&*app.status.read().unwrap_or_else(|e| e.into_inner()))
                        .unwrap_or_default();
                v["cpu_pct"] = governor::global().cpu_pct().into();
                v["live_strategy"] = serde_json::to_value(live_options::LIVE).unwrap_or_default();
                v["data_note"] = "spread from moomoo snapshot quotes (bid/ask and sizes); fees from this account's real paper orders".into();
                v["start_capital"] = app
                    .paper
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map_or(START_CAPITAL, |ps| ps.portfolio.metrics().start_equity)
                    .into();
                v["tasks"] = serde_json::to_value(app.health.snapshot()).unwrap_or_default();
                v["now_ms"] = chrono::Utc::now().timestamp_millis().into();
                if let Some(ps) = app.paper.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    v["paper_positions"] =
                        serde_json::to_value(&ps.portfolio.positions).unwrap_or_default();
                    v["paper_recent_trades"] = serde_json::to_value(
                        ps.portfolio
                            .trades
                            .iter()
                            .rev()
                            .take(40)
                            .collect::<Vec<_>>(),
                    )
                    .unwrap_or_default();
                }
                let wall = app.wall_fn(app.paper_interval());
                v["paper_interval_label"] = app.paper_interval().label().into();
                for k in [
                    "paper_positions",
                    "paper_recent_trades",
                    "walk_forward",
                    "report",
                ] {
                    if let Some(x) = v.get_mut(k) {
                        walls(x, &wall);
                    }
                }
                ("application/json", v.to_string())
            } else if path.starts_with("/api/lab") {
                ("application/json", lab_summary().to_string())
            } else if path.starts_with("/api/research") {
                (
                    "application/json",
                    app.research
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&interval)
                        .map_or("null".into(), |v| v.to_string()),
                )
            } else if path.starts_with("/api/paper") {
                // The paper account's equity bar by bar over every run (the paper state carries over restarts; a chart of this
                // run alone was reset by every restart and rarely showed any history).
                let (points, trades) = match &app.store {
                    Some(st) => (
                        st.equity_curve_all().unwrap_or_default(),
                        st.trades_all().unwrap_or_default(),
                    ),
                    None => (Vec::new(), Vec::new()),
                };
                let start = app
                    .status
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .paper_start_equity;
                (
                    "application/json",
                    serde_json::json!({"start_equity": start, "points": points, "trades": trades})
                        .to_string(),
                )
            } else if path.starts_with("/api/orders") {
                // The real moomoo paper orders (newest first) and the engine holdings they implement.
                let wall = app.wall_fn(app.paper_interval());
                let v = app.paper.read().unwrap_or_else(|e| e.into_inner()).as_ref().map(|ps| {
                    let orders: Vec<serde_json::Value> = ps.ledger.orders.iter().rev().take(200).map(|o| {
                        let mut x = serde_json::to_value(o).unwrap_or_default();
                        x["due_wall"] = wall(o.due_ts).map_or(serde_json::Value::Null, |w| w.into());
                        x["final"] = o.is_final().into();
                        x
                    }).collect();
                    serde_json::json!({"orders": orders, "holdings": ps.ledger.holdings.iter().rev().take(100).collect::<Vec<_>>(), "planned_for": ps.ledger.planned_for.and_then(&wall)})
                });
                (
                    "application/json",
                    v.map_or("null".into(), |v| v.to_string()),
                )
            } else if path.starts_with("/api/walkforward") {
                let w = app
                    .walk_forward
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&interval)
                    .cloned();
                (
                    "application/json",
                    w.map_or("null".into(), |v| v.to_string()),
                )
            } else if path.starts_with("/api/winners") {
                // Every interval's latest search, its winner and which one the paper trades.
                let searches = app
                    .searches
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let restored = app
                    .restored
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let research = app
                    .research
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let wf = app
                    .walk_forward
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let rows: Vec<serde_json::Value> = Interval::LIVE
                    .iter()
                    .map(|i| {
                        // a fresh search, else the one saved on disk before the last restart
                        let sum = searches.get(i).map(search_summary).or_else(|| restored.get(i).cloned());
                        let x = sum.as_ref();
                        let w = x.map(|x| &x["winner"]).filter(|w| !w.is_null());
                        serde_json::json!({
                            "interval": i, "key": i.key(), "label": i.label(), "sessions": i.test_sessions(),
                            "candles": i.test_candles(), "warmup_candles": i.warmup_candles(),
                            "searched_ms": x.map(|x| x["searched_ms"].clone()), "window": x.map(|x| x["window"].clone()),
                            "evaluated": x.map(|x| x["evaluated"].clone()), "usable": x.map(|x| x["usable"].clone()),
                            "choice": x.map(|x| x["choice"].clone()), "params": x.map(|x| x["params"].clone()),
                            "restored": !searches.contains_key(i) && x.is_some(),
                            "return_pct": w.and_then(|w| w["return_pct"].as_f64()), "trades": w.and_then(|w| w["trades"].as_u64()), "wins": w.and_then(|w| w["wins"].as_u64()),
                            "max_drawdown_pct": w.and_then(|w| w["max_drawdown_pct"].as_f64()),
                            "common_return_pct": x.and_then(|x| x["common_return_pct"].as_f64()),
                            "walk_forward_pct": wf.get(i).and_then(|v| v["return_pct"].as_f64()),
                            "walk_forward_error": wf.get(i).and_then(|v| v["error"].as_str().map(String::from)),
                            "stability_positive": research.get(i).and_then(|v| v["runs_positive"].as_u64()),
                            "stability_found": research.get(i).and_then(|v| v["runs_with_settings"].as_u64()),
                        })
                    })
                    .collect();
                let paper = app
                    .paper
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|ps| ps.interval);
                let pick = *app.paper_choice.read().unwrap_or_else(|e| e.into_inner());
                let note = app
                    .status
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .interval_note
                    .clone();
                (
                    "application/json",
                    serde_json::json!({"rows": rows, "paper_interval": paper, "paper_choice": pick, "note": note,
                        "common_sessions": TEST_SESSIONS}).to_string(),
                )
            } else if path.starts_with("/api/winner") {
                let w = app
                    .searches
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&interval)
                    .and_then(|x| x.winner.clone())
                    .or_else(|| {
                        app.restored
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&interval)
                            .map(|x| x["winner"].clone())
                            .filter(|w| !w.is_null())
                    });
                (
                    "application/json",
                    w.map_or("null".into(), |v| v.to_string()),
                )
            } else if path.starts_with("/api/signals") {
                (
                    "application/json",
                    serde_json::to_string(&*app.signals.read().unwrap_or_else(|e| e.into_inner()))
                        .unwrap_or_default(),
                )
            } else {
                ("text/html; charset=utf-8", PAGE.to_string())
            };
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ =
                tokio::time::timeout(Duration::from_secs(5), sock.write_all(resp.as_bytes())).await;
        });
    }
}

/// The research lab's state for the console (read-only), from the lab dashboard's local snapshot (127.0.0.1:8790): the
/// "considerably better strategy" alert (decided by the lab's code), validated and candidate variants, the agents' reward
/// scoreboard, the Hermes agents' newest test requests, notes between agents and security events. `{"up": false}` when the lab
/// dashboard does not answer.
fn lab_summary() -> serde_json::Value {
    use std::io::{Read, Write};
    let fetch = || -> Result<serde_json::Value> {
        let addr: std::net::SocketAddr = "127.0.0.1:8790".parse()?;
        let mut s = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(2))?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        s.write_all(b"GET /api/snapshot HTTP/1.0\r\nHost: 127.0.0.1:8790\r\n\r\n")?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw)?;
        let text = String::from_utf8_lossy(&raw);
        anyhow::ensure!(
            text.starts_with("HTTP/1.1 200") || text.starts_with("HTTP/1.0 200"),
            "lab dashboard answered {}",
            text.lines().next().unwrap_or("")
        );
        let body = text
            .split_once("\r\n\r\n")
            .map(|x| x.1)
            .context("no HTTP body")?;
        Ok(serde_json::from_str(body)?)
    };
    let Ok(d) = fetch() else {
        return serde_json::json!({"up": false});
    };
    let i = &d["inputs"];
    let take = |v: &serde_json::Value, n: usize| {
        serde_json::Value::Array(
            v.as_array()
                .map(|a| a.iter().take(n).cloned().collect())
                .unwrap_or_default(),
        )
    };
    let winners: Vec<serde_json::Value> = i["leaderboard"].as_array().map(|a| a.iter().filter(|l| matches!(l["status"].as_str(), Some("validated" | "candidate")))
        .map(|l| serde_json::json!({"status": l["status"], "interval": l["interval"], "tokens": l["tokens"], "holdout_a": l["a"]["h2"], "base_a": l["baseline_a"]["h2"],
            "holdout_b": l["b"]["h2"], "base_b": l["baseline_b"]["h2"]})).collect()).unwrap_or_default();
    // flash only what passed the FULL validation (every block offset): the lab's promotion status decides, not cuts A and B alone
    let qualifying: Vec<String> = i["promotion_status"]["qualifying"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let better: Vec<serde_json::Value> = i["better"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|b| {
                    qualifying.contains(&format!(
                        "{}|{}",
                        b["interval"].as_str().unwrap_or(""),
                        b["tokens"].as_str().unwrap_or("")
                    ))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut pending: Vec<serde_json::Value> =
        i["better_pending"].as_array().cloned().unwrap_or_default();
    pending.extend(
        i["promotion_status"]["not"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    serde_json::json!({"up": true, "level": d["status"]["level"], "alerts": d["alerts"], "better": better, "better_pending": pending,
        "winners": winners, "scoreboard": i["scoreboard"], "hermes": take(&i["hermes_queue"], 6), "messages": take(&i["agent_messages"], 6),
        "security": take(&i["security"], 5), "quarantined": i["quarantined"],
        "agents": i["agent_results"]["summary"], "agent_rows": take(&i["agent_results"]["rows"], 10), "capital": i["agent_results"]["capital"]})
}

const PAGE: &str = r#"<!doctype html><title>SPY ETF Universe Holdings Trading Bot — live console</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>body{font:14px -apple-system,system-ui,sans-serif;margin:16px;background:#fafafa;color:#222}
h1{font-size:18px;margin:0 0 6px}.flash{padding:12px;margin:10px 0;border:2px solid #0a7d2c;border-radius:8px;font-weight:600;animation:fl 1.1s steps(2,jump-none) infinite}@keyframes fl{0%{background:#e4f5ec}100%{background:#fff;border-color:#1d5fd1}}@media (prefers-reduced-motion:reduce){.flash{animation-duration:3s}}section{background:#fff;border:1px solid #ddd;border-radius:8px;padding:12px;margin:10px 0;overflow-x:auto}
.k{color:#666}.ok{color:#0a7d2c}.bad{color:#b00020}.warn{color:#b26a00}.Long{color:#0a7d2c}.Short{color:#b00020}pre{white-space:pre-wrap;font-size:12px;margin:0;max-height:420px;overflow:auto}
table{border-collapse:collapse;font-size:12px}td,th{padding:2px 8px;text-align:right}th{color:#666;font-weight:500}td:first-child,th:first-child{text-align:left}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:8px}.ivgrid{display:grid;grid-template-columns:repeat(auto-fit,minmax(420px,1fr));gap:10px;margin-top:8px}.ivgrid>div{border:1px solid #eee;border-radius:6px;padding:6px}.ivgrid .paper{border:2px solid #2463eb}.tabs button{font:12px system-ui;margin-right:4px;padding:2px 8px;border:1px solid #ccc;border-radius:4px;background:#fff;cursor:pointer}.tabs button.on{background:#2463eb;color:#fff;border-color:#2463eb}.card{background:#f4f4f4;border-radius:6px;padding:8px}.card b{display:block;font-size:18px}</style>
<h1>SPY ETF Universe Holdings Trading Bot — live console <span class=k>(SPY's holdings on moomoo · REAL moomoo paper orders, US SIMULATE account · trades a 5,000 USD account · all live intervals · 1 position)</span></h1>
<div id=labalert></div>
<section><b>Search progress</b><div id=searchProgress class=k>waiting for search…</div></section>
<section><b>24/7 learning overview</b> <span class=k>(all values come from real moomoo candle research; bars are comparative diagnostics, not trade orders)</span><div id=learnSummary class=grid></div><div id=learnChart></div><div id=learnEvents class=k></div></section>
<div class=k id=strat></div>
<section><b>Research lab: local models</b> <span class=k>(the Rust brain and neural trainer test stored real candles; code judges strategy variants on unseen 14-session blocks; full lab dashboard: <a href="http://127.0.0.1:8790" target=_blank>127.0.0.1:8790</a>)</span><div id=lab class=k>loading…</div></section>
<section><div class=grid id=cards></div></section>
<section><b>Interval winners</b> <span class=k>(all supported live intervals aggregated from real 1m bars: a 200k-combination search on the latest 14 sessions. Paper trades only a currently profitable, safe winner; in-sample, not a forecast)</span> <span class=k id=ivnote></span><table id=ivtab></table><div class=ivgrid id=ivcharts></div></section>
<section><b id=whead>Winning settings: backtest</b> <span class=k id=wsum></span><div id=wchart></div><table id=wtr></table></section>
<section><b>Paper account, live</b> <span class=k id=pcsum></span><div id=pchart></div></section>
<section><b>Signal and positions</b> <span class=k>(each symbol ranked by the signal in use, best first; bars are paper positions over time, green/red = closed win/loss, blue = open now; ▲ = bought at the next rebalance)</span><div id=pgantt></div></section>
<section><b>Paper positions</b> <span class=k id=nextreb></span><table id=pos></table></section>
<section><b>moomoo paper account positions</b> <span class=k>(what moomoo itself reports for the account; compared with this bot's own fills every bar)</span><table id=ibpos></table><div class=k id=mism></div></section>
<section><b>Real moomoo paper orders</b> <span class=k>(each engine decision placed as a market order at the open of the paper interval's candle; fills and fees from moomoo replace the simulated ones) <span id=onote></span></span><table id=ord></table></section>
<section><b>Exclusions</b> <span class=k>(left out for missing or ineligible real data, with the reason; nothing is substituted)</span><table id=excl></table></section>
<section><b>Next rebalance targets</b> <span class=k id=sighelp></span><table id=sig></table></section>
<section><b>Live event log</b><pre id=ev></pre></section>
<section><b>Recent paper trades</b><table id=tr></table></section>
<section><b>Tests per interval</b> <span class=tabs id=rtabs></span> <span class=k>(the walk-forward and stability tests below run for every interval while the US market is closed; pick one)</span></section>
<section><b>Walk-forward test (inside the test window)</b> <span class=k id=wfsum></span><div id=wfchart></div><table id=wftab></table></section>
<section><b>Stability test, whole test window</b> <span class=k id=hnote></span> <span class=k>(runs while the US market is closed on the same latest-14-session window: independent 200k-combination searches with fresh seeds, plus random settings as a baseline. In-sample: it shows whether the search keeps finding the same kind of settings or just lucky noise, not a forecast)</span><div id=fsum class=k></div><table id=fdays></table></section>
Warning: truncated output (original token count: 13269)
Total output lines: 600

<section><b>Walk-forward of the chosen settings</b> <span class=k>(research only; decides eligibility for paper trading at a later stage. The forward track is the bars that arrived after the settings were chosen; the separate walk-forward test below re-chooses settings on earlier 14-session windows)</span><pre id=wfwd></pre></section>
<section><b>Recorded best settings</b> <span class=k>(kept across restarts; every recorded setting is re-tested in each search and on every window; ranked by mean return over all its tests)</span><table id=rec></table></section>
<section><b>Trades of the chosen settings</b> <span class=k>(backtest: which symbol, when, result; in-sample window and the forward track)</span><table id=trd></table></section>
<section><b>Parameter search (one latest-14-session window, 200k random combinations; recorded winners tested first; next search 5 min after the last one finished)</b><pre id=wf></pre></section>
<script>
// Number with `d` decimals ('-' when missing).
const f=(x,d=2)=>x==null||isNaN(x)?'-':Number(x).toFixed(d);
// Wall time (ms) as local date and time ('-' when missing).
const t=ms=>ms?new Date(ms).toLocaleString():'-';
const IM={M15:15,H1:60,H2:120,H3:180,H4:240,H5:300,D1:390},IL={M15:'1m',H1:'1h',H2:'2h',H3:'3h',H4:'4h',H5:'5h',D1:'1 session'},IK={M15:'1m',H1:'1h',H2:'2h',H3:'3h',H4:'4h',H5:'5h',D1:'1d'};
let CM=15,RI=null,PI='M15';
// A candle count on interval `iv` as hours (or sessions for one-session candles).
const dur=(c,iv)=>iv==='D1'?f(c,0)+' sessions':f(c*(IM[iv]||15)/60,1)+'h';
// SVG line chart of an equity curve `w` (points and start equity) with axes and trade markers.
function curve(w,H){var pts=w.points,st=w.start_equity,W=960,L=60,R=10,T=14,B=26;
const xs=pts.map(p=>p[0]),ys=pts.map(p=>p[1]).concat([st]),lo=Math.min(...ys),hi=Math.max(...ys);
const n=pts.length,X=i=>L+i/(n-1||1)*(W-L-R),Y=v=>T+(H-T-B)-(v-lo)/(hi-lo||1)*(H-T-B);
// Index of the first chart point at or after wall time `ms` (the last one if none).
const idx=ms=>{let i=xs.findIndex(x=>x>=ms);return i<0?n-1:i};
var g='<svg viewBox="0 0 '+W+' '+H+'" style="width:100%;max-width:'+W+'px;height:auto;font:11px system-ui">';
for(const v of [lo,(lo+hi)/2,hi])g+='<line x1='+L+' x2='+(W-R)+' y1='+Y(v)+' y2='+Y(v)+' stroke=#eee /><text x='+(L-4)+' y='+(Y(v)+4)+' text-anchor=end fill=#888>'+f(v,2)+'</text>';
g+='<line x1='+L+' x2='+(W-R)+' y1='+Y(st)+' y2='+Y(st)+' stroke=#bbb stroke-dasharray=4 />';
for(const x of w.trade_list){const a=idx(x.entry_ms),b=idx(x.exit_ms);g+='<rect x='+X(a)+' y='+T+' width='+Math.max(X(b)-X(a),2)+' height='+(H-T-B)+' fill='+(x.pnl>=0?'#0a7d2c':'#b00020')+' opacity=0.08><title>'+x.symbol+' '+(x.pnl>=0?'+':'')+f(x.pnl)+' USD ('+x.reason+')</title></rect>';}
g+='<polyline fill=none stroke='+(w.end_equity>=st?'#0a7d2c':'#b00020')+' stroke-width=2 points="'+pts.map((p,i)=>X(i)+','+Y(p[1])).join(' ')+'"/>';
let lastDay='',lx=-99;pts.forEach((p,i)=>{const d=new Date(p[0]).toLocaleDateString([], {month:'2-digit',day:'2-digit'});if(d!==lastDay&&i<n-1&&X(i)-lx>40){lastDay=d;lx=X(i);g+='<text x='+X(i)+' y='+(H-8)+' fill=#888>'+d+'</text>';}});
return g+'</svg>'}
// Loads the interval table (/api/winners) and draws one chart per interval.
async function intervals(){try{
const r=await (await fetch('/api/winners')).json(),P=x=>x==null?'-':(x>=0?'+':'')+f(x)+'%';
document.getElementById('ivnote').textContent=r.note?'— '+r.note:'';
document.getElementById('ivtab').innerHTML='<tr><th>Interval<th>Test window<th>Candles (+ warm-up)<th>Last search<th>Usable<th>Winner return (window)<th>Winner, last '+r.common_sessions+' sessions<th>Trades<th>Win rate<th>Max drawdown<th>Walk-forward (3 unseen days)<th>Stability: positive / found<th>Paper</tr>'+r.rows.map(x=>`<tr><td><b>${x.label}</b><td>${x.sessions} sessions<td>${x.candles}${x.warmup_candles?' (+'+x.warmup_candles+')':''}<td>${t(x.searched_ms)}<td>${x.usable??'-'}<td class=${(x.return_pct||0)>0?'ok':'bad'}>${x.params?P(x.return_pct):'<span class=k>no profitable safe settings</span>'}<td class=${(x.common_return_pct||0)>0?'ok':'bad'}>${x.params?P(x.common_return_pct):'-'}<td>${x.params?x.trades:'-'}<td class=${x.trades&&x.wins/x.trades>=0.6?'ok':''}>${x.params&&x.trades?f(x.wins/x.trades*100,0)+'%':'-'}<td>${x.params?f(x.max_drawdown_pct,1)+'%':'-'}<td>${x.walk_forward_error?'<span class=k title="'+x.walk_forward_error.replace(/"/g,"'")+'">failed</span>':P(x.walk_forward_pct)}<td>${x.stability_found==null?'-':x.stability_positive+' / '+x.stability_found}<td>${r.paper_interval===x.interval&&r.paper_choice===x.interval&&x.params&&x.return_pct>0&&x.common_return_pct>0?'<b class=ok>TRADING</b>':r.paper_choice===x.interval?'<b class=warn>switching</b>':r.paper_interval===x.interval?'<span class=warn>inactive</span>':''}</tr>`).join('');
const box=document.getElementById('ivcharts');
if(!box.children.length)box.innerHTML=r.rows.map(x=>`<div id=iv_${x.key}><b>${x.label}</b> <span class=k id=ivs_${x.key}></span><div id=ivc_${x.key} class=k>waiting for its first search…</div></div>`).join('');
for(const x of r.rows){const el=document.getElementById('iv_'+x.key);el.className=r.paper_interval===x.interval?'paper':'';
const w=await (await fetch('/api/winner?i='+x.key)).json();
if(!w){document.getElementById('ivc_'+x.key).textContent=x.searched_ms?'no settings pass the safety rules on its '+x.sessions+'-session window':'waiting for its first search…';document.getElementById('ivs_'+x.key).textContent='';continue}
const net=w.end_equity-w.start_equity;
document.getElementById('ivs_'+x.key).innerHTML=(r.paper_interval===x.interval?'<b style="color:#2463eb">paper trades this</b> · ':'')+'last '+w.sessions+' sessions: <b class='+(net>=0?'ok':'bad')+'>'+(net>=0?'+':'')+f(w.return_pct)+'%</b>, '+w.trades+' trades, '+(w.params?w.params.signal+' '+(w.params.flip?'follow':'contrarian')+', top '+w.params.top+', hold '+dur(w.params.hold,x.interval):'');
document.getElementById('ivc_'+x.key).innerHTML=curve(w,170);}
}catch(e){document.getElementById('ivtab').textContent='error: '+e}}
// Consolidated research telemetry: the priority walk-forward worker, broad search winners,
// and closed-market stability evidence are shown together so learning cannot be mistaken for
// live/paper performance.
async function learningOverview(s){try{
const r=await (await fetch('/api/winners')).json(), rows=r.rows||[], W=960,H=260,L=72,R=20,T=28,B=42;
const vals=rows.flatMap(x=>[x.return_pct,x.walk_forward_pct]).filter(x=>x!=null&&isFinite(x));
const lo=Math.min(-1,...vals),hi=Math.max(1,...vals), span=hi-lo||1, bw=Math.max(8,Math.min(26,(W-L-R)/(rows.length*4||1)));
const X=i=>L+i*((W-L-R)/(Math.max(rows.length,1))), Y=v=>T+(H-T-B)-(v-lo)/span*(H-T-B), z=v=>v==null?null:Number(v);
var g='<svg viewBox="0 0 '+W+' '+H+'" style="width:100%;max-width:'+W+'px;height:auto;font:11px system-ui"><text x='+L+' y=14 fill=#444>latest research return (%)</text>';
for(const v of [lo,0,hi])g+='<line x1='+L+' x2='+(W-R)+' y1='+Y(v)+' y2='+Y(v)+' stroke='+(v===0?'#999':'#eee')+' /><text x='+(L-6)+' y='+(Y(v)+4)+' text-anchor=end fill=#888>'+f(v,1)+'</text>';
rows.forEach((x,i)=>{const x0=X(i), a=z(x.return_pct), b=z(x.walk_forward_pct), c=z(x.common_return_pct);for(const [v,col,label] of [[a,'#2463eb','search'],[b,'#0a7d2c','walk-forward'],[c,'#b26a00','common']])if(v!=null){const y=Y(Math.max(0,v)), y0=Y(Math.min(0,v));g+='<rect x='+(x0+(label==='search'?-bw:label==='walk-forward'?0:bw))+' y='+Math.min(y,y0)+' width='+bw+' height='+Math.max(1,Math.abs(y-y0))+' fill='+col+'><title>'+x.label+' '+label+': '+f(v)+'%</title></rect>';}g+='<text x='+x0+' y='+(H-22)+' text-anchor=middle fill=#555>'+x.label+'</text>';});
g+='<text x='+(W-R)+' y=14 text-anchor=end fill=#2463eb>blue search</text><text x='+(W-R)+' y=26 text-anchor=end fill=#0a7d2c>green walk-forward</text><text x='+(W-R)+' y=38 text-anchor=end fill=#b26a00>amber common window</text></svg>';
document.getElementById('learnChart').innerHTML=g;
const sp=s.search_progress||{}, tasks=Object.entries(s.tasks||{}).map(([k,v])=>k+': '+(v.running?'running':'stopped')).join(' · ');
document.getElementById('learnSummary').innerHTML=[['Priority worker',tasks.includes('walk_forward_priority_task: running')?'RUNNING':'waiting for real frames'],['Last walk-forward',rows.filter(x=>x.walk_forward_pct!=null).length+' intervals scored'],['Search',sp.running?'searching '+(sp.interval||'-')+' '+(sp.combinations_evaluated||0).toLocaleString()+'/'+(sp.combinations_target||0).toLocaleString():'idle'],['Promotion','research-only until unseen multi-block/noise gates pass']].map(x=>`<div class=card><span class=k>${x[0]}</span><b>${x[1]}</b></div>`).join('');
document.getElementById('learnEvents').textContent=(s.events||[]).filter(x=>/WALK-FORWARD|TENSOR|STABILITY|SEARCH/.test(x)).slice(0,8).join('  ·  ')||'Waiting for the first real-data learning event.';
}catch(e){document.getElementById('learnChart').textContent='learning chart error: '+e}}
// Loads the current search winner (/api/winner) and draws its equity curve with its trades.
async function winnerChart(){try{
const w=await (await fetch('/api/winner')).json();
if(!w){document.getElementById('wchart').textContent='waiting for the first search…';return}
document.getElementById('whead').textContent='Winning settings of the paper interval ('+w.interval_label+'): '+w.sessions+'-session backtest';
var pts=w.points,st=w.start_equity,W=960,H=240,L=60,R=10,T=14,B=26;
const xs=pts.map(p=>p[0]),ys=pts.map(p=>p[1]).concat([st]),x0=xs[0],x1=xs[xs.length-1],lo=Math.min(...ys),hi=Math.max(...ys);
const n=pts.length,X=i=>L+i/(n-1||1)*(W-L-R),Y=v=>T+(H-T-B)-(v-lo)/(hi-lo||1)*(H-T-B);
// Index of the first chart point at or after wall time `ms` (the last one if none).
const idx=ms=>{let i=xs.findIndex(x=>x>=ms);return i<0?n-1:i};
var g='<svg viewBox="0 0 '+W+' '+H+'" style="width:100%;max-width:'+W+'px;height:auto;font:11px system-ui">';
for(const v of [lo,(lo+hi)/2,hi])g+='<line x1='+L+' x2='+(W-R)+' y1='+Y(v)+' y2='+Y(v)+' stroke=#eee /><text x='+(L-4)+' y='+(Y(v)+4)+' text-anchor=end fill=#888>'+f(v,2)+'</text>';
g+='<line x1='+L+' x2='+(W-R)+' y1='+Y(st)+' y2='+Y(st)+' stroke=#bbb stroke-dasharray=4 />';
for(const x of w.trade_list){const a=idx(x.entry_ms),b=idx(x.exit_ms);g+='<rect x='+X(a)+' y='+T+' width='+Math.max(X(b)-X(a),2)+' height='+(H-T-B)+' fill='+(x.pnl>=0?'#0a7d2c':'#b00020')+' opacity=0.08><title>'+x.symbol+' '+(x.pnl>=0?'+':'')+f(x.pnl)+' USD ('+x.reason+')</title></rect>';}
g+='<polyline fill=none stroke='+(w.end_equity>=st?'#0a7d2c':'#b00020')+' stroke-width=2 points="'+pts.map((p,i)=>X(i)+','+Y(p[1])).join(' ')+'"/>';
let lastDay='';pts.forEach((p,i)=>{const d=new Date(p[0]).toLocaleDateString([], {month:'2-digit',day:'2-digit'});if(d!==lastDay&&i<n-1){lastDay=d;g+='<text x='+X(i)+' y='+(H-8)+' fill=#888>'+d+'</text>';}});
document.getElementById('wchart').innerHTML=g+'</svg>';
const net=w.end_equity-st,p=w.params||{};
document.getElementById('wsum').innerHTML='(the settings the search chose, replayed candle by candle on the last '+w.sessions+' sessions of real bars ('+w.interval_label+' candles); in-sample, not a forecast; x axis = trading time) <b class='+(net>=0?'ok':'bad')+'>'+f(st)+' → '+f(w.end_equity)+' USD ('+(net>=0?'+':'')+f(net)+', '+(net>=0?'+':'')+f(w.return_pct)+'%)</b> · '+w.trades+' trades, '+w.wins+' wins · max drawdown '+f(w.max_drawdown_pct,2)+'% · '+p.signal+' '+(p.flip?'follow':'contrarian')+', top '+p.top;
document.getElementById('wtr').innerHTML='<tr><th>Symbol<th>Opened<th>Closed<th>Qty<th>Entry<th>Exit<th>P&L USD<th>Commission<th>Exit reason</tr>'+w.trade_list.map(x=>`<tr><td>${x.symbol}<td>${t(x.entry_ms)}<td>${t(x.exit_ms)}<td>${x.qty}<td>${f(x.entry,2)}<td>${f(x.exit,2)}<td class=${x.pnl>=0?'ok':'bad'}>${(x.pnl>=0?'+':'')+f(x.pnl)}<td>${f(x.fees)}<td>${x.reason}</tr>`).join('');
}catch(e){document.getElementById('wchart').textContent='chart error: '+e}}
// Loads the paper account (/api/paper) and draws its equity curve and trade timeline.
async function paperChart(s){try{
const r=await (await fetch('/api/paper')).json(), st=r.start_equity||s.paper_start_equity||0;
ganttChart(s,window.lastSig,r);
const pts=r.points.map(p=>[p[0],p[1]]);
if(s.paper_equity&&pts.length&&Date.now()>pts[pts.length-1][0])pts.push([Date.now(),s.paper_equity]);
if(pts.length<2){document.getElementById('pchart').textContent='no paper equity recorded yet (it starts at the first candle after the first search)';return}
const W=960,H=240,L=60,R=10,T=14,B=26,xs=pts.map(p=>p[0]),ys=pts.map(p=>p[1]).concat([st]);
const x0=xs[0],x1=xs[xs.length-1],lo=Math.min(...ys),hi=Math.max(...ys);
// Chart coordinates: X from time, Y from equity.
const X=v=>L+(v-x0)/(x1-x0||1)*(W-L-R),Y=v=>T+(H-T-B)-(v-lo)/(hi-lo||1)*(H-T-B);
var g='<svg viewBox="0 0 '+W+' '+H+'" style="width:100%;max-width:'+W+'px;height:auto;font:11px system-ui">';
for(const v of [lo,(lo+hi)/2,hi])g+='<line x1='+L+' x2='+(W-R)+' y1='+Y(v)+' y2='+Y(v)+' stroke=#eee /><text x='+(L-4)+' y='+(Y(v)+4)+' text-anchor=end fill=#888>'+f(v,2)+'</text>';
g+='<line x1='+L+' x2='+(W-R)+' y1='+Y(st)+' y2='+Y(st)+' stroke=#bbb stroke-dasharray=4 />';
g+='<polyline fill=none stroke=#2463eb stroke-width=2 points="'+pts.map(p=>X(p[0])+','+Y(p[1])).join(' ')+'"/>';
const last=pts[pts.length-1];
g+='<circle cx='+X(last[0])+' cy='+Y(last[1])+' r=3 fill=#2463eb />';
const n=6;for(let i=0;i<=n;i++){const ms=x0+(x1-x0)*i/n;g+='<text x='+X(ms)+' y='+(H-8)+' text-anchor=middle fill=#888>'+new Date(ms).toLocaleString([], {month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit'})+'</text>';}
document.getElementById('pchart').innerHTML=g+'</svg>';
const net=last[1]-st;
document.getElementById('pcsum').innerHTML='(paper equity per closed bar since this service started, plus the latest value; booked from real moomoo paper fills and fees) <b class='+(net>=0?'ok':'bad')+'>'+f(last[1])+' USD ('+(net>=0?'+':'')+f(net)+', '+(net>=0?'+':'')+f(st?net/st*100:0)+'%)</b>';
}catch(e){document.getElementById('pchart').textContent='chart error: '+e}}
// Draws the trade timeline (Gantt): one bar per trade per symbol from entry to exit, coloured by PnL.
async function ganttChart(s,g,r){try{
const rows=(g||[]).slice().sort((a,b)=>a.rank-b.rank), now=Date.now();
const trs=(r.trades||[]).map(x=>({sym:x[0],a:x[1],b:x[2],pnl:x[3],why:x[4],open:false}));
for(const p of (s.paper_positions||[]))trs.push({sym:p.symbol,a:p.entry_ts,b:now,pnl:(p.mark-p.entry)*p.qty-p.fees,why:'open',open:true});
const x0=Math.min(...(r.points||[]).map(p=>p[0]).concat(trs.map(x=>x.a)).concat([now-3600000])),x1=now;
const W=960,L=170,R=30,T=8,rh=18,H=T+rows.length*rh+24,X=v=>L+(v-x0)/(x1-x0||1)*(W-L-R);
let h='<svg viewBox="0 0 '+W+' '+H+'" style="width:100%;max-width:'+W+'px;height:auto;font:11px system-ui">';
rows.forEach((c,i)=>{const y=T+i*rh;
h+='<rect x='+L+' y='+y+' width='+(W-L-R)+' height='+(rh-2)+' fill='+(i%2?'#f4f4f4':'#fafafa')+' /><text x='+(L-6)+' y='+(y+12)+' text-anchor=end fill=#444>#'+c.rank+' '+c.symbol+' '+f(c.signal,2)+'</text>';
for(const x of trs.filter(x=>x.sym===c.symbol))h+='<rect x='+X(Math.max(x.a,x0))+' y='+(y+2)+' width='+Math.max(X(x.b)-X(Math.max(x.a,x0)),2)+' height='+(rh-6)+' fill='+(x.open?'#2463eb':x.pnl>=0?'#0a7d2c':'#b00020')+'><title>'+c.symbol+' '+t(x.a)+' to '+(x.open?'now':t(x.b))+': '+(x.pnl>=0?'+':'')+f(x.pnl)+' USD ('+x.why+')</title></rect>';
if(c.target&&!c.held)h+='<text x='+(W-R+4)+' y='+(y+12)+' fill=#2463eb>▲</text>';
else if(c.target)h+='<text x='+(W-R+4)+' y='+(y+12)+' fill=#2463eb>●</text>';});
const n=5;for(let i=0;i<=n;i++){const ms=x0+(x1-x0)*i/n;h+='<text x='+X(ms)+' y='+(H-6)+' text-anchor=middle fill=#888>'+new Date(ms).toLocaleString([], {month:'2-digit',day:'2-digit',hour:'2-digit',minute:'2-digit'})+'</text>';}
document.getElementById('pgantt').innerHTML=h+'</svg>';
}catch(e){document.getElementById('pgantt').textContent='chart error: '+e}}
// Loads the recent paper orders (/api/orders) into the orders table.
async function orders(){try{const r=await (await fetch('/api/orders')).json();if(!r){document.getElementById('ord').innerHTML='<tr><td class=k>no orders yet</tr>';return}
document.getElementById('ord').innerHTML='<tr><th>Placed<th>For open<th>Side<th>Symbol<th>Qty<th>Status<th>Filled<th>Avg price<th>Fee<th>moomoo id<th>Why</tr>'+r.orders.map(o=>{const st=o.state||{};return `<tr><td>${t(o.placed_ms)}<td>${t(o.due_wall)}<td>${o.side==1?'BUY':'SELL'}<td>${o.symbol}<td>${o.qty}<td>${o.error?'<span class=bad>refused</span>':(st.status??'sent')}<td>${st.fill_qty??'-'}<td>${f(st.fill_avg_price,4)}<td>${o.fee?f(o.fee.total):(o.fee_unavailable?'<span class=warn>excluded</span>':'-')}<td>${o.order_id??'-'}<td style="text-align:left">${o.error||o.reason}</tr>`}).join('');}catch(e){}}
// Refreshes the console from /api/status: account, positions, PnL, signals and events.
async function tick(){try{
const s=await (await fetch('/api/status')).json(); var g=await (await fetch('/api/signals')).json();
document.getElementById('strat').textContent=s.strategy+' — last update '+t(s.last_bar_ts+900000);
window.lastSig=g;
CM=IM[s.paper_interval]||15;PI=s.paper_interval||'M15';if(!RI){RI=IK[PI];tabs();research();walkForward();}paperChart(s);winnerChart();intervals();
learningOverview(s);
// Signed number, signed percent, and the paper start equity for percentages.
const sg=x=>(x>=0?'+':'')+f(x), pc=x=>sg(x)+'%'; var st=s.paper_start_equity||1;
// Open PnL of a position at its mark, after fees.
const pu=p=>(p.side==="Long"?1:-1)*(p.mark-p.entry)*p.qty-p.fees;
const A=s.account||{};
const cards=[['moomoo paper account (SIMULATE)',(A.id||'-')+' <span class=k>total assets</span> '+f(A.total_assets)+' USD <span class=k>(cash '+f(A.cash)+', buying power '+f(A.power)+', read '+t(A.ts)+'; the bot trades '+f(s.start_capital,0)+' USD of it)</span>'],['US market',s.market_open?'<span class=ok>OPEN</span> <span class=k>closes '+t(s.session_close_ms)+'</span>':'<span class=bad>CLOSED</span> <span class=k>opens '+t(s.next_open_ms)+'</span>'],['Paper equity',s.paper_start_equity?f(s.paper_equity)+' USD <span class=k>('+pc((s.paper_equity/st-1)*100)+' from '+f(st,2)+')</span>':'<span class=k>not started: starts with '+f(s.start_capital,0)+' USD at the first candle after the first search</span>'],['Realised P&L <span class=k title="closed trades + commission already paid">ⓘ</span>','<span class='+(s.paper_realized>=0?'ok':'bad')+'>'+sg(s.paper_realized)+' USD ('+pc(s.paper_realized/st*100)+')</span>'],['Open P&L <span class=k title="open positions marked at the last close">ⓘ</span>','<span class='+(s.paper_open>=0?'ok':'bad')+'>'+sg(s.paper_open)+' USD ('+pc(s.paper_open/st*100)+')</span>'],['Open positions',s.paper_open_positions],['Dynamic allocation',s.effective_pairs+' symbols <span class=k>'+s.allocation_note+'</span>'],
['Closed trades / wins (win rate)',s.paper_trades+' / '+s.paper_wins+(s.paper_trades?' <span class='+(s.paper_wins/s.paper_trades>=0.6?'ok':'bad')+'>('+f(s.paper_wins/s.paper_trades*100,0)+'%)</span>':'')],['Drawdown recovery',s.resume_not_before?'cooldown until '+t(s.resume_not_before)+'; exits must finish and entry gates must pass':'ready; 25% exit remains active'],['Max drawdown',f(s.paper_max_drawdown_pct,1)+'%'],
['Paper settings',s.report?'<span class='+((s.mode||'').includes('TRADING')?'ok':'bad')+'>'+s.mode+'</span>':'…'],['Paper interval','<b>'+(s.paper_interval_label||'-')+'</b> <span class=k>'+(s.interval_note||'chosen after the first search round')+'</span>'],
['Parameter search',s.search_finished_ms?'last finished '+t(s.search_finished_ms)+', next about '+t(s.search_finished_ms+3600000):'first search running'],['Last 1m bar',t(s.last_bar_ts+900000)],['Candidates',s.symbols+' <span class=k>(moomoo screener: NYSE/Nasdaq by average daily dollar volume, with measured rules; the engine screener trades from the top '+s.universe+' by dollar volume each bar)</span>'],['moomoo rules',s.rules_symbols+' symbols, measured '+t(s.rules_ts)+' <span class=k>'+(s.data_note||'')+'</span>'],['Commission',(s.commission_source||'').includes('excluded')?'<span class=warn>'+s.commission_source+'</span>':(s.commission_source||'-')],['Candle quota',s.quota?s.quota.used+' used, '+s.quota.remain+' left <span class=k>(distinct stocks per 7 days)</span>':'-'],['Calendar','<span class=k>'+(s.calendar_note||'-')+'</span>'],['Next orders',s.next_order_open_ms?t(s.next_order_open_ms):'-'],['Store (SQLite)',f((s.store_bytes||0)/1048576,1)+' MB, '+(s.store_candles||0)+' candles <span class=k>(oldest candles culled only at 10 GB)</span>'],['CPU',f(s.cpu_pct,0)+'%'],
['Tasks',Object.entries(s.tasks||{}).map(([k,v])=>k+(v.running?' ✓':' ✗')).join(' ')]];
document.getElementById('sighelp').textContent=(s.effective_pairs===0&&s.allocation_note?'NO TARGETS NOW: '+s.allocation_note+' ':'')+'(at the next rebalance: long the '+s.effective_pairs+(s.params?' '+(s.params.flip?'highest':'lowest')+' '+s.params.signal+' values':' symbols ranked first')+', among the top '+s.universe+' candidates by dollar volume; no entries below 5 USD free balance)';
document.getElementById('cards').innerHTML=cards.map(c=>`<div class=card><span class=k>${c[0]}</span><b>${c[1]}</b></div>`).join('');
const sp=s.search_progress||{};document.getElementById('searchProgress').textContent=sp.running?('RUNNING '+(sp.interval||'-')+' — interval '+sp.interval_index+'/'+sp.interval_total+' — '+(sp.combinations_evaluated||0).toLocaleString()+'/'+(sp.combinations_target||0).toLocaleString()+' combinations ('+f((sp.combinations_evaluated||0)/(sp.combinations_target||1)*100,1)+'%) — '+sp.completed_intervals+'/'+sp.interval_total+' intervals complete'):(s.search_finished_ms?'last search finished '+t(s.search_finished_ms):'waiting for first search');
document.getElementById('nextreb').textContent=s.next_rebalance_ts?'next rebalance at '+t(s.next_rebalance_ts)+(s.params?' (every '+dur(s.params.hold,s.paper_interval)+')':''):'';
document.getElementById('pos').innerHTML='<tr><th>Symbol<th>Side<th>Qty<th>Entry<th>Mark<th>P&L USD (after commission)<th>P&L %<th>Stop<th>Opened</tr>'+(s.paper_positions||[]).map(p=>{
 const u=pu(p),pl=u/p.margin*100;return `<tr><td>${p.symbol}<td class=${p.side}>${p.side}<td>${f(p.qty,4)}<td>${f(p.entry,4)}<td>${f(p.mark,4)}<td class=${u>=0?"ok":"bad"}>${sg(u)}<td class=${u>=0?"ok":"bad"}>${pc(pl)}<td>${f(p.stop,4)}<td>${t(p.entry_ts)}</tr>`}).join('');
document.getElementById('ibpos').innerHTML='<tr><th>Symbol<th>Quantity<th>Sellable<th>Cost price<th>Price<th>Bot fills</tr>'+((s.broker_positions||[]).length?(s.broker_positions||[]).map(p=>`<tr><td>${p.symbol}<td>${p.qty}<td>${p.can_sell_qty}<td>${f(p.cost_price,4)}<td>${f(p.price,4)}<td>${(s.bot_shares||{})[p.symbol]??'-'}</tr>`).join(''):'<tr><td colspan=6 class=k>none</tr>');
document.getElementById('mism').textContent=(s.position_mismatches||[]).length?'mismatches: '+s.position_mismatches.join('; '):'no mismatch between moomoo positions and the bot\'s fills';
document.getElementById('excl').innerHTML='<tr><th>Symbol<th>Reason</tr>'+Object.entries(s.exclusions||{}).map(([k,v])=>`<tr><td>${k}<td style="text-align:left">${v}</tr>`).join('');
document.getElementById('onote').textContent=s.last_orders_note||'';orders();
const tg=g.filter(r=>r.target||r.held);
document.getElementById('sig').innerHTML='<tr><th>Symbol<th>Target<th>Held<th>Signal value (sign flipped when buying the highest)<th>Rank (1 = bought first)<th>Close<th>Dollar volume (1 trading day)</tr>'+
tg.map(r=>`<tr><td>${r.symbol}<td class=${r.target||''}>${r.target||'-'}<td class=${r.held||''}>${r.held||'-'}<td>${f(r.signal)}<td>${r.rank}<td>${r.close}<td>${f(r.turnover_24h/1e6,1)}M</tr>`).join('');
document.getElementById('rec').innerHTML=s.record?('<tr><th>#<th>Signal<th>Symbols<th>Hold<th>Tests<th>Mean return<th>Positive<th>Worst<th>In a top-20<th>Chosen<th>Last seen</tr>'+s.record.rows.map((r,i)=>`<tr><td>${i+1}<td>${r.params.signal}${r.params.flip?' (follow)':' (contrarian)'}<td>${r.params.top}<td>${r.params.hold}<td>${r.evals}<td class=${r.mean_return_pct>=0?'ok':'bad'}>${pc(r.mean_return_pct)}<td>${f(r.positive_share_pct,0)}%<td>${pc(r.worst_return_pct)}<td>${r.times_in_top}<td>${r.times_chosen}<td>${t(r.last_seen_ms)}</tr>`).join('')):'';
walkForward();
}catch(e){}}
async function walkForward(){try{
const w=await (await fetch('/api/walkforward?i='+(RI||''))).json(), chart=document.getElementById('wfchart'), table=document.getElementById('wftab');
if(!w||w.error){document.getElementById('wfsum').textContent=w&&w.error||'not computed yet';chart.replaceChildren();table.replaceChildren();return}
const F=(x,d=2)=>x==null?'-':Number(x).toFixed(d),S=x=>x==null?'-':(x>=0?'+':'')+F(x),D=ms=>new Date(ms).toLocaleDateString([], {month:'2-digit',day:'2-digit'});
const pts=w.points||[], start=w.start_equity, W=960,H=200,L=60,R=10,T=14,B=26, n=pts.length;
if(n){const ys=pts.map(p=>p[1]).concat([start]),lo=Math.min(...ys),hi=Math.max(...ys),X=i=>L+i/(n-1||1)*(W-L-R),Y=v=>T+(H-T-B)-(v-lo)/(hi-lo||1)*(H-T-B);
let svg='<svg viewBox="0 0 '+W+' '+H+'" style="width:100%;max-width:'+W+'px;height:auto;font:11px system-ui">';
for(const v of [lo,(lo+hi)/2,hi])svg+='<line x1='+L+' x2='+(W-R)+' y1='+Y(v)+' y2='+Y(v)+' stroke=#eee /><text x='+(L-4)+' y='+(Y(v)+4)+' text-anchor=end fill=#888>'+F(v)+'</text>';
svg+='<line x1='+L+' x2='+(W-R)+' y1='+Y(start)+' y2='+Y(start)+' stroke=#bbb stroke-dasharray=4 />';
svg+='<polyline fill=none stroke='+(w.end_equity>=start?'#0a7d2c':'#b00020')+' stroke-width=2 points="'+pts.map((p,i)=>X(i)+','+Y(p[1])).join(' ')+'"/>';
let lastDay='';pts.forEach((p,i)=>{const day=D(p[0]);if(day!==lastDay){lastDay=day;svg+='<text x='+X(i)+' y='+(H-8)+' fill=#888>'+day+'</text>';}});
chart.innerHTML=svg+'</svg>';}else chart.textContent='no walk-forward equity points yet';
document.getElementById('wfsum').innerHTML='(inside the last '+w.sessions+' sessions of '+w.interval_label+' candles; settings re-chosen before each unseen day; '+w.warmup_bars+' warm-up bars) <b class='+(w.end_equity>=start?'ok':'bad')+'>'+F(start)+' → '+F(w.end_equity)+' USD ('+S(w.return_pct)+'%)</b> vs '+w.reference+' '+S(w.reference_bh_pct)+'% · '+w.trades+' trades, '+w.wins+' wins · max drawdown '+F(w.max_drawdown_pct)+'% · computed '+new Date(w.computed_ms).toLocaleString();
table.innerHTML='<tr><th>Unseen day<th>Settings chosen on<th>Settings<th>Usable<th>Day result USD<th>'+w.reference+' that day<th>Trades closed</tr>'+w.segments.map(x=>{const p=x.params,r=x.end_equity-x.start_equity;return '<tr><td>'+D(x.traded[0])+'<td>'+D(x.chosen_on[0])+'–'+D(x.chosen_on[1])+'<td style="text-align:left">'+(p?p.signal+' '+(p.flip?'follow':'contrarian')+', top '+p.top+(x.new_settings?'':' (kept: none passed)'):'none yet: flat')+'<td>'+x.usable+'<td class='+(r>=0?'ok':'bad')+'>'+S(r)+'<td>'+S(x.reference_bh_pct)+'%<td>'+x.trades_closed+'</tr>'}).join('');
}catch(e){document.getElementById('wfsum').textContent='chart error: '+e}}
// Loads the research results (/api/research) for the selected interval tab.
async function research(){try{
var st=await (await fetch('/api/status')).json();
document.getElementById('hnote').textContent=st.history_note?'— '+st.history_note:'';
const r=await (await fetch('/api/research?i='+(RI||''))).json();
document.getElementById('fdays').innerHTML='';
if(!r){document.getElementById('fsum').textContent='not computed yet: runs while the US market is closed';return}
if(r.error){document.getElementById('fsum').textContent='unavailable: '+r.error;return}
// Number, signed number and percent formatters for the research table.
const F=(x,d=2)=>x==null?'-':Number(x).toFixed(d),S=x=>x==null?'-':(x>=0?'+':'')+F(x),P=x=>S(x)+'%',b=r.baseline||{};
document.getElementById('fsum').innerHTML=`window ${new Date(r.window[0]).toLocaleString()} → ${new Date(r.window[1]).toLocaleString()} · start ${F(r.start_equity)} USD<br><b>${r.runs_with_settings} of ${r.runs.length} searches found settings that pass the safety rules; ${r.runs_positive} of them are profitable on the window</b><br>most often chosen: ${(r.families||[]).map(f=>f[0]+' ×'+f[1]).join(', ')||'-'}<br>random settings (${b.samples}): median ${P(b.median_pct)}, 10th–90th percentile ${P(b.p10_pct)} to ${P(b.p90_pct)}, ${F(b.positive_share,0)}% positive · computed ${new Date(r.computed_ms).toLocaleString()} in ${r.elapsed_s}s<div class=k>${(r.assumptions||[]).join(' ')}</div>`;
document.getElementById('fdays').innerHTML='<tr><th>Run<th>Usable settings<th>Best settings<th>Return %<th>Trades<th>Win rate<th>Max drawdown<th>Robust score</tr>'+r.runs.map((x,i)=>{const p=x.params;return `<tr><td>${i+1}<td>${x.usable}<td style="text-align:left">${p?p.signal+' '+(p.flip?'follow':'contrarian')+', top '+p.top+', every '+dur(p.hold,Object.keys(IK).find(k=>IK[k]===RI))+', SPY margin '+F((p.risk||{}).trend_margin_pct,2)+'%':'<span class=k>none passed the safety rules</span>'}<td class=${(x.return_pct||0)>=0?'ok':'bad'}>${P(x.return_pct)}<td>${x.trades??'-'}<td class=${x.trades&&x.wins/x.trades>=0.6?'ok':''}>${x.trades&&x.wins!=null?F(x.wins/x.trades*100,0)+'%':'-'}<td>${F(x.max_drawdown_pct,1)}%<td>${F(x.robust_score)}</tr>`}).join('');
}catch(e){document.getElementById('fsum').textContent='chart error: '+e}}
// Draws the interval tabs of the research section and marks the selected one.
function tabs(){document.getElementById('rtabs').innerHTML=Object.keys(IK).map(k=>'<button class='+(IK[k]===RI?'on':'')+' onclick="RI=\''+IK[k]+'\';tabs();research();walkForward()">'+IL[k]+'</button>').join('')}
setInterval(research,60000);setInterval(walkForward,60000);
// Research lab panel: the flashing alert appears only when the lab's code finds a considerably better strategy (validated on both
// block cuts, same candles as the current strategy, unseen-block return >= +1 point per block better, win rate not lower).
const esc=x=>String(x==null?'':x).replace(/[&<>"]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
async function lab(){try{const L=await (await fetch('/api/lab')).json();const el=document.getElementById('lab'),al=document.getElementById('labalert');
if(!L.up){el.textContent='lab dashboard not answering (check com.qwenlab.dash on 127.0.0.1:8790)';al.innerHTML='';return}
al.innerHTML=(L.better||[]).map(b=>'<div class=flash>BETTER STRATEGY PASSED THE FULL VALIDATION: '+esc(b.interval)+' | '+esc(b.tokens)+' — '+(b.cuts||[]).map(c=>'cut '+c.cut+': unseen '+f(c.holdout)+'%/block vs current '+f(c.baseline_holdout)+'%, win '+f(c.win,0)+'% vs '+f(c.baseline_win,0)+'%').join(' · ')+'. Goes live at the next market close (Claude Code applies it in code).</div>').join('');
const w=(L.winners||[]).map(x=>'<tr><td>'+esc(x.status)+'<td>'+esc(x.interval+' | '+x.tokens)+'<td>'+f(x.holdout_a)+' ('+f(x.base_a)+')<td>'+f(x.holdout_b)+' ('+f(x.base_b)+')</tr>').join('');
const sb=(L.scoreboard||[]).map(x=>'<tr><td>'+esc(x.agent)+'<td class='+(x.points>0?'ok':x.points<0?'bad':'k')+'>'+f(x.points,0)+'<td>'+f(x.requests,0)+'<td>'+f(x.pending,0)+'</tr>').join('');
const hq=(L.hermes||[]).map(x=>'<tr><td>'+esc(x.author)+'<td>'+esc(x.interval+' | '+x.variant)+'<td>'+esc(x.status)+'</tr>').join('');
const ms=(L.messages||[]).map(x=>'<tr><td>'+esc(x.from)+' → '+esc(x.to)+'<td style="text-align:left">'+esc(x.text)+'</tr>').join('');
let LS=null;try{LS=(await (await fetch('/api/status')).json()).live_strategy}catch(e){}
el.innerHTML=(LS?'<div><b>Live strategy:</b> '+(LS.options.length?esc(LS.interval)+' + '+LS.options.map(esc).join(' + ')+' <span class=k>(promoted '+esc(LS.since)+' from '+esc(LS.variant)+', found by '+esc(LS.source)+')</span>':'the plain strategy (no promoted option)')+' <span class=k>— '+esc(LS.why)+' Promotions: automatic after the full validation, applied in code while the market is closed (docs/STRATEGY.md).</span></div>':'')+'lab status: <b class='+(L.level==='ok'?'ok':'warn')+'>'+esc(L.level)+'</b>'+((L.alerts||[]).length?' — '+L.alerts.map(esc).join('; '):'')
+((L.better_pending||[]).length?'<br>better on cuts A and B but not eligible to go live: '+L.better_pending.map(esc).join(' · '):'')
+'<div class=ivgrid><div><b>Validated / candidate variants</b> <span class=k>(unseen-block %/block, current strategy in brackets)</span><table><tr><th>status<th>variant<th>cut A<th>cut B</tr>'+(w||'<tr><td>none yet</tr>')+'</table></div>'
+'<div style="grid-column:1/-1"><b>Lab brain and strategy tests</b> <span class=k>(every proposal backtested on 2026 walk-forward blocks; judged by code on unseen blocks against the current strategy; % per 14-session block, USD at the '+f(L.capital,0)+' USD account)</span>'
+'<table><tr><th>agent<th>type<th>tested<th>validated<th>cut A only<th>failed<th>avg vs current (pts)<th>best on unseen blocks</tr>'+(L.agents||[]).map(x=>'<tr><td>'+esc(x.source)+'<td>'+esc(x.kind)+'<td>'+f(x.tested,0)+'<td class='+(x.validated?'ok':'k')+'>'+f(x.validated,0)+'<td>'+f(x.candidate,0)+'<td>'+f(x.failed+x.failed_alt_cut,0)+'<td class='+(x.avg_gain_a>0?'ok':'bad')+'>'+f(x.avg_gain_a)+'<td style="text-align:left">'+(x.best?esc(x.best.variant)+': '+f(x.best.holdout_a)+' vs '+f(x.best.base_a)+' ('+esc(x.best.status)+')':'-')+'</tr>').join('')+'</table>'
+'<b>Newest agent proposals</b><table><tr><th>agent<th>variant<th>result<th>cut A %/block<th>USD/block<th>cut B<th>win rate</tr>'+(L.agent_rows||[]).map(x=>'<tr><td>'+esc(x.source)+'<td>'+esc(x.variant)+'<td>'+esc(x.status)+'<td class='+(x.holdout_a>x.base_a?'ok':'bad')+'>'+f(x.holdout_a)+' <span class=k>vs '+f(x.base_a)+'</span><td>'+f(x.usd_a,0)+' <span class=k>vs '+f(x.base_usd_a,0)+'</span><td>'+f(x.holdout_b)+' <span class=k>vs '+f(x.base_b)+'</span><td>'+f(x.win_a,0)+'% <span class=k>vs '+f(x.base_win_a,0)+'%</span></tr>').join('')+'</table></div>'
+'<div><b>Agent reward</b> <span class=k>(code-computed from real test outcomes)</span><table><tr><th>agent<th>points<th>requests<th>testing</tr>'+sb+'</table></div>'
+'<div><b>Historical Hermes requests</b><table><tr><th>agent<th>variant<th>status</tr>'+(hq||'<tr><td>none yet</tr>')+'</table></div>'
+'<div><b>Agents talking</b><table>'+(ms||'<tr><td>no notes yet</tr>')+'</table></div></div>'
+'<div>security guard: '+((L.security||[]).length?L.security.length+' recent item(s) withheld':'nothing withheld')+((L.quarantined||[]).length?', '+L.quarantined.length+' skill(s) quarantined':'')+'</div>';
}catch(e){document.getElementById('lab').textContent='lab panel error: '+e}}
lab();setInterval(lab,30000);
// NOTE(agents): Reload on process restart: the page embeds strategy labels and
// chart code. Polling new numbers into an old page would mix code versions.
let boot=null;
// Reloads the page when the service restarted (its start time changed), so the page code is never stale.
async function guard(){try{const s=await (await fetch('/api/status')).json();if(boot&&s.started_ms!==boot)location.reload();boot=s.started_ms;}catch(e){}}
tick();guard();setInterval(tick,5000);setInterval(guard,15000);
</script>"#;

#[cfg(test)]
mod causal_tests {
    use super::*;

    /// Test: the console state file restores research, walk-forward, search summaries and the paper interval choice.
    #[test]
    fn console_state_file_restores_what_was_in_memory() {
        let dir = std::env::temp_dir().join(format!("console-state-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = serde_json::json!({"saved_ms": 1, "research": {"5h": {"runs_positive": 3}}, "walk_forward": {"5h": {"return_pct": 1.5}},
            "searches": {"5h": {"searched_ms": 7, "winner": {"return_pct": 2.0}}}, "paper_choice": Interval::H5});
        std::fs::write(dir.join(CONSOLE_STATE_FILE), state.to_string()).unwrap();
        let s = load_console_state(&dir);
        assert_eq!(
            s.research[&Interval::H5]["runs_positive"],
            serde_json::json!(3)
        );
        assert_eq!(
            s.walk_forward[&Interval::H5]["return_pct"],
            serde_json::json!(1.5)
        );
        assert_eq!(
            s.searches[&Interval::H5]["winner"]["return_pct"],
            serde_json::json!(2.0)
        );
        assert_eq!(s.paper_choice, Some(Interval::H5));
        assert!(
            load_console_state(&dir.join("missing")).research.is_empty(),
            "a missing file restores nothing, no error"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Test: candle service drawdown exit is observed resumed within an hour.
    #[test]
    fn candle_service_drawdown_exit_is_observed_resumed_within_an_hour() {
        let mut full = fixture_market();
        for rows in [&mut full.bars, &mut full.marks] {
            for row in rows {
                let price = row[193].unwrap().close * 0.7;
                for bar in row.iter_mut().skip(194).flatten() {
                    bar.open = price;
                    bar.high = price;
                    bar.low = price;
                    bar.close = price;
                }
            }
        }
        let p = XsParams {
            signal: xs::Signal::Return,
            flip: false,
            lookback: 1,
            hold: 192,
            top: 1,
            gross_leverage: 2.0,
            stop_pct: None,
            risk: xs::Risk::default(),
            long_only: true,
            regime: xs::Regime::Off,
        };
        let mut ps = PaperState {
            schema_version: 2,
            interval: Interval::M1,
            interval_since: None,
            settlements_seen: Default::default(),
            portfolio: XsPortfolio::new(1000.0),
            last_ts: 189 * BAR_MS,
            params: None,
            last_account_balance: None,
            ledger: Default::default(),
        };
        let mut resumed_observed_at = None;
        for t in 190..=199 {
            let mut market = full.clone();
            market.ts.truncate(t + 1);
            for row in &mut market.bars {
                row.truncate(t + 1);
            }
            for row in &mut market.marks {
                row.truncate(t + 1);
            }
            let sc = scores::compute(&market, 20);
            advance_paper(
                &mut ps,
                &market,
                &sc,
                &p,
                true,
                market.ts[t] + BAR_MS + 5000,
            )
            .unwrap();
            if !ps.portfolio.trades.is_empty()
                && !ps.portfolio.positions.is_empty()
                && resumed_observed_at.is_none()
            {
                resumed_observed_at = Some(market.ts[t] + BAR_MS + 5000);
            }
            if t == 194 {
                assert!(ps.portfolio.stopped_out);
            }
            if t == 195 {
                assert!(
                    ps.portfolio.stopped_out && ps.portfolio.positions.len() == 1,
                    "finish the delayed exit before rearming"
                );
            }
            if t == 196 {
                assert!(ps.portfolio.positions.is_empty());
                assert!(!ps.portfolio.stopped_out);
                assert_eq!(ps.portfolio.trades[0].reason, "Drawdown stop");
            }
        }
        assert_eq!(ps.portfolio.positions.len(), 1);
        let entry = ps.portfolio.positions[0].entry_ts;
        assert!(
            entry - 195 * BAR_MS < 60 * 60_000,
            "entry timestamp {entry} must follow stop decision within the hour"
        );
        assert_eq!(entry, 198 * BAR_MS, "resume bypasses the 48-hour hold");
        assert!(
            resumed_observed_at.unwrap() - (195 * BAR_MS + 5000) <= 60 * 60_000,
            "paper observation timing, not just simulated entry time, must meet the hour"
        );
        assert!(
            ps.portfolio.metrics().max_drawdown_pct > 25.0,
            "resuming must not erase global historical drawdown"
        );
    }

    /// Test: on time live reports do not postpone an older entry forever.
    #[test]
    fn on_time_live_reports_do_not_postpone_an_older_entry_forever() {
        let full = fixture_market();
        let p = XsParams {
            signal: xs::Signal::Return,
            flip: false,
            lookback: 1,
            hold: 16,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: xs::Risk::default(),
            long_only: true,
            regime: xs::Regime::Off,
        };
        let mut ps = PaperState {
            schema_version: 2,
            interval: Interval::M1,
            interval_since: None,
            settlements_seen: Default::default(),
            portfolio: XsPortfolio::new(1000.0),
            last_ts: 157 * BAR_MS,
            params: None,
            last_account_balance: None,
            ledger: Default::default(),
        };
        for t in 158..=162 {
            let mut market = full.clone();
            market.ts.truncate(t + 1);
            for row in &mut market.bars {
                row.truncate(t + 1);
            }
            for row in &mut market.marks {
                row.truncate(t + 1);
            }
            let sc = scores::compute(&market, 20);
            advance_paper(
                &mut ps,
                &market,
                &sc,
                &p,
                true,
                market.ts[t] + BAR_MS + 5000,
            )
            .unwrap();
            if t <= 160 {
                assert!(ps.portfolio.positions.is_empty());
            }
        }
        assert_eq!(ps.portfolio.positions.len(), 1);
        assert_eq!(ps.portfolio.positions[0].entry_ts, 161 * BAR_MS);
    }
    /// Two symbols with deterministic noisy paths and real wicks, so stops, trailing exits and
    /// take-profits all fire.
    fn noisy_market() -> moomoo_bot::engine::Market {
        let mut m = fixture_market();
        m.symbols[0] = REFERENCE.into();
        let mut seed = 12345u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) - 0.5
        };
        for s in 0..2 {
            let mut price = 100.0;
            for t in 0..200 {
                let open = price;
                price *= 1.0 + next() * 0.03;
                let close = price;
                let high = open.max(close) * (1.0 + next().abs() * 0.01);
                let low = open.min(close) * (1.0 - next().abs() * 0.01);
                m.bars[s][t] = Some(moomoo_bot::engine::Bar {
                    open,
                    high,
                    low,
                    close,
                    volume: 1.0,
                    turnover: 100.0,
                });
            }
        }
        m.marks = m.bars.clone();
        m
    }

    /// PAPER == TESTING. Stepping the paper portfolio bar by bar the way the live service does
    /// (market grows one closed bar at a time, scores recomputed, `advance_paper`) must produce
    /// exactly the closed trades of one `xs::backtest` over the same bars with the same settings,
    /// including trailing, break-even, take-profit (70% rule) and stops.
    fn check_parity(p: XsParams, min_trades: usize) {
        let full = noisy_market();
        let sc_full = scores::compute(&full, 20);
        let bt = xs::backtest(&full, &sc_full, 100..200, &p, 1000.0);
        let bt_trades: Vec<_> = bt
            .trades
            .iter()
            .filter(|t| t.reason != "End of test")
            .collect();
        assert!(
            bt_trades.len() >= min_trades,
            "fixture must trade: {}",
            bt_trades.len()
        );

        let mut all_events: Vec<moomoo_bot::engine::xs::EngineEvent> = Vec::new();
        let mut ps = PaperState {
            schema_version: 2,
            interval: Interval::M1,
            interval_since: None,
            settlements_seen: Default::default(),
            portfolio: XsPortfolio::new(1000.0),
            last_ts: 99 * BAR_MS,
            params: None,
            last_account_balance: None,
            ledger: Default::default(),
        };
        for t in 100..200 {
            let mut market = full.clone();
            market.ts.truncate(t + 1);
            for row in &mut market.bars {
                row.truncate(t + 1);
            }
            for row in &mut market.marks {
                row.truncate(t + 1);
            }
            let sc = scores::compute(&market, 20);
            all_events.extend(
                advance_paper(
                    &mut ps,
                    &market,
                    &sc,
                    &p,
                    true,
                    market.ts[t] + BAR_MS + 5000,
                )
                .unwrap(),
            );
        }
        // The event log mirrors what happened: one decision per rebalance with every coin's
        // fate, and one fill event per entry (closed trades + still-open positions).
        let fills = all_events
            .iter()
            .filter(|e| e.kind == "entry_filled")
            .count();
        assert_eq!(fills, bt.trades.len(), "every entry is logged exactly once");
        let decisions: Vec<_> = all_events.iter().filter(|e| e.kind == "decision").collect();
        assert!(!decisions.is_empty());
        assert!(decisions.iter().all(|d| d.detail["candidates"].is_array()));
        // And it round-trips into the store.
        let dir = std::env::temp_dir().join(format!(
            "moomoo-parity-store-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let st = Store::open(&dir.join("s.sqlite3")).unwrap();
        st.events(1, &all_events, Some).unwrap();
        let paper: Vec<_> = ps.portfolio.trades.iter().collect();
        assert_eq!(
            paper.len(),
            bt_trades.len(),
            "paper {:?}\nbacktest {:?}",
            paper
                .iter()
                .map(|t| (
                    &t.symbol,
                    t.entry_ts / BAR_MS,
                    t.exit_ts / BAR_MS,
                    &t.reason
                ))
                .collect::<Vec<_>>(),
            bt_trades
                .iter()
                .map(|t| (
                    &t.symbol,
                    t.entry_ts / BAR_MS,
                    t.exit_ts / BAR_MS,
                    &t.reason
                ))
                .collect::<Vec<_>>()
        );
        for (a, b) in paper.iter().zip(&bt_trades) {
            assert_eq!(
                (&a.symbol, a.entry_ts, a.exit_ts, &a.reason),
                (&b.symbol, b.entry_ts, b.exit_ts, &b.reason)
            );
            assert!(
                (a.pnl - b.pnl).abs() < 1e-6
                    && (a.entry - b.entry).abs() < 1e-9
                    && (a.exit - b.exit).abs() < 1e-9
            );
        }
        // Same open positions at the end (the backtest closes them as "End of test").
        let open_bt = bt
            .trades
            .iter()
            .filter(|t| t.reason == "End of test")
            .count();
        assert_eq!(ps.portfolio.positions.len(), open_bt);
    }

    /// Test: paper stepping matches the backtest trade for trade.
    #[test]
    fn paper_stepping_matches_the_backtest_trade_for_trade() {
        check_parity(
            XsParams {
                signal: xs::Signal::Return,
                flip: false,
                lookback: 4,
                hold: 8,
                top: 1,
                gross_leverage: 5.0,
                stop_pct: Some(4.0),
                risk: xs::Risk {
                    trail_pct: Some(1.5),
                    take_profit_pct: Some(3.0),
                    breakeven_pct: Some(1.0),
                    close_stop_pct: Some(6.0),
                    ..xs::Risk::default()
                },
                long_only: true,
                regime: xs::Regime::Off,
            },
            5,
        );
    }

    /// The dashboard's winner curve ends exactly where the backtest ends.
    #[test]
    fn winner_curve_matches_the_backtest() {
        let m = noisy_market();
        let sc = scores::compute(&m, walkforward::UNIVERSE);
        let p = XsParams {
            signal: xs::Signal::Return,
            flip: false,
            lookback: 4,
            hold: 8,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: xs::Risk::default(),
            long_only: true,
            regime: xs::Regime::Off,
        };
        let frame = |test_start: usize| Frame {
            interval: Interval::M1,
            market: Arc::new(m.clone()),
            sessions: Arc::new(
                Sessions::new(vec![(m.wall[0], m.wall[m.wall.len() - 1] + BAR_MS)]).unwrap(),
            ),
            test_start,
            closes: m.wall.iter().map(|w| w + BAR_MS).collect(),
        };
        let bt = xs::backtest(&m, &sc, 0..m.ts.len(), &p, 1000.0);
        let w = winner_curve(&frame(0), &sc, &p, 1000.0);
        assert!(bt.trades.len() >= 2, "fixture must trade");
        assert!((w["end_equity"].as_f64().unwrap() - bt.equity).abs() < 1e-9);
        assert_eq!(w["trade_list"].as_array().unwrap().len(), bt.trades.len());
        assert_eq!(w["points"].as_array().unwrap().len(), m.ts.len() + 1);
        // With warm-up candles before the test window, only the window is traded and charted.
        let start = m.ts.len() / 2;
        let bt = xs::backtest(&m, &sc, start..m.ts.len(), &p, 1000.0);
        let w = winner_curve(&frame(start), &sc, &p, 1000.0);
        assert!((w["end_equity"].as_f64().unwrap() - bt.equity).abs() < 1e-9);
        assert_eq!(
            w["points"].as_array().unwrap().len(),
            m.ts.len() - start + 1
        );
        assert_eq!(w["points"][0][0].as_i64(), Some(m.wall[start]));
        // The 14-session span return starts from the equity at that span's start.
        let pts = w["points"].as_array().unwrap();
        let mid = pts[pts.len() / 2][0].as_i64().unwrap();
        let base = pts[pts.len() / 2][1].as_f64().unwrap();
        let r = span_return(&w, mid).unwrap();
        assert!((r - (bt.equity / base - 1.0) * 100.0).abs() < 1e-9);
    }

    /// The paper interval changes only between sessions, as often as needed.
    #[test]
    fn orders_are_sized_to_available_cash_and_capital_follows_the_account() {
        assert_eq!(
            affordable_qty(10_000.0, 100.0, 50.0),
            50.0,
            "enough cash: the plan stands"
        );
        assert_eq!(
            affordable_qty(1_000.0, 100.0, 50.0),
            9.0,
            "cash for 9 whole shares incl. the slack"
        );
        assert_eq!(affordable_qty(50.0, 100.0, 5.0), 0.0, "not one share");
        assert_eq!(affordable_qty(f64::NAN, 100.0, 5.0), 0.0);
        let acct = |cash: f64| Account {
            id: 1,
            total_assets: cash,
            cash,
            power: cash,
            market_val: 0.0,
            ts: 0,
        };
        assert_eq!(
            capital_from(&acct(999_991.34)),
            START_CAPITAL,
            "a large paper balance is traded as a 5000 USD account"
        );
        assert_eq!(
            capital_from(&acct(3_200.7)),
            3_200.0,
            "less cash than the account size: trade what is there"
        );
        assert_eq!(
            capital_from(&acct(12.0)),
            12.0,
            "known low cash is used as-is"
        );
        assert_eq!(
            capital_from(&acct(f64::NAN)),
            0.0,
            "unknown cash disables sizing"
        );
    }

    /// Test: a healthy paper interval changes only between sessions; a losing one does not hold the choice hostage.
    #[test]
    fn paper_interval_changes_only_between_sessions() {
        let n = || String::from("cmp");
        let (d, h4) = (Some(Interval::D1), Some(Interval::H4));
        assert_eq!(
            hold_interval(true, d, true, h4, n()).0,
            d,
            "never while the market is open"
        );
        assert_eq!(
            hold_interval(false, d, true, h4, n()).0,
            h4,
            "between sessions: switch"
        );
        assert_eq!(
            hold_interval(true, None, false, h4, n()).0,
            h4,
            "first pick"
        );
        assert_eq!(
            hold_interval(true, d, true, None, n()).0,
            d,
            "eligible settings may finish the session"
        );
        assert_eq!(
            hold_interval(true, d, false, h4, n()).0,
            h4,
            "losing settings cannot hold the interval choice"
        );
        assert_eq!(
            hold_interval(true, d, false, None, n()).0,
            None,
            "no profitable interval means no new entries"
        );
    }

    #[test]
    fn a_disqualified_flat_interval_does_not_keep_a_stale_buy_plan() {
        assert!(can_handoff_interval(true, true, true, true, false));
        assert!(
            can_handoff_interval(true, true, true, false, true),
            "a flat account can switch between sessions"
        );
        assert!(!can_handoff_interval(true, true, true, false, false));
        assert!(
            !can_handoff_interval(false, false, false, true, true),
            "held positions are never force-switched"
        );
    }

    /// The shared span starts at the 14th most recent CLOSED session, also mid-session.
    #[test]
    fn common_span_counts_closed_sessions_only() {
        let day = 86_400_000i64;
        let len = 26 * BAR_MS;
        let list: Vec<(i64, i64)> = (0..20).map(|d| (d * day, d * day + len)).collect();
        let s = Sessions::new(list).unwrap();
        // After session 19 closed: sessions 6..=19.
        assert_eq!(common_span_start(&s, 19 * day + len), Some(6 * day));
        // Four bars into session 19: it is not closed, so sessions 5..=18.
        assert_eq!(common_span_start(&s, 19 * day + 4 * BAR_MS), Some(5 * day));
        assert_eq!(common_span_start(&s, 12 * day + len), None);
    }

    /// The paper interval is the best safe winner over the shared span; ties keep the current one.
    #[test]
    fn paper_picks_the_best_interval_and_keeps_the_current_on_a_tie() {
        let p = XsParams {
            signal: xs::Signal::Return,
            flip: false,
            lookback: 4,
            hold: 8,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: xs::Risk::default(),
            long_only: true,
            regime: xs::Regime::Off,
        };
        let report = SearchReport {
            first_bar_ts: 0,
            last_bar_ts: 0,
            start_equity: 1000.0,
            combos: 1,
            evaluated: 1,
            usable: 1,
            elapsed_ms: 0,
            params: None,
            robust_score: None,
            raw_best_score: None,
            seed: None,
            margin_range: None,
            exit_analysis: None,
            top: Vec::new(),
            usable_all: Vec::new(),
            recorded_tested: 0,
            recorded_usable: 0,
            from_record: false,
            metrics: None,
        };
        let mk = |r: Option<f64>, chosen: bool| Search {
            common_return_pct: r,
            window: (0, 0),
            report: report.clone(),
            chosen: chosen.then(|| p.clone()),
            choice: walkforward::Choice::NewBest,
            finished_ms: 0,
            winner: None,
        };
        let mut all = BTreeMap::new();
        all.insert(Interval::M1, mk(Some(1.0), true));
        all.insert(Interval::H1, mk(Some(2.5), true));
        all.insert(Interval::H4, mk(Some(9.0), false)); // not safe: never chosen
        all.insert(Interval::H5, mk(Some(-1.0), true)); // losing: never chosen
        assert_eq!(pick_among(&Interval::ALL, &all, None).0, Some(Interval::H1));
        all.insert(Interval::D1, mk(Some(2.5), true));
        assert_eq!(
            pick_among(&Interval::ALL, &all, Some(Interval::D1)).0,
            Some(Interval::D1),
            "tie keeps the current"
        );
        assert_eq!(
            pick_among(&Interval::ALL, &all, Some(Interval::M1)).0,
            Some(Interval::H1),
            "tie: first best"
        );
        let none: BTreeMap<Interval, Search> =
            [(Interval::M1, mk(None, false))].into_iter().collect();
        assert_eq!(
            pick_among(&Interval::ALL, &none, Some(Interval::M1)).0,
            None
        );
        let losing: BTreeMap<Interval, Search> =
            [(Interval::H5, mk(Some(-1.0), true))].into_iter().collect();
        assert_eq!(
            pick_among(&Interval::ALL, &losing, Some(Interval::H5)).0,
            None
        );
    }

    /// Same, with the SPY trend filter and flatten-on-turn, ATR-scaled exits and the 70% take-profit.
    #[test]
    fn paper_stepping_matches_the_backtest_with_trend_filter_and_atr_exits() {
        check_parity(
            XsParams {
                signal: xs::Signal::Return,
                flip: true,
                lookback: 3,
                hold: 6,
                top: 1,
                gross_leverage: 3.0,
                stop_pct: None,
                risk: xs::Risk {
                    atr_trail_mult: Some(1.5),
                    atr_tp_mult: Some(2.0),
                    atr_stop_mult: Some(3.0),
                    trend_bars: Some(24),
                    trend_margin_pct: Some(0.2),
                    trend_exit: true,
                    ..xs::Risk::default()
                },
                long_only: true,
                regime: xs::Regime::MarketTrend,
            },
            2,
        );
    }

    /// Test fixture: a 200-bar market of synthetic symbols plus SPY with rules, for the service tests.
    fn fixture_market() -> moomoo_bot::engine::Market {
        let mut market = moomoo_bot::engine::Market {
            marks: Vec::new(),
            listing_times: Vec::new(),
            entry_eligible: Vec::new(),
            ts: (0..200).map(|i| i * BAR_MS).collect(),
            wall: (0..200).map(|i| i * BAR_MS).collect(),
            symbols: vec!["A".into(), "B".into()],
            bars: [-0.1, 0.1]
                .iter()
                .map(|drift| {
                    (0..200)
                        .map(|i| {
                            let price = 100.0 * (1.0 + drift * i as f64 / 200.0);
                            Some(moomoo_bot::engine::Bar {
                                open: price,
                                high: price,
                                low: price,
                                close: price,
                                volume: 1.0,
                                turnover: 100.0,
                            })
                        })
                        .collect()
                })
                .collect(),
            rules: vec![
                Some(moomoo_bot::engine::rules::Rules {
                    qty_step: 0.0,
                    min_qty: 0.0,
                    min_notional: 5.0,
                    max_market_qty: 1e12,
                    commission: vec![(1.0, 0.00055), (1e12, 1e12 * 0.00055)],
                    commission_source: "test".into(),
                    max_leverage: 100.0,
                    book: vec![(6400.0, 0.0002, 0.0002)],
                    book_ts: 0,
                    adv_usd: None,
                    daily_vol: None,
                });
                2
            ],
            funding: vec![vec![]; 2],
        };
        market.marks = market.bars.clone();
        market
    }

    /// Test: new report does not change replayed bars.
    #[test]
    fn new_report_does_not_change_replayed_bars() {
        let market = fixture_market();
        let sc = scores::compute(&market, 100);
        let old = XsParams {
            signal: xs::Signal::Return,
            flip: false,
            lookback: 1,
            hold: 2,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: xs::Risk::default(),
            long_only: true,
            regime: xs::Regime::Off,
        };
        let mut next = old.clone();
        next.hold = 4;
        let mut ps = PaperState {
            schema_version: 2,
            interval: Interval::M1,
            interval_since: None,
            settlements_seen: Default::default(),
            portfolio: XsPortfolio::new(1000.0),
            last_ts: 175 * BAR_MS,
            params: Some(old.clone()),
            last_account_balance: None,
            ledger: Default::default(),
        };
        ps.portfolio.entries_allowed = false;
        let mut expected = ps.portfolio.clone();
        for t in 176..200 {
            expected.step(&market, &sc, t, &old);
        }
        expected.install_entry_gate(true);
        expected.decide_close(&market, &sc, 199, &next);
        expected.defer_new_decisions(market.ts[199] + BAR_MS, market.ts[199]);
        advance_paper(&mut ps, &market, &sc, &next, true, market.ts[199] + BAR_MS).unwrap();
        assert_eq!(
            serde_json::to_value(&ps.portfolio).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
        assert_eq!(ps.params.unwrap().hold, 4);
        assert_eq!(ps.last_ts, 199 * BAR_MS);
    }

    /// A same-settings decision reaches its order; one made with superseded settings does not.
    #[test]
    fn only_a_still_selected_decision_reaches_its_order() {
        let market = fixture_market();
        let cut = |n: usize| {
            let mut m = market.clone();
            m.ts.truncate(n);
            m.wall.truncate(n);
            m.bars.iter_mut().for_each(|b| b.truncate(n));
            m.marks.iter_mut().for_each(|b| b.truncate(n));
            m
        };
        let old = XsParams {
            signal: xs::Signal::Return,
            flip: false,
            lookback: 1,
            hold: 2,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: xs::Risk::default(),
            long_only: true,
            regime: xs::Regime::Off,
        };
        let mut next = old.clone();
        next.hold = 3;
        let (m198, m197) = (cut(199), cut(198));
        let (sc198, sc197) = (scores::compute(&m198, 100), scores::compute(&m197, 100));
        let mut portfolio = XsPortfolio::new(1000.0);
        portfolio.install_entry_gate(true);
        for t in 176..198 {
            portfolio.step(&m197, &sc197, t, &old);
        }
        portfolio.decide_close(&m197, &sc197, 197, &old);
        portfolio.defer_new_decisions(m197.ts[197] + BAR_MS, m197.ts[197]);
        let mut ps = PaperState {
            schema_version: 2,
            interval: Interval::M1,
            interval_since: None,
            settlements_seen: Default::default(),
            portfolio,
            last_ts: m197.ts[197],
            params: Some(old.clone()),
            last_account_balance: None,
            ledger: Default::default(),
        };
        let mut same = ps.clone();
        advance_paper(&mut same, &m198, &sc198, &old, true, m198.ts[198] + BAR_MS).unwrap();
        advance_paper(&mut ps, &m198, &sc198, &next, true, m198.ts[198] + BAR_MS).unwrap();
        let mids: std::collections::HashMap<String, f64> = (0..2)
            .filter_map(|s| market.bars[s][199].map(|b| (market.symbols[s].clone(), b.open)))
            .collect();
        let same_plan = same
            .ledger
            .preview(
                &same.portfolio,
                &m198,
                &sc198,
                &old,
                market.ts[199],
                market.wall[199],
                &mids,
            )
            .unwrap();
        assert!(
            same_plan.iter().any(|p| p.side == 1),
            "same-settings decision must survive: {same_plan:?}"
        );
        let plan = ps
            .ledger
            .preview(
                &ps.portfolio,
                &m198,
                &sc198,
                &next,
                market.ts[199],
                market.wall[199],
                &mids,
            )
            .unwrap();
        assert!(
            plan.iter().all(|p| p.side != 1),
            "superseded buy must be canceled: {plan:?}"
        );
    }

    /// Test: a different winner cancels a queued buy using the superseded settings.
    #[test]
    fn new_settings_cancel_a_superseded_pending_buy() {
        let market = fixture_market();
        let sc = scores::compute(&market, 100);
        let old = XsParams {
            signal: xs::Signal::Return,
            flip: false,
            lookback: 1,
            hold: 2,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: xs::Risk::default(),
            long_only: true,
            regime: xs::Regime::Off,
        };
        let mut next = old.clone();
        next.hold = 3;
        let mut portfolio = XsPortfolio::new(1000.0);
        portfolio.install_entry_gate(true);
        for t in 176..200 {
            portfolio.step(&market, &sc, t, &old);
        }
        portfolio.decide_close(&market, &sc, 199, &old);
        let pending = serde_json::to_value(&portfolio).unwrap()["pending_targets"].clone();
        assert!(
            pending.as_array().is_some_and(|a| !a.is_empty()),
            "the fixture must produce a decision: {pending}"
        );
        let mut ps = PaperState {
            schema_version: 2,
            interval: Interval::M1,
            interval_since: None,
            settlements_seen: Default::default(),
            portfolio,
            last_ts: market.ts[199],
            params: Some(old),
            last_account_balance: None,
            ledger: Default::default(),
        };
        advance_paper(&mut ps, &market, &sc, &next, true, market.ts[199] + BAR_MS).unwrap();
        assert_eq!(
            ps.params.as_ref().unwrap().hold,
            3,
            "the new settings are installed"
        );
        assert_eq!(
            serde_json::to_value(&ps.portfolio).unwrap()["pending_targets"],
            serde_json::Value::Null,
            "the old decision cannot execute under the new winner"
        );
        // switching the entry gate off still cancels it
        advance_paper(&mut ps, &market, &sc, &next, false, market.ts[199] + BAR_MS).unwrap();
        assert!(serde_json::to_value(&ps.portfolio).unwrap()["pending_targets"].is_null());
    }
}

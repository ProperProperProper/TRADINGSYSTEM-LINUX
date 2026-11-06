#![allow(clippy::needless_update)] // req! always adds ..Default::default() so fields added by newer protocol files stay unset
//! Real moomoo data and PAPER trading through the local OpenD gateway (`crate::opend`):
//! * the US paper account (found via Trd_GetAccList, never a REAL account), its
//!   funds, positions, orders and order fees,
//! * the universe: SPY's holdings (data/spy_holdings.csv) by weight, NYSE / Nasdaq listings only, nothing delisted
//!   or unknown to moomoo,
//! * the session calendar (Qot_RequestTradeDate, whole and half days) and the trading clock built
//!   from it (`Sessions`), checked against the reference's candles,
//! * closed 1-minute regular-session candles (Qot_RequestHistoryKL, unadjusted), converted from
//!   moomoo's close-time labels to the engine's open-time convention,
//! * per-symbol rules: lot size, quote (bid/ask and sizes from the snapshot), fees of this account's
//!   real paper orders,
//! * the 7-day historical-candle quota (`QuotaState`): a symbol not pulled in the window is pulled
//!   only while quota remains.
//!
//! Nothing here invents a value: a request that fails is an error, a symbol with incomplete data
//! or rules sits out (and the service logs why).

use super::rules::{measure_book, Rules};
use super::{Bar, Market, BAR_MS};
use crate::opend::{id, OpenD, Refused, DEFAULT_ADDR};
use crate::pb;
use anyhow::{anyhow, bail, ensure, Context, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::America::New_York;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

/// First session of the trading clock (US/Eastern date). Fixed so the clock,
/// and with it every rebalance alignment and persisted timestamp, is identical on every fetch.
pub const CLOCK_EPOCH: (i32, u32, u32) = (2026, 1, 2);
// NOTE(agents): User request (2026-10-09): the bot trades and references ONLY the stocks SPY holds: every holding in SPY's
//               official file (data/spy_holdings.csv, installed by deploy.sh), by weight, minus anything delisted or not
//               available on moomoo (`ineligible`, no static info). There is no other universe source: without the file the bot
//               has no universe. Which holdings have candles at a time is set by moomoo's candle quota (heaviest first).
/// The universe is every eligible SPY holding (no cap).
pub const SPY_POOL: usize = usize::MAX;

/// Tickers of SPY's holdings by weight, best first, from the installed holdings file (`rank,ticker,name,weight_pct`, `#` comment lines).
pub fn load_holdings(path: &std::path::Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut rows: Vec<(f64, String)> = text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("rank,"))
        .filter_map(|l| {
            let c: Vec<&str> = l.split(',').collect();
            let (t, w) = (c.get(1)?.trim(), c.last()?.trim().parse::<f64>().ok()?);
            (!t.is_empty() && w.is_finite()).then(|| (w, t.to_string()))
        })
        .collect();
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    (!rows.is_empty()).then(|| rows.into_iter().map(|r| r.1).collect())
}
/// Where ThomasAi's weekly symbol pick is: `$THOMASAI_SELECTION`, else `data/selection.json` of the repository this bot lives in (bot/ sits inside it).
pub fn selection_path() -> std::path::PathBuf {
    std::env::var_os("THOMASAI_SELECTION")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../data/selection.json")
        })
}

/// The pick older than this is ignored (the bot falls back to SPY's weight order): ThomasAi makes one every week.
const SELECTION_MAX_AGE_MS: i64 = 8 * 86_400_000;

/// ThomasAi's pick from `path`, best first, if the file is readable, well formed and fresh at `now_ms`; otherwise None. Only tickers made of letters, digits, '.'
/// and '-' are accepted.
pub fn selection_symbols_at(path: &std::path::Path, now_ms: i64) -> Option<Vec<String>> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let at = v["generated_ms"].as_i64()?;
    if now_ms - at > SELECTION_MAX_AGE_MS || at > now_ms + 86_400_000 {
        return None;
    }
    let list: Vec<String> = v["symbols"]
        .as_array()?
        .iter()
        .filter_map(|s| s.as_str())
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 8
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        })
        .map(str::to_string)
        .collect();
    (!list.is_empty()).then_some(list)
}

/// `holdings` (SPY's, by weight) reordered so ThomasAi's picked symbols come first, in its order; only symbols SPY holds are used, the rest follow in weight
/// order. The universe is unchanged, only who gets the weekly candle quota first (and so who is backtested and traded) changes.
pub fn prefer_selected(holdings: Vec<String>, selected: &[String]) -> Vec<String> {
    let held: std::collections::BTreeSet<&String> = holdings.iter().collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<String> = selected
        .iter()
        .filter(|s| held.contains(s) && seen.insert((*s).clone()))
        .cloned()
        .collect();
    out.extend(holdings.iter().filter(|s| !seen.contains(*s)).cloned());
    out
}

/// `prefer_selected` with the current pick from `selection_path()` (no file or a stale one: the weight order unchanged).
pub fn prefer_selection(holdings: Vec<String>) -> Vec<String> {
    match selection_symbols_at(&selection_path(), ms_now()) {
        Some(sel) => prefer_selected(holdings, &sel),
        None => holdings,
    }
}

/// Quota left for held / reference symbols: a NEW candidate is pulled only while more than this
/// remains (the engine holds at most one position; the reference needs one more).
pub const QUOTA_RESERVE: i32 = 6;

/// moomoo market codes (Qot_Common.QotMarket / Trd_Common.TrdMarket / TradeDateMarket).
const QOT_US: i32 = 11;
const TRD_US: i32 = 2;
const TRADE_DATE_US: i32 = 2;
/// Qot_Common.ExchType: NYSE and Nasdaq (user rule: only these listings).
const EXCH_NYSE: i32 = 4;
const EXCH_NASDAQ: i32 = 5;
/// Qot_Common.SecurityType: common stock and trust (ETFs are trusts in moomoo's static info).
const SEC_EQUITY: i32 = 3;
const SEC_TRUST: i32 = 4;
/// Qot_Common.KLType 15 minutes.
const KL_1M: i32 = 1;
/// Trd_Common.TrdEnv: SIMULATE (paper). The REAL environment (1) is never used.
pub const TRD_ENV_SIMULATE: i32 = 0;

/// Current wall time in milliseconds.
pub fn ms_now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

// ---------------------------------------------------------------------------------------------
// Account guard
// ---------------------------------------------------------------------------------------------

// NOTE(agents): User rule (2026-10-06): NEVER send an order or an unlock to a REAL account. The
//               only account this bot trades is the US SIMULATE (paper) account that Trd_GetAccList
//               reports; `guard_order_header` runs before every trading write and refuses anything
//               else. The bot never sends Trd_UnlockTrade (paper needs no unlock).
/// The US paper account: exactly one SIMULATE account with US trading rights and a stock sim type.
pub fn select_paper_account(
    list: &[pb::trd_common::TrdAcc],
    preferred: Option<u64>,
) -> Result<u64> {
    let paper: Vec<&pb::trd_common::TrdAcc> = list
        .iter()
        .filter(|a| {
            a.trd_env == TRD_ENV_SIMULATE
                && a.trd_market_auth_list.contains(&TRD_US)
                && matches!(a.sim_acc_type, Some(1) | Some(4))
                && a.acc_status.unwrap_or(0) == 0
        })
        .collect();
    // MOOMOO_PAPER_ACCOUNT names the paper account to use when OpenD reports several (e.g. a competition account next to the
    // default one). It must be one of the active US SIMULATE stock accounts: a REAL or unknown account is still refused.
    if let Some(want) = preferred {
        ensure!(
            paper.iter().any(|a| a.acc_id == want),
            "MOOMOO_PAPER_ACCOUNT {want} is not an active US SIMULATE stock account"
        );
        return Ok(want);
    }
    ensure!(
        paper.len() == 1,
        "expected exactly one active US SIMULATE stock account, OpenD reports {} ({:?}); set MOOMOO_PAPER_ACCOUNT to choose one",
        paper.len(),
        list.iter().map(|a| (a.acc_id, a.trd_env, &a.trd_market_auth_list, a.sim_acc_type)).collect::<Vec<_>>()
    );
    Ok(paper[0].acc_id)
}

/// Refuse any trading write that is not for the US SIMULATE account `paper`.
pub fn guard_order_header(h: &pb::trd_common::TrdHeader, paper: u64) -> Result<()> {
    ensure!(
        h.trd_env == TRD_ENV_SIMULATE,
        "REFUSED: trading environment {} is not SIMULATE (paper)",
        h.trd_env
    );
    ensure!(
        h.acc_id == paper,
        "REFUSED: account {} is not the US paper account {paper}",
        h.acc_id
    );
    ensure!(
        h.trd_market == TRD_US,
        "REFUSED: trading market {} is not US",
        h.trd_market
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Universe
// ---------------------------------------------------------------------------------------------

/// The paper account as moomoo reports it (USD).
#[derive(Debug, Clone, serde::Serialize)]
pub struct Account {
    pub id: u64,
    pub total_assets: f64,
    pub cash: f64,
    pub power: f64,
    pub market_val: f64,
    /// When it was read (wall ms).
    pub ts: i64,
}

/// One position moomoo holds in the paper account.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Held {
    pub symbol: String,
    pub qty: f64,
    pub can_sell_qty: f64,
    pub cost_price: Option<f64>,
    pub price: f64,
}

/// A universe member: symbol and moomoo's static information.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Listing {
    pub symbol: String,
    pub name: String,
    pub sec_type: i32,
    pub exch_type: i32,
    pub lot_size: f64,
    /// Average daily turnover (None: SPY holdings carry none).
    pub avg_turnover: Option<f64>,
}

// NOTE(agents): User rule: NO leverage, long only. Leveraged (2x/3x) and inverse ETFs carry
//               leverage or short exposure inside the fund, so they are never traded. Identified
//               from the fund name (moomoo publishes no leverage flag).
/// Whether an ETF's name marks it as leveraged or inverse.
pub fn leveraged_or_inverse(name: &str) -> bool {
    let n = name.to_uppercase();
    let multiple = n
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.'))
        .any(|w| {
            w.len() >= 2
                && w.ends_with('X')
                && w[..w.len() - 1].parse::<f64>().is_ok_and(|x| x != 1.0)
        });
    multiple
        || ["ULTRA", "LEVERAGED", "INVERSE", "SHORT", "BEAR", "BULL"]
            .iter()
            .any(|k| {
                n.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|w| w.starts_with(k))
            })
}

/// Why a security may not be a candidate (None = eligible): NYSE / Nasdaq listing, common stock or
/// a non-leveraged, non-inverse fund, not delisting, a positive lot size.
pub fn ineligible(b: &pb::qot_common::SecurityStaticBasic) -> Option<String> {
    if b.security.market != QOT_US {
        return Some(format!("market {} is not US", b.security.market));
    }
    match b.exch_type {
        Some(EXCH_NYSE) | Some(EXCH_NASDAQ) => {}
        other => {
            return Some(format!(
                "listed on exchange type {other:?}, not NYSE or Nasdaq"
            ))
        }
    }
    if b.delisting == Some(true) {
        return Some("delisting".into());
    }
    match b.sec_type {
        SEC_EQUITY => {}
        SEC_TRUST if leveraged_or_inverse(&b.name) => {
            return Some(format!("leveraged or inverse fund ({})", b.name))
        }
        SEC_TRUST => {}
        t => return Some(format!("security type {t} is neither a stock nor a fund")),
    }
    if b.lot_size <= 0 {
        return Some(format!("lot size {}", b.lot_size));
    }
    None
}

impl Listing {
    /// Listing from moomoo's static info.
    fn of(b: &pb::qot_common::SecurityStaticBasic, avg_turnover: Option<f64>) -> Listing {
        Listing {
            symbol: b.security.code.clone(),
            name: b.name.clone(),
            sec_type: b.sec_type,
            exch_type: b.exch_type.unwrap_or(0),
            lot_size: b.lot_size as f64,
            avg_turnover,
        }
    }
}

/// The candidate pool from screener rows (best first) and their static information: eligible
/// rows in screener order, at most `cap`; the rest with the reason they were left out.
pub fn screen(
    rows: &[(String, f64)],
    info: &HashMap<String, pb::qot_common::SecurityStaticBasic>,
    cap: usize,
) -> (Vec<Listing>, Vec<(String, String)>) {
    let mut out = Vec::new();
    let mut excluded = Vec::new();
    for (sym, turnover) in rows {
        if out.len() >= cap {
            break;
        }
        match info.get(sym) {
            None => excluded.push((sym.clone(), "no static information from moomoo".to_string())),
            Some(b) => match ineligible(b) {
                Some(why) => excluded.push((sym.clone(), why)),
                None => out.push(Listing::of(b, Some(*turnover))),
            },
        }
    }
    (out, excluded)
}

// ---------------------------------------------------------------------------------------------
// Candle quota
// ---------------------------------------------------------------------------------------------

/// The historical-candle quota (Qot_RequestHistoryKLQuota): distinct stocks per rolling 7 days.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct QuotaState {
    pub used: i32,
    pub remain: i32,
    /// Symbols already pulled inside the window (re-pulling them is free), with moomoo's time text.
    pub pulled: BTreeMap<String, String>,
    pub ts: i64,
}

/// Whether candles of `symbol` may be requested now. Re-pulling a symbol already used in the
/// 7-day window is free; a new symbol needs remaining quota above `reserve` (0 for symbols that
/// must be read: held positions and the reference).
pub fn quota_allows(q: &QuotaState, symbol: &str, reserve: i32) -> bool {
    q.pulled.contains_key(symbol) || q.remain > reserve
}

// ---------------------------------------------------------------------------------------------
// Calendar and candle times
// ---------------------------------------------------------------------------------------------

// NOTE(agents): MARKET FACTS (not available from the API, documented here): the US regular session
//               runs 09:30-16:00 US/Eastern; on a half day (moomoo TradeDateType_Morning) it closes
//               at 13:00. moomoo's calendar gives the dates and whole/half type; `check_calendar`
//               verifies these hours against the reference's real candles every bar.
const SESSION_OPEN: (u32, u32) = (9, 30);
const SESSION_CLOSE: (u32, u32) = (16, 0);
const HALF_DAY_CLOSE: (u32, u32) = (13, 0);
/// moomoo Qot_Common.TradeDateType.
const TRADE_DATE_WHOLE: i32 = 0;
const TRADE_DATE_MORNING: i32 = 1;

/// Wall time (ms) of `hm` (hour, minute) US/Eastern on `date`.
fn et_ms(date: NaiveDate, hm: (u32, u32)) -> Result<i64> {
    let t = NaiveTime::from_hms_opt(hm.0, hm.1, 0).ok_or_else(|| anyhow!("bad time {hm:?}"))?;
    let local = New_York
        .from_local_datetime(&date.and_time(t))
        .single()
        .ok_or_else(|| anyhow!("ambiguous US/Eastern time {date} {hm:?}"))?;
    Ok(local.timestamp_millis())
}

/// Wall (open, close) of the regular session on `date` of moomoo trade-date type `kind`.
pub fn session_bounds(date: NaiveDate, kind: i32) -> Result<(i64, i64)> {
    let close = match kind {
        TRADE_DATE_WHOLE => SESSION_CLOSE,
        TRADE_DATE_MORNING => HALF_DAY_CLOSE,
        k => bail!("{date}: unexpected US trade-date type {k} (afternoon-only session)"),
    };
    Ok((et_ms(date, SESSION_OPEN)?, et_ms(date, close)?))
}

// NOTE(agents): moomoo labels a 1-minute candle with its CLOSE time in US/Eastern ("09:45:00" is
//               the 09:30-09:45 bar; a full session has 26, the last labelled "16:00:00"). The
//               engine keys bars by OPEN time (UTC ms), so the open is the label minus 15 minutes.
/// UTC open time (ms) of the 1-minute candle moomoo labels `time` (US/Eastern close time).
pub fn candle_open_ms(time: &str) -> Result<i64> {
    let t = NaiveDateTime::parse_from_str(time.trim(), "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(time.trim(), "%Y-%m-%d %H:%M:%S%.f"))
        .with_context(|| format!("bad candle time {time:?}"))?;
    let close = New_York
        .from_local_datetime(&t)
        .single()
        .ok_or_else(|| anyhow!("ambiguous US/Eastern candle time {time:?}"))?;
    Ok(close.timestamp_millis() - BAR_MS)
}

/// Check the calendar against the reference's real candles: every session fully inside
/// [`from`, `to`) must have exactly its 1-minute bars (first at the open, last ending at the
/// close), and no candle may fall outside a session. Returns (sessions with no candles at all:
/// unscheduled closures the calendar does not know, problems).
pub fn check_calendar(
    sessions: &Sessions,
    candles: &BTreeMap<i64, Bar>,
    from: i64,
    to: i64,
) -> (Vec<(i64, i64)>, Vec<String>) {
    let mut closed = Vec::new();
    let mut problems = Vec::new();
    for &(a, b) in sessions.list.iter().filter(|s| s.0 >= from && s.1 <= to) {
        let got: Vec<i64> = candles.range(a..b).map(|(k, _)| *k).collect();
        let want = ((b - a) / BAR_MS) as usize;
        if got.is_empty() {
            closed.push((a, b));
        } else if got.len() != want || got[0] != a || *got.last().unwrap_or(&0) + BAR_MS != b {
            problems.push(format!(
                "session {} has {} of {want} candles ({} .. {})",
                fmt_et(a),
                got.len(),
                got.first().map_or("-".into(), |&x| fmt_et(x)),
                got.last().map_or("-".into(), |&x| fmt_et(x + BAR_MS))
            ));
        }
    }
    for &k in candles.range(from..to).map(|(k, _)| k) {
        if sessions.clock(k).is_none() {
            problems.push(format!(
                "candle opening {} lies outside the calendar's sessions",
                fmt_et(k)
            ));
        }
    }
    (closed, problems)
}

/// Formats a wall time (ms) as "YYYY-MM-DD HH:MM ET".
pub fn fmt_et(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| {
            d.with_timezone(&New_York)
                .format("%Y-%m-%d %H:%M ET")
                .to_string()
        })
        .unwrap_or_else(|| ms.to_string())
}

/// ET date of a wall time.
pub fn et_date(ms: i64) -> NaiveDate {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&New_York).date_naive())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------------------------

/// One order as moomoo reports it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OrderState {
    pub order_id: u64,
    pub order_id_ex: String,
    pub symbol: String,
    /// 1 buy, 2 sell (Trd_Common.TrdSide).
    pub side: i32,
    pub qty: f64,
    pub status: i32,
    pub fill_qty: f64,
    pub fill_avg_price: Option<f64>,
    pub last_err: Option<String>,
    pub create_time: String,
    pub update_time: String,
    pub remark: Option<String>,
}

/// Trd_Common.OrderStatus values after which an order never changes again.
pub fn order_final(status: i32) -> bool {
    matches!(
        pb::trd_common::OrderStatus::try_from(status),
        Ok(pb::trd_common::OrderStatus::SubmitFailed)
            | Ok(pb::trd_common::OrderStatus::FilledAll)
            | Ok(pb::trd_common::OrderStatus::CancelledPart)
            | Ok(pb::trd_common::OrderStatus::CancelledAll)
            | Ok(pb::trd_common::OrderStatus::Failed)
            | Ok(pb::trd_common::OrderStatus::Disabled)
            | Ok(pb::trd_common::OrderStatus::Deleted)
            | Ok(pb::trd_common::OrderStatus::FillCancelled)
    )
}

impl OrderState {
    /// Order state from moomoo's order record.
    fn of(o: &pb::trd_common::Order) -> OrderState {
        OrderState {
            order_id: o.order_id,
            order_id_ex: o.order_id_ex.clone(),
            symbol: o.code.clone(),
            side: o.trd_side,
            qty: o.qty,
            status: o.order_status,
            fill_qty: o.fill_qty.unwrap_or(0.0),
            fill_avg_price: o.fill_avg_price.filter(|p| *p > 0.0),
            last_err: o.last_err_msg.clone().filter(|s| !s.is_empty()),
            create_time: o.create_time.clone(),
            update_time: o.update_time.clone(),
            remark: o.remark.clone(),
        }
    }
}

/// Fees moomoo charged an order: total and itemised.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OrderFee {
    pub order_id_ex: String,
    pub total: f64,
    pub items: Vec<(String, f64)>,
}

/// A real quote from the snapshot.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
    pub bid_vol: f64,
    pub ask_vol: f64,
    pub last: f64,
    pub lot: f64,
    pub suspended: bool,
}

impl Quote {
    /// Quote midpoint: (bid + ask) / 2.
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
    /// A two-sided, uncrossed quote with sizes.
    pub fn valid(&self) -> bool {
        !self.suspended
            && self.bid > 0.0
            && self.ask >= self.bid
            && self.bid_vol > 0.0
            && self.ask_vol > 0.0
            && self.mid().is_finite()
    }
}

// ---------------------------------------------------------------------------------------------
// Fees
// ---------------------------------------------------------------------------------------------

// NOTE(agents): moomoo has no pre-trade fee quote, and Trd_GetOrderFee answers "Paper trading is
//               not supported" for the SIMULATE account (verified 2026-10-06). The paper account
//               does charge fees: its cash moves by the fill value plus fees (INTC test orders:
//               buy 3 -> 0.99 USD, sell 3 -> 1.00 USD). Each order's fee is therefore measured as
//               the cash change around its fill minus the fill value (`derived_fee`; orders are placed
//               one at a time for that). The commission ladder is built from those real
//               observations (order notional, fee; buys and sells mixed, the larger fee at equal
//               notionals). Where no real fee exists, the COMMISSION
//               IS EXCLUDED from the simulation (user rule 2026-10-06: exclude, never invent) and
//               labelled so everywhere ("excluded: ..."); real fills still carry their real fees.
/// The fee of one fill from the paper account's cash before and after it: a buy costs its value
/// plus fees, a sell brings its value minus fees. `side`: 1 buy, 2 sell.
pub fn derived_fee(side: i32, value: f64, cash_before: f64, cash_after: f64) -> f64 {
    let change = cash_after - cash_before;
    if side == 1 {
        -change - value
    } else {
        value - change
    }
}

/// A real fill's fee: (notional USD, total fee USD).
pub type FeePoint = (f64, f64);

/// The commission ladder for the engine from real fee observations: ascending notional,
/// non-decreasing fee; with its source label. No observation: the commission is excluded (zero
/// over every size) and labelled as such.
pub fn commission_ladder(points: &[FeePoint]) -> (Vec<(f64, f64)>, String) {
    let mut pts: Vec<FeePoint> = points
        .iter()
        .copied()
        .filter(|(n, f)| n.is_finite() && *n > 0.0 && f.is_finite() && *f >= 0.0)
        .collect();
    if pts.is_empty() {
        return (
            vec![(1.0, 0.0), (1e12, 0.0)],
            "excluded: no real moomoo fee observed yet (commission not simulated)".into(),
        );
    }
    pts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut ladder: Vec<(f64, f64)> = Vec::new();
    for (n, f) in pts {
        match ladder.last().copied() {
            Some(last) if (last.0 - n).abs() < 1e-9 => {
                if let Some(l) = ladder.last_mut() {
                    l.1 = last.1.max(f);
                }
            }
            Some(last) => ladder.push((n, f.max(last.1))),
            None => ladder.push((n, f)),
        }
    }
    let label = format!(
        "real moomoo paper fees ({} order(s), {:.2}-{:.2} USD notional)",
        points.len(),
        ladder[0].0,
        ladder[ladder.len() - 1].0
    );
    // Beyond the largest observed size the fee is unmeasured. Do not append a synthetic
    // endpoint: Rules::fee must return None so the caller refuses to size that order.
    (ladder, label)
}

// ---------------------------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------------------------

/// Requests per 30 seconds this client allows itself per protocol (below moomoo's limits).
fn rate_limit(proto: u32) -> usize {
    match proto {
        id::QOT_REQUEST_HISTORY_KL => 50,
        id::QOT_GET_SECURITY_SNAPSHOT => 50,
        id::TRD_PLACE_ORDER => 12,
        id::TRD_MODIFY_ORDER => 15,
        id::KEEP_ALIVE | id::INIT_CONNECT | id::GET_GLOBAL_STATE => 1000,
        _ => 9,
    }
}

struct Conn {
    c: Option<OpenD>,
    sent: HashMap<u32, VecDeque<Instant>>,
}

pub struct Gateway {
    conn: tokio::sync::Mutex<Conn>,
    /// The US paper account (SIMULATE), the only account orders go to.
    pub account: u64,
    pub addr: String,
    client_id: String,
    /// Last successful OpenD reply.
    last_ok: std::sync::Mutex<Option<Instant>>,
}

macro_rules! req {
    ($m:ident { $($f:ident : $v:expr),* $(,)? }) => {
        pb::$m::Request { c2s: pb::$m::C2s { $($f: $v),*, ..Default::default() } }
    };
}

impl Gateway {
    /// Connect to OpenD, check it is logged in, and select the US paper account.
    pub async fn connect(client_id: &str) -> Result<Gateway> {
        let addr = std::env::var("OPEND").unwrap_or_else(|_| DEFAULT_ADDR.into());
        let mut g = Gateway {
            conn: tokio::sync::Mutex::new(Conn {
                c: None,
                sent: HashMap::new(),
            }),
            account: 0,
            addr,
            client_id: client_id.into(),
            last_ok: std::sync::Mutex::new(None),
        };
        let st = g
            .call::<pb::get_global_state::Response>(
                id::GET_GLOBAL_STATE,
                &req!(get_global_state { user_id: 0 }),
            )
            .await?;
        ensure!(
            st.qot_logined && st.trd_logined,
            "OpenD is not logged in (quotes {}, trading {})",
            st.qot_logined,
            st.trd_logined
        );
        let accs = g
            .call::<pb::trd_get_acc_list::Response>(
                id::TRD_GET_ACC_LIST,
                &req!(trd_get_acc_list {
                    user_id: 0,
                    trd_category: Some(1),
                    need_general_sec_account: Some(true)
                }),
            )
            .await?;
        g.account = select_paper_account(
            &accs.acc_list,
            std::env::var("MOOMOO_PAPER_ACCOUNT")
                .ok()
                .and_then(|v| v.trim().parse().ok()),
        )?;
        Ok(g)
    }

    /// Trading header for the US SIMULATE (paper) account; every trading request uses it.
    fn header(&self) -> pb::trd_common::TrdHeader {
        pb::trd_common::TrdHeader {
            trd_env: TRD_ENV_SIMULATE,
            acc_id: self.account,
            trd_market: TRD_US,
            jp_acc_type: None,
        }
    }

    /// One request (rate-limited per protocol); connects on demand. A transport failure drops the
    /// connection so the next call reconnects; a refusal from OpenD keeps it.
    pub async fn call<R: crate::opend::Reply>(
        &self,
        proto: u32,
        req: &impl prost::Message,
    ) -> Result<R::S2c> {
        let mut g = self.conn.lock().await;
        loop {
            let limit = rate_limit(proto);
            let q = g.sent.entry(proto).or_default();
            while q
                .front()
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(30))
            {
                q.pop_front();
            }
            if q.len() < limit {
                q.push_back(Instant::now());
                break;
            }
            let wait = Duration::from_secs(30)
                .saturating_sub(q.front().map_or(Duration::ZERO, |t| t.elapsed()))
                + Duration::from_millis(50);
            tokio::time::sleep(wait).await;
        }
        if g.c.is_none() {
            g.c = Some(OpenD::connect(&self.addr, &self.client_id).await?);
        }
        let c = g.c.as_mut().expect("connected above");
        match c.call::<R>(proto, req).await {
            Ok(x) => {
                *self.last_ok.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                Ok(x)
            }
            Err(e) => {
                if e.downcast_ref::<Refused>().is_some() {
                    *self.last_ok.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                } else {
                    g.c = None;
                }
                Err(e)
            }
        }
    }

    /// The keep-alive OpenD expects every `keep_alive_secs` (proto 1004).
    pub async fn keep_alive(&self) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.call::<pb::keep_alive::Response>(id::KEEP_ALIVE, &req!(keep_alive { time: now }))
            .await?;
        Ok(())
    }

    /// OpenD's keep-alive interval (seconds) of the current connection.
    pub async fn keep_alive_secs(&self) -> Option<i32> {
        self.conn.lock().await.c.as_ref().map(|c| c.keep_alive_secs)
    }

    /// Whether OpenD answers and is logged in for both quotes and trading.
    pub async fn alive(&self) -> bool {
        match self
            .call::<pb::get_global_state::Response>(
                id::GET_GLOBAL_STATE,
                &req!(get_global_state { user_id: 0 }),
            )
            .await
        {
            Ok(s) => s.qot_logined && s.trd_logined,
            Err(_) => false,
        }
    }

    /// The paper account's funds (total assets, cash, buying power, market value) now.
    pub async fn account(&self) -> Result<Account> {
        let f = self
            .call::<pb::trd_get_funds::Response>(
                id::TRD_GET_FUNDS,
                &req!(trd_get_funds {
                    header: self.header(),
                    refresh_cache: Some(true),
                    currency: None,
                    asset_category: None
                }),
            )
            .await?
            .funds
            .ok_or_else(|| anyhow!("moomoo returned no funds for the paper account"))?;
        Ok(Account {
            id: self.account,
            total_assets: f.total_assets,
            cash: f.cash,
            power: f.power,
            market_val: f.market_val,
            ts: ms_now(),
        })
    }

    /// The paper account's current positions.
    pub async fn positions(&self) -> Result<Vec<Held>> {
        let p = self
            .call::<pb::trd_get_position_list::Response>(
                id::TRD_GET_POSITION_LIST,
                &req!(trd_get_position_list {
                    header: self.header(),
                    filter_conditions: None,
                    filter_pl_ratio_min: None,
                    filter_pl_ratio_max: None,
                    refresh_cache: Some(true),
                    asset_category: None,
                }),
            )
            .await?;
        Ok(p.position_list
            .iter()
            .filter(|x| x.qty != 0.0)
            .map(|x| Held {
                symbol: x.code.clone(),
                qty: if x.position_side == 1 { -x.qty } else { x.qty },
                can_sell_qty: x.can_sell_qty,
                cost_price: x.diluted_cost_price.or(x.cost_price),
                price: x.price,
            })
            .collect())
    }

    /// moomoo's US trade dates from `from` to `to` (ET dates), with their type, in chunks (one
    /// request covers at most a year).
    async fn trade_dates(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<(NaiveDate, i32)>> {
        let mut out = Vec::new();
        let mut a = from;
        while a <= to {
            let b = (a + chrono::Duration::days(300)).min(to);
            let r = self
                .call::<pb::qot_request_trade_date::Response>(
                    id::QOT_REQUEST_TRADE_DATE,
                    &req!(qot_request_trade_date {
                        market: TRADE_DATE_US,
                        begin_time: a.to_string(),
                        end_time: b.to_string(),
                        security: None
                    }),
                )
                .await?;
            for d in r.trade_date_list {
                let date =
                    NaiveDate::parse_from_str(d.time.get(..10).unwrap_or(&d.time), "%Y-%m-%d")
                        .with_context(|| format!("bad trade date {:?}", d.time))?;
                out.push((date, d.trade_date_type.unwrap_or(TRADE_DATE_WHOLE)));
            }
            a = b + chrono::Duration::days(1);
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    /// Regular sessions from `CLOCK_EPOCH` through the coming weeks, from moomoo's calendar.
    pub async fn sessions(&self) -> Result<Sessions> {
        let epoch = NaiveDate::from_ymd_opt(CLOCK_EPOCH.0, CLOCK_EPOCH.1, CLOCK_EPOCH.2)
            .ok_or_else(|| anyhow!("bad clock epoch"))?;
        let s = self.sessions_from(epoch).await?;
        ensure!(
            s.list().first().map(|x| et_date(x.0)) == Some(epoch),
            "moomoo calendar does not start at the clock epoch {epoch}"
        );
        Ok(s)
    }

    /// Regular sessions from the first trading day on or after `from` through the coming weeks (research calendars that reach
    /// further back than the live clock epoch; the live service always uses `sessions`).
    pub async fn sessions_from(&self, from: NaiveDate) -> Result<Sessions> {
        let to = et_date(ms_now()) + chrono::Duration::days(40);
        let dates = self.trade_dates(from, to).await?;
        let list = dates
            .iter()
            .map(|&(d, k)| session_bounds(d, k))
            .collect::<Result<Vec<_>>>()?;
        Sessions::new(list)
    }

    /// The universe from a ticker list (SPY's holdings, best first): eligible listings in that order, at most `cap`, plus the excluded ones with reasons.
    pub async fn universe_from(
        &self,
        tickers: &[String],
        cap: usize,
    ) -> Result<(Vec<Listing>, Vec<(String, String)>)> {
        let info = self.static_info(tickers).await?;
        // Rank only orders the rows; the pool carries no turnover figure for these (it is not a screener result).
        let rows: Vec<(String, f64)> = tickers
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), (tickers.len() - i) as f64))
            .collect();
        let (mut pool, excluded) = screen(&rows, &info, cap);
        for l in &mut pool {
            l.avg_turnover = None;
        }
        Ok((pool, excluded))
    }

    /// Static information of `symbols` (US), keyed by symbol.
    pub async fn static_info(
        &self,
        symbols: &[String],
    ) -> Result<HashMap<String, pb::qot_common::SecurityStaticBasic>> {
        let mut out = HashMap::new();
        // Batches of 200 (moomoo's maximum per request), paced so 500+ holdings never hit the request-rate limit.
        for (k, chunk) in symbols.chunks(200).enumerate() {
            if k > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            let r = self
                .call::<pb::qot_get_static_info::Response>(
                    id::QOT_GET_STATIC_INFO,
                    &req!(qot_get_static_info {
                        market: None,
                        sec_type: None,
                        security_list: chunk
                            .iter()
                            .map(|s| pb::qot_common::Security {
                                market: QOT_US,
                                code: s.clone()
                            })
                            .collect(),
                    }),
                )
                .await?;
            for i in r.static_info_list {
                out.insert(i.basic.security.code.clone(), i.basic);
            }
        }
        Ok(out)
    }

    /// Static information of one symbol, which must be an eligible listing.
    pub async fn listing(&self, symbol: &str) -> Result<Listing> {
        let info = self.static_info(&[symbol.to_string()]).await?;
        let b = info
            .get(symbol)
            .ok_or_else(|| anyhow!("{symbol}: moomoo has no static information"))?;
        if let Some(why) = ineligible(b) {
            bail!("{symbol}: not eligible: {why}");
        }
        Ok(Listing::of(b, None))
    }

    /// The historical-candle quota now: used, remaining and which stocks were pulled when.
    pub async fn quota(&self) -> Result<QuotaState> {
        let q = self
            .call::<pb::qot_request_history_kl_quota::Response>(
                id::QOT_REQUEST_HISTORY_KL_QUOTA,
                &req!(qot_request_history_kl_quota {
                    b_get_detail: Some(true)
                }),
            )
            .await?;
        Ok(QuotaState {
            used: q.used_quota,
            remain: q.remain_quota,
            pulled: q
                .detail_list
                .into_iter()
                .map(|d| (d.security.code, d.request_time))
                .collect(),
            ts: ms_now(),
        })
    }

    /// Closed 1-minute regular-session candles of `symbol` from the ET date of `from` through
    /// today, keyed by wall OPEN time (ms). The candle still forming is dropped; blank candles are
    /// missing. Candles outside `sessions` are dropped and counted (second value).
    pub async fn bars(
        &self,
        symbol: &str,
        from: i64,
        sessions: &Sessions,
    ) -> Result<(BTreeMap<i64, Bar>, usize)> {
        // NOTE(agents): moomoo reads a date-only end time as 00:00 of that day (today's candles
        //               were missing, verified 2026-10-06): the range is given with explicit times,
        //               in US/Eastern like the candle labels.
        let begin = format!("{} 00:00:00", et_date(from));
        let end = format!("{} 23:59:59", et_date(ms_now()));
        let mut key: Option<Vec<u8>> = None;
        let mut out = BTreeMap::new();
        let mut off_grid = 0;
        let now = ms_now();
        for _ in 0..50 {
            let r = self
                .call::<pb::qot_request_history_kl::Response>(
                    id::QOT_REQUEST_HISTORY_KL,
                    &req!(qot_request_history_kl {
                        rehab_type: 0, // unadjusted (user decision)
                        kl_type: KL_1M,
                        security: pb::qot_common::Security {
                            market: QOT_US,
                            code: symbol.into()
                        },
                        begin_time: begin.clone(),
                        end_time: end.clone(),
                        max_ack_kl_num: Some(1000),
                        need_kl_fields_flag: None,
                        next_req_key: key.take(),
                        extended_time: Some(false),
                        session: None,
                    }),
                )
                .await?;
            for k in &r.kl_list {
                if k.is_blank {
                    continue;
                }
                let open = candle_open_ms(&k.time)?;
                if open + BAR_MS > now {
                    continue; // still forming
                }
                let (Some(o), Some(h), Some(l), Some(c)) =
                    (k.open_price, k.high_price, k.low_price, k.close_price)
                else {
                    bail!("{symbol}: candle {} without prices", k.time);
                };
                if sessions.clock(open).is_none() {
                    off_grid += 1;
                    continue;
                }
                out.insert(
                    open,
                    Bar {
                        open: o,
                        high: h,
                        low: l,
                        close: c,
                        volume: k
                            .volume
                            .ok_or_else(|| anyhow!("{symbol}: candle {} without volume", k.time))?
                            as f64,
                        turnover: k.turnover.ok_or_else(|| {
                            anyhow!("{symbol}: candle {} without turnover", k.time)
                        })?,
                    },
                );
            }
            match r.next_req_key {
                Some(k) if !k.is_empty() => key = Some(k),
                _ => return Ok((out, off_grid)),
            }
        }
        bail!("{symbol}: candle paging did not end")
    }

    /// Snapshot quotes of `symbols` (bid/ask, sizes, last, lot); symbols moomoo omits are absent.
    pub async fn quotes(&self, symbols: &[String]) -> Result<HashMap<String, Quote>> {
        let mut out = HashMap::new();
        for chunk in symbols.chunks(200) {
            let r = self
                .call::<pb::qot_get_security_snapshot::Response>(
                    id::QOT_GET_SECURITY_SNAPSHOT,
                    &req!(qot_get_security_snapshot {
                        security_list: chunk
                            .iter()
                            .map(|s| pb::qot_common::Security {
                                market: QOT_US,
                                code: s.clone()
                            })
                            .collect(),
                    }),
                )
                .await?;
            for s in r.snapshot_list {
                let b = s.basic;
                out.insert(
                    b.security.code.clone(),
                    Quote {
                        bid: b.bid_price.unwrap_or(0.0),
                        ask: b.ask_price.unwrap_or(0.0),
                        bid_vol: b.bid_vol.unwrap_or(0) as f64,
                        ask_vol: b.ask_vol.unwrap_or(0) as f64,
                        last: b.cur_price,
                        lot: b.lot_size as f64,
                        suspended: b.is_suspend,
                    },
                );
            }
        }
        Ok(out)
    }

    /// Place a MARKET order (day, regular hours) on the paper account. `side`: 1 buy, 2 sell.
    pub async fn place_market(
        &self,
        symbol: &str,
        side: i32,
        qty: f64,
        remark: &str,
    ) -> Result<(u64, String)> {
        ensure!(
            qty > 0.0 && qty.fract() == 0.0,
            "{symbol}: order quantity {qty} is not a whole positive number"
        );
        self.place(symbol, side, qty, None, remark).await
    }

    /// Research probe (examples/fractional_check.rs): a LIMIT order on the paper account whose
    /// quantity may be fractional. The bot itself never calls this (it trades whole shares).
    pub async fn place_limit_probe(
        &self,
        symbol: &str,
        side: i32,
        qty: f64,
        price: f64,
        remark: &str,
    ) -> Result<(u64, String)> {
        ensure!(
            qty > 0.0 && price > 0.0,
            "{symbol}: quantity {qty} / price {price}"
        );
        self.place(symbol, side, qty, Some(price), remark).await
    }

    /// Market order (`price` None) or limit order at `price`, paper account only.
    async fn place(
        &self,
        symbol: &str,
        side: i32,
        qty: f64,
        price: Option<f64>,
        remark: &str,
    ) -> Result<(u64, String)> {
        ensure!(side == 1 || side == 2, "order side {side}");
        let header = self.header();
        guard_order_header(&header, self.account)?;
        let (conn_id, serial) = {
            let mut g = self.conn.lock().await;
            if g.c.is_none() {
                g.c = Some(OpenD::connect(&self.addr, &self.client_id).await?);
            }
            let c = g.c.as_mut().expect("connected above");
            (c.conn_id, c.next_serial())
        };
        let r = self
            .call::<pb::trd_place_order::Response>(
                id::TRD_PLACE_ORDER,
                &req!(trd_place_order {
                    packet_id: pb::common::PacketId {
                        conn_id,
                        serial_no: serial
                    },
                    header: header,
                    trd_side: side,
                    order_type: if price.is_some() { 1 } else { 2 }, // 1 limit, 2 market
                    code: symbol.into(),
                    qty: qty,
                    price: price,
                    adjust_price: None,
                    adjust_side_and_limit: None,
                    sec_market: Some(2),
                    remark: Some(remark.chars().take(60).collect()),
                    time_in_force: Some(0),
                    fill_outside_rth: None,
                    aux_price: None,
                    trail_type: None,
                    trail_value: None,
                    trail_spread: None,
                    session: None,
                    position_id: None,
                }),
            )
            .await?;
        let oid = r
            .order_id
            .ok_or_else(|| anyhow!("{symbol}: moomoo returned no order id"))?;
        Ok((oid, r.order_id_ex.unwrap_or_default()))
    }

    /// Cancel an open order on the paper account.
    pub async fn cancel(&self, order_id: u64) -> Result<()> {
        let header = self.header();
        guard_order_header(&header, self.account)?;
        let (conn_id, serial) = {
            let mut g = self.conn.lock().await;
            if g.c.is_none() {
                g.c = Some(OpenD::connect(&self.addr, &self.client_id).await?);
            }
            let c = g.c.as_mut().expect("connected above");
            (c.conn_id, c.next_serial())
        };
        self.call::<pb::trd_modify_order::Response>(
            id::TRD_MODIFY_ORDER,
            &req!(trd_modify_order {
                packet_id: pb::common::PacketId {
                    conn_id,
                    serial_no: serial
                },
                header: header,
                order_id: order_id,
                modify_order_op: 2, // cancel
                for_all: None,
                trd_market: None,
                qty: None,
                price: None,
                adjust_price: None,
                adjust_side_and_limit: None,
                aux_price: None,
                trail_type: None,
                trail_value: None,
                trail_spread: None,
                order_id_ex: None,
            }),
        )
        .await?;
        Ok(())
    }

    /// Today's orders of the paper account (moomoo's order list), optionally only `ids`.
    pub async fn orders(&self, ids: &[u64]) -> Result<Vec<OrderState>> {
        let r = self
            .call::<pb::trd_get_order_list::Response>(
                id::TRD_GET_ORDER_LIST,
                &req!(trd_get_order_list {
                    header: self.header(),
                    filter_conditions: (!ids.is_empty()).then(|| {
                        pb::trd_common::TrdFilterConditions {
                            id_list: ids.to_vec(),
                            ..Default::default()
                        }
                    }),
                    filter_status_list: vec![],
                    refresh_cache: Some(true),
                }),
            )
            .await?;
        Ok(r.order_list.iter().map(OrderState::of).collect())
    }

    /// Orders of earlier days (history order list) from `from_ms` on, optionally only `ids`.
    pub async fn history_orders(&self, ids: &[u64], from_ms: i64) -> Result<Vec<OrderState>> {
        let fmt = |ms: i64| {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|d| {
                    d.with_timezone(&New_York)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default()
        };
        let r = self
            .call::<pb::trd_get_history_order_list::Response>(
                id::TRD_GET_HISTORY_ORDER_LIST,
                &req!(trd_get_history_order_list {
                    header: self.header(),
                    filter_conditions: pb::trd_common::TrdFilterConditions {
                        id_list: ids.to_vec(),
                        begin_time: Some(fmt(from_ms)),
                        end_time: Some(fmt(ms_now() + 86_400_000)),
                        ..Default::default()
                    },
                    filter_status_list: vec![],
                }),
            )
            .await?;
        Ok(r.order_list.iter().map(OrderState::of).collect())
    }

    /// Fees moomoo charged the orders `ids_ex` (only those it reports).
    pub async fn order_fees(&self, ids_ex: &[String]) -> Result<Vec<OrderFee>> {
        let mut out = Vec::new();
        for chunk in ids_ex.chunks(20) {
            let r = self
                .call::<pb::trd_get_order_fee::Response>(
                    id::TRD_GET_ORDER_FEE,
                    &req!(trd_get_order_fee {
                        header: self.header(),
                        order_id_ex_list: chunk.to_vec()
                    }),
                )
                .await?;
            for f in r.order_fee_list {
                out.push(OrderFee {
                    order_id_ex: f.order_id_ex,
                    total: f
                        .fee_amount
                        .unwrap_or_else(|| f.fee_list.iter().filter_map(|i| i.value).sum()),
                    items: f
                        .fee_list
                        .iter()
                        .map(|i| (i.title.clone().unwrap_or_default(), i.value.unwrap_or(0.0)))
                        .collect(),
                });
            }
        }
        Ok(out)
    }
}

/// Average daily dollar turnover and daily return volatility (a fraction) of a symbol from its REAL 1-minute traded candles: the last
/// `sessions` days that have at least 20 of a session's 26 candles (a half day or a gap is not mistaken for a quiet day). None when fewer than 5 such days.
pub fn liquidity_from_bars(bars: &BTreeMap<i64, Bar>, sessions: usize) -> Option<(f64, f64)> {
    let mut days: BTreeMap<chrono::NaiveDate, (usize, f64, f64)> = BTreeMap::new();
    for (&wall, b) in bars {
        let d = chrono::DateTime::from_timestamp_millis(wall)?
            .with_timezone(&New_York)
            .date_naive();
        let e = days.entry(d).or_insert((0, 0.0, 0.0));
        e.0 += 1;
        e.1 += b.turnover;
        e.2 = b.close; // the last candle of the day (the map is ordered by time)
    }
    let full: Vec<(f64, f64)> = days
        .values()
        .filter(|d| d.0 >= 20 && d.1 > 0.0)
        .map(|d| (d.1, d.2))
        .collect();
    if full.len() < 5 {
        return None;
    }
    let tail = &full[full.len().saturating_sub(sessions)..];
    let adv = tail.iter().map(|d| d.0).sum::<f64>() / tail.len() as f64;
    let rets: Vec<f64> = tail
        .windows(2)
        .filter(|w| w[0].1 > 0.0)
        .map(|w| w[1].1 / w[0].1 - 1.0)
        .collect();
    if rets.len() < 4 {
        return None;
    }
    let mean = rets.iter().sum::<f64>() / rets.len() as f64;
    let vol =
        (rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (rets.len() - 1) as f64).sqrt();
    (adv.is_finite() && adv > 0.0 && vol.is_finite()).then_some((adv, vol))
}

/// Rules of `l` from a real quote and this account's real fee observations: whole shares in lots
/// of `lot_size`, the quote's displayed depth, the commission ladder (`commission_ladder`).
pub fn rules_from_quote(l: &Listing, q: &Quote, fees: &[FeePoint], now: i64) -> Result<Rules> {
    ensure!(
        q.valid(),
        "{}: no valid two-sided quote ({:?})",
        l.symbol,
        q
    );
    let book = measure_book(&[(q.bid, q.bid_vol)], &[(q.ask, q.ask_vol)]);
    ensure!(!book.is_empty(), "{}: quote too thin to measure", l.symbol);
    // NOTE(agents): Whole shares (user rule): the order step is the lot size moomoo reports (1 for
    //               US stocks), never a fraction.
    let step = l.lot_size.max(1.0).round();
    let (commission, commission_source) = commission_ladder(fees);
    Ok(Rules {
        qty_step: step,
        min_qty: step,
        min_notional: 0.0,
        max_market_qty: q.bid_vol.min(q.ask_vol),
        commission,
        commission_source,
        // NOTE(agents): User rule for this bot: no leverage. Never raise.
        max_leverage: 1.0,
        book,
        book_ts: now,
        adv_usd: None,
        daily_vol: None,
    })
}

// ---------------------------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------------------------

/// The regular-session calendar and the trading clock: every 1-minute bar of every session
/// since `CLOCK_EPOCH` gets the next clock slot, so clock time is contiguous across nights,
/// weekends and holidays (`Market::ts`).
// NOTE(agents): With an interval (`with_interval`), a "bar" here is a CANDLE of `k` 1-minute
//               bars aligned to the session open (the session's last candle may be shorter);
//               each candle takes one clock slot, so the engine's contiguous BAR_MS clock holds.
#[derive(Debug, Clone)]
pub struct Sessions {
    /// (start, end) wall ms, ascending, non-overlapping.
    list: Vec<(i64, i64)>,
    /// Clock slot of each session's first candle.
    base: Vec<i64>,
    /// 1-minute bars per candle (1 = plain 1m bars).
    k: i64,
}

impl Sessions {
    /// Session calendar of 1m candles from (open, close) wall pairs.
    pub fn new(list: Vec<(i64, i64)>) -> Result<Sessions> {
        Sessions::build(list, 1)
    }

    /// Session calendar for candles of `k` 1m bars; checks sessions are whole bars and do not overlap.
    fn build(list: Vec<(i64, i64)>, k: i64) -> Result<Sessions> {
        ensure!(!list.is_empty(), "no sessions");
        ensure!(k >= 1, "candle of {k} bars");
        let mut base = Vec::with_capacity(list.len());
        let mut slot = 0i64;
        for (i, &(a, b)) in list.iter().enumerate() {
            ensure!(
                b > a && (b - a) % BAR_MS == 0,
                "session {a}..{b} is not whole 1-minute bars"
            );
            ensure!(i == 0 || a >= list[i - 1].1, "sessions overlap");
            base.push(slot);
            slot += ((b - a) / BAR_MS + k - 1) / k;
        }
        Ok(Sessions { list, base, k })
    }

    /// The calendar without the sessions in `drop` (unscheduled closures found in the candles).
    pub fn without(&self, drop: &[(i64, i64)]) -> Result<Sessions> {
        Sessions::build(
            self.list
                .iter()
                .copied()
                .filter(|s| !drop.contains(s))
                .collect(),
            self.k,
        )
    }

    /// The sessions (wall open, close).
    pub fn list(&self) -> &[(i64, i64)] {
        &self.list
    }

    /// The same calendar with candles of `interval` (aligned to each session's open).
    pub fn with_interval(&self, interval: crate::engine::Interval) -> Sessions {
        let k = interval.bars() as i64;
        Sessions::build(self.list.clone(), k).expect("a valid calendar stays valid")
    }

    /// Candle length (ms).
    fn step(&self) -> i64 {
        self.k * BAR_MS
    }

    /// Wall close of the candle opening at `open` inside session `(a, b)`.
    fn candle_end(&self, open: i64, b: i64) -> i64 {
        (open + self.step()).min(b)
    }

    /// Clock time of the candle containing `wall`; between sessions, the next session's first
    /// candle (the clock skips the gap). Display/re-mapping only.
    pub fn clock_at(&self, wall: i64) -> i64 {
        let i = self.list.partition_point(|s| s.0 <= wall);
        if let Some(j) = i.checked_sub(1) {
            let (a, b) = self.list[j];
            if wall < b {
                return self.origin() + (self.base[j] + (wall - a) / self.step()) * BAR_MS;
            }
        }
        let slot = match self.base.get(i) {
            Some(&s) => s,
            None => {
                let (a, b) = self.list[self.list.len() - 1];
                self.base[self.list.len() - 1] + ((b - a) / BAR_MS + self.k - 1) / self.k
            }
        };
        self.origin() + slot * BAR_MS
    }

    /// Wall [open, close) of the candle opening at `open`; None off-grid or outside a session.
    pub fn candle_bounds(&self, open: i64) -> Option<(i64, i64)> {
        let i = self.list.partition_point(|s| s.0 <= open).checked_sub(1)?;
        let (a, b) = self.list[i];
        (open < b && (open - a) % self.step() == 0).then(|| (open, self.candle_end(open, b)))
    }

    /// Clock origin (ms): the epoch session's open.
    fn origin(&self) -> i64 {
        self.list[0].0
    }

    /// Trading-clock time of the bar (candle) opening at `wall`; None outside a session or off-grid.
    pub fn clock(&self, wall: i64) -> Option<i64> {
        let i = self.list.partition_point(|s| s.0 <= wall).checked_sub(1)?;
        let (a, b) = self.list[i];
        (wall < b && (wall - a) % self.step() == 0)
            .then(|| self.origin() + (self.base[i] + (wall - a) / self.step()) * BAR_MS)
    }

    /// Wall open time of the bar (candle) at trading-clock time `clock`.
    pub fn wall(&self, clock: i64) -> Option<i64> {
        let slot = (clock - self.origin()).checked_div(BAR_MS)?;
        if slot < 0 || (clock - self.origin()) % BAR_MS != 0 {
            return None;
        }
        let i = self.base.partition_point(|&b| b <= slot).checked_sub(1)?;
        let (a, b) = self.list[i];
        let w = a + (slot - self.base[i]) * self.step();
        (w < b).then_some(w)
    }

    /// How many sessions the bars opening at `walls` fall in.
    #[cfg(test)]
    pub fn sessions_spanned(&self, walls: &[i64]) -> usize {
        let mut idx: Vec<usize> = walls
            .iter()
            .filter_map(|&w| self.list.iter().position(|s| s.0 <= w && w < s.1))
            .collect();
        idx.dedup();
        idx.len()
    }

    /// Sessions from the one containing (or following) `wall` through the last one started by `now`.
    pub fn sessions_from(&self, wall: i64, now: i64) -> usize {
        self.list
            .iter()
            .filter(|s| s.1 > wall && s.0 <= now)
            .count()
    }

    /// The session containing `wall`, if the market is open then.
    pub fn open_session(&self, wall: i64) -> Option<(i64, i64)> {
        self.list
            .iter()
            .copied()
            .find(|&(a, b)| a <= wall && wall < b)
    }

    /// The next session opening after `wall`.
    pub fn next_open(&self, wall: i64) -> Option<i64> {
        self.list.iter().map(|s| s.0).find(|&a| a > wall)
    }

    /// Wall open times of the last `n` closed bars (candles) at `now`, ascending.
    pub fn last_closed_bars(&self, now: i64, n: usize) -> Vec<i64> {
        let mut out = Vec::with_capacity(n);
        'outer: for &(a, b) in self.list.iter().rev() {
            let mut t = a + ((b - a - 1) / self.step()) * self.step();
            while t >= a {
                if self.candle_end(t, b) <= now {
                    out.push(t);
                    if out.len() == n {
                        break 'outer;
                    }
                }
                t -= self.step();
            }
        }
        out.reverse();
        out
    }

    /// Wall close of the bar (candle) opening at `open` (None off-grid).
    pub fn close_of(&self, open: i64) -> Option<i64> {
        self.candle_bounds(open).map(|x| x.1)
    }

    /// The trading-clock time of the next bar (candle) to open after `now` (for live fill bounds).
    pub fn clock_now(&self, now: i64) -> Option<i64> {
        let i = self.list.partition_point(|s| s.0 <= now);
        if let Some(j) = i.checked_sub(1) {
            let (a, b) = self.list[j];
            if now < b {
                let next = a + ((now - a) / self.step() + 1) * self.step();
                return if next < b {
                    self.clock(next)
                } else {
                    self.list.get(j + 1).and_then(|s| self.clock(s.0))
                };
            }
        }
        self.list.get(i).and_then(|s| self.clock(s.0))
    }
}

/// Real data of one symbol for `build_market`.
pub struct SymbolData {
    pub symbol: String,
    pub trades: BTreeMap<i64, Bar>,
    pub listing_wall: i64,
    pub eligible: bool,
    pub rules: Option<Rules>,
}

/// One candle [a, b) from closed 1-minute bars: open of the first, extremes of all, close of the
/// last, summed volume and turnover. None unless EVERY 1-minute bar of the candle is present
/// (a gap is never papered over).
fn aggregate(bars: &BTreeMap<i64, Bar>, a: i64, b: i64) -> Option<Bar> {
    let want = ((b - a) / BAR_MS) as usize;
    let mut it = bars.range(a..b).map(|(_, x)| *x);
    let first = it.next()?;
    let mut out = first;
    let mut n = 1;
    for x in it {
        out.high = out.high.max(x.high);
        out.low = out.low.min(x.low);
        out.close = x.close;
        out.volume += x.volume;
        out.turnover += x.turnover;
        n += 1;
    }
    (n == want).then_some(out)
}

// NOTE(agents): moomoo publishes no bid/ask MIDPOINT candles. The
//               engine's mark candles are therefore the same real TRADED candles: stops, valuation
//               and the entry wick guard use traded prices (the wick guard, which compares the
//               traded open with the mark open, can then never fire). Never invented: it is the
//               only real candle series moomoo provides.
/// The market over the bars (candles of `sessions`' interval) opening at `walls` (ascending,
/// all inside sessions), built from closed 1-minute bars.
pub fn build_market(sessions: &Sessions, walls: &[i64], data: &[SymbolData]) -> Result<Market> {
    let ts: Vec<i64> = walls
        .iter()
        .map(|&w| {
            sessions
                .clock(w)
                .ok_or_else(|| anyhow!("bar {w} is outside the sessions"))
        })
        .collect::<Result<_>>()?;
    let mut m = Market {
        ts,
        wall: walls.to_vec(),
        ..Default::default()
    };
    let bounds: Vec<(i64, i64)> = walls
        .iter()
        .map(|&w| {
            sessions
                .candle_bounds(w)
                .ok_or_else(|| anyhow!("candle {w} is off the calendar grid"))
        })
        .collect::<Result<_>>()?;
    for d in data {
        // Data starting before the clock epoch counts as listed at the epoch. With candles, a
        // listing inside a candle counts from the next candle (the first complete one).
        let listing = sessions
            .clock(d.listing_wall)
            .or_else(|| (d.listing_wall < sessions.origin()).then(|| sessions.origin()))
            .or_else(|| {
                let c = sessions.clock_at(d.listing_wall);
                Some(if sessions.wall(c).is_some_and(|w| w >= d.listing_wall) {
                    c
                } else {
                    c + BAR_MS
                })
            });
        m.symbols.push(d.symbol.clone());
        m.listing_times.push(listing);
        m.entry_eligible.push(d.eligible);
        let bars: Vec<Option<Bar>> = bounds
            .iter()
            .map(|&(a, b)| aggregate(&d.trades, a, b))
            .collect();
        m.marks.push(bars.clone());
        m.bars.push(bars);
        m.funding.push(Vec::new());
        m.rules.push(d.rules.clone());
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;

    /// Test fixture: a calendar of two full sessions and one half day.
    fn cal() -> Sessions {
        // Two full days and a half day: 26 + 26 + 14 bars.
        Sessions::new(vec![
            (100 * H, 100 * H + 26 * BAR_MS),
            (124 * H, 124 * H + 26 * BAR_MS),
            (148 * H, 148 * H + 14 * BAR_MS),
        ])
        .unwrap()
    }

    /// Test fixture: an account record (id, environment, markets, simulated account type).
    fn acc(id: u64, env: i32, markets: Vec<i32>, sim: Option<i32>) -> pb::trd_common::TrdAcc {
        pb::trd_common::TrdAcc {
            trd_env: env,
            acc_id: id,
            trd_market_auth_list: markets,
            sim_acc_type: sim,
            acc_status: Some(0),
            ..Default::default()
        }
    }

    /// The accounts this OpenD login reports (probe 2026-10-06).
    fn this_login() -> Vec<pb::trd_common::TrdAcc> {
        vec![
            acc(9000000000000001, 1, vec![1, 2, 8], None),
            acc(900002, 0, vec![1], Some(1)),
            acc(900001, 0, vec![2], Some(4)),
        ]
    }

    /// Test: only the us paper account is selected and real accounts are refused.
    #[test]
    fn only_the_us_paper_account_is_selected_and_real_accounts_are_refused() {
        let paper = select_paper_account(&this_login(), None).unwrap();
        assert_eq!(paper, 900001);
        let ok = pb::trd_common::TrdHeader {
            trd_env: 0,
            acc_id: 900001,
            trd_market: 2,
            jp_acc_type: None,
        };
        assert!(guard_order_header(&ok, paper).is_ok());
        // The REAL account, even in the SIMULATE environment field, is refused; so is the REAL env.
        let real = pb::trd_common::TrdHeader {
            trd_env: 1,
            acc_id: 9000000000000001,
            trd_market: 2,
            jp_acc_type: None,
        };
        assert!(guard_order_header(&real, paper)
            .unwrap_err()
            .to_string()
            .contains("REFUSED"));
        let real_env = pb::trd_common::TrdHeader { trd_env: 1, ..ok };
        assert!(guard_order_header(&real_env, paper).is_err());
        let wrong_acc = pb::trd_common::TrdHeader {
            acc_id: 9000000000000001,
            ..ok
        };
        assert!(guard_order_header(&wrong_acc, paper).is_err());
        let hk_paper = pb::trd_common::TrdHeader {
            acc_id: 900002,
            trd_market: 1,
            ..ok
        };
        assert!(guard_order_header(&hk_paper, paper).is_err());
        // A login with only a REAL US account has no paper account to select.
        assert!(select_paper_account(&[acc(1, 1, vec![2], None)], None).is_err());
        // Two US paper accounts are ambiguous: refused, never guessed.
        let mut two = this_login();
        two.push(acc(999, 0, vec![2], Some(1)));
        assert!(
            select_paper_account(&two, None).is_err(),
            "two paper accounts and no choice: refuse"
        );
        assert_eq!(
            select_paper_account(&two, Some(999)).unwrap(),
            999,
            "an explicit choice among the US paper accounts is honoured"
        );
        assert_eq!(select_paper_account(&two, Some(900001)).unwrap(), 900001);
        assert!(
            select_paper_account(&two, Some(900002)).is_err(),
            "the HK paper account is not a US paper stock account"
        );
        assert!(
            select_paper_account(&two, Some(424242)).is_err(),
            "an unknown or non-paper account is refused even when named"
        );
    }

    /// Test: liquidity comes from real candles and ignores partial days.
    #[test]
    fn liquidity_comes_from_real_candles_and_ignores_partial_days() {
        let mut bars = BTreeMap::new();
        // 8 full days of 26 candles (turnover 100 USD each: 2600 a day), closes rising 1% a day; plus one 5-candle day that must not count
        for day in 0..9i64 {
            let open = 1_790_000_000_000i64 - (1_790_000_000_000i64 % 86_400_000)
                + day * 86_400_000
                + 14 * 3_600_000; // 14:00 UTC, inside one ET date
            let n = if day == 8 { 5 } else { 26 };
            for k in 0..n {
                let close = 100.0 * 1.01f64.powi(day as i32);
                bars.insert(
                    open + k * 60_000,
                    Bar {
                        open: close,
                        high: close,
                        low: close,
                        close,
                        volume: 1.0,
                        turnover: 100.0,
                    },
                );
            }
        }
        let (adv, vol) = liquidity_from_bars(&bars, 20).unwrap();
        assert!(
            (adv - 2600.0).abs() < 1e-9,
            "average over the full days only: {adv}"
        );
        assert!(vol < 1e-9, "a constant 1% a day has no volatility: {vol}");
        assert!(liquidity_from_bars(&BTreeMap::new(), 20).is_none());
    }

    /// Test: spy holdings file gives tickers by weight and ignores comments.
    #[test]
    fn spy_holdings_file_gives_tickers_by_weight_and_ignores_comments() {
        let p =
            std::env::temp_dir().join(format!("moomoo-holdings-test-{}.csv", std::process::id()));
        std::fs::write(&p, "# source: test\nrank,ticker,name,weight_pct\n3,C,Third Inc,1.5\n1,A,First Corp,8.2\n2,BRK.B,Berkshire,3.1\n4,,Blank,0.5\n5,X,Bad weight,n/a\n").unwrap();
        assert_eq!(
            load_holdings(&p).unwrap(),
            vec!["A", "BRK.B", "C"],
            "by weight, blanks and bad rows dropped"
        );
        std::fs::remove_file(&p).unwrap();
        assert!(
            load_holdings(std::path::Path::new("/nonexistent/holdings.csv")).is_none(),
            "no file: no universe"
        );
    }

    /// Test: ThomasAi's pick puts its symbols first, ignores stale, malformed or foreign entries, and never changes the universe.
    #[test]
    fn the_weekly_pick_orders_the_universe_and_bad_files_change_nothing() {
        let hold: Vec<String> = ["A", "B", "C", "D", "E"].map(String::from).to_vec();
        assert_eq!(
            prefer_selected(
                hold.clone(),
                &["D".into(), "ZZZ".into(), "B".into(), "D".into()]
            ),
            ["D", "B", "A", "C", "E"],
            "picked first in its order, foreign and repeated ones dropped, the rest by weight"
        );
        assert_eq!(prefer_selected(hold.clone(), &[]), hold);
        let dir =
            std::env::temp_dir().join(format!("moomoo-selection-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("selection.json");
        let now = 1_792_323_480_000_i64;
        let write = |at: i64, syms: &str| {
            std::fs::write(&f, format!(r#"{{"generated_ms":{at},"symbols":{syms}}}"#)).unwrap()
        };
        write(
            now - 86_400_000,
            r#"["NVDA","bad symbol","DROP;TABLE","AAPL"]"#,
        );
        assert_eq!(
            selection_symbols_at(&f, now).unwrap(),
            ["NVDA", "AAPL"],
            "fresh; entries that are not tickers are dropped"
        );
        write(now - 9 * 86_400_000, r#"["NVDA"]"#);
        assert!(
            selection_symbols_at(&f, now).is_none(),
            "stale after 8 days"
        );
        write(now + 3 * 86_400_000, r#"["NVDA"]"#);
        assert!(selection_symbols_at(&f, now).is_none(), "from the future");
        std::fs::write(&f, "not json").unwrap();
        assert!(selection_symbols_at(&f, now).is_none());
        write(now, "[]");
        assert!(
            selection_symbols_at(&f, now).is_none(),
            "an empty pick is no pick"
        );
        assert!(selection_symbols_at(&dir.join("missing.json"), now).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Test: candle labels are close times in us eastern.
    #[test]
    fn candle_labels_are_close_times_in_us_eastern() {
        // 2026-09-29 09:45 EDT (UTC-4) close -> 09:30 EDT open = 13:30 UTC.
        let open = candle_open_ms("2026-09-29 09:45:00").unwrap();
        assert_eq!(
            open,
            chrono::Utc
                .with_ymd_and_hms(2026, 9, 29, 13, 30, 0)
                .unwrap()
                .timestamp_millis()
        );
        // The last candle of a full session (labelled 16:00) opens 15:45.
        let last = candle_open_ms("2026-10-05 16:00:00").unwrap();
        assert_eq!(
            last,
            chrono::Utc
                .with_ymd_and_hms(2026, 10, 5, 19, 45, 0)
                .unwrap()
                .timestamp_millis()
        );
        // Winter time (EST, UTC-5).
        let w = candle_open_ms("2026-12-01 09:45:00").unwrap();
        assert_eq!(
            w,
            chrono::Utc
                .with_ymd_and_hms(2026, 12, 1, 14, 30, 0)
                .unwrap()
                .timestamp_millis()
        );
        assert!(candle_open_ms("09:45").is_err());
    }

    /// Test: sessions come from trade date types with half days.
    #[test]
    fn sessions_come_from_trade_date_types_with_half_days() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let (a, b) = session_bounds(d, 1).unwrap();
        assert_eq!((b - a) / BAR_MS, 14, "half day: 09:30-13:00");
        let (a2, b2) = session_bounds(NaiveDate::from_ymd_opt(2026, 11, 30).unwrap(), 0).unwrap();
        assert_eq!((b2 - a2) / BAR_MS, 26);
        assert_eq!(
            a,
            chrono::Utc
                .with_ymd_and_hms(2026, 11, 27, 14, 30, 0)
                .unwrap()
                .timestamp_millis(),
            "EST after the DST change"
        );
        assert!(
            session_bounds(d, 2).is_err(),
            "afternoon-only sessions do not exist in the US"
        );
    }

    /// Test: calendar is checked against the reference candles.
    #[test]
    fn calendar_is_checked_against_the_reference_candles() {
        let s = cal();
        let bar = Bar {
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
            turnover: 1.0,
        };
        let all: BTreeMap<i64, Bar> = s
            .last_closed_bars(400 * H, 66)
            .into_iter()
            .map(|w| (w, bar))
            .collect();
        let (closed, problems) = check_calendar(&s, &all, 0, 400 * H);
        assert!(closed.is_empty() && problems.is_empty(), "{problems:?}");
        // Day 2 has no candles at all: an unscheduled closure; dropping it keeps the clock contiguous.
        let mut gap = all.clone();
        gap.retain(|&w, _| !(124 * H..124 * H + 26 * BAR_MS).contains(&w));
        let (closed, problems) = check_calendar(&s, &gap, 0, 400 * H);
        assert_eq!(closed, vec![(124 * H, 124 * H + 26 * BAR_MS)]);
        assert!(problems.is_empty());
        let s2 = s.without(&closed).unwrap();
        assert_eq!(s2.last_closed_bars(400 * H, 100).len(), 40);
        // The "half day" really traded until 16:00: the calendar is wrong there.
        let mut late = all.clone();
        late.insert(148 * H + 14 * BAR_MS, bar);
        let (_, problems) = check_calendar(&s, &late, 0, 400 * H);
        assert_eq!(problems.len(), 1, "{problems:?}");
        // A session with a missing candle is reported, not hidden.
        let mut hole = all;
        hole.remove(&(100 * H + 3 * BAR_MS));
        let (_, problems) = check_calendar(&s, &hole, 0, 400 * H);
        assert_eq!(problems.len(), 1);
    }

    /// Test fixture: static info of a security (code, name, exchange, security type).
    fn basic(code: &str, name: &str, exch: i32, sec: i32) -> pb::qot_common::SecurityStaticBasic {
        pb::qot_common::SecurityStaticBasic {
            security: pb::qot_common::Security {
                market: 11,
                code: code.into(),
            },
            id: 0,
            lot_size: 1,
            sec_type: sec,
            name: name.into(),
            list_time: String::new(),
            delisting: None,
            list_timestamp: None,
            exch_type: Some(exch),
        }
    }

    /// Test: screener keeps nyse and nasdaq stocks and plain funds in rank order.
    #[test]
    fn screener_keeps_nyse_and_nasdaq_stocks_and_plain_funds_in_rank_order() {
        let rows: Vec<(String, f64)> = [
            "MU", "OTCX", "TQQQ", "SPY", "AMEX1", "AAPL", "WARR", "NOINFO", "MSFT",
        ]
        .iter()
        .enumerate()
        .map(|(i, s)| (s.to_string(), 1e9 - i as f64))
        .collect();
        let info: HashMap<String, pb::qot_common::SecurityStaticBasic> = [
            basic("MU", "Micron Technology", 5, 3),
            basic("OTCX", "Some OTC", 6, 3),
            basic("TQQQ", "ProShares UltraPro QQQ", 5, 4),
            basic("SPY", "SPDR S&P 500 ETF Trust", 4, 4),
            basic("AMEX1", "NYSE American stock", 7, 3),
            basic("AAPL", "Apple", 5, 3),
            basic("WARR", "A warrant", 5, 5),
            basic("MSFT", "Microsoft", 5, 3),
        ]
        .into_iter()
        .map(|b| (b.security.code.clone(), b))
        .collect();
        let (pool, excluded) = screen(&rows, &info, 80);
        let syms: Vec<&str> = pool.iter().map(|l| l.symbol.as_str()).collect();
        assert_eq!(syms, vec!["MU", "SPY", "AAPL", "MSFT"]);
        let why: HashMap<&str, &str> = excluded
            .iter()
            .map(|(s, w)| (s.as_str(), w.as_str()))
            .collect();
        assert!(why["OTCX"].contains("not NYSE or Nasdaq"));
        assert!(why["AMEX1"].contains("not NYSE or Nasdaq"));
        assert!(why["TQQQ"].contains("leveraged"));
        assert!(why["WARR"].contains("neither"));
        assert!(why["NOINFO"].contains("no static information"));
        // The cap keeps the best-ranked eligible rows.
        let (pool, _) = screen(&rows, &info, 2);
        assert_eq!(
            pool.iter().map(|l| l.symbol.as_str()).collect::<Vec<_>>(),
            vec!["MU", "SPY"]
        );
    }

    /// Test: leveraged and inverse funds are recognised from their names.
    #[test]
    fn leveraged_and_inverse_funds_are_recognised_from_their_names() {
        for n in [
            "DIREX DAIL SEMI BU 3X ET-USD",
            "Direxion Daily Semiconductor Bull 3X Shares",
            "ProShares UltraPro QQQ",
            "ProShares UltraPro Short QQQ",
            "PROSHARES SHORT S&P500",
            "T-Rex 2X Long NVIDIA Daily Target ETF",
        ] {
            assert!(leveraged_or_inverse(n), "{n}");
        }
        for n in [
            "Invesco QQQ Trust",
            "SPDR S&P 500 ETF Trust",
            "iShares Semiconductor ETF",
            "Vanguard S&P 500 ETF",
            "Prologis",
        ] {
            assert!(!leveraged_or_inverse(n), "{n}");
        }
    }

    /// Test: quota guard never pulls a new symbol beyond the quota.
    #[test]
    fn quota_guard_never_pulls_a_new_symbol_beyond_the_quota() {
        let q = QuotaState {
            used: 94,
            remain: 6,
            pulled: [("AAPL".to_string(), "2026-10-06 20:26:37".to_string())]
                .into_iter()
                .collect(),
            ts: 0,
        };
        assert!(
            quota_allows(&q, "AAPL", QUOTA_RESERVE),
            "re-pulling a symbol in the window is free"
        );
        assert!(
            !quota_allows(&q, "MSFT", QUOTA_RESERVE),
            "a new candidate must leave the reserve"
        );
        assert!(
            quota_allows(&q, crate::engine::REFERENCE, 0),
            "the reference may use the reserve"
        );
        let empty = QuotaState {
            remain: 0,
            ..q.clone()
        };
        assert!(
            !quota_allows(&empty, crate::engine::REFERENCE, 0),
            "nothing new once the quota is used up"
        );
        assert!(quota_allows(&empty, "AAPL", 0));
    }

    /// Test: fees are the cash change beyond the fill value.
    #[test]
    fn fees_are_the_cash_change_beyond_the_fill_value() {
        // Real paper orders 2026-10-06: buy 3 INTC @ 117.395, sell 3 @ 117.435.
        assert!((derived_fee(1, 3.0 * 117.395, 999997.415, 999644.24) - 0.99).abs() < 1e-6);
        assert!((derived_fee(2, 3.0 * 117.435, 999644.24, 999995.545) - 1.0).abs() < 1e-6);
    }

    /// Test: commission comes from real fees or is excluded.
    #[test]
    fn commission_comes_from_real_fees_or_is_excluded() {
        let (c, src) = commission_ladder(&[]);
        assert!(src.starts_with("excluded"));
        assert!(c.iter().all(|x| x.1 == 0.0));
        let (c, src) =
            commission_ladder(&[(500.0, 1.0), (100.0, 0.99), (500.0, 1.02), (900.0, 1.01)]);
        assert!(src.starts_with("real moomoo paper fees"));
        assert_eq!(c[0], (100.0, 0.99));
        assert_eq!(c[1], (500.0, 1.02), "the larger fee at equal sizes");
        assert_eq!(c[2], (900.0, 1.02), "never decreasing with size");
        assert_eq!(c.len(), 3, "the ladder contains only measured sizes");
    }

    #[test]
    fn order_final_uses_the_protocol_enum_and_fails_closed() {
        for status in [
            pb::trd_common::OrderStatus::SubmitFailed,
            pb::trd_common::OrderStatus::FilledAll,
            pb::trd_common::OrderStatus::CancelledPart,
            pb::trd_common::OrderStatus::CancelledAll,
            pb::trd_common::OrderStatus::Failed,
            pb::trd_common::OrderStatus::Disabled,
            pb::trd_common::OrderStatus::Deleted,
            pb::trd_common::OrderStatus::FillCancelled,
        ] {
            assert!(order_final(status as i32), "{status:?} must be terminal");
        }
        for status in [
            pb::trd_common::OrderStatus::Unknown,
            pb::trd_common::OrderStatus::WaitingSubmit,
            pb::trd_common::OrderStatus::Submitting,
            pb::trd_common::OrderStatus::Submitted,
            pb::trd_common::OrderStatus::FilledPart,
            pb::trd_common::OrderStatus::CancellingPart,
            pb::trd_common::OrderStatus::CancellingAll,
        ] {
            assert!(!order_final(status as i32), "{status:?} must remain open");
        }
        assert!(
            !order_final(i32::MAX),
            "unknown future statuses must fail closed"
        );
    }

    /// Test: rules need a real two sided quote.
    #[test]
    fn rules_need_a_real_two_sided_quote() {
        let l = Listing {
            symbol: "X".into(),
            name: "X".into(),
            sec_type: 3,
            exch_type: 5,
            lot_size: 1.0,
            avg_turnover: None,
        };
        let q = Quote {
            bid: 99.9,
            ask: 100.1,
            bid_vol: 300.0,
            ask_vol: 200.0,
            last: 100.0,
            lot: 1.0,
            suspended: false,
        };
        let r = rules_from_quote(&l, &q, &[], 1).unwrap();
        assert_eq!(
            (r.qty_step, r.min_qty, r.max_market_qty, r.max_leverage),
            (1.0, 1.0, 200.0, 1.0)
        );
        assert!(r.book_cost(50.0, true).is_some());
        assert_eq!(r.order_qty(2.7, 100.0), Some(2.0), "whole shares");
        let crossed = Quote { bid: 100.2, ..q };
        assert!(rules_from_quote(&l, &crossed, &[], 1).is_err());
        let no_bid = Quote { bid: 0.0, ..q };
        assert!(rules_from_quote(&l, &no_bid, &[], 1).is_err());
    }

    /// Test: clock is contiguous across nights and half days.
    #[test]
    fn clock_is_contiguous_across_nights_and_half_days() {
        let s = cal();
        let walls = s.last_closed_bars(200 * H, 66);
        assert_eq!(walls.len(), 66);
        let clocks: Vec<i64> = walls.iter().map(|&w| s.clock(w).unwrap()).collect();
        assert!(clocks.windows(2).all(|w| w[1] - w[0] == BAR_MS));
        for (&w, &c) in walls.iter().zip(&clocks) {
            assert_eq!(s.wall(c), Some(w));
        }
        assert_eq!(s.clock(100 * H + 26 * BAR_MS), None, "after the close");
        assert_eq!(s.clock(100 * H + 1), None, "off the 1-minute grid");
    }

    /// Test: only closed bars and session state.
    #[test]
    fn only_closed_bars_and_session_state() {
        let s = cal();
        let now = 124 * H + 2 * BAR_MS + 60_000;
        let walls = s.last_closed_bars(now, 3);
        assert_eq!(
            walls,
            vec![100 * H + 25 * BAR_MS, 124 * H, 124 * H + BAR_MS]
        );
        assert!(s.open_session(now).is_some());
        assert!(s.open_session(110 * H).is_none());
        assert_eq!(s.next_open(110 * H), Some(124 * H));
        assert_eq!(s.clock_now(now), s.clock(124 * H + 3 * BAR_MS));
        assert_eq!(
            s.clock_now(110 * H),
            s.clock(124 * H),
            "overnight: the next open"
        );
        assert_eq!(s.sessions_spanned(&s.last_closed_bars(now, 3)), 2);
        assert_eq!(s.sessions_spanned(&s.last_closed_bars(200 * H, 14)), 1);
    }

    /// Test: candles align to each session open with a short last candle.
    #[test]
    fn candles_align_to_each_session_open_with_a_short_last_candle() {
        let s = cal().with_interval(crate::engine::Interval::H1);
        // 26 bars -> 7 candles (6 x 1h + 30 min); the 14-bar half day -> 4 candles (3 x 1h + 30 min).
        let all = s.last_closed_bars(400 * H, 100);
        assert_eq!(all.len(), 7 + 7 + 4);
        assert_eq!(
            s.candle_bounds(100 * H + 24 * BAR_MS),
            Some((100 * H + 24 * BAR_MS, 100 * H + 26 * BAR_MS))
        );
        assert_eq!(
            s.candle_bounds(100 * H + 2 * BAR_MS),
            None,
            "off the 1h grid"
        );
        let clocks: Vec<i64> = all.iter().map(|&w| s.clock(w).unwrap()).collect();
        assert!(
            clocks.windows(2).all(|w| w[1] - w[0] == BAR_MS),
            "one clock slot per candle"
        );
        assert!(all.iter().zip(&clocks).all(|(&w, &c)| s.wall(c) == Some(w)));
        // A candle is closed only when its last 1m bar is.
        let open = 100 * H + 4 * BAR_MS;
        assert_eq!(s.last_closed_bars(open + 4 * BAR_MS - 1, 1), vec![100 * H]);
        assert_eq!(s.last_closed_bars(open + 4 * BAR_MS, 1), vec![open]);
        assert_eq!(s.clock_now(open + 1), s.clock(open + 4 * BAR_MS));
        // One-session candles: one per session; clock_at maps any time inside it to the candle.
        let d = cal().with_interval(crate::engine::Interval::D1);
        assert_eq!(
            d.last_closed_bars(400 * H, 10),
            vec![100 * H, 124 * H, 148 * H]
        );
        assert_eq!(d.clock_at(124 * H + 7 * BAR_MS), d.clock(124 * H).unwrap());
        assert_eq!(
            d.clock_at(110 * H),
            d.clock(124 * H).unwrap(),
            "between sessions: the next candle"
        );
    }

    /// Test: candles aggregate complete 1m bars only.
    #[test]
    fn candles_aggregate_complete_1m_bars_only() {
        let s = cal();
        let h = s.with_interval(crate::engine::Interval::H1);
        let bar = |o: f64| Bar {
            open: o,
            high: o + 2.0,
            low: o - 1.0,
            close: o + 1.0,
            volume: 10.0,
            turnover: 100.0,
        };
        let walls = s.last_closed_bars(400 * H, 66);
        let mut trades: BTreeMap<i64, Bar> = walls
            .iter()
            .enumerate()
            .map(|(i, &w)| (w, bar(100.0 + i as f64)))
            .collect();
        trades.remove(&(124 * H + 5 * BAR_MS)); // a gap inside day 2's second candle
        let d = SymbolData {
            symbol: "X".into(),
            trades,
            listing_wall: 100 * H,
            eligible: true,
            rules: None,
        };
        let candles = h.last_closed_bars(400 * H, 18);
        let m = build_market(&h, &candles, &[d]).unwrap();
        let c0 = m.bars[0][0].unwrap();
        assert_eq!(
            (c0.open, c0.high, c0.low, c0.close, c0.volume, c0.turnover),
            (100.0, 105.0, 99.0, 104.0, 40.0, 400.0)
        );
        let last_of_day1 = m.bars[0][6].unwrap();
        assert_eq!(
            (last_of_day1.open, last_of_day1.close, last_of_day1.volume),
            (124.0, 126.0, 20.0),
            "30-minute close candle"
        );
        assert!(
            m.bars[0][8].is_none(),
            "a candle with a missing 1m bar is missing, never filled"
        );
        assert!(m.bars[0][7].is_some() && m.bars[0][9].is_some());
        assert!(m.ts.windows(2).all(|w| w[1] - w[0] == BAR_MS));
        assert_eq!(
            m.marks, m.bars,
            "marks are the traded candles (moomoo has no midpoint candles)"
        );
    }
}

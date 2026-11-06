//! Cross-sectional portfolio over the top-20 USDT perpetuals (live: long only).
//! Every `hold` bars (aligned to timestamps, so backtest and live rebalance at
//! the same closes) rank the universe by a causal `Signal` (screener scores,
//! return or settled funding), then hold LONG the `top` lowest and SHORT the
//! `top` highest in research's neutral mode. Close-based decisions fill no
//! earlier than t+2's open; rebalances close and reopen the basket. Isolated margin per position, real
//! funding, optional protective stop, pessimistic intrabar order.

use super::metrics::{Metrics, Trade};
use super::scores::Score;
use super::{Market, Side, BAR_MS, DAY_BARS, REFERENCE};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// NOTE(agents): User requirement: below 5 USDT free (after margin committed ANYWHERE on the real
//               account, see `reserved`) nothing is opened or added; exits always run. Checked once
//               per rebalance and per add, not per leg, or a 10 USDT account could never open its
//               second leg.
/// No new position or add is placed while the free balance (after every
/// posted margin, including margin other positions hold on the real account)
/// is below this many USDT.
pub const MIN_ENTRY_BALANCE: f64 = 5.0;
/// Long and short gross notional may differ by at most this fraction.
pub const NEUTRAL_TOLERANCE: f64 = 0.02;
// NOTE(agents): User rule (2026-10-03): "if drawdown goes beyond 25% then it should stop out".
//               At every 1m close, marked equity >= this % below its peak closes everything at
//               causal future open and blocks entries/adds until exits finish and the one-bar
//               cooldown expires. Re-entry still requires every entry gate. A crash inside one
//               candle can overshoot before the next open. The search's safety rule uses the
//               same value (walkforward::MAX_DRAWDOWN_PCT).
/// Account drawdown stop, % below the peak of marked equity.
pub const DRAWDOWN_STOP_PCT: f64 = 25.0;
/// After a drawdown exit, wait one bar, finish all exits and revalidate entry
/// gates. Re-entry is evaluated immediately rather than waiting a long hold.
pub const DRAWDOWN_COOLDOWN_MS: i64 = BAR_MS;
// NOTE(agents): Decision latency, identical in backtests, the search, the forward chart and paper
//               (user: fix timing before relying on paper P&L or optimizer results). Bar t is
//               processed after its close, so live a decision taken at that close can first fill
//               at bar t+2's open. Every decision (rebalance, close-based exit, add, breaker or
//               drawdown flatten) is therefore stamped with a bound of its close + one more bar
//               at the moment it is decided. Never stamp already-pending items at later bars:
//               that slides due actions forward forever. Live, `defer_new_decisions(now)` raises
//               bounds to the next open after `now` (nothing can fill at a past open). Resting
//               orders (intrabar stops, liquidation, live WebSocket exits) are not delayed.
/// Open time of the first bar a decision taken at the close of the bar opening
/// at `bar_ts` may fill at: the open two bars later.
fn decision_fill_bound(bar_ts: i64) -> i64 {
    bar_ts + 2 * BAR_MS
}
// NOTE(agents): Entry wick guard (user request, 2026-10-03). Entries are market orders at an open
//               and fill at TRADED prices (never at the mark: no exchange fills there). If the
//               traded open is more than this % away from the mark open, that open is a wick:
//               the entry (or add) waits for the next open instead of filling into the spike.
//               The rebalance's decision, targets and slot budget are kept while it waits.
/// Maximum gap between the traded open and the mark open for an entry to fill.
pub const ENTRY_WICK_GUARD_PCT: f64 = 0.5;

/// True when bar t's traded open is within `ENTRY_WICK_GUARD_PCT` of its mark
/// open (both must exist).
// NOTE(agents): User rule (2026-10-04): "whatever take-profit it decides, only use 70% of that
//               value." The take-profit the search chooses (fixed % or ATR multiple) is the DECIDED
//               value; the engine exits at this fraction of it, in every backtest and in the search,
//               so selection already accounts for it. The console shows both numbers.
pub const TP_USE_FRACTION: f64 = 0.7;

/// A distance in %: `mult` ATRs when both the multiple and the coin's ATR are known, else `fixed`.
fn eff_pct(fixed: Option<f64>, mult: Option<f64>, atr_pct: f64) -> Option<f64> {
    match mult {
        Some(k) if atr_pct > 0.0 => Some(k * atr_pct),
        _ => fixed,
    }
}

/// Mean true range of the 14 bars before `t`, in % of the previous close; 0 if any bar is missing.
pub fn atr_pct(m: &Market, sym: usize, t: usize) -> f64 {
    const N: usize = 14;
    if t < N + 1 {
        return 0.0;
    }
    let mut sum = 0.0;
    for k in t - N..t {
        let (Some(b), Some(prev)) = (m.bars[sym][k], m.bars[sym][k - 1]) else {
            return 0.0;
        };
        sum += (b.high - b.low)
            .max((b.high - prev.close).abs())
            .max((b.low - prev.close).abs());
    }
    let last = m.bars[sym][t - 1].map_or(0.0, |b| b.close);
    if last > 0.0 {
        sum / N as f64 / last * 100.0
    } else {
        0.0
    }
}

/// Whether the traded open is within the entry wick guard of the mark open.
fn open_is_fair(m: &Market, sym: usize, t: usize) -> bool {
    match (m.bars[sym][t], m.marks[sym][t]) {
        (Some(b), Some(mark)) => (b.open / mark.open - 1.0).abs() * 100.0 <= ENTRY_WICK_GUARD_PCT,
        _ => false,
    }
}
// NOTE(agents): Slots are sized at the decision close from marked equity; closing the old positions
//               costs fees and book cost before the new ones fill. Without this headroom the last
//               leg didn't fit and the whole rebalance was rejected (919 times in 192 real-data
//               runs).
/// Research switches for examples/oos_study.rs (production: all off).
pub mod research {
    use std::sync::atomic::AtomicBool;
    /// A rebalance keeps positions still in the new basket instead of selling and rebuying them.
    pub static KEEP_ON_REBALANCE: AtomicBool = AtomicBool::new(false);
    /// An entry needs a take-profit whose used part covers its round trip's fees and spread.
    pub static FEE_GATE: AtomicBool = AtomicBool::new(false);
    /// While the portfolio is flat, an entry decision is taken at EVERY closed bar instead of only at hold-aligned bars.
    pub static FLAT_ENTER: AtomicBool = AtomicBool::new(false);
    /// Research only (owner 2026-10-13): the lab's machine-learning score is a ranking signal (`Signal::Ml`), read from the file named by ML_SCORES. The live bot never sets this.
    pub static ML_SIGNAL: AtomicBool = AtomicBool::new(false);
    /// Research only: read the lab's score file when scoring bars (set once by a study that uses `sig:Ml`, never reset, so rescoring keeps the scores).
    pub static ML_SCORES_ON: AtomicBool = AtomicBool::new(false);
    /// Research only: an entry is blocked when the stock's machine-learning score is below this percentile of the universe at that bar (-1 = off; no score = allowed).
    pub static ML_GATE_PCTL: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
    /// Reads a research switch.
    pub fn on(x: &AtomicBool) -> bool {
        x.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Share of the free balance a rebalance commits: the rest covers closing the
/// outgoing positions (fees, book cost) between the decision and the fill.
const SLOT_HEADROOM: f64 = 0.99;
/// No entry is sized below this share of its slot (user 2026-10-09: with a 5000 USD account the minimum trade starts at 50%).
pub const MIN_SIZE_FRACTION: f64 = 0.5;

/// What the universe is ranked by at each rebalance close. All are causal:
/// screener scores from closed bars up to t, funding already settled by bar t.
/// LONG the `top` lowest values, SHORT the `top` highest (`flip` reverses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Signal {
    /// Return over `lookback` bars (unflipped = reversal, flipped = momentum).
    #[default]
    Return,
    /// Screener trend score 1-100, bearish to bullish.
    TrendScore,
    /// Screener directional moving-average price-action percentile (bearish to bullish).
    PriceAction,
    /// Screener multi-interval volatility score (unflipped = low-vol long).
    Volatility,
    /// RSI14 (unflipped = oversold long, overbought short).
    Rsi,
    /// Screener Pulse: pulse_long - pulse_short.
    Pulse,
    /// Average hourly funding rate of the last 3 settlements (unflipped = carry:
    /// long the most negative, short the most positive, paid on both legs).
    Funding,
    // NOTE(agents): The one signal with pre-registered out-of-sample evidence (docs/validation.md,
    //               commits 5329e48/6988079); the live search may pick any of the 8 signals. That
    //               holdout is spent: do NOT re-tune CalmDip's definition on windows -3..0. Any
    //               change needs a fresh pre-registered holdout (windows before 2026-06-25).
    /// Mean of the volatility and 24h-return percentiles (unflipped = long the
    /// calmest coins that fell most). Both effects held in every research
    /// window on the full historical universe (examples/signal_ic.rs).
    CalmDip,
    // NOTE(agents): Research only. The lab's own machine-learning score (a ridge model refitted every 14 sessions on OLDER data only, scored for every closed bar,
    //               `scores::ml`). Never drawn unless `research::ML_SIGNAL` is on, so the live search and the live bot are unchanged.
    /// The lab's causal machine-learning score for the next session's relative return (unflipped = long the lowest, flipped = the highest).
    Ml,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct XsParams {
    #[serde(default)]
    pub signal: Signal,
    #[serde(default)]
    pub flip: bool,
    pub lookback: usize,
    pub hold: usize,
    pub top: usize,
    /// Leverage of each position. Each of the 2 * `top` slots gets a budget of
    /// equity / slots (top in long-only, 2 * top in neutral mode)
    /// for margin + entry fee, so total exposure is about equity * gross_leverage.
    pub gross_leverage: f64,
    /// Intrabar stop: close the moment price touches this % against the entry.
    pub stop_pct: Option<f64>,
    /// Drawdown handling (all off by default; see `Risk`).
    #[serde(default)]
    pub risk: Risk,
    // NOTE(agents): The user wants LONG ONLY live. Market-neutral (long_only = false) stays for
    //               research comparison only; shorting the strongest coins got squeezed into
    //               liquidations on real data.
    /// Long only: hold the `top` lowest values (after `flip`), never short.
    #[serde(default)]
    pub long_only: bool,
    /// Entry filter: new positions only while it allows (see `Regime`).
    #[serde(default)]
    pub regime: Regime,
}

/// Market filter on entries, decided at the rebalance close. When it blocks,
/// the rebalance holds nothing (existing positions are closed as usual).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Regime {
    #[default]
    Off,
    /// The reference market (`REFERENCE`, SPY) closes above its average close of the last `REGIME_BARS` bars.
    MarketTrend,
}

// NOTE(agents): Trading-time windows (user decision 2026-10-05, "strictly 15 minute candle").
//               Windows that fit inside one regular session keep the 1-minute
//               bar counts (1 h = 4 bars); a 24 h window becomes one trading day (DAY_BARS).
/// 1 h, 4 h and 6 h of 1-minute candles, and one trading day.
const H1: usize = 4;
const H4: usize = 16;
const H6: usize = 24;
const H24: usize = DAY_BARS;
/// History a loss-zone test needs: a trading day plus 6 h.
const ZONE_HISTORY: usize = H24 + H6;

/// One trading day of 1m bars: the trend filter's default averaging window.
pub const REGIME_BARS: usize = H24;

// NOTE(agents): Missing SPY history returns false (no entries) on purpose: never guess market
//               state. The SPY filter is NOT covered by the CalmDip holdout proof; it is the only
//               market-timing element.
/// Whether `r` allows entries at bar t. A missing SPY history blocks entries.
pub fn regime_allows(m: &Market, t: usize, r: Regime) -> bool {
    regime_allows_with(m, t, r, REGIME_BARS, 0.0)
}

/// `regime_allows` with an explicit averaging window and up-trend margin (% above the average).
pub fn regime_allows_with(m: &Market, t: usize, r: Regime, bars: usize, margin_pct: f64) -> bool {
    match r {
        Regime::Off => true,
        Regime::MarketTrend => {
            let bars = bars.max(2);
            let Some(spy) = m.symbols.iter().position(|s| s == REFERENCE) else {
                return false;
            };
            if t + 1 < bars {
                return false;
            }
            let closes: Option<Vec<f64>> = (t + 1 - bars..=t)
                .map(|i| m.bars[spy][i].map(|b| b.close))
                .collect();
            closes.is_some_and(|c| {
                c[c.len() - 1] > c.iter().sum::<f64>() / c.len() as f64 * (1.0 + margin_pct / 100.0)
            })
        }
    }
}

/// Volume / buy-sell-pressure and breadth confirmations of the trend (all must pass when set).
fn pressure_ok(m: &Market, t: usize, spy: usize, bars: usize, r: &Risk) -> bool {
    let bar = |sym: usize, i: usize| m.bars[sym][i];
    if let Some(n) = r.cmf_bars {
        let n = n.max(2);
        if t + 1 < n + 1 {
            return false;
        }
        let (mut flow, mut vol) = (0.0, 0.0);
        for i in t + 1 - n..=t {
            let Some(b) = bar(spy, i) else {
                return false;
            };
            let range = b.high - b.low;
            let clv = if range > 0.0 {
                ((b.close - b.low) - (b.high - b.close)) / range
            } else {
                0.0
            };
            flow += clv * b.volume;
            vol += b.volume;
        }
        if vol <= 0.0 || flow / vol <= r.cmf_min.unwrap_or(0.0) {
            return false;
        }
    }
    if let Some(n) = r.vwap_bars {
        let n = n.max(2);
        if t + 1 < n {
            return false;
        }
        let (mut turnover, mut vol) = (0.0, 0.0);
        for i in t + 1 - n..=t {
            let Some(b) = bar(spy, i) else {
                return false;
            };
            turnover += b.turnover;
            vol += b.volume;
        }
        let Some(last) = bar(spy, t) else {
            return false;
        };
        if vol <= 0.0 || last.close <= turnover / vol {
            return false;
        }
    }
    if let Some(n) = r.obv_bars {
        let n = n.max(2);
        if t < n {
            return false;
        }
        let mut obv = 0.0;
        for i in t + 1 - n..=t {
            let (Some(b), Some(prev)) = (bar(spy, i), bar(spy, i - 1)) else {
                return false;
            };
            obv += (b.close - prev.close).signum() * b.volume;
        }
        if obv <= 0.0 {
            return false;
        }
    }
    if let Some(min) = r.breadth_min_pct {
        if t + 1 < bars {
            return false;
        }
        let (mut up, mut seen) = (0usize, 0usize);
        for sym in 0..m.symbols.len() {
            let closes: Option<Vec<f64>> = (t + 1 - bars..=t)
                .map(|i| bar(sym, i).map(|b| b.close))
                .collect();
            if let Some(c) = closes {
                seen += 1;
                up += usize::from(c[c.len() - 1] > c.iter().sum::<f64>() / c.len() as f64);
            }
        }
        if seen == 0 || (up as f64 / seen as f64 * 100.0) < min {
            return false;
        }
    }
    true
}

/// Weighted moving average of the last `n` values (newest weighted most).
fn wma(v: &[f64], n: usize) -> f64 {
    let w = &v[v.len() - n..];
    let total: f64 = (1..=n).map(|k| k as f64).sum();
    w.iter()
        .enumerate()
        .map(|(i, x)| x * (i + 1) as f64)
        .sum::<f64>()
        / total
}

/// The trend line of `kind` over `closes` (oldest first, at least `bars` long).
fn trend_line(kind: TrendKind, closes: &[f64], bars: usize) -> Option<f64> {
    let n = bars.max(2);
    if closes.len() < n {
        return None;
    }
    let tail = &closes[closes.len() - n..];
    Some(match kind {
        TrendKind::Sma => tail.iter().sum::<f64>() / n as f64,
        TrendKind::Ema => {
            let a = 2.0 / (n as f64 + 1.0);
            closes
                .iter()
                .skip(1)
                .fold(closes[0], |e, c| e + a * (c - e))
        }
        TrendKind::Hma => {
            let half = (n / 2).max(1);
            let sq = ((n as f64).sqrt().round() as usize).max(1);
            if closes.len() < n + sq {
                return None;
            }
            let raw: Vec<f64> = (n..=closes.len())
                .map(|end| 2.0 * wma(&closes[..end], half) - wma(&closes[..end], n))
                .collect();
            if raw.len() < sq {
                return None;
            }
            wma(&raw, sq)
        }
        TrendKind::Kama => {
            let (fast, slow) = (2.0 / 3.0, 2.0 / 31.0);
            let mut k = closes[0];
            for i in 1..closes.len() {
                let lo = i.saturating_sub(n);
                let change = (closes[i] - closes[lo]).abs();
                let path: f64 = (lo + 1..=i)
                    .map(|j| (closes[j] - closes[j - 1]).abs())
                    .sum();
                let er = if path > 0.0 { change / path } else { 0.0 };
                let sc = (er * (fast - slow) + slow).powi(2);
                k += sc * (closes[i] - k);
            }
            k
        }
        TrendKind::Donchian => {
            let hi = tail.iter().copied().fold(f64::MIN, f64::max);
            let lo = tail.iter().copied().fold(f64::MAX, f64::min);
            (hi + lo) / 2.0
        }
    })
}

/// Kaufman's efficiency ratio of the last `n` closes: net move / path length (0-1).
fn efficiency_ratio(closes: &[f64], n: usize) -> Option<f64> {
    if closes.len() <= n {
        return None;
    }
    let w = &closes[closes.len() - n - 1..];
    let path: f64 = w.windows(2).map(|p| (p[1] - p[0]).abs()).sum();
    Some(if path > 0.0 {
        (w[n] - w[0]).abs() / path
    } else {
        0.0
    })
}

/// Wilder's ADX(14) at bar t from `m`'s bars of `sym`; None without 120 complete bars.
fn adx14(m: &Market, sym: usize, t: usize) -> Option<f64> {
    const N: usize = 14;
    const LOOK: usize = 120;
    if t < LOOK {
        return None;
    }
    let bars: Option<Vec<_>> = (t - LOOK..=t).map(|i| m.bars[sym][i]).collect();
    let bars = bars?;
    let (mut tr_s, mut pdm_s, mut ndm_s) = (0.0, 0.0, 0.0);
    let mut dx: Vec<f64> = Vec::new();
    for i in 1..bars.len() {
        let (b, p) = (bars[i], bars[i - 1]);
        let up = b.high - p.high;
        let dn = p.low - b.low;
        let tr = (b.high - b.low)
            .max((b.high - p.close).abs())
            .max((b.low - p.close).abs());
        let (pdm, ndm) = (
            if up > dn && up > 0.0 { up } else { 0.0 },
            if dn > up && dn > 0.0 { dn } else { 0.0 },
        );
        if i <= N {
            tr_s += tr;
            pdm_s += pdm;
            ndm_s += ndm;
        } else {
            tr_s = tr_s - tr_s / N as f64 + tr;
            pdm_s = pdm_s - pdm_s / N as f64 + pdm;
            ndm_s = ndm_s - ndm_s / N as f64 + ndm;
        }
        if i >= N && tr_s > 0.0 {
            let (pdi, ndi) = (100.0 * pdm_s / tr_s, 100.0 * ndm_s / tr_s);
            dx.push(if pdi + ndi > 0.0 {
                100.0 * (pdi - ndi).abs() / (pdi + ndi)
            } else {
                0.0
            });
        }
    }
    if dx.len() < N {
        return None;
    }
    let mut adx = dx[..N].iter().sum::<f64>() / N as f64;
    for d in &dx[N..] {
        adx = (adx * (N as f64 - 1.0) + d) / N as f64;
    }
    Some(adx)
}

/// The entry/hold test for these settings at bar t: the SPY average test (sampled every
/// `trend_step` bars) and, if set, the crash trigger.
/// Everything observable about the market at decision bar t, for the research archive: the trend
/// filter's inputs, market-wide state (dispersion, breadth, SPY returns/slope) and every coin's signal
/// value, price, returns, ATR and volume ratio. Closed bars only.
pub fn market_context(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    t: usize,
    p: &XsParams,
) -> serde_json::Value {
    let zones = zone_market(m, t).map(|z| {
        serde_json::json!({
            "dispersion_24h_pct": z.dispersion, "median_return_24h_pct": z.median_ret24,
            "ref_return_4h_pct": z.ref_ret4h, "ref_avg_slope_pct_vs_6h": z.ref_slope,
        })
    });
    let (mut up, mut seen) = (0usize, 0usize);
    let mut coins = Vec::new();
    for s in 0..m.symbols.len() {
        let c = |i: usize| m.bars[s][i].map(|b| b.close);
        let (Some(now), Some(avg)) = (
            c(t),
            if t >= H24 {
                avg_close(m, s, t, H24)
            } else {
                None
            },
        ) else {
            continue;
        };
        seen += 1;
        up += usize::from(now > avg);
        let ret = |back: usize| {
            (t >= back)
                .then(|| c(t - back))
                .flatten()
                .map(|b| (now / b - 1.0) * 100.0)
        };
        let vol = |a: usize, b: usize| -> Option<f64> {
            let mut sum = 0.0;
            for i in a..=b {
                sum += m.bars[s][i]?.volume;
            }
            Some(sum / (b - a + 1) as f64)
        };
        let vol_ratio = (t >= H24 + H1)
            .then(
                || match (vol(t + 1 - H1, t), vol(t + 1 - H1 - H24, t - H1)) {
                    (Some(r), Some(q)) if q > 0.0 => Some(r / q),
                    _ => None,
                },
            )
            .flatten();
        coins.push(serde_json::json!({
            "symbol": m.symbols[s],
            "signal": signal_value(m, scores, s, t, p),
            "close": now,
            "ret_1h_pct": ret(H1), "ret_4h_pct": ret(H4), "ret_24h_pct": ret(H24),
            "dist_from_24h_avg_pct": (now / avg - 1.0) * 100.0,
            "atr14_pct": atr_pct(m, s, t + 1),
            "volume_ratio_1h_vs_prior_24h": vol_ratio,
            "turnover_24h": scores.get(s).and_then(|r| r.get(t)).and_then(|x| x.as_ref()).map(|x| x.turnover_24h),
        }));
    }
    serde_json::json!({
        "bar_close_ms": m.ts[t] + BAR_MS,
        "trend": trend_snapshot(m, t, p),
        "zones": zones,
        "breadth_pct_above_own_24h_avg": if seen > 0 { Some(up as f64 / seen as f64 * 100.0) } else { None },
        "coins": coins,
    })
}

/// SPY trend state for the decision log (what the filter saw).
fn trend_snapshot(m: &Market, t: usize, p: &XsParams) -> serde_json::Value {
    let r = &p.risk;
    let bars = r.trend_bars.unwrap_or(REGIME_BARS).max(2);
    let margin = r.trend_margin_pct.unwrap_or(0.0);
    if p.regime == Regime::Off {
        return serde_json::json!({"regime": "Off"});
    }
    let spy = m.symbols.iter().position(|s| s == REFERENCE);
    let closes: Option<Vec<f64>> = spy.and_then(|b| {
        let from = (t + 1).saturating_sub(bars * 3 + 30);
        (from..=t).map(|i| m.bars[b][i].map(|x| x.close)).collect()
    });
    let line = closes
        .as_ref()
        .and_then(|c| trend_line(r.trend_kind, c, bars));
    let last = closes.as_ref().and_then(|c| c.last().copied());
    serde_json::json!({
        "regime": format!("{:?}", p.regime),
        "kind": format!("{:?}", r.trend_kind),
        "window_bars": bars,
        "margin_pct": margin,
        "ref_close": last,
        "ref_line": line,
        "dist_pct": match (last, line) { (Some(a), Some(b)) if b > 0.0 => Some((a / b - 1.0) * 100.0), _ => None },
        "needed_pct": margin,
        "passes": trend_ok(m, t, p),
    })
}

/// The ENTRY trend test (entry-only options and confirmations apply).
fn trend_ok(m: &Market, t: usize, p: &XsParams) -> bool {
    trend_ok_margin(m, t, p, p.risk.trend_margin_pct.unwrap_or(0.0), true)
}

/// The trend test. `entry = false` is the EXIT test ("trend turned down"): it ignores every entry-only option and confirmation.
fn trend_ok_margin(m: &Market, t: usize, p: &XsParams, margin_pct: f64, entry: bool) -> bool {
    if p.regime == Regime::Off {
        return true;
    }
    let r = &p.risk;
    let bars = r.trend_bars.unwrap_or(REGIME_BARS).max(2);
    let step = if entry {
        r.entry_step.or(r.trend_step)
    } else {
        r.trend_step
    }
    .unwrap_or(1)
    .max(1);
    let kind = if entry {
        r.entry_kind.unwrap_or(r.trend_kind)
    } else {
        r.trend_kind
    };
    let Some(spy) = m.symbols.iter().position(|s| s == REFERENCE) else {
        return false;
    };
    if t + 1 < bars {
        return false;
    }
    let close = |i: usize| m.bars[spy][i].map(|b| b.close);
    let Some(last) = close(t) else {
        return false;
    };
    let avg = if kind == TrendKind::Sma {
        let samples: Option<Vec<f64>> = (0..)
            .map(|k| t.checked_sub(k * step))
            .take_while(|i| i.is_some_and(|i| i + bars > t))
            .map(|i| close(i.unwrap_or(0)))
            .collect();
        let Some(samples) = samples else {
            return false;
        };
        samples.iter().sum::<f64>() / samples.len() as f64
    } else {
        let from = (t + 1).saturating_sub(bars * 3 + 30);
        let closes: Option<Vec<f64>> = (from..=t).map(close).collect();
        let Some(line) = closes.and_then(|c| trend_line(kind, &c, bars)) else {
            return false;
        };
        line
    };
    if last <= avg * (1.0 + margin_pct / 100.0) {
        return false;
    }
    if !entry {
        return true;
    }
    if let Some(min) = r.er_min {
        let from = (t + 1).saturating_sub(bars + 1);
        let closes: Option<Vec<f64>> = (from..=t).map(close).collect();
        match closes.and_then(|c| efficiency_ratio(&c, bars)) {
            Some(er) if er >= min => {}
            _ => return false,
        }
    }
    if let Some(min) = r.adx_min {
        if adx14(m, spy, t).is_none_or(|a| a < min) {
            return false;
        }
    }
    if !pressure_ok(m, t, spy, bars, r) {
        return false;
    }
    if let Some(cp) = r.crash_pct {
        let n = r.crash_bars.unwrap_or(H4).max(1);
        let from = t.saturating_sub(n);
        let mut high = f64::MIN;
        for i in from..=t {
            match close(i) {
                Some(c) => high = high.max(c),
                None => return false,
            }
        }
        if last <= high * (1.0 - cp / 100.0) {
            return false;
        }
    }
    true
}

impl XsParams {
    /// Positions held at a full rebalance: `top` longs, plus `top` shorts
    /// unless long only.
    pub fn slots(&self) -> usize {
        if self.long_only {
            self.top
        } else {
            2 * self.top
        }
    }
}

/// Drawdown handling, each rule decided on a 1m CLOSE and executed at the
/// next open, as a live bot would (no intrabar hindsight).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Risk {
    /// Exit a position whose close is this % against its average entry.
    #[serde(default)]
    pub close_stop_pct: Option<f64>,
    /// Same, for shorts only (short squeezes are the unbounded risk).
    #[serde(default)]
    pub short_stop_pct: Option<f64>,
    /// Exit a position whose close is this % in its favour (lock the snap-back).
    #[serde(default)]
    pub take_profit_pct: Option<f64>,
    /// Add once, the same size again, when a close is this % against the entry.
    #[serde(default)]
    pub add_pct: Option<f64>,
    /// Portfolio breaker: when marked equity closes this % below the equity at
    /// the last rebalance, close everything; no new positions until the next one.
    #[serde(default)]
    pub breaker_pct: Option<f64>,
    /// Size positions inversely to their recent volatility (same total size).
    #[serde(default)]
    pub vol_scaled: bool,
    // NOTE(agents): Exit research (user request 2026-10-04). Both rules only ever tighten the
    //               position's existing mark-triggered stop, and use the extreme of bars already
    //               fully processed (no look-ahead): they take effect from the NEXT bar.
    /// Trailing stop: the stop follows the best mark reached, this % behind it.
    #[serde(default)]
    pub trail_pct: Option<f64>,
    /// Break-even stop: once the mark has reached this % in favour, the stop moves to the entry.
    #[serde(default)]
    pub breakeven_pct: Option<f64>,
    // NOTE(agents): Re-entry pause after the drawdown stop (research, 2026-10-04). Trade-level
    //               replay of the forward test showed four entry rounds in one day (09-22), each
    //               stopped out within minutes: the user's "resume within an hour" rule re-entered
    //               a falling market. `None` keeps that rule (one bar); the search may pick a longer
    //               pause, in bars, as an exit parameter. The live service no longer trades.
    /// Bars to wait after the drawdown stop before entries may resume.
    #[serde(default)]
    pub resume_pause_bars: Option<usize>,
    // NOTE(agents): Volatility-scaled exits (research, 2026-10-04): distances as multiples of the
    //               coin's own ATR (mean true range of the 14 bars BEFORE entry, in % of price) so one
    //               rule means the same risk on a calm coin and a wild one. When set (and the ATR is
    //               known) they replace the fixed % of the same exit.
    /// Intrabar stop at this many ATRs against the entry.
    #[serde(default)]
    pub atr_stop_mult: Option<f64>,
    /// Close-based take-profit at this many ATRs in favour.
    #[serde(default)]
    pub atr_tp_mult: Option<f64>,
    /// Trailing stop this many ATRs behind the best mark.
    #[serde(default)]
    pub atr_trail_mult: Option<f64>,
    // NOTE(agents): Down-trend control (user rule 2026-10-04: never go long in a down trend; the
    //               search finds the values). With `Regime::MarketTrend` a down trend is the reference (SPY) close
    //               NOT above its average close of the last `trend_bars` bars by `trend_margin_pct`.
    //               New entries are blocked then; with `trend_exit` open positions are also closed
    //               (decided at the close, executed at the next open like the breaker).
    /// Averaging window of the SPY trend test, bars (default `REGIME_BARS`).
    #[serde(default)]
    pub trend_bars: Option<usize>,
    /// SPY must close this % above its average to count as an up trend (default 0).
    #[serde(default)]
    pub trend_margin_pct: Option<f64>,
    /// Close open positions when the trend turns down.
    #[serde(default)]
    pub trend_exit: bool,
    /// Sample SPY every this many bars inside the averaging window (1 = every 1m bar; 2 = 30m,
    /// 4 = 1h closes): a coarser trend interval built from the 1m bars.
    #[serde(default)]
    pub trend_step: Option<usize>,
    // NOTE(agents): ENTRY-ONLY research options (user 2026-10-08: "exits are perfect, dont fuck with them"). The exit test
    //               (`trend_ok_margin(.., entry = false)`) never reads these: it uses `trend_kind` / `trend_step` and none of the
    //               confirmations below, exactly as before.
    /// Trend-average type for ENTRIES only (None = `trend_kind`).
    #[serde(default)]
    pub entry_kind: Option<TrendKind>,
    /// Trend sampling step for ENTRIES only (None = `trend_step`).
    #[serde(default)]
    pub entry_step: Option<usize>,
    /// A data-driven per-stock entry gate found by the search (research).
    #[serde(default)]
    pub entry_gate: Option<EntryGate>,
    // NOTE(agents): GRADUATED SIZING (user 2026-10-08), entries only: the budget the funding check validated per leg is only ever scaled
    //               DOWN, so no leg can exceed its checked size or the order-book cap, and every leg keeps room for one whole share.
    /// Rank grading: the best-ranked stock gets the full slot, the worst `1 - grade_rank` of it (linear in between).
    #[serde(default)]
    pub grade_rank: Option<f64>,
    /// Trend grading: total exposure = clamp(0.4 + grade_trend x SPY's % distance above its trend average, 0.4, 1).
    #[serde(default)]
    pub grade_trend: Option<f64>,
    /// RISK-BASED sizing (position size = account risk / stop distance): each trade is sized so that its stop loses at most this % of the account;
    /// a tighter stop allows a bigger position, up to the funded slot, never above.
    #[serde(default)]
    pub grade_risk: Option<f64>,
    /// THREE-TIER DRAWDOWN protocol (entries only): below `.0` % drawdown trade normally, between `.0` and `.1` % size is multiplied by `.2`, from `.1` %
    /// on no new entries (open positions keep their exits). The measure is the drawdown from the cycle's peak equity.
    #[serde(default)]
    pub grade_dd: Option<(f64, f64, f64)>,
    /// LOSS-STREAK PAUSE (entries only, research 2026-10-11): after `.0` consecutive losing closed trades no new entry is opened for `.1`
    /// hours after the last of them closed (open positions keep their exits). Losses cluster in the blocks that break the -8% limit; this
    /// stops the strategy re-entering straight into the same bad regime. Stateless: read from the closed trades.
    #[serde(default)]
    pub loss_pause: Option<(usize, f64)>,
    /// ROLLING-LOSS STOP (entries only, research 2026-10-13): no new entry while the trades closed in the last `.1` hours lost more than
    /// `.0` % of the equity they started from. A magnitude rule (the streak pause counts losers and did not help): it reacts to a bad
    /// stretch while the block is still running. Stateless: read from the closed trades; open positions keep their exits.
    #[serde(default)]
    pub roll_stop: Option<(f64, f64)>,
    /// Research only (owner 2026-10-13): skip an entry whose machine-learning score is below this percentile of the scored universe; the search draws the percentile
    /// (no fixed value). None = off. Needs `research::ML_SCORES_ON` and the lab's score file.
    #[serde(default)]
    pub ml_gate: Option<f64>,
    /// Crash trigger: also a down trend when SPY's close is this % below its highest close of the
    /// last `crash_bars` bars (catches a fast fall the moving average is slow to show).
    #[serde(default)]
    pub crash_pct: Option<f64>,
    #[serde(default)]
    pub crash_bars: Option<usize>,
    // NOTE(agents): Volume / buy-sell pressure confirmation of the SPY trend (user request
    //               2026-10-04). Candles carry no taker-buy/taker-sell split, so pressure is
    //               PROXIED from candles: Chaikin money flow (close location x volume), VWAP, on-balance
    //               volume. Each set option must also pass for the market to count as an up trend.
    /// Chaikin money flow over this many bars must exceed `cmf_min` (default 0): net buying.
    #[serde(default)]
    pub cmf_bars: Option<usize>,
    #[serde(default)]
    pub cmf_min: Option<f64>,
    /// SPY's close must be above its volume-weighted average price over this many bars.
    #[serde(default)]
    pub vwap_bars: Option<usize>,
    /// On-balance volume (volume signed by the close direction) must be rising over this many bars.
    #[serde(default)]
    pub obv_bars: Option<usize>,
    /// At least this % of the universe's coins must close above their own average (breadth).
    #[serde(default)]
    pub breadth_min_pct: Option<f64>,
    // NOTE(agents): Trend-line variants and gates (user request 2026-10-04, after web research on
    //               low-lag filters): the line SPY's close is compared with (SMA/EMA/Hull/KAMA/Donchian
    //               midpoint over `trend_bars`), Kaufman's efficiency ratio and Wilder's ADX as
    //               "is it trending at all" gates, and hysteresis (a separate margin for GOING FLAT).
    #[serde(default)]
    pub trend_kind: TrendKind,
    /// Efficiency ratio over `trend_bars` must be at least this (0-1): net move / path length.
    #[serde(default)]
    pub er_min: Option<f64>,
    /// Wilder ADX(14) of SPY must be at least this (a trend-strength gate).
    #[serde(default)]
    pub adx_min: Option<f64>,
    /// Margin used for the go-flat test only (default = `trend_margin_pct`); lower = hysteresis.
    #[serde(default)]
    pub trend_exit_margin_pct: Option<f64>,
    // NOTE(agents): Entry-skip "loss zones" mined from 130k long trades (docs/validation.md,
    //               examples/loss_conditions.rs), all decided on bars closed by the decision. Each
    //               zone is two conditions that must BOTH hold to skip; thresholds are searched, never
    //               fixed. Unknown data fails closed (the coin is skipped).
    /// Z1/Z2 share this: skip a coin that fell more than this % over the last hour (4 bars).
    #[serde(default)]
    pub knife_drop_pct: Option<f64>,
    /// Z1: ...while the dispersion (stdev) of the universe's 24h returns is at least this %.
    #[serde(default)]
    pub zone_dispersion_pct: Option<f64>,
    /// Z2: ...while the coin's last-hour volume is below this multiple of its prior-24h average.
    #[serde(default)]
    pub zone_low_volume_ratio: Option<f64>,
    /// Z3: skip a coin more than this % below its own 24h average...
    #[serde(default)]
    pub zone_laggard_pct: Option<f64>,
    /// ...while the median coin is up at least this % over 24h (needs `zone_laggard_pct`).
    #[serde(default)]
    pub zone_rally_pct: Option<f64>,
    /// Z4: skip ALL entries when SPY fell more than this % over 4h (16 bars)...
    #[serde(default)]
    pub zone_spy_pullback_pct: Option<f64>,
    /// ...while its 24h average is still rising by at least this % (vs 6h ago).
    #[serde(default)]
    pub zone_spy_slope_pct: Option<f64>,
}

/// What a data-driven entry gate looks at, per stock at the decision close. Return and distance are divided by the stock's own
/// ATR %, so one threshold range fits every candle size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GateFeature {
    /// Return over `bars` candles, in ATRs.
    Ret,
    /// Distance of the close from its `bars`-candle average, in ATRs.
    DistAvg,
    /// Volume of the last `bars` candles over the 3x`bars` before them.
    VolRatio,
    /// The stock's ATR % itself (volatility).
    Atr,
    /// SPY's trend slope: % change of its day-long average close over the last quarter day (the same market measure the
    /// research archive records as `m_ref_slope_pct`). Market-wide: it gates every stock alike. Research option (user
    /// 2026-10-09 win-rate analysis: runs entering while SPY trends steeply win less often).
    RefSlope,
    /// SPY's volatility expansion: its 14-bar ATR % over the mean of its four 14-bar ATRs before that (1.0 = as calm as lately).
    /// Market-wide: it gates every stock alike. Research option: the 8%-worst-block rule kills most entry ideas, and big losing
    /// blocks are market shocks, which volatility expansion tends to precede.
    RefVol,
    /// Relative strength: the stock's return over `bars` candles minus SPY's over the same candles, in the stock's ATRs.
    /// Research option (supervisor 2026-10-14): every other stock gate reads the stock alone or the market alone; this one asks
    /// whether the entry is a laggard or a leader against the market, which separates a market-wide move from a stock-specific one.
    RelStrength,
}

/// Skip a stock when its feature is above (or below) a threshold. The search draws feature, direction, window and threshold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EntryGate {
    pub feature: GateFeature,
    pub bars: usize,
    pub skip_above: bool,
    pub threshold: f64,
}

/// The gate feature of stock `s` at the decision close `t` (None when it cannot be measured: the gate then fails closed).
pub fn gate_value(m: &Market, s: usize, t: usize, g: &EntryGate) -> Option<f64> {
    let b = g.bars.max(2);
    let atr = atr_pct(m, s, t + 1);
    if !atr.is_finite() || atr <= 0.0 {
        return None;
    }
    let close = |i: usize| m.bars[s][i].map(|x| x.close);
    match g.feature {
        GateFeature::Atr => Some(atr),
        GateFeature::RefSlope => {
            let spy = m.symbols.iter().position(|x| x == REFERENCE)?;
            Some(
                (avg_close(m, spy, t, H24)? / avg_close(m, spy, t.checked_sub(H6)?, H24)? - 1.0)
                    * 100.0,
            )
        }
        GateFeature::RefVol => {
            let spy = m.symbols.iter().position(|x| x == REFERENCE)?;
            ref_vol_expansion(m, spy, t)
        }
        GateFeature::RelStrength => {
            let spy = m.symbols.iter().position(|x| x == REFERENCE)?;
            let t0 = t.checked_sub(b)?;
            let (now, then) = (close(t)?, close(t0)?);
            let (ref_now, ref_then) = (m.bars[spy][t]?.close, m.bars[spy][t0]?.close);
            ((then > 0.0) && (ref_then > 0.0))
                .then(|| ((now / then - ref_now / ref_then) * 100.0) / atr)
        }
        GateFeature::Ret => {
            let (now, then) = (close(t)?, close(t.checked_sub(b)?)?);
            Some((now / then - 1.0) * 100.0 / atr)
        }
        GateFeature::DistAvg => {
            if t + 1 < b {
                return None;
            }
            let w: Option<Vec<f64>> = (t + 1 - b..=t).map(close).collect();
            let w = w?;
            let avg = w.iter().sum::<f64>() / w.len() as f64;
            let now = close(t)?;
            (avg > 0.0).then(|| (now / avg - 1.0) * 100.0 / atr)
        }
        GateFeature::VolRatio => {
            if t + 1 < 4 * b {
                return None;
            }
            let vol = |a: usize, z: usize| -> Option<f64> {
                let mut sum = 0.0;
                for i in a..=z {
                    sum += m.bars[s][i]?.volume;
                }
                Some(sum / (z - a + 1) as f64)
            };
            let (recent, prior) = (vol(t + 1 - b, t)?, vol(t + 1 - 4 * b, t - b)?);
            (prior > 0.0).then(|| recent / prior)
        }
    }
}

/// SPY's ATR % at decision close `t` over the mean ATR % of the four 14-bar windows before it (None when any is unmeasured).
fn ref_vol_expansion(m: &Market, spy: usize, t: usize) -> Option<f64> {
    let now = atr_pct(m, spy, t + 1);
    let prior: Vec<f64> = (1..=4)
        .map(|k| atr_pct(m, spy, (t + 1).saturating_sub(14 * k)))
        .collect();
    if now <= 0.0 || prior.iter().any(|v| *v <= 0.0) {
        return None;
    }
    Some(now / (prior.iter().sum::<f64>() / 4.0))
}

/// The line SPY's close is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TrendKind {
    #[default]
    Sma,
    Ema,
    /// Hull moving average: WMA(2*WMA(n/2) - WMA(n), sqrt n).
    Hma,
    /// Kaufman adaptive moving average (efficiency-ratio weighted EMA).
    Kama,
    /// Midpoint of the highest and lowest close of the window (Donchian middle).
    Donchian,
}

/// Action decided at a close, executed no earlier than t+2's open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NextAction {
    Stop,
    TakeProfit,
    Add,
    Rebalance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XsPosition {
    pub sym: usize,
    pub symbol: String,
    pub side: Side,
    pub entry_ts: i64,
    /// Average entry price (changes if `Risk::add_pct` adds to the position).
    pub entry: f64,
    pub qty: f64,
    pub margin: f64,
    pub leverage: f64,
    pub liquidation: f64,
    pub stop: Option<f64>,
    /// Best mark price reached in the position's favour (high for longs), for trailing rules.
    #[serde(default)]
    pub peak: f64,
    /// ATR of the coin before entry, % of price (0 = unknown); scales the ATR exits.
    #[serde(default)]
    pub atr_pct: f64,
    /// Open time (ms) of the bar whose close decided this entry.
    #[serde(default)]
    pub decided_ts: Option<i64>,
    pub fees: f64,
    pub funding: f64,
    #[serde(default)]
    pub last_funding_ts: Option<i64>,
    pub mark: f64,
    #[serde(default)]
    pub next_action: Option<NextAction>,
    #[serde(default)]
    pub action_not_before: i64,
    /// Decision close that created the action; later reports must not restamp it.
    #[serde(default)]
    action_decided_at: Option<i64>,
    /// Already added to once.
    #[serde(default)]
    pub added: bool,
    /// This account's commission rate (commission / notional) at entry.
    pub fee_rate: f64,
    /// An exit the live mark-price monitor locked in mid-bar: (time ms, mark
    /// price, reason). Booked when that bar is processed, after its funding.
    /// Mark plus measured book cost is a simulated fill approximation, not
    /// a contemporaneous executable price or an exchange execution record.
    #[serde(default)]
    pub live_exit: Option<(i64, f64, String)>,
    /// The moomoo rules measured when the position was opened (commission, book):
    /// the fallback for managing it when the market has no fresh rules for it.
    #[serde(default)]
    pub entry_rules: Option<super::rules::Rules>,
}

/// Something the engine decided, skipped or refused, for the research archive (user request
/// 2026-10-05: record denied and not-placed orders, with as much context as possible). Only produced
/// when the portfolio's event log is enabled (paper trading); backtests never allocate it.
#[derive(Debug, Clone, Serialize)]
pub struct EngineEvent {
    /// Open time (ms) of the bar being processed (live monitor events: wall-clock ms).
    pub ts_ms: i64,
    pub kind: &'static str,
    pub symbol: Option<String>,
    pub detail: serde_json::Value,
}

/// One coin's fate at a rebalance decision.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateNote {
    pub symbol: String,
    pub signal: Option<f64>,
    /// 1 = the lowest (best-for-long) signal value among eligible coins.
    pub rank: Option<usize>,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XsPortfolio {
    pub equity: f64,
    /// A failed execution/data precondition invalidates the simulation, never a fake fill.
    #[serde(default)]
    pub execution_error: Option<String>,
    #[serde(default)]
    pub rejected_rebalances: usize,
    #[serde(default)]
    pub effective_top: usize,
    #[serde(default)]
    pub allocation_note: String,
    /// USDT the real account has committed elsewhere (other positions' and
    /// orders' initial margin, locked funds): never available to this bot.
    #[serde(default)]
    pub reserved: f64,
    start_equity: f64,
    pub positions: Vec<XsPosition>,
    /// Target set decided at the last rebalance close, executed at the next open.
    /// Carries symbol names so it survives the live symbol list changing.
    #[serde(default)]
    pending_targets: Option<Vec<(usize, String, Side)>>,
    #[serde(default)]
    pending_params: Option<XsParams>,
    #[serde(default)]
    pending_not_before: i64,
    #[serde(default)]
    pending_decided_at: Option<i64>,
    /// Margin + entry fee per slot fixed at the decision close, so the fill
    /// uses exactly the sizes whose lot rounding was checked then.
    #[serde(default)]
    pending_slot: f64,
    pub trades: Vec<Trade>,
    peak: f64,
    /// Per-cycle stop reference; global `peak` and `max_dd` remain unchanged
    /// by a resume, so historical drawdown is never erased.
    #[serde(default)]
    risk_peak: f64,
    max_dd: f64,
    liquidations: usize,
    /// Cost multiplier on the measured fee and order-book cost (1 = as measured);
    /// >1 only for cost stress tests.
    #[serde(default = "one")]
    pub cost_mult: f64,
    /// False while the walk-forward is not passing: no new positions, and the
    /// next rebalance closes everything (go flat rather than trade unvalidated).
    #[serde(default = "yes")]
    pub entries_allowed: bool,
    /// Marked equity right after the last rebalance (breaker reference).
    #[serde(default)]
    rebalance_equity: f64,
    /// Breaker tripped at a close: close everything at the next open.
    #[serde(default)]
    flatten_next: bool,
    // NOTE(agents): User override: retain the 25% exit but resume within an hour.
    // One-bar cooldown + finished exits + valid entry gates permit immediate
    // re-evaluation with the normal causal fill delay. Missing data or insufficient
    // free balance may prevent a fill; never promise or fabricate one.
    /// The drawdown stop fired: finish exits before resuming after cooldown.
    #[serde(default)]
    pub stopped_out: bool,
    #[serde(default)]
    pub resume_not_before: Option<i64>,
    /// Force a fresh entry decision after cooldown regardless of holding period.
    #[serde(default)]
    resume_pending: bool,
    /// Earliest open at which a pending breaker/drawdown flatten may fill.
    #[serde(default)]
    flatten_not_before: i64,
    #[serde(default)]
    flatten_decided_at: Option<i64>,
    /// Inverse-volatility weight per pending target symbol, fixed at the
    /// decision close (mean 1 per side); empty when sizing is equal.
    #[serde(default)]
    pending_weights: BTreeMap<String, f64>,
    /// Structured decision/denial log (None = off). Never persisted.
    #[serde(skip)]
    events: Option<Vec<EngineEvent>>,
}

/// Serde default: 1.0.
fn one() -> f64 {
    1.0
}

/// Serde default: true.
fn yes() -> bool {
    true
}

/// Standard deviation of 1m log returns over the trading day ending at bar `t`
/// (None with fewer than half that many returns).
fn realized_vol(m: &Market, sym: usize, t: usize) -> Option<f64> {
    let from = t.saturating_sub(H24);
    let r: Vec<f64> = (from + 1..=t)
        .filter_map(|i| Some((m.bars[sym][i]?.close / m.bars[sym][i - 1]?.close).ln()))
        .filter(|x| x.is_finite())
        .collect();
    if r.len() < H24 / 2 {
        return None;
    }
    let mean = r.iter().sum::<f64>() / r.len() as f64;
    let v = (r.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / r.len() as f64).sqrt();
    (v > 0.0).then_some(v)
}

/// Inverse-volatility weights at decision bar t, normalised to mean 1 within
/// each side so both sides keep equal notional; a target without enough history
/// gets weight 1 (equal size).
fn decision_weights(m: &Market, t: usize, target: &BTreeMap<usize, Side>) -> BTreeMap<String, f64> {
    let inv: Vec<(usize, Side, Option<f64>)> = target
        .iter()
        .map(|(&sym, &side)| (sym, side, realized_vol(m, sym, t).map(|v| 1.0 / v)))
        .collect();
    let mean = |side: Side| {
        let known: Vec<f64> = inv
            .iter()
            .filter(|x| x.1 == side)
            .filter_map(|x| x.2)
            .collect();
        if known.is_empty() {
            1.0
        } else {
            known.iter().sum::<f64>() / known.len() as f64
        }
    };
    let (long, short) = (mean(Side::Long), mean(Side::Short));
    inv.into_iter()
        .map(|(sym, side, w)| {
            let mean = if side == Side::Long { long } else { short };
            (
                m.symbols[sym].clone(),
                w.map_or(1.0, |x| x / mean).max(MIN_SIZE_FRACTION),
            )
        })
        .collect()
}

/// Size multipliers (best-ranked first), all in (0, 1]: rank grading shrinks lower-ranked legs, trend grading scales every leg.
fn graded_multipliers(n: usize, grade_rank: Option<f64>, trend_mult: f64) -> Vec<f64> {
    (0..n)
        .map(|i| {
            let rank = match grade_rank {
                Some(g) if n > 1 => 1.0 - g.clamp(0.0, 0.95) * i as f64 / (n - 1) as f64,
                _ => 1.0,
            };
            (rank * trend_mult).clamp(MIN_SIZE_FRACTION, 1.0)
        })
        .collect()
}

/// A leg's multiplier raised so that its budget still buys one whole share (never above 1).
fn afford_floor(mult: f64, price: f64, slot: f64) -> f64 {
    if slot > 0.0 && price > 0.0 {
        mult.max((price * 1.02 / slot).min(1.0))
    } else {
        mult
    }
}

/// The tightest percentage distance at which a new long would be stopped (hard stop, close stop, ATR stop), None when it has no stop.
fn effective_stop_pct(m: &Market, s: usize, t: usize, p: &XsParams) -> Option<f64> {
    let atr = atr_pct(m, s, t + 1);
    [
        p.stop_pct,
        p.risk.close_stop_pct,
        p.risk.atr_stop_mult.filter(|_| atr > 0.0).map(|k| k * atr),
    ]
    .into_iter()
    .flatten()
    .filter(|x| x.is_finite() && *x > 0.0)
    .reduce(f64::min)
}

/// Risk-based multiplier of one leg: the slot is reduced so that its stop loses at most `risk_pct` of `equity`; never above 1.
fn risk_multiplier(risk_pct: f64, equity: f64, stop_pct: f64, slot: f64) -> f64 {
    if !(risk_pct > 0.0 && equity > 0.0 && stop_pct > 0.0 && slot > 0.0) {
        return 1.0;
    }
    ((risk_pct / 100.0 * equity) / (stop_pct / 100.0) / slot).min(1.0)
}

/// Drawdown tier of the new-entry size: Some(multiplier), or None = no new entries (the third tier).
/// Whether the loss-streak pause blocks entries at `now_ms`: the last `streak` closed trades all lost and the last one closed no more
/// than `hours` ago. `trades` are in closing order.
fn loss_pause_active(trades: &[Trade], rule: Option<(usize, f64)>, now_ms: i64) -> bool {
    let Some((streak, hours)) = rule else {
        return false;
    };
    if streak == 0 || trades.len() < streak {
        return false;
    }
    let tail = &trades[trades.len() - streak..];
    tail.iter().all(|t| t.pnl < 0.0)
        && (now_ms - tail[streak - 1].exit_ts) as f64 <= hours * 3_600_000.0
}

/// Whether the rolling-loss stop blocks entries at `now_ms`: the pnl of trades closed within the last `hours` is below `-pct` % of the
/// equity at the start of that window (`equity` now minus that pnl). `trades` are in closing order.
fn roll_stop_active(trades: &[Trade], rule: Option<(f64, f64)>, equity: f64, now_ms: i64) -> bool {
    let Some((pct, hours)) = rule else {
        return false;
    };
    if !(pct > 0.0 && hours > 0.0) {
        return false;
    }
    let from = now_ms - (hours * 3_600_000.0) as i64;
    let lost: f64 = trades
        .iter()
        .rev()
        .take_while(|t| t.exit_ts >= from)
        .map(|t| t.pnl)
        .sum();
    let start = equity - lost;
    lost < 0.0 && start > 0.0 && -lost / start * 100.0 > pct
}

fn drawdown_multiplier(dd_pct: f64, tiers: (f64, f64, f64)) -> Option<f64> {
    let (t1, t2, m) = tiers;
    if dd_pct >= t2 {
        None
    } else if dd_pct >= t1 {
        Some(m.clamp(MIN_SIZE_FRACTION, 1.0))
    } else {
        Some(1.0)
    }
}

/// Graduated sizing for the targets of a rebalance decision: multiplies the legs' weights (see `Risk::grade_rank` / `grade_trend`).
#[allow(clippy::too_many_arguments)]
fn grade_weights(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    t: usize,
    p: &XsParams,
    target: &BTreeMap<usize, Side>,
    slot: f64,
    equity: f64,
    dd_mult: f64,
    weights: &mut BTreeMap<String, f64>,
) {
    let trend_mult = match p.risk.grade_trend {
        Some(k) => {
            let dist = trend_snapshot(m, t, p)["dist_pct"]
                .as_f64()
                .unwrap_or(0.0)
                .max(0.0);
            (0.4 + k * dist).clamp(0.4, 1.0)
        }
        None => 1.0,
    };
    let mut order: Vec<(usize, f64)> = target
        .keys()
        .filter_map(|&s| signal_value(m, scores, s, t, p).map(|v| (s, v)))
        .collect();
    order.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    let mults = graded_multipliers(order.len(), p.risk.grade_rank, trend_mult);
    for ((s, _), mult) in order.into_iter().zip(mults) {
        let price = m.bars[s][t].map_or(0.0, |b| b.close);
        let name = m.symbols[s].clone();
        let base = weights.get(&name).copied().unwrap_or(1.0);
        let risk = p
            .risk
            .grade_risk
            .and_then(|r| {
                effective_stop_pct(m, s, t, p).map(|sp| risk_multiplier(r, equity, sp, slot))
            })
            .unwrap_or(1.0);
        weights.insert(
            name,
            base * afford_floor(mult * risk * dd_mult, price, slot).max(MIN_SIZE_FRACTION),
        );
    }
}

/// True when bar t's close is a rebalance point (timestamp-aligned).
fn is_rebalance(ts: i64, hold: usize) -> bool {
    (ts + BAR_MS) % (hold as i64 * BAR_MS) == 0
}

/// Average hourly rate of the last 3 funding settlements at or before `ts`
/// (intervals differ per symbol: 1h, 4h or 8h, so normalise per hour).
fn funding_hourly(f: &[(i64, f64)], ts: i64) -> Option<f64> {
    let n = f.partition_point(|x| x.0 <= ts);
    if n < 2 {
        return None;
    }
    let from = n.saturating_sub(3).max(1);
    let rates: Vec<f64> = (from..n)
        .filter_map(|i| {
            let hours = (f[i].0 - f[i - 1].0) as f64 / 3_600_000.0;
            (hours > 0.0).then(|| f[i].1 / hours)
        })
        .collect();
    (!rates.is_empty()).then(|| rates.iter().sum::<f64>() / rates.len() as f64)
}

/// Ranking value of symbol s at bar t (None = not rankable now).
pub fn signal_value(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    s: usize,
    t: usize,
    p: &XsParams,
) -> Option<f64> {
    let sc = scores[s][t]?;
    let v = match p.signal {
        Signal::Return => {
            let (now, then) = (
                m.bars[s][t]?.close,
                m.bars[s][t.checked_sub(p.lookback)?]?.close,
            );
            if then <= 0.0 {
                return None;
            }
            now / then - 1.0
        }
        Signal::TrendScore => sc.trend_score,
        Signal::PriceAction => sc.price_action_score,
        Signal::Volatility => sc.volatility_score,
        Signal::Rsi => sc.rsi14,
        Signal::Pulse => sc.pulse_long? - sc.pulse_short?,
        Signal::Funding => funding_hourly(&m.funding[s], m.ts[t])?,
        Signal::CalmDip => (sc.volatility_score + sc.return_score) / 2.0,
        Signal::Ml => sc.ml?,
    };
    v.is_finite().then_some(if p.flip { -v } else { v })
}

/// Target sides at bar t: LONG the `top` lowest signal values, SHORT the `top` highest.
#[cfg(test)]
fn targets(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    t: usize,
    p: &XsParams,
) -> BTreeMap<usize, Side> {
    ranked_targets(m, scores, t, p, None)
}

/// Market-wide state at decision bar t for the entry-skip zones.
struct ZoneMarket {
    dispersion: f64,
    median_ret24: f64,
    ref_ret4h: f64,
    ref_slope: f64,
}

/// Average close of symbol `s` over the `n` bars ending at `end` (None if any bar is missing).
fn avg_close(m: &Market, s: usize, end: usize, n: usize) -> Option<f64> {
    if end + 1 < n {
        return None;
    }
    let mut sum = 0.0;
    for i in end + 1 - n..=end {
        sum += m.bars[s][i]?.close;
    }
    Some(sum / n as f64)
}

/// Market state for the pullback zone at bar `t`: SPY's and the universe's recent returns.
fn zone_market(m: &Market, t: usize) -> Option<ZoneMarket> {
    if t < ZONE_HISTORY {
        return None;
    }
    let spy = m.symbols.iter().position(|s| s == REFERENCE)?;
    let c = |s: usize, i: usize| m.bars[s][i].map(|b| b.close);
    let mut rets: Vec<f64> = Vec::new();
    for s in 0..m.symbols.len() {
        if let (Some(a), Some(b)) = (c(s, t), c(s, t - H24)) {
            if b > 0.0 {
                rets.push((a / b - 1.0) * 100.0);
            }
        }
    }
    if rets.len() < 5 {
        return None;
    }
    let mean = rets.iter().sum::<f64>() / rets.len() as f64;
    let dispersion =
        (rets.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / rets.len() as f64).sqrt();
    rets.sort_by(f64::total_cmp);
    Some(ZoneMarket {
        dispersion,
        median_ret24: rets[rets.len() / 2],
        ref_ret4h: (c(spy, t)? / c(spy, t - H4)? - 1.0) * 100.0,
        ref_slope: (avg_close(m, spy, t, H24)? / avg_close(m, spy, t - H6, H24)? - 1.0) * 100.0,
    })
}

/// True when a loss zone says: do not enter `s` now (see `Risk` zone fields).
fn zone_blocks(m: &Market, t: usize, s: usize, r: &Risk, mk: Option<&ZoneMarket>) -> bool {
    let any = r.knife_drop_pct.is_some() || r.zone_laggard_pct.is_some();
    if !any {
        return false;
    }
    let Some(mk) = mk else {
        return true;
    };
    let c = |i: usize| m.bars[s][i].map(|b| b.close);
    let (Some(now), Some(h1), Some(avg)) =
        (c(t), t.checked_sub(H1).and_then(c), avg_close(m, s, t, H24))
    else {
        return true;
    };
    let ret1h = (now / h1 - 1.0) * 100.0;
    let dropped = r.knife_drop_pct.is_some_and(|d| ret1h < -d);
    if dropped && r.zone_dispersion_pct.is_some_and(|x| mk.dispersion >= x) {
        return true;
    }
    if dropped {
        if let Some(v) = r.zone_low_volume_ratio {
            let vol = |a: usize, b: usize| -> Option<f64> {
                let mut sum = 0.0;
                for i in a..=b {
                    sum += m.bars[s][i]?.volume;
                }
                Some(sum / (b - a + 1) as f64)
            };
            match (vol(t + 1 - H1, t), vol(t + 1 - H1 - H24, t - H1)) {
                (Some(recent), Some(prior)) if recent < v * prior.max(1e-12) => return true,
                (Some(_), Some(_)) => {}
                _ => return true,
            }
        }
    }
    if let (Some(below), Some(rally)) = (r.zone_laggard_pct, r.zone_rally_pct) {
        if (now / avg - 1.0) * 100.0 < -below && mk.median_ret24 >= rally {
            return true;
        }
    }
    false
}

/// Research: true when the stock's ML score is below the `pctl`-th percentile of the scored stocks at bar `t` (needs at least 5 scored; no score = not blocked).
fn ml_gate_blocks(scores: &[Vec<Option<Score>>], s: usize, t: usize, pctl: i64) -> bool {
    if pctl < 0 {
        return false;
    }
    let Some(mine) = scores[s][t].and_then(|x| x.ml) else {
        return false;
    };
    let (mut below, mut n) = (0usize, 0usize);
    for row in scores {
        if let Some(v) = row[t].and_then(|x| x.ml) {
            n += 1;
            below += usize::from(v < mine);
        }
    }
    n >= 5 && (below as f64 / n as f64) * 100.0 < pctl as f64
}

/// Why `s` can or cannot be held as a long entry at decision bar t.
fn candidate_value(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    s: usize,
    t: usize,
    p: &XsParams,
    budget: Option<(f64, f64)>,
    mk: Option<&ZoneMarket>,
) -> Result<f64, &'static str> {
    if zone_blocks(m, t, s, &p.risk, mk) {
        return Err("blocked by a loss zone (falling knife / laggard in a rally)");
    }
    if let Some(g) = p.risk.entry_gate.as_ref() {
        match gate_value(m, s, t, g) {
            Some(v) if (g.skip_above && v > g.threshold) || (!g.skip_above && v < g.threshold) => {
                return Err("blocked by the data-driven entry gate")
            }
            None => return Err("entry gate: value unavailable (fails closed)"),
            _ => {}
        }
    }
    let pctl = p.risk.ml_gate.map_or_else(
        || research::ML_GATE_PCTL.load(std::sync::atomic::Ordering::Relaxed),
        |q| q.round() as i64,
    );
    if ml_gate_blocks(scores, s, t, pctl) {
        return Err(
            "blocked by the machine-learning gate (score in the lowest part of the universe)",
        );
    }
    let value =
        signal_value(m, scores, s, t, p).ok_or("no signal value (not enough history or data)")?;
    if let Some((margin, cm)) = budget {
        let bar = m.bars[s][t].ok_or("no bar at the decision close")?;
        let r = m.rules(s).ok_or("no moomoo rules for this symbol")?;
        let cap = r
            .order_notional_cap(bar.close)
            .ok_or("order notional cap unavailable")?;
        let notional = r
            .sized_notional(margin, p.gross_leverage, cm, cap)
            .ok_or("commission not measured at this size")?;
        let sides = if p.long_only {
            &[Side::Long][..]
        } else {
            &[Side::Long, Side::Short][..]
        };
        for &side in sides {
            let cost = r
                .book_cost(notional, side == Side::Long)
                .ok_or("order book too thin for this size")?;
            let price = bar.close * (1.0 + side.sign() * cost * cm);
            let qty = r
                .order_qty(notional / price, price)
                .ok_or("below the minimum order size or lot step")?;
            // NOTE(agents): Keep this check at decision time with the SAME budget the fill
            //               will use (`pending_slot`); checking a different size let
            //               rounding unbalance fills.
            // Lot rounding must not unbalance the legs: a contract whose
            // step is coarse for this slot size cannot be held neutrally.
            if !p.long_only && qty * price < notional * (1.0 - NEUTRAL_TOLERANCE / 2.0) {
                return Err("lot step too coarse to balance the legs");
            }
            if !r.leverage_allowed(qty * price, p.gross_leverage) {
                return Err("leverage not allowed at this size (margin tier)");
            }
            if research::on(&research::FEE_GATE) {
                let n = qty * price;
                let round_trip = (r.fee(n).ok_or("commission not measured at this size")? * 2.0
                    / n
                    + r.book_cost(n, true).unwrap_or(0.0)
                    + r.book_cost(n, false)
                        .ok_or("order book too thin for this size")?)
                    * cm
                    * 100.0;
                match eff_pct(p.risk.take_profit_pct, p.risk.atr_tp_mult, atr_pct(m, s, t)) {
                    Some(tp) if tp * TP_USE_FRACTION > round_trip => {}
                    _ => return Err("take-profit does not cover the round trip's fees and spread"),
                }
            }
        }
    }
    Ok(value)
}

/// Targets of a rebalance at bar `t`: the best-ranked eligible symbols per side.
fn ranked_targets(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    t: usize,
    p: &XsParams,
    budget: Option<(f64, f64)>,
) -> BTreeMap<usize, Side> {
    ranked_targets_notes(m, scores, t, p, budget, None)
}

/// `ranked_targets`, optionally explaining every coin's fate into `notes`.
fn ranked_targets_notes(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    t: usize,
    p: &XsParams,
    budget: Option<(f64, f64)>,
    mut notes: Option<&mut Vec<CandidateNote>>,
) -> BTreeMap<usize, Side> {
    let mut out = BTreeMap::new();
    if p.top == 0 {
        return out;
    }
    let block_all = |notes: &mut Option<&mut Vec<CandidateNote>>, why: &str| {
        if let Some(n) = notes.as_mut() {
            n.push(CandidateNote {
                symbol: "(all coins)".into(),
                signal: None,
                rank: None,
                outcome: format!("all entries blocked: {why}"),
            });
        }
    };
    let zones_on = p.risk.knife_drop_pct.is_some()
        || p.risk.zone_laggard_pct.is_some()
        || p.risk.zone_spy_pullback_pct.is_some();
    let mk = if zones_on { zone_market(m, t) } else { None };
    if zones_on && mk.is_none() {
        block_all(&mut notes, "market state unknown (loss zones fail closed)");
        return out; // zones enabled but market state unknown: fail closed, no entries
    }
    if let (Some(pb), Some(mk)) = (p.risk.zone_spy_pullback_pct, mk.as_ref()) {
        if mk.ref_ret4h < -pb
            && p.risk
                .zone_spy_slope_pct
                .is_none_or(|sl| mk.ref_slope >= sl)
        {
            block_all(
                &mut notes,
                "SPY pullback zone (sharp 4h fall inside a rising 24h average)",
            );
            return out; // Z4: a sharp pullback inside a still-rising 24h average
        }
    }
    let mut ranked: Vec<(usize, f64)> = Vec::new();
    for s in 0..m.symbols.len() {
        match candidate_value(m, scores, s, t, p, budget, mk.as_ref()) {
            Ok(value) => ranked.push((s, value)),
            Err(why) => {
                if let Some(n) = notes.as_mut() {
                    n.push(CandidateNote {
                        symbol: m.symbols[s].clone(),
                        signal: signal_value(m, scores, s, t, p),
                        rank: None,
                        outcome: format!("rejected: {why}"),
                    });
                }
            }
        }
    }
    ranked.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
    if ranked.len() < p.slots() {
        if let Some(n) = notes.as_mut() {
            for (i, &(s, v)) in ranked.iter().enumerate() {
                n.push(CandidateNote {
                    symbol: m.symbols[s].clone(),
                    signal: Some(v),
                    rank: Some(i + 1),
                    outcome: format!(
                        "eligible, but only {} coins are eligible and the basket needs {}: no entries",
                        ranked.len(),
                        p.slots()
                    ),
                });
            }
        }
        return out;
    }
    for &(s, _) in &ranked[..p.top] {
        out.insert(s, Side::Long);
    }
    if !p.long_only {
        for &(s, _) in &ranked[ranked.len() - p.top..] {
            out.insert(s, Side::Short);
        }
    }
    if let Some(n) = notes.as_mut() {
        for (i, &(s, v)) in ranked.iter().enumerate() {
            n.push(CandidateNote {
                symbol: m.symbols[s].clone(),
                signal: Some(v),
                rank: Some(i + 1),
                outcome: if out.contains_key(&s) {
                    "selected for entry".into()
                } else {
                    format!(
                        "eligible but ranked #{} (only the best {} are entered)",
                        i + 1,
                        p.top
                    )
                },
            });
        }
    }
    out
}

// NOTE(agents): Balance-aware sizing for every account size: tries the configured basket, then
//               smaller ones, until each slot passes the real lot, minimum, depth and leverage
//               checks. Returns the slot budget so execution reuses it exactly.
//               User rule (2026-10-03): NEVER halve the budget. Every slot gets the full share
//               of the free balance: no half size after drawdowns, no half slot reserved for an
//               average-down add (an add only fills from free balance that happens to remain).
/// Reduce the basket until the balance can fund the contracts' actual lot rules.
fn funded_targets(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    t: usize,
    p: &XsParams,
    equity: f64,
    cost_mult: f64,
) -> (BTreeMap<usize, Side>, usize, f64) {
    if equity < MIN_ENTRY_BALANCE {
        return (BTreeMap::new(), 0, 0.0);
    }
    let sides = if p.long_only { 1 } else { 2 };
    for top in (1..=p.top.min(m.symbols.len() / sides)).rev() {
        let mut adaptive = p.clone();
        adaptive.top = top;
        let budget = equity * SLOT_HEADROOM / adaptive.slots() as f64;
        let target = ranked_targets(m, scores, t, &adaptive, Some((budget, cost_mult)));
        if target.len() == adaptive.slots() {
            return (target, top, budget);
        }
    }
    (BTreeMap::new(), 0, 0.0)
}

/// `funded_targets` plus the explanation of every coin's fate at the basket size actually used.
fn funded_targets_notes(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    t: usize,
    p: &XsParams,
    equity: f64,
    cost_mult: f64,
) -> (BTreeMap<usize, Side>, usize, f64, Vec<CandidateNote>) {
    let mut notes = Vec::new();
    if equity < MIN_ENTRY_BALANCE {
        return (BTreeMap::new(), 0, 0.0, notes);
    }
    let sides = if p.long_only { 1 } else { 2 };
    for top in (1..=p.top.min(m.symbols.len() / sides)).rev() {
        let mut adaptive = p.clone();
        adaptive.top = top;
        let budget = equity * SLOT_HEADROOM / adaptive.slots() as f64;
        notes.clear();
        let target = ranked_targets_notes(
            m,
            scores,
            t,
            &adaptive,
            Some((budget, cost_mult)),
            Some(&mut notes),
        );
        if target.len() == adaptive.slots() {
            return (target, top, budget, notes);
        }
    }
    (BTreeMap::new(), 0, 0.0, notes)
}

impl XsPortfolio {
    /// Empty portfolio starting with `equity` USD.
    pub fn new(equity: f64) -> Self {
        XsPortfolio {
            equity,
            execution_error: None,
            rejected_rebalances: 0,
            effective_top: 0,
            allocation_note: String::new(),
            reserved: 0.0,
            start_equity: equity,
            positions: vec![],
            pending_targets: None,
            pending_params: None,
            pending_not_before: 0,
            pending_decided_at: None,
            pending_slot: 0.0,
            trades: vec![],
            peak: equity,
            risk_peak: equity,
            max_dd: 0.0,
            liquidations: 0,
            cost_mult: 1.0,
            entries_allowed: true,
            rebalance_equity: equity,
            flatten_next: false,
            stopped_out: false,
            resume_not_before: None,
            resume_pending: false,
            flatten_not_before: 0,
            flatten_decided_at: None,
            pending_weights: BTreeMap::new(),
            events: None,
        }
    }

    /// Start recording decisions, skipped/denied entries and gates.
    pub fn enable_events(&mut self) {
        if self.events.is_none() {
            self.events = Some(Vec::new());
        }
    }

    /// Take the events recorded since the last call (empty when logging is off).
    pub fn take_events(&mut self) -> Vec<EngineEvent> {
        self.events.as_mut().map(std::mem::take).unwrap_or_default()
    }

    /// Whether this portfolio records engine events.
    fn logging(&self) -> bool {
        self.events.is_some()
    }

    /// Records one engine event (at most 20,000 per portfolio).
    fn log(
        &mut self,
        ts_ms: i64,
        kind: &'static str,
        symbol: Option<&str>,
        detail: serde_json::Value,
    ) {
        if let Some(ev) = self.events.as_mut() {
            if ev.len() < 20_000 {
                ev.push(EngineEvent {
                    ts_ms,
                    kind,
                    symbol: symbol.map(str::to_string),
                    detail,
                });
            }
        }
    }

    /// Sets the fee/slippage multiplier (stress tests use 1.5).
    pub fn with_cost_mult(mut self, k: f64) -> Self {
        self.cost_mult = k;
        self
    }

    /// Mirror a deposit (+) or withdrawal (-) on the real account: equity changes
    /// and so does the P&L baseline, so cash moved is never counted as profit or
    /// loss. Later positions are sized from the new equity.
    #[cfg(test)]
    pub fn cash_flow(&mut self, delta: f64) {
        self.equity += delta;
        self.start_equity += delta;
        self.peak += delta;
        self.risk_peak = (self.risk_peak + delta).max(0.0);
        self.rebalance_equity += delta;
    }

    /// Point positions and pending targets at `symbols` by NAME. The live symbol
    /// list is re-fetched every bar, so indices shift when a symbol joins the list;
    /// a held position whose symbol is gone fails explicitly: exchange settlement
    /// data is required, and a last-mark exit must never be invented.
    pub fn reindex(&mut self, symbols: &[String]) {
        let idx: std::collections::HashMap<&str, usize> = symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (s.as_str(), i))
            .collect();
        let mut i = 0;
        while i < self.positions.len() {
            match idx.get(self.positions[i].symbol.as_str()) {
                Some(&s) => {
                    self.positions[i].sym = s;
                    i += 1;
                }
                None => {
                    self.execution_error = Some(format!(
                        "{}: held symbol missing; exchange settlement data required",
                        self.positions[i].symbol
                    ));
                    return;
                }
            }
        }
        if let Some(pending) = self.pending_targets.as_mut() {
            pending.retain_mut(|(s, name, _)| idx.get(name.as_str()).map(|&n| *s = n).is_some());
        }
    }

    /// Close position i at `price` (already including any market-order cost), paying this
    /// account's commission for the exit size (rules measured at entry; the entry's rate
    /// only if those are missing).
    fn close(&mut self, i: usize, ts: i64, price: f64, reason: &str) {
        let p = self.positions.remove(i);
        let gross = p.side.sign() * (price - p.entry) * p.qty;
        let fee = p
            .entry_rules
            .as_ref()
            .and_then(|r| r.fee(price * p.qty))
            .unwrap_or(price * p.qty * p.fee_rate)
            * self.cost_mult;
        self.equity += gross - fee;
        let fees = p.fees + fee;
        let pnl = gross - fees - p.funding;
        let risk = p.margin.max(1e-9);
        self.trades.push(Trade {
            symbol: p.symbol,
            side: p.side,
            entry_ts: p.entry_ts,
            exit_ts: ts,
            entry: p.entry,
            exit: price,
            qty: p.qty,
            leverage: p.leverage,
            fees,
            funding: p.funding,
            pnl,
            r: pnl / risk,
            reason: reason.into(),
            decided_ts: p.decided_ts,
        });
    }

    // NOTE(agents): A HELD position must always stay manageable (exits, funding, liquidation)
    //               even when the market has no fresh rules for it, e.g. after turnover moved it
    //               out of the measured top 20 or a book measurement failed. Fresh rules are used
    //               when present; otherwise the rules measured at entry (real, older). New
    //               orders (entries, adds) still require fresh rules. The service also measures
    //               held symbols every refresh, so the fallback is a last resort.
    /// The rules to manage held position `i` with: fresh, else those measured at entry.
    fn held_rules<'a>(&'a self, m: &'a Market, i: usize) -> Option<&'a super::rules::Rules> {
        let pos = &self.positions[i];
        m.rules(pos.sym).or(pos.entry_rules.as_ref())
    }

    /// A close must fit the measured book. Extrapolated or nonpositive prices
    /// invalidate the simulation rather than becoming invented fills.
    fn exit_price(&self, m: &Market, i: usize, reference: f64) -> Option<f64> {
        let pos = &self.positions[i];
        let cost = self
            .held_rules(m, i)?
            .book_cost(pos.qty * reference, pos.side == Side::Short)?;
        let price = reference * (1.0 - pos.side.sign() * cost * self.cost_mult);
        (price.is_finite() && price > 0.0).then_some(price)
    }

    /// Closes position `i` at the market exit price (fees and book cost); false when no exit price exists.
    fn close_market(
        &mut self,
        m: &Market,
        i: usize,
        ts: i64,
        reference: f64,
        reason: &str,
    ) -> bool {
        if let Some(price) = self.exit_price(m, i, reference) {
            self.close(i, ts, price, reason);
            true
        } else {
            self.execution_error = Some(format!(
                "{}: {reason} cannot fill within measured book depth",
                self.positions[i].symbol
            ));
            false
        }
    }

    // NOTE(agents): Funding (none for US stocks: always zero) is valued at the MARK price and debited from free balance
    //               first, then the isolated margin, which moves the liquidation price. Never value
    //               it at traded prices.
    /// Funding consumes free cash first, then this position's isolated margin.
    fn charge_funding(&mut self, m: &Market, i: usize, ts: i64, rate: f64, mark: f64) {
        if self.positions[i].last_funding_ts == Some(ts) {
            return;
        }
        let cost = self.positions[i].side.sign() * rate * self.positions[i].qty * mark;
        let debit = (cost - self.available().max(0.0)).max(0.0);
        self.equity -= cost;
        let rules = self.held_rules(m, i).cloned();
        let pos = &mut self.positions[i];
        pos.funding += cost;
        pos.last_funding_ts = Some(ts);
        pos.margin = (pos.margin - debit).max(0.0);
        if let Some(price) = rules.and_then(|r| {
            r.liquidation_price(pos.side, pos.qty, pos.entry, pos.margin, self.cost_mult)
        }) {
            pos.liquidation = price;
        } else {
            self.execution_error = Some(format!(
                "{}: no valid liquidation tier after funding",
                pos.symbol
            ));
        }
    }

    /// Closes position `i` at its liquidation price; the loss is floored at margin plus fees.
    fn liquidate(&mut self, i: usize, ts: i64) {
        let (liq, margin) = (self.positions[i].liquidation, self.positions[i].margin);
        self.liquidations += 1;
        self.close(i, ts, liq, "Liquidated");
        if let Some(tr) = self.trades.last_mut() {
            let floor = -margin - tr.fees - tr.funding;
            if tr.pnl < floor {
                self.equity += floor - tr.pnl;
                tr.pnl = floor;
            }
        }
    }

    // NOTE(agents): Only LOCKS IN exits (`live_exit`); it never closes positions or moves cash
    //               itself, because funding for the current bar is charged when the bar is
    //               stepped. Pass MARK prices only (wick-safe), never last-trade prices.
    /// The live monitor's check on fresh mark prices (`marks`: symbol -> mark).
    /// Locks in an exit for every position whose stop the mark has reached and,
    /// if marked equity is `DRAWDOWN_STOP_PCT` below its peak, stops out: every
    /// position gets an exit and nothing new opens. Returns what it did.
    pub fn live_check(
        &mut self,
        marks: &std::collections::HashMap<String, f64>,
        now_ms: i64,
    ) -> Vec<String> {
        let mut done = Vec::new();
        let mut live_events: Vec<(String, f64, f64)> = Vec::new();
        for pos in self.positions.iter_mut().filter(|p| p.live_exit.is_none()) {
            let (Some(&mark), Some(stop)) = (marks.get(&pos.symbol), pos.stop) else {
                continue;
            };
            if pos.side.sign() * (mark - stop) <= 0.0 {
                pos.live_exit = Some((now_ms, mark, "Stop (live mark)".into()));
                done.push(format!("{} stop hit at mark {mark}", pos.symbol));
                live_events.push((pos.symbol.clone(), mark, stop));
            }
        }
        for (name, mark, stop) in live_events {
            self.log(now_ms, "live_stop_hit", Some(&name), serde_json::json!({
                "mark_price": mark, "stop_level": stop,
                "note": "locked in mid-bar by the live mark-price monitor; booked with the next bar",
            }));
        }
        // A portfolio risk decision needs current prices for every open leg.
        // Individual stops above remain active while another subscription is
        // stale; never fabricate a contemporaneous liquidation/exit price from
        // that leg's previous candle mark.
        if self
            .positions
            .iter()
            .any(|p| p.live_exit.is_none() && !marks.contains_key(&p.symbol))
        {
            return done;
        }
        let marked = self.equity
            + self
                .positions
                .iter()
                .map(|p| {
                    let price = p
                        .live_exit
                        .as_ref()
                        .map(|x| x.1)
                        .or_else(|| marks.get(&p.symbol).copied())
                        .expect("all live valuations prechecked");
                    p.side.sign() * (price - p.entry) * p.qty
                })
                .sum::<f64>();
        let peak = self.peak.max(marked);
        self.peak = peak;
        if peak > 0.0 {
            self.max_dd = self.max_dd.max((peak - marked) / peak * 100.0);
        }
        let risk_peak = self.risk_peak.max(marked);
        self.risk_peak = risk_peak;
        if !self.stopped_out
            && !self.positions.is_empty()
            && risk_peak > 0.0
            && (risk_peak - marked) / risk_peak * 100.0 >= DRAWDOWN_STOP_PCT
        {
            self.stopped_out = true;
            self.resume_not_before = Some(now_ms + DRAWDOWN_COOLDOWN_MS);
            self.resume_pending = false;
            self.pending_targets = None;
            self.pending_params = None;
            self.pending_slot = 0.0;
            self.pending_weights.clear();
            for pos in self.positions.iter_mut().filter(|p| p.live_exit.is_none()) {
                let price = marks
                    .get(&pos.symbol)
                    .copied()
                    .expect("all live valuations prechecked");
                pos.live_exit = Some((now_ms, price, "Drawdown stop (live mark)".into()));
            }
            done.push(format!(
                "drawdown stop: marked equity {marked:.2} is {:.1}% below its cycle peak {risk_peak:.2}; closing everything, then re-evaluating after a 1-minute cooldown",
                (risk_peak - marked) / risk_peak * 100.0
            ));
        }
        done
    }

    /// Persist these when the live monitor observes a new peak or drawdown,
    /// even if it did not lock an exit.
    #[cfg(test)]
    pub fn risk_watermarks(&self) -> (f64, f64, f64) {
        (self.peak, self.max_dd, self.risk_peak)
    }

    /// Cancel queued entries when the report changes; retain risk exits.
    // NOTE(agents): Interval switch (paper trades the best interval, user request 2026-10-06):
    //               the service first closes every position at real prices, then moves the flat
    //               portfolio onto the new interval's clock. Closed trades keep their results
    //               (times re-mapped for display); pending decisions are dropped, so the new
    //               interval's next close decides afresh. Equity, peak and drawdown carry over.
    /// Move a FLAT portfolio onto another bar clock: every stored clock time is mapped by `f`.
    pub fn reclock(&mut self, f: impl Fn(i64) -> anyhow::Result<i64>) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.positions.is_empty(),
            "only a flat portfolio can change interval"
        );
        for t in &mut self.trades {
            t.entry_ts = f(t.entry_ts)?;
            t.exit_ts = f(t.exit_ts)?;
            t.decided_ts = t.decided_ts.map(&f).transpose()?;
        }
        self.pending_targets = None;
        self.pending_params = None;
        self.pending_slot = 0.0;
        self.pending_weights.clear();
        self.pending_not_before = 0;
        self.pending_decided_at = None;
        self.flatten_next = false;
        self.flatten_not_before = 0;
        self.flatten_decided_at = None;
        self.resume_not_before = self.resume_not_before.map(&f).transpose()?;
        Ok(())
    }

    /// Whether a queued buy was decided with the currently selected settings. The
    /// captured top may be reduced by account sizing without changing the strategy.
    pub fn pending_buy_uses(&self, selected: &XsParams) -> bool {
        match (&self.pending_targets, &self.pending_params) {
            (Some(_), Some(captured)) => {
                if captured.top > selected.top {
                    return false;
                }
                let mut captured = captured.clone();
                captured.top = selected.top;
                captured == *selected
            }
            _ => true,
        }
    }

    /// Turns the entry gate on or off and clears pending targets.
    pub fn install_entry_gate(&mut self, allowed: bool) {
        self.entries_allowed = allowed;
        self.pending_targets = None;
        self.pending_params = None;
        self.pending_slot = 0.0;
        self.pending_weights.clear();
        self.effective_top = 0;
        self.allocation_note = if allowed {
            "settings changed: waiting for the next rebalance close".into()
        } else {
            "entry gate disabled".into()
        };
        if !allowed {
            for pos in &mut self.positions {
                if pos.next_action == Some(NextAction::Add) {
                    pos.next_action = None;
                }
            }
        }
    }

    /// Free balance: equity minus this bot's posted margins and the real
    /// account's margin committed elsewhere.
    pub fn available(&self) -> f64 {
        self.equity - self.reserved - self.positions.iter().map(|x| x.margin).sum::<f64>()
    }

    /// Market order at bar t's open spending `budget` USDT on margin + fee, under
    /// moomoo lot rules, the measured order-book cost for this size, the account's
    /// taker fee and the real margin tier. Not placed when the symbol has no
    /// rules, the book is too thin, or the account lacks the cash.
    /// Why an entry order was not placed (event log only).
    fn skip_entry(
        &mut self,
        m: &Market,
        t: usize,
        sym: usize,
        why: &str,
        extra: serde_json::Value,
    ) {
        if self.logging() {
            let sym_name = m.symbols[sym].clone();
            self.log(
                m.ts[t],
                "entry_skipped",
                Some(&sym_name),
                serde_json::json!({"reason": why, "available_balance": self.available(), "extra": extra}),
            );
        }
    }

    /// Opens a position in `sym` at bar `t`'s open with `budget` (rules, book cost, fees, whole shares); skips with a logged reason when it cannot.
    fn open(&mut self, m: &Market, t: usize, sym: usize, side: Side, budget: f64, p: &XsParams) {
        let atr = atr_pct(m, sym, t);
        let (Some(b), Some(r), Some(mark)) = (m.bars[sym][t], m.rules(sym), m.marks[sym][t]) else {
            self.skip_entry(
                m,
                t,
                sym,
                "missing bar, moomoo rules or mark candle at the fill open",
                serde_json::Value::Null,
            );
            return;
        };
        if budget <= 0.0 {
            self.skip_entry(
                m,
                t,
                sym,
                "no budget for this slot",
                serde_json::json!({"budget": budget}),
            );
            return;
        }
        let (buy, leverage, cm) = (side == Side::Long, p.gross_leverage, self.cost_mult);
        // Size so that margin + entry fee == budget (the book cost is not a separate
        // payment: it is already in the worse fill price).
        let Some(cap) = r.order_notional_cap(b.open) else {
            self.skip_entry(
                m,
                t,
                sym,
                "order notional cap unavailable",
                serde_json::Value::Null,
            );
            return;
        };
        let Some(notional) = r.sized_notional(budget, leverage, cm, cap) else {
            self.skip_entry(
                m,
                t,
                sym,
                "commission not measured at this size",
                serde_json::json!({"budget": budget}),
            );
            return;
        };
        let (Some(cost), Some(_)) = (r.book_cost(notional, buy), r.book_cost(notional, !buy))
        else {
            self.skip_entry(
                m,
                t,
                sym,
                "order book too thin for this size",
                serde_json::json!({"notional": notional}),
            );
            return;
        };
        let entry = b.open * (1.0 + side.sign() * cost * cm);
        let Some(qty) = r.order_qty(notional / entry, entry) else {
            self.skip_entry(
                m,
                t,
                sym,
                "below the minimum order size or lot step",
                serde_json::json!({"notional": notional, "price": entry}),
            );
            return;
        };
        if !r.leverage_allowed(qty * entry, leverage) {
            self.skip_entry(
                m,
                t,
                sym,
                "leverage not allowed at this size (margin tier)",
                serde_json::json!({"notional": qty * entry, "leverage": leverage}),
            );
            return;
        }
        let Some(liquidation) = r.liquidation_price(side, qty, entry, qty * entry / leverage, cm)
        else {
            self.skip_entry(
                m,
                t,
                sym,
                "no liquidation price available (margin tiers)",
                serde_json::Value::Null,
            );
            return;
        };
        let Some(fee) = r.fee(qty * entry).map(|f| f * cm) else {
            self.skip_entry(
                m,
                t,
                sym,
                "commission not measured at this size",
                serde_json::json!({"notional": qty * entry}),
            );
            return;
        };
        if qty * entry / leverage + fee > self.available() + 1e-9 {
            // the broker would reject: not enough available balance
            self.skip_entry(
                m,
                t,
                sym,
                "not enough available balance (the broker would reject the order)",
                serde_json::json!({"needed_margin_plus_fee": qty * entry / leverage + fee}),
            );
            return;
        }
        if self.logging() {
            let sym_name = m.symbols[sym].clone();
            self.log(
                m.ts[t],
                "entry_filled",
                Some(&sym_name),
                serde_json::json!({
                    "side": format!("{side:?}"), "fill_price": entry, "open_price": b.open, "book_cost": cost,
                    "qty": qty, "notional": qty * entry, "margin": qty * entry / leverage, "fee": fee,
                    "leverage": leverage, "liquidation_price": liquidation, "atr_pct": atr,
                    "stop_pct": eff_pct(p.stop_pct, p.risk.atr_stop_mult, atr),
                    "trail_pct": eff_pct(p.risk.trail_pct, p.risk.atr_trail_mult, atr),
                    "take_profit_decided_pct": eff_pct(p.risk.take_profit_pct, p.risk.atr_tp_mult, atr),
                    "take_profit_used_pct": eff_pct(p.risk.take_profit_pct, p.risk.atr_tp_mult, atr).map(|v| v * TP_USE_FRACTION),
                    "budget": budget,
                }),
            );
        }
        self.equity -= fee;
        self.positions.push(XsPosition {
            sym,
            symbol: m.symbols[sym].clone(),
            side,
            entry_ts: m.ts[t],
            entry,
            qty,
            margin: qty * entry / leverage,
            leverage,
            liquidation,
            stop: eff_pct(p.stop_pct, p.risk.atr_stop_mult, atr)
                .map(|s| entry * (1.0 - side.sign() * s / 100.0)),
            peak: entry,
            atr_pct: atr,
            decided_ts: self.pending_decided_at,
            fees: fee,
            funding: 0.0,
            last_funding_ts: None,
            mark: mark.open,
            next_action: None,
            action_not_before: 0,
            action_decided_at: None,
            added: false,
            fee_rate: fee / cm / (qty * entry),
            live_exit: None,
            entry_rules: Some(r.clone()),
        });
    }

    /// Add the position's current size again at bar t's open (same rules as
    /// `open`); entry, margin, liquidation and stop move to the new average.
    fn add_skipped(&mut self, m: &Market, t: usize, i: usize, why: &str) {
        if self.logging() {
            let name = self.positions[i].symbol.clone();
            self.log(
                m.ts[t],
                "add_skipped",
                Some(&name),
                serde_json::json!({"reason": why, "available_balance": self.available()}),
            );
        }
    }

    /// Adds to position `i` at bar `t`'s open (same size again), if the rules and book allow it.
    fn add(&mut self, m: &Market, t: usize, i: usize, p: &XsParams) {
        let (sym, side, qty0) = (
            self.positions[i].sym,
            self.positions[i].side,
            self.positions[i].qty,
        );
        let (Some(b), Some(r)) = (m.bars[sym][t], m.rules(sym)) else {
            self.add_skipped(m, t, i, "missing bar or moomoo rules");
            return;
        };
        let s = side.sign();
        let Some(cost) = r.book_cost(qty0 * b.open, side == Side::Long) else {
            self.add_skipped(m, t, i, "order book too thin for the add");
            return;
        };
        let px = b.open * (1.0 + s * cost * self.cost_mult);
        let Some(q) = r.order_qty(qty0, px) else {
            self.add_skipped(m, t, i, "add below the minimum order size or lot step");
            return;
        };
        if !r.leverage_allowed((qty0 + q) * px, self.positions[i].leverage) {
            self.add_skipped(m, t, i, "leverage not allowed at the combined size");
            return;
        }
        let entry = (self.positions[i].entry * qty0 + px * q) / (qty0 + q);
        let margin = self.positions[i].margin + q * px / self.positions[i].leverage;
        let Some(liquidation) = r.liquidation_price(side, qty0 + q, entry, margin, self.cost_mult)
        else {
            self.add_skipped(m, t, i, "no liquidation price available");
            return;
        };
        let Some(fee) = r.fee(q * px).map(|f| f * self.cost_mult) else {
            self.add_skipped(m, t, i, "commission not measured for the add");
            return;
        };
        if self.stopped_out
            || self.available() < MIN_ENTRY_BALANCE
            || q * px / self.positions[i].leverage + fee > self.available() + 1e-9
        {
            // the broker would reject: not enough available balance
            self.add_skipped(
                m,
                t,
                i,
                "not enough available balance, or the drawdown stop is active",
            );
            return;
        }
        if self.logging() {
            let name = self.positions[i].symbol.clone();
            self.log(
                m.ts[t],
                "add_filled",
                Some(&name),
                serde_json::json!({"price": px, "qty_added": q, "new_entry": entry, "fee": fee}),
            );
        }
        self.equity -= fee;
        let pos = &mut self.positions[i];
        let qty = pos.qty + q;
        pos.entry = (pos.entry * pos.qty + px * q) / qty;
        pos.qty = qty;
        pos.margin += q * px / pos.leverage;
        pos.fees += fee;
        pos.added = true;
        pos.liquidation = liquidation;
        pos.stop = eff_pct(p.stop_pct, p.risk.atr_stop_mult, pos.atr_pct)
            .map(|st| pos.entry * (1.0 - s * st / 100.0));
    }

    /// Advances the portfolio by bar `t`: fills pending orders at the open, then exits, stops and the rebalance decision at the close.
    pub fn step(&mut self, m: &Market, scores: &[Vec<Option<Score>>], t: usize, p: &XsParams) {
        if self.execution_error.is_some() {
            return;
        }
        if p.hold == 0
            || p.top == 0
            || !p.gross_leverage.is_finite()
            || p.gross_leverage <= 0.0
            || !self.cost_mult.is_finite()
            || self.cost_mult <= 0.0
        {
            self.execution_error = Some("invalid strategy parameters or cost multiplier".into());
            return;
        }
        let ts = m.ts[t];
        // Held positions need actual mark data before any state mutation.
        if let Some(pos) = self.positions.iter().find(|pos| {
            m.marks
                .get(pos.sym)
                .and_then(|r| r.get(t))
                .copied()
                .flatten()
                .is_none()
        }) {
            self.execution_error = Some(format!("{}: missing mark candle at {ts}", pos.symbol));
            return;
        }
        let mut i = 0;
        while i < self.positions.len() {
            let sym = self.positions[i].sym;
            let mark = m.marks[sym][t].expect("prechecked").open;
            for &(_, rate) in m.funding[sym].iter().filter(|(f, _)| *f == ts) {
                self.charge_funding(m, i, ts, rate, mark);
            }
            if self.execution_error.is_some() {
                return;
            }
            // NOTE(agents): Order matters. Funding at this open, then mark-price gap liquidation,
            //               THEN queued stops/exits/adds. Reordering lets a queued stop book a loss
            //               beyond the isolated margin with zero liquidations (old audit finding
            //               1).
            // Gap liquidation precedes every queued discretionary action.
            if self.positions[i].side.sign() * (mark - self.positions[i].liquidation) <= 0.0 {
                self.liquidate(i, ts);
            } else {
                i += 1;
            }
        }
        // NOTE(agents): Live exits (locked in by the WebSocket monitor, `live_check`) are booked
        //               here: after this bar's opening funding and gap liquidation, before any
        //               queued action, at the recorded mark price and time. Booking them here
        //               keeps funding and the settlement ledger exact.
        let mut i = 0;
        while i < self.positions.len() {
            match self.positions[i].live_exit.clone() {
                Some((at, price, reason)) if at < ts + BAR_MS => {
                    if !self.close_market(m, i, at.max(ts), price, &reason) {
                        return;
                    }
                }
                _ => i += 1,
            }
        }
        let rebalancing = self.pending_targets.is_some() && ts >= self.pending_not_before;
        // 0. Actions decided at the previous close execute at this open. An add
        //    that coincides with a rebalance is skipped: the rebalance decides.
        if self.flatten_next && ts >= self.flatten_not_before {
            self.flatten_next = false;
            self.pending_targets = None;
            let mut i = 0;
            while i < self.positions.len() {
                let sym = self.positions[i].sym;
                match m.bars[sym][t] {
                    Some(b) => {
                        let reason = if self.stopped_out {
                            "Drawdown stop"
                        } else {
                            "Breaker"
                        };
                        if !self.close_market(m, i, ts, b.open, reason) {
                            return;
                        }
                    }
                    None => {
                        self.flatten_next = true;
                        i += 1;
                    }
                }
            }
        }
        let mut i = 0;
        while i < self.positions.len() {
            let sym = self.positions[i].sym;
            if ts < self.positions[i].action_not_before {
                i += 1;
                continue;
            }
            let action = self.positions[i].next_action.take();
            match (action, m.bars[sym][t]) {
                (Some(NextAction::Stop), Some(b)) => {
                    if !self.close_market(m, i, ts, b.open, "Stop (close)") {
                        return;
                    }
                    continue;
                }
                (Some(NextAction::TakeProfit), Some(b)) => {
                    if !self.close_market(m, i, ts, b.open, "Take profit (close)") {
                        return;
                    }
                    continue;
                }
                (Some(NextAction::Rebalance), Some(b)) => {
                    if !self.close_market(m, i, ts, b.open, "Rebalance") {
                        return;
                    }
                    continue;
                }
                (Some(action), None) => self.positions[i].next_action = Some(action),
                (Some(NextAction::Add), Some(_)) if !rebalancing && self.entries_allowed => {
                    if open_is_fair(m, sym, t) {
                        self.add(m, t, i, p)
                    } else {
                        // A wicked open: try the add again at the next open.
                        self.positions[i].next_action = Some(NextAction::Add);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        // 1. Rebalance decided at the previous close, executed at this open.
        if rebalancing {
            // Missing data or a wicked open on any target: the whole rebalance waits a
            // bar, so the basket still fills together.
            let wait = self
                .pending_targets
                .as_ref()
                .is_some_and(|target| target.iter().any(|&(sym, _, _)| !open_is_fair(m, sym, t)));
            if wait {
                self.pending_not_before = ts + BAR_MS;
                if self.logging() {
                    let bad: Vec<String> = self
                        .pending_targets
                        .as_ref()
                        .map(|tg| {
                            tg.iter()
                                .filter(|(sym, _, _)| !open_is_fair(m, *sym, t))
                                .map(|(_, name, _)| name.clone())
                                .collect()
                        })
                        .unwrap_or_default();
                    self.log(ts, "rebalance_waits", None, serde_json::json!({
                        "reason": "a target's open is wicked or its data is missing: the whole basket waits one bar",
                        "symbols": bad,
                    }));
                }
            } else if let Some(target) = self.pending_targets.take() {
                let execution = self.pending_params.take().unwrap_or_else(|| p.clone());
                let p = &execution;
                // NOTE(agents): Positions are always fully closed and reopened at a rebalance (no
                //               partial keep); simpler and keeps every leg at the decided slot
                //               size.
                // Every position closes so both sides are re-sized to neutral.
                let keep = research::on(&research::KEEP_ON_REBALANCE) && p.long_only;
                let kept = |pos: &XsPosition| {
                    keep && pos.next_action.is_none()
                        && target.iter().any(|x| x.0 == pos.sym && x.2 == pos.side)
                };
                let mut i = 0;
                while i < self.positions.len() {
                    if kept(&self.positions[i]) {
                        i += 1;
                        continue;
                    }
                    match m.bars[self.positions[i].sym][t] {
                        Some(b) => {
                            if !self.close_market(m, i, ts, b.open, "Rebalance") {
                                return;
                            }
                        }
                        None => {
                            self.positions[i].next_action = Some(NextAction::Rebalance);
                            i += 1;
                        }
                    }
                }
                // Each slot's budget (margin + entry fee) was fixed at the decision
                // so the base order AND its possible add both fit in the account.
                let base = std::mem::take(&mut self.pending_slot);
                // NOTE(agents): Volatility weights were fixed at the decision close
                //               (`decide_close`); a delayed fill (missing data, wick guard,
                //               latency) must not recompute them from later bars.
                let weights = std::mem::take(&mut self.pending_weights);
                let can_enter = self.entries_allowed
                    && !self.stopped_out
                    && !self.flatten_next
                    && self.positions.iter().all(kept)
                    && !target.is_empty();
                if !can_enter && self.logging() && !target.is_empty() {
                    self.log(ts, "entries_blocked", None, serde_json::json!({
                        "reason": "entries not allowed at the fill",
                        "entries_allowed": self.entries_allowed, "stopped_out": self.stopped_out,
                        "flatten_pending": self.flatten_next, "positions_still_open": self.positions.len(),
                        "targets": target.iter().map(|x| x.1.clone()).collect::<Vec<_>>(),
                    }));
                }
                if can_enter && self.available() < MIN_ENTRY_BALANCE {
                    self.allocation_note = format!(
                        "free balance {:.2} USDT below the {MIN_ENTRY_BALANCE} USDT minimum: no entries",
                        self.available()
                    );
                    if self.logging() {
                        let avail = self.available();
                        self.log(
                            ts,
                            "entries_blocked",
                            None,
                            serde_json::json!({
                                "reason": "free balance below the minimum",
                                "available_balance": avail, "minimum": MIN_ENTRY_BALANCE,
                                "targets": target.iter().map(|x| x.1.clone()).collect::<Vec<_>>(),
                            }),
                        );
                    }
                } else if can_enter {
                    let mut candidate = self.clone();
                    let base_len = self.events.as_ref().map(|e| e.len());
                    for (sym, name, side) in &target {
                        if candidate.positions.iter().any(|x| x.sym == *sym) {
                            continue;
                        }
                        let w = weights.get(name).copied().unwrap_or(1.0);
                        candidate.open(m, t, *sym, *side, base * w, p);
                    }
                    let filled = candidate.positions.len();
                    let long = candidate
                        .positions
                        .iter()
                        .filter(|p| p.side == Side::Long)
                        .map(|p| p.qty * p.entry)
                        .sum::<f64>();
                    let short = candidate
                        .positions
                        .iter()
                        .filter(|p| p.side == Side::Short)
                        .map(|p| p.qty * p.entry)
                        .sum::<f64>();
                    let gross = long + short;
                    // NOTE(agents): All legs or none: a partial fill would leave an unintended
                    //               basket (one-sided when market-neutral). Fills are simulated on
                    //               a clone and committed only if complete.
                    if candidate.positions.len() == target.len()
                        && gross > 0.0
                        && (p.long_only || (long - short).abs() / gross <= NEUTRAL_TOLERANCE)
                    {
                        candidate.resume_pending = false;
                        candidate.log(
                            ts,
                            "rebalance_committed",
                            None,
                            serde_json::json!({
                                "legs": filled, "gross_notional": gross, "slot_budget": base,
                            }),
                        );
                        *self = candidate;
                    } else {
                        self.rejected_rebalances += 1;
                        // Keep the skip reasons the attempted fills produced.
                        if let (Some(from), Some(ev)) = (base_len, candidate.events.take()) {
                            if let Some(mine) = self.events.as_mut() {
                                mine.extend(ev.into_iter().skip(from));
                            }
                        }
                        self.log(ts, "rebalance_rejected", None, serde_json::json!({
                            "reason": "all legs or none: not every target could be filled, so nothing was entered",
                            "filled_legs": filled, "targets": target.len(),
                        }));
                    }
                }
                self.rebalance_equity = self.equity + self.unrealized();
            }
        }
        // 2. Funding, liquidation and the intrabar stop (worst case first), then
        //    the close-based rules, executed at the next open.
        let mut i = 0;
        while i < self.positions.len() {
            let (sym, side) = (self.positions[i].sym, self.positions[i].side);
            let mark = m.marks[sym][t].expect("held marks validated at entry");
            if let Some(&(_, rate)) = m.funding[sym]
                .iter()
                .find(|(f, _)| *f > ts && *f < ts + BAR_MS)
            {
                // Non-boundary settlement prices cannot be recovered exactly from
                // 1m candles. Refuse that data rather than use an opening estimate.
                self.execution_error = Some(format!(
                    "{}: intrabar funding at {ts} requires finer mark data (rate {rate})",
                    self.positions[i].symbol
                ));
                return;
            }
            let Some(b) = m.bars[sym][t] else {
                i += 1;
                continue;
            };
            let pos = &self.positions[i];
            let s = side.sign();
            // NOTE(agents): Wick-safe (user rule, 2026-10-03): stops TRIGGER on the mark (midpoint)
            //               price, never on last-trade wicks; fills use traded prices and the
            //               measured book cost. Liquidation also uses the mark.
            let mark_adverse = if s > 0.0 { mark.low } else { mark.high };
            let through = |level: f64, price: f64| s * (price - level) <= 0.0;
            // Mark liquidation is checked first when both mark liquidation and
            // traded-price stop occur inside one candle: their ordering is unknown.
            if through(pos.liquidation, mark.open) || through(pos.liquidation, mark_adverse) {
                self.liquidate(i, ts);
                continue;
            }
            if let Some(st) = pos.stop {
                if through(st, mark.open) {
                    if !self.close_market(m, i, ts, b.open, "Stop (gap)") {
                        return;
                    }
                    continue;
                }
                if through(st, mark_adverse) {
                    if !self.close_market(m, i, ts, st, "Stop") {
                        return;
                    }
                    continue;
                }
            }
            let r = p.risk;
            let pos = &mut self.positions[i];
            pos.mark = mark.close;
            // Trailing / break-even: tighten the stop from this bar's favourable extreme.
            let trail = eff_pct(r.trail_pct, r.atr_trail_mult, pos.atr_pct);
            if trail.is_some() || r.breakeven_pct.is_some() {
                let best = if s > 0.0 { mark.high } else { mark.low };
                pos.peak = if pos.peak <= 0.0 {
                    best
                } else if s > 0.0 {
                    pos.peak.max(best)
                } else {
                    pos.peak.min(best)
                };
                let tighter = |old: Option<f64>, new: f64| match old {
                    Some(o) if s * (new - o) <= 0.0 => Some(o),
                    _ => Some(new),
                };
                if let Some(tr) = trail {
                    pos.stop = tighter(pos.stop, pos.peak * (1.0 - s * tr / 100.0));
                }
                if r.breakeven_pct
                    .is_some_and(|be| s * (pos.peak - pos.entry) / pos.entry * 100.0 >= be)
                {
                    pos.stop = tighter(pos.stop, pos.entry);
                }
            }
            let against = -s * (mark.close - pos.entry) / pos.entry * 100.0;
            let pending_exit = matches!(
                pos.next_action,
                Some(NextAction::Stop | NextAction::TakeProfit | NextAction::Rebalance)
            );
            if pending_exit {
                i += 1;
                continue;
            }
            let decided = if r.close_stop_pct.is_some_and(|c| against >= c)
                || (s < 0.0 && r.short_stop_pct.is_some_and(|c| against >= c))
            {
                Some(NextAction::Stop)
            } else if eff_pct(r.take_profit_pct, r.atr_tp_mult, pos.atr_pct)
                .is_some_and(|tp| -against >= tp * TP_USE_FRACTION)
            {
                Some(NextAction::TakeProfit)
            } else if !pos.added && r.add_pct.is_some_and(|a| against >= a) {
                Some(NextAction::Add)
            } else {
                None
            };
            // Re-reaching the same decision at a later close keeps its original bound
            // (otherwise the bound slides forward every bar and it never fills).
            let log_exit = if decided != pos.next_action {
                decided.map(|d| (pos.symbol.clone(), format!("{d:?}"), against, mark.close))
            } else {
                None
            };
            if decided != pos.next_action {
                pos.action_decided_at = decided.map(|_| ts);
                pos.action_not_before = if decided.is_some() {
                    decision_fill_bound(ts)
                } else {
                    0
                };
                pos.next_action = decided;
            }
            if let Some((name, kind, against, close)) = log_exit {
                if self.logging() {
                    self.log(
                        ts,
                        "exit_decided",
                        Some(&name),
                        serde_json::json!({
                            "action": kind, "against_entry_pct": against, "mark_close": close,
                            "fills_no_earlier_than_bar_after_next": true,
                        }),
                    );
                }
            }
            i += 1;
        }
        let end = ts + BAR_MS;
        for i in 0..self.positions.len() {
            let sym = self.positions[i].sym;
            // NOTE(agents): Do not turn this back into an error: the live market always loads
            //               funding stamped at the newest close, whose mark price (next open)
            //               doesn't exist yet. Erroring here permanently stopped the paper account.
            // The newest loaded bar's closing settlement is charged when the next
            // bar (and its real mark open) is processed.
            if t + 1 == m.ts.len() {
                break;
            }
            for &(_, rate) in m.funding[sym].iter().filter(|(f, _)| *f == end) {
                let mark =
                    m.ts.get(t + 1)
                        .filter(|&&time| time == end)
                        .and_then(|_| m.marks[sym].get(t + 1))
                        .copied()
                        .flatten()
                        .map(|b| b.open);
                let Some(mark) = mark else {
                    self.execution_error = Some(format!(
                        "{}: funding at end boundary {end} needs its real mark opening price",
                        self.positions[i].symbol
                    ));
                    return;
                };
                self.charge_funding(m, i, end, rate, mark);
            }
        }
        let marked = self.equity + self.unrealized();
        if let Some(bk) = p.risk.breaker_pct {
            if !self.positions.is_empty()
                && !self.flatten_next
                && self.rebalance_equity > 0.0
                && marked <= self.rebalance_equity * (1.0 - bk / 100.0)
            {
                self.flatten_next = true;
                self.flatten_not_before = decision_fill_bound(ts);
                self.flatten_decided_at = Some(ts);
                self.log(ts, "gate", None, serde_json::json!({
                    "gate": "portfolio breaker", "action": "flatten everything at the next open",
                    "marked_equity": marked, "equity_at_last_rebalance": self.rebalance_equity,
                    "breaker_pct": bk,
                }));
            }
        }
        let exit_margin = p
            .risk
            .trend_exit_margin_pct
            .or(p.risk.trend_margin_pct)
            .unwrap_or(0.0);
        let trend_flatten = p.risk.trend_exit
            && !self.positions.is_empty()
            && !self.flatten_next
            && p.regime == Regime::MarketTrend
            && !trend_ok_margin(m, t, p, exit_margin, false);
        if trend_flatten {
            // Down trend: go flat (decided at this close, filled at the next open).
            if self.logging() {
                let snap = trend_snapshot(m, t, p);
                self.log(ts, "gate", None, serde_json::json!({
                    "gate": "trend turned down", "action": "flatten everything at the next open", "trend": snap,
                }));
            }
            self.flatten_next = true;
            self.flatten_not_before = decision_fill_bound(ts);
            self.flatten_decided_at = Some(ts);
        }
        let peak = self.risk_peak.max(marked);
        self.risk_peak = peak;
        if !self.stopped_out && peak > 0.0 && (peak - marked) / peak * 100.0 >= DRAWDOWN_STOP_PCT {
            self.log(
                ts,
                "gate",
                None,
                serde_json::json!({
                    "gate": "25% drawdown stop", "action": "close everything and pause entries",
                    "marked_equity": marked, "cycle_peak": peak,
                    "drawdown_pct": (peak - marked) / peak * 100.0,
                }),
            );
            self.stopped_out = true;
            let pause = p
                .risk
                .resume_pause_bars
                .map_or(DRAWDOWN_COOLDOWN_MS, |b| b as i64 * BAR_MS);
            self.resume_not_before = Some(ts + BAR_MS + pause.max(DRAWDOWN_COOLDOWN_MS));
            self.resume_pending = false;
            self.pending_targets = None;
            self.pending_params = None;
            self.pending_slot = 0.0;
            self.pending_weights.clear();
            for pos in &mut self.positions {
                if pos.next_action == Some(NextAction::Add) {
                    pos.next_action = None;
                }
            }
            if !self.positions.is_empty() && !self.flatten_next {
                self.flatten_next = true;
                self.flatten_not_before = decision_fill_bound(ts);
                self.flatten_decided_at = Some(ts);
            }
        }
        // 3. Rebalance decision at aligned closes.
        self.decide_close(m, scores, t, p);
        self.peak = self.peak.max(marked);
        if self.peak > 0.0 {
            self.max_dd = self.max_dd.max((self.peak - marked) / self.peak * 100.0);
        }
    }

    // NOTE(agents): Decisions use only data up to bar t and fill at a later open
    //               (`defer_new_decisions` live). Targets, parameters and slot budget are frozen
    //               here; `install_entry_gate` cancels them when the entry gate changes (a new settings winner alone does not).
    /// Queue a decision using only information available at this close.
    pub fn decide_close(
        &mut self,
        m: &Market,
        scores: &[Vec<Option<Score>>],
        t: usize,
        p: &XsParams,
    ) {
        if self.stopped_out
            && self
                .resume_not_before
                .is_some_and(|at| m.ts[t] + BAR_MS >= at)
            && self.positions.is_empty()
            && self.entries_allowed
            && self.available() >= MIN_ENTRY_BALANCE
        {
            self.stopped_out = false;
            self.risk_peak = self.equity;
            self.rebalance_equity = self.equity;
            self.flatten_next = false;
            self.pending_targets = None;
            self.pending_params = None;
            self.pending_weights.clear();
            self.resume_pending = true;
        }
        let resume_decision =
            self.resume_pending && self.pending_targets.as_ref().is_none_or(Vec::is_empty);
        let flat_enter = research::on(&research::FLAT_ENTER)
            && self.positions.is_empty()
            && !self.stopped_out
            && self.entries_allowed
            && self.pending_targets.as_ref().is_none_or(Vec::is_empty);
        if p.hold > 0 && (is_rebalance(m.ts[t], p.hold) || resume_decision || flat_enter) {
            self.pending_not_before = decision_fill_bound(m.ts[t]);
            self.pending_decided_at = Some(m.ts[t]);
            let marked = self.equity + self.unrealized();
            let usable = marked - self.reserved;
            let regime = trend_ok(m, t, p);
            let logging = self.logging();
            let mut notes: Vec<CandidateNote> = Vec::new();
            // Three-tier drawdown protocol (entries only): the third tier opens no new position, the second halves-ish the size.
            let dd_pct = if self.risk_peak > 0.0 {
                ((self.risk_peak - marked) / self.risk_peak * 100.0).max(0.0)
            } else {
                0.0
            };
            let dd_tier = p
                .risk
                .grade_dd
                .map_or(Some(1.0), |tiers| drawdown_multiplier(dd_pct, tiers));
            let dd_mult = dd_tier.unwrap_or(1.0);
            let (target, top, slot) = if self.entries_allowed
                && regime
                && !self.stopped_out
                && dd_tier.is_some()
                && !loss_pause_active(&self.trades, p.risk.loss_pause, m.ts[t])
                && !roll_stop_active(&self.trades, p.risk.roll_stop, self.equity, m.ts[t])
            {
                if logging {
                    let (a, b, c, n) =
                        funded_targets_notes(m, scores, t, p, usable, self.cost_mult);
                    notes = n;
                    (a, b, c)
                } else {
                    funded_targets(m, scores, t, p, usable, self.cost_mult)
                }
            } else {
                (BTreeMap::new(), 0, 0.0)
            };
            let target_log: Vec<(String, String)> = if logging {
                target
                    .iter()
                    .map(|(s, side)| (m.symbols[*s].clone(), format!("{side:?}")))
                    .collect()
            } else {
                Vec::new()
            };
            self.effective_top = top;
            self.pending_slot = slot;
            self.allocation_note = if !self.entries_allowed {
                "entry gate disabled".into()
            } else if self.stopped_out {
                format!("drawdown exit: {DRAWDOWN_STOP_PCT}% reached; finish exits and wait 15 minutes, then re-evaluate entries")
            } else if !regime {
                "regime filter: SPY below its 24h average, no entries".into()
            } else if usable < MIN_ENTRY_BALANCE {
                format!("free balance {usable:.2} USDT below the {MIN_ENTRY_BALANCE} USDT minimum: no entries")
            } else if top == 0 {
                "balance, minimum orders or measured liquidity cannot fund the basket".into()
            } else if top < p.top {
                format!(
                    "balance-adjusted basket: {top} per side (configured maximum {})",
                    p.top
                )
            } else {
                format!("{top} funded per side")
            };
            let mut execution = p.clone();
            if top > 0 {
                execution.top = top;
            }
            self.pending_params = Some(execution);
            self.pending_weights = if p.risk.vol_scaled {
                decision_weights(m, t, &target)
            } else {
                BTreeMap::new()
            };
            if p.risk.grade_rank.is_some()
                || p.risk.grade_trend.is_some()
                || p.risk.grade_risk.is_some()
                || dd_mult != 1.0
            {
                grade_weights(
                    m,
                    scores,
                    t,
                    p,
                    &target,
                    slot,
                    usable,
                    dd_mult,
                    &mut self.pending_weights,
                );
            }
            self.pending_targets = Some(
                target
                    .into_iter()
                    .map(|(s, side)| (s, m.symbols[s].clone(), side))
                    .collect(),
            );
            if logging {
                let held: Vec<serde_json::Value> = self
                    .positions
                    .iter()
                    .map(|x| serde_json::json!({"symbol": x.symbol, "side": format!("{:?}", x.side), "entry": x.entry, "mark": x.mark}))
                    .collect();
                let detail = serde_json::json!({
                    "trigger": if resume_decision { "resume" } else { "rebalance" },
                    "decision_close_ms": m.ts[t] + BAR_MS,
                    "gates": {"entries_allowed": self.entries_allowed, "trend_ok": regime, "stopped_out": self.stopped_out},
                    "trend": trend_snapshot(m, t, p),
                    "usable_balance": usable,
                    "reserved": self.reserved,
                    "requested_top": p.top,
                    "funded_top": top,
                    "slot_budget": slot,
                    "allocation_note": self.allocation_note.clone(),
                    "settings": {"signal": format!("{:?}", p.signal), "flip": p.flip, "hold_bars": p.hold, "leverage": p.gross_leverage},
                    "targets": target_log,
                    "held_now": held,
                    "candidates": notes,
                });
                self.log(m.ts[t], "decision", None, detail);
            }
        }
    }

    /// The targets the portfolio could fund now at bar `t` (none when stopped out, entries off or the trend filter blocks).
    pub fn executable_targets(
        &self,
        m: &Market,
        scores: &[Vec<Option<Score>>],
        t: usize,
        p: &XsParams,
    ) -> BTreeMap<usize, Side> {
        if self.stopped_out || !self.entries_allowed || !trend_ok(m, t, p) {
            return BTreeMap::new();
        }
        funded_targets(
            m,
            scores,
            t,
            p,
            self.equity + self.unrealized() - self.reserved,
            self.cost_mult,
        )
        .0
    }

    /// Live reports/data received after a candle closes can first fill at a
    /// future opening boundary. Backtests leave this delay unset.
    pub fn defer_new_decisions(&mut self, ready_ts: i64, decision_bar_ts: i64) {
        let earliest = ((ready_ts + BAR_MS - 1) / BAR_MS) * BAR_MS;
        // Adjust only decisions first made at this close. Moving old bounds on
        // every later report postpones them forever in ordinary live operation.
        if self.pending_targets.is_some() && self.pending_decided_at == Some(decision_bar_ts) {
            self.pending_not_before = self.pending_not_before.max(earliest);
        }
        if self.flatten_next && self.flatten_decided_at == Some(decision_bar_ts) {
            self.flatten_not_before = self.flatten_not_before.max(earliest);
        }
        for pos in &mut self.positions {
            if pos.next_action.is_some() && pos.action_decided_at == Some(decision_bar_ts) {
                pos.action_not_before = pos.action_not_before.max(earliest);
            }
        }
    }

    /// Checks every money field is finite and the state is consistent; errors otherwise.
    pub fn validate_state(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            [
                self.equity,
                self.start_equity,
                self.peak,
                self.risk_peak,
                self.max_dd,
                self.cost_mult,
                self.reserved
            ]
            .iter()
            .all(|v| v.is_finite())
                && self.reserved >= 0.0
                && self.risk_peak >= 0.0
                && self.resume_not_before.is_none_or(|at| at >= 0)
                && self.start_equity > 0.0
                && self.cost_mult > 0.0,
            "invalid persisted account values"
        );
        anyhow::ensure!(
            self.execution_error.is_none(),
            "paper execution failed: {:?}",
            self.execution_error
        );
        let mut symbols = std::collections::HashSet::new();
        for pos in &self.positions {
            anyhow::ensure!(
                symbols.insert(&pos.symbol),
                "duplicate persisted position {}",
                pos.symbol
            );
            anyhow::ensure!(
                [
                    pos.entry,
                    pos.qty,
                    pos.margin,
                    pos.leverage,
                    pos.liquidation,
                    pos.fees,
                    pos.funding,
                    pos.mark,
                    pos.fee_rate,
                ]
                .iter()
                .all(|v| v.is_finite())
                    && pos.entry > 0.0
                    && pos.qty > 0.0
                    && pos.margin >= 0.0
                    && pos.leverage > 0.0
                    && pos.mark > 0.0
                    && pos.fees >= 0.0
                    && pos.fee_rate >= 0.0,
                "invalid persisted position {}",
                pos.symbol
            );
        }
        Ok(())
    }

    /// Close every position at its last mark (bar close) with its measured
    /// order-book cost and fee.
    pub fn close_all(&mut self, m: &Market, ts: i64, reason: &str) {
        if self.execution_error.is_some() {
            return;
        }
        // NOTE(agents): Paper may defer newest-boundary funding until its real mark arrives.
        // A terminal backtest cannot finish with that liability silently omitted.
        if let Some(pos) = self.positions.iter().find(|pos| {
            m.funding[pos.sym].iter().any(|&(settled, _)| {
                settled == ts && pos.entry_ts < settled && pos.last_funding_ts != Some(settled)
            })
        }) {
            self.execution_error = Some(format!(
                "{}: final settlement at {ts} is unpaid; load its real boundary mark before closing",
                pos.symbol
            ));
            return;
        }
        while !self.positions.is_empty() {
            let i = self.positions.len() - 1;
            let final_bar = m.ts.partition_point(|t| *t + BAR_MS <= ts).checked_sub(1);
            let reference = final_bar
                .and_then(|t| m.bars[self.positions[i].sym][t])
                .map(|b| b.close);
            let Some(reference) = reference else {
                self.execution_error = Some("no traded close for final exit".into());
                return;
            };
            if !self.close_market(m, i, ts, reference, reason) {
                return;
            }
        }
        self.peak = self.peak.max(self.equity);
        if self.peak > 0.0 {
            self.max_dd = self
                .max_dd
                .max((self.peak - self.equity) / self.peak * 100.0);
        }
    }

    /// Open unrealized PnL of all positions at their marks.
    pub fn unrealized(&self) -> f64 {
        self.positions
            .iter()
            .map(|p| p.side.sign() * (p.mark - p.entry) * p.qty)
            .sum()
    }

    /// Performance metrics of this portfolio (equity, trades, drawdown, profit factor).
    pub fn metrics(&self) -> Metrics {
        let mut m = Metrics {
            execution_error: self.execution_error.clone(),
            rejected_rebalances: self.rejected_rebalances,
            start_equity: self.start_equity,
            end_equity: self.equity,
            open_unrealized: self.unrealized(),
            trades: self.trades.len(),
            liquidations: self.liquidations,
            max_drawdown_pct: self.max_dd,
            ..Default::default()
        };
        for t in &self.trades {
            if t.pnl > 0.0 {
                m.wins += 1;
                m.gross_profit += t.pnl
            } else {
                m.gross_loss -= t.pnl
            }
        }
        m
    }
}

pub const DAY_MS: i64 = 86_400_000;

/// One UTC day of a backtest: marked equity at the day's last closed bar.
#[derive(Debug, Clone, Serialize)]
pub struct DayRow {
    /// 00:00 UTC of the day.
    pub day_ts: i64,
    pub equity: f64,
    pub pnl: f64,
    pub pnl_pct: f64,
    /// Positions closed that day and how many made money.
    pub trades: usize,
    pub wins: usize,
    /// Drawdown exits/cooldown are still pending at this day's close.
    pub stopped_out: bool,
    /// Which tokens had trades closed that day (user request 2026-10-05), best net P&L first.
    pub tokens: Vec<DayToken>,
}

/// One token's closed trades of a day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayToken {
    pub symbol: String,
    pub trades: usize,
    pub pnl: f64,
}

/// Closed trades of a day grouped by symbol (count and net PnL), best first.
#[cfg(test)]
fn day_tokens(closed: &[Trade]) -> Vec<DayToken> {
    let mut by: BTreeMap<&str, (usize, f64)> = BTreeMap::new();
    for t in closed {
        let e = by.entry(t.symbol.as_str()).or_default();
        e.0 += 1;
        e.1 += t.pnl;
    }
    let mut out: Vec<DayToken> = by
        .into_iter()
        .map(|(s, (n, pnl))| DayToken {
            symbol: s.to_string(),
            trades: n,
            pnl,
        })
        .collect();
    out.sort_by(|a, b| b.pnl.total_cmp(&a.pnl));
    out
}

/// Step `pf` over `range` with `p`, appending one row per UTC day (equity marked
/// to the last close, fees and funding included). With `close_at_end` the
/// positions still open at the end are closed at the last close (realised);
/// without it they carry into the next call, as they would live when settings
/// change.
#[cfg(test)]
pub fn run_daily(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    range: std::ops::Range<usize>,
    p: &XsParams,
    pf: &mut XsPortfolio,
    out: &mut Vec<DayRow>,
    close_at_end: bool,
) {
    let Some(last) = range.clone().last() else {
        return;
    };
    let mut prev = pf.equity + pf.unrealized();
    let mut seen = pf.trades.len();
    for t in range {
        pf.step(m, scores, t, p);
        let end = m.ts[t] + BAR_MS;
        // A row per trading session: it ends at a gap in wall time (the close), at a UTC
        // midnight, or at the last bar.
        let wall_end = m.wall[t] + BAR_MS;
        let session_end = m.wall.get(t + 1).is_none_or(|&w| w != wall_end);
        if session_end || wall_end % DAY_MS == 0 || t == last {
            if t == last && close_at_end {
                pf.close_all(m, end, "End of test");
            }
            let equity = pf.equity + pf.unrealized();
            let closed = &pf.trades[seen..];
            out.push(DayRow {
                day_ts: (wall_end - 1) / DAY_MS * DAY_MS,
                equity,
                pnl: equity - prev,
                pnl_pct: if prev > 0.0 {
                    (equity / prev - 1.0) * 100.0
                } else {
                    0.0
                },
                trades: closed.len(),
                wins: closed.iter().filter(|x| x.pnl > 0.0).count(),
                stopped_out: pf.stopped_out,
                tokens: day_tokens(closed),
            });
            prev = equity;
            seen = pf.trades.len();
        }
    }
}

/// Backtest of settings `p` over `range` from `equity` with normal costs.
pub fn backtest(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    range: std::ops::Range<usize>,
    p: &XsParams,
    equity: f64,
) -> XsPortfolio {
    backtest_costs(m, scores, range, p, equity, 1.0)
}

/// Backtest with a fee/slippage multiplier. Positions still open at the end are
/// closed at the last close (with fees), so net and profit factor always come
/// from the same closed trades.
pub fn backtest_costs(
    m: &Market,
    scores: &[Vec<Option<Score>>],
    range: std::ops::Range<usize>,
    p: &XsParams,
    equity: f64,
    cost_mult: f64,
) -> XsPortfolio {
    let mut pf = XsPortfolio::new(equity).with_cost_mult(cost_mult);
    // NOTE(agents): User rule (2026-10-04): get rid of shorts. Every backtest (the search, the
    //               walk-forward, all tools) is long only; shorting settings are rejected, not run.
    if !p.long_only {
        pf.execution_error = Some("shorting is removed: the bot is long only".into());
        return pf;
    }
    let Some(last) = range.clone().last() else {
        return pf;
    };
    for t in range {
        pf.step(m, scores, t, p);
    }
    pf.close_all(m, m.ts[last] + BAR_MS, "End of test");
    pf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::scores::tests::market;

    /// Test helper: screener scores of every symbol (universe 100).
    fn sc(m: &Market) -> Vec<Vec<Option<Score>>> {
        crate::engine::scores::compute(m, 100)
    }

    /// Test: long only accepts one legal coarse lot without neutral constraints.
    #[test]
    fn long_only_accepts_one_legal_coarse_lot_without_neutral_constraints() {
        let mut m = market(&[vec![100.0; 200]]);
        let rules = m.rules[0].as_mut().unwrap();
        rules.qty_step = 0.01;
        rules.min_qty = 0.01;
        let scores = sc(&m);
        assert!(scores[0][150].is_some(), "one-symbol percentile is neutral");
        let p = XsParams {
            long_only: true,
            signal: Signal::Return,
            lookback: 1,
            top: 1,
            ..risk_params(Risk::default())
        };
        let (targets, top, _) = funded_targets(&m, &scores, 150, &p, 10.0, 1.0);
        assert_eq!(top, 1);
        assert_eq!(targets.get(&0), Some(&Side::Long));
        let neutral = XsParams {
            long_only: false,
            ..p
        };
        assert!(funded_targets(&m, &scores, 150, &neutral, 10.0, 1.0)
            .0
            .is_empty());
    }

    /// Test: live high water mark survives until later drawdown.
    #[test]
    fn live_high_water_mark_survives_until_later_drawdown() {
        let m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 400.0, &p);
        let high = std::collections::HashMap::from([("AUSDT".into(), 200.0)]);
        assert!(pf.live_check(&high, 101 * BAR_MS + 1).is_empty());
        let (peak, _, _) = pf.risk_watermarks();
        assert!(peak > 1700.0);
        let mut restored: XsPortfolio =
            serde_json::from_value(serde_json::to_value(&pf).unwrap()).unwrap();
        let low = std::collections::HashMap::from([("AUSDT".into(), 100.0)]);
        restored.live_check(&low, 101 * BAR_MS + 2);
        assert!(restored.stopped_out);
        assert!(restored.metrics().max_drawdown_pct > 25.0);
    }

    /// Test: live portfolio drawdown waits for every legs fresh price.
    #[test]
    fn live_portfolio_drawdown_waits_for_every_legs_fresh_price() {
        let m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        pf.open(&m, 100, 1, Side::Long, 200.0, &p);
        let mut marks = std::collections::HashMap::from([("S0USDT".into(), 20.0)]);
        let before = pf.risk_watermarks();
        pf.live_check(&marks, 101 * BAR_MS + 1);
        assert!(!pf.stopped_out);
        assert_eq!(pf.risk_watermarks(), before);
        assert!(pf.positions.iter().all(|p| p.live_exit.is_none()));
        marks.insert("S1USDT".into(), 100.0);
        pf.live_check(&marks, 101 * BAR_MS + 2);
        assert!(pf.stopped_out);
        assert!(pf.positions.iter().all(|p| p.live_exit.is_some()));
    }

    /// Test: cooldown rearm retains balance gate and second cycle stop.
    #[test]
    fn cooldown_rearm_retains_balance_gate_and_second_cycle_stop() {
        let mut m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        for rows in [&mut m.bars, &mut m.marks] {
            for row in rows {
                for (t, bar) in row.iter_mut().enumerate().skip(151) {
                    let price = if t >= 157 { 36.0 } else { 60.0 };
                    *bar = Some(crate::engine::Bar {
                        open: price,
                        high: price,
                        low: price,
                        close: price,
                        volume: 1.0,
                        turnover: 100.0,
                    });
                }
            }
        }
        let p = XsParams {
            long_only: true,
            hold: 192,
            signal: Signal::Return,
            lookback: 1,
            ..risk_params(Risk::default())
        };
        let scores = sc(&m);
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 150, 0, Side::Long, 400.0, &p);
        pf.step(&m, &scores, 151, &p);
        assert!(pf.stopped_out);
        pf.entries_allowed = false;
        for t in 152..=153 {
            pf.step(&m, &scores, t, &p);
        }
        assert!(
            pf.stopped_out && pf.positions.is_empty(),
            "invalid settings block resume"
        );
        pf.entries_allowed = true;
        pf.reserved = pf.equity - 4.0;
        pf.decide_close(&m, &scores, 153, &p);
        assert!(pf.stopped_out, "less than 5 free remains blocked");
        pf.reserved = 0.0;
        pf.decide_close(&m, &scores, 153, &p);
        assert!(!pf.stopped_out);
        let global_dd = pf.metrics().max_drawdown_pct;
        assert!(global_dd > 25.0);
        pf.step(&m, &scores, 154, &p);
        assert!(pf.positions.is_empty(), "normal decision latency remains");
        pf.step(&m, &scores, 155, &p);
        assert_eq!(pf.positions.len(), 1);
        assert!(
            pf.metrics().max_drawdown_pct >= global_dd,
            "new entry costs also count"
        );
        assert_eq!(pf.peak, 1000.0, "global peak is never reset on resume");
        pf.step(&m, &scores, 156, &p);
        pf.step(&m, &scores, 157, &p);
        assert!(pf.stopped_out, "a new 25% cycle drawdown must exit again");
        assert!(pf.metrics().max_drawdown_pct > global_dd);
    }

    /// Test: persisted reserved margin must be finite and nonnegative.
    #[test]
    fn persisted_reserved_margin_must_be_finite_and_nonnegative() {
        let mut pf = XsPortfolio::new(100.0);
        for reserved in [f64::NAN, f64::INFINITY, -1.0] {
            pf.reserved = reserved;
            assert!(pf.validate_state().is_err());
        }
        pf.reserved = 150.0; // Commitments can exceed equity; this blocks entries.
        assert!(pf.validate_state().is_ok());
    }

    /// Test: mark gap liquidates before queued stop.
    #[test]
    fn mark_gap_liquidates_before_queued_stop() {
        let mut m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        pf.positions[0].next_action = Some(NextAction::Stop);
        m.marks[0][101] = Some(super::super::Bar {
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
            turnover: 0.0,
        });
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.trades[0].reason, "Liquidated");
        assert_eq!(pf.metrics().liquidations, 1);
    }

    /// Test: funding uses mark and debits isolated margin.
    #[test]
    fn funding_uses_mark_and_debits_isolated_margin() {
        let mut m = flat_with(&[]);
        let r = m.rules[0].as_mut().unwrap();
        r.commission = vec![(1.0, 0.0), (1e12, 0.0)];
        for b in &mut r.book {
            b.1 = 0.0;
            b.2 = 0.0;
        }
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 1000.0, &p);
        let (qty, margin, liq) = (
            pf.positions[0].qty,
            pf.positions[0].margin,
            pf.positions[0].liquidation,
        );
        let mark = m.marks[0][101].as_mut().unwrap();
        mark.open = 110.0;
        mark.high = 110.0;
        m.funding[0] = vec![(101 * BAR_MS, 0.1)];
        pf.step(&m, &[vec![None; 200]], 101, &p);
        let cost = qty * 110.0 * 0.1;
        assert!((pf.positions[0].funding - cost).abs() < 1e-9);
        assert!((pf.positions[0].margin - (margin - cost)).abs() < 1e-9);
        assert!(pf.positions[0].liquidation > liq);
        assert!((pf.available()).abs() < 1e-9);
    }

    /// Test: mark data is required and intrabar funding is rejected.
    #[test]
    fn mark_data_is_required_and_intrabar_funding_is_rejected() {
        let mut m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        let cash = pf.equity;
        m.marks[0][101] = None;
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!(pf
            .execution_error
            .as_ref()
            .unwrap()
            .contains("missing mark"));
        assert_eq!(pf.equity, cash);
        m.marks[0][101] = m.marks[0][100];
        m.funding[0] = vec![(101 * BAR_MS + 1, 0.01)];
        pf.execution_error = None;
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!(pf
            .execution_error
            .as_ref()
            .unwrap()
            .contains("intrabar funding"));
    }

    /// Test helper: sets a pending long/short pair on the portfolio, as a rebalance decision would.
    fn pending_pair(pf: &mut XsPortfolio, p: &XsParams) {
        pf.pending_targets = Some(vec![
            (0, "S0USDT".into(), Side::Long),
            (1, "S1USDT".into(), Side::Short),
        ]);
        pf.pending_params = Some(p.clone());
        pf.pending_slot = pf.equity * SLOT_HEADROOM / 2.0;
    }

    #[test]
    fn queued_buy_is_rejected_when_its_settings_are_superseded() {
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pending_pair(&mut pf, &p);
        assert!(pf.pending_buy_uses(&p));
        let mut sized = p.clone();
        sized.top += 1;
        assert!(pf.pending_buy_uses(&sized), "account sizing may reduce top");
        let mut reduced = p.clone();
        reduced.top = p.top.saturating_sub(1);
        assert!(
            !pf.pending_buy_uses(&reduced),
            "a smaller strategy cap rejects the old larger buy"
        );
        let mut changed = p.clone();
        changed.hold += 1;
        assert!(!pf.pending_buy_uses(&changed));
    }

    /// Test: paired entries wait for missing candle and use captured parameters.
    #[test]
    fn paired_entries_wait_for_missing_candle_and_use_captured_parameters() {
        let mut m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pending_pair(&mut pf, &p);
        m.bars[1][101] = None;
        pf.step(&m, &vec![vec![None; 200]; 2], 101, &p);
        assert!(pf.positions.is_empty());
        assert!(pf.pending_targets.is_some());
        let mut later = p.clone();
        later.gross_leverage = 10.0;
        pf.step(&m, &vec![vec![None; 200]; 2], 102, &later);
        assert_eq!(pf.positions.len(), 2);
        assert!(pf.positions.iter().all(|x| x.leverage == p.gross_leverage));
    }

    /// Test: disabled gate and report delay prevent stale entries.
    #[test]
    fn disabled_gate_and_report_delay_prevent_stale_entries() {
        let m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pending_pair(&mut pf, &p);
        pf.install_entry_gate(false);
        assert_eq!(pf.allocation_note, "entry gate disabled");
        pf.step(&m, &vec![vec![None; 200]; 2], 101, &p);
        assert!(pf.positions.is_empty());
        pf.install_entry_gate(true);
        assert!(pf.allocation_note.starts_with("settings changed"));
        pending_pair(&mut pf, &p);
        pf.pending_decided_at = Some(100 * BAR_MS);
        pf.defer_new_decisions(101 * BAR_MS + 1, 100 * BAR_MS);
        pf.step(&m, &vec![vec![None; 200]; 2], 101, &p);
        assert!(pf.positions.is_empty());
        pf.step(&m, &vec![vec![None; 200]; 2], 102, &p);
        assert_eq!(pf.positions.len(), 2);
    }

    /// Test: incomplete pair does not leave one sided orders or fees.
    #[test]
    fn incomplete_pair_does_not_leave_one_sided_orders_or_fees() {
        let mut m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        let p = risk_params(Risk::default());
        m.rules[1].as_mut().unwrap().min_qty = 1000.0;
        let mut pf = XsPortfolio::new(1000.0);
        pending_pair(&mut pf, &p);
        pf.step(&m, &vec![vec![None; 200]; 2], 101, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.equity, 1000.0);
        assert_eq!(pf.rejected_rebalances, 1);
    }

    /// Test: thin book exit is invalid instead of negative fill.
    #[test]
    fn thin_book_exit_is_invalid_instead_of_negative_fill() {
        let mut m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        m.rules[0].as_mut().unwrap().book = vec![(1.0, 0.5, 0.5)];
        pf.close_all(&m, 101 * BAR_MS, "End of test");
        assert!(pf.execution_error.is_some());
        assert_eq!(pf.positions.len(), 1);
        assert!(pf.trades.is_empty());
    }

    /// Test: final exit uses traded close and never future data.
    #[test]
    fn final_exit_uses_traded_close_and_never_future_data() {
        let mut m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        pf.positions[0].mark = 200.0;
        for b in m.bars[0][101..].iter_mut().flatten() {
            b.close = 500.0;
            b.high = 500.0;
        }
        pf.close_all(&m, 101 * BAR_MS, "End of test");
        assert!(pf.trades[0].exit < 100.0 && pf.trades[0].exit > 99.0);
    }

    /// Test: allocation adapts to small and large balances.
    #[test]
    fn allocation_adapts_to_small_and_large_balances() {
        let series: Vec<Vec<f64>> = (0..12)
            .map(|s| {
                (0..200)
                    .map(|t| 100.0 + (s as f64 - 5.5) * t as f64 * 0.0001)
                    .collect()
            })
            .collect();
        let mut m = market(&series);
        for r in m.rules.iter_mut().flatten() {
            r.qty_step = 0.001;
            r.min_qty = 0.001;
        }
        let scores = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 5,
            gross_leverage: 2.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        for equity in [0.1, 1.0, 10.0, 100.0, 1000.0, 1_000_000.0] {
            let mut pf = XsPortfolio::new(equity);
            pf.decide_close(&m, &scores, 175, &p);
            pf.step(&m, &scores, 176, &p);
            // Decided at 175's close, filled at 177's open (decision latency).
            pf.step(&m, &scores, 177, &p);
            assert!(
                pf.execution_error.is_none(),
                "balance {equity}: {:?}",
                pf.execution_error
            );
            if equity < 5.01 {
                assert!(pf.positions.is_empty());
                assert_eq!(pf.effective_top, 0);
            } else {
                assert!(
                    !pf.positions.is_empty(),
                    "balance {equity}: {}",
                    pf.allocation_note
                );
            }
            assert!(pf.available() >= -1e-8);
            assert!(pf.positions.len() <= 2 * p.top);
            for pos in &pf.positions {
                assert!(pos.qty * pos.entry >= 5.0);
                assert!(pos.qty * pos.entry <= 3200.0 + 1e-8);
            }
            if equity == 10.0 {
                assert!(pf.effective_top < p.top);
            }
        }
    }

    /// Test: no entries or adds below the free balance floor.
    #[test]
    fn no_entries_or_adds_below_the_free_balance_floor() {
        let series: Vec<Vec<f64>> = (0..12)
            .map(|s| {
                (0..200)
                    .map(|t| 100.0 + (s as f64 - 5.5) * t as f64 * 0.0001)
                    .collect()
            })
            .collect();
        let m = market(&series);
        let scores = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        // 100 USDT wallet, 96 committed to other positions: 4 free, no trade.
        let mut pf = XsPortfolio::new(100.0);
        pf.reserved = 96.0;
        pf.decide_close(&m, &scores, 175, &p);
        assert_eq!(pf.effective_top, 0);
        assert!(
            pf.allocation_note.contains("below the 5 USDT minimum"),
            "{}",
            pf.allocation_note
        );
        pf.step(&m, &scores, 176, &p);
        assert!(pf.positions.is_empty());
        // 20 free: one neutral pair opens, sized from the free balance only.
        let mut pf = XsPortfolio::new(100.0);
        pf.reserved = 80.0;
        pf.decide_close(&m, &scores, 175, &p);
        pf.step(&m, &scores, 176, &p);
        // Decided at 175's close, filled at 177's open (decision latency).
        pf.step(&m, &scores, 177, &p);
        assert_eq!(pf.positions.len(), 2);
        let posted: f64 = pf.positions.iter().map(|x| x.margin + x.fees).sum();
        assert!(posted <= 20.0 + 1e-9, "{posted}");
        // An add is refused once the free balance is under the floor.
        let a = flat_with(&[]);
        let rp = risk_params(Risk {
            add_pct: Some(1.0),
            ..Default::default()
        });
        let mut pf = XsPortfolio::new(100.0);
        pf.open(&a, 100, 0, Side::Long, 96.0, &rp);
        let qty = pf.positions[0].qty;
        pf.add(&a, 101, 0, &rp);
        assert_eq!(pf.positions[0].qty, qty, "4 USDT free: add refused");
    }

    /// Test: coarse lot steps are left out instead of unbalancing the basket.
    #[test]
    fn coarse_lot_steps_are_left_out_instead_of_unbalancing_the_basket() {
        let series: Vec<Vec<f64>> = (0..12)
            .map(|s| {
                (0..200)
                    .map(|t| 100.0 + (s as f64 - 5.5) * t as f64 * 0.0001)
                    .collect()
            })
            .collect();
        let mut m = market(&series);
        // Symbol 0 (a long candidate) trades in steps of 0.5 (~50 USDT): a 75
        // USDT slot would round down to 50 and unbalance the basket.
        if let Some(r) = m.rules[0].as_mut() {
            r.qty_step = 0.5;
            r.min_qty = 0.5;
        }
        let scores = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 2,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        let mut pf = XsPortfolio::new(300.0);
        pf.decide_close(&m, &scores, 175, &p);
        pf.step(&m, &scores, 176, &p);
        // Decided at 175's close, filled at 177's open (decision latency).
        pf.step(&m, &scores, 177, &p);
        assert_eq!(pf.rejected_rebalances, 0);
        assert_eq!(pf.positions.len(), 4);
        assert!(pf.positions.iter().all(|x| x.sym != 0));
        let side = |s: Side| -> f64 {
            pf.positions
                .iter()
                .filter(|x| x.side == s)
                .map(|x| x.qty * x.entry)
                .sum()
        };
        let (l, sh) = (side(Side::Long), side(Side::Short));
        assert!((l - sh).abs() / (l + sh) <= NEUTRAL_TOLERANCE);
    }

    /// Test fixture: long-only Return settings with the given flip and regime.
    fn long_params(flip: bool, regime: Regime) -> XsParams {
        XsParams {
            signal: Signal::Return,
            flip,
            lookback: 16,
            hold: 16,
            top: 3,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: true,
            regime,
        }
    }

    /// Test: long only buys the ranked end and never shorts.
    #[test]
    fn long_only_buys_the_ranked_end_and_never_shorts() {
        // Symbol s drifts by (s - 5.5): 0..5 fall, 6..11 rise.
        let series: Vec<Vec<f64>> = (0..12)
            .map(|s| {
                (0..200)
                    .map(|t| 100.0 + (s as f64 - 5.5) * t as f64 * 0.0001)
                    .collect()
            })
            .collect();
        let m = market(&series);
        let scores = sc(&m);
        for (flip, expected) in [(false, [0, 1, 2]), (true, [9, 10, 11])] {
            let p = long_params(flip, Regime::Off);
            let mut pf = XsPortfolio::new(300.0);
            pf.decide_close(&m, &scores, 175, &p);
            pf.step(&m, &scores, 176, &p);
            // Decided at 175's close, filled at 177's open (decision latency).
            pf.step(&m, &scores, 177, &p);
            assert_eq!(pf.rejected_rebalances, 0);
            let mut held: Vec<usize> = pf.positions.iter().map(|x| x.sym).collect();
            held.sort();
            assert_eq!(held, expected, "flip {flip}");
            assert!(pf.positions.iter().all(|x| x.side == Side::Long));
            // Each of the 3 slots gets a third of 99% of the balance.
            for x in &pf.positions {
                assert!((x.margin + x.fees - 300.0 * SLOT_HEADROOM / 3.0).abs() < 1.0);
            }
        }
    }

    /// Test: the research ML gate blocks only stocks scored in the lowest part of the universe, never when off or unscored.
    #[test]
    fn ml_gate_blocks_only_the_lowest_scored_stocks() {
        let sc = |ml: Option<f64>| {
            Some(Score {
                volatility_score: 0.0,
                volume_score: 0.0,
                pa_strength: 0.0,
                price_action_score: 0.0,
                rank_value: 0.0,
                rank: 1,
                trend_score: 50.0,
                rsi14: 50.0,
                turnover_24h: 0.0,
                return_score: 0.0,
                pulse_long: None,
                pulse_short: None,
                ml,
            })
        };
        let scores: Vec<Vec<Option<Score>>> = (0..10).map(|i| vec![sc(Some(i as f64))]).collect();
        assert!(!ml_gate_blocks(&scores, 0, 0, -1), "off");
        assert!(
            ml_gate_blocks(&scores, 0, 0, 25),
            "the lowest of 10 is below the 25th percentile"
        );
        assert!(
            ml_gate_blocks(&scores, 2, 0, 25),
            "rank 2 of 10 = 20th percentile < 25"
        );
        assert!(!ml_gate_blocks(&scores, 3, 0, 25), "30th percentile passes");
        assert!(!ml_gate_blocks(&scores, 9, 0, 50));
        assert!(ml_gate_blocks(&scores, 4, 0, 50) && !ml_gate_blocks(&scores, 5, 0, 50));
        let mut few = scores.clone();
        few.truncate(4);
        assert!(
            !ml_gate_blocks(&few, 0, 0, 90),
            "fewer than 5 scored: not applied"
        );
        let mut unscored = scores.clone();
        unscored[0] = vec![sc(None)];
        assert!(
            !ml_gate_blocks(&unscored, 0, 0, 90),
            "no score for this stock: allowed"
        );
    }

    /// Test: ref trend filter blocks entries below its 24h average.
    #[test]
    fn ref_trend_filter_blocks_entries_below_its_24h_average() {
        let rising: Vec<f64> = (0..200).map(|t| 100.0 + t as f64 * 0.01).collect();
        let falling: Vec<f64> = (0..200).map(|t| 100.0 - t as f64 * 0.01).collect();
        for (spy, allowed) in [(rising, true), (falling, false)] {
            let mut series: Vec<Vec<f64>> = (0..12)
                .map(|s| {
                    (0..200)
                        .map(|t| 100.0 + (s as f64 - 5.5) * t as f64 * 0.0001)
                        .collect()
                })
                .collect();
            series[11] = spy;
            let mut m = market(&series);
            m.symbols[11] = REFERENCE.into();
            let scores = sc(&m);
            assert_eq!(regime_allows(&m, 175, Regime::MarketTrend), allowed);
            let p = long_params(false, Regime::MarketTrend);
            let mut pf = XsPortfolio::new(300.0);
            pf.decide_close(&m, &scores, 175, &p);
            pf.step(&m, &scores, 176, &p);
            // Decided at 175's close, filled at 177's open (decision latency).
            pf.step(&m, &scores, 177, &p);
            assert_eq!(pf.positions.len(), if allowed { 3 } else { 0 });
            if !allowed {
                assert!(
                    pf.allocation_note.contains("SPY below"),
                    "{}",
                    pf.allocation_note
                );
            }
        }
        // Without SPY history the filter never guesses: no entries.
        let m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        assert!(!regime_allows(&m, 150, Regime::MarketTrend));
        assert!(regime_allows(&m, 150, Regime::Off));
    }

    /// Test fixture: a 12-symbol market whose reference (SPY) follows `spy`.
    fn ref_market(spy: Vec<f64>) -> Market {
        let mut series: Vec<Vec<f64>> = (0..12)
            .map(|s| {
                (0..200)
                    .map(|t| 100.0 + (s as f64 - 5.5) * t as f64 * 0.0001)
                    .collect()
            })
            .collect();
        series[11] = spy;
        let mut m = market(&series);
        m.symbols[11] = REFERENCE.into();
        m
    }

    /// Test: trend window and margin change what counts as an up trend.
    #[test]
    fn trend_window_and_margin_change_what_counts_as_an_up_trend() {
        let m = ref_market((0..200).map(|t| 100.0 + t as f64 * 0.01).collect());
        // A slow steady rise: ~0.47% above its 96-bar average at bar 175.
        assert!(regime_allows_with(&m, 175, Regime::MarketTrend, 96, 0.0));
        assert!(regime_allows_with(&m, 175, Regime::MarketTrend, 96, 0.3));
        assert!(!regime_allows_with(&m, 175, Regime::MarketTrend, 96, 1.0));
        // A shorter window sits closer to the price, so the same margin is smaller to clear.
        assert!(regime_allows_with(&m, 175, Regime::MarketTrend, 8, 0.0));
        assert!(
            !regime_allows_with(&m, 175, Regime::MarketTrend, 400, 0.0),
            "not enough history"
        );
    }

    /// Test: pressure confirmations pass on buying and block on selling.
    #[test]
    fn pressure_confirmations_pass_on_buying_and_block_on_selling() {
        // SPY rises steadily; candles close at their highs with volume (buying pressure) or at
        // their lows (selling pressure) in the same price path.
        let mut m = ref_market((0..200).map(|t| 100.0 + t as f64 * 0.05).collect());
        let set = |m: &mut Market, buying: bool| {
            for t in 1..200 {
                let c = 100.0 + t as f64 * 0.05;
                let (high, low) = if buying { (c, c - 1.0) } else { (c + 1.0, c) };
                m.bars[11][t] = Some(crate::engine::Bar {
                    open: c - 0.02,
                    high,
                    low,
                    close: c,
                    volume: 10.0,
                    turnover: 10.0 * (c - 0.5),
                });
            }
        };
        let mut p = long_params(false, Regime::MarketTrend);
        p.risk.trend_bars = Some(48);
        p.risk.trend_margin_pct = Some(0.0);
        p.risk.cmf_bars = Some(48);
        p.risk.vwap_bars = Some(48);
        p.risk.obv_bars = Some(48);
        set(&mut m, true);
        assert!(trend_ok(&m, 150, &p), "buying pressure confirms the trend");
        set(&mut m, false);
        assert!(
            !trend_ok(&m, 150, &p),
            "closing at the lows is selling pressure"
        );
        // Breadth above 100% of the universe can never pass.
        set(&mut m, true);
        p.risk.cmf_bars = None;
        p.risk.vwap_bars = None;
        p.risk.obv_bars = None;
        p.risk.breadth_min_pct = Some(40.0);
        p.risk.breadth_min_pct = Some(101.0);
        assert!(!trend_ok(&m, 150, &p));
    }

    /// Test: crash trigger and coarser sampling change the trend test.
    #[test]
    fn crash_trigger_and_coarser_sampling_change_the_trend_test() {
        // SPY climbs to 130 by bar 170 (well above its average), then drops 3% in four bars.
        let spy: Vec<f64> = (0..200)
            .map(|t| match t {
                0..=170 => 100.0 + t as f64 * 0.176,
                171..=174 => 130.0 * (1.0 - 0.0075 * (t - 170) as f64),
                _ => 130.0 * 0.97,
            })
            .collect();
        let m = ref_market(spy);
        let mut p = long_params(false, Regime::MarketTrend);
        p.risk.trend_bars = Some(96);
        p.risk.trend_margin_pct = Some(1.0);
        // Still far above the slow average after a 3% dip: plain MA test says up trend.
        assert!(trend_ok(&m, 178, &p));
        // A crash trigger of 2% within 8 bars calls it a down trend; 5% does not.
        p.risk.crash_pct = Some(2.0);
        p.risk.crash_bars = Some(8);
        assert!(!trend_ok(&m, 178, &p));
        p.risk.crash_pct = Some(5.0);
        assert!(trend_ok(&m, 178, &p));
        // Sampling every 4th bar keeps the verdict on a smooth trend and never panics near the start.
        p.risk.crash_pct = None;
        p.risk.trend_step = Some(4);
        assert!(trend_ok(&m, 178, &p));
        assert!(!trend_ok(&m, 50, &p), "not enough history");
        // The Off regime never blocks.
        p.regime = Regime::Off;
        assert!(trend_ok(&m, 50, &p));
    }

    /// Test: trend exit goes flat when spy turns down and stays flat.
    #[test]
    fn trend_exit_goes_flat_when_spy_turns_down_and_stays_flat() {
        // SPY rises to bar 150, then falls steadily: the trend turns down a few bars later.
        let spy: Vec<f64> = (0..200)
            .map(|t| {
                if t <= 150 {
                    100.0 + t as f64 * 0.05
                } else {
                    107.5 - (t - 150) as f64 * 0.4
                }
            })
            .collect();
        let m = ref_market(spy);
        let scores = sc(&m);
        for exit in [false, true] {
            let mut p = long_params(false, Regime::MarketTrend);
            p.risk.trend_exit = exit;
            p.hold = 100_000; // no scheduled rebalance: only the trend rule can close it
            let mut pf = XsPortfolio::new(300.0);
            pf.open(&m, 150, 0, Side::Long, 100.0, &p);
            assert_eq!(pf.positions.len(), 1);
            for t in 150..190 {
                pf.step(&m, &scores, t, &p);
            }
            if exit {
                assert!(pf.positions.is_empty(), "down trend must close the long");
            } else {
                assert_eq!(
                    pf.positions.len(),
                    1,
                    "without trend_exit the position is held"
                );
            }
            // Either way, no new entry is decided while the trend is down.
            pf.decide_close(&m, &scores, 189, &p);
            assert!(pf.pending_targets.as_ref().is_none_or(|t| t.is_empty()));
        }
    }

    /// Test: terminal close cannot omit deferred funding.
    #[test]
    fn terminal_close_cannot_omit_deferred_funding() {
        let mut m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        let end = m.ts.last().unwrap() + BAR_MS;
        m.funding[0] = vec![(end, 0.01)];
        pf.close_all(&m, end, "End of test");
        assert!(pf
            .execution_error
            .as_ref()
            .unwrap()
            .contains("final settlement"));
        assert_eq!(pf.positions.len(), 1);
        assert!(pf.trades.is_empty());
    }

    /// Test: drawdown stop closes everything and blocks new entries.
    #[test]
    fn drawdown_stop_closes_everything_and_blocks_new_entries() {
        // Price falls 40% at 2x on most of the account: marked equity drops > 25%.
        let m = flat_with(&[
            (101, (100.0, 100.0, 60.0, 60.0)),
            (102, (60.0, 60.0, 60.0, 60.0)),
            (103, (60.0, 60.0, 60.0, 60.0)),
            (104, (60.0, 60.0, 60.0, 60.0)),
        ]);
        let p = risk_params(Risk {
            add_pct: Some(1.0),
            ..Default::default()
        });
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 400.0, &p);
        pf.step(&m, &[vec![None; 200]], 100, &p);
        assert!(!pf.stopped_out);
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!(
            pf.stopped_out,
            "-40% on 800 notional is about -32% of equity"
        );
        pf.step(&m, &[vec![None; 200]], 102, &p);
        assert_eq!(
            pf.positions.len(),
            1,
            "decision latency: not at the very next open"
        );
        pf.step(&m, &[vec![None; 200]], 103, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.trades.last().unwrap().reason, "Drawdown stop");
        // Nothing opens or adds afterwards.
        pf.step(&m, &[vec![None; 200]], 104, &p);
        assert!(pf.positions.is_empty());
        assert!(pf.pending_targets.as_ref().is_none_or(|t| t.is_empty()));
    }

    /// Test: stops trigger on mark price not on traded wicks.
    #[test]
    fn stops_trigger_on_mark_price_not_on_traded_wicks() {
        // Bar 101: the traded low wicks to 80 but the mark only dips to 95.
        let mut m = flat_with(&[(101, (100.0, 100.0, 80.0, 99.0))]);
        m.marks[0][101] = Some(crate::engine::Bar {
            open: 100.0,
            high: 100.0,
            low: 95.0,
            close: 99.0,
            volume: 0.0,
            turnover: 0.0,
        });
        let mut p = risk_params(Risk::default());
        p.stop_pct = Some(10.0);
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        pf.step(&m, &[vec![None; 200]], 100, &p);
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert_eq!(pf.positions.len(), 1, "a last-trade wick must not stop out");
        // When the mark itself reaches the stop, it fires.
        m.marks[0][102] = Some(crate::engine::Bar {
            open: 99.0,
            high: 99.0,
            low: 85.0,
            close: 90.0,
            volume: 0.0,
            turnover: 0.0,
        });
        pf.step(&m, &[vec![None; 200]], 102, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.trades[0].reason, "Stop");
    }

    /// Test fixture: a mark candle with the given open, high, low and close.
    fn mark_bar(o: f64, h: f64, l: f64, c: f64) -> Option<crate::engine::Bar> {
        Some(crate::engine::Bar {
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 0.0,
            turnover: 0.0,
        })
    }

    /// Test: the SPY volatility-expansion gate sees a calm tape as 1.0, a volatility spike as ~3x, and fails closed on short history.
    #[test]
    fn spy_volatility_expansion_gate() {
        let mut m = flat_with(&[]);
        m.symbols = vec![REFERENCE.to_string()];
        let bar = |half: f64| {
            Some(crate::engine::Bar {
                open: 100.0,
                high: 100.0 + half,
                low: 100.0 - half,
                close: 100.0,
                volume: 1.0,
                turnover: 1.0,
            })
        };
        for t in 0..200 {
            m.bars[0][t] = bar(if t >= 180 { 3.0 } else { 1.0 });
        }
        let g = EntryGate {
            feature: GateFeature::RefVol,
            bars: 24,
            skip_above: true,
            threshold: 1.5,
        };
        let calm = gate_value(&m, 0, 150, &g).expect("measurable");
        assert!((calm - 1.0).abs() < 1e-9, "calm tape is 1.0, got {calm}");
        let spike = gate_value(&m, 0, 195, &g).expect("measurable");
        assert!(
            spike > 2.5 && spike <= 3.0,
            "a tripled range reads about 3x, got {spike}"
        );
        assert!(
            spike > g.threshold && calm <= g.threshold,
            "the gate skips the spike and keeps the calm tape"
        );
        assert!(
            gate_value(&m, 0, 40, &g).is_none(),
            "not enough history: fails closed"
        );
    }

    /// Test: the relative-strength gate is the stock's return minus SPY's, in the stock's ATRs; fails closed without SPY or history.
    #[test]
    fn relative_strength_gate() {
        let mut m = flat_with(&[]);
        m.symbols = vec!["AAA".to_string(), REFERENCE.to_string()];
        m.bars = vec![vec![None; 200], vec![None; 200]];
        let bar = |c: f64| {
            Some(crate::engine::Bar {
                open: c,
                high: c + 1.0,
                low: c - 1.0,
                close: c,
                volume: 1.0,
                turnover: 1.0,
            })
        };
        for t in 0..200 {
            m.bars[0][t] = bar(if t >= 100 { 110.0 } else { 100.0 }); // stock +10% at t=100
            m.bars[1][t] = bar(if t >= 100 { 102.0 } else { 100.0 }); // SPY +2%
        }
        let g = EntryGate {
            feature: GateFeature::RelStrength,
            bars: 10,
            skip_above: false,
            threshold: 0.0,
        };
        let atr = atr_pct(&m, 0, 101);
        assert!(atr > 0.0);
        let v = gate_value(&m, 0, 100, &g).expect("measurable");
        assert!(
            (v - 8.0 / atr).abs() < 1e-9,
            "stock +10% minus SPY +2% = 8 points in ATRs, got {v}"
        );
        assert!(
            gate_value(&m, 0, 150, &g).unwrap().abs() < 1e-9,
            "both flat: no relative strength"
        );
        assert!(
            gate_value(&m, 0, 5, &g).is_none(),
            "not enough history: fails closed"
        );
        m.symbols[1] = "OTHER".to_string();
        assert!(
            gate_value(&m, 0, 100, &g).is_none(),
            "no SPY in the market: fails closed"
        );
    }

    /// Test: atr pct and effective distances.
    #[test]
    fn atr_pct_and_effective_distances() {
        let m = flat_with(&[]);
        // Flat 100/100/100/100 bars have a true range of 0 -> ATR 0 -> fixed distance is used.
        assert_eq!(atr_pct(&m, 0, 50), 0.0);
        assert_eq!(eff_pct(Some(5.0), Some(2.0), 0.0), Some(5.0));
        assert_eq!(eff_pct(Some(5.0), Some(2.0), 1.5), Some(3.0));
        assert_eq!(eff_pct(Some(5.0), None, 1.5), Some(5.0));
        assert_eq!(eff_pct(None, None, 1.5), None);
        // A bar series with a constant 2-point range on a 100 close: ATR = 2%.
        let mut m = flat_with(&[]);
        for t in 0..200 {
            m.bars[0][t] = Some(crate::engine::Bar {
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.0,
                volume: 1.0,
                turnover: 1.0,
            });
        }
        assert!((atr_pct(&m, 0, 50) - 2.0).abs() < 1e-9);
        assert_eq!(atr_pct(&m, 0, 5), 0.0, "not enough history");
        m.bars[0][45] = None;
        assert_eq!(
            atr_pct(&m, 0, 50),
            0.0,
            "a missing bar means unknown, never a guess"
        );
    }

    /// Test: resume pause delays reentry but never below one bar.
    #[test]
    fn resume_pause_delays_reentry_but_never_below_one_bar() {
        let mut m = flat_with(&[]);
        // A crash after entry triggers the 25% drawdown stop at bar 101's close.
        m.marks[0][101] = mark_bar(100.0, 100.0, 40.0, 50.0);
        m.bars[0][101] = Some(m.marks[0][101].unwrap());
        for pause in [None, Some(0usize), Some(24usize)] {
            let p = risk_params(Risk {
                resume_pause_bars: pause,
                ..Risk::default()
            });
            let mut pf = XsPortfolio::new(1000.0);
            pf.open(&m, 100, 0, Side::Long, 900.0, &p);
            for t in 100..=102 {
                pf.step(&m, &[vec![None; 200]], t, &p);
            }
            let at = pf
                .resume_not_before
                .expect("the drawdown stop must have fired");
            let waited = (at - m.ts[101]) / BAR_MS;
            match pause {
                Some(24) => assert!(waited >= 24, "pause of 24 bars, got {waited}"),
                _ => assert_eq!(
                    waited, 2,
                    "default is the one-bar cooldown after the stop bar"
                ),
            }
        }
    }

    /// Test: trend lines lag in the expected order on a ramp.
    #[test]
    fn trend_lines_lag_in_the_expected_order_on_a_ramp() {
        let ramp: Vec<f64> = (0..300).map(|i| 100.0 + i as f64).collect();
        let last = *ramp.last().unwrap();
        let line = |k| trend_line(k, &ramp, 48).unwrap();
        let (sma, ema, hma, kama, don) = (
            line(TrendKind::Sma),
            line(TrendKind::Ema),
            line(TrendKind::Hma),
            line(TrendKind::Kama),
            line(TrendKind::Donchian),
        );
        // On a straight ramp: SMA lags half the window; EMA a bit less; Hull and the adaptive KAMA
        // (efficiency ratio 1 -> fast) hug the price much more closely; Donchian middle = SMA here.
        assert!((last - sma - 23.5).abs() < 1e-9);
        assert!(last - ema < last - sma && last - ema > 20.0);
        assert!(last - hma < 5.0, "hull lag {}", last - hma);
        assert!(last - kama < 5.0, "kama lag {}", last - kama);
        assert!((don - sma).abs() < 1e-9);
        assert!(trend_line(TrendKind::Sma, &ramp[..10], 48).is_none());
    }

    /// Test: efficiency ratio and adx separate trends from chop.
    #[test]
    fn efficiency_ratio_and_adx_separate_trends_from_chop() {
        let ramp: Vec<f64> = (0..100).map(|i| 100.0 + i as f64).collect();
        let chop: Vec<f64> = (0..100)
            .map(|i| 100.0 + if i % 2 == 0 { 0.0 } else { 1.0 })
            .collect();
        assert!((efficiency_ratio(&ramp, 48).unwrap() - 1.0).abs() < 1e-9);
        assert!(efficiency_ratio(&chop, 48).unwrap() < 0.05);
        assert!(efficiency_ratio(&ramp[..10], 48).is_none());
        let build = |closes: Vec<f64>| {
            let mut m = ref_market((0..200).map(|t| 100.0 + t as f64 * 0.01).collect());
            for (t, c) in closes.iter().enumerate() {
                m.bars[11][t] = Some(crate::engine::Bar {
                    open: *c,
                    high: c + 0.5,
                    low: c - 0.5,
                    close: *c,
                    volume: 1.0,
                    turnover: *c,
                });
            }
            m
        };
        let trending = build((0..200).map(|i| 100.0 + i as f64 * 0.8).collect());
        let ranging = build(
            (0..200)
                .map(|i| 100.0 + if i % 2 == 0 { 0.0 } else { 0.6 })
                .collect(),
        );
        let (a_trend, a_range) = (
            adx14(&trending, 11, 190).unwrap(),
            adx14(&ranging, 11, 190).unwrap(),
        );
        assert!(
            a_trend > 40.0 && a_range < 20.0,
            "adx {a_trend} vs {a_range}"
        );
        assert!(adx14(&trending, 11, 50).is_none(), "needs history");
    }

    /// Test: loss zones block dips in turbulence and fail closed.
    #[test]
    fn loss_zones_block_dips_in_turbulence_and_fail_closed() {
        // 12 coins; coin 0 falls 3% in the last hour while the others drift, SPY is symbol 11.
        let mut series: Vec<Vec<f64>> = (0..12)
            .map(|s| {
                (0..200)
                    .map(|t| 100.0 + s as f64 * 0.5 + t as f64 * 0.01)
                    .collect()
            })
            .collect();
        for t in 196..200 {
            series[0][t] = series[0][195] * (1.0 - 0.0075 * (t - 195) as f64);
        }
        let m = {
            let mut mm = market(&series);
            mm.symbols[11] = REFERENCE.into();
            mm
        };
        let t = 199;
        let mk = zone_market(&m, t).expect("history is long enough");
        assert!(
            zone_market(&m, ZONE_HISTORY - 1).is_none(),
            "too little history"
        );
        let mut r = Risk::default();
        // Nothing enabled: nothing blocked.
        assert!(!zone_blocks(&m, t, 0, &r, Some(&mk)));
        // Z1: the dropped coin is blocked only when the dispersion condition is met.
        r.knife_drop_pct = Some(1.5);
        r.zone_dispersion_pct = Some(mk.dispersion - 0.01);
        assert!(zone_blocks(&m, t, 0, &r, Some(&mk)));
        r.zone_dispersion_pct = Some(mk.dispersion + 10.0);
        assert!(!zone_blocks(&m, t, 0, &r, Some(&mk)));
        // A coin that did not drop is never a falling knife.
        r.zone_dispersion_pct = Some(0.0);
        assert!(!zone_blocks(&m, t, 5, &r, Some(&mk)));
        // Zones enabled but market state unknown: fail closed.
        assert!(zone_blocks(&m, t, 5, &r, None));
    }

    /// Test: day tokens group closed trades by symbol best first.
    #[test]
    fn day_tokens_group_closed_trades_by_symbol_best_first() {
        let tr = |sym: &str, pnl: f64| Trade {
            symbol: sym.into(),
            side: Side::Long,
            entry_ts: 0,
            exit_ts: 1,
            entry: 1.0,
            exit: 1.0,
            qty: 1.0,
            leverage: 1.0,
            fees: 0.0,
            funding: 0.0,
            pnl,
            r: 0.0,
            reason: String::new(),
            decided_ts: None,
        };
        let v = day_tokens(&[
            tr("AAA", -2.0),
            tr("BBB", 5.0),
            tr("AAA", 1.0),
            tr("CCC", -0.5),
        ]);
        let names: Vec<_> = v.iter().map(|t| (t.symbol.as_str(), t.trades)).collect();
        assert_eq!(names, vec![("BBB", 1), ("CCC", 1), ("AAA", 2)]);
        assert!((v[2].pnl + 1.0).abs() < 1e-12);
        assert!(day_tokens(&[]).is_empty());
    }

    /// Test: decision events record the trend gate candidates and targets.
    #[test]
    fn decision_events_record_the_trend_gate_candidates_and_targets() {
        let falling: Vec<f64> = (0..200).map(|t| 100.0 - t as f64 * 0.01).collect();
        let rising: Vec<f64> = (0..200).map(|t| 100.0 + t as f64 * 0.01).collect();
        for (spy, passes) in [(falling, false), (rising, true)] {
            let m = ref_market(spy);
            let scores = sc(&m);
            let p = long_params(false, Regime::MarketTrend);
            let mut pf = XsPortfolio::new(300.0);
            pf.enable_events();
            pf.decide_close(&m, &scores, 175, &p);
            let ev = pf.take_events();
            let d = ev
                .iter()
                .find(|e| e.kind == "decision")
                .expect("a decision is logged");
            assert_eq!(d.detail["gates"]["trend_ok"], passes);
            assert_eq!(d.detail["trend"]["passes"], passes);
            assert_eq!(d.detail["trend"]["regime"], "MarketTrend");
            let targets = d.detail["targets"].as_array().unwrap();
            let cands = d.detail["candidates"].as_array().unwrap();
            if passes {
                assert_eq!(targets.len(), p.top);
                assert!(cands
                    .iter()
                    .any(|c| c["outcome"] == "selected for entry" && c["rank"].is_number()));
                assert!(cands.iter().any(|c| c["outcome"]
                    .as_str()
                    .unwrap()
                    .starts_with("eligible but ranked")));
            } else {
                assert!(targets.is_empty(), "the trend gate blocks every entry");
                assert!(d.detail["allocation_note"]
                    .as_str()
                    .unwrap()
                    .contains("SPY below"));
            }
        }
    }

    /// Test: denied entries are logged with the reason and logging never changes results.
    #[test]
    fn denied_entries_are_logged_with_the_reason_and_logging_never_changes_results() {
        // Not enough balance for the order: skipped, with the reason and the balance.
        let m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(10.0);
        pf.enable_events();
        pf.open(&m, 100, 0, Side::Long, 5000.0, &p);
        let ev = pf.take_events();
        assert!(pf.positions.is_empty());
        let e = ev
            .iter()
            .find(|e| e.kind == "entry_skipped")
            .expect("the denial is logged");
        assert!(e.detail["reason"]
            .as_str()
            .unwrap()
            .contains("not enough available balance"));
        assert!(e.detail["available_balance"].as_f64().unwrap() <= 10.0);
        // A filled entry logs its full context.
        let mut ok = XsPortfolio::new(1000.0);
        ok.enable_events();
        ok.open(&m, 100, 0, Side::Long, 200.0, &p);
        let ev = ok.take_events();
        let f = ev.iter().find(|e| e.kind == "entry_filled").unwrap();
        assert!(f.detail["fill_price"].is_number() && f.detail["liquidation_price"].is_number());
        // Logging is purely observational: identical results with it on and off.
        let run = |on: bool| {
            let mut pf = XsPortfolio::new(1000.0);
            if on {
                pf.enable_events();
            }
            pf.open(&m, 100, 0, Side::Long, 200.0, &p);
            for t in 100..140 {
                pf.step(&m, &[vec![None; 200]], t, &p);
            }
            (pf.equity, pf.positions.len(), pf.trades.len())
        };
        assert_eq!(run(true), run(false));
        // Off by default: nothing is recorded.
        let mut quiet = XsPortfolio::new(10.0);
        quiet.open(&m, 100, 0, Side::Long, 5000.0, &p);
        assert!(quiet.take_events().is_empty());
    }

    /// Test: market context describes the trend breadth and every coin.
    #[test]
    fn market_context_describes_the_trend_breadth_and_every_coin() {
        let m = ref_market((0..200).map(|t| 100.0 + t as f64 * 0.01).collect());
        let scores = sc(&m);
        let p = long_params(false, Regime::MarketTrend);
        let c = market_context(&m, &scores, 175, &p);
        assert_eq!(c["trend"]["passes"], true);
        assert!(c["trend"]["dist_pct"].as_f64().unwrap() > 0.0);
        let coins = c["coins"].as_array().unwrap();
        assert_eq!(coins.len(), 12);
        assert!(coins.iter().all(|x| x["symbol"].is_string()
            && x["close"].is_number()
            && x["ret_24h_pct"].is_number()));
        assert!(c["breadth_pct_above_own_24h_avg"].as_f64().unwrap() >= 0.0);
        assert!(c["zones"]["dispersion_24h_pct"].is_number() || c["zones"].is_null());
        assert_eq!(c["bar_close_ms"].as_i64().unwrap(), m.ts[175] + BAR_MS);
    }

    /// Test: backtests reject shorting settings.
    #[test]
    fn backtests_reject_shorting_settings() {
        let m = flat_with(&[]);
        let mut p = long_params(false, Regime::Off);
        p.long_only = false;
        let pf = backtest(&m, &[vec![None; 200]], 0..150, &p, 1000.0);
        assert!(pf
            .execution_error
            .as_deref()
            .is_some_and(|e| e.contains("shorting is removed")));
        assert!(pf.trades.is_empty() && pf.positions.is_empty());
        p.long_only = true;
        assert!(backtest(&m, &[vec![None; 200]], 0..150, &p, 1000.0)
            .execution_error
            .is_none());
    }

    /// Test: take profit uses 70 percent of the decided value.
    #[test]
    fn take_profit_uses_70_percent_of_the_decided_value() {
        let mut m = flat_with(&[]);
        m.marks[0][100] = mark_bar(100.0, 100.0, 100.0, 100.0);
        m.marks[0][101] = mark_bar(100.0, 106.5, 100.0, 106.0); // +6%: below 70% of 10%
        m.marks[0][102] = mark_bar(106.0, 108.5, 106.0, 108.0); // +8%: above 7%
        let mut p = risk_params(Risk {
            take_profit_pct: Some(10.0),
            ..Risk::default()
        });
        p.stop_pct = None;
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        for t in 100..=101 {
            pf.step(&m, &[vec![None; 200]], t, &p);
        }
        assert_eq!(
            pf.positions[0].next_action, None,
            "6% is under 70% of the 10% decided"
        );
        pf.step(&m, &[vec![None; 200]], 102, &p);
        assert_eq!(
            pf.positions[0].next_action,
            Some(NextAction::TakeProfit),
            "8% passes 7% = 70% of 10%"
        );
        assert!((TP_USE_FRACTION - 0.7).abs() < 1e-12);
    }

    /// Test: trailing stop follows the peak from the next bar and never loosens.
    #[test]
    fn trailing_stop_follows_the_peak_from_the_next_bar_and_never_loosens() {
        let mut m = flat_with(&[]);
        // Bar 101 spikes to 120 but also dips to 95: the trail it creates must NOT act in the
        // same bar (no look-ahead). Bar 102 stays above 108; bar 103 falls through it.
        m.marks[0][101] = mark_bar(100.0, 120.0, 95.0, 119.0);
        m.marks[0][102] = mark_bar(119.0, 119.0, 110.0, 112.0);
        m.marks[0][103] = mark_bar(112.0, 112.0, 107.0, 108.0);
        let mut p = risk_params(Risk {
            trail_pct: Some(10.0),
            ..Risk::default()
        });
        p.stop_pct = None;
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        for t in 100..=102 {
            pf.step(&m, &[vec![None; 200]], t, &p);
        }
        assert_eq!(
            pf.positions.len(),
            1,
            "same-bar spike and dip must not stop"
        );
        let stop = pf.positions[0].stop.unwrap();
        assert!(
            (stop - 108.0).abs() < 1e-9,
            "trail 10% behind the 120 peak, got {stop}"
        );
        pf.step(&m, &[vec![None; 200]], 103, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.trades[0].reason, "Stop");
        assert!(pf.trades[0].exit > 100.0, "the trail locked in a profit");
    }

    /// Test: breakeven stop arms after the trigger and exits at entry.
    #[test]
    fn breakeven_stop_arms_after_the_trigger_and_exits_at_entry() {
        let mut m = flat_with(&[]);
        m.marks[0][101] = mark_bar(100.0, 106.0, 100.0, 105.0);
        m.marks[0][102] = mark_bar(105.0, 105.0, 98.0, 99.0);
        let mut p = risk_params(Risk {
            breakeven_pct: Some(5.0),
            ..Risk::default()
        });
        p.stop_pct = None;
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        for t in 100..=101 {
            pf.step(&m, &[vec![None; 200]], t, &p);
        }
        assert!((pf.positions[0].stop.unwrap() - pf.positions[0].entry).abs() < 1e-9);
        pf.step(&m, &[vec![None; 200]], 102, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.trades[0].reason, "Stop");
    }

    /// Test: trailing stop never loosens an existing tighter stop.
    #[test]
    fn trailing_stop_never_loosens_an_existing_tighter_stop() {
        let m = flat_with(&[]);
        let mut p = risk_params(Risk {
            trail_pct: Some(50.0),
            ..Risk::default()
        });
        p.stop_pct = Some(5.0);
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        let before = pf.positions[0].stop.unwrap();
        pf.step(&m, &[vec![None; 200]], 100, &p);
        assert!(pf.positions[0].stop.unwrap() >= before);
    }

    /// Test: live exits lock in and book at the bar after its funding.
    #[test]
    fn live_exits_lock_in_and_book_at_the_bar_after_its_funding() {
        let mut m = flat_with(&[]);
        let mut p = risk_params(Risk::default());
        p.stop_pct = Some(5.0);
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        pf.step(&m, &[vec![None; 200]], 100, &p);
        // Funding settles at the open of bar 101; the live stop fires inside it.
        m.funding[0] = vec![(101 * BAR_MS, 0.001)];
        let marks = std::collections::HashMap::from([("AUSDT".to_string(), 94.0)]);
        let at = 101 * BAR_MS + 5 * 60_000;
        let lines = pf.live_check(&marks, at);
        assert_eq!(lines.len(), 1);
        assert!(
            !pf.live_check(&marks, at + 1)
                .iter()
                .any(|l| l.contains("stop hit")),
            "locked once"
        );
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!(pf.positions.is_empty());
        let t = &pf.trades[0];
        assert_eq!((t.reason.as_str(), t.exit_ts), ("Stop (live mark)", at));
        assert!(
            t.funding > 0.0,
            "the settlement before the live exit is paid"
        );
        assert!(
            t.exit < 94.0 && t.exit > 93.0,
            "mark minus measured book cost: {}",
            t.exit
        );
    }

    /// Test: live drawdown stop closes everything and blocks entries.
    #[test]
    fn live_drawdown_stop_closes_everything_and_blocks_entries() {
        let m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 400.0, &p);
        pf.step(&m, &[vec![None; 200]], 100, &p);
        // ~800 notional at 2x: a mark of 66 is about -27% of equity.
        let marks = std::collections::HashMap::from([("AUSDT".to_string(), 66.0)]);
        let lines = pf.live_check(&marks, 101 * BAR_MS + 1);
        assert!(pf.stopped_out && lines.iter().any(|l| l.contains("drawdown stop")));
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.trades[0].reason, "Drawdown stop (live mark)");
    }

    /// Test: entries wait out a wicked open and fill at the next fair one.
    #[test]
    fn entries_wait_out_a_wicked_open_and_fill_at_the_next_fair_one() {
        // Bar 101's traded open spikes 2% above the mark open; bar 102 is fair.
        let mut m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        if let Some(b) = m.bars[0][101].as_mut() {
            b.open = 102.0;
            b.high = 102.0;
        }
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pending_pair(&mut pf, &p);
        let slot = pf.pending_slot;
        pf.step(&m, &vec![vec![None; 200]; 2], 101, &p);
        assert!(pf.positions.is_empty(), "no entry into a wicked open");
        assert!(pf.pending_targets.is_some());
        assert_eq!(
            pf.pending_slot, slot,
            "the decided budget is kept while waiting"
        );
        pf.step(&m, &vec![vec![None; 200]; 2], 102, &p);
        assert_eq!(pf.positions.len(), 2);
        assert!(
            pf.positions.iter().all(|x| x.entry < 100.1),
            "filled at the fair open"
        );
    }

    /// Test: trades record the decision bar even when a wick delays the fill.
    #[test]
    fn trades_record_the_decision_bar_even_when_a_wick_delays_the_fill() {
        // Decided at bar 99's close; bar 101's open is wicked so the fill waits until bar 102.
        let mut m = market(&[vec![100.0; 200], vec![100.0; 200]]);
        if let Some(b) = m.bars[0][101].as_mut() {
            b.open = 102.0;
            b.high = 102.0;
        }
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pending_pair(&mut pf, &p);
        pf.pending_decided_at = Some(m.ts[99]);
        pf.step(&m, &vec![vec![None; 200]; 2], 101, &p);
        assert!(pf.positions.is_empty(), "no entry into a wicked open");
        pf.step(&m, &vec![vec![None; 200]; 2], 102, &p);
        assert_eq!(pf.positions.len(), 2);
        for pos in &pf.positions {
            assert_eq!(pos.entry_ts, m.ts[102], "filled at the delayed open");
            assert_eq!(
                pos.decided_ts,
                Some(m.ts[99]),
                "but decided at bar 99, not entry minus two"
            );
        }
        pf.close_all(&m, m.ts[110], "End of test");
        assert!(pf.trades.iter().all(|t| t.decided_ts == Some(m.ts[99])));
    }

    /// Test: volatility weights are fixed at the decision close.
    #[test]
    fn volatility_weights_are_fixed_at_the_decision_close() {
        let series: Vec<Vec<f64>> = (0..12)
            .map(|s| {
                (0..200)
                    .map(|t| {
                        100.0 + ((t as f64) * (0.3 + s as f64 * 0.07)).sin() * (1.0 + s as f64)
                    })
                    .collect()
            })
            .collect();
        let m = market(&series);
        let scores = sc(&m);
        let mut p = long_params(false, Regime::Off);
        p.risk.vol_scaled = true;
        let mut pf = XsPortfolio::new(300.0);
        pf.decide_close(&m, &scores, 175, &p);
        let decided = pf.pending_weights.clone();
        assert!(!decided.is_empty() && decided.values().any(|w| (w - 1.0).abs() > 1e-6));
        // Later bars change volatility; the fill must still use the decided weights.
        let mut later = m.clone();
        for row in later.bars.iter_mut() {
            for b in row.iter_mut().skip(176).flatten() {
                b.high *= 1.5;
            }
        }
        pf.step(&later, &scores, 176, &p);
        pf.step(&later, &scores, 177, &p);
        assert!(!pf.positions.is_empty());
        let base = pf
            .positions
            .iter()
            .map(|x| (x.margin + x.fees) / decided[&x.symbol])
            .collect::<Vec<_>>();
        assert!(
            base.windows(2).all(|w| (w[0] - w[1]).abs() / w[0] < 0.02),
            "{base:?}"
        );
    }

    /// Test: held positions stay manageable when fresh rules disappear.
    #[test]
    fn held_positions_stay_manageable_when_fresh_rules_disappear() {
        // Turnover moved the coin out of the measured set: no fresh rules now.
        let mut m = flat_with(&[]);
        let mut p = risk_params(Risk::default());
        p.stop_pct = Some(5.0);
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        m.rules[0] = None;
        m.funding[0] = vec![(101 * BAR_MS, 0.0005)];
        pf.step(&m, &[vec![None; 200]], 100, &p);
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!(pf.execution_error.is_none(), "{:?}", pf.execution_error);
        assert!(
            pf.positions[0].funding > 0.0,
            "funding still paid, liquidation recomputed"
        );
        // A scheduled exit still fills, at the entry-time book cost.
        pf.close_all(&m, 102 * BAR_MS, "End of test");
        assert!(pf.execution_error.is_none(), "{:?}", pf.execution_error);
        assert!(pf.positions.is_empty() && pf.trades.len() == 1);
    }

    /// Test: settlement at the newest close waits for the next bar.
    #[test]
    fn settlement_at_the_newest_close_waits_for_the_next_bar() {
        let mut m = flat_with(&[]);
        let p = risk_params(Risk::default());
        // A settlement stamped at the close of the last loaded bar.
        m.funding[0] = vec![(200 * BAR_MS, 0.001)];
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 199, 0, Side::Long, 200.0, &p);
        pf.step(&m, &[vec![None; 200]], 199, &p);
        assert!(pf.execution_error.is_none(), "{:?}", pf.execution_error);
        assert_eq!(pf.positions[0].funding, 0.0);
        // Once that bar exists, the settlement is charged at its mark open.
        let mut next = flat_with(&[]);
        next.ts = (1..201).map(|i| i * BAR_MS).collect();
        next.funding[0] = m.funding[0].clone();
        pf.step(&next, &[vec![None; 200]], 199, &p);
        assert!(pf.execution_error.is_none());
        let pos = &pf.positions[0];
        assert!((pos.funding - 0.001 * pos.qty * 100.0).abs() < 1e-9);
    }

    /// Test: boundary funding is paid by outgoing holdings.
    #[test]
    fn boundary_funding_is_paid_by_outgoing_holdings() {
        let mut m = flat_with(&[]);
        m.funding[0] = vec![(101 * BAR_MS, 0.01)];
        let sc = vec![vec![None; 200]];
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        let qty = pf.positions[0].qty;
        pf.pending_targets = Some(vec![(0, "AUSDT".into(), Side::Short)]);
        pf.step(&m, &sc, 101, &p);
        assert!((pf.trades[0].funding - qty).abs() < 1e-9);
        assert!(
            pf.positions.is_empty(),
            "unpaired replacement must be rejected"
        );
        assert_eq!(pf.rejected_rebalances, 1);
        pf.open(&m, 101, 0, Side::Short, 200.0, &p);
        assert_eq!(pf.positions[0].funding, 0.0);
    }

    /// Test: missing candle does not erase funding.
    #[test]
    fn missing_candle_does_not_erase_funding() {
        let mut m = flat_with(&[]);
        m.bars[0][101] = None;
        m.funding[0] = vec![(101 * BAR_MS, 0.01)];
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        let qty = pf.positions[0].qty;
        let mark = pf.positions[0].mark;
        pf.step(&m, &[vec![None; 200]], 101, &p);
        assert!((pf.positions[0].funding - 0.01 * qty * mark).abs() < 1e-9);
    }

    /// Test: queued exits survive missing candles.
    #[test]
    fn queued_exits_survive_missing_candles() {
        for action in [
            NextAction::Stop,
            NextAction::TakeProfit,
            NextAction::Rebalance,
        ] {
            let mut m = flat_with(&[]);
            m.bars[0][101] = None;
            let sc = vec![vec![None; 200]];
            let p = risk_params(Risk::default());
            let mut pf = XsPortfolio::new(1000.0);
            pf.open(&m, 100, 0, Side::Long, 200.0, &p);
            pf.positions[0].next_action = Some(action);
            pf.step(&m, &sc, 101, &p);
            assert_eq!(pf.positions[0].next_action, Some(action));
            pf.step(&m, &sc, 102, &p);
            assert!(pf.positions.is_empty());
            assert_eq!(pf.trades[0].exit_ts, 102 * BAR_MS);
        }
    }

    /// Test: breaker and rebalance exits survive missing candles.
    #[test]
    fn breaker_and_rebalance_exits_survive_missing_candles() {
        for breaker in [false, true] {
            let mut m = flat_with(&[]);
            m.bars[0][101] = None;
            let sc = vec![vec![None; 200]];
            let p = risk_params(Risk::default());
            let mut pf = XsPortfolio::new(1000.0);
            pf.open(&m, 100, 0, Side::Long, 200.0, &p);
            if breaker {
                pf.flatten_next = true;
            } else {
                pf.pending_targets = Some(vec![]);
            }
            pf.step(&m, &sc, 101, &p);
            pf.step(&m, &sc, 102, &p);
            assert!(pf.positions.is_empty());
        }
    }

    /// Test: final closing costs are in drawdown.
    #[test]
    fn final_closing_costs_are_in_drawdown() {
        let m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        pf.step(&m, &[vec![None; 200]], 100, &p);
        let before = pf.metrics().max_drawdown_pct;
        pf.close_all(&m, 101 * BAR_MS, "End of test");
        assert!(pf.metrics().max_drawdown_pct > before);
        assert!((pf.metrics().max_drawdown_pct - (1000.0 - pf.equity) / 10.0).abs() < 1e-9);
    }

    /// Test: longs losers and shorts winners.
    #[test]
    fn longs_losers_and_shorts_winners() {
        let mk = |drift: f64| {
            (0..200)
                .map(|i| 100.0 * (1.0 + drift * i as f64 / 200.0))
                .collect::<Vec<f64>>()
        };
        let m = market(&[mk(-0.2), mk(-0.1), mk(0.0), mk(0.1), mk(0.2)]);
        let s = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        let t = targets(&m, &s, 190, &p);
        assert_eq!(t.get(&0), Some(&Side::Long));
        assert_eq!(t.get(&4), Some(&Side::Short));
        assert_eq!(t.len(), 2);
    }

    /// Test: rebalance is timestamp aligned and executes next open.
    #[test]
    fn rebalance_is_timestamp_aligned_and_executes_next_open() {
        assert!(is_rebalance(15 * BAR_MS, 16));
        assert!(!is_rebalance(14 * BAR_MS, 16));
        let mk = |drift: f64| {
            (0..200)
                .map(|i| 100.0 * (1.0 + drift * i as f64 / 200.0))
                .collect::<Vec<f64>>()
        };
        let m = market(&[mk(-0.2), mk(-0.1), mk(0.0), mk(0.1), mk(0.2)]);
        let s = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        let mut pf = XsPortfolio::new(100.0);
        for t in 160..=175 {
            pf.step(&m, &s, t, &p);
        }
        // bar 175 is a rebalance close (ts+BAR = 176*BAR, 176 % 16 == 0): nothing held yet.
        assert!(pf.positions.is_empty());
        pf.step(&m, &s, 176, &p);
        assert!(
            pf.positions.is_empty(),
            "decision latency: not at the very next open"
        );
        pf.step(&m, &s, 177, &p);
        assert_eq!(pf.positions.len(), 2);
        assert!(pf.positions.iter().all(|x| x.entry_ts == m.ts[177]));
    }

    /// Step without the end-of-test close (positions stay open).
    fn stepped(
        m: &Market,
        s: &[Vec<Option<Score>>],
        range: std::ops::Range<usize>,
        p: &XsParams,
    ) -> XsPortfolio {
        let mut pf = XsPortfolio::new(100.0);
        for t in range {
            pf.step(m, s, t, p);
        }
        pf
    }

    /// Test: backtest closes everything at the end.
    #[test]
    fn backtest_closes_everything_at_the_end() {
        let m = market(&wavy(200));
        let s = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 2,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        let pf = backtest(&m, &s, 150..199, &p, 100.0);
        assert!(pf.positions.is_empty());
        let r = pf.metrics();
        assert!(
            ((r.gross_profit - r.gross_loss) - r.net()).abs() < 1e-9,
            "net must equal closed-trade P&L"
        );
    }

    /// One symbol, flat at 100, with chosen (open, high, low, close) bars.
    type Ohlc = (f64, f64, f64, f64);

    /// Test fixture: a flat 200-bar market with the given candles changed.
    fn flat_with(changes: &[(usize, Ohlc)]) -> Market {
        let mut bars: Vec<Option<crate::engine::Bar>> = (0..200)
            .map(|_| {
                Some(crate::engine::Bar {
                    open: 100.0,
                    high: 100.0,
                    low: 100.0,
                    close: 100.0,
                    volume: 1.0,
                    turnover: 1.0,
                })
            })
            .collect();
        for &(t, (o, h, l, c)) in changes {
            bars[t] = Some(crate::engine::Bar {
                open: o,
                high: h,
                low: l,
                close: c,
                volume: 1.0,
                turnover: 1.0,
            });
        }
        Market {
            marks: vec![bars.clone()],
            listing_times: Vec::new(),
            entry_eligible: Vec::new(),
            ts: (0..200).map(|i| i * BAR_MS).collect(),
            wall: (0..200).map(|i| i * BAR_MS).collect(),
            symbols: vec!["AUSDT".into()],
            bars: vec![bars],
            funding: vec![vec![]],
            rules: vec![Some(crate::engine::rules::Rules::test_liquid()); 3],
        }
    }

    /// Test fixture: Pulse settings using the given risk rules.
    fn risk_params(risk: Risk) -> XsParams {
        XsParams {
            signal: Signal::Pulse,
            flip: false,
            lookback: 0,
            hold: 96,
            top: 1,
            gross_leverage: 2.0,
            stop_pct: None,
            risk,
            long_only: false,
            regime: Regime::Off,
        }
    }

    /// Close-based stop: a wick through 10% that closes back is ignored; a CLOSE
    /// 10% against exits at the NEXT open, not at the close.
    #[test]
    fn close_stop_ignores_wicks_and_exits_next_open() {
        let m = flat_with(&[
            (101, (100.0, 100.5, 85.0, 99.0)),
            (102, (99.0, 99.5, 88.0, 89.0)),
            (103, (89.0, 89.5, 88.5, 89.0)),
            (104, (88.0, 90.0, 87.0, 89.5)),
        ]);
        let s = vec![vec![None; 200]];
        let p = risk_params(Risk {
            close_stop_pct: Some(10.0),
            ..Default::default()
        });
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        pf.step(&m, &s, 100, &p);
        pf.step(&m, &s, 101, &p);
        assert_eq!(pf.positions[0].next_action, None, "-15% wick but close -1%");
        pf.step(&m, &s, 102, &p);
        assert_eq!(
            pf.positions.len(),
            1,
            "decided at the close, not filled at it"
        );
        assert_eq!(pf.positions[0].next_action, Some(NextAction::Stop));
        pf.step(&m, &s, 103, &p);
        assert_eq!(
            pf.positions.len(),
            1,
            "decision latency: not at the very next open"
        );
        pf.step(&m, &s, 104, &p);
        let tr = pf.trades.last().unwrap();
        assert_eq!(tr.reason, "Stop (close)");
        assert!(
            (tr.exit - 88.0 * (1.0 - crate::engine::rules::TEST_COST)).abs() < 1e-9
                && tr.exit_ts == m.ts[104]
        );
    }

    /// Test: short stop only stops shorts.
    #[test]
    fn short_stop_only_stops_shorts() {
        let m = flat_with(&[
            (101, (100.0, 116.0, 99.0, 115.0)),
            (102, (115.0, 116.0, 114.0, 115.0)),
            (103, (115.0, 116.0, 114.0, 115.0)),
        ]);
        let s = vec![vec![None; 200]];
        let p = risk_params(Risk {
            short_stop_pct: Some(15.0),
            ..Default::default()
        });
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Short, 200.0, &p);
        pf.step(&m, &s, 100, &p);
        pf.step(&m, &s, 101, &p);
        pf.step(&m, &s, 102, &p);
        pf.step(&m, &s, 103, &p);
        assert_eq!(pf.trades.last().unwrap().reason, "Stop (close)");
        let m2 = flat_with(&[
            (101, (100.0, 101.0, 84.0, 85.0)),
            (102, (85.0, 86.0, 84.0, 85.0)),
            (103, (85.0, 86.0, 84.0, 85.0)),
        ]);
        let mut pf2 = XsPortfolio::new(1000.0);
        pf2.open(&m2, 100, 0, Side::Long, 200.0, &p);
        for t in 100..=103 {
            pf2.step(&m2, &s, t, &p);
        }
        assert_eq!(pf2.positions.len(), 1, "a long 15% down is not stopped");
    }

    /// Test: take profit on close exits next open.
    #[test]
    fn take_profit_on_close_exits_next_open() {
        let m = flat_with(&[
            (101, (100.0, 112.0, 99.0, 111.0)),
            (102, (111.0, 111.5, 110.5, 111.0)),
            (103, (110.0, 111.0, 109.0, 110.0)),
        ]);
        let s = vec![vec![None; 200]];
        let p = risk_params(Risk {
            take_profit_pct: Some(10.0),
            ..Default::default()
        });
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        for t in 100..=103 {
            pf.step(&m, &s, t, &p);
        }
        let tr = pf.trades.last().unwrap();
        assert_eq!(tr.reason, "Take profit (close)");
        assert!(
            (tr.exit - 110.0 * (1.0 - crate::engine::rules::TEST_COST)).abs() < 1e-9,
            "fills at the next open, not the high"
        );
    }

    /// Test: add once averages the entry.
    #[test]
    fn add_once_averages_the_entry() {
        let m = flat_with(&[
            (101, (100.0, 100.0, 89.0, 90.0)),
            (102, (90.0, 90.0, 90.0, 90.0)),
            (103, (90.0, 90.0, 90.0, 90.0)),
            (104, (80.0, 80.0, 79.0, 79.5)),
        ]);
        let s = vec![vec![None; 200]];
        let p = risk_params(Risk {
            add_pct: Some(10.0),
            ..Default::default()
        });
        let mut pf = XsPortfolio::new(1000.0);
        pf.open(&m, 100, 0, Side::Long, 200.0, &p);
        for t in 100..=104 {
            pf.step(&m, &s, t, &p);
        }
        let pos = &pf.positions[0];
        assert!(pos.added);
        let first = 100.0 * (1.0 + crate::engine::rules::TEST_COST);
        let second = 90.0 * (1.0 + crate::engine::rules::TEST_COST);
        assert!(
            (pos.entry - (first + second) / 2.0).abs() < 0.01,
            "same size again at the next open (90)"
        );
        assert_eq!(pos.next_action, None, "never adds twice");
    }

    /// Test: breaker flattens after equity falls from the rebalance.
    #[test]
    fn breaker_flattens_after_equity_falls_from_the_rebalance() {
        let m = flat_with(&[
            (101, (100.0, 100.0, 60.0, 60.0)),
            (102, (60.0, 60.0, 60.0, 60.0)),
            (103, (60.0, 60.0, 60.0, 60.0)),
        ]);
        let s = vec![vec![None; 200]];
        let p = risk_params(Risk {
            breaker_pct: Some(10.0),
            ..Default::default()
        });
        let mut pf = XsPortfolio::new(1000.0);
        // 400 budget at 1x = ~400 notional: -40% is -16% of equity, past the
        // 10% breaker but short of the 25% drawdown stop.
        let mut p = p;
        p.gross_leverage = 1.0;
        pf.open(&m, 100, 0, Side::Long, 400.0, &p);
        pf.step(&m, &s, 100, &p);
        pf.step(&m, &s, 101, &p);
        assert!(pf.flatten_next, "-40% on 400 notional = -16% equity");
        assert!(!pf.stopped_out);
        pf.step(&m, &s, 102, &p);
        assert_eq!(
            pf.positions.len(),
            1,
            "decision latency: not at the very next open"
        );
        pf.step(&m, &s, 103, &p);
        assert!(pf.positions.is_empty());
        assert_eq!(pf.trades.last().unwrap().reason, "Breaker");
    }

    /// Test: realized vol uses only past bars.
    #[test]
    fn realized_vol_uses_only_past_bars() {
        let m = market(&wavy(300));
        let v = realized_vol(&m, 0, 200).unwrap();
        let mut later = m.clone();
        for b in later.bars[0].iter_mut().skip(201).flatten() {
            b.close *= 3.0;
        }
        assert_eq!(realized_vol(&later, 0, 200), Some(v));
    }

    /// Orders the account cannot fund are rejected.
    #[test]
    fn rejects_orders_beyond_available_margin() {
        let m = flat_with(&[]);
        let p = risk_params(Risk::default());
        let mut pf = XsPortfolio::new(100.0);
        pf.open(&m, 100, 0, Side::Long, 90.0, &p);
        assert_eq!(pf.positions.len(), 1);
        let used = pf.positions[0].margin + pf.positions[0].fees;
        assert!(
            (used - 90.0).abs() < 1e-6,
            "margin + fee == budget ({used})"
        );
        pf.open(&m, 100, 0, Side::Short, 20.0, &p);
        assert_eq!(pf.positions.len(), 1, "only ~10 USDT available: rejected");
    }

    /// With adds on, each slot keeps room for its add: base + add == the slot.
    #[test]
    fn averaging_down_never_halves_the_first_order() {
        let m = market(&wavy(200));
        let s = sc(&m);
        let mut p = risk_params(Risk::default());
        p.signal = Signal::Return;
        p.lookback = 16;
        p.hold = 16;
        p.top = 2;
        // Bar 159 closes a rebalance (hold 16); positions open at bar 161, before any add.
        let plain = stepped(&m, &s, 150..162, &p);
        p.risk.add_pct = Some(10.0);
        let with_add = stepped(&m, &s, 150..162, &p);
        assert!(!plain.positions.is_empty());
        let total = |pf: &XsPortfolio| pf.positions.iter().map(|x| x.margin).sum::<f64>();
        assert!((total(&with_add) - total(&plain)).abs() / total(&plain) < 1e-9);
    }

    /// Test: cash flows resize but are not profit.
    #[test]
    fn cash_flows_resize_but_are_not_profit() {
        let mut pf = XsPortfolio::new(100.0);
        pf.cash_flow(50.0);
        assert_eq!(pf.equity, 150.0);
        assert_eq!(pf.metrics().net(), 0.0, "a deposit is not profit");
        pf.cash_flow(-30.0);
        assert_eq!(pf.metrics().net(), 0.0, "a withdrawal is not a loss");
    }

    /// Test: daily rows add up to the backtest.
    #[test]
    fn daily_rows_add_up_to_the_backtest() {
        let m = market(&wavy(400));
        let s = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 2,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: true,
            regime: Regime::Off,
        };
        let bt = backtest(&m, &s, 120..400, &p, 100.0);
        let mut pf = XsPortfolio::new(100.0);
        let mut days = Vec::new();
        run_daily(&m, &s, 120..400, &p, &mut pf, &mut days, true);
        assert!(days.len() >= 3);
        assert!((days.last().unwrap().equity - bt.equity).abs() < 1e-9);
        assert!((days.iter().map(|d| d.pnl).sum::<f64>() - (bt.equity - 100.0)).abs() < 1e-9);
        assert_eq!(
            days.iter().map(|d| d.trades).sum::<usize>(),
            bt.trades.len()
        );
        assert!(days.windows(2).all(|w| w[1].day_ts - w[0].day_ts == DAY_MS));
    }

    /// Funding carry: long the most negative hourly funding, short the most
    /// positive, normalising 1h vs 8h intervals; only settled funding counts.
    #[test]
    fn funding_carry_ranks_hourly_rate_from_settled_funding_only() {
        let mut m = market(&wavy(200));
        let s = sc(&m);
        let h = 3_600_000;
        let t = 190;
        let now = m.ts[t];
        for f in m.funding.iter_mut() {
            *f = vec![(now - 16 * h, 0.0001), (now - 8 * h, 0.0001), (now, 0.0001)];
        }
        // Symbol 1: 1h funding at -0.0002/h -> most negative per hour.
        m.funding[1] = (0..4).map(|i| (now - (3 - i) * h, -0.0002)).collect();
        // Symbol 2: 8h funding 0.004 = 0.0005/h -> most positive.
        m.funding[2] = vec![(now - 16 * h, 0.004), (now - 8 * h, 0.004), (now, 0.004)];
        // Symbol 3: a huge rate settling AFTER bar t must be ignored.
        m.funding[3].push((now + BAR_MS, 1.0));
        let p = XsParams {
            signal: Signal::Funding,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        let tg = targets(&m, &s, t, &p);
        assert_eq!(tg.get(&1), Some(&Side::Long));
        assert_eq!(tg.get(&2), Some(&Side::Short));
        assert_eq!(tg.len(), 2);
    }

    /// Test fixture: eight sine-wave close series of length `n`.
    fn wavy(n: usize) -> Vec<Vec<f64>> {
        (0..8)
            .map(|k| {
                (0..n)
                    .map(|i| {
                        100.0 + ((i as f64) * (0.03 + k as f64 * 0.011)).sin() * (3.0 + k as f64)
                    })
                    .collect()
            })
            .collect()
    }

    /// No repaint / lookahead: everything decided and filled through bar t is
    /// identical whether or not bars after t exist, or are wildly different.
    #[test]
    fn decisions_never_depend_on_future_bars() {
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 2,
            gross_leverage: 2.0,
            stop_pct: Some(20.0),
            risk: Risk::default(),
            long_only: true,
            regime: Regime::Off,
        };
        let full = wavy(400);
        let mut altered = full.clone();
        for s in altered.iter_mut() {
            for (i, x) in s.iter_mut().enumerate().skip(301) {
                *x *= 1.0 + (i as f64 * 0.7).sin() * 0.3;
            }
        }
        let cut: Vec<Vec<f64>> = full.iter().map(|s| s[..=300].to_vec()).collect();
        let run = |series: &[Vec<f64>]| {
            let m = market(series);
            let s = sc(&m);
            let pf = backtest(&m, &s, 120..301, &p, 100.0);
            (
                serde_json::to_string(&pf).unwrap(),
                (120..301)
                    .map(|t| targets(&m, &s, t, &p))
                    .collect::<Vec<_>>(),
            )
        };
        let (a, b, c) = (run(&full), run(&altered), run(&cut));
        assert!(
            !a.0.contains("\"trades\":[]"),
            "test needs trades to be meaningful"
        );
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    /// Walk-forward not passing: no new positions; the next rebalance goes flat.
    #[test]
    fn unvalidated_goes_flat_at_next_rebalance() {
        let m = market(&wavy(220));
        let s = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 2,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        let mut pf = stepped(&m, &s, 150..180, &p);
        assert!(!pf.positions.is_empty());
        pf.entries_allowed = false;
        for t in 180..219 {
            pf.step(&m, &s, t, &p);
        }
        assert!(pf.positions.is_empty());
        assert!(pf.trades.iter().all(|t| t.entry_ts < m.ts[180]));
    }

    /// Live: a new listing shifts symbol indices; positions must follow by name.
    #[test]
    fn reindex_follows_names_and_refuses_invented_delisting_fills() {
        let m = market(&wavy(200));
        let s = sc(&m);
        let p = XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 16,
            hold: 16,
            top: 2,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: false,
            regime: Regime::Off,
        };
        let pf0 = stepped(&m, &s, 150..199, &p);
        assert_eq!(pf0.positions.len(), 4);
        let mut pf = pf0.clone();
        let mut shifted = vec!["AAANEWUSDT".to_string()];
        shifted.extend(m.symbols.iter().cloned());
        pf.reindex(&shifted);
        for x in &pf.positions {
            assert_eq!(shifted[x.sym], x.symbol);
        }
        let gone = pf0.positions[0].symbol.clone();
        let mut pf = pf0.clone();
        let fewer: Vec<String> = m.symbols.iter().filter(|x| **x != gone).cloned().collect();
        pf.reindex(&fewer);
        assert_eq!(pf.positions.len(), 4);
        assert!(pf.execution_error.as_ref().unwrap().contains(&gone));
        assert_eq!(pf.trades.len(), pf0.trades.len());
    }

    /// Test: graduated sizing only scales legs down and keeps a share affordable.
    #[test]
    fn graduated_sizing_only_scales_legs_down_and_keeps_a_share_affordable() {
        let m = graded_multipliers(4, Some(0.6), 1.0);
        assert_eq!(
            m[0], 1.0,
            "the best-ranked leg keeps the full validated slot"
        );
        assert!(
            (m[3] - MIN_SIZE_FRACTION).abs() < 1e-9,
            "the worst leg would get 1 - grade_rank = 0.4 but no trade is below 50% of its slot"
        );
        assert!(
            (graded_multipliers(4, Some(0.3), 1.0)[3] - 0.7).abs() < 1e-9,
            "above the floor the worst leg gets 1 - grade_rank"
        );
        assert!(
            m.windows(2).all(|w| w[0] >= w[1]),
            "never larger for a worse rank: {m:?}"
        );
        assert!(
            graded_multipliers(4, None, 0.7)
                .iter()
                .all(|x| (x - 0.7).abs() < 1e-9),
            "trend grading scales every leg"
        );
        assert!(
            graded_multipliers(3, Some(5.0), 0.1)
                .iter()
                .all(|x| (MIN_SIZE_FRACTION..=1.0).contains(x)),
            "always inside [50%, 100%]"
        );
        assert_eq!(
            graded_multipliers(1, Some(0.7), 1.0),
            vec![1.0],
            "a single leg is not graded by rank"
        );
        // a leg whose scaled budget cannot buy one share is lifted to just afford it, never above the validated slot
        assert!((afford_floor(0.1, 100.0, 500.0) - 0.204).abs() < 1e-9);
        assert_eq!(
            afford_floor(0.1, 100.0, 50.0),
            1.0,
            "a share costing more than the slot stays at the slot"
        );
        assert_eq!(afford_floor(0.8, 10.0, 1000.0), 0.8);
    }

    /// Test: risk based sizing and the three drawdown tiers.
    #[test]
    fn risk_based_sizing_and_the_three_drawdown_tiers() {
        // 1% of 50,000 = 500 at risk; a 2% stop -> 25,000 USD of position; the slot is 40,000, so the leg is cut to 62.5%
        assert!((risk_multiplier(1.0, 50_000.0, 2.0, 40_000.0) - 0.625).abs() < 1e-9);
        assert_eq!(
            risk_multiplier(1.0, 50_000.0, 0.5, 40_000.0),
            1.0,
            "a tight stop allows the whole slot but never more"
        );
        assert_eq!(risk_multiplier(0.0, 50_000.0, 2.0, 40_000.0), 1.0, "off");
        let tiers = (5.0, 15.0, 0.5);
        assert_eq!(
            drawdown_multiplier(2.0, tiers),
            Some(1.0),
            "tier 1: no change"
        );
        assert_eq!(
            drawdown_multiplier(5.0, tiers),
            Some(0.5),
            "tier 2: half size"
        );
        assert_eq!(drawdown_multiplier(14.9, tiers), Some(0.5));
        assert_eq!(
            drawdown_multiplier(15.0, tiers),
            None,
            "tier 3: no new entries"
        );
    }

    /// Test: the loss-streak pause blocks entries only after a full streak of losers, only for its hours, and is off by default.
    #[test]
    fn loss_streak_pause_blocks_entries_only_after_a_losing_streak_for_its_hours() {
        let h = 3_600_000_i64;
        let trade = |exit_h: i64, pnl: f64| Trade {
            symbol: "AAPL".into(),
            side: Side::Long,
            entry_ts: 0,
            exit_ts: exit_h * h,
            entry: 100.0,
            exit: 99.0,
            qty: 1.0,
            leverage: 1.0,
            fees: 0.0,
            funding: 0.0,
            pnl,
            r: pnl,
            reason: "test".into(),
            decided_ts: None,
        };
        let rule = Some((2, 10.0));
        let win_then_loss = [trade(1, 5.0), trade(2, -1.0)];
        assert!(
            !loss_pause_active(&win_then_loss, rule, 3 * h),
            "a winner inside the streak window resets it"
        );
        let two_losses = [trade(1, 5.0), trade(2, -1.0), trade(4, -2.0)];
        assert!(
            loss_pause_active(&two_losses, rule, 4 * h),
            "two losers in a row pause entries at once"
        );
        assert!(
            loss_pause_active(&two_losses, rule, 14 * h),
            "still paused exactly 10 h after the last loser closed"
        );
        assert!(
            !loss_pause_active(&two_losses, rule, 14 * h + 1),
            "entries resume after the pause"
        );
        assert!(
            !loss_pause_active(&[trade(4, -2.0)], rule, 5 * h),
            "fewer trades than the streak never pause"
        );
        assert!(
            !loss_pause_active(&two_losses, None, 4 * h),
            "off unless the option is set"
        );
        assert!(
            !loss_pause_active(&two_losses, Some((0, 10.0)), 4 * h),
            "a zero streak is off"
        );
    }

    /// Test: the rolling-loss stop fires only when the closed trades inside its window lost more than its percentage of the window's
    /// starting equity; old losses, gains and the off setting never block.
    #[test]
    fn rolling_loss_stop_blocks_entries_only_after_a_loss_inside_its_window() {
        let h = 3_600_000_i64;
        let trade = |exit_h: i64, pnl: f64| Trade {
            symbol: "AAPL".into(),
            side: Side::Long,
            entry_ts: 0,
            exit_ts: exit_h * h,
            entry: 100.0,
            exit: 99.0,
            qty: 1.0,
            leverage: 1.0,
            fees: 0.0,
            funding: 0.0,
            pnl,
            r: pnl,
            reason: "test".into(),
            decided_ts: None,
        };
        let rule = Some((3.0, 100.0));
        // equity 9,600 after losing 400 in the window: the window started at 10,000, so 4% > 3%
        let recent = [trade(10, 50.0), trade(20, -250.0), trade(30, -200.0)];
        assert!(
            roll_stop_active(&recent, rule, 9_600.0, 40 * h),
            "-400 on a 10,000 start is -4%, above the 3% limit"
        );
        assert!(
            !roll_stop_active(&recent, Some((5.0, 100.0)), 9_600.0, 40 * h),
            "-4% is inside a 5% limit"
        );
        assert!(
            !roll_stop_active(&recent, rule, 9_600.0, 200 * h),
            "losses older than the window are forgotten"
        );
        assert!(
            !roll_stop_active(&[trade(10, 300.0)], rule, 10_300.0, 40 * h),
            "a gain never blocks"
        );
        assert!(
            !roll_stop_active(&recent, None, 9_600.0, 40 * h),
            "off unless the option is set"
        );
        assert!(
            !roll_stop_active(&recent, Some((0.0, 100.0)), 9_600.0, 40 * h),
            "a zero limit is off"
        );
    }
}

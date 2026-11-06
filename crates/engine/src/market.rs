//! Builds the bot's `Market` from the stored candle snapshot, following the bot's own pipeline
//! (examples/common/mod.rs `load_data` + `big_market`) so ThomasAi tests exactly what the bot would.
//! Data: the merged base (`thomas merge`): moomoo candles, with Yahoo bars only where moomoo has none (Yahoo turnover is an estimate, flagged in the table).
use anyhow::{Context, Result, ensure};
use moomoo_bot::engine::moomoo::{
    Sessions, SymbolData, build_market, liquidity_from_bars, load_holdings,
};
use moomoo_bot::engine::rules::Rules;
use moomoo_bot::engine::{BAR_MS, Bar, Interval, Market, REFERENCE};
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::sessions::sessions_from_bars;

/// 2026-01-02 00:00 UTC: the bot's research floor (owner rule: 2026 data and later only).
pub const RESEARCH_FROM_MS: i64 = 1_767_312_000_000;

#[derive(Default)]
pub struct Loaded {
    /// The real trading calendar since the research start (empty if `thomas fetch-sessions` has not run: sessions are then derived from bar times).
    pub calendar: Vec<(i64, i64)>,
    /// Where the load time goes: (step, duration).
    pub timings: Vec<(&'static str, std::time::Duration)>,
    pub data: Vec<SymbolData>,
    /// (symbol, reason) for everything left out, so nothing disappears silently.
    pub dropped: Vec<(String, String)>,
}

/// Bot-style `SymbolData` for every SPY holding (plus SPY) that has enough clean moomoo candles.
pub fn load_symbol_data(snapshot: &Path, store: &Path, holdings: &Path) -> Result<Loaded> {
    let mut out = Loaded::default();
    let mut lap = std::time::Instant::now();

    let mut rules: BTreeMap<String, Rules> = BTreeMap::new();
    if snapshot.exists() {
        let db = Connection::open_with_flags(snapshot, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let has_rules: bool = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='rules'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if has_rules {
            let mut st = db.prepare("SELECT r.symbol, r.json FROM rules r JOIN (SELECT symbol, max(ts_ms) AS m FROM rules GROUP BY symbol) x ON r.symbol = x.symbol AND r.ts_ms = x.m")?;
            for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (s, j) = row?;
                rules.insert(s, serde_json::from_str(&j).context("rules json")?);
            }
        }
    }

    out.timings.push(("rules", lap.elapsed()));
    lap = std::time::Instant::now();
    // The merged base (thomas merge): moomoo bars, Yahoo only where moomoo lacks a real bar; placeholders are already out.
    let ours = Connection::open_with_flags(store, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut bars: BTreeMap<String, BTreeMap<i64, Bar>> = BTreeMap::new();
    let mut st = ours.prepare("SELECT moomoo_symbol, wall_ms, open, high, low, close, volume, turnover FROM candles_merged")?;
    for row in st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            Bar {
                open: r.get(2)?,
                high: r.get(3)?,
                low: r.get(4)?,
                close: r.get(5)?,
                volume: r.get(6)?,
                turnover: r.get(7)?,
            },
        ))
    })? {
        let (s, w, b) = row?;
        bars.entry(s).or_default().insert(w, b);
    }
    ensure!(
        !bars.is_empty(),
        "candles_merged is empty: run `thomas merge`"
    );

    for b in bars.values_mut() {
        b.retain(|&w, _| w >= RESEARCH_FROM_MS);
    }
    // Unadjusted candles: only the history after a symbol's last split-like jump is kept, as the bot does.
    for b in bars.values_mut() {
        let keys: Vec<i64> = b.keys().copied().collect();
        if let Some(cut) = keys
            .windows(2)
            .filter(|w| !(0.6..=1.8).contains(&(b[&w[1]].open / b[&w[0]].close)))
            .map(|w| w[1])
            .next_back()
        {
            b.retain(|&w, _| w >= cut);
        }
    }
    // No minimum-history floor (the bot drops symbols under ~20 sessions): the owner wants every SPY holding in the market.
    // Universe: SPY's holdings plus the reference, like the bot.
    let universe: BTreeSet<String> = load_holdings(holdings)
        .context("holdings file")?
        .into_iter()
        .chain(std::iter::once(REFERENCE.to_string()))
        .collect();
    let missing: Vec<String> = universe
        .iter()
        .filter(|s| bars.get(*s).is_none_or(|trades| trades.is_empty()))
        .cloned()
        .collect();
    for s in missing {
        bars.remove(&s);
        out.dropped.push((s, "no usable merged candles".into()));
    }
    for s in bars
        .keys()
        .filter(|s| !universe.contains(*s))
        .cloned()
        .collect::<Vec<_>>()
    {
        bars.remove(&s);
        out.dropped.push((s, "not a SPY holding".into()));
    }
    ensure!(
        bars.get(REFERENCE).is_some_and(|trades| !trades.is_empty()),
        "{REFERENCE} (the reference symbol) has no usable candles"
    );

    out.timings.push(("clean and filter", lap.elapsed()));
    lap = std::time::Instant::now();
    out.calendar = ours
        .prepare("SELECT open_ms, close_ms FROM sessions_calendar ORDER BY open_ms")?
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    out.data = bars
        .into_iter()
        .map(|(symbol, trades)| {
            let mut r = rules.get(&symbol).cloned().or_else(|| {
                Some(moomoo_bot::engine::rules::Rules {
                    qty_step: 1.0,
                    min_qty: 1.0,
                    min_notional: 1.0,
                    max_market_qty: 1e6,
                    commission: vec![(1.0, 0.0005), (1e6, 1e6 * 0.0005)],
                    commission_source: "standard_sim".into(),
                    max_leverage: 2.0,
                    book: vec![(10000.0, 0.0002, 0.0002)],
                    book_ts: 0,
                    adv_usd: None,
                    daily_vol: None,
                })
            });
            if let (Some(rules), Some((adv, vol))) = (r.as_mut(), liquidity_from_bars(&trades, 20))
            {
                rules.adv_usd = Some(adv);
                rules.daily_vol = Some(vol);
            }
            let listing_wall = *trades.keys().next().unwrap_or(&0);
            SymbolData {
                eligible: r.is_some() && symbol != REFERENCE,
                rules: r,
                listing_wall,
                trades,
                symbol,
            }
        })
        .collect();
    out.timings.push(("liquidity and assemble", lap.elapsed()));
    Ok(out)
}

/// Limits the learning data to the latest `window` sessions for EVERY symbol, whichever source the bars came from (thomas_core::config::WINDOW_SESSIONS).
/// Nothing is deleted: the full data stays in the store, and validation against the archive runs on the full `Loaded`. The real calendar is kept whole
/// so the trading clock still starts where the bot's does. Returns the window start (ms) or None when there is nothing to trim.
pub fn apply_window(l: &mut Loaded, window: usize) -> Option<i64> {
    let last = l
        .data
        .iter()
        .filter_map(|d| d.trades.keys().next_back().copied())
        .max()?;
    let start = thomas_core::config::window_start(&l.calendar, last, window)?;
    for d in l.data.iter_mut() {
        d.trades.retain(|w, _| *w >= start);
        d.listing_wall = d.trades.keys().next().copied().unwrap_or(0);
    }
    Some(start)
}

/// Session list from the market-wide calendar: bar times at which at least half of the symbols already listed at that time have a bar.
/// (Counting all symbols would drop every time before the latest listings, e.g. before the Yahoo-only symbols start.)
pub fn calendar_sessions(data: &[SymbolData]) -> Vec<(i64, i64)> {
    let mut count: BTreeMap<i64, usize> = BTreeMap::new();
    for d in data {
        for w in d.trades.keys() {
            *count.entry(*w).or_default() += 1;
        }
    }
    let mut firsts: Vec<i64> = data.iter().map(|d| d.listing_wall).collect();
    firsts.sort_unstable();
    let walls: Vec<i64> = count
        .into_iter()
        .filter(|(w, n)| *n * 2 >= firsts.partition_point(|f| f <= w))
        .map(|(w, _)| w)
        .collect();
    sessions_from_bars(&walls)
}

pub struct Built {
    pub market: Market,
    pub sessions: Sessions,
    pub sessions_count: usize,
    /// Symbols the engine's own validation rejected (incomplete candles).
    pub rejected: Vec<String>,
}

/// The 1-minute research market over the whole stored span, as the bot's `big_market` builds it.
pub fn build_1m(data: &[SymbolData]) -> Result<Built> {
    build(data, Interval::M1)
}

/// The research market for one candle interval (this system trades and learns on the real 1-minute candles, `search::ENGINE_INTERVAL`).
pub fn build(data: &[SymbolData], interval: Interval) -> Result<Built> {
    build_with_calendar(data, interval, Vec::new())
}

/// The default market on a loaded set: REAL 1-minute bars only (rule 15: every other interval is aggregated from 1-minute bars), on the
/// real trading calendar so the trading clock starts where the bot's does.
pub fn build_on(l: &Loaded, interval: Interval) -> Result<Built> {
    build_with_calendar(&l.data, interval, calendar_sessions(&l.data))
}

/// `calendar` = real (open, close) sessions since the research start; empty falls back to sessions derived from the bars.
/// The bot's rebalance timing runs on a trading clock that starts at the first session of its calendar (since 2026-01-02),
/// so a calendar that starts later shifts that timing and changes results.
pub fn build_with_calendar(
    data: &[SymbolData],
    interval: Interval,
    calendar: Vec<(i64, i64)>,
) -> Result<Built> {
    let list = if calendar.is_empty() {
        calendar_sessions(data)
    } else {
        calendar
    };
    let sessions_count = list.len();
    let s = Sessions::new(list)?.with_interval(interval);
    let first = data
        .iter()
        .map(|d| d.listing_wall)
        .max()
        .unwrap_or(0);
    let last = data
        .iter()
        .filter_map(|d| d.trades.keys().next_back().copied())
        .max()
        .unwrap_or(0)
        + BAR_MS;
    let mut walls = s.last_closed_bars(last, 100_000);
    walls.retain(|&w| w >= first);
    let m = build_market(&s, &walls, data)?;
    let rejected: Vec<String> = (0..m.symbols.len())
        .filter(|&k| {
            if let Err(e) = m.validate_symbol(k) {
                if k == 0 {
                    eprintln!("validate_symbol rejected {}: {e}", m.symbols[k]);
                }
                true
            } else {
                false
            }
        })
        .map(|k| m.symbols[k].clone())
        .collect();
    let keep: Vec<SymbolData> = data
        .iter()
        .filter(|d| !rejected.contains(&d.symbol))
        .map(|d| SymbolData {
            symbol: d.symbol.clone(),
            trades: d.trades.clone(),
            listing_wall: d.listing_wall,
            eligible: d.eligible,
            rules: d.rules.clone(),
        })
        .collect();
    let market = build_market(&s, &walls, &keep)?;
    market.validate()?;
    Ok(Built {
        market,
        sessions: s,
        sessions_count,
        rejected,
    })
}

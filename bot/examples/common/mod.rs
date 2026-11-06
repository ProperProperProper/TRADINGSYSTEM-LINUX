//! Shared by the research examples: the stored candles, moomoo's session calendar and one interval's
//! whole stored span as a market.
#![allow(dead_code)]
use anyhow::{Context, Result};
use moomoo_bot::engine::moomoo::{build_market, Gateway, Sessions, SymbolData};
use moomoo_bot::engine::rules::Rules;
use moomoo_bot::engine::{Bar, Interval, Market, BAR_MS, REFERENCE};
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, HashMap};

pub const CACHE: &str = "target/oos-cache";

/// First day of the research history. USER RULE (2026-10-09): research and testing use 2026 and later ONLY. `RESEARCH_FROM=YYYY-MM-DD`
/// may move the start later, never earlier than this floor. The live service keeps its own calendar from `CLOCK_EPOCH`.
pub const RESEARCH_FROM: (i32, u32, u32) = (2026, 1, 2);

/// The research history's first day: `RESEARCH_FROM`, or the later `RESEARCH_FROM` environment value (never before 2026-01-02).
pub fn research_from() -> Result<chrono::NaiveDate> {
    let floor = chrono::NaiveDate::from_ymd_opt(RESEARCH_FROM.0, RESEARCH_FROM.1, RESEARCH_FROM.2)
        .context("bad RESEARCH_FROM")?;
    match std::env::var("RESEARCH_FROM") {
        Ok(v) => Ok(chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d")
            .context("RESEARCH_FROM must be YYYY-MM-DD")?
            .max(floor)),
        Err(_) => Ok(floor),
    }
}

/// Opens the bot's permanent store (candles, rules, paper state) read-only for a research tool.
pub fn store() -> Result<Connection> {
    Ok(Connection::open_with_flags(
        moomoo_bot::platform::bot_store_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?)
}

/// moomoo's session calendar from the research start (cached per start date; calendar requests use no candle quota).
pub async fn sessions() -> Result<Sessions> {
    let from = research_from()?;
    let f = format!("{CACHE}/sessions-{from}.json");
    if let Ok(b) = std::fs::read(&f) {
        let list: Vec<(i64, i64)> = serde_json::from_slice(&b)?;
        return Sessions::new(list);
    }
    let gw = Gateway::connect("claude-moomoo-oos-study").await?;
    let s = gw.sessions_from(from).await?;
    std::fs::create_dir_all(CACHE)?;
    std::fs::write(&f, serde_json::to_vec(s.list())?)?;
    Ok(s)
}

/// Stored 15m candles and the latest stored rules of every symbol.
pub fn load_data() -> Result<Vec<SymbolData>> {
    let db = store()?;
    let mut rules: HashMap<String, Rules> = HashMap::new();
    let mut st = db.prepare("SELECT symbol, json FROM rules r WHERE ts_ms = (SELECT max(ts_ms) FROM rules x WHERE x.symbol = r.symbol)")?;
    for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (s, j) = row?;
        rules.insert(s, serde_json::from_str(&j)?);
    }
    let mut bars: BTreeMap<String, BTreeMap<i64, Bar>> = BTreeMap::new();
    // Longer history of the same symbols (examples/history_fetch.rs), unless OOS_STORE_ONLY is set.
    if std::env::var("OOS_STORE_ONLY").is_err() {
        if let Ok(b) = std::fs::read(format!("{CACHE}/history.json")) {
            let rows: Vec<(String, Vec<(i64, Bar)>)> = serde_json::from_slice(&b)?;
            for (s, v) in rows {
                bars.entry(s).or_default().extend(v);
            }
        }
    }
    let mut st = db.prepare("SELECT symbol, wall_ms, open, high, low, close, volume, turnover FROM candles WHERE kind = 'trades'")?;
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
    // User rule: nothing before the research start (2026 and later only), even when the cache holds older candles.
    let start_ms = research_from()?
        .and_hms_opt(0, 0, 0)
        .context("start")?
        .and_utc()
        .timestamp_millis();
    for b in bars.values_mut() {
        b.retain(|&w, _| w >= start_ms);
    }
    // Unadjusted candles: a split shows as a jump between consecutive candles (below 0.6x or above 1.8x; smaller gaps are kept
    // as real earnings moves). Only the history AFTER a symbol's last such jump is kept (prices are never adjusted or invented);
    // the symbol then joins the market from that point, like a new listing.
    for (s, b) in bars.iter_mut() {
        let keys: Vec<i64> = b.keys().copied().collect();
        let last_jump = keys
            .windows(2)
            .filter(|w| !(0.6..=1.8).contains(&(b[&w[1]].open / b[&w[0]].close)))
            .map(|w| w[1])
            .next_back();
        if let Some(cut) = last_jump {
            let before = b.len();
            b.retain(|&w, _| w >= cut);
            eprintln!("  {s}: candle jump (split?) at {}: history before it dropped ({} of {before} candles kept)", fmt(cut), b.len());
        }
    }
    bars.retain(|_, b| b.len() > 26 * 20); // at least ~20 sessions left
                                           // The universe is SPY's top holdings by weight when the holdings file is present (user 2026-10-08: the top 100 symbols SPY covers).
    if let Some(h) =
        moomoo_bot::engine::moomoo::load_holdings(std::path::Path::new("data/spy_holdings.csv"))
    {
        let keep: std::collections::BTreeSet<String> = h
            .into_iter()
            .take(moomoo_bot::engine::moomoo::SPY_POOL)
            .chain(std::iter::once(REFERENCE.to_string()))
            .collect();
        let missing: Vec<&String> = keep.iter().filter(|s| !bars.contains_key(*s)).collect();
        bars.retain(|s, _| keep.contains(s));
        eprintln!("  universe: SPY holdings ({}) + {REFERENCE}: {} have candle history here, {} not yet pulled (quota): {:?}", keep.len() - 1, bars.len(), missing.len(), missing.iter().take(12).collect::<Vec<_>>());
    }
    Ok(bars
        .into_iter()
        .map(|(symbol, trades)| {
            let mut r = rules.get(&symbol).cloned();
            // real liquidity of the symbol from its candles (used when the liquidity model is switched on)
            if let (Some(rules), Some((adv, vol))) = (
                r.as_mut(),
                moomoo_bot::engine::moomoo::liquidity_from_bars(&trades, 20),
            ) {
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
        .collect())
}

/// One interval's whole stored span as a market (symbols with incomplete candles dropped).
pub struct Big {
    pub interval: Interval,
    pub m: Market,
    /// Indices of candles that open a session.
    pub opens: Vec<usize>,
    /// Wall close of every candle.
    pub closes: Vec<i64>,
}

/// Builds the research market for `interval` from all stored history (from SPY's first candle), dropping symbols whose data fails validation.
pub fn big_market(cal: &Sessions, data: &[SymbolData], interval: Interval) -> Result<Big> {
    let s = cal.with_interval(interval);
    let first = data
        .iter()
        .find(|d| d.symbol == REFERENCE)
        .context("SPY missing")?
        .listing_wall;
    let last = data
        .iter()
        .filter_map(|d| d.trades.keys().next_back().copied())
        .max()
        .unwrap_or(0)
        + BAR_MS;
    let mut walls = s.last_closed_bars(last, 100_000);
    walls.retain(|&w| w >= first);
    let m = build_market(&s, &walls, data)?;
    let bad: Vec<String> = (0..m.symbols.len())
        .filter(|&k| m.validate_symbol(k).is_err())
        .map(|k| m.symbols[k].clone())
        .collect();
    let keep: Vec<SymbolData> = data
        .iter()
        .filter(|d| !bad.contains(&d.symbol))
        .map(|d| SymbolData {
            symbol: d.symbol.clone(),
            trades: d.trades.clone(),
            listing_wall: d.listing_wall,
            eligible: d.eligible,
            rules: d.rules.clone(),
        })
        .collect();
    let m = build_market(&s, &walls, &keep)?;
    m.validate()?;
    let opens: Vec<usize> = (0..m.wall.len())
        .filter(|&t| s.open_session(m.wall[t]).is_some_and(|x| x.0 == m.wall[t]))
        .collect();
    eprintln!(
        "{}: {} candles, {} sessions ({} .. {}), {} symbols, dropped for incomplete candles: {:?}",
        interval.label(),
        m.ts.len(),
        opens.len(),
        fmt(m.wall[0]),
        fmt(m.wall[m.wall.len() - 1]),
        m.symbols.len(),
        bad
    );
    let closes: Vec<i64> = m
        .wall
        .iter()
        .map(|&w| s.close_of(w).unwrap_or(w + BAR_MS))
        .collect();
    Ok(Big {
        interval,
        m,
        opens,
        closes,
    })
}

/// Formats a wall time (ms) as US/Eastern date and time.
pub fn fmt(ms: i64) -> String {
    moomoo_bot::engine::moomoo::fmt_et(ms)
}

/// The research tools model what the bot will do: the real-volume liquidity model is ON unless STUDY_LIQUIDITY=0 (the old one-moment top-of-book
/// snapshot capped every order at a few thousand USD). Call once at the start of an example's main.
pub fn liquidity_default() -> bool {
    std::env::var("STUDY_LIQUIDITY").map_or(true, |v| v != "0")
}

/// `STUDY_ORDER_CAP=<usd>`: ceiling on one entry order's notional (research only). Also lifts the live service's CPU governor for
/// this research process, so its threads can use every core.
pub fn apply_order_cap() {
    moomoo_bot::engine::governor::UNTHROTTLED.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(c) = std::env::var("STUDY_ORDER_CAP")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|c| *c > 0.0)
    {
        moomoo_bot::engine::rules::ORDER_CAP_USD
            .store(c.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// Current wall time in milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Execution/price noise for stress tests: every bar's open and close are multiplied by exp(sigma * z) (independent,
/// no drift), highs and lows widened to still cover them. Traded and mark candles get the same factors. Deterministic in `seed`.
pub fn jitter(m: &Market, sigma: f64, seed: u64) -> Market {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut normal = move || {
        let (u1, u2) = (next().max(1e-12), next());
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    };
    let mut out = m.clone();
    for s in 0..out.bars.len() {
        for t in 0..out.bars[s].len() {
            let (fo, fc) = ((sigma * normal()).exp(), (sigma * normal()).exp());
            for rows in [&mut out.bars, &mut out.marks] {
                if let Some(b) = rows[s][t].as_mut() {
                    b.open *= fo;
                    b.close *= fc;
                    b.high = b.high.max(b.open).max(b.close);
                    b.low = b.low.min(b.open).min(b.close);
                }
            }
        }
    }
    out
}

/// The USD the bot trades with, as the live service sizes it: the paper account's latest AVAILABLE cash in the store (the
/// research tools must answer for the same size the bot really trades, since market impact grows with order size). Falls
/// back to START_CAPITAL when the store has no balance. Override with STUDY_EQUITY.
pub fn capital() -> f64 {
    if let Some(x) = std::env::var("STUDY_EQUITY")
        .ok()
        .and_then(|x| x.parse::<f64>().ok())
    {
        return x;
    }
    let from_store = (|| -> Option<f64> {
        let c = store().ok()?;
        c.query_row(
            "SELECT cash FROM accounts ORDER BY ts_ms DESC LIMIT 1",
            [],
            |r| r.get::<_, f64>(0),
        )
        .ok()
    })();
    match from_store {
        // the same rule as the service's `capital_from`: a START_CAPITAL account, never more than the cash there
        Some(c) if c.is_finite() && c.floor() >= 1000.0 => {
            c.floor().min(moomoo_bot::engine::START_CAPITAL)
        }
        _ => moomoo_bot::engine::START_CAPITAL,
    }
}

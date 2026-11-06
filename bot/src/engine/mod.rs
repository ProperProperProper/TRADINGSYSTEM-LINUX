//! The trading engine for US stocks on moomoo, on closed 1-minute regular-session bars (aggregated to
//! supported live intervals aggregated from 1m bars): a cross-sectional mean-reversion engine on a trading-time clock (see `Market::ts`)
//! with an SPY trend filter (`REFERENCE`). Data and execution come from moomoo OpenD only.
//!
//! * `moomoo` — real moomoo data through OpenD: trading sessions, closed traded 1m candles,
//!   the screener universe, quotes, the paper account, real paper orders and their fees.
//! * `rules` — per-symbol rules: lot size, real paper fees, quoted spread cost.
//! * `scores` — the screener, computed causally from closed bars.
//! * `xs` — the strategy: cross-sectional ranking, long only, settings from
//!   `walkforward::search_live` (a random search), SPY trend filter and drawdown rules.
//! * `walkforward` — the full-window random parameter search and the champion/challenger rule.
//! * `metrics` — trade records and performance metrics.
//! * `governor`, `supervisor` — CPU cap, task restarts.
//!
//! Decisions are made at a bar's close and filled at a later open (or at a level inside a
//! later bar, adverse first): no lookahead, no repainting; the same engine drives backtests,
//! the parameter search and paper trading.

/// Git commit of this build (see build.rs), stored with every recorded result.
pub const ENGINE_COMMIT: &str = env!("BOT_GIT_COMMIT");

/// What the simulation charges and assumes. Bump when any of this changes.
pub const COST_MODEL_VERSION: &str = "moomoo-v1 2026-10-06: commission from this account's real moomoo paper order fees (excluded, and labelled so, until a real fee exists), half the quoted bid/ask spread each way (moomoo snapshot), 1x (no leverage, no liquidation), no funding, decisions fill two trading bars after the deciding close, take-profit exits at 70% of the decided value, long only; marks are traded candles (moomoo has no midpoint candles); paper fills are real moomoo paper fills";

pub mod btarchive;
pub mod execution;
pub mod governor;
pub mod live_options;
pub mod metrics;
pub mod moomoo;
pub mod record;
pub mod rules;
pub mod scores;
pub mod store;
pub mod supervisor;
pub mod tensor;
pub mod walkforward;
pub mod xs;

/// Runtime data folder (bars, rules, paper account, logs). It lives outside ~/Documents:
/// macOS privacy protection blocks launchd jobs there.
pub fn runtime_dir() -> std::path::PathBuf {
    let dir = crate::platform::bot_state_dir();
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Locked: 1-minute bars.
pub const BAR_MS: i64 = 60 * 1000;
/// Regular-session bars in a full US trading day (09:30-16:00 ET). Day-long windows
/// are one trading day of this many bars (user decision 2026-10-05:
/// trading-time clock).
pub const DAY_BARS: usize = 390;
// NOTE(agents): User rule (2026-10-06, final): the search and every test use the last 14 TRADING
//               SESSIONS (not calendar days), nothing older. (Tried earlier the same day: 10 sessions,
//               14 sessions for every interval.)
/// Every test uses the last 14 full trading sessions.
pub const TEST_SESSIONS: usize = 14;
pub const BARS: usize = TEST_SESSIONS * DAY_BARS; // 364

// NOTE(agents): User request (2026-10-06): test the strategy on 1m, 1h, 4h and one-session
//               candles; paper trades the interval whose winner did best ("aggregate is good").
//               Longer candles are AGGREGATED from the real closed 1m bars, aligned to each
//               session's open (1h: 6 x 1h + a 30 min close; 4h: 4h + 2.5h; 1 session: the whole
//               day). The engine runs unchanged on them: every bar-count rule (holds, windows,
//               warm-up) counts candles of that interval. 1m tests on the last 14 sessions;
//               every interval on the last 14 sessions, and only intervals that need it get older bars,
//               strictly to warm their indicators up (user decision): no trade, score or
//               calibration uses a bar before their test window.
/// Sessions of the 1h, 4h and one-session test windows (user rule: larger time frames test 21 days).
pub const LONG_TEST_SESSIONS: usize = TEST_SESSIONS;

/// A candle interval the strategy is tested and paper-traded on.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum Interval {
    #[default]
    M1,
    /// 30-minute, 45-minute and 90-minute candles.
    M30,
    M45,
    H1,
    M90,
    /// 2-hour candles.
    H2,
    /// 3-hour candles aggregated from the real 1m bars (09:30-12:30, 12:30-15:30, 15:30-16:00).
    H3,
    H4,
    /// 5-hour candles aggregated from the real 1m bars (09:30-14:30, 14:30-16:00).
    H5,
    /// 2.5-hour and 6-hour candles.
    M150,
    H6,
    D1,
}

impl Interval {
    /// Every interval the research tools can test.
    pub const ALL: [Interval; 12] = [
        Interval::M1,
        Interval::M30,
        Interval::M45,
        Interval::H1,
        Interval::M90,
        Interval::H2,
        Interval::M150,
        Interval::H3,
        Interval::H4,
        Interval::H5,
        Interval::H6,
        Interval::D1,
    ];

    // The live service searches and paper-trades every interval from the same closed 1m source bars.
    // The winner is selected only after each interval's search result is available; no candle data
    // is fetched separately for aggregated intervals.
    pub const LIVE: [Interval; 12] = Self::ALL;

    /// 1-minute bars per candle (a session's last candle may be shorter).
    pub const fn bars(self) -> usize {
        match self {
            Interval::M1 => 1,
            Interval::M30 => 2,
            Interval::M45 => 3,
            Interval::H1 => 4,
            Interval::M90 => 6,
            Interval::M150 => 10,
            Interval::H6 => 24,
            Interval::H2 => 8,
            Interval::H3 => 12,
            Interval::H4 => 16,
            Interval::H5 => 20,
            Interval::D1 => DAY_BARS,
        }
    }

    /// Human label of the interval (e.g. "5h", "1 session").
    pub const fn label(self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M30 => "30m",
            Interval::M45 => "45m",
            Interval::H1 => "1h",
            Interval::M90 => "90m",
            Interval::H2 => "2h",
            Interval::M150 => "2.5h",
            Interval::H3 => "3h",
            Interval::H4 => "4h",
            Interval::H5 => "5h",
            Interval::H6 => "6h",
            Interval::D1 => "1 session",
        }
    }

    /// Short key (file names, API query `?i=`).
    pub const fn key(self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M30 => "30m",
            Interval::M45 => "45m",
            Interval::H1 => "1h",
            Interval::M90 => "90m",
            Interval::H2 => "2h",
            Interval::M150 => "150m",
            Interval::H3 => "3h",
            Interval::H4 => "4h",
            Interval::H5 => "5h",
            Interval::H6 => "6h",
            Interval::D1 => "1d",
        }
    }

    /// The interval whose key (e.g. "5h") is `k`.
    pub fn from_key(k: &str) -> Option<Interval> {
        Interval::ALL.into_iter().find(|i| i.key() == k)
    }

    /// Sessions of the test window.
    pub const fn test_sessions(self) -> usize {
        match self {
            Interval::M1 => LONG_TEST_SESSIONS,
            _ => LONG_TEST_SESSIONS,
        }
    }

    /// Candles in a full regular session.
    pub const fn per_session(self) -> usize {
        DAY_BARS.div_ceil(self.bars())
    }

    /// Candles in the test window (1m: `BARS`).
    pub const fn test_candles(self) -> usize {
        self.test_sessions() * self.per_session()
    }

    /// Candles before the test window that only warm the indicators up (none for 1m, whose
    /// warm-up stays inside the window as before).
    pub const fn warmup_candles(self) -> usize {
        match self {
            Interval::M1 => scores::WARMUP,
            _ => scores::WARMUP,
        }
    }
}

/// A window of `crypto_bars` 1-minute bars of 24/7 time, in trading-time bars (96 = one trading day).
pub const fn day_scaled(crypto_bars: usize) -> usize {
    let b = (crypto_bars * DAY_BARS + 48) / 96;
    if b == 0 {
        1
    } else {
        b
    }
}

/// The USD capital the bot trades, backtests and learns with: ONE source for the live paper account, the searches, the research
/// tools and the lab (user 2026-10-09: a 5000 USD account, replacing 10000). The moomoo paper balance itself is never a deposit here, and it is
/// the CEILING of what the bot trades: a bigger balance is not used.
pub const START_CAPITAL: f64 = 5_000.0;

// NOTE(agents): User request (2026-10-08): the base trend symbol is SPY (was QQQ until the universe became
//               SPY's own top holdings).
/// The trend filter's reference market (also the buy-and-hold benchmark the research tools print).
pub const REFERENCE: &str = "SPY";

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Base-asset volume.
    pub volume: f64,
    /// Quote (USDT) turnover.
    pub turnover: f64,
}

/// All symbols on one common timeline of closed bars. `bars[s][t]` is None when
/// symbol `s` has no bar at `ts[t]` (not listed yet, or a gap).
#[derive(Debug, Clone, Default)]
pub struct Market {
    // NOTE(agents): `ts` is the TRADING CLOCK, not wall time: it advances one BAR_MS per
    //               regular-session bar and skips nights, weekends and holidays, so every engine
    //               timing rule (rebalance alignment, fill delay, cooldown) runs in trading time.
    //               `wall[t]` is the real open time of the same bar; use it for display, sessions
    //               and orders, never for engine timing.
    /// Trading-clock open time of each bar (ms), contiguous BAR_MS steps.
    pub ts: Vec<i64>,
    /// Real (UTC epoch ms) open time of each bar.
    pub wall: Vec<i64>,
    pub symbols: Vec<String>,
    /// First available bar (trading clock, ms). None means unverified listing history.
    pub listing_times: Vec<Option<i64>>,
    /// Held symbols outside the current top 20 remain managed but cannot re-enter.
    pub entry_eligible: Vec<bool>,
    pub bars: Vec<Vec<Option<Bar>>>,
    // NOTE(agents): Stops and valuation use these MARK candles. moomoo publishes no bid/ask MIDPOINT
    //               bars, so they are the real TRADED candles
    //               (see `moomoo::build_market`). Never fill a missing mark candle from anything else.
    /// Mark OHLC (here: the traded candles). Never invent a missing one.
    pub marks: Vec<Vec<Option<Bar>>>,
    /// Funding settlements (timestamp ms, rate). Always empty for stocks; kept so the engine's
    /// accounting is unchanged.
    pub funding: Vec<Vec<(i64, f64)>>,
    /// Trading rules per symbol (`rules::Rules`: size increment, measured commission and
    /// spread cost). None = unknown: not traded.
    pub rules: Vec<Option<rules::Rules>>,
}

impl Market {
    /// Reject incomplete or malformed real data before evaluating a strategy.
    pub fn validate(&self) -> anyhow::Result<()> {
        let n = self.symbols.len();
        let bars = self.ts.len();
        anyhow::ensure!(
            bars > 0
                && self.ts.windows(2).all(|w| w[1] - w[0] == BAR_MS)
                && self.wall.len() == bars
                && self.wall.windows(2).all(|w| w[1] > w[0]),
            "invalid market timeline"
        );
        anyhow::ensure!(
            self.bars.len() == n
                && self.marks.len() == n
                && self.funding.len() == n
                && self.rules.len() == n
                && self.listing_times.len() == n,
            "market shape mismatch or missing listing metadata"
        );
        for s in 0..n {
            self.validate_symbol(s)?;
        }
        Ok(())
    }

    // NOTE(agents): Per-symbol on purpose: the service lets an incomplete coin sit out a bar
    //               instead of failing the whole market.
    /// One symbol's rows: complete traded and mark candles after listing, sane
    /// OHLC, ascending finite funding.
    pub fn validate_symbol(&self, s: usize) -> anyhow::Result<()> {
        let bars = self.ts.len();
        anyhow::ensure!(
            self.bars[s].len() == bars && self.marks[s].len() == bars,
            "market row shape mismatch: {}",
            self.symbols[s]
        );
        let launch = self.listing_times[s]
            .ok_or_else(|| anyhow::anyhow!("{}: unverified listing time", self.symbols[s]))?;
        let eligible = ((launch + BAR_MS - 1) / BAR_MS) * BAR_MS;
        for t in 0..bars {
            if self.ts[t] < eligible {
                continue;
            }
            for (name, bar) in [("traded", self.bars[s][t]), ("mark", self.marks[s][t])] {
                let b = bar.ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}: missing {name} candle at {}",
                        self.symbols[s],
                        self.ts[t]
                    )
                })?;
                anyhow::ensure!(
                    [b.open, b.high, b.low, b.close, b.volume, b.turnover]
                        .iter()
                        .all(|v| v.is_finite())
                        && b.open > 0.0
                        && b.low > 0.0
                        && b.high >= b.open.max(b.close).max(b.low)
                        && b.low <= b.open.min(b.close)
                        && b.volume >= 0.0
                        && b.turnover >= 0.0,
                    "{}: invalid {name} candle at {}",
                    self.symbols[s],
                    self.ts[t]
                );
            }
        }
        anyhow::ensure!(
            self.funding[s].windows(2).all(|w| w[0].0 < w[1].0)
                && self.funding[s].iter().all(|(_, v)| v.is_finite()),
            "{}: malformed funding history",
            self.symbols[s]
        );
        Ok(())
    }

    /// The symbol's rules; None means the symbol must not be traded.
    pub fn rules(&self, sym: usize) -> Option<&rules::Rules> {
        self.rules.get(sym).and_then(|r| r.as_ref())
    }

    /// The bars in `r` (all symbols, rules and listing data kept).
    pub fn slice(&self, r: std::ops::Range<usize>) -> Market {
        Market {
            ts: self.ts[r.clone()].to_vec(),
            wall: self.wall[r.clone()].to_vec(),
            symbols: self.symbols.clone(),
            listing_times: self.listing_times.clone(),
            entry_eligible: self.entry_eligible.clone(),
            bars: self.bars.iter().map(|b| b[r.clone()].to_vec()).collect(),
            marks: self.marks.iter().map(|b| b[r.clone()].to_vec()).collect(),
            funding: self.funding.clone(),
            rules: self.rules.clone(),
        }
    }

    /// Join back-to-back markets into one continuous timeline (union of symbols;
    /// a symbol missing from one part has no bars there). Indicators computed on
    /// the result carry their history across the joins, as they do live.
    pub fn concat(parts: &[Market]) -> anyhow::Result<Market> {
        for w in parts.windows(2) {
            let (a, b) = (w[0].ts.last(), w[1].ts.first());
            anyhow::ensure!(
                matches!((a, b), (Some(a), Some(b)) if b - a == BAR_MS),
                "markets are not back-to-back"
            );
        }
        let mut symbols: Vec<String> = parts
            .iter()
            .flat_map(|m| m.symbols.iter().cloned())
            .collect();
        symbols.sort();
        symbols.dedup();
        let mut out = Market {
            ts: parts.iter().flat_map(|m| m.ts.iter().copied()).collect(),
            wall: parts.iter().flat_map(|m| m.wall.iter().copied()).collect(),
            symbols,
            ..Default::default()
        };
        for s in &out.symbols {
            let (mut bars, mut marks, mut funding) = (
                Vec::with_capacity(out.ts.len()),
                Vec::with_capacity(out.ts.len()),
                Vec::new(),
            );
            for m in parts {
                match m.symbols.iter().position(|x| x == s) {
                    Some(i) => {
                        bars.extend_from_slice(&m.bars[i]);
                        marks.extend_from_slice(&m.marks[i]);
                        for &f in &m.funding[i] {
                            if funding.last().is_none_or(|l: &(i64, f64)| f.0 > l.0) {
                                funding.push(f);
                            }
                        }
                    }
                    None => {
                        bars.extend(std::iter::repeat_n(None, m.ts.len()));
                        marks.extend(std::iter::repeat_n(None, m.ts.len()));
                    }
                }
            }
            let launches: Vec<i64> = parts
                .iter()
                .filter_map(|m| {
                    let i = m.symbols.iter().position(|x| x == s)?;
                    m.listing_times.get(i).copied().flatten()
                })
                .collect();
            anyhow::ensure!(
                launches.iter().all(|t| Some(t) == launches.first()),
                "conflicting listing times for {s}"
            );
            out.entry_eligible.push(parts.iter().any(|m| {
                m.symbols
                    .iter()
                    .position(|x| x == s)
                    .is_some_and(|i| m.entry_eligible.get(i).copied().unwrap_or(true))
            }));
            out.listing_times.push(launches.first().copied());
            out.bars.push(bars);
            out.marks.push(marks);
            out.funding.push(funding);
            // The most recent part's rules (they are measured, newest is best).
            out.rules.push(parts.iter().rev().find_map(|m| {
                let i = m.symbols.iter().position(|x| x == s)?;
                m.rules.get(i).cloned().flatten()
            }));
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Side {
    Long,
    Short,
}

impl Side {
    /// +1 for long, -1 for short.
    pub fn sign(self) -> f64 {
        match self {
            Side::Long => 1.0,
            Side::Short => -1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Joined windows behave exactly like one continuous series: scores at the
    /// join carry their history instead of restarting a warm-up.
    #[test]
    fn concat_is_continuous() {
        let series: Vec<Vec<f64>> = (0..4)
            .map(|k| {
                (0..500)
                    .map(|i| 100.0 + ((i as f64) * (0.04 + k as f64 * 0.01)).sin() * 5.0)
                    .collect()
            })
            .collect();
        let full = scores::tests::market(&series);
        let joined = Market::concat(&[full.slice(0..200), full.slice(200..500)]).unwrap();
        assert_eq!(joined.ts, full.ts);
        assert_eq!(scores::compute(&joined, 100), scores::compute(&full, 100));
        assert!(
            Market::concat(&[full.slice(0..200), full.slice(201..500)]).is_err(),
            "gaps are rejected"
        );
    }
}

//! Replays archived backtest runs through the bot's real engine (`engine::xs::backtest`) so ThomasAi can check that its data
//! pipeline reproduces what the bot stored. An archived search run is the latest-14-session window of the market as it stood then.
use anyhow::{Context, Result};
use moomoo_bot::engine::xs::{XsParams, backtest};
use moomoo_bot::engine::{scores, walkforward};

use crate::market::Built;

/// Engine scores for a market (computed once, reused by every replay).
pub fn compute_scores(b: &Built) -> Vec<Vec<Option<scores::Score>>> {
    scores::compute(&b.market, walkforward::UNIVERSE)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Replayed {
    pub return_pct: f64,
    pub max_drawdown_pct: f64,
    pub trades: usize,
    pub wins: usize,
    pub profit_factor: f64,
}

/// Index of the candle opening at wall time `ts`. (Wall first: a trading-clock value can coincide with a different candle's wall time.)
pub fn index_of(b: &Built, ts: i64) -> Option<usize> {
    b.market.wall.iter().position(|&w| w == ts)
}

/// Runs archived `params_json` over the candles from `first_ts` up to and including the last candle that opens at or before `last_ts`.
pub fn replay(
    b: &Built,
    sc: &[Vec<Option<scores::Score>>],
    params_json: &str,
    first_ts: i64,
    last_ts: i64,
    equity: f64,
) -> Result<Replayed> {
    let p: XsParams = serde_json::from_str(params_json)
        .context("archived params do not parse with the current engine")?;
    let start = index_of(b, first_ts).context("window start is not a candle of this market")?;
    let end = b
        .market
        .wall
        .iter()
        .rposition(|&w| w <= last_ts)
        .context("window end")?
        + 1;
    anyhow::ensure!(
        end > start + 1,
        "window of {} candles is too short",
        end.saturating_sub(start)
    );
    let m = backtest(&b.market, sc, start..end, &p, equity).metrics();
    Ok(Replayed {
        return_pct: m.return_pct(),
        max_drawdown_pct: m.max_drawdown_pct,
        trades: m.trades,
        wins: m.wins,
        profit_factor: m.profit_factor(),
    })
}

/// Switches the engine's liquidity model (archived runs used "liq-sqrt": order size capped by average daily volume, square-root impact).
pub fn use_liquidity_model(on: bool) {
    moomoo_bot::engine::rules::LIQUIDITY_MODEL.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Research only: score a symbol after `sessions` trading sessions of history (`per_session` candles each) instead of the bot's default
/// 69 candles; 0 sessions restores the default. The bot clamps it to the smallest history the features can index.
pub fn use_warmup_sessions(sessions: usize, per_session: usize) {
    scores::set_warmup(sessions * per_session);
}

/// Applies ThomasAi's research settings from `thomas_core::config` for the 5-hour interval (2 candles per session): what the archive-parity and search tests, which
/// replay the bot's archived 5-hour runs, build on. The live commands set the warm-up from their own interval in `search::prepare_for`.
pub fn apply_research_settings() {
    use_warmup_sessions(thomas_core::config::WARMUP_SESSIONS, 2);
}

//! Closed-trade records and performance metrics shared by every engine (costs
//! come from each symbol's moomoo rules, `rules::Rules`).

use super::Side;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub symbol: String,
    pub side: Side,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub entry: f64,
    pub exit: f64,
    pub qty: f64,
    pub leverage: f64,
    pub fees: f64,
    pub funding: f64,
    pub pnl: f64,
    /// pnl / margin posted for the position.
    pub r: f64,
    pub reason: String,
    /// Open time (ms) of the bar whose close DECIDED this entry (a wick-guard delay does not move it).
    #[serde(default)]
    pub decided_ts: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Metrics {
    #[serde(default)]
    pub execution_error: Option<String>,
    #[serde(default)]
    pub rejected_rebalances: usize,
    pub start_equity: f64,
    pub end_equity: f64,
    pub open_unrealized: f64,
    pub trades: usize,
    pub wins: usize,
    pub gross_profit: f64,
    pub gross_loss: f64,
    pub liquidations: usize,
    pub max_drawdown_pct: f64,
}

impl Metrics {
    /// Net result in USD: end equity plus open unrealized PnL minus start equity.
    pub fn net(&self) -> f64 {
        self.end_equity + self.open_unrealized - self.start_equity
    }
    /// Net result as % of the start equity (0 with no start equity).
    pub fn return_pct(&self) -> f64 {
        if self.start_equity > 0.0 {
            self.net() / self.start_equity * 100.0
        } else {
            0.0
        }
    }
    /// Gross profit over gross loss (infinite with no loss, 0 with no trades).
    pub fn profit_factor(&self) -> f64 {
        if self.gross_loss > 0.0 {
            self.gross_profit / self.gross_loss
        } else if self.gross_profit > 0.0 {
            f64::INFINITY
        } else {
            0.0
        }
    }
}

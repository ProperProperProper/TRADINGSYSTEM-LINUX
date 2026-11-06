//! Real moomoo paper orders for the engine's decisions, and reconciliation of the engine's
//! paper account with what moomoo reports.
//!
//! The engine (`xs::XsPortfolio`) decides exactly as in the source and simulates every fill at a
//! candle open, which it can only do after that candle has CLOSED. Live, the order has to go out
//! AT the open. So, at each open of the paper interval:
//!
//! 1. `preview`: a copy of the paper portfolio steps one candle whose prices are the current real
//!    quote mid (open = high = low = close). What that copy executes at the open (exits, entries,
//!    adds) is exactly what the engine's own code decides at this open given today's price; those
//!    become MARKET orders (sells first). The copy is discarded; nothing of it is recorded.
//! 2. When the candle has closed, the engine steps it with the real candle (as in the source).
//!    `Ledger::link_step` ties the engine's fills to the orders, and `Ledger::recompute`
//!    overwrites every simulated fill with moomoo's real fill quantity, average price and fee
//!    (equity adjusted by the difference). A fill the engine simulated without a real order is
//!    removed (and logged); a real fill the engine did not make is sold at the next open and its
//!    real result booked as a reconciliation trade; an engine exit that has no real sell yet
//!    (an intrabar stop, a failed order) is sold at the next open.
//!
//! Nothing is invented: until moomoo reports a fill or fee, the engine's simulated value stays
//! (marked pending); an order that never fills contributes nothing.

use super::metrics::Trade;
use super::moomoo::{order_final, OrderFee, OrderState};
use super::scores::Score;
use super::xs::{XsParams, XsPortfolio, XsPosition};
use super::{Bar, Market, Side, BAR_MS};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One execution of an engine holding (simulated by the engine): quantity, price, fee.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Leg {
    pub qty: f64,
    pub price: f64,
    pub fee: f64,
}

/// Which execution of its holding an order implements.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LegKind {
    /// The k-th buy (0 = the entry, 1 = an add).
    Buy(usize),
    Sell,
}

/// A real paper order and what moomoo reports about it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveOrder {
    /// Local id (unique within the ledger).
    pub id: u64,
    /// The holding it belongs to (0 until linked).
    pub holding: u64,
    pub leg: LegKind,
    pub symbol: String,
    /// 1 buy, 2 sell.
    pub side: i32,
    pub qty: f64,
    /// Engine clock time of the candle open this order implements.
    pub due_ts: i64,
    /// Wall time it was placed.
    pub placed_ms: i64,
    pub order_id: Option<u64>,
    pub order_id_ex: Option<String>,
    /// Placement refused (by moomoo or the guard): the order does not exist.
    pub error: Option<String>,
    pub state: Option<OrderState>,
    pub fee: Option<OrderFee>,
    /// moomoo has not reported a fee for this filled order after repeated requests: the fee is
    /// EXCLUDED (zero), never invented.
    #[serde(default)]
    pub fee_unavailable: bool,
    #[serde(default)]
    pub fee_requests: u32,
    /// Why it was placed (log text).
    pub reason: String,
}

impl LiveOrder {
    /// No further change can happen (refused, or a final moomoo state).
    pub fn is_final(&self) -> bool {
        self.error.is_some() || self.state.as_ref().is_some_and(|s| order_final(s.status))
    }
    /// Quantity filled so far (0 before any state).
    pub fn filled(&self) -> f64 {
        self.state.as_ref().map_or(0.0, |s| s.fill_qty)
    }
    /// (filled qty, filled value).
    pub fn fill(&self) -> (f64, f64) {
        match &self.state {
            Some(s) if s.fill_qty > 0.0 => {
                (s.fill_qty, s.fill_qty * s.fill_avg_price.unwrap_or(0.0))
            }
            _ => (0.0, 0.0),
        }
    }
    /// The real fee once known (Some(0) when moomoo reports none: excluded).
    pub fn real_fee(&self) -> Option<f64> {
        if self.filled() <= 0.0 && self.is_final() {
            return Some(0.0);
        }
        self.fee
            .as_ref()
            .map(|f| f.total)
            .or(self.fee_unavailable.then_some(0.0))
    }
}

/// One engine position from entry to exit, with the engine's simulated executions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holding {
    pub id: u64,
    pub symbol: String,
    /// Engine clock time of the entry (the engine position's `entry_ts`).
    pub entry_ts: i64,
    /// Index of the engine trade once the engine closed it.
    pub trade_idx: Option<usize>,
    pub sim_buys: Vec<Leg>,
    pub sim_sell: Option<Leg>,
    /// Shares moomoo filled that the engine never held (sold at the next open).
    pub residual: bool,
    /// Every order final, every fee known, and booked: nothing changes any more.
    pub settled: bool,
    /// Clock time of the residual sale (residual holdings only).
    #[serde(default)]
    pub exit_ts: Option<i64>,
}

/// An order to place now.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    pub symbol: String,
    pub side: i32,
    pub qty: f64,
    /// The holding (0 for a new entry: linked when the engine fills it).
    pub holding: u64,
    pub leg: LegKind,
    pub due_ts: i64,
    pub reason: String,
}

/// Every real order and engine holding (part of the persisted paper state).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ledger {
    pub holdings: Vec<Holding>,
    pub orders: Vec<LiveOrder>,
    #[serde(default)]
    next_id: u64,
    /// Clock time of the last candle open whose orders were planned (never twice).
    #[serde(default)]
    pub planned_for: Option<i64>,
}

/// Whether two floats are equal within a relative 1e-9.
fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

/// The market plus one candle at clock `next_ts` (wall `next_wall`) priced at each symbol's real
/// quote mid (None without a quote), and scores extended with None. For the preview only.
fn preview_market(
    m: &Market,
    sc: &[Vec<Option<Score>>],
    next_ts: i64,
    next_wall: i64,
    mids: &HashMap<String, f64>,
) -> Result<(Market, Vec<Vec<Option<Score>>>)> {
    ensure!(
        m.ts.last().is_some_and(|&l| next_ts == l + BAR_MS),
        "preview candle {next_ts} does not follow the market"
    );
    let mut m2 = m.clone();
    m2.ts.push(next_ts);
    m2.wall.push(next_wall);
    for (s, sym) in m.symbols.iter().enumerate() {
        let bar = mids
            .get(sym)
            .filter(|p| p.is_finite() && **p > 0.0)
            .map(|&p| Bar {
                open: p,
                high: p,
                low: p,
                close: p,
                volume: 0.0,
                turnover: 0.0,
            });
        m2.bars[s].push(bar);
        m2.marks[s].push(bar);
    }
    let mut sc2 = sc.to_vec();
    for row in &mut sc2 {
        row.push(None);
    }
    Ok((m2, sc2))
}

impl Ledger {
    /// Next holding id.
    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// The holding with id `id`.
    pub fn holding(&self, id: u64) -> Option<&Holding> {
        self.holdings.iter().find(|h| h.id == id)
    }

    /// The open holding of engine position `p`.
    fn open_holding(&self, symbol: &str, entry_ts: i64) -> Option<u64> {
        self.holdings
            .iter()
            .find(|h| {
                !h.residual
                    && h.trade_idx.is_none()
                    && !h.settled
                    && h.symbol == symbol
                    && h.entry_ts == entry_ts
            })
            .map(|h| h.id)
    }

    /// Orders that implement the engine's executions at the open `next_ts` (see the module doc).
    #[allow(clippy::too_many_arguments)]
    pub fn preview(
        &self,
        pf: &XsPortfolio,
        m: &Market,
        sc: &[Vec<Option<Score>>],
        p: &XsParams,
        next_ts: i64,
        next_wall: i64,
        mids: &HashMap<String, f64>,
    ) -> Result<Vec<Planned>> {
        let (m2, sc2) = preview_market(m, sc, next_ts, next_wall, mids)?;
        let mut copy = pf.clone();
        copy.take_events();
        let t = m2.ts.len() - 1;
        let before_trades = copy.trades.len();
        copy.step(&m2, &sc2, t, p);
        ensure!(
            copy.execution_error.is_none(),
            "preview failed: {:?}",
            copy.execution_error
        );
        let mut out = Vec::new();
        for tr in &copy.trades[before_trades..] {
            if tr.exit_ts != next_ts {
                continue;
            }
            let holding = self.open_holding(&tr.symbol, tr.entry_ts).unwrap_or(0);
            out.push(Planned {
                symbol: tr.symbol.clone(),
                side: 2,
                qty: tr.qty,
                holding,
                leg: LegKind::Sell,
                due_ts: next_ts,
                reason: format!("engine exit at this open: {}", tr.reason),
            });
        }
        for pos in &copy.positions {
            match pf
                .positions
                .iter()
                .find(|x| x.symbol == pos.symbol && x.entry_ts == pos.entry_ts)
            {
                None if pos.entry_ts == next_ts => out.push(Planned {
                    symbol: pos.symbol.clone(),
                    side: 1,
                    qty: pos.qty,
                    holding: 0,
                    leg: LegKind::Buy(0),
                    due_ts: next_ts,
                    reason: "engine entry at this open".into(),
                }),
                Some(old) if pos.qty > old.qty + 1e-9 => {
                    let holding = self.open_holding(&pos.symbol, pos.entry_ts).unwrap_or(0);
                    let k = self.holding(holding).map_or(1, |h| h.sim_buys.len());
                    out.push(Planned {
                        symbol: pos.symbol.clone(),
                        side: 1,
                        qty: (pos.qty - old.qty).round(),
                        holding,
                        leg: LegKind::Buy(k),
                        due_ts: next_ts,
                        reason: "engine add at this open".into(),
                    });
                }
                _ => {}
            }
        }
        out.sort_by_key(|o| -o.side); // sells first
        Ok(out)
    }

    /// Sells still owed at this open: shares of holdings the engine has closed that moomoo still
    /// holds, with no live sell order. Unplanned broker fills are deliberately excluded: a broker
    /// fill is real exposure and must not be converted into a forced loss merely because the
    /// candle-open simulation differed after a restart, quote change, or interval switch.
    pub fn owed_sells(&self, due_ts: i64, can_sell: &HashMap<String, f64>) -> Vec<Planned> {
        let mut out = Vec::new();
        let mut left: HashMap<String, f64> = can_sell.clone();
        for h in self
            .holdings
            .iter()
            .filter(|h| !h.settled && h.trade_idx.is_some())
        {
            let orders: Vec<&LiveOrder> =
                self.orders.iter().filter(|o| o.holding == h.id).collect();
            if orders.iter().any(|o| !o.is_final()) {
                continue;
            }
            let bought: f64 = orders
                .iter()
                .filter(|o| o.side == 1)
                .map(|o| o.filled())
                .sum();
            let sold: f64 = orders
                .iter()
                .filter(|o| o.side == 2)
                .map(|o| o.filled())
                .sum();
            let owed = (bought - sold).round();
            if owed <= 0.0 {
                continue;
            }
            let avail = left.get(&h.symbol).copied().unwrap_or(0.0);
            let qty = owed.min(avail).floor();
            if qty <= 0.0 {
                continue;
            }
            left.insert(h.symbol.clone(), avail - qty);
            out.push(Planned {
                symbol: h.symbol.clone(),
                side: 2,
                qty,
                holding: h.id,
                leg: LegKind::Sell,
                due_ts,
                reason: if h.residual {
                    "reconciliation: sell shares moomoo filled that the engine never held".into()
                } else {
                    "engine exit still owed (no real sell filled yet)".into()
                },
            });
        }
        out
    }

    /// Record an order placed (or refused) for `p`.
    pub fn record(
        &mut self,
        p: &Planned,
        placed_ms: i64,
        placed: Result<(u64, String), String>,
    ) -> u64 {
        let id = self.new_id();
        let (order_id, order_id_ex, error) = match placed {
            Ok((a, b)) => (Some(a), Some(b), None),
            Err(e) => (None, None, Some(e)),
        };
        self.orders.push(LiveOrder {
            id,
            holding: p.holding,
            leg: p.leg,
            symbol: p.symbol.clone(),
            side: p.side,
            qty: p.qty,
            due_ts: p.due_ts,
            placed_ms,
            order_id,
            order_id_ex,
            error,
            state: None,
            fee: None,
            fee_unavailable: false,
            fee_requests: 0,
            reason: p.reason.clone(),
        });
        id
    }

    /// moomoo's latest state of an order.
    pub fn update_state(&mut self, s: &OrderState) -> bool {
        match self
            .orders
            .iter_mut()
            .find(|o| o.order_id == Some(s.order_id))
        {
            Some(o) if o.state.as_ref() != Some(s) => {
                o.state = Some(s.clone());
                true
            }
            _ => false,
        }
    }

    /// Stores the real fee of the order it belongs to; true when it changed.
    pub fn update_fee(&mut self, f: &OrderFee) -> bool {
        match self
            .orders
            .iter_mut()
            .find(|o| o.order_id_ex.as_deref() == Some(f.order_id_ex.as_str()))
        {
            Some(o) if o.fee.as_ref() != Some(f) => {
                o.fee = Some(f.clone());
                o.fee_unavailable = false;
                true
            }
            _ => false,
        }
    }

    /// Orders moomoo may still change (ids).
    pub fn open_order_ids(&self) -> Vec<u64> {
        self.orders
            .iter()
            .filter(|o| !o.is_final())
            .filter_map(|o| o.order_id)
            .collect()
    }

    /// Filled orders whose fee is still unknown (ids_ex).
    pub fn fee_missing(&self) -> Vec<String> {
        self.orders
            .iter()
            .filter(|o| o.is_final() && o.filled() > 0.0 && o.fee.is_none() && !o.fee_unavailable)
            .filter_map(|o| o.order_id_ex.clone())
            .collect()
    }

    /// Tie the engine's executions at `due_ts` (one `advance` of the paper account) to orders:
    /// `before` = positions before the advance, `trades_before` = trades before it. Divergences
    /// are turned into the standard shapes (see the module doc). Returns log lines.
    pub fn link_step(
        &mut self,
        pf: &mut XsPortfolio,
        before: &[XsPosition],
        trades_before: usize,
        due_ts: Option<i64>,
    ) -> Vec<String> {
        let mut log = Vec::new();
        // 1. Engine exits: link the holding to its trade with the simulated sell.
        for idx in trades_before..pf.trades.len() {
            let tr = pf.trades[idx].clone();
            let Some(old) = before
                .iter()
                .find(|p| p.symbol == tr.symbol && p.entry_ts == tr.entry_ts)
            else {
                // Opened and closed inside this advance (entry and exit between two closes).
                let id = self.new_id();
                self.holdings.push(Holding {
                    id,
                    symbol: tr.symbol.clone(),
                    entry_ts: tr.entry_ts,
                    trade_idx: Some(idx),
                    sim_buys: vec![Leg {
                        qty: tr.qty,
                        price: tr.entry,
                        fee: 0.0,
                    }],
                    sim_sell: Some(Leg {
                        qty: tr.qty,
                        price: tr.exit,
                        fee: tr.fees,
                    }),
                    residual: false,
                    settled: false,
                    exit_ts: None,
                });
                self.attach(id, &tr.symbol, LegKind::Buy(0), tr.entry_ts);
                continue;
            };
            let hid = match self.open_holding(&tr.symbol, tr.entry_ts) {
                Some(h) => h,
                None => {
                    let id = self.new_id();
                    log.push(format!("RECONCILE {}: engine position had no holding record; tracking from its exit", tr.symbol));
                    self.holdings.push(Holding {
                        id,
                        symbol: tr.symbol.clone(),
                        entry_ts: tr.entry_ts,
                        trade_idx: None,
                        sim_buys: vec![Leg {
                            qty: old.qty,
                            price: old.entry,
                            fee: old.fees,
                        }],
                        sim_sell: None,
                        residual: false,
                        settled: false,
                        exit_ts: None,
                    });
                    id
                }
            };
            if let Some(h) = self.holdings.iter_mut().find(|h| h.id == hid) {
                h.trade_idx = Some(idx);
                h.sim_sell = Some(Leg {
                    qty: tr.qty,
                    price: tr.exit,
                    fee: (tr.fees - old.fees).max(0.0),
                });
            }
            self.attach(hid, &tr.symbol, LegKind::Sell, tr.exit_ts);
        }
        // 2. Engine entries and adds.
        for pos in &pf.positions {
            match before
                .iter()
                .find(|b| b.symbol == pos.symbol && b.entry_ts == pos.entry_ts)
            {
                None => {
                    let id = self.new_id();
                    self.holdings.push(Holding {
                        id,
                        symbol: pos.symbol.clone(),
                        entry_ts: pos.entry_ts,
                        trade_idx: None,
                        sim_buys: vec![Leg {
                            qty: pos.qty,
                            price: pos.entry,
                            fee: pos.fees,
                        }],
                        sim_sell: None,
                        residual: false,
                        settled: false,
                        exit_ts: None,
                    });
                    self.attach(id, &pos.symbol, LegKind::Buy(0), pos.entry_ts);
                }
                Some(old) if pos.qty > old.qty + 1e-9 => {
                    if let Some(hid) = self.open_holding(&pos.symbol, pos.entry_ts) {
                        let q = pos.qty - old.qty;
                        let price = (pos.entry * pos.qty - old.entry * old.qty) / q;
                        let fee = (pos.fees - old.fees).max(0.0);
                        let k = match self.holdings.iter_mut().find(|h| h.id == hid) {
                            Some(h) => {
                                h.sim_buys.push(Leg { qty: q, price, fee });
                                h.sim_buys.len() - 1
                            }
                            None => continue,
                        };
                        let at = due_ts.unwrap_or(pos.entry_ts);
                        self.attach(hid, &pos.symbol, LegKind::Buy(k), at);
                    }
                }
                _ => {}
            }
        }
        // 3. Orders for this open the engine did not execute.
        if let Some(due) = due_ts {
            let stray: Vec<usize> = (0..self.orders.len())
                .filter(|&i| self.orders[i].due_ts <= due && self.unlinked(i))
                .collect();
            for i in stray {
                let o = self.orders[i].clone();
                if o.side == 1 {
                    let id = self.new_id();
                    self.holdings.push(Holding {
                        id,
                        symbol: o.symbol.clone(),
                        entry_ts: due,
                        trade_idx: None,
                        sim_buys: vec![],
                        sim_sell: None,
                        residual: true,
                        settled: false,
                        exit_ts: None,
                    });
                    self.orders[i].holding = id;
                    log.push(format!("RECONCILE {}: real buy of {} at this open differed from the candle simulation; keeping the broker fill and refusing an automatic liquidation", o.symbol, o.qty));
                } else if let Some(pi) = pf.positions.iter().position(|p| {
                    p.symbol == o.symbol
                        && self.open_holding(&p.symbol, p.entry_ts) == Some(o.holding)
                }) {
                    // A real sell the engine did not make: the engine follows reality and closes the
                    // position (its real sell price is booked by `recompute`).
                    let p = pf.positions.remove(pi);
                    let gross = p.side.sign() * (p.mark - p.entry) * p.qty;
                    pf.equity += gross;
                    let pnl = gross - p.fees - p.funding;
                    pf.trades.push(Trade {
                        symbol: p.symbol.clone(),
                        side: p.side,
                        entry_ts: p.entry_ts,
                        exit_ts: due,
                        entry: p.entry,
                        exit: p.mark,
                        qty: p.qty,
                        leverage: p.leverage,
                        fees: p.fees,
                        funding: p.funding,
                        pnl,
                        r: pnl / p.margin.max(1e-9),
                        reason: "Reconcile: sold by a real order the engine did not repeat at the candle".into(),
                        decided_ts: p.decided_ts,
                    });
                    let idx = pf.trades.len() - 1;
                    if let Some(h) = self.holdings.iter_mut().find(|h| h.id == o.holding) {
                        h.trade_idx = Some(idx);
                        h.sim_sell = Some(Leg {
                            qty: p.qty,
                            price: p.mark,
                            fee: 0.0,
                        });
                    }
                    self.orders[i].leg = LegKind::Sell;
                    log.push(format!("RECONCILE {}: real sell at this open that the engine did not make at the candle open; the engine position is closed at the real fill", o.symbol));
                }
            }
        }
        log
    }

    /// True when order `i` is not tied to an engine execution.
    fn unlinked(&self, i: usize) -> bool {
        let o = &self.orders[i];
        match self.holding(o.holding) {
            None => true,
            Some(h) => match o.leg {
                LegKind::Buy(k) => k >= h.sim_buys.len() && !h.residual,
                LegKind::Sell => h.sim_sell.is_none() && !h.residual,
            },
        }
    }

    /// Give the unlinked order for (`symbol`, `leg`, `due_ts`) to holding `hid`.
    fn attach(&mut self, hid: u64, symbol: &str, leg: LegKind, due_ts: i64) {
        let found = self.orders.iter().position(|o| {
            o.symbol == symbol
                && o.due_ts == due_ts
                && match (o.leg, leg) {
                    (LegKind::Buy(_), LegKind::Buy(_)) => o.holding == 0 || o.holding == hid,
                    (LegKind::Sell, LegKind::Sell) => o.holding == 0 || o.holding == hid,
                    _ => false,
                }
        });
        if let Some(i) = found {
            self.orders[i].holding = hid;
            self.orders[i].leg = leg;
        }
    }

    /// Book every holding's real executions into the engine portfolio (see the module doc).
    /// Returns log lines for changes.
    pub fn recompute(&mut self, pf: &mut XsPortfolio) -> Vec<String> {
        let mut log = Vec::new();
        let mut removed_trades: Vec<usize> = Vec::new();
        for hi in 0..self.holdings.len() {
            if self.holdings[hi].settled {
                continue;
            }
            let h = self.holdings[hi].clone();
            let orders: Vec<LiveOrder> = self
                .orders
                .iter()
                .filter(|o| o.holding == h.id)
                .cloned()
                .collect();
            if h.residual {
                if let Some(line) = self.book_residual(hi, &orders, pf) {
                    log.push(line);
                }
                continue;
            }
            // Buys: real where the order is final, simulated while pending, nothing without an order.
            let (mut bq, mut bv, mut bf, mut pending) = (0.0, 0.0, 0.0, false);
            for (k, leg) in h.sim_buys.iter().enumerate() {
                let os: Vec<&LiveOrder> =
                    orders.iter().filter(|o| o.leg == LegKind::Buy(k)).collect();
                if os.is_empty() {
                    continue;
                }
                if os.iter().any(|o| !o.is_final()) {
                    bq += leg.qty;
                    bv += leg.qty * leg.price;
                    bf += leg.fee;
                    pending = true;
                    continue;
                }
                for o in os {
                    let (q, v) = o.fill();
                    bq += q;
                    bv += v;
                    match o.real_fee() {
                        Some(f) => bf += f,
                        None => {
                            bf += if same(q, leg.qty) { leg.fee } else { 0.0 };
                            pending = true;
                        }
                    }
                }
            }
            match h.trade_idx {
                None => {
                    let Some(pi) = pf
                        .positions
                        .iter()
                        .position(|p| p.symbol == h.symbol && p.entry_ts == h.entry_ts)
                    else {
                        continue;
                    };
                    let p = &mut pf.positions[pi];
                    if bq <= 0.0 {
                        pf.equity += p.fees;
                        log.push(format!("RECONCILE {}: the engine's entry never filled at moomoo (no real order or no fill): position removed, simulated fee {:.2} refunded", h.symbol, p.fees));
                        pf.positions.remove(pi);
                        self.holdings[hi].settled = true;
                        continue;
                    }
                    let entry = bv / bq;
                    if !(same(p.qty, bq) && same(p.entry, entry) && same(p.fees, bf)) {
                        log.push(format!(
                            "RECONCILE {}: engine entry {} @ {:.4} fee {:.2} -> moomoo {} @ {:.4} fee {:.2}{}",
                            h.symbol, p.qty, p.entry, p.fees, bq, entry, bf, if pending { " (fee pending)" } else { "" }
                        ));
                        let scale = entry / p.entry;
                        if same(p.peak, p.entry) {
                            p.peak = entry;
                        }
                        p.stop = p.stop.map(|s| s * scale);
                        pf.equity += p.fees - bf;
                        p.qty = bq;
                        p.entry = entry;
                        p.margin = bq * entry / p.leverage;
                        p.fees = bf;
                        p.fee_rate = bf / bv;
                    }
                }
                Some(idx) => {
                    let Some(tr) = pf.trades.get_mut(idx) else {
                        continue;
                    };
                    if tr.symbol != h.symbol {
                        log.push(format!(
                            "RECONCILE {}: trade index {idx} now holds {}; holding left unbooked",
                            h.symbol, tr.symbol
                        ));
                        self.holdings[hi].settled = true;
                        continue;
                    }
                    if bq <= 0.0 {
                        pf.equity -= tr.pnl;
                        log.push(format!("RECONCILE {}: the engine's trade never filled at moomoo: removed (its simulated pnl {:+.2} reversed)", h.symbol, tr.pnl));
                        removed_trades.push(idx);
                        self.holdings[hi].settled = true;
                        continue;
                    }
                    let sell = h.sim_sell.unwrap_or(Leg {
                        qty: bq,
                        price: tr.exit,
                        fee: 0.0,
                    });
                    let (mut sq, mut sv, mut sf) = (0.0, 0.0, 0.0);
                    for o in orders.iter().filter(|o| o.leg == LegKind::Sell) {
                        let (q, v) = o.fill();
                        sq += q;
                        sv += v;
                        match o.real_fee() {
                            Some(f) => sf += f,
                            None => pending = true,
                        }
                        if !o.is_final() {
                            pending = true;
                        }
                    }
                    let unsold = (bq - sq).max(0.0);
                    if unsold > 1e-9 {
                        pending = true;
                        sv += unsold * sell.price;
                        sf += if sell.qty > 0.0 {
                            sell.fee * unsold / sell.qty
                        } else {
                            0.0
                        };
                    }
                    let entry = bv / bq;
                    let exit = sv / bq;
                    let fees = bf + sf;
                    let pnl = tr.side.sign() * (exit - entry) * bq - fees - tr.funding;
                    if !(same(tr.qty, bq)
                        && same(tr.entry, entry)
                        && same(tr.exit, exit)
                        && same(tr.fees, fees))
                    {
                        log.push(format!(
                            "RECONCILE {}: engine trade {} @ {:.4} -> {:.4}, fees {:.2}, pnl {:+.2} -> moomoo {} @ {:.4} -> {:.4}, fees {:.2}, pnl {:+.2}{}",
                            h.symbol, tr.qty, tr.entry, tr.exit, tr.fees, tr.pnl, bq, entry, exit, fees, pnl, if pending { " (pending)" } else { "" }
                        ));
                        pf.equity += pnl - tr.pnl;
                        tr.qty = bq;
                        tr.entry = entry;
                        tr.exit = exit;
                        tr.fees = fees;
                        tr.pnl = pnl;
                        tr.r = pnl / (bq * entry / tr.leverage).max(1e-9);
                    }
                    if !pending && orders.iter().all(|o| o.is_final()) {
                        self.holdings[hi].settled = true;
                    }
                }
            }
        }
        removed_trades.sort_unstable();
        for &idx in removed_trades.iter().rev() {
            pf.trades.remove(idx);
            for h in &mut self.holdings {
                if let Some(t) = h.trade_idx.as_mut() {
                    if *t > idx {
                        *t -= 1;
                    }
                }
            }
        }
        log
    }

    /// A residual holding is booked once all its shares are sold: a reconciliation trade with the
    /// real buy and sell prices and fees.
    fn book_residual(
        &mut self,
        hi: usize,
        orders: &[LiveOrder],
        pf: &mut XsPortfolio,
    ) -> Option<String> {
        if orders.iter().any(|o| !o.is_final()) {
            return None;
        }
        let (mut bq, mut bv, mut sq, mut sv, mut fees) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for o in orders {
            let (q, v) = o.fill();
            if o.side == 1 {
                bq += q;
                bv += v;
            } else {
                sq += q;
                sv += v;
            }
            fees += o.real_fee()?;
        }
        if bq <= 0.0 {
            self.holdings[hi].settled = true;
            return None;
        }
        if sq + 1e-9 < bq {
            return None;
        }
        let h = &mut self.holdings[hi];
        let (entry, exit) = (bv / bq, sv / sq);
        let pnl = (exit - entry) * bq - fees;
        let exit_ts = orders
            .iter()
            .filter(|o| o.side == 2)
            .map(|o| o.due_ts)
            .max()
            .unwrap_or(h.entry_ts);
        pf.equity += pnl;
        pf.trades.push(Trade {
            symbol: h.symbol.clone(),
            side: Side::Long,
            entry_ts: h.entry_ts,
            exit_ts,
            entry,
            exit,
            qty: bq,
            leverage: 1.0,
            fees,
            funding: 0.0,
            pnl,
            r: pnl / (bq * entry).max(1e-9),
            reason: "Reconcile: real fill the engine did not make, sold".into(),
            decided_ts: None,
        });
        h.trade_idx = Some(pf.trades.len() - 1);
        h.exit_ts = Some(exit_ts);
        h.settled = true;
        Some(format!(
            "RECONCILE {}: unplanned real fill of {bq} sold, pnl {pnl:+.2} booked",
            h.symbol
        ))
    }

    /// Shares the engine's holdings account for at moomoo, per symbol (real filled buys minus sells
    /// of this bot's orders; simulated pending legs excluded).
    pub fn bot_shares(&self) -> HashMap<String, f64> {
        let mut out: HashMap<String, f64> = HashMap::new();
        for o in &self.orders {
            let q = o.filled();
            *out.entry(o.symbol.clone()).or_default() += if o.side == 1 { q } else { -q };
        }
        out.retain(|_, q| q.abs() > 1e-9);
        out
    }

    /// Drop settled holdings and their final orders older than the last `keep` (the store keeps
    /// the full history); trade indices of kept holdings stay valid.
    pub fn compact(&mut self, keep: usize) {
        let settled: Vec<u64> = self
            .holdings
            .iter()
            .filter(|h| h.settled)
            .map(|h| h.id)
            .collect();
        if settled.len() <= keep {
            return;
        }
        let drop: std::collections::HashSet<u64> =
            settled[..settled.len() - keep].iter().copied().collect();
        self.holdings.retain(|h| !drop.contains(&h.id));
        self.orders
            .retain(|o| !(drop.contains(&o.holding) && o.is_final()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::rules::Rules;
    use crate::engine::xs::{Regime, Risk, Signal};

    /// Test fixture: a two-symbol market of `n` bars with rules and mark candles.
    fn market(n: usize) -> Market {
        let mut m = Market {
            ts: (0..n as i64).map(|i| i * BAR_MS).collect(),
            wall: (0..n as i64).map(|i| i * BAR_MS).collect(),
            symbols: vec!["A".into(), "B".into()],
            listing_times: vec![Some(0); 2],
            entry_eligible: vec![true; 2],
            bars: [-0.1, 0.1]
                .iter()
                .map(|d| {
                    (0..n)
                        .map(|i| {
                            let p = 100.0 * (1.0 + d * i as f64 / n as f64);
                            Some(Bar {
                                open: p,
                                high: p,
                                low: p,
                                close: p,
                                volume: 1.0,
                                turnover: 100.0,
                            })
                        })
                        .collect()
                })
                .collect(),
            marks: vec![],
            funding: vec![vec![]; 2],
            rules: vec![
                Some(Rules {
                    qty_step: 1.0,
                    min_qty: 1.0,
                    min_notional: 0.0,
                    max_market_qty: 1e9,
                    commission: vec![(1.0, 1.0), (1e12, 1.0)],
                    commission_source: "test".into(),
                    max_leverage: 1.0,
                    book: vec![(10.0, 0.0005, 0.0005), (1e9, 0.0005, 0.0005)],
                    book_ts: 0,
                    adv_usd: None,
                    daily_vol: None,
                });
                2
            ],
        };
        m.marks = m.bars.clone();
        m
    }

    /// Test fixture: long-only Return settings with holding period `hold`.
    fn params(hold: usize) -> XsParams {
        XsParams {
            signal: Signal::Return,
            flip: false,
            lookback: 1,
            hold,
            top: 1,
            gross_leverage: 1.0,
            stop_pct: None,
            risk: Risk::default(),
            long_only: true,
            regime: Regime::Off,
        }
    }

    /// Test fixture: a completely filled order state.
    fn filled(order_id: u64, symbol: &str, side: i32, qty: f64, price: f64) -> OrderState {
        OrderState {
            order_id,
            order_id_ex: format!("x{order_id}"),
            symbol: symbol.into(),
            side,
            qty,
            status: 11,
            fill_qty: qty,
            fill_avg_price: Some(price),
            last_err: None,
            create_time: String::new(),
            update_time: String::new(),
            remark: None,
        }
    }

    /// Test fixture: a real fee of `total` USD for an order.
    fn fee(order_id: u64, total: f64) -> OrderFee {
        OrderFee {
            order_id_ex: format!("x{order_id}"),
            total,
            items: vec![("Commission".into(), total)],
        }
    }

    /// Steps a portfolio to the close before a rebalance fill, previews the open, places the orders
    /// (fills given by `fill`), steps the real candle and reconciles.
    struct Run {
        m: Market,
        pf: XsPortfolio,
        ledger: Ledger,
        p: XsParams,
        t: usize,
        next_order: u64,
    }

    impl Run {
        /// Test fixture: a run on the 200-bar market with a fresh 1,000 USD portfolio and empty ledger.
        fn new(hold: usize) -> Run {
            Run {
                m: market(200),
                pf: XsPortfolio::new(1000.0),
                ledger: Ledger::default(),
                p: params(hold),
                t: 0,
                next_order: 1,
            }
        }

        /// Steps bars up to and including `t` (no orders due) and decides at `t`.
        fn to(&mut self, t: usize) {
            let sc = crate::engine::scores::compute(&self.m, 20);
            while self.t <= t {
                self.pf.step(&self.m, &sc, self.t, &self.p);
                self.t += 1;
            }
        }

        /// Preview at the next open with quote mids `mids`, fill each order via `fill`, step the
        /// real candle, link and recompute.
        fn open(
            &mut self,
            mids: &[(&str, f64)],
            fill: impl Fn(&Planned) -> Option<f64>,
        ) -> (Vec<Planned>, Vec<String>) {
            let sc = crate::engine::scores::compute(&self.m, 20);
            let sub = self.m.slice(0..self.t);
            let subsc: Vec<Vec<Option<Score>>> = sc.iter().map(|r| r[..self.t].to_vec()).collect();
            let mids: HashMap<String, f64> =
                mids.iter().map(|(s, p)| (s.to_string(), *p)).collect();
            let next = self.m.ts[self.t];
            let mut plan = self
                .ledger
                .preview(
                    &self.pf,
                    &sub,
                    &subsc,
                    &self.p,
                    next,
                    self.m.wall[self.t],
                    &mids,
                )
                .unwrap();
            plan.extend(self.ledger.owed_sells(next, &self.ledger.bot_shares()));
            for pl in &plan {
                let oid = self.next_order;
                self.next_order += 1;
                self.ledger.record(pl, 0, Ok((oid, format!("x{oid}"))));
                let price = fill(pl);
                let st = match price {
                    Some(px) => filled(oid, &pl.symbol, pl.side, pl.qty, px),
                    None => OrderState {
                        status: 15,
                        fill_qty: 0.0,
                        fill_avg_price: None,
                        ..filled(oid, &pl.symbol, pl.side, pl.qty, 0.0)
                    },
                };
                self.ledger.update_state(&st);
                if price.is_some() {
                    self.ledger.update_fee(&fee(oid, 0.5));
                }
            }
            let before = self.pf.positions.clone();
            let tb = self.pf.trades.len();
            self.pf.step(&self.m, &sc, self.t, &self.p);
            let mut log = self.ledger.link_step(&mut self.pf, &before, tb, Some(next));
            log.extend(self.ledger.recompute(&mut self.pf));
            self.t += 1;
            (plan, log)
        }

        /// Equity invariant: start + closed pnl - fees of open positions.
        fn check(&self) {
            let closed: f64 = self.pf.trades.iter().map(|t| t.pnl).sum();
            let open_fees: f64 = self.pf.positions.iter().map(|p| p.fees).sum();
            assert!(
                (self.pf.equity - (1000.0 + closed - open_fees)).abs() < 1e-6,
                "equity {} vs {}",
                self.pf.equity,
                1000.0 + closed - open_fees
            );
            self.pf.validate_state().unwrap();
        }
    }

    /// hold 4: rebalance decided at the close of bar 71 (after the score warm-up) fills at the open of bar 73.
    #[test]
    fn preview_orders_the_engines_entry_and_real_fills_replace_the_simulated_ones() {
        let mut r = Run::new(4);
        r.to(72); // decision at bar 3's close; bar 4 passes (fill bound is bar 5)
        assert!(r.pf.positions.is_empty());
        let (plan, log) = r.open(&[("A", 90.0), ("B", 110.0)], |_| Some(90.5));
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(
            (plan[0].symbol.as_str(), plan[0].side, plan[0].leg),
            ("A", 1, LegKind::Buy(0)),
            "long the lowest return"
        );
        let pos = &r.pf.positions[0];
        assert_eq!(
            (pos.symbol.as_str(), pos.qty, pos.entry, pos.fees),
            ("A", plan[0].qty, 90.5, 0.5),
            "{log:?}"
        );
        assert!(
            r.ledger.holdings[0].sim_buys[0].price != 90.5,
            "the simulated fill is kept for the record"
        );
        r.check();
    }

    /// Test: an entry without a real fill is removed and its fee refunded.
    #[test]
    fn an_entry_without_a_real_fill_is_removed_and_its_fee_refunded() {
        let mut r = Run::new(4);
        r.to(72);
        let (plan, log) = r.open(&[("A", 90.0), ("B", 110.0)], |_| None);
        assert_eq!(plan.len(), 1);
        assert!(r.pf.positions.is_empty(), "{log:?}");
        assert!(log.iter().any(|l| l.contains("never filled")));
        assert_eq!(r.pf.equity, 1000.0);
        r.check();
    }

    /// Test: exits are ordered at their open and booked at the real price.
    #[test]
    fn exits_are_ordered_at_their_open_and_booked_at_the_real_price() {
        let mut r = Run::new(4);
        r.to(72);
        r.open(&[("A", 90.0), ("B", 110.0)], |_| Some(90.5));
        let qty = r.pf.positions[0].qty;
        r.to(76); // next rebalance decided at bar 7's close, fills at bar 9
        let (plan, log) = r.open(&[("A", 89.0), ("B", 111.0)], |pl| {
            Some(if pl.side == 2 { 88.0 } else { 88.9 })
        });
        assert!(
            plan.iter()
                .any(|p| p.side == 2 && p.symbol == "A" && p.qty == qty),
            "{plan:?}"
        );
        assert_eq!(plan[0].side, 2, "sells first");
        let tr = r.pf.trades.last().unwrap();
        assert_eq!((tr.exit, tr.fees), (88.0, 1.0), "{log:?}");
        assert!((tr.pnl - ((88.0 - 90.5) * qty - 1.0)).abs() < 1e-9);
        r.check();
    }

    /// Test: a real fill the engine did not make is sold and booked.
    #[test]
    fn a_real_fill_the_engine_did_not_make_is_sold_and_booked() {
        let mut r = Run::new(4);
        r.to(72);
        // A real buy of B at this open that the engine does not make (e.g. the engine waited).
        let stray = Planned {
            symbol: "B".into(),
            side: 1,
            qty: 2.0,
            holding: 0,
            leg: LegKind::Buy(0),
            due_ts: r.m.ts[r.t],
            reason: "test".into(),
        };
        r.ledger.record(&stray, 0, Ok((900, "x900".into())));
        r.ledger.update_state(&filled(900, "B", 1, 2.0, 50.0));
        r.ledger.update_fee(&fee(900, 0.5));
        let (_, log) = r.open(&[("A", 90.0), ("B", 110.0)], |_| Some(90.5));
        assert!(
            r.ledger
                .holdings
                .iter()
                .any(|h| h.residual && h.symbol == "B"),
            "{log:?}"
        );
        assert!(
            log.iter()
                .any(|line| line.contains("refusing an automatic liquidation")),
            "{log:?}"
        );
        r.check();
        // Next open: an unplanned broker fill is kept; reconciliation must not force a loss.
        let (plan, _log) = r.open(&[("A", 90.0), ("B", 51.0)], |pl| {
            Some(if pl.symbol == "B" { 51.0 } else { 90.0 })
        });
        assert!(
            !plan.iter().any(|p| p.symbol == "B" && p.side == 2),
            "{plan:?}"
        );
        assert!(!r
            .pf
            .trades
            .iter()
            .any(|t| t.reason.starts_with("Reconcile")));
        r.check();
    }

    /// Test: an engine exit without a real sell is sold at the next open.
    #[test]
    fn an_engine_exit_without_a_real_sell_is_sold_at_the_next_open() {
        let mut r = Run::new(4);
        r.to(72);
        r.open(&[("A", 90.0), ("B", 110.0)], |_| Some(90.5));
        r.to(76);
        // The exit order is refused (no fill): the engine still books its exit at the candle; the
        // shares are owed and sold at the next open.
        let (_, _) = r.open(&[("A", 89.0), ("B", 111.0)], |pl| {
            if pl.side == 2 {
                None
            } else {
                Some(88.9)
            }
        });
        let owed = r.ledger.owed_sells(
            r.m.ts[r.t],
            &[("A".to_string(), 100.0)].into_iter().collect(),
        );
        assert_eq!(owed.len(), 1);
        assert_eq!((owed[0].symbol.as_str(), owed[0].side), ("A", 2));
        r.check();
    }

    /// Test: fees arriving later adjust equity once.
    #[test]
    fn fees_arriving_later_adjust_equity_once() {
        let mut r = Run::new(4);
        r.to(72);
        let sc = crate::engine::scores::compute(&r.m, 20);
        let sub = r.m.slice(0..r.t);
        let subsc: Vec<Vec<Option<Score>>> = sc.iter().map(|x| x[..r.t].to_vec()).collect();
        let mids: HashMap<String, f64> = [("A".to_string(), 90.0), ("B".to_string(), 110.0)]
            .into_iter()
            .collect();
        let plan = r
            .ledger
            .preview(&r.pf, &sub, &subsc, &r.p, r.m.ts[r.t], r.m.wall[r.t], &mids)
            .unwrap();
        r.ledger.record(&plan[0], 0, Ok((7, "x7".into())));
        r.ledger.update_state(&filled(7, "A", 1, plan[0].qty, 90.5));
        let before = r.pf.positions.clone();
        r.pf.step(&r.m, &sc, r.t, &r.p);
        r.ledger.link_step(&mut r.pf, &before, 0, Some(r.m.ts[r.t]));
        r.ledger.recompute(&mut r.pf);
        assert_eq!(
            r.pf.positions[0].fees, 1.0,
            "simulated fee while moomoo's is pending"
        );
        assert_eq!(r.ledger.fee_missing(), vec!["x7".to_string()]);
        r.ledger.update_fee(&fee(7, 0.3));
        r.ledger.recompute(&mut r.pf);
        r.ledger.recompute(&mut r.pf);
        assert_eq!(r.pf.positions[0].fees, 0.3);
        r.check();
    }

    /// Test: residual broker fills are never auto-liquidated.
    #[test]
    fn residual_shares_never_exceed_what_moomoo_can_sell() {
        let mut l = Ledger::default();
        l.holdings.push(Holding {
            id: 1,
            symbol: "X".into(),
            entry_ts: 0,
            trade_idx: None,
            sim_buys: vec![],
            sim_sell: None,
            residual: true,
            settled: false,
            exit_ts: None,
        });
        let p = Planned {
            symbol: "X".into(),
            side: 1,
            qty: 5.0,
            holding: 1,
            leg: LegKind::Buy(0),
            due_ts: 0,
            reason: String::new(),
        };
        l.record(&p, 0, Ok((1, "x1".into())));
        l.update_state(&filled(1, "X", 1, 5.0, 10.0));
        let owed = l.owed_sells(BAR_MS, &[("X".to_string(), 3.0)].into_iter().collect());
        assert!(owed.is_empty());
        assert!(l.owed_sells(BAR_MS, &HashMap::new()).is_empty());
    }
}

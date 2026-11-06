//! Per-symbol trading rules, all taken from moomoo, never assumed:
//! * order rules from the static security info (lot size = whole shares),
//! * this account's own commission for each order size, from the real fees of its paper orders
//!   (Trd_GetOrderFee; excluded until a real fee exists),
//! * the cost of a market order of a given size: the measured top-of-book bid/ask walked
//!   like a book, or, under the liquidity model, half-spread plus square-root impact on real ADV,
//! * the leverage cap (1x: the user's no-leverage rule for this bot).
//!
//! moomoo publishes no historical books, so backtests use the most recent measurement
//! (`book_ts` says when); the live service re-measures every hour. A symbol without rules is
//! not traded, and an order larger than the measured depth is not filled.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

// NOTE(agents): LIQUIDITY MODEL (user 2026-10-09: at the account's size most of the money sat undeployed). The measured "book" is ONE snapshot of the
//               displayed top-of-book size (e.g. NVDA ~20 shares), which capped every order at a few thousand USD whatever the account. When this
//               switch is on and a symbol has `adv_usd`/`daily_vol` (measured from its REAL traded candles), an order may be at most
//               `PARTICIPATION_CAP` of the stock's average daily dollar volume and costs the measured half-spread plus a square-root market impact
//               `IMPACT_Y x daily_vol x sqrt(order / adv)`. Both constants are modelling choices (the common square-root law), labelled as such,
//               and are checked against real paper fills by examples/fill_probe.rs.
pub static LIQUIDITY_MODEL: AtomicBool = AtomicBool::new(false);
/// An order is at most this share of the stock's average daily dollar volume.
pub const PARTICIPATION_CAP: f64 = 0.002;

/// Research-only ceiling on one entry order's notional in USD (bits of an f64; 0 = none). The service never sets it.
pub static ORDER_CAP_USD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The research-only per-order USD cap, when set.
fn order_cap_usd() -> Option<f64> {
    let c = f64::from_bits(ORDER_CAP_USD.load(Ordering::Relaxed));
    (c.is_finite() && c > 0.0).then_some(c)
}
/// Square-root-law impact coefficient.
pub const IMPACT_Y: f64 = 1.0;

/// Whether the liquidity model (ADV participation cap and square-root impact) is on.
pub fn liquidity_model_on() -> bool {
    LIQUIDITY_MODEL.load(Ordering::Relaxed)
}

// NOTE(agents): No fixed upper size (user rule: no fixed parameters). The ladder doubles from
//               10 USD; measurement stops where the data stops it: the quote's displayed depth
//               (book cost) or the largest order with a real fee from this account (commission).
/// Order sizes (USD notional) at which the book cost and the commission are measured:
/// 10, 20, 40, ... (the measurements end where the data ends).
pub fn book_points() -> impl Iterator<Item = f64> {
    (0..40).map(|k| 10.0 * 2f64.powi(k))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    pub qty_step: f64,
    pub min_qty: f64,
    pub min_notional: f64,
    pub max_market_qty: f64,
    /// This account's commission: (order notional USD, commission USD), ascending notional,
    /// from the real fees of this account's paper orders, up to the largest order with a fee.
    pub commission: Vec<(f64, f64)>,
    /// Where `commission` comes from: real paper-order fees, "excluded" or the simulation in use.
    #[serde(default)]
    pub commission_source: String,
    /// Highest leverage allowed (1 = cash only).
    pub max_leverage: f64,
    /// Measured market-order cost: (notional USD, buy cost, sell cost) as a
    /// fraction of the mid price, for each book_points() size the book could fill.
    pub book: Vec<(f64, f64, f64)>,
    /// When the book was measured (ms).
    pub book_ts: i64,
    /// Average daily dollar turnover and daily return volatility (a fraction) measured from the symbol's real traded candles (None: not measured).
    #[serde(default)]
    pub adv_usd: Option<f64>,
    #[serde(default)]
    pub daily_vol: Option<f64>,
}

impl Rules {
    /// Quantity moomoo accepts for an order of about `qty` at `price`: rounded
    /// down to the size increment; None below the minimum size or order value.
    pub fn order_qty(&self, qty: f64, price: f64) -> Option<f64> {
        if !qty.is_finite()
            || !price.is_finite()
            || price <= 0.0
            || qty <= 0.0
            || self.qty_step < 0.0
        {
            return None;
        }
        // The displayed top-of-book size caps an order only under the old snapshot model; the liquidity model limits size by participation (`order_notional_cap`).
        let qty = if liquidity_model_on() && self.adv_usd.is_some() {
            qty
        } else {
            qty.min(self.max_market_qty)
        };
        let q = if self.qty_step > 0.0 {
            (qty / self.qty_step + 1e-9).floor() * self.qty_step
        } else {
            qty
        };
        (q > 0.0 && q >= self.min_qty && q * price >= self.min_notional).then_some(q)
    }

    // NOTE(agents): Half the measured depth is kept free so the position can be closed; don't raise
    //               it to fit bigger orders. Large balances keep cash instead.
    /// Leave half of the measured book capacity as a closing-liquidity reserve.
    /// Large balances keep unallocated cash instead of assuming infinite depth.
    pub fn order_notional_cap(&self, reference: f64) -> Option<f64> {
        let cap = self.book_notional_cap(reference)?;
        Some(order_cap_usd().map_or(cap, |c| cap.min(c)))
    }

    /// Largest order notional for this symbol: 0.2% of ADV under the liquidity model, else half the displayed book depth.
    fn book_notional_cap(&self, reference: f64) -> Option<f64> {
        if let (true, Some(adv)) = (
            liquidity_model_on(),
            self.adv_usd.filter(|a| a.is_finite() && *a > 0.0),
        ) {
            return Some(PARTICIPATION_CAP * adv);
        }
        let depth = self.book.last()?.0;
        Some((depth * 0.5).min(self.max_market_qty * reference))
    }

    // NOTE(agents): Beyond measured depth this returns None (no fill). Never extrapolate: the old
    //               linear extrapolation produced negative exit prices.
    /// Market-order cost (fraction of price) for `notional` USD, buying or
    /// selling, interpolated between measured sizes; None if the measured book
    /// was too thin for that size.
    pub fn book_cost(&self, notional: f64, buy: bool) -> Option<f64> {
        if !notional.is_finite()
            || notional <= 0.0
            || self.book.iter().any(|&(n, b, s)| {
                !n.is_finite()
                    || n <= 0.0
                    || !b.is_finite()
                    || !s.is_finite()
                    || !(0.0..1.0).contains(&b)
                    || !(0.0..1.0).contains(&s)
            })
            || self.book.windows(2).any(|w| w[0].0 >= w[1].0)
        {
            return None;
        }
        let pick = |p: &(f64, f64, f64)| if buy { p.1 } else { p.2 };
        let first = self.book.first()?;
        if let (true, Some(adv), Some(vol)) = (
            liquidity_model_on(),
            self.adv_usd.filter(|a| a.is_finite() && *a > 0.0),
            self.daily_vol.filter(|v| v.is_finite() && *v >= 0.0),
        ) {
            // measured half-spread + square-root impact; never beyond a sane bound
            let cost = pick(first) + IMPACT_Y * vol * (notional / adv).sqrt();
            return (cost < 0.5).then_some(cost);
        }
        if notional <= first.0 {
            return Some(pick(first));
        }
        for w in self.book.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            if notional <= b.0 {
                let f = (notional - a.0) / (b.0 - a.0);
                return Some(pick(a) + f * (pick(b) - pick(a)));
            }
        }
        None
    }

    // NOTE(agents): US stock fees have per-share parts and per-order minimums, so the fee is a function of
    //               the order size, never a flat rate. Beyond the largest measured size: None.
    /// This account's commission (USD) for an order of `notional` USD, interpolated between
    /// the measured sizes (the measured minimum below the smallest); None when unmeasured.
    pub fn fee(&self, notional: f64) -> Option<f64> {
        if !notional.is_finite()
            || notional <= 0.0
            || self
                .commission
                .iter()
                .any(|&(n, c)| !n.is_finite() || n <= 0.0 || !c.is_finite() || c < 0.0)
            || self.commission.windows(2).any(|w| w[0].0 >= w[1].0)
        {
            return None;
        }
        let first = self.commission.first()?;
        if notional <= first.0 {
            return Some(first.1);
        }
        for w in self.commission.windows(2) {
            let (a, b) = (w[0], w[1]);
            if notional <= b.0 {
                return Some(a.1 + (notional - a.0) / (b.0 - a.0) * (b.1 - a.1));
            }
        }
        None
    }

    /// The order notional whose margin plus commission (× `cost_mult`) uses `budget` at
    /// `leverage`, capped at `cap`; None when the commission is unmeasured at that size.
    pub fn sized_notional(
        &self,
        budget: f64,
        leverage: f64,
        cost_mult: f64,
        cap: f64,
    ) -> Option<f64> {
        if !budget.is_finite() || budget <= 0.0 || !leverage.is_finite() || leverage <= 0.0 {
            return None;
        }
        // Margin + fee(n) is increasing in n; bisect for the largest n that fits the budget.
        let fits = |n: f64| self.fee(n).map(|f| n / leverage + f * cost_mult <= budget);
        let hi0 = (budget * leverage).min(cap);
        if fits(hi0)? {
            return Some(hi0);
        }
        let (mut lo, mut hi) = (0.0_f64, hi0);
        for _ in 0..60 {
            let mid = (lo + hi) / 2.0;
            match fits(mid) {
                Some(true) => lo = mid,
                Some(false) => hi = mid,
                None => return None,
            }
        }
        (lo > 0.0).then_some(lo)
    }

    /// Price at which a position's loss uses up its posted margin. At 1x (cash) a long's
    /// is zero: it cannot be liquidated.
    pub fn liquidation_price(
        &self,
        side: super::Side,
        qty: f64,
        entry: f64,
        margin: f64,
        _fee_mult: f64,
    ) -> Option<f64> {
        if !qty.is_finite()
            || qty <= 0.0
            || !entry.is_finite()
            || entry <= 0.0
            || !margin.is_finite()
            || margin < 0.0
        {
            return None;
        }
        Some((entry - side.sign() * margin / qty).max(0.0))
    }

    /// Whether `leverage` is allowed for an order of `value` (finite and within the symbol's maximum).
    pub fn leverage_allowed(&self, value: f64, leverage: f64) -> bool {
        value.is_finite() && leverage.is_finite() && leverage > 0.0 && leverage <= self.max_leverage
    }
}

/// UNIT TESTS ONLY: a deep, uniform book (0.02% cost at every size) and a flat 0.055%
/// commission, so engine tests can trade synthetic series.
#[cfg(test)]
pub const TEST_COST: f64 = 0.0002;

#[cfg(test)]
impl Rules {
    /// Test fixture: rules of a deep, cheap market that never limits an order.
    pub fn test_liquid() -> Rules {
        Rules {
            qty_step: 0.0,
            min_qty: 0.0,
            min_notional: 5.0,
            max_market_qty: 1e12,
            commission: vec![(1.0, 0.00055), (1e12, 1e12 * 0.00055)],
            commission_source: "test".into(),
            max_leverage: 100.0,
            book: book_points()
                .take(10)
                .map(|n| (n, TEST_COST, TEST_COST))
                .collect(),
            book_ts: 0,
            adv_usd: None,
            daily_vol: None,
        }
    }
}

/// Cost of a market order of `notional` USD against one side of the book
/// (`levels`: (price, size) best first), as a fraction of `mid`; None when the
/// levels cannot fill it.
pub fn walk_book(levels: &[(f64, f64)], mid: f64, notional: f64) -> Option<f64> {
    if !mid.is_finite()
        || mid <= 0.0
        || !notional.is_finite()
        || notional <= 0.0
        || levels
            .iter()
            .any(|&(p, q)| !p.is_finite() || !q.is_finite() || p <= 0.0 || q <= 0.0)
    {
        return None;
    }
    let (mut left, mut qty, mut spent) = (notional, 0.0, 0.0);
    for &(price, size) in levels {
        let take = (left / price).min(size);
        qty += take;
        spent += take * price;
        left -= take * price;
        if left <= 1e-9 {
            let vwap = spent / qty;
            return Some((vwap / mid - 1.0).abs());
        }
    }
    None
}

/// Book cost at every book_points() size the book can fill.
pub fn measure_book(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> Vec<(f64, f64, f64)> {
    let (Some(b), Some(a)) = (bids.first(), asks.first()) else {
        return vec![];
    };
    if b.0 > a.0
        || bids.windows(2).any(|w| w[0].0 < w[1].0)
        || asks.windows(2).any(|w| w[0].0 > w[1].0)
    {
        return vec![];
    }
    let mid = (a.0 + b.0) / 2.0;
    book_points()
        .map_while(|n| Some((n, walk_book(asks, mid, n)?, walk_book(bids, mid, n)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test fixture: rules with fine lot steps and a measured book and commission ladder.
    fn rules() -> Rules {
        Rules {
            qty_step: 0.0001,
            min_qty: 0.0001,
            min_notional: 0.0,
            max_market_qty: 1e12,
            commission: vec![(10.0, 1.0), (200.0, 1.0), (6400.0, 2.5)],
            commission_source: "test".into(),
            max_leverage: 1.0,
            book: measure_book(&[(99.9, 1.0), (99.0, 10.0)], &[(100.1, 1.0), (101.0, 10.0)]),
            book_ts: 0,
            adv_usd: None,
            daily_vol: None,
        }
    }

    /// Test: cash long cannot be liquidated.
    #[test]
    fn cash_long_cannot_be_liquidated() {
        let r = rules();
        let long = super::super::Side::Long;
        assert_eq!(
            r.liquidation_price(long, 10.0, 100.0, 1000.0, 1.0),
            Some(0.0)
        );
        assert_eq!(r.liquidation_price(long, 0.0, 100.0, 10.0, 1.0), None);
        assert_eq!(r.liquidation_price(long, 1.0, 100.0, -1.0, 1.0), None);
    }

    /// Test: walks the book for the order size.
    #[test]
    fn walks_the_book_for_the_order_size() {
        // Mid 100. 50 USD fits in the first ask level: cost 0.1%.
        assert!(
            (walk_book(&[(100.1, 1.0), (101.0, 10.0)], 100.0, 50.0).unwrap() - 0.001).abs() < 1e-9
        );
        let c = walk_book(&[(100.1, 1.0), (101.0, 10.0)], 100.0, 200.0).unwrap();
        assert!(c > 0.001 && c < 0.01, "{c}");
        assert_eq!(
            walk_book(&[(100.1, 1.0)], 100.0, 200.0),
            None,
            "book too thin"
        );
    }

    /// Test: interpolates cost and refuses beyond depth.
    #[test]
    fn interpolates_cost_and_refuses_beyond_depth() {
        let r = rules();
        let small = r.book_cost(10.0, true).unwrap();
        let mid = r.book_cost(150.0, true).unwrap();
        assert!(mid > small);
        assert_eq!(r.book_cost(1600.0, true), None);
    }

    /// Test: commission follows the measured ladder and minimum.
    #[test]
    fn commission_follows_the_measured_ladder_and_minimum() {
        let r = rules();
        assert_eq!(
            r.fee(5.0),
            Some(1.0),
            "the minimum applies below the smallest size"
        );
        assert_eq!(r.fee(150.0), Some(1.0));
        assert!((r.fee(3300.0).unwrap() - 1.75).abs() < 1e-9);
        assert_eq!(r.fee(7000.0), None, "never extrapolated");
    }

    /// Test: sizing spends the budget on margin plus commission.
    #[test]
    fn sizing_spends_the_budget_on_margin_plus_commission() {
        let r = rules();
        let n = r.sized_notional(250.0, 1.0, 1.0, 1e9).unwrap();
        assert!((n + r.fee(n).unwrap() - 250.0).abs() < 1e-6, "{n}");
        assert_eq!(r.sized_notional(250.0, 1.0, 1.0, 100.0), Some(100.0), "cap");
    }

    /// Test: order qty and leverage cap.
    #[test]
    fn order_qty_and_leverage_cap() {
        let r = rules();
        assert!((r.order_qty(0.54321, 370.0).unwrap() - 0.5432).abs() < 1e-12);
        assert_eq!(r.order_qty(0.00009, 370.0), None);
        assert!(r.leverage_allowed(1_000.0, 1.0));
        assert!(!r.leverage_allowed(1_000.0, 1.5), "no leverage");
    }
}

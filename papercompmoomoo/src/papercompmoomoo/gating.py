"""Live gating architecture (live-trading-gating-architecture, 2026-11-02 session): four tiers between OpenD's top movers and an order.

Tier 1 hygiene: Nasdaq-listed common stock, price in [$2, $1000], and no buy order already working.
Tier 2 early inflection: RVOL >= 1.8 (1.2 pre-market), within +60% of the base, price still accelerating.
Tier 3 connectome: entry conviction must exceed the bear sentry reflex plus a margin (0.0; bullish conviction never feeds the veto).
Tier 4 sizing: clamp(cash / max positions x scale, $1000, net assets x 0.25), never above 40% of the portfolio; prices on the cent; slip-protected limits.

Pure functions only, so every gate is unit-tested.
"""
from __future__ import annotations

import math
from decimal import Decimal, ROUND_HALF_UP, ROUND_FLOOR, ROUND_CEILING

MIN_PRICE = 2.0
MAX_PRICE = 1000.0
MIN_RVOL = 1.8
MIN_RVOL_PRE = 1.2
MAX_BASE_EXTENSION_PCT = 60.0
ENTRY_MARGIN = 0.0
MIN_NOTIONAL = 1000.0
MAX_NOTIONAL_SHARE = 0.25
MAX_CONCENTRATION = 0.40
SLIP_LIMIT = 0.005


def tier1_hygiene(symbol: str, price: float, nasdaq: frozenset[str], held: set[str], working: set[str]) -> str | None:
    if symbol not in nasdaq:
        return "not a Nasdaq-listed common stock"
    if not (MIN_PRICE <= price <= MAX_PRICE):
        return f"price {price:.2f} outside [{MIN_PRICE:.2f}, {MAX_PRICE:.2f}]"
    # A current position does not veto a fresh qualifying entry. The broker
    # aggregates same-symbol buys into one position; capital/risk sizing still
    # applies and the working-order gate below prevents duplicate submissions.
    if symbol in working:
        return "an order for it is already working"
    return None


def base_support(price: float, change_pct: float) -> float:
    """Price / (1 + max(change/100, -0.5)): where the move started."""
    return price / (1.0 + max(change_pct / 100.0, -0.5))


def base_extension_pct(price: float, change_pct: float) -> float:
    base = base_support(price, change_pct)
    return (price / base - 1.0) * 100.0 if base > 0 else math.inf


def accelerating(prices: list[float]) -> bool:
    """Kinematic check on the latest live prices: velocity positive and not decelerating (a 1-D alpha-beta smoothed trajectory). Fewer than 3 points: unknown, allowed."""
    if len(prices) < 3:
        return True
    level, velocity, prev_velocity = prices[0], 0.0, 0.0
    for p in prices[1:]:
        predicted = level + velocity
        residual = p - predicted
        level = predicted + 0.5 * residual
        prev_velocity, velocity = velocity, velocity + 0.3 * residual
    return velocity > 0 and velocity >= prev_velocity * 0.5


def tier2_inflection(price: float, change_pct: float, rvol: float, pre_market: bool, recent_prices: list[float] | None = None) -> str | None:
    if not all(math.isfinite(x) for x in (price, change_pct, rvol)) or price <= 0:
        return "invalid market evidence"
    minimum = MIN_RVOL_PRE if pre_market else MIN_RVOL
    if rvol < minimum:
        return f"RVOL {rvol:.2f} below {minimum:.2f}"
    if change_pct <= 0:
        return "not a gainer"
    ext = base_extension_pct(price, change_pct)
    if ext > MAX_BASE_EXTENSION_PCT + 1e-9:
        return f"extended {ext:.1f}% from base (max {MAX_BASE_EXTENSION_PCT:.0f}%)"
    if recent_prices and not accelerating(recent_prices):
        return "price is decelerating"
    return None


def tier3_connectome(conviction: float, bear: float, margin: float = ENTRY_MARGIN) -> str | None:
    if all(math.isfinite(x) for x in (conviction, bear, margin)) and conviction > bear + margin:
        return None
    return f"conviction {conviction:.2f} not above bear sentry {bear:.2f} + {margin:.2f}"


def tier4_notional(cash: float, net_assets: float, max_positions: int, size_scale: float = 1.0) -> float:
    if not all(math.isfinite(x) for x in (cash, net_assets, size_scale)) or cash <= 0 or net_assets <= 0 or size_scale <= 0:
        return 0.0
    raw = cash / max(1, max_positions) * size_scale
    target = min(max(raw, MIN_NOTIONAL), net_assets * MAX_NOTIONAL_SHARE)
    return min(target, net_assets * MAX_CONCENTRATION, cash)


def tick(price: float) -> float:
    """Exchange tick: floor(price * 100 + 0.5) / 100."""
    return float(Decimal(str(price)).quantize(Decimal("0.01"), rounding=ROUND_HALF_UP))


def slip_limit(price: float, side_buy: bool = True) -> float:
    # Directional rounding keeps the final cent price inside the slippage budget.
    bound = Decimal(str(price)) * (Decimal("1.005") if side_buy else Decimal("0.995"))
    return float(bound.quantize(Decimal("0.01"), rounding=ROUND_FLOOR if side_buy else ROUND_CEILING))

"""A symbol's own swings: when getting in would have paid, and what a trailing stop would have kept (owner order 2026-10-21).

For one symbol's stored 1-minute bars: find every up-swing as it would have been traded. The entry is the first bar whose close is ``confirm`` percent above the lowest low since the
last exit (the swing is confirmed, no guessing of the bottom); the trade then trails the highest high with ``trail`` percent and has a hard stop ``stop`` percent under the entry; a bar
that touches both is taken as the stop (the cautious order). Everything uses only bars up to the one being judged, entry at that bar's close plus the cost. The result per
(confirm, trail) is what that symbol's history says: how many entries, the mean and median net return, the win rate, how far the price ran after entry and how long the trade lasted.
"""
from __future__ import annotations

import sqlite3
from pathlib import Path

import numpy as np

CONFIRMS = (1.5, 2.0, 3.0)
TRAILS = (1.5, 2.0, 3.0, 4.0)
STOP_PCT = 2.5
COSTS_PCT = 0.10
MAX_BARS = 26  # a swing trade is closed at the end of the session at the latest
MIN_ENTRIES = 5


def quote_spread_pct(quote: dict | None) -> float:
    """Return the current two-sided Moomoo spread as a percent of midpoint."""
    try:
        bid, ask = float((quote or {}).get("bid") or 0), float((quote or {}).get("ask") or 0)
        if bid > 0 and ask >= bid:
            return (ask - bid) / ((ask + bid) / 2.0) * 100.0
    except (TypeError, ValueError):
        pass
    return 0.0


def apply_spread_floor(terms: dict, spread_pct: float) -> dict:
    """Keep symbol-derived brackets wider than the live symbol's spread."""
    if not terms or spread_pct <= 0:
        return terms
    floor = spread_pct * 1.25
    if floor > 6.0:
        return {}
    result = dict(terms)
    result["stop_pct"] = round(min(6.0, max(float(result.get("stop_pct", 0) or 0), floor)), 3)
    result["trail_pct"] = round(min(6.0, max(float(result.get("trail_pct", 0) or 0), floor)), 3)
    result["spread_floor_pct"] = round(floor, 4)
    return result


def trade_swings(highs, lows, closes, session_ends, confirm: float, trail: float, stop: float = STOP_PCT, max_bars: int = MAX_BARS) -> list[tuple[float, float, int]]:
    """Every swing trade of one series: (net return %, best run-up %, bars held). ``session_ends[i]`` is True on the last bar of a session."""
    trades: list[tuple[float, float, int]] = []
    low_since = lows[0]
    i, n = 1, len(closes)
    while i < n:
        low_since = min(low_since, lows[i])
        if closes[i] >= low_since * (1 + confirm / 100) and not session_ends[i]:
            entry = closes[i]
            peak = entry
            held = 0
            exit_return = None
            j = i + 1
            while j < n:
                held += 1
                if lows[j] <= entry * (1 - stop / 100):
                    exit_return = (min(closes[j - 1], entry * (1 - stop / 100)) / entry - 1) * 100
                    break
                peak = max(peak, highs[j])
                if peak > entry and lows[j] <= peak * (1 - trail / 100):
                    exit_return = (peak * (1 - trail / 100) / entry - 1) * 100
                    break
                if session_ends[j] or held >= max_bars:
                    exit_return = (closes[j] / entry - 1) * 100
                    break
                j += 1
            if exit_return is None:
                break
            trades.append((exit_return - COSTS_PCT, (peak / entry - 1) * 100, held))
            low_since = lows[min(j, n - 1)]
            i = j + 1
            continue
        i += 1
    return trades


def profile_from_bars(bars: list[tuple], bar_minutes: int = 15) -> dict:
    """The swing profile of one symbol from rows ``(ts_ms, open, high, low, close, volume)`` in time order."""
    if len(bars) < 60:
        return {}
    highs = np.array([b[2] for b in bars], dtype=float)
    lows = np.array([b[3] for b in bars], dtype=float)
    closes = np.array([b[4] for b in bars], dtype=float)
    days = [b[0] // 86_400_000 for b in bars]  # UTC day: a regular session never crosses UTC midnight
    ends = [i == len(bars) - 1 or days[i + 1] != days[i] for i in range(len(bars))]
    table = {}
    for confirm in CONFIRMS:
        for trail in TRAILS:
            trades = trade_swings(highs, lows, closes, ends, confirm, trail, max_bars=390 // bar_minutes)
            if len(trades) < MIN_ENTRIES:
                continue
            returns = np.array([t[0] for t in trades])
            table[f"confirm{confirm:g}_trail{trail:g}"] = {
                "entries": len(trades), "mean_net_pct": round(float(returns.mean()), 3), "median_net_pct": round(float(np.median(returns)), 3),
                "win_rate": round(float((returns > 0).mean()), 3), "mean_run_up_pct": round(float(np.mean([t[1] for t in trades])), 2),
                "median_bars_held": int(np.median([t[2] for t in trades])),
            }
    if not table:
        return {}
    best = max(table, key=lambda k: table[k]["mean_net_pct"])
    return {"bars": len(bars), "best": {"rule": best, **table[best]}, "table": table}


def symbol_profile(db_path: Path, ticker: str) -> dict:
    """The profile of one Moomoo ticker without the US. prefix."""
    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=30)
    try:
        rows = db.execute("SELECT ts, open, high, low, close, volume FROM bars WHERE symbol=? AND source='moomoo' AND volume>0 ORDER BY ts", (ticker,)).fetchall()
    finally:
        db.close()
    return profile_from_bars(rows)


def history_for_plan(db_path: Path, ticker: str, bar_minutes: int = 15, before_ms: int | None = None) -> dict:
    """Everything the stored bars say about one symbol's size of moves, for choosing its stop, trailing stop and profit target: the usual range of a 1-minute bar and of a day, how far
    the price usually runs from the open and falls back from the high, and its own swing results per trailing stop. {} when there is too little history."""
    db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True, timeout=30)
    try:
        # Calibrate only on earlier completed UTC sessions, never today's forming bars.
        cutoff = before_ms // 86_400_000 * 86_400_000 if before_ms is not None else 9_223_372_036_854_775_807
        rows = db.execute("SELECT ts, open, high, low, close, volume FROM bars WHERE symbol=? AND source='moomoo' AND volume>0 AND ts<? ORDER BY ts", (ticker, cutoff)).fetchall()
    finally:
        db.close()
    if not rows:
        return {}
    bar_range = [(r[2] / r[3] - 1) * 100 for r in rows if r[3] > 0]
    days: dict[int, list[tuple]] = {}
    for r in rows:
        days.setdefault(r[0] // 86_400_000, []).append(r)
    day_range, run_up, give_back = [], [], []
    for bars in days.values():
        if len(bars) < 8:
            continue
        open_, high, low, close = bars[0][1], max(b[2] for b in bars), min(b[3] for b in bars), bars[-1][4]
        if open_ <= 0 or low <= 0 or high <= 0:
            continue
        day_range.append((high / low - 1) * 100)
        run_up.append((high / open_ - 1) * 100)
        give_back.append((1 - close / high) * 100)
    if not day_range:
        # A candidate can have only a few prior Moomoo bars. Keep the estimate
        # symbol-specific; callers can supplement it with the candidate's live
        # Moomoo candles if there is not enough earlier history.
        valid_ranges = [x for x in bar_range if np.isfinite(x) and x > 0]
        if len(valid_ranges) < 5:
            return {}
        med_bar = float(np.median(valid_ranges))
        return {
            "bars": len(rows), "sessions": 0,
            "median_bar_range_pct": round(med_bar, 4),
            "p90_bar_range_pct": round(float(np.percentile(valid_ranges, 90)), 4),
            "median_price": round(float(np.median([r[4] for r in rows if r[4] > 0])), 6),
            "median_day_range_pct": 0.0, "median_run_up_from_open_pct": 0.0,
            "p75_run_up_from_open_pct": 0.0, "median_give_back_from_high_pct": 0.0,
            "best_swing_rules": {}, "calibrated_stop_pct": None,
            "calibrated_take_profit_pct": None,
        }
    profile = profile_from_bars([(r[0], r[1], r[2], r[3], r[4], r[5]) for r in rows], bar_minutes=bar_minutes)
    table = profile.get("table", {})
    compact = {k: {"entries": v["entries"], "mean_net_pct": v["mean_net_pct"], "win_rate": v["win_rate"], "mean_run_up_pct": v["mean_run_up_pct"]} for k, v in
               sorted(table.items(), key=lambda kv: -kv[1]["mean_net_pct"])[:4]}
    # Systematic intraday optimization across the symbol's own historical sessions
    session_list = [bars for bars in days.values() if len(bars) >= 8]
    med_bar = float(np.median(bar_range))
    median_price = float(np.median([r[4] for r in rows if r[4] > 0]))
    tick_floor = 0.01 / median_price * 100.0 if median_price > 0 else 0.0
    run_up_reference = float(np.percentile(run_up, 75)) if run_up else med_bar * 3.0
    best_stop = min(6.0, max(tick_floor, round(1.5 * med_bar, 3)))
    best_tp = min(15.0, max(best_stop * 1.5, round(0.75 * run_up_reference, 3)))
    if len(session_list) >= 10:
        best_ev = -999.0
        stop_candidates = sorted({round(min(6.0, max(tick_floor, s_mult * med_bar)), 3)
                                  for s_mult in (0.75, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0)})
        target_multipliers = (0.40, 0.60, 0.80, 1.0, 1.25, 1.5)
        for candidate_stop in stop_candidates:
            target_candidates = sorted({round(min(15.0, max(candidate_stop * 1.5,
                multiplier * run_up_reference)), 3) for multiplier in target_multipliers})
            for candidate_tp in target_candidates:
                pnl_sum = 0.0
                for bars in session_list:
                    entry = bars[0][1]
                    high = entry
                    exit_p = None
                    for b in bars:
                        if b[2] > high:
                            high = b[2]
                        if (b[2] - entry) / entry * 100 >= candidate_tp:
                            exit_p = entry * (1 + candidate_tp / 100.0)
                            break
                        if (b[3] - entry) / entry * 100 <= -candidate_stop:
                            exit_p = entry * (1 - candidate_stop / 100.0)
                            break
                    if exit_p is None:
                        exit_p = bars[-1][4]
                    pnl_sum += (exit_p - entry) / entry * 100.0 - 0.05
                mean_pnl = pnl_sum / len(session_list)
                if mean_pnl > best_ev:
                    best_ev = mean_pnl
                    best_stop, best_tp = candidate_stop, candidate_tp

    return {
        "bars": len(rows), "sessions": len(day_range),
        "median_price": round(float(np.median([r[4] for r in rows if r[4] > 0])), 6),
        "median_bar_range_pct": round(float(np.median(bar_range)), 2), "p90_bar_range_pct": round(float(np.percentile(bar_range, 90)), 2),
        "median_day_range_pct": round(float(np.median(day_range)), 2), "median_run_up_from_open_pct": round(float(np.median(run_up)), 2),
        "p75_run_up_from_open_pct": round(float(np.percentile(run_up, 75)), 2), "median_give_back_from_high_pct": round(float(np.median(give_back)), 2),
        "best_swing_rules": compact,
        "calibrated_stop_pct": best_stop,
        "calibrated_take_profit_pct": best_tp,
    }


def symbol_volatility_terms(bars: list[tuple], spread_pct: float = 0.0) -> dict:
    """Estimate protective distances from this symbol's Moomoo OHLC bars."""
    ranges = []
    advances = []
    closes = []
    for row in bars or []:
        try:
            high, low, close = float(row[2]), float(row[3]), float(row[4])
        except (IndexError, TypeError, ValueError):
            continue
        if low <= 0 or close <= 0 or high < low:
            continue
        closes.append(close)
        ranges.append((high / low - 1.0) * 100.0)
        if len(row) > 1:
            try:
                open_price = float(row[1])
                if open_price > 0:
                    advances.append(max(0.0, (high / open_price - 1.0) * 100.0))
            except (TypeError, ValueError):
                pass
    ranges = [x for x in ranges if np.isfinite(x) and x > 0]
    if len(ranges) < 5:
        return {}
    p75_range = float(np.percentile(ranges, 75))
    p90_range = float(np.percentile(ranges, 90))
    median_range = float(np.median(ranges))
    # These are volatility multiples with broad safety bounds, not fallback
    # percentages. Every resulting value scales with the candidate's own bars.
    tick_floor = 0.01 / float(np.median(closes)) * 100.0 if closes else 0.0
    microstructure_floor = max(tick_floor, max(0.0, float(spread_pct)) * 1.25)
    if microstructure_floor > 6.0:
        return {}
    stop = min(6.0, max(microstructure_floor, 1.5 * p90_range))
    trail = min(6.0, max(microstructure_floor, 1.5 * p75_range, median_range))
    target_basis = float(np.percentile(advances, 75)) if advances else p75_range
    target = min(15.0, max(stop * 1.5, target_basis * 3.0))
    return {"stop_pct": round(stop, 3), "trail_pct": round(trail, 3),
            "take_profit_pct": round(target, 3), "volatility_bar_count": len(ranges),
            "minimum_pct": round(microstructure_floor, 4),
            "volatility_source": "symbol_moomoo_ohlc"}


def default_terms(history: dict, strategy_override: dict | None = None, current_bars: list[tuple] | None = None, spread_pct: float = 0.0) -> dict:
    """Build this symbol's brackets from its own Moomoo history and candles."""
    history = history or {}
    if not np.isfinite(float(spread_pct)) or float(spread_pct) < 0 or float(spread_pct) * 1.25 > 6.0:
        return {}
    bar = float(history.get("median_bar_range_pct") or 0.0)
    symbol_terms = symbol_volatility_terms(current_bars or [], spread_pct=spread_pct)
    if bar <= 0 and history.get("p90_bar_range_pct"):
        bar = float(history["p90_bar_range_pct"])
    if bar <= 0 and symbol_terms:
        bar = float(symbol_terms["stop_pct"]) / 1.5
    if bar <= 0:
        return {}
    historical_tick = 0.01 / float(history["median_price"]) * 100.0 if float(history.get("median_price") or 0)>0 else 0.0
    minimum_pct = max(historical_tick, max(0.0,float(spread_pct))*1.25, float(symbol_terms.get("minimum_pct",0) or 0))

    # Prefer a per-symbol walk-forward result when enough prior sessions exist;
    # otherwise scale each distance from that symbol's measured bar movement.
    stop = min(6.0, max(minimum_pct, float(history.get("calibrated_stop_pct") or symbol_terms.get("stop_pct") or min(6.0, 1.5 * bar))))
    give_back = float(history.get("median_give_back_from_high_pct") or 0.0)
    trail = min(6.0, max(minimum_pct, float(symbol_terms.get("trail_pct") or min(6.0, give_back if give_back > 0 else 1.5 * bar))))
    target = history.get("calibrated_take_profit_pct") or symbol_terms.get("take_profit_pct") or min(15.0, max(float(stop) * 1.5, round(3.0 * bar, 3)))
    return {
        "stop_pct": round(float(stop), 3), "trail_pct": round(float(trail), 3),
        "take_profit_pct": round(float(target), 3), "hold_minutes": 360.0,
        "hard_tp": True,
        "calibrated_from_symbol_history": bool(history.get("calibrated_stop_pct") and history.get("calibrated_take_profit_pct")),
        "volatility_bar_count": symbol_terms.get("volatility_bar_count", history.get("bars", 0)),
        "minimum_pct": round(minimum_pct, 4),
        "volatility_source": symbol_terms.get("volatility_source", "symbol_moomoo_history"),
    }



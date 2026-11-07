"""Causal, normalized Moomoo-only market brief for live exit decisions."""
from __future__ import annotations

import datetime as dt
import math
import sqlite3
from contextlib import closing
from pathlib import Path
from zoneinfo import ZoneInfo

NY = ZoneInfo("America/New_York")


def _utc(ms):
    if ms is None:
        return None
    return dt.datetime.fromtimestamp(ms / 1000, dt.timezone.utc).isoformat(timespec="milliseconds")


def _n(value, digits=4):
    try:
        value = float(value)
        return round(value, digits) if math.isfinite(value) else None
    except (TypeError, ValueError):
        return None


def read_moomoo_closed_minutes(path: Path, symbol: str, as_of_ms: int, limit: int = 1000):
    """Return only closed, current-ET-session candles explicitly sourced from Moomoo."""
    local_day = dt.datetime.fromtimestamp(as_of_ms / 1000, NY).date()
    session_start = int(dt.datetime.combine(local_day, dt.time(4), NY).timestamp() * 1000)
    ticker = symbol.removeprefix("US.")
    try:
        with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=2)) as db:
            rows = db.execute(
                "SELECT ts,open,high,low,close,volume FROM bars "
                "WHERE symbol IN (?,?) AND source='moomoo' AND ts>=? AND ts<=? "
                "UNION ALL SELECT ts,open,high,low,close,volume FROM premarket_bars "
                "WHERE symbol IN (?,?) AND source='moomoo' AND ts>=? AND ts<=? "
                "ORDER BY ts DESC LIMIT ?",
                (ticker, "US." + ticker, session_start, as_of_ms - 60_000,
                 ticker, "US." + ticker, session_start, as_of_ms - 60_000, limit),
            ).fetchall()
    except (sqlite3.Error, OSError):
        return [], "Moomoo candle store unavailable"
    from .candlestore import acceptable
    unique = {int(row[0]): row for row in rows}
    bars = [row for row in sorted(unique.values()) if acceptable(*row, bar_ms=60_000)]
    bars = bars[-limit:]
    if not bars:
        return [], "no closed Moomoo candles for the current ET session"
    if as_of_ms - int(bars[-1][0]) > 300_000:
        return [], "latest closed Moomoo candle is over five minutes old"
    return bars, None


def _market_context(context, as_of_ms):
    result = {}
    for symbol in ("US.SPY", "US.QQQ"):
        item = (context or {}).get(symbol)
        if not item:
            result[symbol] = {"status": "missing", "reason": "no Moomoo quote push received"}
            continue
        values = item.get("values") or {}
        received = item.get("received_ms")
        prior = _n(values.get("prev_close_price"))
        pre = _n(values.get("pre_price"))
        last = _n(values.get("last_price"))
        age = max(0, as_of_ms - received) if isinstance(received, int) else None
        result[symbol] = {
            "source": "Moomoo StockQuote push",
            "received_utc": _utc(received),
            "local_receipt_age_ms": age,
            "provider_trade_time_utc": _utc(item.get("provider_ms")),
            "last_regular_trade": last,
            "premarket_price": pre,
            "previous_close": prior,
            "premarket_vs_previous_close_pct": _n((pre / prior - 1) * 100, 3) if pre and prior else None,
            "reported_regular_volume": _n(values.get("volume"), 0),
            "reported_premarket_volume": _n(values.get("pre_volume"), 0),
            "status": "fresh push" if age is not None and age <= 60_000 else "stale push" if age is not None else "timestamp unavailable",
            "live_order_book": ({**item["live_order_book"],
                "received_utc": _utc(item["live_order_book"].get("received_ms")),
                "provider_time_utc": _utc(item["live_order_book"].get("provider_ms"))}
                if item.get("live_order_book") else None),
        }
    return result


def build_market_brief(symbol, position, quote, bars, market_context, as_of_ms, missing_reason=None, strategy_context=None):
    """Build the identical compact schema used before and after model inference."""
    entry = _n((position or {}).get("entry"))
    bid, ask = _n(getattr(quote, "bid", None)), _n(getattr(quote, "ask", None))
    received = getattr(quote, "received_ms", None)
    provider = getattr(quote, "provider_ms", None)
    valid_book = bid is not None and ask is not None and bid > 0 and ask >= bid
    candles = []
    for row in bars[-12:]:
        ts, opn, high, low, close, volume = row[:6]
        candles.append({"start_utc": _utc(int(ts)), "open": _n(opn), "high": _n(high),
                        "low": _n(low), "close": _n(close), "volume": _n(volume, 0)})
    issues = []
    if not valid_book:
        issues.append("executable bid/ask unavailable; candle-based analysis remains usable, but EXIT execution is blocked")
    if missing_reason:
        issues.append(missing_reason)
    context_brief = _market_context(market_context, as_of_ms)
    issues.extend(f"{name} Moomoo benchmark context is {item['status']}" for name, item in context_brief.items()
                  if item.get("status") != "fresh push")
    issues.extend(f"{name} benchmark order book is {item['live_order_book']['status']}"
                  for name, item in context_brief.items() if item.get("live_order_book")
                  and item["live_order_book"].get("status") != "fresh push")
    current_bid_pnl = (bid / entry - 1) * 100 if bid and entry else None
    vwap_den = sum(float(b[5]) for b in bars if float(b[5]) > 0)
    vwap = sum(float(b[4]) * float(b[5]) for b in bars if float(b[5]) > 0) / vwap_den if vwap_den else None
    close_values = [float(b[4]) for b in bars]
    returns = {}
    for count in (1, 3, 5):
        if len(close_values) > count and close_values[-count-1] > 0:
            returns[f"{count}_bar_close_return_pct"] = _n((close_values[-1] / close_values[-count-1] - 1) * 100, 3)
    volume_signal = None
    if len(bars) >= 6:
        baseline = sum(float(b[5]) for b in bars[-6:-3]) / 3
        recent = sum(float(b[5]) for b in bars[-3:]) / 3
        volume_signal = _n(recent / baseline, 3) if baseline > 0 else None
    technical = {}
    if bars:
        highs = [float(b[2]) for b in bars]
        lows = [float(b[3]) for b in bars]
        last_close = float(bars[-1][4])
        session_high, session_low = max(highs), min(lows)
        technical.update({
            "session_high": _n(session_high), "session_low": _n(session_low),
            "bid_drawdown_from_session_high_pct": _n((bid / session_high - 1) * 100, 3) if bid and session_high else None,
            "close_location_in_session_range_pct": _n((last_close - session_low) / (session_high - session_low) * 100, 1) if session_high > session_low else None,
        })
        closes = [float(b[4]) for b in bars]
        for window in (5, 12):
            if len(closes) >= window:
                technical[f"sma_{window}_close"] = _n(sum(closes[-window:]) / window)
        if len(closes) >= 12:
            sma5, sma12 = sum(closes[-5:]) / 5, sum(closes[-12:]) / 12
            technical["sma5_vs_sma12_pct"] = _n((sma5 / sma12 - 1) * 100, 3) if sma12 else None
        if len(closes) >= 6:
            prior_3 = closes[-6:-3]
            recent_3 = closes[-3:]
            technical["recent_3bar_vs_prior_3bar_pct"] = _n((sum(recent_3) / 3 / (sum(prior_3) / 3) - 1) * 100, 3) if sum(prior_3) else None
        if vwap:
            technical["last_close_vs_session_vwap_pct"] = _n((last_close / vwap - 1) * 100, 3)
            earlier_bars = bars[:-3]
            earlier_den = sum(float(b[5]) for b in earlier_bars if float(b[5]) > 0)
            earlier_vwap = (sum(float(b[4]) * float(b[5]) for b in earlier_bars if float(b[5]) > 0) / earlier_den
                            if earlier_den else None)
            technical["session_vwap_slope_last_3bar_pct"] = _n((vwap / earlier_vwap - 1) * 100, 3) if earlier_vwap else None
        if len(bars) >= 6:
            prior_volumes = [float(b[5]) for b in bars[-6:-3]]
            recent_volumes = [float(b[5]) for b in bars[-3:]]
            down_volume = sum(float(b[5]) for b in bars[-3:] if float(b[4]) < float(b[1]))
            up_volume = sum(float(b[5]) for b in bars[-3:] if float(b[4]) > float(b[1]))
            technical["recent_3bar_down_volume_share"] = _n(down_volume / sum(recent_volumes), 3) if sum(recent_volumes) else None
            technical["recent_3bar_up_volume_share"] = _n(up_volume / sum(recent_volumes), 3) if sum(recent_volumes) else None
            technical["recent_3bar_volume_vs_prior_3bar"] = _n(sum(recent_volumes) / sum(prior_volumes), 3) if sum(prior_volumes) else None
        true_ranges = []
        for index, bar in enumerate(bars):
            high, low = float(bar[2]), float(bar[3])
            previous_close = float(bars[index - 1][4]) if index else float(bar[4])
            true_ranges.append(max(high - low, abs(high - previous_close), abs(low - previous_close)))
        if true_ranges:
            atr_count = min(5, len(true_ranges))
            technical["atr_5_price"] = _n(sum(true_ranges[-atr_count:]) / atr_count)
            technical["atr_5_pct_of_close"] = _n(sum(true_ranges[-atr_count:]) / atr_count / last_close * 100, 3) if last_close else None
        if len(bars) >= 5:
            flow_volume = total_volume = 0.0
            for bar in bars[-5:]:
                opn, high, low, close, volume = map(float, bar[1:6])
                total_volume += volume
                flow_volume += ((2 * close - high - low) / (high - low) * volume) if high > low else 0.0
            technical["last_5bar_volume_flow"] = _n(flow_volume / total_volume, 3) if total_volume else None
    structure = None
    if bars:
        _, opn, high, low, close, _ = bars[-1]
        span = float(high) - float(low)
        structure = {
            "latest_closed_candle_utc": _utc(int(bars[-1][0])),
            "body_pct_of_range": _n(abs(float(close) - float(opn)) / span * 100, 1) if span > 0 else None,
            "close_location_in_range_pct": _n((float(close) - float(low)) / span * 100, 1) if span > 0 else None,
            "close_vs_session_vwap_pct": _n((float(close) / vwap - 1) * 100, 3) if vwap else None,
        }
    return {
        "schema": "live_market_brief_v1",
        "as_of_utc": _utc(as_of_ms),
        "symbol": symbol,
        "position": {"quantity": (position or {}).get("qty"),
                     "available_quantity": (position or {}).get("available_qty"),
                     "entry_price": entry, "opened_utc": _utc((position or {}).get("opened_ms")),
                     "unrealized_at_executable_bid_pct": _n(current_bid_pnl, 3)},
        "executable_book": {"purpose": "execution validation only; not a strategy signal", "source": "live Moomoo order book", "live_push_or_read": bool(getattr(quote, "live_data", False)),
            "bid": bid, "ask": ask, "spread": _n(ask - bid) if valid_book else None,
            "spread_bps": _n((ask - bid) / ((ask + bid) / 2) * 10000, 2) if valid_book else None,
            "received_utc": _utc(received), "local_receipt_age_ms": max(0, as_of_ms - received) if isinstance(received, int) else None,
            "provider_book_time_utc": _utc(provider), "provider_time_is_execution_freshness": False},
        "current_session_candles": {"source": "Moomoo only", "interval": "1 minute", "closed_count_available": len(bars),
            "session_vwap": _n(vwap), "latest_closed_candle": structure,
            "recent_close_returns": returns, "recent_to_prior_3bar_average_volume_ratio": volume_signal,
            "derived_structure": technical,
            "candles_oldest_first": candles},
        "market_context": context_brief,
        "strategy_context": strategy_context or {"strategy": "intraday_gainer"},
        "data_quality": {"missing_or_caveats": issues,
            "note": "Strategy decisions use current-session closed Moomoo candles and derived trend/volume features. Order book is only an execution check. No legacy provider or snapshot values included."},
    }

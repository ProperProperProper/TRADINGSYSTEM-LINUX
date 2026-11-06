"""Signal arithmetic for the momentum scanner, kept free of any network or broker code so it can be tested."""

from __future__ import annotations

import math
from datetime import datetime, time as clock_time, timedelta, timezone
from statistics import fmean
from zoneinfo import ZoneInfo


from datetime import date

REGULAR_CLOSE = clock_time(16, 0)
EARLY_CLOSES: dict[date, clock_time] = {date(2026, 11, 27): clock_time(13, 0), date(2026, 12, 24): clock_time(13, 0)}  # NYSE half days; the others are holidays (no bars at all)


def session_covers(day: date, last_time: clock_time) -> bool:
    """Whether the session of ``day`` was still open at ``last_time`` (a bar starting then existed): false on a half day before the time, true otherwise."""
    return last_time < EARLY_CLOSES.get(day, REGULAR_CLOSE)


def time_matched_relative_volume(frame, minimum_sessions: int = 2) -> tuple[float | None, float | None]:
    """Relative volume of the latest session and the previous session's close, from one frame of 1-minute bars.

    Relative volume is today's cumulative volume up to the latest bar divided by the average cumulative volume of the earlier sessions in the frame up to the same
    time of day (a stock trades most at the open, so a whole-day average would flag every morning). A slot in which nothing traded has no bar and counts as zero volume:
    a thin stock is judged like any other, the missing slot is a true "no trade", not a gap in the data. Only a session that was already over at that time (a half day)
    is left out. Returns ``(None, previous_close)`` when fewer than ``minimum_sessions`` earlier sessions are comparable, and ``(None, None)`` for an empty frame. The
    latest bar may still be in progress; its partial volume makes the figure conservative, never inflated.
    """
    if frame is None or len(frame) == 0:
        return None, None
    index = frame.index
    if getattr(index, "tz", None) is not None:
        index = index.tz_convert("America/New_York")
    days = index.date
    ordered = sorted(set(days))
    today = ordered[-1]
    today_mask = days == today
    last_time = index[today_mask].time[-1]
    volume = frame["Volume"].to_numpy()
    close = frame["Close"].to_numpy()
    cumulative_today = float(volume[today_mask].sum())
    previous_close = None
    earlier: list[float] = []
    for day in ordered[:-1]:
        mask = days == day
        previous_close = float(close[mask][-1])
        if not session_covers(day, last_time):
            continue
        earlier.append(float(volume[mask][index[mask].time <= last_time].sum()))
    if len(earlier) < minimum_sessions:
        return None, previous_close
    average = fmean(earlier)
    if average <= 0:
        return None, previous_close
    return cumulative_today / average, previous_close


def candidate_tape_activity(bars: list[tuple], now_ms: int, session_start_ms: int,
                            window_bars: int = 15, minimum_observations: int = 5,
                            minimum_trade_share: float = 0.20, max_trade_age_ms: int = 5 * 60_000) -> dict:
    """Block candidate entry while its current-session Moomoo tape is mostly no-trade bars.

    Zero-volume carry-forward candles remain valid chart data, but they do not
    establish tradable momentum. The gate is rolling: later real prints can
    make the same candidate eligible without a manual blacklist reset.
    """
    current = []
    for row in bars or []:
        try:
            ts, volume = int(row[0]), float(row[5])
        except (IndexError, TypeError, ValueError):
            continue
        if session_start_ms <= ts <= now_ms and math.isfinite(volume) and volume >= 0:
            current.append((ts, volume))
    current.sort()
    recent = current[-max(1, int(window_bars)):]
    traded = [(ts, volume) for ts, volume in recent if volume > 0]
    required = max(2, int(math.ceil(len(recent) * float(minimum_trade_share))))
    latest_trade_ms = max((ts for ts, _ in traded), default=None)
    trade_age_ms = now_ms - latest_trade_ms if latest_trade_ms is not None else None
    reason = None
    if len(recent) < minimum_observations:
        reason = f"waiting for {minimum_observations} current-session Moomoo candles ({len(recent)} available)"
    elif len(traded) < required:
        reason = f"thin traded tape: {len(traded)}/{len(recent)} recent candles have volume; need {required}"
    elif trade_age_ms is None or trade_age_ms > max_trade_age_ms:
        reason = f"no Moomoo trade in the last {max_trade_age_ms // 60_000} minutes"
    return {"eligible": reason is None, "reason": reason,
            "session_candle_count": len(current), "recent_candle_count": len(recent),
            "recent_traded_candle_count": len(traded), "required_traded_candles": required,
            "recent_volume": round(sum(v for _, v in recent), 4),
            "latest_trade_ms": latest_trade_ms, "trade_age_ms": trade_age_ms,
            "activity_window_minutes": int(window_bars)}


def nasdaq_listed_symbols(listing) -> frozenset[str]:
    """Moomoo codes (``US.XXXX``) of the ordinary Nasdaq-listed symbols in Nasdaq Trader's ``nasdaqlisted`` table (a frame with ``Symbol`` and ``Test Issue``).

    Test issues are left out, and so are symbols the universe filter would drop anyway (not alphanumeric, more than five characters).
    """
    valid = listing["Test Issue"] == "N"
    if "ETF" in listing:
        valid &= listing["ETF"] == "N"
    if "Security Name" in listing:
        names = listing["Security Name"].fillna("").str.lower()
        valid &= names.str.contains(r"common (?:stock|shares)|ordinary shares", regex=True)
        valid &= ~names.str.contains(r"depositary|depository|\badr\b|warrant|preferred|\bunits?\b|\brights?\b|\betf\b|exchange.traded", regex=True)
    rows = listing[valid]["Symbol"].dropna().astype(str)
    return frozenset(f"US.{symbol}" for symbol in rows if symbol.isalnum() and len(symbol) <= 5)


def may_trade(symbol: str, nasdaq: frozenset[str]) -> bool:
    """Only Nasdaq-listed symbols may be bought. An empty set means nothing can be shown to be Nasdaq, so nothing is traded."""
    return symbol in nasdaq


BAR = timedelta(minutes=1)


def closed_bars(frame, now: datetime | None = None):
    """The frame without any bar that has not finished: a bar starting at ``t`` is final only once ``t + 1 minutes`` has passed.

    The newest bar of a live download is still in progress (partial volume, a close that keeps moving); signals are computed on closed bars only.
    """
    if frame is None or len(frame) == 0:
        return frame
    now = now or datetime.now(timezone.utc)
    index = frame.index
    ends = index + BAR
    if getattr(index, "tz", None) is None:
        ends = ends.tz_localize("America/New_York")
    return frame[ends <= now]


def latest_closed_bar_start(now: datetime) -> datetime:
    """Start of the newest 1-minute bar that has completed at ``now`` (UTC quarter hours are New York quarter hours)."""
    now = now.astimezone(timezone.utc)
    start = now.replace(second=0, microsecond=0)
    return start - BAR


def scan_due(now: datetime, last_scanned: datetime | None, settle_seconds: int = 75) -> datetime | None:
    """The closed bar to scan now, or None. A new scan is due once per bar, ``settle_seconds`` after it closes (time for the data to publish it)."""
    bar = latest_closed_bar_start(now)
    if last_scanned is not None and bar <= last_scanned:
        return None
    if (now - (bar + BAR)).total_seconds() < settle_seconds:
        return None
    return bar


def in_regular_session(now: datetime) -> bool:
    """Weekday 09:30-16:00 New York time (the hours of the 1-minute data the signals use)."""
    local = now.astimezone(ZoneInfo("America/New_York"))
    return local.weekday() < 5 and clock_time(9, 30) <= local.time() < clock_time(16, 0)


def target_notional(total_assets: float, buying_power: float, leverage: float, max_position_pct: float, buffer: float) -> float:
    """Dollars to put into one position: leverage times account EQUITY, never more than the buying power or the per-position cap, less a buffer.

    Equity (not cash) is the base: on a margin account cash goes negative once the account borrows, which used to size every order at zero shares.
    """
    if min(total_assets, buying_power) <= 0:
        return 0.0
    return min(total_assets * leverage, buying_power, total_assets * max_position_pct) * (1.0 - buffer)


def average_daily_volume(frame, sessions: int = 21, today=None, minimum: int | None = None) -> float | None:
    """Mean daily volume over the last ``sessions`` COMPLETE sessions of a 1-minute frame (today's session, still forming, is left out).

    None when fewer than ``minimum`` (default ``sessions``) complete sessions are available, or the mean is not positive.
    """
    if frame is None or len(frame) == 0:
        return None
    index = frame.index
    if getattr(index, "tz", None) is not None:
        index = index.tz_convert("America/New_York")
    days = index.date
    today = today or datetime.now(ZoneInfo("America/New_York")).date()
    volume = frame["Volume"].to_numpy()
    totals = {}
    for day, value in zip(days, volume):
        if day != today:
            totals[day] = totals.get(day, 0.0) + float(value)
    recent = [totals[d] for d in sorted(totals)][-sessions:]
    if len(recent) < (sessions if minimum is None else max(1, minimum)):
        return None
    average = fmean(abs(v) for v in recent)
    return average if average > 0 else None


WORKING_ORDER_STATUSES = frozenset({"WAITING_SUBMIT", "SUBMITTING", "SUBMITTED", "FILLED_PART"})


def order_gate(symbol: str, held: set[str] | frozenset[str], working: set[str] | frozenset[str], open_count: int, max_positions: int,
               price: float, min_price: float) -> str | None:
    """Why a new buy must NOT be placed, or None when it may be.

    A held symbol may receive a fresh add-on entry. It already occupies one
    unique-symbol slot, so only a new symbol consumes another slot. A working
    buy for the same symbol remains a hard duplicate-order veto.
    """
    if symbol in working:
        return "an order for it is already working"
    if symbol not in held and open_count >= max_positions:
        return f"{open_count} positions or working orders already (maximum {max_positions})"
    if price < min_price:
        return f"price {price:.2f} is below the {min_price:.2f} minimum"
    return None


OWN_REMARK = "papercompmoomoo"
PRE_OPEN_REMARK = "papercompmoomoo pre-open plan"
PRE_OPEN_MAX_AGE_SECONDS = 30  # no entry order should remain parked for hours, including a pre-open candidate order


def orders_to_cancel(rows: list[dict], now: datetime, max_age_seconds: int = 300, at_startup: bool = False, keep_codes: set[str] | None = None) -> list[dict]:
    """Working orders this bot should cancel: its own (remark starts with ``papercompmoomoo``) that are stale, or at start-up any it left behind, plus duplicates.

    A market order fills at once in the session, so one still working after ``max_age_seconds`` is stuck (or was placed out of session). At start-up nothing of
    ours can legitimately be working, except the daily plan's pre-market orders: those survive a restart (the plan on disk says what is still to be placed). Of several working orders for the same symbol only the oldest is kept.
    Orders placed by hand (any other remark) are never touched. ``rows`` are dicts with ``order_id``, ``code``, ``remark`` and ``created`` (New York time).
    """
    ny = ZoneInfo("America/New_York")
    own = [r for r in rows if str(r.get("remark", "")).startswith(OWN_REMARK)]
    for r in own:
        created = r["created"]
        r["_age"] = (now - (created if created.tzinfo else created.replace(tzinfo=ny))).total_seconds()
    own.sort(key=lambda r: -r["_age"])
    keep: dict[str, dict] = {}
    cancel: list[dict] = []
    for r in own:
        # a pre-market order of the daily plan legitimately waits for the open for hours; every other order of ours is stale after a few minutes
        limit = PRE_OPEN_MAX_AGE_SECONDS if str(r.get("remark", "")).startswith(PRE_OPEN_REMARK) else max_age_seconds
        stale = (at_startup and limit != PRE_OPEN_MAX_AGE_SECONDS) or r["_age"] > limit
        if at_startup and limit == PRE_OPEN_MAX_AGE_SECONDS and keep_codes is not None and r["code"] not in keep_codes:
            stale = True  # a pre-market order survives a restart only for a candidate of the locked plan
        if stale or r["code"] in keep:
            cancel.append(r)
        else:
            keep[r["code"]] = r
    return cancel


def symbol_profile(frame) -> dict[str, float]:
    """What this one symbol's own recent bars say about how it trades (a frame of closed 1-minute bars, several sessions).

    ``last_bar_volume``: the newest bar. ``typical_bar_volume``: the median volume of the bars at the same time of day on the earlier sessions (what it normally
    trades at this hour). ``median_range``: the median (high - low) / close of its bars, a measure of how far it moves within one bar. Zero when unknown.
    """
    if frame is None or len(frame) < 4:
        return {"last_bar_volume": 0.0, "typical_bar_volume": 0.0, "median_range": 0.0}
    index = frame.index
    if getattr(index, "tz", None) is not None:
        index = index.tz_convert("America/New_York")
    times, days = index.time, index.date
    last_time, last_day = times[-1], days[-1]
    same_hour = [float(v) for v, t, d in zip(frame["Volume"], times, days) if t == last_time and d != last_day]
    ranges = sorted(float(h - l) / float(c) for h, l, c in zip(frame["High"], frame["Low"], frame["Close"]) if c > 0)
    typical = sorted(same_hour)[len(same_hour) // 2] if same_hour else 0.0
    return {"last_bar_volume": float(frame["Volume"].iloc[-1]), "typical_bar_volume": typical,
            "median_range": ranges[len(ranges) // 2] if ranges else 0.0}


def size_order(price: float, notional: float, broker_max: float, profile: dict[str, float], avg_daily_volume: float, equity: float,
               stop_loss_pct: float, risk_per_trade: float = 0.005, bar_participation: float = 0.10,
               daily_participation: float = 0.02) -> tuple[int, str]:
    """Whole shares to buy and which limit decided, worked out for THIS symbol from its own data.

    The smallest of: the money rules (``notional``); the broker's own maximum buy; the stock's own supply, i.e. ``bar_participation`` of the SMALLER of its newest bar
    and its normal bar at this hour (a one-off spike must not inflate the size) and ``daily_participation`` of its average daily volume; and a risk cap, the loss
    budget (``risk_per_trade`` of equity) divided by this symbol's real stop distance, the larger of the configured stop and its own median bar range (a stop inside
    the noise of a volatile stock is hit by chance, so a volatile symbol gets a smaller position). 0 when any limit is zero or unknown.
    """
    if price <= 0 or equity <= 0:
        return 0, "no price or equity"
    bar = min(profile.get("last_bar_volume", 0.0), profile.get("typical_bar_volume", 0.0) or profile.get("last_bar_volume", 0.0))
    stop_distance = max(stop_loss_pct / 100.0, profile.get("median_range", 0.0))
    limits = {
        "money": notional / price,
        "broker maximum": broker_max,
        "this symbol's bar volume": bar * bar_participation,
        "this symbol's average daily volume": avg_daily_volume * daily_participation,
        "risk (loss budget over this symbol's stop distance)": equity * risk_per_trade / (stop_distance * price),
    }
    reason, shares = min(limits.items(), key=lambda kv: kv[1])
    return max(0, int(shares)), reason


def signal_bar_features(frame) -> tuple[float, int, int]:
    """(where the newest bar closed in its range 0..1, its slot in the session 0..25, bars left after it) for a frame of closed 1-minute bars."""
    last = frame.iloc[-1]
    index = frame.index
    local = index[-1].tz_convert("America/New_York") if getattr(index, "tz", None) is not None else index[-1]
    slot = max(0, min(389, ((local.hour * 60 + local.minute) - 570)))
    high, low, close = float(last["High"]), float(last["Low"]), float(last["Close"])
    return ((close - low) / (high - low) if high > low else 0.5), int(slot), int(389 - slot)


def filter_failed(close_pos: float, slot: int, bars_left: int, min_close_pos: float, skip_first_slots: int, min_bars_left: int) -> str | None:
    """Which strategy filter rejects a signal bar, or None when it passes them all (a filter set to 0 is off)."""
    if close_pos < min_close_pos:
        return f"closed low in its range ({close_pos:.2f} < {min_close_pos:.2f})"
    if slot < skip_first_slots:
        return f"bar {slot} of the session is in the first {skip_first_slots} bars"
    if bars_left < min_bars_left:
        return f"only {bars_left} bars left in the session (needs {min_bars_left})"
    return None


def entry_gate(signal_price: float, live: dict | None, max_chase_pct: float, max_spread_pct: float) -> str | None:
    """Why NOT to buy right now given Moomoo's live quote, or None. A market order fills at the live price: if it already ran away from the signal, or the spread is wide,
    the replayed edge is gone (the first live order filled 3.5% above its signal)."""
    if not live or live.get("last", 0.0) <= 0:
        return "no live Moomoo quote"
    last, bid, ask = live["last"], live.get("bid", 0.0), live.get("ask", 0.0)
    fill = ask if ask > 0 else last
    if signal_price > 0 and (fill / signal_price - 1.0) * 100.0 > max_chase_pct:
        return f"price already {(fill / signal_price - 1.0) * 100.0:+.2f}% from the signal (limit {max_chase_pct:.2f}%)"
    if bid > 0 and ask > 0 and (ask / bid - 1.0) * 100.0 > max_spread_pct:
        return f"spread {(ask / bid - 1.0) * 100.0:.2f}% is wider than {max_spread_pct:.2f}%"
    return None

"""Closed native one-minute evidence for gainer admissions; never resamples 1m bars."""
from __future__ import annotations

import math
import sqlite3
from contextlib import closing
from pathlib import Path

MINUTE_DB = Path("data/common1m.sqlite3")
MIN_BARS = 25


def ticker_of(symbol: str) -> str:
    return symbol.removeprefix("US.")


def write_minutes(path: Path, symbol: str, frame) -> int:
    """Persist native Moomoo bars for identified movers. Keep real, grid-aligned, internally consistent bars."""
    path.parent.mkdir(parents=True, exist_ok=True)
    rows = []
    for at, r in frame.iterrows():
        ts = int(at.timestamp() * 1000)
        o, h, low, c, v = (float(r[k]) for k in ("Open", "High", "Low", "Close", "Volume"))
        if ts % 60_000 or not all(math.isfinite(x) for x in (o, h, low, c, v)):
            continue
        if not (0 < low <= min(o, c) <= max(o, c) <= h and (v > 0 or v == 0 and o == h == low == c)):
            continue
        rows.append((ticker_of(symbol), ts, o, h, low, c, v))
    from .candlestore import CandleStore
    store = CandleStore(path, bar_ms=60_000)
    try:
        with store.db:
            store.db.executemany("INSERT INTO bars(symbol,ts,open,high,low,close,volume,source,vol_m) VALUES(?,?,?,?,?,?,?,'moomoo',?) ON CONFLICT(symbol,ts) DO UPDATE SET open=excluded.open,high=excluded.high,low=excluded.low,close=excluded.close,volume=excluded.volume,source='moomoo',vol_m=excluded.vol_m", [(*row, row[-1]) for row in rows])
    finally:
        store.close()
    return len(rows)


def capture_candidate_minutes(feed, symbols: list[str], path: Path | None = None) -> int:
    """Fetch native one-minute candles only for the supplied Moomoo candidates."""
    codes = [f"US.{str(symbol).removeprefix('US.')}" for symbol in dict.fromkeys(symbols)]
    target = path or MINUTE_DB
    feed.release_unused(set(codes))
    available = feed.ensure(codes)
    written = 0
    for code in codes:
        if code in available:
            frame = feed.candles(code)
            if frame is not None:
                written += write_minutes(target, code, frame)
    return written


def read_minutes(path: Path, symbol: str, now_ms: int) -> list[tuple]:
    """Read closed one-minute bars; deduplicate raw/qualified aliases."""
    try:
        with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as db:
            rows = db.execute(
                "SELECT ts, open, high, low, close, volume FROM bars WHERE symbol IN (?,?) AND ts <= ? "
                "UNION ALL SELECT ts, open, high, low, close, volume FROM premarket_bars WHERE symbol IN (?,?) AND ts <= ? "
                "ORDER BY ts DESC LIMIT 100",
                (ticker_of(symbol), "US." + ticker_of(symbol), now_ms - 60_000, ticker_of(symbol), "US." + ticker_of(symbol), now_ms - 60_000),
            ).fetchall()
    except sqlite3.Error:
        return []
    by_ts = {r[0]: r for r in rows}
    from .candlestore import acceptable
    bars = [b for b in sorted(by_ts.values()) if acceptable(*b, bar_ms=60_000)][-MIN_BARS:]
    if not bars or now_ms - bars[-1][0] > 300_000:
        return []
    return bars


def kalman_motion(values: list[float]) -> tuple[float, float]:
    """Two-state Kalman filter (level, velocity), unit minute time step."""
    level, velocity = values[0], 0.0
    p00, p01, p11 = 1.0, 0.0, 1.0
    acceleration = 0.0
    for value in values[1:]:
        level += velocity
        a, b, d = p00 + 2 * p01 + p11 + 0.01, p01 + p11, p11 + 0.01
        innovation = value - level
        k0, k1 = a / (a + 0.1), b / (a + 0.1)
        level += k0 * innovation
        acceleration = k1 * innovation
        velocity += acceleration
        p00, p01, p11 = (1 - k0) * a, (1 - k0) * b, d - k1 * b
    return velocity, acceleration


def features(bars: list[tuple], rvol: float, bid: float, ask: float) -> dict[str, float]:
    closes = [b[4] for b in bars]
    clv = [(2*b[4] - b[2] - b[3]) / (b[2] - b[3]) if b[2] > b[3] else 0.0 for b in bars]
    velocity, acceleration = kalman_motion([c / closes[0] - 1 for c in closes])
    clv_velocity, clv_acceleration = kalman_motion(clv)
    spread = (ask - bid) / ((ask + bid) / 2) if 0 < bid <= ask else 1.0
    clamp = lambda x: max(0.0, min(1.0, x))
    return {
        "node:sensor:rvol_surge": clamp(rvol / 4),
        "node:sensor:morning_thrust": clamp((closes[-1]/closes[0] - 1) / 0.05),
        "node:sensor:regime_trend": clamp(0.5 + velocity * 100),
        "node:sensor:vol_contraction": clamp(1 - (bars[-1][2] - bars[-1][3]) / closes[-1] / 0.02),
        "node:sensor:spread_drag": clamp(spread / 0.01),
        "price_velocity": velocity, "price_acceleration": acceleration,
        "clv_velocity": clv_velocity, "clv_acceleration": clv_acceleration,
    }


def forward_pass(path: Path, sensors: dict[str, float]) -> tuple[float, float]:
    """Candidate-specific sigmoid network, with defense isolated from decision nodes.

    Unknown/unconnected features carry zero evidence. Cycles, invalid weights and
    missing decision/defense paths fail closed rather than inventing conviction.
    """
    with closing(sqlite3.connect(f"file:{path}?mode=ro", uri=True)) as db:
        edges = db.execute("SELECT source_id, target_id, weight FROM brain_synapses").fetchall()
    def evaluate(target: str, defense: bool, visiting: frozenset[str]) -> float:
        if defense and target.startswith(("node:decision:", "node:outcome:")):
            return 0.0
        if target in visiting:
            raise ValueError("cyclic connectome")
        if target in sensors:
            return sensors[target]
        incoming = [(src, float(w)) for src, dst, w in edges if dst == target]
        if not incoming:
            return 0.0
        if not all(math.isfinite(w) for _, w in incoming):
            raise ValueError("invalid synaptic weight")
        z = sum(w * evaluate(src, defense, visiting | {target}) for src, w in incoming)
        return 1 / (1 + math.exp(-max(-60.0, min(60.0, z))))
    conviction = evaluate("node:decision:rank1_execute", False, frozenset())
    bear = evaluate("node:reflex:adversarial_bear_sentry", True, frozenset())
    if conviction == 0 or bear == 0:
        raise ValueError("missing entry or defense pathway")
    from .decision_audit import record
    record(path, "connectome_forward", {"sensors": sensors, "edges": edges},
           {"conviction": conviction, "bear": bear})
    return conviction, bear


def confirm_candidate(bars: list[tuple], rvol: float, bid: float, ask: float, brain_path: Path) -> dict:
    """Shared causal momentum confirmation used by both live paper bots."""
    sensed = features(bars, rvol, bid, ask)
    if not any(float(b[5])>0 for b in bars):
        return {"reason":"current Moomoo candles contain no traded volume; price/CLV inflection is unavailable",
                "features":sensed,"evidence_mode":"closed_1m_no_volume"}
    if len(bars) >= 2 and (
        sensed["price_velocity"] <= 0
        or sensed["price_acceleration"] < 0
        or sensed["clv_acceleration"] < 0
    ):
        return {"reason": "price/CLV Kalman inflection not confirmed", "features": sensed}
    try:
        conviction, bear = forward_pass(brain_path, sensed)
    except (sqlite3.Error, ValueError, TypeError, OSError):
        return {"reason": "candidate connectome evidence unavailable", "features": sensed}
    from . import gating
    return {"reason": gating.tier3_connectome(conviction, bear), "conviction": conviction,
            "bear": bear, "features": sensed}

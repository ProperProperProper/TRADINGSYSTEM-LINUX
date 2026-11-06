"""Identify, capture, and reverse-engineer algorithmic trading on market for candidates.

Pulls all available live microstructure intelligence from Moomoo OpenD:
1. ``get_rt_ticker`` (SubType.TICKER): millisecond tick-by-tick trade tape
   - Sub-penny dark-pool / midpoint-peg print ratio (``price * 100 % 1 != 0``)
   - Odd-lot algorithmic child-order slicing ratio (``type == 'ODD_LOT'``)
   - Active buyer vs seller vs midpoint aggression (``ticker_direction``: BUY / SELL / NEUTRAL)
   - Dominant child-order lot size (mode) & lot-size repetition rate
   - Millisecond inter-arrival cadence & sub-50ms intermarket sweep burst ratio
2. ``get_order_book`` (SubType.ORDER_BOOK): Level-2 Bid/Ask depth across price levels
   - Multi-level Order Book Imbalance (``OBI``)
   - Resting bid support wall & ask ceiling wall prices and volumes
3. ``get_capital_distribution``: cumulative dollar inflow/outflow by order size tier
   - Super-large (``super``) + Large (``big``) institutional block net flow (USD)
   - Medium (``mid``) + Small (``small``) algorithmic child-slice net flow (USD)
   - Whale-vs-Slicer divergence (stealth child-slice accumulation vs block distribution)
4. ``get_capital_flow`` (PeriodType.INTRADAY): minute-by-minute intraday dollar flow curve
   - Net capital flow velocity ($/min) and acceleration across size tiers
5. ``get_market_snapshot``: exchange-computed session VWAP (``avg_price``), ``turnover``,
   ``turnover_rate``, ``amplitude``, ``volume_ratio``, and top-of-book ``bid_vol`` / ``ask_vol``.
6. Native 1-minute candles (``bars``, ``premarket_bars``) and sub-minute ``pushes`` in ``common1m.sqlite3``.

Persists raw Moomoo telemetry into ``moomoo_algo_telemetry`` (so historical backtests,
research, and learning have full Moomoo microstructure provenance) and saves trained
classifier weights to ``classifier.safetensors``. This does not train Qwen or
produce a language-model LoRA adapter.
"""
from __future__ import annotations

from collections import Counter
from datetime import datetime
import json
import math
import sqlite3
import struct
import time
from contextlib import closing
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
COMMON_DB = ROOT / "data" / "common1m.sqlite3"
BRAIN_DB = ROOT / "data" / "brain.sqlite3"
ORB_DB = ROOT / "data" / "orb.sqlite3"
OUT_JSON = ROOT / "data" / "algo_fingerprints.json"

SCHEMA = """
CREATE TABLE IF NOT EXISTS algo_fingerprints (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    symbol TEXT NOT NULL,
    algo_type TEXT NOT NULL,
    algo_direction TEXT NOT NULL,
    confidence REAL NOT NULL,
    equation TEXT NOT NULL,
    parameters_json TEXT NOT NULL,
    counter_strategy_json TEXT NOT NULL,
    feature_vector_json TEXT NOT NULL,
    moomoo_telemetry_json TEXT NOT NULL DEFAULT '{}',
    UNIQUE(symbol, ts_ms)
);
CREATE INDEX IF NOT EXISTS idx_algo_fingerprints_symbol_ts ON algo_fingerprints(symbol, ts_ms DESC);

CREATE TABLE IF NOT EXISTS moomoo_algo_telemetry (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    symbol TEXT NOT NULL,
    exchange_vwap REAL,
    turnover_usd REAL,
    turnover_rate_pct REAL,
    amplitude_pct REAL,
    volume_ratio REAL,
    net_whale_flow_usd REAL,
    net_slicer_flow_usd REAL,
    flow_velocity_usd_per_min REAL,
    order_book_imbalance REAL,
    bid_wall_price REAL,
    bid_wall_vol REAL,
    ask_wall_price REAL,
    ask_wall_vol REAL,
    subpenny_darkpool_ratio REAL,
    odd_lot_ratio REAL,
    buy_sweep_ratio REAL,
    sell_sweep_ratio REAL,
    dominant_lot_size REAL,
    lot_repeat_ratio REAL,
    median_inter_trade_ms REAL,
    burst_cluster_ratio REAL,
    raw_json TEXT NOT NULL,
    UNIQUE(symbol, ts_ms)
);
CREATE INDEX IF NOT EXISTS idx_moomoo_telemetry_symbol_ts ON moomoo_algo_telemetry(symbol, ts_ms DESC);
"""


def _safe_mean(vals: list[float]) -> float:
    return sum(vals) / len(vals) if vals else 0.0


def _safe_std(vals: list[float], mean_val: float | None = None) -> float:
    if len(vals) < 2:
        return 0.0
    m = _safe_mean(vals) if mean_val is None else mean_val
    var = sum((x - m) ** 2 for x in vals) / len(vals)
    return math.sqrt(max(0.0, var))


def _lag1_autocorr(vals: list[float]) -> float:
    if len(vals) < 4:
        return 0.0
    m = _safe_mean(vals)
    denom = sum((x - m) ** 2 for x in vals)
    if denom <= 1e-12:
        return 0.0
    num = sum((vals[i] - m) * (vals[i - 1] - m) for i in range(1, len(vals)))
    return max(-1.0, min(1.0, num / denom))


def _parse_tick_ms(time_str: str) -> float | None:
    try:
        dt = datetime.fromisoformat(str(time_str).strip())
        return dt.timestamp() * 1000.0
    except Exception:
        return None


def fetch_moomoo_telemetry(
    symbols: list[str],
    host: str = "127.0.0.1",
    port: int = 11111,
) -> dict[str, dict[str, Any]]:
    """Pull all available Moomoo OpenD microstructure signals for ``symbols``.

    Combines:
    - ``get_market_snapshot`` (exchange VWAP, turnover, amplitude, BBO volume)
    - ``get_capital_distribution`` (Super/Big block vs Mid/Small slicer dollar inflows/outflows)
    - ``get_capital_flow`` (minute-by-minute intraday dollar flow velocity)
    - ``get_order_book`` (Level-2 Bid/Ask depth imbalance and resting liquidity walls)
    - ``get_rt_ticker`` (millisecond tick-by-tick trade tape: dark-pool sub-penny prints,
      odd-lot slicing ratio, buyer/seller sweep aggression, lot-size mode, inter-trade cadence)
    """
    qualified = [s if s.startswith("US.") else f"US.{s}" for s in dict.fromkeys(symbols) if s]
    if not qualified:
        return {}
    try:
        from futu import OpenQuoteContext, PeriodType, RET_OK, SubType
    except Exception:
        return {}

    out: dict[str, dict[str, Any]] = {}
    q = None
    try:
        q = OpenQuoteContext(host=host, port=port)
        # 1. Batch market snapshots (0 subscription quota)
        snaps: dict[str, dict[str, Any]] = {}
        for i in range(0, len(qualified), 100):
            ret, df = q.get_market_snapshot(qualified[i : i + 100])
            if ret == RET_OK and df is not None and not df.empty:
                for _, r in df.iterrows():
                    code = str(r.get("code", ""))
                    snaps[code] = {
                        "last_price": float(r.get("last_price", 0.0) or 0.0),
                        "exchange_vwap": float(r.get("avg_price", 0.0) or 0.0),
                        "turnover_usd": float(r.get("turnover", 0.0) or 0.0),
                        "turnover_rate_pct": float(r.get("turnover_rate", 0.0) or 0.0),
                        "amplitude_pct": float(r.get("amplitude", 0.0) or 0.0),
                        "volume_ratio": float(r.get("volume_ratio", 0.0) or 0.0),
                        "bid_price": float(r.get("bid_price", 0.0) or 0.0),
                        "ask_price": float(r.get("ask_price", 0.0) or 0.0),
                        "bid_vol": float(r.get("bid_vol", 0.0) or 0.0),
                        "ask_vol": float(r.get("ask_vol", 0.0) or 0.0),
                    }

        # 2. Check available subscription quota for TICKER + ORDER_BOOK
        # During open trading sessions, reserve at least 35 subscription units so live
        # trading candle feeds and order execution always have priority.
        from . import darwin_qos
        reserve_units = 35 if darwin_qos.in_open_session() else 15
        remain = 0
        ret_sub, sub_info = q.query_subscription()
        if ret_sub == RET_OK and isinstance(sub_info, dict):
            remain = max(0, int(sub_info.get("remain", 0)) - reserve_units)
        max_sub_symbols = min(len(qualified), max(1, remain // 2))
        subscribed_count = 0

        for code in qualified:
            info: dict[str, Any] = dict(snaps.get(code, {}))
            if subscribed_count < max_sub_symbols:
                r_s, _ = q.subscribe([code], [SubType.TICKER, SubType.ORDER_BOOK], subscribe_push=False)
                if r_s == RET_OK:
                    subscribed_count += 1
            # 3. Capital Distribution (Super/Big blocks vs Mid/Small child-order slices)
            ret_cd, df_cd = q.get_capital_distribution(code)
            if ret_cd == RET_OK and df_cd is not None and not df_cd.empty:
                r = df_cd.iloc[-1]
                in_super = float(r.get("capital_in_super", 0.0) or 0.0)
                in_big = float(r.get("capital_in_big", 0.0) or 0.0)
                in_mid = float(r.get("capital_in_mid", 0.0) or 0.0)
                in_small = float(r.get("capital_in_small", 0.0) or 0.0)
                out_super = float(r.get("capital_out_super", 0.0) or 0.0)
                out_big = float(r.get("capital_out_big", 0.0) or 0.0)
                out_mid = float(r.get("capital_out_mid", 0.0) or 0.0)
                out_small = float(r.get("capital_out_small", 0.0) or 0.0)
                total_in = in_super + in_big + in_mid + in_small
                total_out = out_super + out_big + out_mid + out_small
                total_turnover = max(1.0, total_in + total_out)
                net_whale = (in_super + in_big) - (out_super + out_big)
                net_slicer = (in_mid + in_small) - (out_mid + out_small)
                info.update({
                    "capital_in_super": round(in_super, 2),
                    "capital_in_big": round(in_big, 2),
                    "capital_in_mid": round(in_mid, 2),
                    "capital_in_small": round(in_small, 2),
                    "capital_out_super": round(out_super, 2),
                    "capital_out_big": round(out_big, 2),
                    "capital_out_mid": round(out_mid, 2),
                    "capital_out_small": round(out_small, 2),
                    "net_whale_flow_usd": round(net_whale, 2),
                    "net_slicer_flow_usd": round(net_slicer, 2),
                    "slicer_participation_share": round((in_mid + in_small + out_mid + out_small) / total_turnover, 4),
                    "whale_participation_share": round((in_super + in_big + out_super + out_big) / total_turnover, 4),
                    "net_flow_imbalance": round((total_in - total_out) / total_turnover, 4),
                })

            # 4. Intraday Capital Flow velocity ($/min)
            ret_cf, df_cf = q.get_capital_flow(code, period_type=PeriodType.INTRADAY)
            if ret_cf == RET_OK and df_cf is not None and len(df_cf) >= 2:
                recent_cf = df_cf.tail(6)
                in_flows = [float(x or 0.0) for x in recent_cf["in_flow"].tolist()]
                mid_flows = [float(x or 0.0) for x in recent_cf["mid_in_flow"].tolist()] if "mid_in_flow" in recent_cf else []
                sml_flows = [float(x or 0.0) for x in recent_cf["sml_in_flow"].tolist()] if "sml_in_flow" in recent_cf else []
                flow_vel = (in_flows[-1] - in_flows[0]) / max(1, len(in_flows) - 1)
                slicer_vel = ((mid_flows[-1] + sml_flows[-1]) - (mid_flows[0] + sml_flows[0])) / max(1, len(in_flows) - 1) if mid_flows and sml_flows else 0.0
                info["flow_velocity_usd_per_min"] = round(flow_vel, 2)
                info["slicer_velocity_usd_per_min"] = round(slicer_vel, 2)

            # 5. Level-2 Order Book depth & liquidity walls
            ret_ob, ob = q.get_order_book(code, num=10)
            if ret_ob == RET_OK and isinstance(ob, dict):
                bids = ob.get("Bid") or []
                asks = ob.get("Ask") or []
                bid_depth = sum(float(level[1]) for level in bids if len(level) >= 2)
                ask_depth = sum(float(level[1]) for level in asks if len(level) >= 2)
                obi = (bid_depth - ask_depth) / max(1.0, bid_depth + ask_depth)
                best_bid_wall = max(bids, key=lambda lv: float(lv[1]), default=(0.0, 0.0))
                best_ask_wall = max(asks, key=lambda lv: float(lv[1]), default=(0.0, 0.0))
                info.update({
                    "order_book_imbalance": round(obi, 4),
                    "bid_depth_shares": round(bid_depth, 1),
                    "ask_depth_shares": round(ask_depth, 1),
                    "bid_wall_price": float(best_bid_wall[0]) if best_bid_wall else 0.0,
                    "bid_wall_vol": float(best_bid_wall[1]) if best_bid_wall else 0.0,
                    "ask_wall_price": float(best_ask_wall[0]) if best_ask_wall else 0.0,
                    "ask_wall_vol": float(best_ask_wall[1]) if best_ask_wall else 0.0,
                })

            # 6. Tick-by-tick millisecond tape (get_rt_ticker)
            ret_rt, df_rt = q.get_rt_ticker(code, num=100)
            if ret_rt == RET_OK and df_rt is not None and not df_rt.empty:
                prices = [float(x or 0.0) for x in df_rt["price"].tolist()]
                vols = [float(x or 0.0) for x in df_rt["volume"].tolist()]
                dirs = [str(x or "NEUTRAL").upper() for x in df_rt["ticker_direction"].tolist()]
                types = [str(x or "").upper() for x in df_rt["type"].tolist()]
                times_ms = [_parse_tick_ms(t) for t in df_rt["time"].tolist()]
                valid_times = [t for t in times_ms if t is not None]

                n_ticks = max(1, len(prices))
                total_tick_vol = max(1.0, sum(vols))
                # Sub-penny execution detection (Dark Pool / Midpoint Peg / Internalizer)
                subpenny_count = sum(1 for p in prices if abs(round(p * 100.0) - p * 100.0) > 1e-3)
                odd_lot_count = sum(1 for k, v in zip(types, vols) if "ODD" in k or v < 100.0)
                buy_vol = sum(v for v, d in zip(vols, dirs) if "BUY" in d)
                sell_vol = sum(v for v, d in zip(vols, dirs) if "SELL" in d)
                neutral_vol = sum(v for v, d in zip(vols, dirs) if "NEUTRAL" in d)

                rounded_lots = [int(round(v)) for v in vols if v >= 1.0]
                lot_counts = Counter(rounded_lots)
                dom_lot, dom_count = lot_counts.most_common(1)[0] if lot_counts else (0, 0)

                inter_ms = [max(0.0, valid_times[i] - valid_times[i - 1]) for i in range(1, len(valid_times))]
                sorted_inter = sorted(inter_ms)
                median_inter_ms = sorted_inter[len(sorted_inter) // 2] if sorted_inter else 1000.0
                burst_ratio = sum(1 for dt in inter_ms if dt <= 50.0) / max(1, len(inter_ms)) if inter_ms else 0.0

                info.update({
                    "tick_sample_count": n_ticks,
                    "subpenny_darkpool_ratio": round(subpenny_count / n_ticks, 4),
                    "odd_lot_ratio": round(odd_lot_count / n_ticks, 4),
                    "buy_sweep_ratio": round(buy_vol / total_tick_vol, 4),
                    "sell_sweep_ratio": round(sell_vol / total_tick_vol, 4),
                    "neutral_midpoint_ratio": round(neutral_vol / total_tick_vol, 4),
                    "dominant_lot_size": dom_lot,
                    "lot_repeat_ratio": round(dom_count / max(1, len(rounded_lots)), 4),
                    "median_inter_trade_ms": round(median_inter_ms, 2),
                    "burst_cluster_ratio": round(burst_ratio, 4),
                })

            out[code] = info
    except Exception:
        pass
    finally:
        if q is not None:
            try:
                q.close()
            except Exception:
                pass
    return out


def load_cached_telemetry(db_path: Path, symbol: str, before_ms: int | None = None) -> dict[str, Any]:
    """Load the latest persisted Moomoo telemetry for ``symbol`` (for offline backtests and learning)."""
    if not db_path.exists():
        return {}
    clean = symbol if symbol.startswith("US.") else f"US.{symbol}"
    try:
        with closing(sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)) as db:
            has_table = db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='moomoo_algo_telemetry'").fetchone()
            if not has_table:
                return {}
            if before_ms is not None:
                row = db.execute(
                    "SELECT raw_json FROM moomoo_algo_telemetry WHERE symbol=? AND ts_ms<=? ORDER BY ts_ms DESC LIMIT 1",
                    (clean, int(before_ms)),
                ).fetchone()
            else:
                row = db.execute(
                    "SELECT raw_json FROM moomoo_algo_telemetry WHERE symbol=? ORDER BY ts_ms DESC LIMIT 1",
                    (clean,),
                ).fetchone()
            if row and row[0]:
                return json.loads(row[0])
    except Exception:
        return {}
    return {}


def analyze_bars(
    symbol: str,
    bars: list[tuple[int, float, float, float, float, float]],
    pushes: list[tuple[int, float, float]] | None = None,
    moomoo_telemetry: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Reverse-engineer the active market algorithm from causal 1m bars, sub-minute pushes, and Moomoo OpenD telemetry."""
    clean_symbol = symbol if symbol.startswith("US.") else f"US.{symbol}"
    ticker = clean_symbol.removeprefix("US.")
    telemetry = dict(moomoo_telemetry or {})
    window = [b for b in bars if len(b) >= 6 and b[4] > 0 and b[5] >= 0][-45:]
    if len(window) < 3:
        return {
            "symbol": clean_symbol,
            "ticker": ticker,
            "ts_ms": int(window[-1][0]) if window else int(time.time() * 1000),
            "algo_type": "INSUFFICIENT_TAPE",
            "algo_direction": "neutral",
            "confidence": 0.0,
            "equation": "V(t) = 0",
            "parameters": {"bar_count": len(window)},
            "moomoo_telemetry": telemetry,
            "counter_strategy": {"decision": "abstain", "entry_mode": "wait", "limit_offset_bps": 0.0, "stop_pct": 1.0, "trail_pct": 1.2, "take_profit_pct": 2.0},
            "feature_vector": [0.0] * 12,
        }

    ts_ms = int(window[-1][0])
    highs = [float(b[2]) for b in window]
    lows = [float(b[3]) for b in window]
    closes = [float(b[4]) for b in window]
    volumes = [max(0.0, float(b[5])) for b in window]

    # 1. Return series, sign flips, and autocorrelation
    returns = [(closes[i] / closes[i - 1] - 1.0) if closes[i - 1] > 0 else 0.0 for i in range(1, len(closes))]
    mean_ret = _safe_mean(returns)
    autocorr_1 = _lag1_autocorr(returns)
    nonzero_signs = [1 if r > 1e-6 else (-1 if r < -1e-6 else 0) for r in returns]
    active_signs = [s for s in nonzero_signs if s != 0]
    flips = sum(1 for i in range(1, len(active_signs)) if active_signs[i] != active_signs[i - 1])
    sign_flip_rate = flips / max(1, len(active_signs) - 1) if len(active_signs) >= 2 else 0.5
    pos_ratio = sum(1 for r in returns if r > 0) / max(1, len(returns))

    # 2. Volume slicing metrics (TWAP / participation signature)
    active_vols = [v for v in volumes if v > 0]
    mean_vol = _safe_mean(active_vols) if active_vols else 1.0
    std_vol = _safe_std(active_vols, mean_vol) if active_vols else 0.0
    volume_cv = std_vol / max(1.0, mean_vol)
    recent_vol = _safe_mean(volumes[-3:])
    vol_surge_ratio = recent_vol / max(1.0, _safe_mean(volumes[:-3]) if len(volumes) > 3 else mean_vol)

    # Sub-minute push & Moomoo tick-tape cadence
    slice_cadence_s = 60.0
    if telemetry.get("median_inter_trade_ms"):
        slice_cadence_s = round(max(0.05, float(telemetry["median_inter_trade_ms"]) / 1000.0), 3)
    elif pushes and len(pushes) >= 4:
        deltas = [(pushes[i][0] - pushes[i - 1][0]) / 1000.0 for i in range(1, len(pushes)) if 0.05 <= (pushes[i][0] - pushes[i - 1][0]) / 1000.0 <= 120.0]
        if deltas:
            slice_cadence_s = round(_safe_mean(deltas), 2)

    # 3. Session VWAP & tracking error (prefer Moomoo exchange avg_price when present)
    typ_prices = [(highs[i] + lows[i] + closes[i]) / 3.0 for i in range(len(window))]
    cum_pv = 0.0
    cum_v = 0.0
    vwap_series: list[float] = []
    for tp, v in zip(typ_prices, volumes):
        cum_pv += tp * v
        cum_v += v
        vwap_series.append(cum_pv / cum_v if cum_v > 0 else tp)
    exchange_vwap = float(telemetry.get("exchange_vwap", 0.0) or 0.0)
    current_vwap = exchange_vwap if exchange_vwap > 0 else vwap_series[-1]
    vwap_devs_bps = [abs(closes[i] - vwap_series[i]) / max(1e-6, vwap_series[i]) * 10_000.0 for i in range(len(window))]
    mean_vwap_dev_bps = _safe_mean(vwap_devs_bps)
    vwap_crossings = sum(
        1 for i in range(1, len(window))
        if (closes[i] - vwap_series[i]) * (closes[i - 1] - vwap_series[i - 1]) <= 0
    )
    vwap_reversion_rate = vwap_crossings / max(1, len(window) - 1)

    # 4. Close Location Value (CLV) & range compression
    clvs = [
        ((closes[i] - lows[i]) - (highs[i] - closes[i])) / (highs[i] - lows[i])
        if highs[i] > lows[i] else 0.0
        for i in range(len(window))
    ]
    mean_clv = _safe_mean(clvs)
    recent_clv = _safe_mean(clvs[-3:])
    ranges_bps = [(highs[i] - lows[i]) / max(1e-6, closes[i]) * 10_000.0 for i in range(len(window))]
    mean_range_bps = max(1.0, _safe_mean(ranges_bps))
    recent_range_bps = _safe_mean(ranges_bps[-3:])
    range_expansion = recent_range_bps / mean_range_bps

    recent_high = max(highs[-8:])
    recent_low = min(lows[-8:])
    high_touches = sum(1 for h in highs[-8:] if abs(h - recent_high) / max(1e-6, recent_high) <= 0.0008)
    low_touches = sum(1 for l in lows[-8:] if abs(l - recent_low) / max(1e-6, recent_low) <= 0.0008)

    net_drift_pct = (closes[-1] / closes[0] - 1.0) * 100.0 if closes[0] > 0 else 0.0
    drift_bps_per_min = (net_drift_pct * 100.0) / max(1, len(window) - 1)

    # 5. Moomoo OpenD microstructure signals (Tick Tape, L2 Order Book, Capital Flow Tiers)
    subpenny_ratio = float(telemetry.get("subpenny_darkpool_ratio", 0.0) or 0.0)
    odd_lot_ratio = float(telemetry.get("odd_lot_ratio", 0.0) or 0.0)
    buy_sweep_ratio = float(telemetry.get("buy_sweep_ratio", 0.0) or 0.0)
    sell_sweep_ratio = float(telemetry.get("sell_sweep_ratio", 0.0) or 0.0)
    lot_repeat_ratio = float(telemetry.get("lot_repeat_ratio", 0.0) or 0.0)
    burst_ratio = float(telemetry.get("burst_cluster_ratio", 0.0) or 0.0)
    obi = float(telemetry.get("order_book_imbalance", 0.0) or 0.0)
    slicer_share = float(telemetry.get("slicer_participation_share", 0.0) or 0.0)
    whale_share = float(telemetry.get("whale_participation_share", 0.0) or 0.0)
    net_whale_usd = float(telemetry.get("net_whale_flow_usd", 0.0) or 0.0)
    net_slicer_usd = float(telemetry.get("net_slicer_flow_usd", 0.0) or 0.0)
    flow_vel_usd = float(telemetry.get("flow_velocity_usd_per_min", 0.0) or 0.0)

    # 6. Score each algorithmic archetype using BOTH 1m bars AND Moomoo OpenD microstructure
    twap_score = max(
        0.0,
        min(
            1.0,
            (1.0 - min(1.0, volume_cv / 1.15)) * 0.35
            + min(0.25, abs(pos_ratio - 0.5) * 1.1)
            + min(0.25, (odd_lot_ratio * 0.6 + lot_repeat_ratio * 0.4) * 0.45)
            + min(0.15, slicer_share * 0.25),
        ),
    )
    vwap_score = max(
        0.0,
        min(
            1.0,
            (1.0 - min(1.0, mean_vwap_dev_bps / 55.0)) * 0.45
            + min(0.25, vwap_reversion_rate * 1.8)
            + min(0.30, subpenny_ratio * 0.75),
        ),
    )
    iceberg_score = (
        max(
            0.0,
            min(
                1.0,
                0.45
                + (0.25 if range_expansion < 0.85 else 0.0)
                + min(0.30, abs(obi) * 0.45),
            ),
        )
        if (max(high_touches, low_touches) >= 4 and range_expansion < 0.90) or abs(obi) >= 0.55
        else 0.15
    )
    ignition_score = max(
        0.0,
        min(
            1.0,
            min(0.35, max(0.0, vol_surge_ratio - 0.95) * 0.40)
            + min(0.25, max(0.0, range_expansion - 0.90) * 0.35)
            + min(0.20, abs(drift_bps_per_min) / 14.0)
            + min(0.20, (burst_ratio * 0.5 + max(buy_sweep_ratio, sell_sweep_ratio) * 0.5) * 0.40),
        ),
    )
    pingpong_score = max(
        0.0,
        min(
            1.0,
            max(0.0, -autocorr_1) * 0.50
            + max(0.0, sign_flip_rate - 0.40) * 0.70
            + min(0.25, subpenny_ratio * 0.50),
        ),
    )
    trap_score = 0.0
    if (closes[-1] < RecentPeak(highs) and recent_clv < -0.20 and returns[-1] < 0 and max(volumes[-3:]) > 1.2 * mean_vol) or (
        net_whale_usd < -500_000 and net_slicer_usd > 500_000 and recent_clv < 0
    ):
        trap_score = max(0.0, min(1.0, 0.60 + abs(recent_clv) * 0.25 + (0.15 if net_whale_usd < 0 else 0.0)))

    scores = {
        "TWAP_ACCUMULATION" if (net_drift_pct + (0.1 if net_slicer_usd > 0 else -0.1)) >= 0 else "TWAP_DISTRIBUTION": round(twap_score, 4),
        "VWAP_PARTICIPATION": round(vwap_score, 4),
        "ICEBERG_ABSORPTION": round(iceberg_score, 4),
        "MOMENTUM_IGNITION_SWEEP": round(ignition_score, 4),
        "MEAN_REVERSION_MM_PINGPONG": round(pingpong_score, 4),
        "LIQUIDITY_VACUUM_TRAP": round(trap_score, 4),
    }
    algo_type = max(scores, key=lambda k: scores[k])
    confidence = round(max(0.35, min(0.99, scores[algo_type])), 4)

    net_capital_signal = net_whale_usd + net_slicer_usd
    if algo_type == "LIQUIDITY_VACUUM_TRAP" or (net_drift_pct < -0.15 and recent_clv < -0.15 and net_capital_signal <= 0):
        direction = "bearish_distribution"
    elif algo_type == "MEAN_REVERSION_MM_PINGPONG" and abs(net_drift_pct) < 0.25:
        direction = "neutral_market_making"
    elif net_drift_pct >= 0 or net_capital_signal > 0 or buy_sweep_ratio > sell_sweep_ratio:
        direction = "bullish_accumulation"
    else:
        direction = "bearish_distribution"

    # 7. Governing mathematical equation incorporating Moomoo OpenD tape & capital flow
    dom_lot = int(telemetry.get("dominant_lot_size") or round(mean_vol * (slice_cadence_s / 60.0)))
    moomoo_tag = ""
    if telemetry:
        moomoo_tag = (
            f" [Moomoo: odd_lot={odd_lot_ratio*100:.0f}%, dark_subpenny={subpenny_ratio*100:.0f}%, "
            f"buy_sweep={buy_sweep_ratio*100:.0f}%, OBI={obi:+.2f}, "
            f"whale=${net_whale_usd/1e6:+.2f}M, slicer=${net_slicer_usd/1e6:+.2f}M]"
        )
    if algo_type.startswith("TWAP_"):
        equation = f"q(t) = {dom_lot} sh @ {slice_cadence_s:.2f}s, P(t) = P0 * (1 + {drift_bps_per_min:+.2f} bps/min * t){moomoo_tag}"
    elif algo_type == "VWAP_PARTICIPATION":
        equation = f"P(t) -> VWAP({current_vwap:.4f}) +/- {mean_vwap_dev_bps:.1f} bps, slicer_share={slicer_share*100:.1f}%{moomoo_tag}"
    elif algo_type == "ICEBERG_ABSORPTION":
        wall_px = float(telemetry.get("bid_wall_price") or recent_low) if direction == "bullish_accumulation" else float(telemetry.get("ask_wall_price") or recent_high)
        wall_vol = float(telemetry.get("bid_wall_vol") or recent_vol) if direction == "bullish_accumulation" else float(telemetry.get("ask_wall_vol") or recent_vol)
        equation = f"Peg({wall_px:.4f}, wall={wall_vol:.0f} sh) absorbs flow with range_compression={range_expansion:.2f}x{moomoo_tag}"
    elif algo_type == "MOMENTUM_IGNITION_SWEEP":
        equation = f"Sweep(V={vol_surge_ratio:.2f}x, burst={burst_ratio*100:.0f}%, CLV={recent_clv:+.2f}) -> dP/dt = {drift_bps_per_min:+.2f} bps/min{moomoo_tag}"
    elif algo_type == "MEAN_REVERSION_MM_PINGPONG":
        equation = f"P(t) in [{recent_low:.4f}, {recent_high:.4f}], autocorr(1)={autocorr_1:+.2f}, flip_rate={sign_flip_rate:.2f}{moomoo_tag}"
    else:
        equation = f"Trap(peak={recent_high:.4f} -> {closes[-1]:.4f}, CLV={recent_clv:+.2f}, whale=${net_whale_usd/1e6:+.2f}M){moomoo_tag}"

    bullish_actionable = direction == "bullish_accumulation" and algo_type != "LIQUIDITY_VACUUM_TRAP"
    if algo_type == "MOMENTUM_IGNITION_SWEEP" and bullish_actionable:
        counter_strategy = {
            "decision": "enter",
            "entry_mode": "ride_ignition_breakout",
            "limit_offset_bps": 15.0,
            "stop_pct": 0.65,
            "trail_pct": 1.35,
            "take_profit_pct": 2.8,
        }
    elif algo_type in ("TWAP_ACCUMULATION", "VWAP_PARTICIPATION") and bullish_actionable:
        counter_strategy = {
            "decision": "enter",
            "entry_mode": "join_institutional_vwap_bid",
            "limit_offset_bps": 8.0,
            "stop_pct": 0.55,
            "trail_pct": 1.15,
            "take_profit_pct": 2.2,
        }
    elif algo_type == "ICEBERG_ABSORPTION" and bullish_actionable:
        counter_strategy = {
            "decision": "enter",
            "entry_mode": "front_run_iceberg_support",
            "limit_offset_bps": 5.0,
            "stop_pct": 0.50,
            "trail_pct": 1.10,
            "take_profit_pct": 2.0,
        }
    else:
        counter_strategy = {
            "decision": "abstain",
            "entry_mode": "avoid_distribution_or_chop",
            "limit_offset_bps": 0.0,
            "stop_pct": 0.50,
            "trail_pct": 0.90,
            "take_profit_pct": 1.5,
        }

    parameters = {
        "bar_count": len(window),
        "last_price": round(closes[-1], 4),
        "vwap_anchor": round(current_vwap, 4),
        "vwap_tracking_bps": round(mean_vwap_dev_bps, 2),
        "vwap_reversion_rate": round(vwap_reversion_rate, 4),
        "slice_cadence_seconds": slice_cadence_s,
        "dominant_lot_size": dom_lot,
        "volume_cv": round(volume_cv, 4),
        "volume_surge_ratio": round(vol_surge_ratio, 4),
        "drift_bps_per_min": round(drift_bps_per_min, 3),
        "lag1_return_autocorr": round(autocorr_1, 4),
        "sign_flip_rate": round(sign_flip_rate, 4),
        "mean_clv": round(mean_clv, 4),
        "recent_clv": round(recent_clv, 4),
        "range_expansion_ratio": round(range_expansion, 4),
        "support_level": round(recent_low, 4),
        "resistance_level": round(recent_high, 4),
        "subpenny_darkpool_ratio": subpenny_ratio,
        "odd_lot_ratio": odd_lot_ratio,
        "buy_sweep_ratio": buy_sweep_ratio,
        "sell_sweep_ratio": sell_sweep_ratio,
        "order_book_imbalance": obi,
        "net_whale_flow_usd": net_whale_usd,
        "net_slicer_flow_usd": net_slicer_usd,
        "flow_velocity_usd_per_min": flow_vel_usd,
        "archetype_scores": scores,
    }

    # 12-dim normalized feature vector fusing 1m bars + Moomoo OpenD tape/book/flow for LoRA adapter
    flow_norm = max(-1.0, min(1.0, (net_whale_usd + net_slicer_usd) / 10_000_000.0))
    sweep_imb = max(-1.0, min(1.0, buy_sweep_ratio - sell_sweep_ratio))
    feature_vector = [
        round(max(-1.0, min(1.0, drift_bps_per_min / 25.0)), 6),
        round(max(0.0, min(1.0, 1.0 - min(1.0, volume_cv))), 6),
        round(max(0.0, min(1.0, 1.0 - min(1.0, mean_vwap_dev_bps / 50.0))), 6),
        round(max(0.0, min(1.0, odd_lot_ratio if odd_lot_ratio > 0 else vwap_reversion_rate)), 6),
        round(max(0.0, min(1.0, subpenny_ratio if subpenny_ratio > 0 else vol_surge_ratio / 4.0)), 6),
        round(max(-1.0, min(1.0, sweep_imb if (buy_sweep_ratio or sell_sweep_ratio) else recent_clv)), 6),
        round(max(-1.0, min(1.0, obi if obi != 0 else autocorr_1)), 6),
        round(max(-1.0, min(1.0, flow_norm)), 6),
        round(max(-1.0, min(1.0, mean_clv)), 6),
        round(max(-1.0, min(1.0, recent_clv)), 6),
        round(float(confidence), 6),
        1.0 if counter_strategy["decision"] == "enter" else -1.0,
    ]

    return {
        "symbol": clean_symbol,
        "ticker": ticker,
        "ts_ms": ts_ms,
        "algo_type": algo_type,
        "algo_direction": direction,
        "confidence": confidence,
        "equation": equation,
        "parameters": parameters,
        "moomoo_telemetry": telemetry,
        "counter_strategy": counter_strategy,
        "feature_vector": feature_vector,
    }


def RecentPeak(highs: list[float]) -> float:
    return max(highs[-6:]) if highs else 0.0


def candidate_universe(root: Path = ROOT) -> list[str]:
    """Use the bots' shared local candidate snapshots and their open positions."""
    symbols: list[str] = []
    for rel, key in (("data/plan.json", "candidates"), ("data/orb_premarket.json", "candidates"), ("data/status.json", "positions_live")):
        path = root / rel
        if not path.exists():
            continue
        try:
            payload = json.loads(path.read_text())
            for item in payload.get(key, []):
                sym = str(item.get("symbol") or item.get("code") or "").strip()
                if sym:
                    full = sym if sym.startswith("US.") else f"US.{sym}"
                    if full not in symbols:
                        symbols.append(full)
        except Exception:
            continue
    return symbols


def _ensure_schema(db: sqlite3.Connection) -> None:
    db.executescript(SCHEMA)
    cols = {r[1] for r in db.execute("PRAGMA table_info(algo_fingerprints)").fetchall()}
    if "moomoo_telemetry_json" not in cols:
        db.execute("ALTER TABLE algo_fingerprints ADD COLUMN moomoo_telemetry_json TEXT NOT NULL DEFAULT '{}'")


def scan_candidates(
    common_db: Path = COMMON_DB,
    brain_db: Path = BRAIN_DB,
    orb_db: Path = ORB_DB,
    out_json: Path = OUT_JSON,
    symbols: list[str] | None = None,
    use_moomoo: bool = True,
) -> dict[str, Any]:
    """Scan all active candidates using common1m.sqlite3 + live Moomoo OpenD microstructure telemetry."""
    target_symbols = symbols or candidate_universe(common_db.parents[1])
    if not common_db.exists():
        return {"scanned": 0, "fingerprints": []}

    results: list[dict[str, Any]] = []
    with closing(sqlite3.connect(f"file:{common_db}?mode=ro", uri=True)) as cdb:
        tables = {r[0] for r in cdb.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()}
        if not target_symbols and "bars" in tables:
            target_symbols = [f"US.{r[0].removeprefix('US.')}" for r in cdb.execute("SELECT symbol FROM bars GROUP BY symbol ORDER BY max(ts) DESC LIMIT 25").fetchall()]

        live_telemetry = fetch_moomoo_telemetry(target_symbols) if use_moomoo else {}

        for sym in target_symbols:
            ticker = sym.removeprefix("US.")
            qualified = f"US.{ticker}"
            rows = list(cdb.execute("SELECT ts, open, high, low, close, volume FROM bars WHERE symbol=? ORDER BY ts DESC LIMIT 60", (ticker,)).fetchall())
            rows += list(cdb.execute("SELECT ts, open, high, low, close, volume FROM bars WHERE symbol=? ORDER BY ts DESC LIMIT 60", (qualified,)).fetchall())
            if "premarket_bars" in tables:
                rows += list(cdb.execute("SELECT ts, open, high, low, close, volume FROM premarket_bars WHERE symbol=? ORDER BY ts DESC LIMIT 60", (ticker,)).fetchall())
                rows += list(cdb.execute("SELECT ts, open, high, low, close, volume FROM premarket_bars WHERE symbol=? ORDER BY ts DESC LIMIT 60", (qualified,)).fetchall())
            dedup = {int(r[0]): (int(r[0]), float(r[1]), float(r[2]), float(r[3]), float(r[4]), float(r[5])) for r in rows}
            bars = [dedup[k] for k in sorted(dedup)][-60:]
            pushes: list[tuple[int, float, float]] = []
            if "pushes" in tables:
                p_rows = list(cdb.execute("SELECT recv_ms, close, volume FROM pushes WHERE code=? ORDER BY recv_ms DESC LIMIT 120", (qualified,)).fetchall())
                if not p_rows:
                    p_rows = list(cdb.execute("SELECT recv_ms, close, volume FROM pushes WHERE code=? ORDER BY recv_ms DESC LIMIT 120", (ticker,)).fetchall())
                pushes = [(int(r[0]), float(r[1]), float(r[2])) for r in reversed(p_rows)]
            tele = live_telemetry.get(qualified) or load_cached_telemetry(brain_db, qualified)
            fp = analyze_bars(qualified, bars, pushes=pushes, moomoo_telemetry=tele)
            if fp["algo_type"] != "INSUFFICIENT_TAPE":
                results.append(fp)

    now_ms = int(time.time() * 1000)
    for db_path in (brain_db, orb_db):
        if not db_path.parent.exists():
            continue
        try:
            with closing(sqlite3.connect(db_path, timeout=10)) as db:
                _ensure_schema(db)
                for fp in results:
                    tele = fp.get("moomoo_telemetry") or {}
                    db.execute(
                        "INSERT OR REPLACE INTO algo_fingerprints(ts_ms, symbol, algo_type, algo_direction, confidence, equation, parameters_json, counter_strategy_json, feature_vector_json, moomoo_telemetry_json) VALUES(?,?,?,?,?,?,?,?,?,?)",
                        (
                            fp["ts_ms"],
                            fp["symbol"],
                            fp["algo_type"],
                            fp["algo_direction"],
                            fp["confidence"],
                            fp["equation"],
                            json.dumps(fp["parameters"], sort_keys=True),
                            json.dumps(fp["counter_strategy"], sort_keys=True),
                            json.dumps(fp["feature_vector"]),
                            json.dumps(tele, sort_keys=True),
                        ),
                    )
                    if tele:
                        db.execute(
                            "INSERT OR REPLACE INTO moomoo_algo_telemetry("
                            "ts_ms, symbol, exchange_vwap, turnover_usd, turnover_rate_pct, amplitude_pct, volume_ratio, "
                            "net_whale_flow_usd, net_slicer_flow_usd, flow_velocity_usd_per_min, order_book_imbalance, "
                            "bid_wall_price, bid_wall_vol, ask_wall_price, ask_wall_vol, subpenny_darkpool_ratio, "
                            "odd_lot_ratio, buy_sweep_ratio, sell_sweep_ratio, dominant_lot_size, lot_repeat_ratio, "
                            "median_inter_trade_ms, burst_cluster_ratio, raw_json"
                            ") VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                            (
                                fp["ts_ms"],
                                fp["symbol"],
                                float(tele.get("exchange_vwap", 0.0) or 0.0),
                                float(tele.get("turnover_usd", 0.0) or 0.0),
                                float(tele.get("turnover_rate_pct", 0.0) or 0.0),
                                float(tele.get("amplitude_pct", 0.0) or 0.0),
                                float(tele.get("volume_ratio", 0.0) or 0.0),
                                float(tele.get("net_whale_flow_usd", 0.0) or 0.0),
                                float(tele.get("net_slicer_flow_usd", 0.0) or 0.0),
                                float(tele.get("flow_velocity_usd_per_min", 0.0) or 0.0),
                                float(tele.get("order_book_imbalance", 0.0) or 0.0),
                                float(tele.get("bid_wall_price", 0.0) or 0.0),
                                float(tele.get("bid_wall_vol", 0.0) or 0.0),
                                float(tele.get("ask_wall_price", 0.0) or 0.0),
                                float(tele.get("ask_wall_vol", 0.0) or 0.0),
                                float(tele.get("subpenny_darkpool_ratio", 0.0) or 0.0),
                                float(tele.get("odd_lot_ratio", 0.0) or 0.0),
                                float(tele.get("buy_sweep_ratio", 0.0) or 0.0),
                                float(tele.get("sell_sweep_ratio", 0.0) or 0.0),
                                float(tele.get("dominant_lot_size", 0.0) or 0.0),
                                float(tele.get("lot_repeat_ratio", 0.0) or 0.0),
                                float(tele.get("median_inter_trade_ms", 0.0) or 0.0),
                                float(tele.get("burst_cluster_ratio", 0.0) or 0.0),
                                json.dumps(tele, sort_keys=True),
                            ),
                        )
                has_memories = db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories'").fetchone()
                if has_memories:
                    for fp in results:
                        finding = (
                            f"Candidate {fp['symbol']} market microstructure identified as {fp['algo_type']} "
                            f"({fp['algo_direction']}, confidence {fp['confidence']:.2f}): {fp['equation']}. "
                            f"Counter-strategy: {fp['counter_strategy']['entry_mode']} "
                            f"(decision={fp['counter_strategy']['decision']}, stop={fp['counter_strategy']['stop_pct']}%, "
                            f"trail={fp['counter_strategy']['trail_pct']}%, tp={fp['counter_strategy']['take_profit_pct']}%)."
                        )
                        evidence_str = json.dumps(
                            {
                                "symbol": fp["symbol"],
                                "algo_type": fp["algo_type"],
                                "algo_direction": fp["algo_direction"],
                                "equation": fp["equation"],
                                "parameters": fp["parameters"],
                                "moomoo_telemetry": fp.get("moomoo_telemetry", {}),
                                "counter_strategy": fp["counter_strategy"],
                            },
                            sort_keys=True,
                        )
                        existing = db.execute(
                            "SELECT id FROM memories WHERE topic='algo_fingerprint' AND source=? AND ts_ms=?",
                            (fp["symbol"], fp["ts_ms"]),
                        ).fetchone()
                        if not existing:
                            db.execute(
                                "INSERT INTO memories(ts_ms, kind, topic, text, evidence, source, via, confidence, status, strategy_version) "
                                "VALUES(?, 'lesson', 'algo_fingerprint', ?, ?, ?, 'rust', ?, 'active', 'nasdaq_hottest_gainers_live:algo_v1')",
                                (fp["ts_ms"], finding, evidence_str, fp["symbol"], fp["confidence"]),
                            )
                db.commit()
        except sqlite3.Error:
            pass

    summary = {
        "updated_ms": now_ms,
        "scanned": len(results),
        "moomoo_enriched": sum(1 for r in results if r.get("moomoo_telemetry")),
        "by_algo_type": {},
        "fingerprints": results,
    }
    for fp in results:
        summary["by_algo_type"][fp["algo_type"]] = summary["by_algo_type"].get(fp["algo_type"], 0) + 1
    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(summary, indent=2) + "\n")
    return summary


def write_safetensors_adapter(
    adapter_dir: Path,
    corpus_dir: Path | None = None,
    fingerprints: list[dict[str, Any]] | None = None,
    model_path: str = "/home/patrick/.omlx/models/Qwen2.5-7B-Instruct-4bit",
) -> Path:
    """Legacy name: fit only a small algorithm classifier, NOT a Qwen adapter."""
    adapter_dir.mkdir(parents=True, exist_ok=True)
    if fingerprints is None:
        if OUT_JSON.exists():
            try:
                fingerprints = json.loads(OUT_JSON.read_text()).get("fingerprints", [])
            except Exception:
                fingerprints = []
        if not fingerprints:
            fingerprints = scan_candidates().get("fingerprints", [])

    w = [0.05] * 12
    bias = 0.0
    samples: list[tuple[list[float], float]] = []
    for fp in fingerprints:
        vec = list(fp.get("feature_vector") or [0.0] * 12)
        if len(vec) == 12:
            label = 1.0 if fp.get("counter_strategy", {}).get("decision") == "enter" else 0.0
            samples.append((vec, label))

    if corpus_dir and (corpus_dir / "train.jsonl").exists():
        for line in (corpus_dir / "train.jsonl").read_text().splitlines():
            try:
                item = json.loads(line)
                ans = json.loads(item["messages"][1]["content"]) if item["messages"][1]["content"].strip().startswith("{") else {}
                y = 1.0 if ans.get("decision") == "enter" or "Accepted" in item["messages"][1]["content"] else 0.0
                af = item.get("source", {}).get("algo_features")
                if isinstance(af, list) and len(af) == 12:
                    samples.append(([float(x) for x in af], y))
            except Exception:
                continue

    for _ in range(120):
        for x, y in samples:
            z = max(-30.0, min(30.0, bias + sum(a * b for a, b in zip(w, x))))
            pred = 1.0 / (1.0 + math.exp(-z))
            err = pred - y
            bias -= 0.04 * err
            for idx in range(12):
                w[idx] -= 0.04 * err * x[idx]

    tensors: dict[str, tuple[list[int], list[float]]] = {
        "algo_adapter.classifier.weight": ([1, 12], [float(x) for x in w]),
        "algo_adapter.classifier.bias": ([1], [float(bias)]),
    }

    header: dict[str, Any] = {
        "__metadata__": {
            "format": "classifier_only",
            "adapter_type": "logistic_classifier_NOT_Qwen_LoRA",
            "candidates_captured": str(len(fingerprints)),
            "moomoo_enriched": str(sum(1 for fp in fingerprints if fp.get("moomoo_telemetry"))),
            "updated_ms": str(int(time.time() * 1000)),
        }
    }
    raw_chunks: list[bytes] = []
    offset = 0
    for name, (shape, values) in tensors.items():
        buf = struct.pack(f"<{len(values)}f", *values)
        header[name] = {
            "dtype": "F32",
            "shape": shape,
            "data_offsets": [offset, offset + len(buf)],
        }
        raw_chunks.append(buf)
        offset += len(buf)

    header_bytes = json.dumps(header, separators=(",", ":")).encode("utf-8")
    pad = (8 - (len(header_bytes) % 8)) % 8
    if pad:
        header_bytes += b" " * pad

    safetensors_path = adapter_dir / "classifier.safetensors"
    with safetensors_path.open("xb") as fh:
        fh.write(struct.pack("<Q", len(header_bytes)))
        fh.write(header_bytes)
        for chunk in raw_chunks:
            fh.write(chunk)

    config = {
        "adapter_path": str(adapter_dir),
        "batch_size": 1,
        "data": str(corpus_dir or adapter_dir.parent),
        "fine_tune_type": "none_classifier_only",
        "grad_checkpoint": True,
        "iters": 120,
        "learning_rate": 1e-05,
        "lora_parameters": None,
        "mask_prompt": True,
        "max_seq_length": 256,
        "model": model_path,
        "num_layers": 1,
        "optimizer": "adamw",
        "save_every": 100,
        "seed": 0,
        "steps_per_eval": 10,
        "steps_per_report": 10,
        "test": True,
        "test_batches": 1,
        "train": False,
        "artifact_kind": "classifier_only",
        "qwen_weights_trained": False,
        "algo_capture": {
            "scanned_candidates": len(fingerprints),
            "moomoo_enriched": sum(1 for fp in fingerprints if fp.get("moomoo_telemetry")),
            "weights": [round(x, 6) for x in w],
            "bias": round(bias, 6),
        },
    }
    (adapter_dir / "adapter_config.json").write_text(json.dumps(config, indent=4) + "\n")
    (adapter_dir / "algo_signatures.json").write_text(
        json.dumps(
            {
                "updated_ms": int(time.time() * 1000),
                "classifier_weights": [round(x, 6) for x in w],
                "classifier_bias": round(bias, 6),
                "feature_names": [
                    "drift_bps_norm",
                    "volume_uniformity_twap",
                    "vwap_anchoring",
                    "odd_lot_slicing_ratio",
                    "darkpool_subpenny_ratio",
                    "sweep_aggression_imbalance",
                    "order_book_imbalance",
                    "net_capital_flow_norm",
                    "mean_clv",
                    "recent_clv",
                    "algo_confidence",
                    "counter_decision_sign",
                ],
                "captured_algorithms": [
                    {
                        "symbol": fp["symbol"],
                        "algo_type": fp["algo_type"],
                        "algo_direction": fp["algo_direction"],
                        "confidence": fp["confidence"],
                        "equation": fp["equation"],
                        "parameters": fp["parameters"],
                        "moomoo_telemetry": fp.get("moomoo_telemetry", {}),
                        "counter_strategy": fp["counter_strategy"],
                    }
                    for fp in fingerprints
                ],
            },
            indent=2,
        )
        + "\n"
    )
    return safetensors_path


def evaluate_dynamic_exit(
    symbol: str,
    entry_price: float,
    high_price: float,
    last_price: float,
    current_stop: float,
    current_target: float,
    bars: list[tuple] | None = None,
    moomoo_telemetry: dict[str, Any] | None = None,
    brain_path: Path = BRAIN_DB,
    common_db: Path = COMMON_DB,
    base_stop_pct: float = 1.5,
    base_trail_pct: float = 1.5,
) -> dict[str, Any]:
    """ThomasAi + Moomoo Microstructure Cognitive Exit & Dynamic Bracket Monitor.

    Evaluates open positions on both bots (ORB Standard and Gainers Competition paper accounts)
    in real time using:
      1. Live 1-minute Kalman price/CLV velocity & acceleration + ThomasAi connectome (`conviction`, `bear_sentry`).
      2. Reverse-engineered Moomoo algorithmic trading fingerprint (`TWAP`, `VWAP`, `ICEBERG`, `IGNITION`, `TRAP`)
         and Level-2 order book imbalance / whale-vs-slicer dollar flow.
      3. Dynamic stop ratcheting (break-even arming + volatility/algo-scaled trailing floor) and
         dynamic target expansion (expanding the target ceiling as runners make new highs or tightening
         to a visible Level-2 ask wall when momentum stalls).
    """
    from . import microsense

    clean = symbol if symbol.startswith("US.") else f"US.{symbol}"
    ticker = clean.removeprefix("US.")
    entry = float(entry_price or last_price or 1.0)
    last = float(last_price or entry)
    high = max(float(high_price or entry), entry, last)
    pnl_pct = ((last - entry) / entry * 100.0) if entry > 0 else 0.0
    run_up_pct = ((high - entry) / entry * 100.0) if entry > 0 else 0.0

    now_ms = int(time.time() * 1000)
    if bars is None:
        bars = microsense.read_minutes(common_db, ticker, now_ms + 60_000)
    if moomoo_telemetry is None:
        moomoo_telemetry = load_cached_telemetry(brain_path, clean)

    fp = analyze_bars(clean, bars or [], moomoo_telemetry=moomoo_telemetry)
    algo_type = str(fp.get("algo_type", "VWAP_PARTICIPATION"))
    algo_dir = str(fp.get("algo_direction", "bullish_accumulation"))
    cs = fp.get("counter_strategy") or {}
    params = fp.get("parameters") or {}

    conviction = 0.65
    bear_sentry = 0.35
    kalman_vel = 0.0
    kalman_acc = 0.0
    clv_vel = 0.0
    if bars:
        rvol = float((moomoo_telemetry or {}).get("volume_ratio") or params.get("volume_surge_ratio") or 1.5)
        bid = float((moomoo_telemetry or {}).get("bid") or last * 0.9995)
        ask = float((moomoo_telemetry or {}).get("ask") or last * 1.0005)
        sensed = microsense.features(bars, rvol, bid, ask)
        kalman_vel = float(sensed.get("price_velocity", 0.0))
        kalman_acc = float(sensed.get("price_acceleration", 0.0))
        clv_vel = float(sensed.get("clv_velocity", 0.0))
        if brain_path.exists():
            try:
                conviction, bear_sentry = microsense.forward_pass(brain_path, sensed)
            except Exception:
                pass

    obi = float(params.get("order_book_imbalance", 0.0) or 0.0)
    sell_sweep = float(params.get("sell_sweep_ratio", 0.0) or 0.0)
    buy_sweep = float(params.get("buy_sweep_ratio", 0.0) or 0.0)
    whale_flow = float(params.get("net_whale_flow_usd", 0.0) or 0.0)
    slicer_flow = float(params.get("net_slicer_flow_usd", 0.0) or 0.0)
    ask_wall_px = float((moomoo_telemetry or {}).get("ask_wall_price") or 0.0)
    bid_wall_px = float((moomoo_telemetry or {}).get("bid_wall_price") or 0.0)

    # Dynamic trailing percentage adapted to live algorithmic regime & ThomasAi conviction/bear ratio
    algo_trail_pct = float(cs.get("trail_pct") or base_trail_pct or 1.25)
    regime_ratio = max(0.45, min(2.2, conviction / max(0.25, bear_sentry)))
    effective_trail_pct = max(0.45, min(2.5, algo_trail_pct / (regime_ratio ** 0.35)))
    if algo_dir == "bearish_distribution" or sell_sweep > buy_sweep + 0.20 or obi < -0.25:
        effective_trail_pct = max(0.35, effective_trail_pct * 0.65)
    elif algo_type in ("MOMENTUM_IGNITION_SWEEP", "TWAP_ACCUMULATION") and algo_dir == "bullish_accumulation":
        effective_trail_pct = max(0.60, effective_trail_pct * 1.10)

    # Dynamic stop ratcheting: never lower an existing stop; lock in break-even + profit cushion as high grows
    initial_stop = float(current_stop) if float(current_stop or 0) > 0 else round(entry * (1.0 - base_stop_pct / 100.0), 4)
    trail_stop = round(high * (1.0 - effective_trail_pct / 100.0), 4)
    dynamic_stop = max(initial_stop, trail_stop) if run_up_pct >= 0.45 else initial_stop
    if run_up_pct >= 0.80:
        # Arm break-even + cushion once runner is up >= +0.80%
        lock_floor = round(entry * (1.0 + min(0.50, run_up_pct * 0.30) / 100.0), 4)
        dynamic_stop = max(dynamic_stop, lock_floor)
    if bid_wall_px > entry and bid_wall_px < last and run_up_pct >= 0.60:
        # Anchor stop just below institutional Level-2 bid support wall
        dynamic_stop = max(dynamic_stop, round(bid_wall_px * 0.998, 4))
    dynamic_stop = round(min(dynamic_stop, last * 0.999 if last > dynamic_stop else dynamic_stop), 4)

    # Dynamic target projection: never static! Expands with bullish momentum/conviction and adapts to L2 ask walls
    base_risk = max(entry * 0.005, entry - initial_stop)
    dynamic_r_mult = max(1.2, min(4.5, 1.5 * regime_ratio * (1.25 if algo_dir == "bullish_accumulation" else 0.80)))
    projected_target = max(high, last) + dynamic_r_mult * base_risk * (0.65 if last >= current_target > 0 else 1.0)
    if ask_wall_px > last * 1.002 and ask_wall_px < projected_target and (kalman_vel <= 0 or obi < -0.15):
        # If momentum is stalling below a heavy L2 ask wall, pull target just inside the ask wall
        projected_target = ask_wall_px * 0.999
    elif last >= float(current_target or 0) > 0 and algo_dir == "bullish_accumulation" and kalman_vel >= 0:
        # Runner broke through prior target with bullish flow: extend target dynamically so it keeps running
        projected_target = max(projected_target, round(last + max(base_risk * 0.85, last * (effective_trail_pct / 100.0)), 4))
    dynamic_target = round(max(entry * 1.004, projected_target), 4)

    # Cognitive exit decision: ThomasAi + Moomoo Algo Detection decides when to exit
    should_exit = False
    exit_reason = ""

    if last <= dynamic_stop:
        should_exit = True
        exit_reason = f"thomasai_dynamic_trailing_stop ({algo_type})" if dynamic_stop >= entry else f"thomasai_protective_stop ({algo_type})"
    elif algo_type == "LIQUIDITY_VACUUM_TRAP" and float(fp.get("confidence", 0.0)) >= 0.62 and (pnl_pct > -0.35 or kalman_vel < 0):
        should_exit = True
        exit_reason = f"thomasai_liquidity_trap_exit (conf={fp['confidence']:.2f}, CLV={params.get('recent_clv', 0):+.2f})"
    elif (
        pnl_pct >= 0.40
        and algo_dir == "bearish_distribution"
        and kalman_vel < 0
        and kalman_acc < 0
        and (sell_sweep > buy_sweep + 0.18 or obi < -0.30 or (whale_flow + slicer_flow) < -500_000)
    ):
        should_exit = True
        exit_reason = f"thomasai_algo_distribution_reversal ({algo_type}, OBI={obi:+.2f}, sell_sweep={sell_sweep*100:.0f}%)"
    elif bear_sentry >= 0.78 and conviction < 0.48 and pnl_pct >= 0.25 and kalman_vel < 0:
        should_exit = True
        exit_reason = f"thomasai_bear_sentry_exit (bear={bear_sentry:.2f}, conv={conviction:.2f})"
    elif last >= dynamic_target and (kalman_vel <= 0 or algo_dir != "bullish_accumulation" or sell_sweep > buy_sweep):
        should_exit = True
        exit_reason = f"thomasai_dynamic_target_exhaustion ({algo_type}, target={dynamic_target:.2f})"

    result = {
        "symbol": clean,
        "should_exit": should_exit,
        "exit_reason": exit_reason,
        "dynamic_stop": dynamic_stop,
        "dynamic_target": dynamic_target,
        "high_price": round(high, 4),
        "trail_pct": round(effective_trail_pct, 3),
        "conviction": round(conviction, 4),
        "bear_sentry": round(bear_sentry, 4),
        "kalman_velocity": round(kalman_vel, 6),
        "kalman_acceleration": round(kalman_acc, 6),
        "clv_velocity": round(clv_vel, 6),
        "algo_type": algo_type,
        "algo_direction": algo_dir,
        "algo_confidence": fp.get("confidence", 0.5),
    }
    from .decision_audit import record
    result["audit_id"] = record(brain_path, "dynamic_exit", {
        "symbol": symbol, "decision_ms": now_ms, "entry_price": entry_price,
        "high_price": high_price, "last_price": last_price,
        "current_stop": current_stop, "current_target": current_target,
        "bars": bars, "telemetry": moomoo_telemetry,
        "base_stop_pct": base_stop_pct, "base_trail_pct": base_trail_pct,
        "fingerprint": fp}, result)
    return result


if __name__ == "__main__":
    from . import darwin_qos
    darwin_qos.set_background()
    out = scan_candidates()
    print(json.dumps({"scanned": out["scanned"], "moomoo_enriched": out["moomoo_enriched"], "by_algo_type": out["by_algo_type"]}, indent=2))

"""Shared, candidate-scoped research replay for the live Gainer and ORB bots.

This replays recorded filled entries, not invented entries. It applies the
initial levels recorded by each executor and the high-water exit lines to
native Moomoo OHLCV candles.
"""
from __future__ import annotations

import argparse
import ast
import json
import hashlib
import math
import random
import re
import sqlite3
from bisect import bisect_left
from collections import defaultdict
from datetime import datetime, time as wall_time
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "data"
NY = ZoneInfo("America/New_York")
OPEN_EVENTS = ("live_gainer_discovered", "gainer_detected", "gainer_admitted", "gainer_rejected")

RANDOM_GRID_COUNT = 10_000
TOP_GRID_COUNT = 10


def random_grid_candidates(session: str, as_of_ms: int, count: int = RANDOM_GRID_COUNT) -> list[dict]:
    """Generate reproducible randomized exit-only grids for one 15-minute run."""
    seed_text = f"{session}:{as_of_ms // 900_000}"
    rng = random.Random(int.from_bytes(hashlib.sha256(seed_text.encode()).digest()[:8], "big"))
    baseline = {"stop_scale": 1.0, "trail_scale": 1.0, "target_scale": 1.0,
                "target_70_fraction": 0.70, "profit_line_fractions": [0.5]}
    candidates = [baseline]
    seen = {(1.0, 1.0, 1.0, 0.70, (0.5,))}
    while len(candidates) < count:
        stop = round(rng.uniform(0.50, 1.50), 4)
        trail = round(rng.uniform(0.50, 1.50), 4)
        target = round(rng.uniform(0.50, 2.00), 4)
        target_70 = round(rng.uniform(0.50, 0.95), 4)
        number = rng.randint(0, 4)
        fractions = tuple(sorted((round(rng.uniform(0.05, 0.95), 4) for _ in range(number)), reverse=True))
        key = (stop, trail, target, target_70, fractions)
        if key in seen:
            continue
        seen.add(key)
        candidates.append({"stop_scale": stop, "trail_scale": trail, "target_scale": target,
                           "target_70_fraction": target_70, "profit_line_fractions": list(fractions)})
    return candidates


def _profit_ladder(target_70: float, midpoint: float, fractions: list[float] | tuple[float, ...]) -> list[float]:
    """Downward-cross exits at configured fractions between midpoint and 70%."""
    if not fractions or target_70 <= midpoint:
        return []
    return [midpoint + (target_70 - midpoint) * float(f) for f in sorted(fractions, reverse=True)]


def _at_or_above(price: float, level: float) -> bool:
    return price >= level or math.isclose(price, level, rel_tol=1e-10, abs_tol=1e-8)


def _at_or_below(price: float, level: float) -> bool:
    return price <= level or math.isclose(price, level, rel_tol=1e-10, abs_tol=1e-8)


def session_for(ts_ms: int) -> str:
    return datetime.fromtimestamp(ts_ms / 1000, NY).date().isoformat()


def _gainer_candidates(db: sqlite3.Connection, today: str, include_today: bool) -> dict[str, dict[str, int]]:
    # Convert each actual open timestamp in New York time. SQLite's unixepoch
    # date() is UTC and assigns evening/pre-market records to the wrong session.
    trade_days = {session_for(int(r[0])) for r in db.execute("SELECT DISTINCT opened_ms FROM trades") if r[0] is not None}
    trades = {d for d in trade_days if d <= today} if include_today else {d for d in trade_days if d < today}
    if not trades:
        return {}
    placeholders = ",".join("?" for _ in OPEN_EVENTS)
    rows = db.execute(
        f"SELECT session,symbol,min(ts_ms) FROM plan_events WHERE event IN ({placeholders}) GROUP BY session,symbol",
        OPEN_EVENTS,
    ).fetchall()
    scope: dict[str, dict[str, int]] = defaultdict(dict)
    for session, symbol, detected in rows:
        if session in trades and symbol and detected is not None:
            scope[session][str(symbol).removeprefix("US.")] = int(detected)
    return dict(scope)


def _orb_candidates(db: sqlite3.Connection, today: str, include_today: bool) -> dict[str, dict[str, int]]:
    trade_days = {session_for(int(r[0])) for r in db.execute("SELECT DISTINCT opened_ms FROM trades") if r[0] is not None}
    trades = {d for d in trade_days if d <= today} if include_today else {d for d in trade_days if d < today}
    if not trades:
        return {}
    rows = db.execute("SELECT session,symbol,detected_ms FROM candidates").fetchall()
    scope: dict[str, dict[str, int]] = defaultdict(dict)
    for session, symbol, detected in rows:
        if session in trades and symbol:
            scope[session][str(symbol).removeprefix("US.")] = int(detected or 0)
    return dict(scope)


def _candidate_records(db: sqlite3.Connection, bot: str, scope: dict[str, dict[str, int]]) -> list[dict]:
    """Keep the bot's own candidate decisions/features beside its trade replay."""
    records: dict[tuple[str, str], dict] = {}
    if bot == "gainer":
        for day, symbols in scope.items():
            for symbol, first_seen in symbols.items():
                records[(day, symbol)] = {
                    "session": day, "symbol": symbol, "first_candidate_event_ms": first_seen,
                    "session_end_summary": {"event_counts": {}, "rejection_reasons": {}},
                    "decision_evidence": [],
                }
        for day, symbol, event, count, detail in db.execute(
            "SELECT session,symbol,event,count(*),max(detail) FROM plan_events "
            "WHERE event IN ('live_gainer_discovered','gainer_detected','gainer_admitted','gainer_rejected','open_order','pre_order','held','candidate_demoted','dropped') "
            "GROUP BY session,symbol,event"
        ):
            key = (str(day), str(symbol).removeprefix("US."))
            record = records.get(key)
            if not record:
                continue
            record["session_end_summary"]["event_counts"][event] = int(count)
            if event not in ("gainer_rejected",):
                for ts, body in db.execute(
                    "SELECT ts_ms,detail FROM plan_events WHERE session=? AND symbol=? AND event=? ORDER BY ts_ms",
                    (day, symbol, event),
                ):
                    record["decision_evidence"].append({"ts_ms": int(ts), "event": event, "detail": body})
        for day, symbol, detail in db.execute(
            "SELECT session,symbol,detail FROM plan_events WHERE event='gainer_rejected'"
        ):
            record = records.get((str(day), str(symbol).removeprefix("US.")))
            if not record:
                continue
            try:
                reason = str(json.loads(detail).get("reason") or "unspecified")
            except (TypeError, ValueError):
                reason = "unparsed"
            reasons = record["session_end_summary"]["rejection_reasons"]
            reasons[reason] = reasons.get(reason, 0) + 1
    else:
        for day, symbols in scope.items():
            for symbol, detected in symbols.items():
                row = db.execute(
                    "SELECT coalesce(detected_ms,(SELECT min(ts_ms) FROM signal_observations o WHERE o.session=candidates.session AND o.symbol=candidates.symbol)),approved_ms,status FROM candidates WHERE session=? AND symbol=?",
                    (day, symbol),
                ).fetchone()
                record = {"session": day, "symbol": symbol, "detected_ms": detected}
                if row:
                    record.update(dict(zip(("detected_ms", "approved_ms", "ledger_status_current"), row)))
                record["signal_observations"] = [
                    {"ts_ms": ts, "decision": decision, "reason": reason, "last_price": price,
                     "orh": orh, "orl": orl, "vwap": vwap, "breakout_volume": volume,
                     "risk": risk, "payload": json.loads(payload or "{}")}
                    for ts, decision, reason, price, orh, orl, vwap, volume, risk, payload in db.execute(
                        "SELECT ts_ms,decision,reason,last_price,orh,orl,vwap,breakout_volume,risk,payload_json "
                        "FROM signal_observations WHERE session=? AND symbol=? ORDER BY ts_ms", (day, symbol)
                    )
                ]
                records[(day, symbol)] = record
    return sorted(records.values(), key=lambda r: (r["session"], r["symbol"]))


def _gainer_rows(db: sqlite3.Connection, scope: dict[str, dict[str, int]], telemetry_path: Path | None = None, only_session: str | None = None) -> list[dict]:
    rows = db.execute("""
        SELECT symbol,opened_ms,max(closed_ms),sum(qty),
               sum(entry*qty)/max(sum(qty),1),sum(exit_price*qty)/max(sum(qty),1),
               sum((exit_price-entry)*qty),group_concat(reason,' | ')
        FROM trades GROUP BY symbol,opened_ms ORDER BY opened_ms,symbol
    """).fetchall()
    output = []
    for symbol, opened, closed, qty, entry, exit_price, pnl, reason in rows:
        day = session_for(int(opened))
        ticker = str(symbol).removeprefix("US.")
        if only_session and day != only_session:
            continue
        if day not in scope:
            scope[day] = {}
        detected_ms = scope[day].get(ticker)
        event = db.execute(
            "SELECT ts_ms,detail FROM plan_events WHERE event='open_order' AND session=? AND symbol=? AND ts_ms<=? ORDER BY ts_ms DESC LIMIT 1",
            (day, symbol, int(opened)),
        ).fetchone()
        terms = {}
        if event:
            match = re.search(r"terms\s+(\{.*\})\s*$", str(event[1]))
            if match:
                try:
                    terms = ast.literal_eval(match.group(1))
                except (ValueError, SyntaxError):
                    terms = {}
        # Premarket planned entries may not emit an `open_order` event. Recover
        # their exact candidate terms from the immutable plan-version history,
        # selecting the last version at or before this actual fill.
        if not terms and telemetry_path and telemetry_path.exists():
            with sqlite3.connect(f"file:{telemetry_path}?mode=ro", uri=True) as audit:
                versions = audit.execute(
                    "SELECT payload_json FROM events WHERE kind='plan_version' AND session=? AND received_ns<=? ORDER BY received_ns DESC",
                    (day, int(opened) * 1_000_000),
                ).fetchall()
            for (encoded,) in versions:
                try:
                    plan = json.loads(encoded).get("plan") or {}
                    candidate = next((c for c in plan.get("candidates", [])
                                      if str(c.get("symbol", "")).removeprefix("US.") == ticker), None)
                    if candidate and isinstance(candidate.get("terms"), dict):
                        terms = candidate["terms"]
                        break
                except (TypeError, ValueError):
                    continue
        output.append({
            "bot": "gainer", "session": day, "symbol": ticker,
            "opened_ms": int(opened), "closed_ms": int(closed), "qty": int(qty or 0),
            "entry": float(entry), "actual_exit": float(exit_price), "actual_pnl_usd": float(pnl),
            "actual_reason": str(reason or ""), "candidate_detected_ms": int(detected_ms or opened),
            "initial_stop": float(entry) * (1 - float(terms.get("stop_pct", 0)) / 100),
            "initial_target": float(entry) * (1 + float(terms.get("take_profit_pct", 0)) / 100),
            "trail_pct": float(terms.get("trail_pct", 0)), "initial_level_source": "Gainer open_order terms",
        })
    return output


def _orb_rows(db: sqlite3.Connection, scope: dict[str, dict[str, int]], only_session: str | None = None) -> list[dict]:
    rows = db.execute("""
        SELECT symbol,opened_ms,max(closed_ms),sum(qty),
               sum(entry_price*qty)/max(sum(qty),1),sum(exit_price*qty)/max(sum(qty),1),sum(pnl_usd),group_concat(reason,' | ')
        FROM trades GROUP BY symbol,opened_ms ORDER BY opened_ms,symbol
    """).fetchall()
    output = []
    for symbol, opened, closed, qty, entry, exit_price, pnl, reason in rows:
        day = session_for(int(opened))
        ticker = str(symbol).removeprefix("US.")
        if only_session and day != only_session:
            continue
        if day not in scope:
            scope[day] = {}
        detected_ms = scope[day].get(ticker)
        session_start = int(datetime.strptime(day, "%Y-%m-%d").replace(tzinfo=NY).timestamp() * 1000)
        order = db.execute(
            "SELECT stop_price,target_price,trail_pct,created_ms FROM orders WHERE side='BUY' AND symbol=? AND created_ms>=? AND created_ms<=? ORDER BY created_ms DESC LIMIT 1",
            (ticker, session_start, int(opened)),
        ).fetchone()
        stop, target, trail = (float(order[0] or 0), float(order[1] or 0), float(order[2] or 0)) if order else (0.0, 0.0, 0.0)
        output.append({
            "bot": "orb", "session": day, "symbol": ticker,
            "opened_ms": int(opened), "closed_ms": int(closed), "qty": int(qty or 0),
            "entry": float(entry), "actual_exit": float(exit_price), "actual_pnl_usd": float(pnl),
            "actual_reason": str(reason or ""), "candidate_detected_ms": int(detected_ms or opened),
            "initial_stop": stop, "initial_target": target, "trail_pct": trail,
            "initial_level_source": "ORB filled-entry order ledger",
        })
    return output


def _bars_by_symbol(path: Path, symbols: set[str] | None = None, session: str | None = None) -> dict[str, list[tuple]]:
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
        columns = {r[1] for r in db.execute("PRAGMA table_info(bars)")}
        if "source" not in columns:
            raise RuntimeError("Moomoo candle store has no source column; refusing ambiguous bars")
        where, args = ["source='moomoo'"], []
        if symbols:
            names = sorted({s.removeprefix("US.") for s in symbols} | {"US." + s.removeprefix("US.") for s in symbols})
            where.append("symbol IN (" + ",".join("?" for _ in names) + ")")
            args.extend(names)
        if session:
            start = int(datetime.strptime(session, "%Y-%m-%d").replace(tzinfo=NY).timestamp() * 1000)
            where.append("ts>=? AND ts<?")
            args.extend((start, start + 86_400_000))
        rows = db.execute("SELECT symbol,ts,open,high,low,close,volume FROM bars WHERE " + " AND ".join(where) + " ORDER BY symbol,ts", args).fetchall()
        seen = {(str(r[0]), int(r[1])) for r in rows}
        # Premarket capture has its own Moomoo-native table. Include it in the
        # research input so the 04:00-09:30 tape is not silently omitted.
        if "premarket_bars" in {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}:
            pwhere, pargs = ["source='moomoo'"], []
            if symbols:
                names = sorted({s.removeprefix("US.") for s in symbols} | {"US." + s.removeprefix("US.") for s in symbols})
                pwhere.append("symbol IN (" + ",".join("?" for _ in names) + ")")
                pargs.extend(names)
            if session:
                pwhere.append("ts>=? AND ts<?")
                pargs.extend((start, start + 86_400_000))
            premarket = db.execute("SELECT symbol,ts,open,high,low,close,volume FROM premarket_bars WHERE " + " AND ".join(pwhere), pargs).fetchall()
            rows.extend(r for r in premarket if (str(r[0]), int(r[1])) not in seen)
    grouped: dict[str, list[tuple]] = defaultdict(list)
    for symbol, ts, op, hi, lo, close, volume in rows:
        grouped[str(symbol).removeprefix("US.")].append((int(ts), float(op), float(hi), float(lo), float(close), float(volume)))
    return grouped


def replay_position(trade: dict, bars: list[tuple], cost_bps: float, as_of_ms: int | None = None,
                    grid_config: dict | None = None, preselected_bars: bool = False) -> dict:
    grid_config = grid_config or {"stop_scale": 1.0, "trail_scale": 1.0, "target_scale": 1.0,
                                  "target_70_fraction": 0.70, "profit_line_fractions": [0.5]}
    entry = trade["entry"]
    base_stop_pct = (entry - trade["initial_stop"]) / entry * 100
    base_target_pct = (trade["initial_target"] - entry) / entry * 100
    stop_pct = base_stop_pct * float(grid_config["stop_scale"])
    stop = entry * (1 - stop_pct / 100)
    initial_target = entry * (1 + base_target_pct * float(grid_config["target_scale"]) / 100)
    trail_pct = trade["trail_pct"] * float(grid_config["trail_scale"])
    target_70_fraction = float(grid_config["target_70_fraction"])
    line_fractions = list(grid_config.get("profit_line_fractions", [0.5]))
    if not (0 < stop < entry < initial_target) or not (0 < trail_pct <= 10):
        return {"replay_status": "missing_or_invalid_recorded_initial_levels"}
    reward = initial_target - entry
    high_water = entry
    target100 = initial_target
    target70 = entry + reward * target_70_fraction
    midpoint = (target70 + stop) / 2
    ladder = _profit_ladder(target70, midpoint, line_fractions)
    intermediate = ladder[0] if ladder else midpoint
    if preselected_bars:
        position_bars = bars
    else:
        position_bars = [b for b in bars if b[0] >= trade["opened_ms"] and session_for(b[0]) == trade["session"] and (as_of_ms is None or b[0] <= as_of_ms)]
    if not position_bars:
        return {"replay_status": "no_post_entry_moomoo_bars"}
    close_ms = int(datetime.strptime(trade["session"] + " 15:00", "%Y-%m-%d %H:%M").replace(tzinfo=NY).timestamp() * 1000)

    def finish(fill, reason, ts, fill_method):
        result = _result(trade, fill, reason, ts, high_water, stop, target100, target70,
                         intermediate, midpoint, cost_bps, fill_method)
        result["grid_config"] = dict(grid_config)
        result["additional_profit_line_levels"] = list(ladder)
        return result

    for ts, op, hi, lo, close, _volume in position_bars:
        if ts >= close_ms:
            return finish(op, "session_flatten", ts, "bar_open_at_15:00_ET")
        # Evaluate only lines that were known before this minute. OHLC does not
        # reveal whether its high or low came first, so apply adverse crossings
        # before advancing the high-water mark; new ratchets take effect next bar.
        checks = (
            ("protective_stop", stop, _at_or_below(lo, stop), False),
            ("profit_target_100", target100, _at_or_above(hi, target100), False),
            ("profit_target_70", target70, _at_or_above(high_water, target70) and _at_or_below(lo, target70), True),
            *(("profit_retrace_failure", level, _at_or_above(high_water, level) and _at_or_below(lo, level), True) for level in ladder),
            ("midpoint_failure", midpoint, _at_or_above(high_water, midpoint) and _at_or_below(lo, midpoint), True),
        )
        for reason, level, hit, requires_high in checks:
            if hit and (not requires_high or _at_or_above(high_water, level)):
                # A bar cannot reveal the historical bid at trigger time.
                # Use the line price (or opening gap) as a disclosed estimate.
                if reason == "profit_target_100":
                    # The live rule sells on an upward touch/cross; an opening
                    # gap above the line is filled at the better opening price.
                    fill = max(op, level)
                else:
                    fill = min(op, level) if _at_or_below(op, level) else level
                return finish(fill, reason, ts, "estimated_from_Moomoo_OHLCV_line_or_opening_gap")
        high_water = max(high_water, hi, entry)
        stop = max(stop, high_water * (1 - trail_pct / 100))
        target100 = max(target100, high_water - reward)
        target70 = max(target70, entry + (target100 - entry) * target_70_fraction)
        midpoint = max(midpoint, (target70 + stop) / 2)
        new_ladder = _profit_ladder(target70, midpoint, line_fractions)
        ladder = [max(old, new) for old, new in zip(ladder, new_ladder)] if len(ladder) == len(new_ladder) else new_ladder
        intermediate = ladder[0] if ladder else midpoint
    last = position_bars[-1]
    if as_of_ms is not None:
        result = finish(last[4], "open_at_asof_mark", last[0], "latest recorded Moomoo close; hypothetical position remains open")
        result["replay_status"] = "open_at_asof_mark"
        return result
    if last[0] < close_ms - 60_000:
        return {"replay_status": "insufficient_bars_through_15:00_ET"}
    return finish(last[4], "session_flatten", last[0], "last_moomoo_close_before_15:00_ET")


def _result(trade, fill, reason, ts, high, stop, target100, target70, intermediate, midpoint, cost_bps, fill_method):
    gross = (fill / trade["entry"] - 1) * 100
    return {
        "replay_status": "price_lines_replayed",
        "replay_exit": float(fill), "replay_reason": reason, "replay_exit_ms": int(ts),
        "replay_gross_return_pct": gross, "replay_net_return_pct": gross - cost_bps / 100,
        "fill_estimate": fill_method,
        "high_water_price": high, "protective_stop": stop, "target_100": target100,
        "target_70": target70, "intermediate": intermediate, "midpoint": midpoint,
    }


def optimize_symbol_grids(trades: list[dict], candles: dict[str, list[tuple]], cost_bps: float,
                          as_of_ms: int | None, candidates: list[dict], top_k: int = TOP_GRID_COUNT) -> list[dict]:
    """Score randomized exit-line layouts independently for each traded symbol."""
    by_symbol: dict[str, list[tuple[dict, list[tuple]]]] = defaultdict(list)
    for trade in trades:
        source = candles.get(trade["symbol"], [])
        times = [b[0] for b in source]
        first = bisect_left(times, trade["opened_ms"])
        selected = [b for b in source[first:] if as_of_ms is None or b[0] <= as_of_ms]
        by_symbol[trade["symbol"]].append((trade, selected))

    baseline = {"stop_scale": 1.0, "trail_scale": 1.0, "target_scale": 1.0,
                "target_70_fraction": 0.70, "profit_line_fractions": [0.5]}
    reports = []
    for symbol, positions in sorted(by_symbol.items()):
        if any(not bars for _, bars in positions):
            reports.append({"symbol": symbol, "status": "insufficient_candles_for_grid_search",
                            "position_groups": len(positions), "scored_grids": 0,
                            "reason": "at least one actual fill has no post-entry Moomoo candle"})
            continue

        def score(config: dict) -> dict | None:
            per_trade = []
            for trade, bars in positions:
                result = replay_position(trade, bars, cost_bps, as_of_ms=as_of_ms,
                                         grid_config=config, preselected_bars=True)
                if not result.get("replay_status", "").startswith(("price_lines_replayed", "open_at_asof_mark")):
                    return None
                pnl = trade["qty"] * trade["entry"] * result["replay_net_return_pct"] / 100
                per_trade.append(pnl)
            if not per_trade:
                return None
            equity = peak = max_drawdown = 0.0
            for pnl in per_trade:
                equity += pnl
                peak = max(peak, equity)
                max_drawdown = max(max_drawdown, peak - equity)
            gains = sum(v for v in per_trade if v > 0)
            losses = -sum(v for v in per_trade if v < 0)
            net = sum(per_trade)
            return {
                "net_pnl_usd": round(net, 4),
                "max_drawdown_usd": round(max_drawdown, 4),
                "risk_adjusted_score_usd": round(net - 0.5 * max_drawdown, 4),
                "profit_factor": round(gains / losses, 4) if losses else (None if not gains else 999.0),
                "win_rate_pct": round(100 * sum(v > 0 for v in per_trade) / len(per_trade), 2),
                "worst_position_pnl_usd": round(min(per_trade), 4),
                "best_position_pnl_usd": round(max(per_trade), 4),
                "position_groups": len(per_trade),
                "config": config,
            }

        base = score(baseline)
        ranked = []
        for config in candidates:
            item = score(config)
            if item is not None:
                ranked.append(item)
        if not ranked or base is None:
            reports.append({"symbol": symbol, "status": "insufficient_replay_coverage",
                            "position_groups": len(positions), "scored_grids": len(ranked)})
            continue
        ranked.sort(key=lambda x: (x["risk_adjusted_score_usd"], x["net_pnl_usd"], x["worst_position_pnl_usd"]), reverse=True)
        reports.append({
            "symbol": symbol,
            "status": "in_sample_research_only" if len(positions) < 3 else "in_sample_research",
            "position_groups": len(positions),
            "scored_grids": len(ranked),
            "baseline": base,
            "best": ranked[0],
            "improvement_vs_baseline_usd": round(ranked[0]["net_pnl_usd"] - base["net_pnl_usd"], 4),
            "top_grids": ranked[:top_k],
            "warning": "Optimized and scored on the same single session; this is in-sample research, not a live setting recommendation.",
        })
    return reports


def run_session(session: str, data: Path = DATA, cost_bps: float = 10.0, as_of_ms: int | None = None) -> dict:
    """Replay only the supplied New York session, including premarket candles."""
    datetime.strptime(session, "%Y-%m-%d")
    grid_seed_ms = as_of_ms if as_of_ms is not None else int(datetime.now(NY).timestamp() * 1000)
    grid_configs = random_grid_candidates(session, grid_seed_ms)
    reports = {}
    journal_path, orb_path = data / "journal.sqlite3", data / "orb.sqlite3"
    for name, ledger in (("gainer", journal_path), ("orb", orb_path)):
        with sqlite3.connect(f"file:{ledger}?mode=ro", uri=True) as db:
            if name == "gainer":
                scope = _gainer_candidates(db, session, True)
                scope.setdefault(session, {})
                trades = _gainer_rows(db, scope, data / "telemetry.sqlite3", only_session=session)
            else:
                scope = _orb_candidates(db, session, True)
                scope.setdefault(session, {})
                trades = _orb_rows(db, scope, only_session=session)
        bars = _bars_by_symbol(data / "common1m.sqlite3", {t["symbol"] for t in trades}, session)
        for trade in trades:
            result = replay_position(trade, bars.get(trade["symbol"], []), cost_bps, as_of_ms=as_of_ms)
            trade.update(result)
            trade["actual_return_pct"] = (trade["actual_exit"] / trade["entry"] - 1) * 100
            if trade.get("replay_status") == "insufficient_bars_through_15:00_ET":
                trade["replay_status"] = "missing_session_flatten_bar"
            if trade.get("replay_status", "").startswith(("price_lines_replayed", "open_at_asof_mark")):
                trade["replay_net_pnl_usd"] = trade["qty"] * trade["entry"] * trade["replay_net_return_pct"] / 100
            else:
                trade["replay_net_pnl_usd"] = None
        complete = [t for t in trades if t.get("replay_net_pnl_usd") is not None]
        candle_replayed = [t for t in complete if t.get("replay_status", "").startswith(("price_lines_replayed", "open_at_asof_mark"))]
        reports[name] = {
            "recorded_positions": len(trades), "replayed_positions": len(complete),
            "candle_replayed_positions": len(candle_replayed),
            "missing_replay_count": len(trades) - len(complete),
            "actual_net_pnl_usd": sum(t["actual_pnl_usd"] for t in trades),
            "replay_net_pnl_usd": sum(t["replay_net_pnl_usd"] for t in complete),
            "trades": trades,
            "exit_grid_optimization": {
                "candidate_grids_per_run": len(grid_configs),
                "grid_dimensions": {"initial_stop_scale": [0.5, 1.5], "high_water_trail_scale": [0.5, 1.5],
                                    "full_target_scale": [0.5, 2.0], "70_percent_line_fraction": [0.5, 0.95],
                                    "extra_downward_lines_between_midpoint_and_70": [0, 4]},
                "refresh_interval_minutes": 15,
                "symbols": optimize_symbol_grids(trades, bars, cost_bps, as_of_ms, grid_configs),
                "live_bot_settings_changed": False,
            },
        }
    return {
        "schema": 2,
        "session": session,
        "as_of_new_york": datetime.now(NY).isoformat(),
        "scope": "today's actual filled positions only; premarket and regular session; no other sessions",
        "as_of_mode": "live mark-to-latest-Moomoo-candle" if as_of_ms is not None else "completed-session replay",
        "optimizer_candidate_grid_count": len(grid_configs),
        "cost_bps_per_round_trip": cost_bps,
        "method": "actual fills; recorded per-position entry terms; high-water ratcheting exit lines; Moomoo-native 1-minute candles",
        "limitations": ["Minute OHLC cannot reconstruct the exact intrabar order of touches or executable bid; estimated line fills are not broker fills.", "Positions without complete Moomoo candles are excluded from replay P&L and listed as data gaps."],
        "bots": reports,
    }


def run(bot: str, data: Path = DATA, cost_bps: float = 10.0) -> dict:
    # Research is offline work and must not compete with the live bots during
    # premarket, regular hours, or extended hours. Allow it overnight after
    # 20:00 ET and on weekends; never include today's session before 20:00 ET.
    now_ny = datetime.now(NY)
    if now_ny.weekday() < 5 and wall_time(4, 0) <= now_ny.time() < wall_time(20, 0):
        raise RuntimeError(
            "research replay is disabled during the US trading window; "
            "run after 20:00 ET when extended hours have closed"
        )
    today = now_ny.date().isoformat()
    report = run_session(today, data=data, cost_bps=cost_bps)
    if bot != "all":
        report["bots"] = {bot: report["bots"][bot]}
    report["execution_window"] = "after 20:00 ET or weekends; blocked 04:00-20:00 ET on weekdays"
    return report


def main():
    parser = argparse.ArgumentParser(description="Replay both live paper bots' recorded trades with their current exit lines")
    parser.add_argument("--bot", choices=("all", "gainer", "orb"), default="all")
    parser.add_argument("--data", type=Path, default=DATA)
    parser.add_argument("--cost-bps", type=float, default=10.0)
    parser.add_argument("--output", type=Path, default=DATA / "strategy_research.json")
    args = parser.parse_args()
    report = run(args.bot, args.data, args.cost_bps)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({name: {k: v for k, v in metrics.items() if k not in ("trades", "candidate_records")} for name, metrics in report["bots"].items()}, indent=2))


if __name__ == "__main__":
    main()

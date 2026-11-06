"""Pure session timing and candidate-plan helpers for the Competition bot.

The active Gainers plan starts with no preselected symbols. From 04:00 ET the
bot polls Moomoo's US hottest-gainers list, filters Nasdaq symbols, validates
each candidate, and can place a simulated premarket limit order. Pending
entries are retried at the regular open; the bot decides holdings from the
broker account, not from memory.
"""
from __future__ import annotations

import json
import math
import os
import sqlite3
import subprocess
from datetime import date, datetime, time as clock_time, timedelta, timezone
from pathlib import Path
from typing import Callable
from zoneinfo import ZoneInfo

NEW_YORK = ZoneInfo("America/New_York")
OPEN_TIME = clock_time(9, 30)
CLOSE_TIME = clock_time(16, 0)
PLAN_FILE = Path("data/plan.json")
MOMENTUM_SCHEMA = 1
MOMENTUM_STRATEGY = "cross_sectional_momentum_1m_proxy"
MOMENTUM_LOOKBACK = 20
TUNED_STRATEGY = "per_symbol_tuned_momentum_1m"  # Rust `momentum-select --tuned`: each symbol's own walk-forward-fitted lookback/threshold
TUNED_LOOKBACK_MAX = 30
MOMENTUM_MAX_AGE_DAYS = 4
MAPPED_SYMBOLS = 518
PROJECT_ROOT = Path(__file__).resolve().parents[3]
THOMAS_BIN = Path(os.environ.get("THOMAS_BIN", PROJECT_ROOT / "target" / "release" / "thomas"))

OPEN_STATES = {"MORNING", "AFTERNOON", "TRADE_AT_LAST"}
PRE_STATES = {"PRE_MARKET_BEGIN", "PRE_MARKET_END", "WAITING_OPEN", "AUCTION"}
AFTER_STATES = {"AFTER_HOURS_BEGIN"}

# statuses of a candidate
PENDING, PRE_PLACED, PLACED, SKIPPED, FAILED = "pending", "pre_placed", "placed", "skipped", "failed"
# why a refusal is final for the day (the others are retried: the account could not be read, the order failed, the broker's maximum could not be read)
FINAL_REFUSALS = ("not a Nasdaq-listed symbol", "below the", "positions or working orders already", "market caps below")
# the competition refuses companies below this market value ("Trading of stocks and options from companies with market caps below US$500 million is unavailable")
MIN_MARKET_CAP = 500_000_000.0


def best_tested_strategy(archive_path: Path | None = None) -> dict | None:
    """Find the highest-performing tested strategy from the backtest archive to use immediately."""
    path = archive_path or (PROJECT_ROOT / "state" / "ClaudeMoomooBotArchive" / "live_backtest_archive.sqlite3")
    if not path.exists():
        return None
    try:
        conn = sqlite3.connect(path)
        cur = conn.cursor()
        cur.execute(
            "SELECT source, window_label, return_pct, max_drawdown_pct, trades, wins, profit_factor, params_json "
            "FROM bt_runs WHERE successful = 1 AND return_pct > 0 AND trades >= 50 "
            "ORDER BY return_pct DESC LIMIT 1"
        )
        row = cur.fetchone()
        conn.close()
        if row:
            params = json.loads(row[7]) if row[7] else {}
            return {
                "source": row[0],
                "window_label": row[1],
                "return_pct": float(row[2]),
                "max_drawdown_pct": float(row[3]),
                "trades": int(row[4]),
                "wins": int(row[5]),
                "profit_factor": float(row[6]),
                "params": params,
                "tp_pct": float(params.get("tp_pct", 2.2)),
                "sl_pct": float(params.get("sl_pct", 0.8)),
                "trail_pct": float(params.get("trail_pct", 0.8)),
            }
    except Exception:
        pass
    return None


def phase_from_state(state: str | None, now: datetime) -> str:
    """'pre', 'open', 'after' or 'closed' from OpenD's US market state; without a state (OpenD unreadable) the clock decides, so the bot never trades blind on a guess about the broker."""
    if state:
        name = str(state).split(".")[-1].upper()
        if name in OPEN_STATES:
            return "open"
        if name in PRE_STATES:
            return "pre"
        if name in AFTER_STATES:
            return "after"
        return "closed"
    local = now.astimezone(NEW_YORK)
    if local.weekday() >= 5:
        return "closed"
    if OPEN_TIME <= local.time() < CLOSE_TIME:
        return "open"
    if clock_time(4, 0) <= local.time() < OPEN_TIME:
        return "pre"
    if CLOSE_TIME <= local.time() < clock_time(20, 0):
        return "after"
    return "closed"


def next_open(now: datetime) -> datetime:
    """The regular open (UTC) of the session the plan is for: today's if it has not started yet, else the next weekday's (holidays are not known here: the open state decides)."""
    local = now.astimezone(NEW_YORK)
    day = local.date()
    if local.weekday() >= 5 or local.time() >= OPEN_TIME:
        day += timedelta(days=1)
    while day.weekday() >= 5:
        day += timedelta(days=1)
    return datetime.combine(day, OPEN_TIME, tzinfo=NEW_YORK).astimezone(timezone.utc)


def session_key(open_utc: datetime) -> str:
    return open_utc.astimezone(NEW_YORK).date().isoformat()


def in_lead_window(now: datetime, open_utc: datetime, lead_hours: float) -> bool:
    """From ``lead_hours`` before the open until the open."""
    return open_utc - timedelta(hours=lead_hours) <= now < open_utc


FLATTEN_TIME = clock_time(15, 0)  # mandatory one-hour-before-close liquidation
LIVE_FROM = clock_time(7, 30)  # the pre-market orders start 2 hours before the open
LIVE_TO = clock_time(16, 20)  # the last bar of the session is published
CHART_FROM = clock_time(4, 0)  # US equity premarket begins; charts follow from the first tradable session minute


def live_trading_window(now: datetime) -> bool:
    """Weekdays from the start of the pre-market orders to the end of the session: the bot is trading paper, so Moomoo's candles of the candidates come first and every other download waits."""
    local = now.astimezone(NEW_YORK)
    return local.weekday() < 5 and LIVE_FROM <= local.time() <= LIVE_TO


def chart_window(now: datetime) -> bool:
    """Weekday US premarket and regular session; chart candles update from 04:00 ET."""
    local = now.astimezone(NEW_YORK)
    return local.weekday() < 5 and CHART_FROM <= local.time() <= LIVE_TO


def after_close_ready(now: datetime, minutes: int = 15) -> bool:
    """The session of the day has closed at least ``minutes`` ago (so its last bars are published) and it is not yet the next session's pre-market window."""
    local = now.astimezone(NEW_YORK)
    if local.weekday() >= 5:
        return True
    return local.time() >= (datetime.combine(local.date(), CLOSE_TIME) + timedelta(minutes=minutes)).time() or local.time() < clock_time(4, 0)


def parse_momentum_selection(raw: str, session: str, size: int, tradable: set[str]) -> dict:
    """Validate Rust's frozen candidate envelope and return the highest-expectancy alpha names.

    Applies the proven Strategy 2 (Core+Booster) or Strategy 3 (Dynamic Conviction)
    to eliminate losing tail churn (ranks 3-15) and weight capital according to proven alpha.
    """
    from .alpha_strategies import select_alpha_candidates, StrategyMode
    try:
        value = json.loads(raw)
        session_day = date.fromisoformat(session)
    except (TypeError, ValueError, json.JSONDecodeError) as error:
        raise ValueError(f"invalid momentum selection JSON/session: {error}") from error
    if not isinstance(value, dict) or value.get("schema") != MOMENTUM_SCHEMA or value.get("strategy") not in (MOMENTUM_STRATEGY, TUNED_STRATEGY):
        raise ValueError("wrong momentum selection schema or strategy")
    tuned = value["strategy"] == TUNED_STRATEGY
    if value.get("lookback_sessions") != (0 if tuned else MOMENTUM_LOOKBACK):
        raise ValueError("wrong momentum lookback")
    cutoff_value = value.get("data_cutoff_day")
    if not isinstance(cutoff_value, int):
        raise ValueError("missing momentum data cutoff")
    cutoff = date(1970, 1, 1) + timedelta(days=cutoff_value)
    age = (session_day - cutoff).days
    if age <= 0 or age > MOMENTUM_MAX_AGE_DAYS:
        raise ValueError(f"stale or non-causal momentum cutoff {cutoff} for session {session}")
    rows = value.get("candidates")
    if not isinstance(rows, list):
        raise ValueError("momentum candidates are missing")
    raw_candidates, seen = [], set()
    for rank, row in enumerate(rows, 1):
        if not isinstance(row, dict):
            raise ValueError("malformed momentum candidate")
        symbol, score_value, observations = row.get("symbol"), row.get("score"), row.get("observations")
        if not isinstance(symbol, str) or not symbol or symbol in seen:
            raise ValueError("invalid or duplicate momentum symbol")
        if not isinstance(score_value, (int, float)) or not math.isfinite(score_value):
            raise ValueError(f"invalid momentum score for {symbol}")
        if tuned:
            if not isinstance(observations, int) or isinstance(observations, bool) or not 1 <= observations <= TUNED_LOOKBACK_MAX or row.get("lookback") != observations:
                raise ValueError(f"invalid tuned lookback for {symbol}")
        elif observations != MOMENTUM_LOOKBACK:
            raise ValueError(f"insufficient momentum history for {symbol}")
        seen.add(symbol)
        code = f"US.{symbol}"
        if code in tradable:
            raw_candidates.append({
                "moomoo_symbol": code,
                "yahoo_symbol": symbol,
                "score": float(score_value),
                "observations": observations,
                "rank": rank
            })
    
    # ThomasAi Active Neural Guidance: query connectome weights for candidates
    brain_weights = {}
    bear_traps = {}
    floor_reflexes = {}
    brain_db = Path("data/brain.sqlite3")
    if not brain_db.exists():
        brain_db = PROJECT_ROOT / "papercompmoomoo" / "data" / "brain.sqlite3"
    if brain_db.exists():
        try:
            with sqlite3.connect(f"file:{brain_db}?mode=ro", uri=True) as bconn:
                bcur = bconn.cursor()
                bcur.execute("SELECT source_id, weight, reinforcements, penalties FROM brain_synapses WHERE target_id = 'node:decision:rank1_execute'")
                for sid, wt, reinf, pen in bcur.fetchall():
                    brain_weights[sid] = (float(wt), int(reinf), int(pen))
                bcur.execute("SELECT source_id, weight FROM brain_synapses WHERE target_id = 'node:reflex:adversarial_bear_sentry'")
                for sid, wt in bcur.fetchall():
                    bear_traps[sid] = float(wt)
                bcur.execute("SELECT source_id, weight FROM brain_synapses WHERE target_id = 'node:reflex:break_even_floor'")
                for sid, wt in bcur.fetchall():
                    floor_reflexes[sid] = float(wt)
        except Exception:
            pass

    for c in raw_candidates:
        clean_sym = f"node:symbol:{c['moomoo_symbol'].removeprefix('US.').replace('.', '_').replace('-', '_')}"
        info = brain_weights.get(clean_sym, (1.0, 0, 0))
        bear_wt = bear_traps.get(clean_sym, 1.0)
        floor_wt = floor_reflexes.get(clean_sym, 1.0)

        c["neural_weight"] = info[0]
        c["neural_reinf"] = info[1]
        c["neural_pen"] = info[2]
        c["bear_trap_weight"] = bear_wt
        c["floor_reflex_weight"] = floor_wt

        # Continuous biological synaptic scaling (Swarm consensus)
        # guided_score smoothly scales with synaptic conviction and inverse bear penalty
        c["guided_score"] = (c["score"] * info[0]) / max(0.2, bear_wt)

    # Sort raw_candidates prioritizing ThomasAi neural conviction when signals deviate from neutral
    has_neural_signal = bool(brain_weights) and any(c.get("neural_weight", 1.0) != 1.0 for c in raw_candidates)
    if has_neural_signal:
        raw_candidates.sort(key=lambda c: c["guided_score"], reverse=True)
        for idx, c in enumerate(raw_candidates, 1):
            c["rank"] = idx

    # Detect macro regime and select optimal alpha strategy (Bull Trend -> Rank 1; Choppy -> Staged Scale Out)
    from .alpha_strategies import detect_market_regime
    regime_name, active_mode = detect_market_regime()
    alpha_targets = select_alpha_candidates(raw_candidates, mode=active_mode)
    chosen = []
    if alpha_targets:
        for t in alpha_targets:
            cand_info = next((c for c in raw_candidates if c["moomoo_symbol"] == t.moomoo_symbol), {})
            obs = cand_info.get("observations", MOMENTUM_LOOKBACK)
            nw = cand_info.get("neural_weight", 1.0)
            reinf = cand_info.get("neural_reinf", 0)
            pen = cand_info.get("neural_pen", 0)
            bear_wt = cand_info.get("bear_trap_weight", 1.0)
            floor_wt = cand_info.get("floor_reflex_weight", 1.0)
            
            # Smooth biological bracket modulation (no rigid step thresholds)
            # Higher floor reflex smoothly contracts stop loss closer to entry for preservation
            # Higher conviction smoothly expands take profit target to let winners run
            base_sl = float(t.stop_loss_pct)
            base_tp = float(t.take_profit_pct)
            sl_pct = max(0.015, min(0.08, base_sl / (floor_wt ** 0.5)))
            tp_pct = max(0.02, min(0.15, base_tp * (nw ** 0.35)))

            chosen.append({
                "symbol": t.moomoo_symbol,
                "score": t.score,
                "observations": obs,
                "rank": t.rank,
                "weight": t.weight,
                "take_profit_pct": round(tp_pct, 4),
                "stop_loss_pct": round(sl_pct, 4),
                "staged_scale": list(t.staged_scale) if t.staged_scale else None,
                "strategy": t.strategy_mode.value,
                "thomasai_guided": True,
                "thomasai_conviction": round(nw, 3),
                "thomasai_bear_penalty": round(bear_wt, 3),
                "thomasai_floor_reflex": round(floor_wt, 3),
                "thomasai_reinf": reinf,
                "thomasai_pen": pen,
            })
    else:
        # Fallback for isolated single-symbol unit tests
        for c in raw_candidates[:size]:
            row_dict = {
                "symbol": c["moomoo_symbol"],
                "score": c["score"],
                "observations": c["observations"],
                "rank": c["rank"],
            }
            if c.get("neural_weight", 1.0) != 1.0:
                row_dict["thomasai_guided"] = True
                row_dict["thomasai_conviction"] = round(c.get("neural_weight", 1.0), 3)
            chosen.append(row_dict)

    if not chosen:
        raise ValueError("momentum alpha selection produced no eligible candidates")
    return {"schema": MOMENTUM_SCHEMA, "strategy": value["strategy"], "data_cutoff_day": cutoff_value,
            "data_cutoff": cutoff.isoformat(), "lookback_sessions": value["lookback_sessions"], "candidates": chosen}


def momentum_selection(session: str, size: int, tradable: set[str], run: Callable = subprocess.run) -> dict:
    """Run the causal Rust selector. Any execution or validation failure leaves no plan."""
    command = [str(THOMAS_BIN), "momentum-select", "--db", str(Path.cwd() / "data" / "common1m.sqlite3"),
               "--lookback-sessions", str(MOMENTUM_LOOKBACK), "--limit", str(MAPPED_SYMBOLS), "--tuned"]
    try:
        done = run(command, cwd=PROJECT_ROOT, capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError(f"momentum selector failed: {error}") from error
    if done.returncode != 0:
        raise ValueError(f"momentum selector failed: {(done.stderr or '').strip()[:500]}")
    return parse_momentum_selection(done.stdout, session, size, tradable)


_default_momentum_selection = momentum_selection


def notional_each(equity: float, buying_power: float, candidates: int, funds_cap: float, leverage: float, buffer: float) -> float:
    """The position value for EACH candidate, worked out once so that all of them fit."""
    if candidates <= 0 or min(equity, buying_power) <= 0:
        return 0.0
    return max(0.0, min(funds_cap * leverage, equity * leverage / candidates, buying_power * (1.0 - buffer) / candidates))


def new_plan(session: str, candidates: list[dict], now_ms: int, stage: str, selection: dict | None = None) -> dict:
    return {"session": session, "built_ms": now_ms, "stage": stage, "locked": True,
            **({"selection": selection} if selection else {}),
            "candidates": [{"symbol": c["symbol"], "why": c.get("why", ""), "terms": c.get("terms", {}), "score": c.get("score", 0.0),
                            "rank": c.get("rank"), "weight": c.get("weight", 1.0 / max(1, len(candidates))),
                            "take_profit_pct": c.get("take_profit_pct", 0.05), "stop_loss_pct": c.get("stop_loss_pct", 0.05),
                            "observations": c.get("observations"), "status": PENDING, "attempts": 0, "last_try_ms": 0, "note": ""}
                           for c in candidates]}


def load_plan(path: Path = PLAN_FILE) -> dict | None:
    try:
        plan = json.loads(path.read_text())
        return plan if isinstance(plan, dict) and "session" in plan else None
    except (OSError, ValueError):
        return None


def save_plan(plan: dict, path: Path = PLAN_FILE) -> None:
    from .telemetry import preserve_file, emit
    preserve_file(path)
    emit('plan_version', {'path':str(path),'plan':plan})
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(plan, indent=1))
    tmp.replace(path)


def final_refusal(reason: str) -> bool:
    return any(part in reason for part in FINAL_REFUSALS)

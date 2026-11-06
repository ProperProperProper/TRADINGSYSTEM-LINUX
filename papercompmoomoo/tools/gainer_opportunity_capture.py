"""Low-impact, read-only shadow capture of Moomoo Gainer leaderboard outcomes.

This process never connects to Moomoo and never writes to either trading bot's
state. It samples the atomic mover snapshot already published by Gainer, imports
that bot's immutable decision events, and stores a separate append-only record.
"""
from __future__ import annotations

import json
import math
import sqlite3
import time
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "data"
BOT_DATA = ROOT / "papercompmoomoo" / "data"
NY = ZoneInfo("America/New_York")
SAMPLE_DB = BOT_DATA / "gainer_opportunities.sqlite3"
MOVER_FILE = BOT_DATA / "shared_moomoo_movers.json"
JOURNAL = BOT_DATA / "journal.sqlite3"
NASDAQ_FILE = BOT_DATA / "nasdaq_common.json"
POLL_SECONDS = 2.0

SCHEMA = """
CREATE TABLE IF NOT EXISTS detections(
    detection_key TEXT PRIMARY KEY, session TEXT NOT NULL, symbol TEXT NOT NULL,
    detected_ms INTEGER NOT NULL, entry_price REAL, source TEXT NOT NULL,
    payload_json TEXT NOT NULL
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS detections_session_symbol ON detections(session,symbol,detected_ms);
CREATE TABLE IF NOT EXISTS observations(
    session TEXT NOT NULL, symbol TEXT NOT NULL, observed_ms INTEGER NOT NULL,
    price REAL NOT NULL, change_pct REAL, volume REAL, turnover REAL, rvol REAL,
    payload_json TEXT NOT NULL, PRIMARY KEY(session,symbol,observed_ms)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS observations_session_symbol ON observations(session,symbol,observed_ms);
CREATE TABLE IF NOT EXISTS decisions(
    source_event_id INTEGER PRIMARY KEY, session TEXT NOT NULL, symbol TEXT NOT NULL,
    event_ms INTEGER NOT NULL, event TEXT NOT NULL, detail TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS decisions_session_symbol ON decisions(session,symbol,event_ms);
CREATE TABLE IF NOT EXISTS bar_observations(
    session TEXT NOT NULL, symbol TEXT NOT NULL, bar_end_ms INTEGER NOT NULL,
    open REAL NOT NULL, high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL,
    volume REAL NOT NULL, source TEXT NOT NULL, PRIMARY KEY(session,symbol,bar_end_ms)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS bars_session_symbol ON bar_observations(session,symbol,bar_end_ms);
CREATE TRIGGER IF NOT EXISTS detections_no_update BEFORE UPDATE ON detections
    BEGIN SELECT RAISE(ABORT,'immutable opportunity detection'); END;
CREATE TRIGGER IF NOT EXISTS detections_no_delete BEFORE DELETE ON detections
    BEGIN SELECT RAISE(ABORT,'immutable opportunity detection'); END;
CREATE TRIGGER IF NOT EXISTS observations_no_update BEFORE UPDATE ON observations
    BEGIN SELECT RAISE(ABORT,'immutable opportunity observation'); END;
CREATE TRIGGER IF NOT EXISTS observations_no_delete BEFORE DELETE ON observations
    BEGIN SELECT RAISE(ABORT,'immutable opportunity observation'); END;
CREATE TRIGGER IF NOT EXISTS decisions_no_update BEFORE UPDATE ON decisions
    BEGIN SELECT RAISE(ABORT,'immutable opportunity decision'); END;
CREATE TRIGGER IF NOT EXISTS decisions_no_delete BEFORE DELETE ON decisions
    BEGIN SELECT RAISE(ABORT,'immutable opportunity decision'); END;
CREATE TRIGGER IF NOT EXISTS bars_no_update BEFORE UPDATE ON bar_observations
    BEGIN SELECT RAISE(ABORT,'immutable opportunity candle'); END;
CREATE TRIGGER IF NOT EXISTS bars_no_delete BEFORE DELETE ON bar_observations
    BEGIN SELECT RAISE(ABORT,'immutable opportunity candle'); END;
"""


def readonly(path: Path) -> sqlite3.Connection | None:
    try:
        db = sqlite3.connect(f"file:{path}?mode=ro", uri=True, timeout=2)
        db.execute("PRAGMA query_only=ON")
        return db
    except sqlite3.Error:
        return None


def finite(value) -> float | None:
    try:
        v = float(value)
        return v if math.isfinite(v) else None
    except (TypeError, ValueError):
        return None


def json_safe(value):
    if isinstance(value, dict):
        return {str(k): json_safe(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [json_safe(v) for v in value]
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    return value


def current_session(now_ms: int) -> str:
    return datetime.fromtimestamp(now_ms / 1000, NY).date().isoformat()


def load_nasdaq() -> set[str]:
    try:
        values = json.loads(NASDAQ_FILE.read_text())
        return {str(s) if str(s).startswith("US.") else f"US.{s}" for s in values}
    except (OSError, ValueError, TypeError):
        return set()


def import_decisions(out: sqlite3.Connection, session: str, last_event_id: int) -> int:
    source = readonly(JOURNAL)
    if source is None:
        return last_event_id
    rows = []
    latest_id = last_event_id
    try:
        latest_id = int(source.execute("SELECT coalesce(max(id),?) FROM plan_events WHERE session=?", (last_event_id,session)).fetchone()[0])
        rows = source.execute(
            "SELECT id,session,ts_ms,symbol,event,detail FROM plan_events "
            "WHERE session=? AND id>? AND symbol IS NOT NULL AND "
            "(event LIKE 'gainer_%' OR event IN "
            "('live_gainer_discovered','pre_order','pre_order_refused','open_order',"
            "'open_order_refused','skipped','failed','candidate_demoted')) ORDER BY id",
            (session,last_event_id),
        ).fetchall()
        with out:
            out.executemany(
                "INSERT OR IGNORE INTO decisions(source_event_id,session,symbol,event_ms,event,detail) VALUES(?,?,?,?,?,?)",
                [(int(i), str(day), str(sym).removeprefix("US."), int(ts), str(event), str(detail))
                 for i, day, ts, sym, event, detail in rows],
            )
            # Actual Gainer detection events carry the bot's own detected time
            # and price, and take precedence over when this sampler first saw it.
            for i, day, ts, sym, event, detail in rows:
                if event not in ("gainer_detected", "live_gainer_discovered"):
                    continue
                try:
                    payload = json.loads(detail)
                except (TypeError, ValueError):
                    payload = {"raw_detail": str(detail)}
                encoded = json.dumps(json_safe(payload), sort_keys=True, separators=(",", ":"), allow_nan=False)
                out.execute(
                    "INSERT OR IGNORE INTO detections(detection_key,session,symbol,detected_ms,entry_price,source,payload_json) "
                    "VALUES(?,?,?,?,?,?,?)",
                    (f"gainer:{i}", str(day), str(sym).removeprefix("US."), int(ts), finite(payload.get("price")),
                     "gainer_bot", encoded),
                )
    except sqlite3.Error:
        pass
    finally:
        source.close()
    return max(latest_id,max((int(row[0]) for row in rows),default=last_event_id))


def capture_once(out: sqlite3.Connection, nasdaq: set[str], last_snapshot: int, last_event_id: int) -> tuple[int,int]:
    now_ms = time.time_ns() // 1_000_000
    session = current_session(now_ms)
    last_event_id = import_decisions(out, session, last_event_id)
    try:
        payload = json.loads(MOVER_FILE.read_text())
        snapshot_ms = int(payload["observed_ms"])
        rows = payload["rows"]
    except (OSError, ValueError, TypeError, KeyError):
        return last_snapshot,last_event_id
    if payload.get("source") != "moomoo_us_hottest_gainers" or snapshot_ms <= last_snapshot or snapshot_ms > now_ms + 2_000:
        return last_snapshot,last_event_id
    if not isinstance(rows, list):
        return last_snapshot,last_event_id

    inserts = []
    detections = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        symbol = str(row.get("security") or "")
        if symbol not in nasdaq:
            continue
        price = finite(row.get("price"))
        if not symbol.startswith("US.") or not price or price <= 0:
            continue
        ticker = symbol.removeprefix("US.")
        clean = json_safe({k: v for k, v in row.items() if isinstance(v, (str, int, float, bool)) or v is None})
        encoded = json.dumps(clean, sort_keys=True, separators=(",", ":"), allow_nan=False)
        inserts.append((session, ticker, snapshot_ms, price, finite(row.get("change_ratio")),
                        finite(row.get("volume")), finite(row.get("turnover")),
                        finite(row.get("volume_ratio")), encoded))
        detections.append((f"snapshot:{session}:{ticker}", session, ticker, snapshot_ms, price,
                           "leaderboard_first_observed", encoded))
    if inserts:
        with out:
            out.executemany(
                "INSERT OR IGNORE INTO observations(session,symbol,observed_ms,price,change_pct,volume,turnover,rvol,payload_json) "
                "VALUES(?,?,?,?,?,?,?,?,?)", inserts,
            )
            out.executemany(
                "INSERT OR IGNORE INTO detections(detection_key,session,symbol,detected_ms,entry_price,source,payload_json) "
                "VALUES(?,?,?,?,?,?,?)", detections,
            )
    return snapshot_ms,last_event_id


def capture_available_bars(out: sqlite3.Connection, session: str, now_ms: int) -> None:
    """Backfill and append candidate-specific Moomoo bars already in the shared store."""
    source = readonly(DATA / "common1m.sqlite3")
    if source is None:
        return
    try:
        symbols = out.execute(
            "SELECT symbol, MIN(detected_ms) FROM detections WHERE session=? GROUP BY symbol",
            (session,),
        ).fetchall()
        if not symbols:
            return
        params: list[int | str] = []
        values = []
        for symbol, detected_ms in symbols:
            values.append("(?,?)")
            params.extend((str(symbol), int(detected_ms) + 60_000))
        session_midnight, _, _ = session_bounds_utc(now_ms)
        params.extend((session_midnight + 4 * 3_600_000, now_ms,
                       session_midnight + 4 * 3_600_000, now_ms))
        query = (
            "WITH candidates(symbol,first_full_bar_ms) AS (VALUES " + ",".join(values) + "), "
            "all_bars AS ("
            "SELECT symbol,ts,open,high,low,close,volume FROM bars "
            "WHERE source='moomoo' AND ts>=? AND ts<=? UNION ALL "
            "SELECT symbol,ts,open,high,low,close,volume FROM premarket_bars "
            "WHERE ts>=? AND ts<=?) "
            "SELECT c.symbol,b.ts,b.open,b.high,b.low,b.close,b.volume FROM candidates c "
            "JOIN all_bars b ON b.symbol=c.symbol AND b.ts>=c.first_full_bar_ms "
            "ORDER BY b.ts,c.symbol"
        )
        rows = source.execute(query, params).fetchall()
        if rows:
            with out:
                out.executemany(
                    "INSERT OR IGNORE INTO bar_observations(session,symbol,bar_end_ms,open,high,low,close,volume,source) "
                    "VALUES(?,?,?,?,?,?,?,?, 'moomoo')",
                    [(session, str(sym), int(ts), float(o), float(h), float(l), float(c), float(v))
                     for sym, ts, o, h, l, c, v in rows],
                )
    except sqlite3.Error:
        pass
    finally:
        source.close()


def session_bounds_utc(now_ms: int) -> tuple[int, int, str]:
    local = datetime.fromtimestamp(now_ms / 1000, NY)
    start_local = local.replace(hour=0, minute=0, second=0, microsecond=0)
    start_ms = int(start_local.timestamp() * 1000)
    return start_ms, start_ms + 86_400_000, local.date().isoformat()


def main() -> None:
    SAMPLE_DB.parent.mkdir(parents=True, exist_ok=True)
    nasdaq = load_nasdaq()
    out = sqlite3.connect(SAMPLE_DB, timeout=10)
    out.execute("PRAGMA journal_mode=WAL")
    out.execute("PRAGMA synchronous=FULL")
    out.executescript(SCHEMA)
    last_snapshot = 0
    last_event_id = 0
    tracked_session = ""
    last_bar_capture = 0.0
    try:
        while True:
            if not nasdaq:
                nasdaq = load_nasdaq()
            try:
                now_ms = time.time_ns() // 1_000_000
                session = current_session(now_ms)
                if session != tracked_session:
                    tracked_session, last_snapshot, last_event_id = session, 0, 0
                last_snapshot,last_event_id = capture_once(out, nasdaq, last_snapshot, last_event_id)
                if time.monotonic() - last_bar_capture >= 15:
                    capture_available_bars(out, current_session(now_ms), now_ms)
                    last_bar_capture = time.monotonic()
            except sqlite3.Error:
                pass
            time.sleep(POLL_SECONDS)
    finally:
        out.close()


if __name__ == "__main__":
    main()

"""Post-extended-hours Moomoo candle recovery and exit-grid replay.

Runs at low frequency, only after 20:05 New York time. It reads the day's
actual Gainer/ORB fills, backfills only traded symbols with gaps (subject to
Moomoo's historical-candle quota), replays price lines, and stores each run.
It has no trading context and cannot submit or amend orders.
"""
from __future__ import annotations

import json
import logging
import sqlite3
import time
from datetime import datetime, time as wall_time, timedelta
from pathlib import Path
from zoneinfo import ZoneInfo

from .runtime import enter_project

ROOT = enter_project()
DATA = ROOT / "data"
NY = ZoneInfo("America/New_York")
LOG = logging.getLogger("papercompmoomoo.research_replay")


def _session(ts_ms: int) -> str:
    return datetime.fromtimestamp(ts_ms / 1000, NY).date().isoformat()


def _session_symbols(session: str) -> dict[str, tuple[int, int]]:
    """Return only symbols with actual fills today and their first/last trade times."""
    values: dict[str, list[int]] = {}
    for name in ("journal.sqlite3", "orb.sqlite3"):
        path = DATA / name
        if not path.exists():
            continue
        with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
            for symbol, opened, closed in db.execute("SELECT symbol,opened_ms,closed_ms FROM trades"):
                if _session(int(opened)) != session:
                    continue
                ticker = str(symbol).removeprefix("US.")
                span = values.setdefault(ticker, [int(opened), int(closed)])
                span[0] = min(span[0], int(opened))
                span[1] = max(span[1], int(closed))
    return {k: (v[0], v[1]) for k, v in values.items()}


def _gaps(session: str, spans: dict[str, tuple[int, int]]) -> set[str]:
    """Find traded symbols whose stored tape does not reach their recorded exits."""
    path = DATA / "common1m.sqlite3"
    if not path.exists():
        return set(spans)
    missing = set()
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
        for symbol, (opened, closed) in spans.items():
            rows = db.execute(
                "SELECT ts FROM bars WHERE symbol IN (?,?) AND source='moomoo' AND ts>=? AND ts<=? ORDER BY ts",
                (symbol, "US." + symbol, opened - 60_000, closed + 60_000),
            ).fetchall()
            if "premarket_bars" in {r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table'")}:
                rows += db.execute(
                    "SELECT ts FROM premarket_bars WHERE symbol IN (?,?) AND source='moomoo' AND ts>=? AND ts<=? ORDER BY ts",
                    (symbol, "US." + symbol, opened - 60_000, closed + 60_000),
                ).fetchall()
            # A position with no tape, a tape that starts after entry, or a gap
            # longer than three minutes needs a single targeted historical pull.
            times = sorted({int(r[0]) for r in rows})
            if not times or times[0] > opened + 60_000 or times[-1] < closed - 120_000:
                missing.add(symbol)
                continue
            if any(b - a > 180_000 for a, b in zip(times, times[1:])):
                missing.add(symbol)
    return missing


def _quota(context) -> tuple[int, set[str]]:
    from futu import RET_OK
    ret, data = context.get_history_kl_quota(get_detail=True)
    if ret != RET_OK:
        raise RuntimeError(f"Moomoo historical candle quota unavailable: {data}")
    if isinstance(data, dict):
        remain = int(data.get("remain_quota", 0))
        details = data.get("detail_list", [])
    elif hasattr(data, "columns") and "remain_quota" in data.columns:
        remain = int(data.iloc[0]["remain_quota"])
        details = data.iloc[0].get("detail_list", [])
    else:
        seq = list(data) if isinstance(data, (tuple, list)) else []
        if len(seq) < 2:
            raise RuntimeError("Moomoo historical quota response had no remaining quota")
        remain = int(seq[1])
        details = seq[2:]
    if isinstance(details, dict):
        details = [details]
    elif isinstance(details, (tuple, list)) and len(details) == 1 and isinstance(details[0], (tuple, list)):
        details = details[0]
    recently_pulled = set()
    cutoff = datetime.now(NY) - timedelta(days=7)
    for row in details:
        if not isinstance(row, dict):
            continue
        code = str(row.get("code", ""))
        try:
            pulled = datetime.strptime(str(row.get("request_time", "")), "%Y-%m-%d %H:%M:%S").replace(tzinfo=NY)
        except ValueError:
            continue
        if pulled >= cutoff:
            recently_pulled.add(code)
    return remain, recently_pulled


def backfill(session: str, symbols: set[str]) -> dict:
    """Backfill only missing, actually-traded symbols; never exceed available KL quota."""
    if not symbols:
        return {"requested": 0, "stored_bars": 0, "skipped_quota": [], "errors": {}}
    from futu import AuType, KLType, OpenQuoteContext, RET_OK, Session
    from .microsense import write_minutes
    from .telemetry import RecordedContext, emit

    quote = RecordedContext(OpenQuoteContext(host="127.0.0.1", port=11111), "research_replay_quote")
    attempted = stored = 0
    skipped: list[str] = []
    errors: dict[str, str] = {}
    try:
        remain, recent = _quota(quote)
        for symbol in sorted(symbols):
            code = "US." + symbol
            if code not in recent and remain <= 0:
                skipped.append(symbol)
                continue
            # Keep below Moomoo's 60 request / 30 second historical-KL limit,
            # leaving headroom for other clients using the same OpenD.
            if attempted:
                time.sleep(1.25)
            ret, frame, _page = quote.request_history_kline(
                code, start=session, end=session, ktype=KLType.K_1M,
                autype=AuType.NONE, max_count=1000,
                extended_time=True, session=Session.ETH,
            )
            attempted += 1
            if ret != RET_OK:
                errors[symbol] = str(frame)[:240]
                continue
            import pandas as pd
            frame = frame.copy()
            frame.index = pd.to_datetime(frame["time_key"]).dt.tz_localize(NY, ambiguous="NaT", nonexistent="NaT")
            frame = frame.rename(columns={"open": "Open", "high": "High", "low": "Low", "close": "Close", "volume": "Volume"})
            stored += write_minutes(DATA / "common1m.sqlite3", code, frame)
            if code not in recent:
                remain -= 1
            emit("exit_replay_candle_backfill", {
                "session": session, "symbol": symbol, "bars_returned": len(frame),
                "stored_total": stored, "source": "Moomoo request_history_kline",
                "extended_time": True, "session_scope": "ETH",
            }, actor="research_replay")
    finally:
        quote.close()
    return {"requested": attempted, "stored_bars": stored, "skipped_quota": skipped, "errors": errors}


def _archive(report: dict) -> None:
    path = DATA / "research_replay.sqlite3"
    with sqlite3.connect(path, timeout=30) as db:
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("CREATE TABLE IF NOT EXISTS replay_runs(id INTEGER PRIMARY KEY,session TEXT NOT NULL,created_ms INTEGER NOT NULL,status TEXT NOT NULL,payload_json TEXT NOT NULL)")
        db.execute("CREATE TRIGGER IF NOT EXISTS replay_runs_no_update BEFORE UPDATE ON replay_runs BEGIN SELECT RAISE(ABORT,'immutable replay record'); END")
        db.execute("CREATE TRIGGER IF NOT EXISTS replay_runs_no_delete BEFORE DELETE ON replay_runs BEGIN SELECT RAISE(ABORT,'immutable replay record'); END")
        if report.get("as_of_mode") == "live mark-to-latest-Moomoo-candle":
            status = "live_update"
        else:
            status = "complete" if sum(x["missing_replay_count"] for x in report["bots"].values()) == 0 else "partial"
        db.execute("INSERT INTO replay_runs(session,created_ms,status,payload_json) VALUES(?,?,?,?)",
                   (report["session"], int(time.time() * 1000), status, json.dumps(report, sort_keys=True, allow_nan=False)))


def run_session(session: str, as_of_ms: int | None = None, recover_gaps: bool = False) -> dict:
    import importlib.util
    replay_path = ROOT / "tools" / "research_replay.py"
    spec = importlib.util.spec_from_file_location("papercompmoomoo_research_replay", replay_path)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load shared research replay rules")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    replay = module.run_session
    spans = _session_symbols(session)
    gaps_before = _gaps(session, spans)
    backfill_info = backfill(session, gaps_before) if recover_gaps else {
        "requested": 0, "stored_bars": 0, "skipped_quota": [], "errors": {},
        "deferred_until_extended_close": bool(gaps_before),
    }
    # Re-run after recovery so the stored result includes the recovered candles.
    result = replay(session, DATA, as_of_ms=as_of_ms)
    result["backfill"] = backfill_info
    result["data_coverage"] = {
        "traded_symbols": sorted(spans),
        "symbols_with_candle_gaps_before_backfill": sorted(gaps_before),
        "symbols_with_candle_gaps_after_backfill": sorted(_gaps(session, spans)) if recover_gaps else sorted(gaps_before),
        "backfill": backfill_info,
    }
    _archive(result)
    DATA.mkdir(parents=True, exist_ok=True)
    target = DATA / "strategy_research.json"
    temp = target.with_suffix(".json.tmp")
    temp.write_text(json.dumps(result, indent=2, sort_keys=True, allow_nan=False) + "\n")
    temp.replace(target)
    return result


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    last_attempt: dict[str, float] = {}
    last_live_slot: tuple[str, int] | None = None
    final_attempts: dict[str, int] = {}
    while True:
        now = datetime.now(NY)
        session = now.date().isoformat()
        trading_window = now.weekday() < 5 and wall_time(4, 0) <= now.time() < wall_time(20, 0)
        has_trades = bool(_session_symbols(session))
        if trading_window and has_trades:
            slot = (session, (now.hour * 60 + now.minute) // 15)
            if slot != last_live_slot:
                last_live_slot = slot
                try:
                    as_of_ms = int(now.timestamp() * 1000)
                    report = run_session(session, as_of_ms=as_of_ms, recover_gaps=False)
                    LOG.info("stored %s live 10,000-grid replay for %s as of %s", session, session, now.isoformat())
                except Exception:
                    LOG.exception("15-minute live-session grid replay failed for %s", session)
        elif now.weekday() < 5 and now.time() >= wall_time(20, 5) and has_trades and final_attempts.get(session, 0) < 3 and time.monotonic() - last_attempt.get(session, 0) >= 1800:
            last_attempt[session] = time.monotonic()
            final_attempts[session] = final_attempts.get(session, 0) + 1
            try:
                report = run_session(session, as_of_ms=None, recover_gaps=True)
                missing = sum(v["missing_replay_count"] for v in report["bots"].values())
                LOG.info("stored %s replay for %s; missing position replays=%s", "complete" if not missing else "partial", session, missing)
                if not missing:
                    last_attempt[session] = float("inf")
            except Exception:
                LOG.exception("session replay failed for %s", session)
        time.sleep(300)


if __name__ == "__main__":
    main()

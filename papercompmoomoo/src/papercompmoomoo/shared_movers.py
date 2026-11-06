"""Atomic local handoff of Moomoo's shared hottest-movers snapshot."""
from __future__ import annotations

import json
import os
import time
from pathlib import Path

DATA = Path(__file__).resolve().parents[2] / "data"
PATH = DATA / "shared_moomoo_movers.json"
MAX_AGE_MS = 90_000


def publish(rows: list[dict], observed_ms: int | None = None) -> None:
    """Publish the single Moomoo leaderboard response for both paper bots."""
    now = int(observed_ms or time.time_ns() // 1_000_000)
    payload = {"source": "moomoo_us_hottest_gainers", "observed_ms": now,
               "rows": rows}
    DATA.mkdir(parents=True, exist_ok=True)
    temporary = PATH.with_suffix(".tmp")
    temporary.write_text(json.dumps(payload, separators=(",", ":"), allow_nan=False) + "\n")
    os.replace(temporary, PATH)


def read(now_ms: int | None = None, max_age_ms: int = MAX_AGE_MS) -> list[dict]:
    """Return only a fresh shared Moomoo response; never make a fallback API call."""
    now = int(now_ms or time.time_ns() // 1_000_000)
    try:
        payload = json.loads(PATH.read_text())
        stamp = int(payload["observed_ms"])
        rows = payload["rows"]
        if payload.get("source") != "moomoo_us_hottest_gainers" or now - stamp > max_age_ms or stamp > now:
            return []
        return [row for row in rows if isinstance(row, dict)] if isinstance(rows, list) else []
    except (OSError, ValueError, TypeError, KeyError):
        return []

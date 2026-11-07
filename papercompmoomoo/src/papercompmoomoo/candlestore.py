"""Disk-backed Moomoo minute candles and volume baselines used by both bots."""

from __future__ import annotations

import logging
import sqlite3
import time
from datetime import datetime, time as clock_time, timedelta, timezone
from pathlib import Path
from typing import Iterable


LOGGER = logging.getLogger(__name__)
BAR_MS = 60_000
COMMON_DB = "data/common1m.sqlite3"
NEW_YORK = "America/New_York"

SCHEMA = """
CREATE TABLE IF NOT EXISTS bars(
    symbol TEXT NOT NULL, ts INTEGER NOT NULL, open REAL NOT NULL, high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL, volume REAL NOT NULL,
    PRIMARY KEY(symbol, ts)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS baselines(symbol TEXT PRIMARY KEY, avg_volume REAL NOT NULL, computed TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS premarket_bars(
    symbol TEXT NOT NULL, ts INTEGER NOT NULL, open REAL NOT NULL, high REAL NOT NULL, low REAL NOT NULL, close REAL NOT NULL, volume REAL NOT NULL,
    source TEXT NOT NULL DEFAULT 'moomoo', updated_ms INTEGER NOT NULL, PRIMARY KEY(symbol, ts)) WITHOUT ROWID;
"""


def to_ms(index) -> list[int]:
    """Milliseconds since the epoch (UTC) of a tz-aware (or UTC-assumed naive) DatetimeIndex."""
    import pandas as pd

    utc = index.tz_convert("UTC") if getattr(index, "tz", None) is not None else index.tz_localize("UTC")
    return [int(v) for v in ((utc - pd.Timestamp("1970-01-01", tz="UTC")) // pd.Timedelta("1ms"))]


def acceptable(ts: int, o: float, h: float, l: float, c: float, v: float, bar_ms: int = BAR_MS) -> bool:
    """Whether a bar may enter the store: on the 1-minute grid, a possible bar (positive prices, high >= open/close >= low), and either with trades or a candle with no
    trade: zero volume and flat (open = high = low = close), the previous price carried. A no-trade candle is true information, so it is kept; a zero-volume bar that
    moved is not a candle of anything (a malformed quote row can do that) and a row off the grid is a quote, not a candle."""
    flat_no_trade = v == 0 and o == h == l == c
    return (ts % bar_ms == 0 and (v > 0 or flat_no_trade) and min(o, h, l, c) > 0 and h >= max(o, c) and l <= min(o, c) and h >= l
            and all(x == x and x not in (float("inf"), float("-inf")) for x in (o, h, l, c, v)))


class CandleStore:
    def __init__(self, path: Path | str = COMMON_DB, bar_ms: int = BAR_MS) -> None:
        self.bar_ms = bar_ms
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(self.path, timeout=60)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=NORMAL")
        self.db.execute("PRAGMA mmap_size=4294967296")
        self.db.execute("PRAGMA cache_size=-65536")
        self.db.execute("PRAGMA temp_store=MEMORY")
        self.db.executescript(SCHEMA)
        if "updated_ms" not in {r[1] for r in self.db.execute("PRAGMA table_info(bars)")}:
            with self.db:  # when the row last changed: TradingSystem imports every row changed since its last import, so revisions reach it as well as new bars
                self.db.execute("ALTER TABLE bars ADD COLUMN updated_ms INTEGER NOT NULL DEFAULT 0")
        if "vol_legacy" not in {r[1] for r in self.db.execute("PRAGMA table_info(bars)")}:
            with self.db:  # retain legacy volume columns without using them for live decisions
                self.db.execute("ALTER TABLE bars ADD COLUMN vol_legacy REAL")
                self.db.execute("ALTER TABLE bars ADD COLUMN vol_m REAL")
        if "source" not in {r[1] for r in self.db.execute("PRAGMA table_info(bars)")}:
            with self.db:  # preserve provenance on existing disk rows
                self.db.execute("ALTER TABLE bars ADD COLUMN source TEXT NOT NULL DEFAULT 'external'")
        from .decision_audit import preserve_versions
        preserve_versions(self.db)
        with self.db:  # preserve existing Moomoo volume provenance during schema upgrades
            self.db.execute("UPDATE bars SET vol_legacy = volume WHERE vol_legacy IS NULL AND vol_m IS NULL AND source = 'external'")
            self.db.execute("UPDATE bars SET vol_m = volume WHERE vol_legacy IS NULL AND vol_m IS NULL AND source = 'moomoo'")

    def close(self) -> None:
        self.db.close()

    # ---- bars -------------------------------------------------------------------------------------------------------------------------------------
    def upsert_premarket_frame(self, symbol: str, frame, now_ms: int | None = None) -> int:
        """Persist today's 04:00–09:30 ET native Moomoo candles."""
        if frame is None or len(frame) == 0:
            return 0
        stamp = now_ms if now_ms is not None else int(time.time() * 1000)
        from zoneinfo import ZoneInfo
        ny = ZoneInfo(NEW_YORK)
        session_date = datetime.fromtimestamp(stamp / 1000, timezone.utc).astimezone(ny).date()
        index = frame.index
        index = index.tz_localize(ny) if index.tz is None else index.tz_convert(ny)
        keep = [(at.date() == session_date and clock_time(4, 0) < at.time().replace(tzinfo=None) <= clock_time(9, 30)) for at in index]
        frame = frame.loc[keep]
        if frame.empty:
            return 0
        rows = [(symbol, ts, float(o), float(h), float(l), float(c), float(v), "moomoo", stamp)
                for ts, o, h, l, c, v in zip(to_ms(frame.index), frame["Open"], frame["High"], frame["Low"], frame["Close"], frame["Volume"])
                if acceptable(ts, o, h, l, c, v, self.bar_ms)]
        with self.db:
            self.db.executemany("INSERT OR REPLACE INTO premarket_bars(symbol,ts,open,high,low,close,volume,source,updated_ms) VALUES(?,?,?,?,?,?,?,?,?)", rows)
        return len(rows)

    def moomoo_frame(self, symbol: str, since: datetime | None = None):
        """Read only native Moomoo bars; legacy legacy provider rows cannot enter live decisions."""
        import pandas as pd

        since_ms = int(since.timestamp() * 1000) if since else 0
        rows = self.db.execute(
            "SELECT ts, open, high, low, close, coalesce(vol_m,volume) FROM bars "
            "WHERE symbol=? AND ts>=? AND (source='moomoo' OR vol_m IS NOT NULL) ORDER BY ts",
            (symbol, since_ms),
        ).fetchall()
        index = pd.to_datetime([r[0] for r in rows], unit="ms", utc=True).tz_convert(NEW_YORK)
        index.name = "Datetime"
        return pd.DataFrame({"Open": [r[1] for r in rows], "High": [r[2] for r in rows], "Low": [r[3] for r in rows],
                             "Close": [r[4] for r in rows], "Volume": [r[5] for r in rows]}, index=index)

    def clean(self) -> dict[str, int]:
        """Remove what should never have been stored, and say how much: rows off the 1-minute grid, bars without volume (filler / stale quotes), bars that cannot exist,
        and bars identical in every number to the symbol's previous bar (a repeated print). Safe to run any time; the newest bar is never touched by the duplicate rule
        (it may legitimately still be forming)."""
        out = {}
        with self.db:
            out["off_grid"] = self.db.execute("DELETE FROM bars WHERE ts % ? != 0", (self.bar_ms,)).rowcount
            out["no_volume"] = self.db.execute("DELETE FROM bars WHERE volume < 0 OR (volume = 0 AND NOT (open = high AND high = low AND low = close))").rowcount
            out["impossible"] = self.db.execute("DELETE FROM bars WHERE high < low OR high < open OR high < close OR low > open OR low > close OR low <= 0").rowcount
            if self.bar_ms == 60_000:
                # Equal OHLCV in adjacent minutes can be real; never erase that evidence.
                out["repeated"] = 0
                return out
            dup = self.db.execute(
                """DELETE FROM bars WHERE (symbol, ts) IN (
                     SELECT symbol, ts FROM (SELECT symbol, ts, open, high, low, close, volume,
                       lag(open) OVER w po, lag(high) OVER w ph, lag(low) OVER w pl, lag(close) OVER w pc, lag(volume) OVER w pv,
                       lead(ts) OVER w next_ts FROM bars WINDOW w AS (PARTITION BY symbol ORDER BY ts))
                     WHERE open=po AND high=ph AND low=pl AND close=pc AND volume=pv AND volume > 0 AND next_ts IS NOT NULL)""")
            out["repeated"] = dup.rowcount
        return out

    def clean_if_due(self, hours: float = 20.0, now: datetime | None = None) -> dict[str, int] | None:
        """``clean`` at most once per ``hours`` (the time of the last run is kept in the store); None when it was not due."""
        now = now or datetime.now(timezone.utc)
        last = self.meta("last_clean")
        if last and (now - datetime.fromisoformat(last)).total_seconds() < hours * 3600:
            return None
        counts = self.clean()
        self.set_meta("last_clean", now.isoformat())
        return counts

    def prune(self, keep_days: int = 60, now: datetime | None = None) -> int:
        """Prune old legacy rows that do not carry native Moomoo volume; keep Moomoo bars and decision evidence."""
        cutoff = int(((now or datetime.now(timezone.utc)) - timedelta(days=keep_days)).timestamp() * 1000)
        with self.db:
            return self.db.execute("DELETE FROM bars WHERE ts<? AND vol_m IS NULL", (cutoff,)).rowcount

    def fresh_symbols(self, newer_than: str) -> set[str]:
        return {r[0] for r in self.db.execute("SELECT symbol FROM baselines WHERE computed>=?", (newer_than,))}

    # ---- baselines and progress, also on disk -----------------------------------------------------------------------------------------------------
    def set_baseline(self, symbol: str, avg_volume: float, computed: str) -> None:
        """Record a symbol's average daily volume; 0.0 means "checked, not enough history yet" so it is not downloaded again until the baselines are refreshed."""
        with self.db:
            self.db.execute("INSERT OR REPLACE INTO baselines VALUES(?,?,?)", (symbol, avg_volume, computed))

    def shortlist(self, universe: Iterable[str], size: int, newer_than: str) -> list[str]:
        """The ``size`` symbols with the highest average daily volume among ``universe``, using baselines computed on or after ``newer_than`` (ISO date)."""
        universe = None if universe is None else set(universe)
        rows = self.db.execute("SELECT symbol FROM baselines WHERE computed>=? AND avg_volume>0 ORDER BY avg_volume DESC", (newer_than,))
        out: list[str] = []
        for (symbol,) in rows:
            if universe is None or symbol in universe:
                out.append(symbol)
                if len(out) == size:
                    break
        return out

    def baseline(self, symbol: str) -> float:
        """The stored average daily volume of ``symbol`` (0.0 when unknown)."""
        row = self.db.execute("SELECT avg_volume FROM baselines WHERE symbol=?", (symbol,)).fetchone()
        return row[0] if row else 0.0

    def baseline_count(self, newer_than: str) -> int:
        return self.db.execute("SELECT count(*) FROM baselines WHERE computed>=? AND avg_volume>0", (newer_than,)).fetchone()[0]

    def meta(self, key: str, default: str | None = None) -> str | None:
        row = self.db.execute("SELECT value FROM meta WHERE key=?", (key,)).fetchone()
        return row[0] if row else default

    def set_meta(self, key: str, value: str) -> None:
        with self.db:
            self.db.execute("INSERT OR REPLACE INTO meta VALUES(?,?)", (key, value))

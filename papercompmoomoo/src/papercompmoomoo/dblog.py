"""Everything the bots and the converter log goes into a database on disk as well as to the terminal, so it can be analysed later (owner 2026-10-22: nothing lives only in
memory or a temp folder). ``data/logs.sqlite3``, table ``logs``."""

from __future__ import annotations

import logging
import sqlite3
import threading
import time
from pathlib import Path

LOG_DB = Path("data/logs.sqlite3")
QUIET = ("peewee", "urllib3", "futu")


class DbLogHandler(logging.Handler):
    def __init__(self, source: str, path: Path | str = LOG_DB, level: int = logging.INFO) -> None:
        super().__init__(level)
        self.source, self.write_lock = source, threading.Lock()  # not ``lock``: logging.Handler owns that name and takes it around emit
        Path(path).parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(path, timeout=30, check_same_thread=False)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("CREATE TABLE IF NOT EXISTS logs(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, source TEXT NOT NULL, level TEXT NOT NULL, logger TEXT NOT NULL, message TEXT NOT NULL)")
        self.db.execute("CREATE INDEX IF NOT EXISTS logs_time ON logs(ts_ms)")

    def emit(self, record: logging.LogRecord) -> None:
        if record.name.split(".")[0] in QUIET:
            return
        try:
            with self.write_lock, self.db:
                self.db.execute("INSERT INTO logs(ts_ms, source, level, logger, message) VALUES(?,?,?,?,?)",
                                (int(time.time() * 1000), self.source, record.levelname, record.name, self.format(record)[:4000]))
        except Exception:  # a log line that cannot be stored must never stop the process
            self.handleError(record)


def install(source: str, path: Path | str = LOG_DB) -> DbLogHandler:
    """Add the database handler to the root logger (the terminal handler stays)."""
    handler = DbLogHandler(source, path)
    handler.setFormatter(logging.Formatter("%(message)s"))
    logging.getLogger().addHandler(handler)
    return handler

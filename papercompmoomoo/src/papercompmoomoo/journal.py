"""What the bot saw and did, on disk (SQLite ``data/journal.sqlite3``), so that it can be analysed and learned from.

Three tables, all appended to: ``signals`` (every symbol evaluated at every scan, with the numbers the decision used and what was decided), ``orders`` (every order
the bot sent) and ``trades`` (every closed round trip with how it ended). TradingSystem's ``thomas comp`` reads them incrementally by ``id`` (ids only grow) and
replays the signals against the stored candles. The journal never holds anything the bot needs to run; losing it loses history, not state.
"""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path

SCHEMA = """
CREATE TABLE IF NOT EXISTS signals(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, symbol TEXT NOT NULL, bar_ms INTEGER NOT NULL, price REAL NOT NULL, rvol REAL NOT NULL,
    move_pct REAL NOT NULL, last_bar_volume REAL NOT NULL, typical_bar_volume REAL NOT NULL, median_range REAL NOT NULL, avg_daily_volume REAL NOT NULL,
    nasdaq INTEGER NOT NULL, decision TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS orders(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, symbol TEXT NOT NULL, side TEXT NOT NULL, qty INTEGER NOT NULL, signal_id INTEGER,
    broker_order_id TEXT, price_seen REAL NOT NULL, rvol REAL, move_pct REAL, limited_by TEXT);
CREATE TABLE IF NOT EXISTS trades(
    id INTEGER PRIMARY KEY AUTOINCREMENT, symbol TEXT NOT NULL, opened_ms INTEGER NOT NULL, closed_ms INTEGER NOT NULL, qty INTEGER NOT NULL, entry REAL NOT NULL,
    exit_price REAL NOT NULL, pnl_pct REAL NOT NULL, reason TEXT NOT NULL, signal_id INTEGER, rvol REAL, move_pct REAL);
-- The dashboard-facing trades row is a projection. Keep the broker-confirmed
-- source immutable and keyed by the exit order so retries cannot double-book it.
CREATE TABLE IF NOT EXISTS broker_exit_records(
    broker_order_id TEXT PRIMARY KEY, recorded_ms INTEGER NOT NULL, symbol TEXT NOT NULL,
    opened_ms INTEGER NOT NULL, closed_ms INTEGER NOT NULL, qty INTEGER NOT NULL,
    entry_price REAL NOT NULL, exit_price REAL NOT NULL, order_status TEXT NOT NULL,
    payload_json TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS broker_exit_records_no_update BEFORE UPDATE ON broker_exit_records
    BEGIN SELECT RAISE(ABORT,'permanent broker exit evidence'); END;
CREATE TRIGGER IF NOT EXISTS broker_exit_records_no_delete BEFORE DELETE ON broker_exit_records
    BEGIN SELECT RAISE(ABORT,'permanent broker exit evidence'); END;
-- The scout's analysis of every closed bar: what it was shown, what it answered, what the bot did with it, how long it took (analysis of the session, kept for good).
CREATE TABLE IF NOT EXISTS scout_runs(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, bar_ms INTEGER NOT NULL, symbols INTEGER NOT NULL, pool_json TEXT NOT NULL, reply TEXT NOT NULL,
    picked TEXT, confidence REAL, terms_json TEXT, outcome TEXT NOT NULL, note TEXT NOT NULL, seconds REAL NOT NULL);
-- What the scout's entries taught: one row per closed scout trade, with the numbers it was entered on, the conditions it was given and what happened. The next analyses are shown these.
CREATE TABLE IF NOT EXISTS scout_learnings(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, symbol TEXT NOT NULL, signal_id INTEGER, confidence REAL, terms_json TEXT NOT NULL, rvol REAL, move_pct REAL,
    spread_pct REAL, pnl_pct REAL NOT NULL, minutes_held REAL NOT NULL, exit_reason TEXT NOT NULL, lesson TEXT NOT NULL);
-- The daily plan, step by step: candidates found after the close, the pre-open refresh, pre-market orders, the open's market orders and every retry.
CREATE TABLE IF NOT EXISTS plan_events(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, session TEXT NOT NULL, symbol TEXT, event TEXT NOT NULL, detail TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS orders_symbol ON orders(symbol, ts_ms);
CREATE INDEX IF NOT EXISTS signals_bar ON signals(bar_ms);
"""


class Journal:
    def __init__(self, path: Path | str = "data/journal.sqlite3") -> None:
        self.path = Path(path)
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.db = sqlite3.connect(self.path, timeout=60)
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=NORMAL")
        self.db.execute("PRAGMA mmap_size=4294967296")
        self.db.execute("PRAGMA cache_size=-65536")
        self.db.execute("PRAGMA temp_store=MEMORY")
        self.db.executescript(SCHEMA)
        columns = {r[1] for r in self.db.execute("PRAGMA table_info(signals)")}
        with self.db:
            if "source" not in columns:  # legacy rows retain their source; new bot signals are Moomoo-only
                self.db.execute("ALTER TABLE signals ADD COLUMN source TEXT NOT NULL DEFAULT 'moomoo'")
            have = {r[1] for r in self.db.execute("PRAGMA table_info(scout_learnings)")}
            for name, kind in (("source", "TEXT NOT NULL DEFAULT 'scout'"), ("entry_price", "REAL"), ("exit_price", "REAL"), ("qty", "INTEGER"), ("mfe_pct", "REAL"), ("mae_pct", "REAL"),
                               ("reason_text", "TEXT"), ("pool_json", "TEXT"), ("memory_json", "TEXT"), ("hour_utc", "INTEGER"), ("scout_run_id", "INTEGER")):
                if name not in have:  # everything about a trade is kept: where it was entered and left, how far it went for and against, what was said, what the scout had been shown
                    self.db.execute(f"ALTER TABLE scout_learnings ADD COLUMN {name} {kind}")
            have = {r[1] for r in self.db.execute("PRAGMA table_info(scout_learnings)")}
            for name, kind in (("source", "TEXT NOT NULL DEFAULT 'scout'"), ("entry_price", "REAL"), ("exit_price", "REAL"), ("qty", "INTEGER"), ("mfe_pct", "REAL"), ("mae_pct", "REAL"),
                               ("reason_text", "TEXT"), ("pool_json", "TEXT"), ("memory_json", "TEXT"), ("hour_utc", "INTEGER"), ("scout_run_id", "INTEGER")):
                if name not in have:  # everything about a trade is kept: where it was entered and left, how far it went for and against, what was said, what the scout had been shown
                    self.db.execute(f"ALTER TABLE scout_learnings ADD COLUMN {name} {kind}")
            if "note" not in columns:
                self.db.execute("ALTER TABLE signals ADD COLUMN note TEXT NOT NULL DEFAULT ''")

        from .decision_audit import preserve_versions
        preserve_versions(self.db)
        self.db.commit()

    def close(self) -> None:
        self.db.close()

    def signal(self, ts_ms: int, symbol: str, bar_ms: int, price: float, rvol: float, move_pct: float, profile: dict, avg_daily: float, nasdaq: bool,
               decision: str, source: str = "moomoo", note: str = "") -> int:
        with self.db:
            cursor = self.db.execute(
                "INSERT INTO signals(ts_ms, symbol, bar_ms, price, rvol, move_pct, last_bar_volume, typical_bar_volume, median_range, avg_daily_volume, nasdaq, decision, source, note)"
                " VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (ts_ms, symbol, bar_ms, price, rvol, move_pct, profile.get("last_bar_volume", 0.0), profile.get("typical_bar_volume", 0.0),
                 profile.get("median_range", 0.0), avg_daily, int(nasdaq), decision, source, note))
        return int(cursor.lastrowid)

    def decide(self, signal_id: int, decision: str) -> None:
        with self.db:
            self.db.execute("UPDATE signals SET decision=? WHERE id=?", (decision, signal_id))

    def scout_run(self, ts_ms: int, bar_ms: int, pool_json: str, symbols: int, reply: str, picked: str | None, confidence: float | None, terms_json: str | None,
                  outcome: str, note: str, seconds: float) -> int:
        with self.db:
            cursor = self.db.execute(
                "INSERT INTO scout_runs(ts_ms, bar_ms, symbols, pool_json, reply, picked, confidence, terms_json, outcome, note, seconds) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                (ts_ms, bar_ms, symbols, pool_json, reply, picked, confidence, terms_json, outcome, note, seconds))
        return int(cursor.lastrowid)

    def scout_learning(self, ts_ms: int, symbol: str, meta: dict, terms: dict, pnl_pct: float, minutes_held: float, exit_reason: str, source: str = "scout", entry_price: float | None = None,
                       exit_price: float | None = None, qty: int | None = None, mfe_pct: float | None = None, mae_pct: float | None = None) -> str:
        """Records everything a closed trade taught; returns the lesson sentence (built from the numbers, not from anyone's opinion)."""
        rvol, move, conf = meta.get("rvol"), meta.get("move_pct"), meta.get("confidence")
        outcome = "won" if pnl_pct > 0 else "lost"
        reach = f", best {mfe_pct:+.2f}% / worst {mae_pct:+.2f}% while held" if mfe_pct is not None and mae_pct is not None else ""
        lesson = (f"[{source}] {symbol} {outcome} {pnl_pct:+.2f}% in {minutes_held:.0f} min, exit by {exit_reason}{reach}; entered at rvol {rvol if rvol is not None else '?'}, move {move if move is not None else '?'}%, "
                  f"scout confidence {conf if conf is not None else '?'}, stop {terms.get('stop_pct', '-')}%, trail {terms.get('trail_pct', '-')}%, take profit {terms.get('take_profit_pct', '-')}%, hold {terms.get('hold_minutes', '-')} min")
        with self.db:
            self.db.execute(
                "INSERT INTO scout_learnings(ts_ms, symbol, signal_id, confidence, terms_json, rvol, move_pct, spread_pct, pnl_pct, minutes_held, exit_reason, lesson, source, entry_price, exit_price, qty,"
                " mfe_pct, mae_pct, reason_text, pool_json, memory_json, hour_utc, scout_run_id) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (ts_ms, symbol, meta.get("signal_id"), conf, json.dumps(terms), rvol, move, meta.get("spread_pct"), pnl_pct, minutes_held, exit_reason, lesson, source, entry_price, exit_price, qty, mfe_pct,
                 mae_pct, meta.get("reason"), meta.get("pool_json"), meta.get("memory_json"), int(ts_ms // 3_600_000 % 24), meta.get("scout_run_id")))
        return lesson

    def scout_memory(self, recent: int = 10) -> dict:
        """What the closed trades showed, separately for the scout's entries and the rule's entries, and the latest lessons of both. Shown to every new analysis."""
        def totals(source: str) -> tuple:
            return self.db.execute("SELECT count(*), coalesce(sum(pnl_pct>0),0), coalesce(avg(pnl_pct),0), coalesce(avg(mfe_pct),0), coalesce(avg(mae_pct),0) FROM scout_learnings WHERE source=?", (source,)).fetchone()
        scout_t, rule_t = totals("scout"), totals("rule")
        lessons = [r[0] for r in self.db.execute("SELECT lesson FROM scout_learnings ORDER BY id DESC LIMIT ?", (recent,))]
        return {"scout_trades": scout_t[0], "scout_wins": scout_t[1], "scout_mean_pnl_pct": round(scout_t[2], 3), "scout_mean_best_pct": round(scout_t[3], 3), "scout_mean_worst_pct": round(scout_t[4], 3),
                "rule_trades": rule_t[0], "rule_wins": rule_t[1], "rule_mean_pnl_pct": round(rule_t[2], 3), "latest_lessons": lessons}

    def plan_event(self, ts_ms: int, session: str, symbol: str | None, event: str, detail: str = "") -> None:
        with self.db:
            self.db.execute("INSERT INTO plan_events(ts_ms, session, symbol, event, detail) VALUES(?,?,?,?,?)", (ts_ms, session, symbol, event, detail))

    def plan_event_if_changed(self, ts_ms: int, session: str, symbol: str, event: str, detail: str) -> bool:
        """Append a candidate decision only when its recorded evidence changes.

        Every scan's complete sensing inputs remain in the immutable decision
        audit. This projection avoids repeating the same dashboard rejection
        for an unchanged bar, quote and reason every scan interval.
        """
        with self.db:
            previous=self.db.execute("SELECT detail FROM plan_events WHERE session=? AND symbol=? AND event=? ORDER BY id DESC LIMIT 1",
                (session,symbol,event)).fetchone()
            if previous:
                try:same=json.loads(previous[0])==json.loads(detail)
                except (TypeError,ValueError):same=previous[0]==detail
                if same:return False
            self.db.execute("INSERT INTO plan_events(ts_ms,session,symbol,event,detail) VALUES(?,?,?,?,?)",
                (ts_ms,session,symbol,event,detail))
        return True

    def annotate(self, signal_id: int, text: str) -> None:
        with self.db:
            self.db.execute("UPDATE signals SET note=CASE WHEN note='' OR note IS NULL THEN ? ELSE note || ' | ' || ? END WHERE id=?", (text, text, signal_id))

    def order(self, ts_ms: int, symbol: str, side: str, qty: int, signal_id: int | None, broker_order_id: str, price_seen: float, rvol: float | None,
              move_pct: float | None, limited_by: str | None) -> int:
        with self.db:
            cursor = self.db.execute(
                "INSERT INTO orders(ts_ms, symbol, side, qty, signal_id, broker_order_id, price_seen, rvol, move_pct, limited_by) VALUES(?,?,?,?,?,?,?,?,?,?)",
                (ts_ms, symbol, side, qty, signal_id, broker_order_id, price_seen, rvol, move_pct, limited_by))
        return int(cursor.lastrowid)

    def entry_context(self, symbol: str) -> dict:
        """The signal behind the newest BUY order of ``symbol`` (empty when the journal does not know it)."""
        row = self.db.execute("SELECT signal_id, rvol, move_pct FROM orders WHERE symbol=? AND side='BUY' ORDER BY id DESC LIMIT 1", (symbol,)).fetchone()
        return {"signal_id": row[0], "rvol": row[1], "move_pct": row[2]} if row else {}

    def trade(self, symbol: str, opened_ms: int, closed_ms: int, qty: int, entry: float, exit_price: float, pnl_pct: float, reason: str) -> int:
        context = self.entry_context(symbol)
        with self.db:
            cursor = self.db.execute(
                "INSERT INTO trades(symbol, opened_ms, closed_ms, qty, entry, exit_price, pnl_pct, reason, signal_id, rvol, move_pct) VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                (symbol, opened_ms, closed_ms, qty, entry, exit_price, pnl_pct, reason, context.get("signal_id"), context.get("rvol"), context.get("move_pct")))
        return int(cursor.lastrowid)

    def broker_exit(self, *, order_id: str, recorded_ms: int, symbol: str, opened_ms: int,
                    closed_ms: int, qty: int, entry: float, exit_price: float,
                    order_status: str, evidence: dict, reason: str) -> bool:
        """Append confirmed broker exit evidence and project it into dashboard trades.

        Returns False for an already recorded order. The source row and dashboard
        projection share one transaction, so a restart cannot leave them split.
        """
        if not order_id or not symbol or qty <= 0 or entry <= 0 or exit_price <= 0:
            raise ValueError("invalid broker-confirmed exit")
        encoded = json.dumps(evidence, sort_keys=True, separators=(",", ":"), allow_nan=False)
        context = self.db.execute(
            "SELECT signal_id,rvol,move_pct FROM orders WHERE symbol=? AND side='BUY' AND ts_ms<=? "
            "ORDER BY abs(ts_ms-?),id DESC LIMIT 1", (symbol, closed_ms, opened_ms)).fetchone()
        with self.db:
            existing = self.db.execute(
                "SELECT symbol,opened_ms,closed_ms,qty,entry_price,exit_price,order_status "
                "FROM broker_exit_records WHERE broker_order_id=?", (order_id,)).fetchone()
            if existing:
                if tuple(existing) != (symbol, opened_ms, closed_ms, qty, entry, exit_price, order_status):
                    raise ValueError("conflicting broker exit evidence for an existing order")
                return False
            self.db.execute(
                "INSERT INTO broker_exit_records VALUES(?,?,?,?,?,?,?,?,?,?)",
                (order_id, recorded_ms, symbol, opened_ms, closed_ms, qty, entry,
                 exit_price, order_status, encoded))
            self.db.execute(
                "INSERT INTO trades(symbol,opened_ms,closed_ms,qty,entry,exit_price,pnl_pct,reason,signal_id,rvol,move_pct) "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?)",
                (symbol, opened_ms, closed_ms, qty, entry, exit_price,
                 (exit_price / entry - 1.0) * 100.0, reason,
                 context[0] if context else None, context[1] if context else None,
                 context[2] if context else None))
        return True

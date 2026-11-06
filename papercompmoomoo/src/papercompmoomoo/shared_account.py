"""Cross-process Competition-account symbol ownership and dynamic buying-power reservations."""
from __future__ import annotations

import json
import sqlite3
import time
import uuid
from pathlib import Path

from .account_ids import account_id

DB_PATH = Path(__file__).resolve().parents[2] / "data" / "account_ledger.sqlite3"
ACCOUNT_ID = account_id("gainer")
RESERVATION_STALE_MS = 300_000
POWER_BUFFER = 0.02

SCHEMA = """
CREATE TABLE IF NOT EXISTS symbol_owners(
  account_id INTEGER NOT NULL, symbol TEXT NOT NULL, owner TEXT NOT NULL,
  order_id TEXT, state TEXT NOT NULL, updated_ms INTEGER NOT NULL,
  PRIMARY KEY(account_id,symbol));
CREATE TABLE IF NOT EXISTS capital_reservations(
  token TEXT PRIMARY KEY, account_id INTEGER NOT NULL, bot TEXT NOT NULL,
  symbol TEXT NOT NULL, amount REAL NOT NULL, order_id TEXT,
  state TEXT NOT NULL, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS capital_reservations_active
  ON capital_reservations(account_id,state,updated_ms);
CREATE TABLE IF NOT EXISTS shared_capital_events(
  id INTEGER PRIMARY KEY AUTOINCREMENT, event_ms INTEGER NOT NULL,
  account_id INTEGER NOT NULL, bot TEXT NOT NULL, symbol TEXT,
  event TEXT NOT NULL, payload_json TEXT NOT NULL);
CREATE TRIGGER IF NOT EXISTS shared_capital_events_no_update BEFORE UPDATE ON shared_capital_events
  BEGIN SELECT RAISE(ABORT,'permanent shared capital evidence'); END;
CREATE TRIGGER IF NOT EXISTS shared_capital_events_no_delete BEFORE DELETE ON shared_capital_events
  BEGIN SELECT RAISE(ABORT,'permanent shared capital evidence'); END;
"""


def connect(path: Path = DB_PATH) -> sqlite3.Connection:
    path.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path, timeout=10, isolation_level=None)
    db.execute("PRAGMA journal_mode=WAL")
    db.execute("PRAGMA busy_timeout=10000")
    db.executescript(SCHEMA)
    return db


def _event(db, now, bot, symbol, event, payload):
    db.execute("INSERT INTO shared_capital_events(event_ms,account_id,bot,symbol,event,payload_json) VALUES(?,?,?,?,?,?)",
               (now, ACCOUNT_ID, bot, symbol, event, json.dumps(payload, sort_keys=True, separators=(",", ":"))))


def adopt(bot: str, symbols: set[str] | list[str], path: Path = DB_PATH) -> None:
    """Restore ownership from the bot's own durable position/order ledger."""
    if bot not in {"gainer", "orb"}:
        raise ValueError("unknown paper strategy")
    now = time.time_ns() // 1_000_000
    db = connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        for symbol in symbols:
            code = str(symbol).removeprefix("US.")
            old = db.execute("SELECT owner FROM symbol_owners WHERE account_id=? AND symbol=?", (ACCOUNT_ID, code)).fetchone()
            if old and old[0] not in (bot, "external"):
                continue
            db.execute("INSERT INTO symbol_owners VALUES(?,?,?,?,?,?) ON CONFLICT(account_id,symbol) DO UPDATE SET owner=excluded.owner,state='owned',updated_ms=excluded.updated_ms",
                       (ACCOUNT_ID, code, bot, None, "owned", now))
        db.commit()
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()


def sync(held_symbols: set[str] | list[str], working_rows: list[dict], bot_ledgers: dict[str, set[str]] | None = None,
         path: Path = DB_PATH, now_ms: int | None = None, terminal_rows: list[dict] | None = None) -> None:
    """Reconcile allocator ownership with broker positions/orders and both local ledgers."""
    now = int(now_ms or time.time_ns() // 1_000_000)
    held = {str(x).removeprefix("US.") for x in held_symbols}
    orders = {str(r.get("order_id")): r for r in working_rows if r.get("order_id") is not None}
    ledgers = bot_ledgers or {}
    db = connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        for owner in ("gainer", "orb"):
            for sym in ledgers.get(owner, set()):
                code = str(sym).removeprefix("US.")
                row = db.execute("SELECT owner FROM symbol_owners WHERE account_id=? AND symbol=?", (ACCOUNT_ID, code)).fetchone()
                if not row or row[0] in (owner, "external"):
                    db.execute("INSERT INTO symbol_owners VALUES(?,?,?,?,?,?) ON CONFLICT(account_id,symbol) DO UPDATE SET owner=excluded.owner,state='owned',updated_ms=excluded.updated_ms",
                               (ACCOUNT_ID, code, owner, None, "owned", now))
        for sym in held:
            db.execute("INSERT OR IGNORE INTO symbol_owners VALUES(?,?,?,?,?,?)", (ACCOUNT_ID, sym, "external", None, "held", now))
        for row in working_rows:
            sym = str(row.get("code", row.get("symbol", ""))).removeprefix("US.")
            oid = str(row.get("order_id", ""))
            side = str(row.get("trd_side", row.get("side", "BUY"))).upper()
            if not sym or not oid or not side.endswith("BUY"):
                continue
            reservation = db.execute("SELECT bot FROM capital_reservations WHERE account_id=? AND order_id=?", (ACCOUNT_ID, oid)).fetchone()
            owner = reservation[0] if reservation else "external"
            db.execute("INSERT OR IGNORE INTO symbol_owners VALUES(?,?,?,?,?,?)", (ACCOUNT_ID, sym, owner, oid, "working", now))
            if reservation:
                db.execute("UPDATE capital_reservations SET amount=0,state='broker_visible',updated_ms=? WHERE account_id=? AND order_id=?", (now, ACCOUNT_ID, oid))
        for row in terminal_rows or []:
            oid = str(row.get("order_id", ""))
            if not oid:
                continue
            reservation = db.execute("SELECT bot,symbol,state FROM capital_reservations WHERE account_id=? AND order_id=?", (ACCOUNT_ID, oid)).fetchone()
            if reservation:
                owner, sym, old_state = reservation
                status = str(row.get("status", "terminal"))
                status_code = status.split(".")[-1].upper()
                row_symbol = str(row.get("code", "")).removeprefix("US.")
                side = str(row.get("side", "")).upper().split(".")[-1]
                try:
                    dealt_qty = float(row.get("dealt_qty", 0) or 0)
                except (TypeError, ValueError):
                    dealt_qty = 0.0
                filled_buy = (side.endswith("BUY") and row_symbol == sym
                              and (dealt_qty > 0 or status_code == "FILLED_ALL"))
                if sym in held and filled_buy:
                    current = db.execute("SELECT owner FROM symbol_owners WHERE account_id=? AND symbol=?",
                                         (ACCOUNT_ID, sym)).fetchone()
                    if not current or current[0] in ("external", owner):
                        db.execute("INSERT INTO symbol_owners VALUES(?,?,?,?,?,?) "
                                   "ON CONFLICT(account_id,symbol) DO UPDATE SET owner=excluded.owner, "
                                   "order_id=excluded.order_id,state='held',updated_ms=excluded.updated_ms",
                                   (ACCOUNT_ID, sym, owner, oid, "held", now))
                        _event(db, now, owner, sym, "position_owner_recovered_from_filled_buy",
                               {"order_id": oid, "status": status_code, "dealt_qty": dealt_qty})
                if old_state != "terminal":
                    db.execute("UPDATE capital_reservations SET amount=0,state='terminal',updated_ms=? WHERE account_id=? AND order_id=?", (now, ACCOUNT_ID, oid))
                    _event(db, now, owner, sym, "order_terminal", {"order_id": oid, "status": status})
        for sym in held:
            row = db.execute("SELECT owner FROM symbol_owners WHERE account_id=? AND symbol=?", (ACCOUNT_ID, sym)).fetchone()
            if row:
                db.execute("UPDATE symbol_owners SET state='held',updated_ms=? WHERE account_id=? AND symbol=?", (now, ACCOUNT_ID, sym))
                db.execute("UPDATE capital_reservations SET amount=0,state='position_open',updated_ms=? WHERE account_id=? AND symbol=? AND state IN ('submitting','submitted','broker_visible')",
                           (now, ACCOUNT_ID, sym))
        # Release ownership after a cancelled/closed position is absent from a
        # complete fresh broker snapshot. Pending API submissions expire only
        # after the same refresh has failed to reveal their order.
        live_symbols = held | {str(r.get("code", r.get("symbol", ""))).removeprefix("US.") for r in working_rows}
        stale = db.execute("SELECT token,symbol,order_id,bot FROM capital_reservations WHERE account_id=? AND updated_ms<? AND state NOT IN ('broker_visible','position_open')",
                           (ACCOUNT_ID, now - RESERVATION_STALE_MS)).fetchall()
        for token, sym, oid, owner in stale:
            if str(oid or "") not in orders and sym not in live_symbols:
                db.execute("DELETE FROM capital_reservations WHERE token=?", (token,))
                db.execute("DELETE FROM symbol_owners WHERE account_id=? AND symbol=? AND owner=?", (ACCOUNT_ID, sym, owner))
                _event(db, now, owner, sym, "reservation_expired", {"token": token, "order_id": oid})
        owners = db.execute("SELECT symbol,owner,order_id FROM symbol_owners WHERE account_id=?", (ACCOUNT_ID,)).fetchall()
        for sym, owner, oid in owners:
            if sym not in live_symbols:
                db.execute("DELETE FROM symbol_owners WHERE account_id=? AND symbol=?", (ACCOUNT_ID, sym))
                if oid:
                    db.execute("UPDATE capital_reservations SET amount=0,state='closed',updated_ms=? WHERE account_id=? AND order_id=?", (now, ACCOUNT_ID, oid))
                    _event(db, now, owner, sym, "position_released", {"order_id": oid})
        db.commit()
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()


def owner_of(symbol: str, path: Path = DB_PATH) -> str | None:
    code = str(symbol).removeprefix("US.")
    db = connect(path)
    try:
        row = db.execute("SELECT owner FROM symbol_owners WHERE account_id=? AND symbol=?", (ACCOUNT_ID, code)).fetchone()
        return row[0] if row else None
    finally:
        db.close()


def owned_symbols(bot: str, path: Path = DB_PATH) -> set[str]:
    db = connect(path)
    try:
        return {r[0] for r in db.execute("SELECT symbol FROM symbol_owners WHERE account_id=? AND owner=?", (ACCOUNT_ID, bot))}
    finally:
        db.close()


def reserve(bot: str, symbol: str, requested: float, buying_power: float, path: Path = DB_PATH,
            now_ms: int | None = None) -> tuple[str | None, float]:
    """Atomically grant a risk-sized amount from live free buying power; no fixed bot split."""
    if bot not in {"gainer", "orb"}:
        raise ValueError("unknown paper strategy")
    code = str(symbol).removeprefix("US.")
    req = max(0.0, float(requested)); power = max(0.0, float(buying_power))
    now = int(now_ms or time.time_ns() // 1_000_000)
    db = connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        owner = db.execute("SELECT owner FROM symbol_owners WHERE account_id=? AND symbol=?", (ACCOUNT_ID, code)).fetchone()
        if owner and owner[0] != bot:
            db.commit()
            return None, 0.0
        pending = db.execute("SELECT coalesce(sum(amount),0) FROM capital_reservations WHERE account_id=? AND state IN ('submitting','submitted') AND updated_ms>=?",
                             (ACCOUNT_ID, now - RESERVATION_STALE_MS)).fetchone()[0]
        cap = max(0.0, power * (1.0 - POWER_BUFFER) - float(pending or 0.0))
        amount = min(req, cap)
        if amount <= 0:
            db.commit()
            return None, 0.0
        token = uuid.uuid4().hex
        db.execute("INSERT INTO capital_reservations VALUES(?,?,?,?,?,?,?, ?,?)",
                   (token, ACCOUNT_ID, bot, code, amount, None, "submitting", now, now))
        db.execute("INSERT INTO symbol_owners VALUES(?,?,?,?,?,?) ON CONFLICT(account_id,symbol) DO UPDATE SET owner=excluded.owner,state='reserved',updated_ms=excluded.updated_ms",
                   (ACCOUNT_ID, code, bot, None, "reserved", now))
        _event(db, now, bot, code, "capital_reserved", {"token": token, "requested": req, "granted": amount,
                                                        "buying_power": power, "other_pending": pending})
        db.commit()
        return token, amount
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()


def bind_order(token: str, order_id: str, path: Path = DB_PATH, now_ms: int | None = None) -> None:
    now = int(now_ms or time.time_ns() // 1_000_000)
    db = connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        row = db.execute("SELECT bot,symbol FROM capital_reservations WHERE token=?", (token,)).fetchone()
        if not row:
            raise ValueError("unknown capital reservation")
        bot, sym = row
        db.execute("UPDATE capital_reservations SET order_id=?,state='submitted',updated_ms=? WHERE token=?", (str(order_id), now, token))
        db.execute("UPDATE symbol_owners SET order_id=?,state='reserved',updated_ms=? WHERE account_id=? AND symbol=? AND owner=?", (str(order_id), now, ACCOUNT_ID, sym, bot))
        _event(db, now, bot, sym, "order_bound", {"token": token, "order_id": str(order_id)})
        db.commit()
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()


def release(token: str, reason: str, path: Path = DB_PATH, now_ms: int | None = None) -> None:
    now = int(now_ms or time.time_ns() // 1_000_000)
    db = connect(path)
    try:
        db.execute("BEGIN IMMEDIATE")
        row = db.execute("SELECT bot,symbol,order_id FROM capital_reservations WHERE token=?", (token,)).fetchone()
        if row:
            bot, sym, oid = row
            db.execute("DELETE FROM capital_reservations WHERE token=?", (token,))
            db.execute("DELETE FROM symbol_owners WHERE account_id=? AND symbol=? AND owner=? AND order_id IS NULL", (ACCOUNT_ID, sym, bot))
            _event(db, now, bot, sym, "reservation_released", {"token": token, "order_id": oid, "reason": reason})
        db.commit()
    except Exception:
        db.rollback()
        raise
    finally:
        db.close()

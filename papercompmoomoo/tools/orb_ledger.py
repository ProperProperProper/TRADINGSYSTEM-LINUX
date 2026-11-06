"""Persistent ORB paper ledger.  This module is account-scoped and SIMULATE-only."""
from __future__ import annotations
import json, sqlite3, time
from pathlib import Path
from papercompmoomoo.account_ids import account_id

DB = Path("data/orb.sqlite3")
ACCOUNT_ID = account_id("orb")
ACCOUNT_KIND = "COMPETITION"

SCHEMA = """
CREATE TABLE IF NOT EXISTS account_snapshots(ts_ms INTEGER PRIMARY KEY, account_id INTEGER NOT NULL, account_kind TEXT NOT NULL, equity REAL NOT NULL, cash REAL NOT NULL, buying_power REAL NOT NULL);
CREATE TABLE IF NOT EXISTS candidates(session TEXT NOT NULL, symbol TEXT NOT NULL, rank INTEGER NOT NULL, score REAL NOT NULL, gap_pct REAL, premarket_volume REAL, status TEXT NOT NULL DEFAULT 'selected', detected_ms INTEGER, approved_ms INTEGER, PRIMARY KEY(session,symbol));
CREATE TABLE IF NOT EXISTS orders(order_id TEXT PRIMARY KEY, session TEXT NOT NULL, symbol TEXT NOT NULL, side TEXT NOT NULL, qty INTEGER NOT NULL, limit_price REAL NOT NULL, stop_price REAL, target_price REAL, status TEXT NOT NULL, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, broker_id TEXT, trail_pct REAL NOT NULL DEFAULT 0.0);
CREATE TABLE IF NOT EXISTS positions(symbol TEXT PRIMARY KEY, qty INTEGER NOT NULL, entry_price REAL NOT NULL, stop_price REAL NOT NULL, target_price REAL NOT NULL, high_price REAL NOT NULL, opened_ms INTEGER NOT NULL, status TEXT NOT NULL, initial_target_price REAL NOT NULL DEFAULT 0, trail_pct REAL NOT NULL DEFAULT 0.0);
CREATE TABLE IF NOT EXISTS trades(id INTEGER PRIMARY KEY, symbol TEXT NOT NULL, qty INTEGER NOT NULL, entry_price REAL NOT NULL, exit_price REAL NOT NULL, pnl_usd REAL NOT NULL, reason TEXT NOT NULL, opened_ms INTEGER NOT NULL, closed_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS health(ts_ms INTEGER PRIMARY KEY, state TEXT NOT NULL, message TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS signal_observations(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, session TEXT NOT NULL, symbol TEXT NOT NULL, side TEXT NOT NULL, last_price REAL, orh REAL, orl REAL, vwap REAL, breakout_volume REAL, risk REAL, decision TEXT NOT NULL, reason TEXT NOT NULL, payload_json TEXT NOT NULL DEFAULT '{}');
"""

def connect(path: Path = DB) -> sqlite3.Connection:
    path.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path)
    db.executescript(SCHEMA)
    # Idempotent broker reconciliation: remember how much of each order has
    # already been applied to the local ledger.
    columns = {row[1] for row in db.execute("PRAGMA table_info(orders)")}
    if "processed_qty" not in columns:
        db.execute("ALTER TABLE orders ADD COLUMN processed_qty INTEGER NOT NULL DEFAULT 0")
    if "trail_pct" not in columns:
        db.execute("ALTER TABLE orders ADD COLUMN trail_pct REAL NOT NULL DEFAULT 0.0")
    pcols = {row[1] for row in db.execute("PRAGMA table_info(positions)")}
    if "initial_target_price" not in pcols:
        db.execute("ALTER TABLE positions ADD COLUMN initial_target_price REAL NOT NULL DEFAULT 0")
    if "trail_pct" not in pcols:
        db.execute("ALTER TABLE positions ADD COLUMN trail_pct REAL NOT NULL DEFAULT 0.0")
    ccols = {row[1] for row in db.execute("PRAGMA table_info(candidates)")}
    if "detected_ms" not in ccols:
        db.execute("ALTER TABLE candidates ADD COLUMN detected_ms INTEGER")
    if "approved_ms" not in ccols:
        db.execute("ALTER TABLE candidates ADD COLUMN approved_ms INTEGER")
    db.execute("UPDATE candidates SET approved_ms=(SELECT min(o.created_ms) FROM orders o WHERE o.session=candidates.session AND o.symbol=candidates.symbol AND o.side='BUY' AND o.status NOT LIKE 'rejected%' AND o.status NOT IN ('FAILED','DISABLED','DELETED')) WHERE approved_ms IS NULL")
    db.execute("UPDATE positions SET initial_target_price=target_price WHERE initial_target_price<=0 AND target_price>0")
    from papercompmoomoo.decision_audit import preserve_versions
    preserve_versions(db)
    db.commit()
    return db

def snapshot(payload: dict, path: Path = DB) -> None:
    if int(payload.get("account_id", 0)) != ACCOUNT_ID or payload.get("account_kind") != ACCOUNT_KIND:
        raise ValueError("ORB ledger refuses a non-Competition account snapshot")
    now = int(payload.get("updated_ms") or time.time() * 1000)
    with connect(path) as db:
        db.execute("INSERT OR REPLACE INTO account_snapshots VALUES(?,?,?,?,?,?)", (now, ACCOUNT_ID, ACCOUNT_KIND, float(payload["equity"]), float(payload["cash"]), float(payload["buying_power"])))
        db.execute("INSERT OR REPLACE INTO health VALUES(?,?,?)", (now, "ok", "shared Competition US paper account refreshed"))

if __name__ == "__main__":
    payload = json.loads(Path("data/orb_account.json").read_text())
    snapshot(payload)
    print(json.dumps({"status":"ok","account_id":ACCOUNT_ID,"ledger":str(DB)}))

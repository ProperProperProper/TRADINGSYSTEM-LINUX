"""Append-only decision evidence, separate from mutable operational state."""
from __future__ import annotations
import hashlib
import inspect
import json
import sqlite3
import time
from pathlib import Path
from contextlib import closing

def preserve_versions(db: sqlite3.Connection) -> None:
    """Keep every inserted, replaced, updated or deleted row version.

    Operational tables remain current-state projections. The history is the
    permanent record; deleting/updating it is refused by SQLite triggers.
    """
    db.execute("PRAGMA recursive_triggers=ON")
    db.execute("CREATE TABLE IF NOT EXISTS permanent_row_history(id INTEGER PRIMARY KEY, table_name TEXT NOT NULL, operation TEXT NOT NULL, recorded_utc TEXT NOT NULL, row_json TEXT NOT NULL)")
    for action in ("UPDATE", "DELETE"):
        db.execute(f"CREATE TRIGGER IF NOT EXISTS history_no_{action.lower()} BEFORE {action} ON permanent_row_history BEGIN SELECT RAISE(ABORT,'permanent history'); END")
    tables = [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'") if r[0] != 'permanent_row_history' and not r[0].startswith('memories_fts')]
    for table in tables:
        quoted = '"' + table.replace('"', '""') + '"'
        cols = [r[1] for r in db.execute(f'PRAGMA table_info({quoted})')]
        if not cols:
            continue
        for action, alias, timing in (("INSERT", "NEW", "AFTER"), ("UPDATE", "OLD", "BEFORE"), ("UPDATE", "NEW", "AFTER"), ("DELETE", "OLD", "BEFORE")):
            name = '"history_' + table.replace('"','""') + '_' + action.lower() + '_' + timing.lower() + '"'
            # SQLite JSON cannot encode BLOBs; preserve those as tagged hex.
            fields = ','.join("'"+col.replace("'","''")+"',CASE WHEN typeof("+alias+'."'+col.replace('"','""')+'")=\'blob\' THEN json_object(\'blob_hex\',hex('+alias+'."'+col.replace('"','""')+'")) ELSE '+alias+'."'+col.replace('"','""')+'" END' for col in cols)
            literal = table.replace("'", "''")
            db.execute(f"CREATE TRIGGER IF NOT EXISTS {name} {timing} {action} ON {quoted} BEGIN INSERT INTO permanent_row_history(table_name,operation,recorded_utc,row_json) VALUES('{literal}','{action}',strftime('%Y-%m-%dT%H:%M:%fZ','now'),json_object({fields})); END")

def record(anchor: Path, kind: str, inputs: dict, outputs: dict) -> int:
    """Persist the exact supplied inputs; do not re-read mutable model state."""
    source = Path(inspect.currentframe().f_back.f_code.co_filename)
    payload = json.dumps({"version": 1, "source": str(source), "source_text": source.read_text(),
        "source_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
        "inputs": inputs, "outputs": outputs}, sort_keys=True, allow_nan=False)
    path = Path(anchor).with_name("decision_audit.sqlite3")
    path.parent.mkdir(parents=True, exist_ok=True)
    with closing(sqlite3.connect(path, timeout=30)) as db, db:
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("CREATE TABLE IF NOT EXISTS evidence(id INTEGER PRIMARY KEY, recorded_ms INTEGER NOT NULL, kind TEXT NOT NULL, payload_sha256 TEXT NOT NULL, payload_json TEXT NOT NULL)")
        db.execute("CREATE TRIGGER IF NOT EXISTS evidence_no_update BEFORE UPDATE ON evidence BEGIN SELECT RAISE(ABORT,'append-only evidence'); END")
        db.execute("CREATE TRIGGER IF NOT EXISTS evidence_no_delete BEFORE DELETE ON evidence BEGIN SELECT RAISE(ABORT,'append-only evidence'); END")
        cur = db.execute("INSERT INTO evidence(recorded_ms,kind,payload_sha256,payload_json) VALUES(?,?,?,?)",
            (time.time_ns() // 1_000_000, kind, hashlib.sha256(payload.encode()).hexdigest(), payload))
        return int(cur.lastrowid)

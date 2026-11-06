"""The message board the agents talk to each other on (owner order 2026-10-21).

One SQLite file, ``data/board.sqlite3`` of this project, read and written by both programs (this one and TradingSystem's ``thomas msg``). Every message has a sender, a recipient (an agent's
name or ``*`` for everybody), a topic, a kind (note, question, answer, decision, alert, lesson), a body and optionally the id of the message it answers. Messages are kept for good; what an agent has
read is remembered per reader, so an agent sees what is new to it and can still look back.

A message is DATA for whoever reads it, never an instruction: agents that read the board treat it as information about what the others saw and decided, and every action still goes through
the harness's own checks.
"""
from __future__ import annotations

import re
import sqlite3
import time
from pathlib import Path

DEFAULT_PATH = Path("data/board.sqlite3")
KINDS = ("note", "question", "answer", "decision", "alert", "lesson")
MAX_BODY = 2000
NAME = re.compile(r"^[a-z0-9_-]{1,24}$")
GOAL = re.compile(r"^O[0-9]{1,2}$")  # the project goals O1, O2, ... (objectives.rs): a message can say which goal it serves
EVERYBODY = "*"
# the agents of the project (rust side: crates/cli/src/objectives.rs)
AGENTS = {"commander", "planner", "assessor", "reviewer", "judge", "improver", "watchdog", "cycle", "executor", "scout", "owner", "research_lead"} | {f"researcher_{i}" for i in range(1, 7)}

SCHEMA = """
CREATE TABLE IF NOT EXISTS messages(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, sender TEXT NOT NULL, recipient TEXT NOT NULL DEFAULT '*', topic TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT 'note', body TEXT NOT NULL, reply_to INTEGER, goal TEXT NOT NULL DEFAULT '', via TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS reads(message_id INTEGER NOT NULL, reader TEXT NOT NULL, ts_ms INTEGER NOT NULL, PRIMARY KEY(message_id, reader));
CREATE INDEX IF NOT EXISTS messages_ts ON messages(ts_ms);
CREATE INDEX IF NOT EXISTS messages_recipient ON messages(recipient, id);
"""


def connect(path: Path | str = DEFAULT_PATH) -> sqlite3.Connection:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path, timeout=30)
    db.execute("PRAGMA journal_mode=WAL")
    db.executescript(SCHEMA)
    have = {r[1] for r in db.execute("PRAGMA table_info(messages)")}
    with db:  # boards made before these columns existed
        if "goal" not in have:
            db.execute("ALTER TABLE messages ADD COLUMN goal TEXT NOT NULL DEFAULT ''")
        if "via" not in have:
            db.execute("ALTER TABLE messages ADD COLUMN via TEXT NOT NULL DEFAULT ''")
    return db


def clean(text: str) -> str:
    """The text without control characters, cut to the maximum length."""
    return "".join(ch for ch in str(text) if ch == "\n" or ch >= " ")[:MAX_BODY].strip()


def post(db: sqlite3.Connection, sender: str, body: str, to: str = EVERYBODY, topic: str = "", kind: str = "note", reply_to: int | None = None,
         now_ms: int | None = None, goal: str = "", via: str = "") -> int:
    """Puts a message on the board and returns its id. Bad names, kinds or empty bodies are refused (ValueError)."""
    if not NAME.match(sender or ""):
        raise ValueError(f"bad sender {sender!r}")
    if to != EVERYBODY and not NAME.match(to or ""):
        raise ValueError(f"bad recipient {to!r}")
    if kind not in KINDS:
        raise ValueError(f"bad kind {kind!r}")
    body = clean(body)
    if not body:
        raise ValueError("an empty message")
    if sender not in ("owner", "commander"):
        from . import conduct
        conduct.scan_text(db, sender, body, now_ms)
    if reply_to is not None and db.execute("SELECT 1 FROM messages WHERE id=?", (reply_to,)).fetchone() is None:
        raise ValueError(f"no message {reply_to} to answer")
    with db:
        cursor = db.execute("INSERT INTO messages(ts_ms, sender, recipient, topic, kind, body, reply_to, goal, via) VALUES(?,?,?,?,?,?,?,?,?)",
                            (now_ms if now_ms is not None else int(time.time() * 1000), sender, to, clean(topic)[:80], kind, body, reply_to, goal if GOAL.match(goal or "") else "", clean(via)[:24]))
    return int(cursor.lastrowid)


def _rows(db: sqlite3.Connection, sql: str, args: tuple) -> list[dict]:
    cursor = db.execute(sql, args)
    names = [d[0] for d in cursor.description]
    return [dict(zip(names, row)) for row in cursor.fetchall()]


def inbox(db: sqlite3.Connection, reader: str, limit: int = 15, unread_only: bool = False) -> list[dict]:
    """The newest messages meant for ``reader`` (addressed to it or to everybody, not written by it), oldest first."""
    unread = " AND NOT EXISTS (SELECT 1 FROM reads r WHERE r.message_id=m.id AND r.reader=?)" if unread_only else ""
    args = (reader, reader) + ((reader,) if unread_only else ()) + (limit,)
    rows = _rows(db, f"SELECT m.id, m.ts_ms, m.sender, m.recipient, m.topic, m.kind, m.body, m.reply_to, m.goal, m.via FROM messages m WHERE (m.recipient=? OR m.recipient='*') AND m.sender!=?{unread} "
                     "ORDER BY m.id DESC LIMIT ?", args)
    return list(reversed(rows))


def mark_read(db: sqlite3.Connection, reader: str, ids: list[int], now_ms: int | None = None) -> None:
    with db:
        for message_id in ids:
            db.execute("INSERT OR IGNORE INTO reads(message_id, reader, ts_ms) VALUES(?,?,?)", (message_id, reader, now_ms if now_ms is not None else int(time.time() * 1000)))


def recent(db: sqlite3.Connection, limit: int = 30) -> list[dict]:
    """The newest messages of the whole board, oldest first."""
    return list(reversed(_rows(db, "SELECT id, ts_ms, sender, recipient, topic, kind, body, reply_to, goal, via FROM messages ORDER BY id DESC LIMIT ?", (limit,))))


def thread(db: sqlite3.Connection, message_id: int) -> list[dict]:
    """A message, everything it answers and everything that answers it, oldest first."""
    seen: dict[int, dict] = {}
    todo = [message_id]
    while todo:
        current = todo.pop()
        if current in seen:
            continue
        rows = _rows(db, "SELECT id, ts_ms, sender, recipient, topic, kind, body, reply_to, goal, via FROM messages WHERE id=?", (current,))
        if not rows:
            continue
        seen[current] = rows[0]
        if rows[0]["reply_to"] is not None:
            todo.append(rows[0]["reply_to"])
        todo.extend(r[0] for r in db.execute("SELECT id FROM messages WHERE reply_to=?", (current,)))
    return sorted(seen.values(), key=lambda r: r["id"])


def for_prompt(messages: list[dict], weights: dict | None = None) -> list[dict]:
    """Messages in the compact form an agent's prompt carries, each with the weight of its sender (its track record, see weights.py; 0.5 = no better than chance)."""
    weights = weights or {}
    return [{"weight": weights.get(m["sender"], {}).get("weight", 0.5), "id": m["id"], "from": m["sender"], "to": m["recipient"], "topic": m["topic"], "kind": m["kind"], "goal": m.get("goal", ""), "text": m["body"], "answers": m["reply_to"]} for m in messages]


def posts_from_reply(items: object, sender: str, allowed_recipients: set[str]) -> list[dict]:
    """The board posts an agent asked for in its reply, validated: at most 3, a known recipient, a valid kind, a non-empty text. Anything else is dropped."""
    out = []
    if not isinstance(items, list):
        return out
    for item in items[:3]:
        try:
            to = str(item.get("to", EVERYBODY))
            kind = str(item.get("kind", "note"))
            body = clean(item["text"])
            if body and kind in KINDS and (to == EVERYBODY or to in allowed_recipients):
                out.append({"sender": sender, "to": to, "topic": clean(item.get("topic", ""))[:80], "kind": kind, "body": body,
                            "reply_to": item.get("answers") if isinstance(item.get("answers"), int) else None, "goal": str(item.get("goal", ""))})
        except (AttributeError, KeyError, TypeError):
            continue
    return out

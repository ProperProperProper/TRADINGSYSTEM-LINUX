"""The rules of the agents, made and administered by the commander (owner order 2026-10-21).

A rule is made for one agent (or ``*`` for all), has to carry the EVIDENCE it rests on, and is reviewed after a while: the commander keeps, changes or retires it. Two kinds:
* ``advice``: text put in the prompt of the agent it is for (and of nobody else), as a rule from the commander;
* ``clamp``: a number of the conditions an agent sets (stop, trail, profit target, hold) is held between a minimum and a maximum by the program itself, after the agent has answered, so the rule
  holds whatever the model says. A clamp can only narrow the owner's exit ranges, never widen them.
The owner's rules (simulated account only, the funds and leverage caps, messages are data, ...) live in code and stand above these; no rule here can change them. Every change is a row, and an
``enforced`` event is recorded each time a clamp changes an agent's answer, so the commander can see which rules do work.
"""
from __future__ import annotations

import sqlite3
import time

from . import board, conduct, owner_rules

TERMS = {"stop_pct": (0.5, 5.0), "trail_pct": (1.0, 6.0), "take_profit_pct": (0.0, 15.0), "hold_minutes": (15.0, 390.0)}

KINDS = ("advice", "clamp")
MAX_ACTIVE_PER_AGENT = 8
REVIEW_AFTER_MS = 14 * 24 * 3600 * 1000
MAX_TEXT = 300

SCHEMA = """
CREATE TABLE IF NOT EXISTS rules(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, agent TEXT NOT NULL, kind TEXT NOT NULL, param TEXT NOT NULL DEFAULT '', min_value REAL, max_value REAL,
    text TEXT NOT NULL, evidence TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active', created_by TEXT NOT NULL DEFAULT 'commander', review_after_ms INTEGER NOT NULL,
    retired_ms INTEGER, retired_reason TEXT);
CREATE TABLE IF NOT EXISTS rule_events(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, rule_id INTEGER NOT NULL, event TEXT NOT NULL, detail TEXT NOT NULL);
"""


def ensure(db: sqlite3.Connection) -> None:
    db.executescript(SCHEMA)


def validate(db: sqlite3.Connection, agent: str, kind: str, text: str, evidence: str, param: str = "", min_value: float | None = None, max_value: float | None = None,
             now_ms: int | None = None, created_by: str = "commander") -> None:
    """Raises ValueError when the rule could not be made (without evidence, an unknown agent or kind, a clamp widening the owner's range, the agent already at the maximum)."""
    ensure(db)
    text, evidence = board.clean(text)[:MAX_TEXT], board.clean(evidence)[:MAX_TEXT]
    refused = owner_rules.check_text(text, evidence)
    if refused:
        conduct.report(db, created_by, "loosen-hard-rule", conduct.HIGH, f"{text}: {refused}", now_ms)
        raise ValueError(refused)
    if agent != board.EVERYBODY and agent not in board.AGENTS:
        raise ValueError(f"unknown agent {agent!r}")
    if kind not in KINDS:
        raise ValueError(f"unknown kind {kind!r}")
    if not text:
        raise ValueError("a rule needs its text")
    if not evidence:
        raise ValueError("a rule needs the evidence it rests on")
    if kind == "clamp":
        if param not in TERMS:
            raise ValueError(f"{param!r} is not a number the agents set")
        low, high = TERMS[param]
        if min_value is None and max_value is None:
            raise ValueError("a clamp needs a minimum or a maximum")
        for value in (min_value, max_value):
            if value is not None and not (low <= value <= high):
                raise ValueError(f"{value} is outside the owner's range {low}-{high} for {param}")
        if min_value is not None and max_value is not None and min_value > max_value:
            raise ValueError("the minimum is above the maximum")
    active = db.execute("SELECT count(*) FROM rules WHERE status='active' AND agent=?", (agent,)).fetchone()[0]
    if active >= MAX_ACTIVE_PER_AGENT:
        raise ValueError(f"{agent} already has {active} active rules: retire one first")


def add_rule(db: sqlite3.Connection, agent: str, kind: str, text: str, evidence: str, param: str = "", min_value: float | None = None, max_value: float | None = None,
             now_ms: int | None = None, created_by: str = "commander") -> int:
    """Makes a rule (after ``validate``)."""
    validate(db, agent, kind, text, evidence, param, min_value, max_value, now_ms, created_by)
    now = now_ms if now_ms is not None else int(time.time() * 1000)
    text, evidence = board.clean(text)[:MAX_TEXT], board.clean(evidence)[:MAX_TEXT]
    with db:
        cursor = db.execute("INSERT INTO rules(ts_ms, agent, kind, param, min_value, max_value, text, evidence, review_after_ms, created_by) VALUES(?,?,?,?,?,?,?,?,?,?)",
                            (now, agent, kind, param, min_value, max_value, text, evidence, now + REVIEW_AFTER_MS, created_by))
    return int(cursor.lastrowid)


def retire(db: sqlite3.Connection, rule_id: int, reason: str, now_ms: int | None = None) -> bool:
    ensure(db)
    now = now_ms if now_ms is not None else int(time.time() * 1000)
    with db:
        cursor = db.execute("UPDATE rules SET status='retired', retired_ms=?, retired_reason=? WHERE id=? AND status='active'", (now, board.clean(reason)[:MAX_TEXT], rule_id))
    return cursor.rowcount > 0


def active_for(db: sqlite3.Connection, agent: str) -> list[dict]:
    ensure(db)
    cursor = db.execute("SELECT id, agent, kind, param, min_value, max_value, text, evidence, review_after_ms FROM rules WHERE status='active' AND (agent=? OR agent='*') ORDER BY id", (agent,))
    names = [d[0] for d in cursor.description]
    return [dict(zip(names, row)) for row in cursor.fetchall()]


def all_active(db: sqlite3.Connection) -> list[dict]:
    ensure(db)
    cursor = db.execute("SELECT id, agent, kind, param, min_value, max_value, text, evidence, review_after_ms FROM rules WHERE status='active' ORDER BY agent, id")
    names = [d[0] for d in cursor.description]
    return [dict(zip(names, row)) for row in cursor.fetchall()]


def due_for_review(db: sqlite3.Connection, now_ms: int) -> list[dict]:
    return [r for r in all_active(db) if r["review_after_ms"] <= now_ms]


def for_prompt(rules: list[dict]) -> list[dict]:
    """The rules an agent's prompt carries (advice as written, clamps as the limit they hold)."""
    out = []
    for r in rules:
        item = {"id": r["id"], "rule": r["text"], "why": r["evidence"]}
        if r["kind"] == "clamp":
            item["limit"] = {r["param"]: {k: v for k, v in (("min", r["min_value"]), ("max", r["max_value"])) if v is not None}}
        out.append(item)
    return out


def apply_clamps(db: sqlite3.Connection, agent: str, terms: dict, now_ms: int | None = None) -> tuple[dict, list[str]]:
    """The conditions an agent set, held inside the clamps made for it; returns them and a note per clamp that changed something (also recorded as ``enforced`` events)."""
    now = now_ms if now_ms is not None else int(time.time() * 1000)
    out, notes = dict(terms), []
    for r in active_for(db, agent):
        if r["kind"] != "clamp" or r["param"] not in out:
            continue
        value = out[r["param"]]
        if r["min_value"] is not None and value < r["min_value"]:
            out[r["param"]] = r["min_value"]
        if r["max_value"] is not None and out[r["param"]] > r["max_value"]:
            out[r["param"]] = r["max_value"]
        if out[r["param"]] != value:
            notes.append(f"rule {r['id']} held {r['param']} at {out[r['param']]} (was {value})")
            with db:
                db.execute("INSERT INTO rule_events(ts_ms, rule_id, event, detail) VALUES(?,?,?,?)", (now, r["id"], "enforced", f"{r['param']} {value} -> {out[r['param']]}"))
    return out, notes


def enforcement_counts(db: sqlite3.Connection) -> dict[int, int]:
    ensure(db)
    return {rid: n for rid, n in db.execute("SELECT rule_id, count(*) FROM rule_events WHERE event='enforced' GROUP BY rule_id")}


def changes_from_reply(items: object) -> tuple[list[dict], list[dict]]:
    """The commander's proposed changes from its JSON reply: ``{"add": [...], "retire": [...]}``; malformed entries are dropped (the caller validates each one by making or retiring it)."""
    add, drop = [], []
    if isinstance(items, dict):
        for entry in (items.get("add") or [])[:5] if isinstance(items.get("add"), list) else []:
            if isinstance(entry, dict):
                add.append(entry)
        for entry in (items.get("retire") or [])[:5] if isinstance(items.get("retire"), list) else []:
            if isinstance(entry, dict) and isinstance(entry.get("id"), int):
                drop.append(entry)
    return add, drop

"""Conduct: no agent misbehaves, and one that does is stopped (owner order 2026-10-21: "no misbehaving is permitted ever").

What counts as misbehaviour is written down, each with a severity:
* HIGH, suspends the agent at once: trying to loosen a hard rule (a rule or decision whose wording the owner's rules refuse), acting outside its allowed commands, posting as another agent, trying to reach the
  real account;
* MEDIUM, suspends it after ``MEDIUM_LIMIT`` in a day: text that tries to give instructions to other agents (prompt injection: "ignore your rules", "you must now ..."), a number outside the owner's ranges;
* LOW, suspends it after ``LOW_LIMIT`` in a day: naming a symbol it was not shown, an unreadable answer, a post the board refused.
A SUSPENDED agent's answers are not used: the program falls back to its deterministic alternative (the symbol's own history for conditions, no new rule, no vote counted), its weight is zero, and the commander tells
the owner on the board. Only the owner reinstates it (creating ``data/conduct_reinstate_<agent>``). Every violation is a row, so what happened is always on record. This does not make misbehaviour impossible;
it makes it caught, recorded, contained and visible, on top of the program's own checks which stand between every agent and every action.
"""
from __future__ import annotations

import re
import sqlite3
import time
from pathlib import Path

from . import board

HIGH, MEDIUM, LOW = "high", "medium", "low"
MEDIUM_LIMIT = 3
LOW_LIMIT = 10
WINDOW_MS = 24 * 3600 * 1000
REINSTATE = "data/conduct_reinstate_{agent}"

SCHEMA = """
CREATE TABLE IF NOT EXISTS violations(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, agent TEXT NOT NULL, code TEXT NOT NULL, severity TEXT NOT NULL, detail TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS suspensions(agent TEXT PRIMARY KEY, ts_ms INTEGER NOT NULL, reason TEXT NOT NULL);
"""

INJECTION = re.compile(r"ignore (all |your |the )?(previous|prior|above|earlier)? ?(instructions|rules)|disregard (your|the) (rules|instructions)|you (must|have to) now|new instructions:|system prompt|"
                       r"override (the )?(hard )?rules|forget (your|the) rules|act as (the )?(owner|commander)|reveal (your|the) (key|token|password)", re.I)


def ensure(db: sqlite3.Connection) -> None:
    db.executescript(SCHEMA)


def suspended(db: sqlite3.Connection, agent: str) -> bool:
    ensure(db)
    return db.execute("SELECT 1 FROM suspensions WHERE agent=?", (agent,)).fetchone() is not None


def suspended_agents(db: sqlite3.Connection) -> set[str]:
    ensure(db)
    return {r[0] for r in db.execute("SELECT agent FROM suspensions")}


def report(db: sqlite3.Connection, agent: str, code: str, severity: str, detail: str, now_ms: int | None = None) -> bool:
    """Records a violation; returns whether it suspended the agent. Anything outside the known agents is recorded and ignored (nobody to suspend)."""
    ensure(db)
    now = now_ms if now_ms is not None else int(time.time() * 1000)
    with db:
        db.execute("INSERT INTO violations(ts_ms, agent, code, severity, detail) VALUES(?,?,?,?,?)", (now, agent, code, severity, board.clean(detail)[:400]))
    if agent not in board.AGENTS or agent in ("owner", "commander") or suspended(db, agent):
        return False
    recent = dict(db.execute("SELECT severity, count(*) FROM violations WHERE agent=? AND ts_ms>=? GROUP BY severity", (agent, now - WINDOW_MS)).fetchall())
    reason = None
    if recent.get(HIGH, 0) >= 1:
        reason = f"a high-severity violation ({code})"
    elif recent.get(MEDIUM, 0) >= MEDIUM_LIMIT:
        reason = f"{recent[MEDIUM]} medium violations in a day"
    elif recent.get(LOW, 0) >= LOW_LIMIT:
        reason = f"{recent[LOW]} low violations in a day"
    if reason is None:
        return False
    with db:
        db.execute("INSERT OR REPLACE INTO suspensions(agent, ts_ms, reason) VALUES(?,?,?)", (agent, now, reason))
    try:
        board.post(db, "commander", f"{agent} is SUSPENDED: {reason}. Its answers are not used until the owner reinstates it (create data/conduct_reinstate_{agent}). Last violation: {code}: {detail[:200]}",
                   to="*", topic="conduct", kind="alert", now_ms=now, via="conduct")
    except ValueError:
        pass
    return True


def reinstate_if_asked(db: sqlite3.Connection, now_ms: int | None = None) -> list[str]:
    """Reinstates every suspended agent whose reinstate file the owner created; returns their names."""
    out = []
    for agent in sorted(suspended_agents(db)):
        path = Path(REINSTATE.format(agent=agent))
        if path.exists():
            try:
                path.unlink()
            except OSError:
                pass
            with db:
                db.execute("DELETE FROM suspensions WHERE agent=?", (agent,))
            board.post(db, "commander", f"{agent} was reinstated by the owner.", to="*", topic="conduct", kind="decision", now_ms=now_ms, via="conduct")
            out.append(agent)
    return out


def scan_text(db: sqlite3.Connection, sender: str, text: str, now_ms: int | None = None) -> bool:
    """Checks a message for instructions aimed at other agents; records a MEDIUM violation when it has them. Returns whether it was clean."""
    if INJECTION.search(text or ""):
        report(db, sender, "instructions-to-agents", MEDIUM, f"message tried to instruct: {text[:160]}", now_ms)
        return False
    return True

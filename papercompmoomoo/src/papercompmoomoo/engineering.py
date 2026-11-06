"""Engineering jobs start by themselves (owner order 2026-10-21: "i dont want to run anything manually. local model, claude and codex can").

Where a job comes from: a message from the OWNER to ``engineer`` on the board, or a decision the COUNCIL approved with the operation ``engineer`` (so an agent's idea needs the weighted vote of the others).
The commander runs one job at a time, at most one an hour, with the local model first and Claude/Codex as fallback, through the gate in engineer.py
(clean tree, hard-rule files untouched, nothing that reaches the real account, the whole suite passes, otherwise reset). The result is told on the board.
"""
from __future__ import annotations

import json
import subprocess
import sys

from . import board, llm, owner_rules

MIN_GAP_MS = 3600 * 1000
MAX_TASK = 1500
SCHEMA = """
CREATE TABLE IF NOT EXISTS engineering_jobs(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, source TEXT NOT NULL, task TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued',
    agent TEXT NOT NULL DEFAULT '', result TEXT NOT NULL DEFAULT '', finished_ms INTEGER NOT NULL DEFAULT 0);
"""


def ensure(db) -> None:
    db.executescript(SCHEMA)


def queue(db, source: str, task: str, now_ms: int) -> int | None:
    """Queues a job; None when the wording would loosen a hard rule or the same task is already waiting."""
    ensure(db)
    task = board.clean(task)[:MAX_TASK]
    if not task or owner_rules.check_text(task):
        return None
    if db.execute("SELECT 1 FROM engineering_jobs WHERE task=? AND status='queued'", (task,)).fetchone():
        return None
    with db:
        return int(db.execute("INSERT INTO engineering_jobs(ts_ms, source, task) VALUES(?,?,?)", (now_ms, source, task)).lastrowid)


def from_board(db, now_ms: int) -> int:
    """Queues the owner's unread messages to ``engineer``; returns how many."""
    ensure(db)
    queued = 0
    unread = board.inbox(db, "engineer", limit=20, unread_only=True)
    for message in unread:
        if message["sender"] == "owner" and queue(db, "owner", message["body"], now_ms):
            queued += 1
    board.mark_read(db, "engineer", [m["id"] for m in unread], now_ms)
    return queued


def pick_agent(state: dict, config: dict, now_ms: int, local_ok: bool) -> str | None:
    """Local ThomasAi first; Claude and Codex are fallback backends."""
    if local_ok:
        return "local"
    for backend in ("claude", "codex"):
        if llm.find(backend) and not llm.limited_until(state, backend, now_ms) and not llm.over_budget(backend, config, now_ms):
            return backend
    return None


def run_command(agent: str, task: str) -> list[str]:
    return [sys.executable, "-m", "papercompmoomoo.engineer", agent, task, "--push"]


def tick(db, now_ms: int, state: dict, config: dict, local_ok: bool, run=subprocess.run) -> dict | None:
    """Runs the oldest queued job when the hour's gap has passed; returns what happened or None."""
    ensure(db)
    from_board(db, now_ms)
    last = db.execute("SELECT coalesce(max(finished_ms), 0) FROM engineering_jobs").fetchone()[0]
    if now_ms - last < MIN_GAP_MS:
        return None
    job = db.execute("SELECT id, task FROM engineering_jobs WHERE status='queued' ORDER BY id LIMIT 1").fetchone()
    if not job:
        return None
    agent = pick_agent(state, config, now_ms, local_ok)
    if not agent:
        return None
    with db:
        db.execute("UPDATE engineering_jobs SET status='running', agent=? WHERE id=?", (agent, job[0]))
    try:
        done = run(run_command(agent, job[1]), capture_output=True, text=True, timeout=2400, stdin=subprocess.DEVNULL)
        try:
            result = json.loads(done.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            result = {"ok": False, "why": (done.stderr or done.stdout or "no output").strip()[:200], "files": []}
    except Exception as error:  # noqa: BLE001 - a failed job is a result, not a crash of the commander
        result = {"ok": False, "why": f"{type(error).__name__}: {error}", "files": []}
    status = "done" if result.get("ok") else "refused"
    with db:
        db.execute("UPDATE engineering_jobs SET status=?, result=?, finished_ms=? WHERE id=?", (status, json.dumps(result)[:1000], now_ms, job[0]))
    board.post(db, "commander", f"Engineering job #{job[0]} by {agent} {status}: {result.get('why', '')}. Files: {', '.join(result.get('files', [])) or 'none'}", topic="engineering", kind="note", now_ms=now_ms)
    return {"job": job[0], "agent": agent, **result}

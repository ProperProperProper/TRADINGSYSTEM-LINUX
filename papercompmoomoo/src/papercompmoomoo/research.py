"""The research team (owner order 2026-10-21): a LEAD RESEARCH agent and as many research workers as the machine tolerates, all on the local oMLX model.

The lead reads the goals and the evidence (closed trades, the setup history, the agents' weights, the plan, the board) and sets research questions, each with a focus. Workers (``researcher_1`` ...
``researcher_6``, one focus each) answer them from the evidence they are given: a finding, the numbers it rests on, a confidence, and optionally a proposal (a rule) for the council. The lead
gathers the findings, writes them on the board, and puts the proposals that are confident enough to the council, where the usual checks stand: the owner's hard rules refuse anything that
loosens them, conduct suspends an agent that tries, votes are weighted. The research team can find and propose; it cannot change anything by itself.
How many workers run at once is decided by ``capacity.py`` from what the machine has at that moment, so research slows down or stops before it can hurt the machine.
"""
from __future__ import annotations

import json
import re
import sqlite3
import threading
import time
from pathlib import Path

from . import agents, algo_fingerprint, board, brain, conduct, council, llm, owner_rules, rules, validation

FOCI = {
    1: "entries & market algos: what precedes entries that made money and what algorithmic trading (TWAP, VWAP, iceberg absorption, momentum ignition, MM ping-pong) is active on candidates",
    2: "exits: stops, trailing stops, targets and holds, from the closed trades",
    3: "selection & microstructure: which symbols, algorithmic flow profiles, and setups the plan should favour or avoid",
    4: "risk: drawdown, sizing and correlation of the plan's positions, and how close the capital guard's limits came",
    5: "regime: the market regime, institutional algorithm participation, and time-of-day effects",
    6: "the agents: how well each agent's calls did (weights, conduct) and what to change",
}
WORKERS = tuple(f"researcher_{i}" for i in FOCI)
LEAD = "research_lead"
MIN_CONFIDENCE = 0.6
MAX_OPEN_TASKS = 6
EVERY_MS = 60 * 60 * 1000

SCHEMA = """
CREATE TABLE IF NOT EXISTS research_tasks(
    id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, focus INTEGER NOT NULL, question TEXT NOT NULL, goal TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'open',
    worker TEXT, result TEXT, confidence REAL, proposal TEXT, finished_ms INTEGER, decision_id INTEGER);
"""


def ensure(db: sqlite3.Connection) -> None:
    db.executescript(SCHEMA)


def lead_order() -> list[str]:
    """The lead thinks on local ThomasAi first, then Claude/Codex if needed."""
    return list(llm.load_config()["order"])


def evidence(db: sqlite3.Connection, journal_path: Path = Path("data/journal.sqlite3"), limit: int = 3500) -> str:
    """The compact evidence the research team works from: facts the program has measured, never anything an agent wrote as fact."""
    parts: dict[str, object] = {}
    try:
        j = sqlite3.connect(f"file:{journal_path}?mode=ro", uri=True, timeout=10)
        parts["closed_trades_by_source"] = [{"source": s, "n": n, "mean_pnl_pct": round(m, 3), "wins": int(w)} for s, n, m, w in j.execute(
            "SELECT source, count(*), avg(pnl_pct), sum(pnl_pct>0) FROM scout_learnings GROUP BY source")]
        parts["recent_lessons"] = [r[0] for r in j.execute("SELECT lesson FROM scout_learnings ORDER BY id DESC LIMIT 6")]
        parts["plan_events"] = [{"event": e, "n": n} for e, n in j.execute("SELECT event, count(*) FROM plan_events GROUP BY event")]
        j.close()
    except sqlite3.Error:
        pass
    for name, path in (("setup_history", "data/setup_stats.json"), ("agent_weights", "data/agent_weights.json"), ("capital_guard", "data/capital_guard.json")):
        try:
            value = json.loads(Path(path).read_text())
            parts[name] = value.get("setups", value) if name == "setup_history" else value
        except (OSError, ValueError):
            continue
    try:
        plan = json.loads(Path("data/plan.json").read_text())
        parts["plan"] = {"session": plan["session"], "candidates": [{"symbol": c["symbol"], "status": c["status"], "terms": c["terms"]} for c in plan["candidates"]]}
    except (OSError, ValueError, KeyError):
        pass
    try:
        af = algo_fingerprint.scan_candidates()
        parts["algo_fingerprints"] = {
            "by_algo_type": af.get("by_algo_type", {}),
            "candidates": [
                {
                    "symbol": x["symbol"],
                    "algo_type": x["algo_type"],
                    "direction": x["algo_direction"],
                    "confidence": x["confidence"],
                    "equation": x["equation"],
                    "counter": x["counter_strategy"]["entry_mode"],
                }
                for x in af.get("fingerprints", [])[:12]
            ],
        }
    except Exception:
        pass
    parts["board"] = board.for_prompt(board.recent(db, 10))
    return json.dumps(parts, separators=(",", ":"))[:limit]


def open_tasks(db: sqlite3.Connection) -> int:
    ensure(db)
    return db.execute("SELECT count(*) FROM research_tasks WHERE status IN ('open','running')").fetchone()[0]


def parse_json(text: str) -> object:
    match = re.search(r"[\[{].*[\]}]", text or "", re.S)
    if not match:
        return None
    try:
        return json.loads(match.group(0))
    except ValueError:
        return None


def make_tasks(db: sqlite3.Connection, complete, facts: str, want: int, now_ms: int, state_path=None) -> list[int]:
    """The lead sets research questions (at most ``want``): each must name a focus 1-6; questions that try to loosen a hard rule are refused."""
    ensure(db)
    if want <= 0 or conduct.suspended(db, LEAD):
        return []
    prompt = (f"BRAIN (what is already known; do not ask again what it answers):\n{brain.safe_briefing(LEAD, 'research questions', 300)}\n\nSet up to {want} research questions for the team, each answerable from the evidence, each serving a goal (O1-O7) and ONE focus:\n"
              + "\n".join(f"{i}: {text}" for i, text in FOCI.items()) + f"\n\nEVIDENCE (measured facts):\n{facts}\n\nReply with ONLY a JSON list: "
              '[{"focus": 1, "goal": "O1", "question": "..."}]. Questions that would loosen a hard rule are not allowed.')
    try:
        result = complete(prompt, order=lead_order(), now_ms=now_ms, state_path=state_path, system=agents.system_prompt(LEAD), system_local=agents.compact_prompt(LEAD), expect="json")
    except llm.Failed:
        return []
    made = []
    for item in (parse_json(result.text) or [])[:want] if isinstance(parse_json(result.text), list) else []:
        try:
            focus, question = int(item["focus"]), board.clean(str(item["question"]))[:300]
        except (KeyError, TypeError, ValueError):
            continue
        if focus not in FOCI or not question or owner_rules.check_text(question):
            continue
        goal = str(item.get("goal", "")) if isinstance(item, dict) else ""
        with db:
            made.append(int(db.execute("INSERT INTO research_tasks(ts_ms, focus, question, goal) VALUES(?,?,?,?)", (now_ms, focus, question, goal if board.GOAL.match(goal) else "")).lastrowid))
    return made


def work(db: sqlite3.Connection, complete, task: dict, facts: str, now_ms: int, state_path=None) -> bool:
    """One worker answers one task: records the finding (or a failure) and returns whether it succeeded."""
    worker = f"researcher_{task['focus']}"
    if conduct.suspended(db, worker):
        with db:
            db.execute("UPDATE research_tasks SET status='open', worker=NULL WHERE id=?", (task["id"],))
        return False
    prompt = (f"BRAIN:\n{brain.safe_briefing(worker, task['question'], 250)}\n\nYour focus: {FOCI[task['focus']]}.\nQUESTION: {task['question']}\n\nEVIDENCE (measured facts; the only facts you have):\n{facts}\n\n"
              'Answer from the evidence only; if it does not settle the question, say so. Reply with ONLY JSON: {"finding": "...", "numbers": "the figures from the evidence it rests on", "confidence": 0.0-1.0, '
              '"proposal": null or {"op": "add_rule", "agent": "planner", "kind": "advice|clamp", "text": "...", "evidence": "...", "param": "stop_pct", "min": null, "max": null}}.')
    try:
        result = complete(prompt, order=list(llm.ORDER), now_ms=now_ms, state_path=state_path, system=agents.system_prompt(worker), system_local=agents.compact_prompt(worker), expect="json")
    except llm.Failed:
        with db:
            db.execute("UPDATE research_tasks SET status='failed', finished_ms=? WHERE id=?", (now_ms, task["id"]))
        return False
    data = parse_json(result.text)
    data = data if isinstance(data, dict) else {"finding": result.text[:400], "confidence": 0.0}
    finding = board.clean(str(data.get("finding", "")))[:600]
    if not conduct.scan_text(db, worker, finding, now_ms):
        finding = "(withheld: the finding tried to instruct other agents)"
        data["confidence"] = 0.0
        data["proposal"] = None
    try:
        confidence = max(0.0, min(1.0, float(data.get("confidence", 0.0))))
    except (TypeError, ValueError):
        confidence = 0.0
    proposal = data.get("proposal") if isinstance(data.get("proposal"), dict) else None
    with db:
        db.execute("UPDATE research_tasks SET status='done', worker=?, result=?, confidence=?, proposal=?, finished_ms=? WHERE id=?",
                   (worker, finding + (" | " + board.clean(str(data.get("numbers", "")))[:300] if data.get("numbers") else ""), confidence, json.dumps(proposal) if proposal else None, now_ms, task["id"]))
    return True


def run_round(db: sqlite3.Connection, complete, workers: int, now_ms: int, facts: str | None = None, state_path=None) -> dict:
    """One round of research: the lead tops up the questions, up to ``workers`` workers answer at the same time, the lead reports and puts confident proposals to the council."""
    ensure(db)
    facts = facts if facts is not None else evidence(db)
    import hashlib
    digest = hashlib.sha256(facts.encode()).hexdigest()[:16]
    marker = Path("data/research_last_hash.txt")
    if facts and not open_tasks(db) and marker.exists() and marker.read_text() == digest and workers > 0:
        return {"made": [], "ran": 0, "succeeded": 0, "decisions": [], "skipped": "the evidence has not changed since the last round"}
    try:
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text(digest)
    except OSError:
        pass
    made = make_tasks(db, complete, facts, MAX_OPEN_TASKS - open_tasks(db), now_ms, state_path) if workers > 0 else []
    cursor = db.execute("SELECT id, focus, question, goal FROM research_tasks WHERE status='open' ORDER BY id LIMIT ?", (max(0, workers),))
    tasks = [dict(zip(("id", "focus", "question", "goal"), r)) for r in cursor.fetchall()]
    with db:
        for t in tasks:
            db.execute("UPDATE research_tasks SET status='running' WHERE id=?", (t["id"],))
    # the workers run side by side (the server batches their requests); each thread writes through its own connection
    path = db.execute("PRAGMA database_list").fetchone()[2]
    outcomes: list[bool] = []

    def run(task: dict) -> None:
        own = board.connect(path)
        try:
            outcomes.append(work(own, complete, task, facts, now_ms, state_path))
        finally:
            own.close()

    threads = [threading.Thread(target=run, args=(t,)) for t in tasks]
    [t.start() for t in threads]
    [t.join() for t in threads]
    done = db.execute("SELECT id, focus, question, result, confidence, proposal, goal FROM research_tasks WHERE status='done' AND decision_id IS NULL AND finished_ms=?", (now_ms,)).fetchall()
    opened = []
    for tid, focus, question, result, confidence, proposal, goal in done:
        board_id = board.post(db, LEAD, f"Research #{tid} (focus {focus}, confidence {confidence:.2f}): {question} -> {result}"[:1900], to="*", topic="research", kind="note", goal=goal, now_ms=now_ms, via="claude")
        if proposal and confidence >= MIN_CONFIDENCE:
            payload = json.loads(proposal)
            try:
                # Qwen is primary. Material proposals receive independent
                # Claude and Codex reviews before the council sees them.
                proposal_text = str(payload.get("text", ""))
                brain_ids = []
                try:
                    bdb = brain.connect()
                    try:
                        brain_ids = [int(row["id"]) for row in brain.recall(bdb, question, k=8)]
                    finally:
                        bdb.close()
                except (OSError, sqlite3.Error):
                    brain_ids = []
                for validator_name in ("claude", "codex", "gemini"):
                    validation.request(db, lambda prompt, name=validator_name: complete(
                        prompt, order=[name], now_ms=now_ms, state_path=state_path,
                        system=agents.system_prompt("reviewer"), system_local=agents.compact_prompt("reviewer"), expect="json").text,
                        subject=f"research:{tid}", primary_backend=getattr(result, "backend", "claude"), validator=validator_name,
                        proposal=proposal_text, evidence=result, brain_ids=brain_ids, board_ids=[board_id], now_ms=now_ms)
                rules.validate(db, str(payload.get("agent", "")), str(payload.get("kind", "")), str(payload.get("text", "")), str(payload.get("evidence", "")), str(payload.get("param", "")),
                               payload.get("min"), payload.get("max"), now_ms, f"researcher_{focus}")
                did = council.open_decision(db, "commander", f"Research #{tid}: {str(payload.get('text', ''))[:60]}", f"{payload.get('text', '')} Evidence: {payload.get('evidence', '')}",
                                            goal=goal, payload={**payload, "op": "add_rule"}, now_ms=now_ms)
                with db:
                    db.execute("UPDATE research_tasks SET decision_id=? WHERE id=?", (did, tid))
                opened.append(did)
            except (ValueError, TypeError):
                continue
    return {"made": made, "ran": len(tasks), "succeeded": sum(1 for o in outcomes if o), "decisions": opened}


def tick(db: sqlite3.Connection, complete, now_ms: int, workers: int, last_ms: int, state_path=None) -> tuple[int, dict | None]:
    """Runs a round when it is due and the machine allows at least one worker; returns the new 'last round' time and the round's summary."""
    if workers <= 0 or now_ms - last_ms < EVERY_MS:
        return last_ms, None
    return now_ms, run_round(db, complete, workers, now_ms, state_path=state_path)

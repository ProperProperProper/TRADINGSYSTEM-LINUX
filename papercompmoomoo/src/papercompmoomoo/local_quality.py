"""The local model corrects its mistakes and learns from them (owner order 2026-10-21: "dont ban anything: correct it, or learn from it").

Nothing is ever banned. When an answer is garbage the program first CORRECTS it, and records which correction worked:
1. repair (``repair_json``): strip code fences and the talk around a JSON answer, drop trailing commas, cut to the outermost object or list;
2. re-ask (``corrective_prompt``): the same question with what was wrong with the last answer, asked once more;
3. the next backend (Claude or Codex) when the local answer still cannot be used.
The order of the first two follows what has worked best for that kind of mistake before (``remedy_order``).
It also LEARNS. Every raw answer is recorded against the acceleration settings that were running, as LEVELS: prediction 0/1, the Neural Engine prefill 0-3 (how many layers it covers), dual Neural Engine 0/1.
When a setting is followed by too many bad answers (three in a row, or at least 20% of at least ten) it is not removed: it is turned DOWN one level, the level that failed is remembered as a ceiling, and
the setting is tried at the higher level again after a cool-down that doubles with each failure (1 h, 2 h, 4 h ... at most a day) and only once the answers have been good for a while; a retry that fails
goes straight back down and waits twice as long. What was learned is written into the shared brain as a lesson, so every agent can see it.
"""
from __future__ import annotations

import json
import re
import sqlite3
import time
from pathlib import Path

STATE_FILE = Path("data/omlx_state.json")
CONFIG_FILE = Path("data/omlx_config.json")
WINDOW = 20
BAD_RATE = 0.20
BAD_STREAK = 3
MIN_CALLS = 3  # three bad in a row is enough to act
MIN_RATE_CALLS = 10  # a RATE needs at least this many answers: one empty answer in five is not evidence
LEVELS = {"mtp": 1, "ane": 3, "ane_dual": 1}  # the highest level of each setting
ANE_LAYERS = {0: 0, 1: 16, 2: 32, 3: 64}  # layers the Neural Engine prefill covers at each level
COOLDOWN_BASE_MS = 3600 * 1000
COOLDOWN_MAX_MS = 24 * 3600 * 1000
GOOD_BEFORE_RETRY = 20  # good answers since the last bad one before a turned-down setting is tried again
RISK_ORDER = ("ane_dual", "ane", "mtp")  # turned down in this order: the riskiest first
REMEDIES = ("repair", "reask")

SCHEMA = """
CREATE TABLE IF NOT EXISTS local_calls(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, config_key TEXT NOT NULL, ok INTEGER NOT NULL, reason TEXT NOT NULL, sample TEXT NOT NULL DEFAULT '');
CREATE TABLE IF NOT EXISTS local_remedies(id INTEGER PRIMARY KEY AUTOINCREMENT, ts_ms INTEGER NOT NULL, reason TEXT NOT NULL, remedy TEXT NOT NULL, success INTEGER NOT NULL);
"""


def ensure(db: sqlite3.Connection) -> None:
    db.executescript(SCHEMA)


def is_garbage(text: str, expect_json: bool = False) -> str | None:
    """Why an answer is garbage, or None when it looks like an answer."""
    t = (text or "").strip()
    if len(t) < 2:
        return "empty"
    printable = sum(1 for ch in t if ch.isprintable() or ch in "\n\t")
    if printable / len(t) < 0.95:
        return "unprintable characters"
    words = re.findall(r"\S+", t)
    if len(words) >= 12:
        top = max((words.count(w) for w in set(words)), default=0)
        if top / len(words) > 0.5 and len(set(words)) <= 4:
            return "one or two words repeated"
    lines = [ln.strip() for ln in t.splitlines() if ln.strip()]
    if len(lines) >= 6 and len(set(lines)) <= 2:
        return "the same line repeated"
    if re.search(r"(.{3,12})\1{6,}", t):
        return "a short pattern repeated"
    if expect_json:
        match = re.search(r"[\[{].*[\]}]", t, re.S)
        if not match:
            return "no JSON"
        try:
            json.loads(match.group(0))
        except ValueError:
            return "JSON that does not parse"
    return None


# ---- correcting an answer ---------------------------------------------------------------------------------------------------------------------------
def repair_json(text: str) -> str | None:
    """A JSON text recovered from an answer that has talk around it, code fences or trailing commas; None when nothing parseable can be found."""
    t = re.sub(r"```(?:json)?", "", text or "").strip()
    for opener, closer in (("{", "}"), ("[", "]")):
        start, end = t.find(opener), t.rfind(closer)
        if start != -1 and end > start:
            candidate = re.sub(r",\s*([}\]])", r"\1", t[start:end + 1])
            try:
                json.loads(candidate)
                return candidate
            except ValueError:
                continue
    return None


def corrective_prompt(prompt: str, reason: str) -> str:
    """The same question again, with what was wrong with the last answer."""
    return (f"{prompt}\n\nYour previous answer could not be used ({reason}). Answer again: concise, complete, and exactly in the format asked"
            + (" (ONLY the JSON, no other text)" if "JSON" in reason else "") + ".")


def record_remedy(db: sqlite3.Connection, reason: str, remedy: str, success: bool, now_ms: int | None = None) -> None:
    ensure(db)
    with db:
        db.execute("INSERT INTO local_remedies(ts_ms, reason, remedy, success) VALUES(?,?,?,?)", (now_ms if now_ms is not None else int(time.time() * 1000), reason, remedy, int(success)))


def remedy_order(db: sqlite3.Connection, reason: str) -> list[str]:
    """The corrections to try, the one that has fixed this kind of mistake most often first (an untried one counts as promising)."""
    ensure(db)
    rates = {}
    for remedy in REMEDIES:
        n, ok = db.execute("SELECT count(*), coalesce(sum(success), 0) FROM local_remedies WHERE reason=? AND remedy=?", (reason, remedy)).fetchone()
        rates[remedy] = (ok + 1) / (n + 2)  # a small prior: no evidence yet means 50%
    return sorted(REMEDIES, key=lambda r: (-rates[r], REMEDIES.index(r)))


def correct(db: sqlite3.Connection | None, text: str, expect_json: bool, ask, prompt: str) -> tuple[str | None, str | None, str | None]:
    """Tries to turn a bad answer into a good one. Returns (the good text or None, the first reason it was bad, the remedy that worked). ``ask`` re-asks the model."""
    reason = is_garbage(text, expect_json)
    if reason is None:
        return text, None, None
    order = remedy_order(db, reason) if db is not None else list(REMEDIES)
    for remedy in order:
        fixed = None
        if remedy == "repair" and expect_json:
            candidate = repair_json(text)
            fixed = candidate if candidate and not is_garbage(candidate, True) else None
        elif remedy == "reask":
            try:
                again = ask(corrective_prompt(prompt, reason))
            except Exception:  # noqa: BLE001 - the server could not answer: the next remedy or backend
                again = ""
            fixed = again if not is_garbage(again, expect_json) else None
        if db is not None and not (remedy == "repair" and not expect_json):
            record_remedy(db, reason, remedy, fixed is not None)
        if fixed is not None:
            return fixed, reason, remedy
    return None, reason, None


# ---- learning from it ---------------------------------------------------------------------------------------------------------------------------
def load_state(path: Path | None = None) -> dict:
    try:
        data = json.loads((path or STATE_FILE).read_text())
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_state(data: dict, path: Path | None = None) -> None:
    path = path or STATE_FILE
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(data, indent=1))
    tmp.replace(path)


def levels_of(features: dict) -> dict:
    """The settings as levels: prediction 0/1, the Neural Engine 0-3 (a bool counts as the highest level), dual 0/1."""
    ane = features.get("ane", 0)
    return {"mtp": int(bool(features.get("mtp"))), "ane": (LEVELS["ane"] if ane else 0) if isinstance(ane, bool) else max(0, min(LEVELS["ane"], int(ane))), "ane_dual": int(bool(features.get("ane_dual")))}


def config_key(features: dict) -> str:
    lv = levels_of(features)
    return f"mtp={lv['mtp']},ane={lv['ane']},dual={lv['ane_dual']}"


def current_features(path: Path | None = None) -> dict:
    try:
        return json.loads((path or CONFIG_FILE).read_text()).get("features", {})
    except (OSError, ValueError):
        return {}


def record(db: sqlite3.Connection, features: dict, text: str, expect_json: bool, now_ms: int | None = None) -> str | None:
    """Checks one RAW answer and records the verdict against the running configuration; returns the reason when it was garbage."""
    ensure(db)
    reason = is_garbage(text, expect_json)
    with db:
        db.execute("INSERT INTO local_calls(ts_ms, config_key, ok, reason, sample) VALUES(?,?,?,?,?)",
                   (now_ms if now_ms is not None else int(time.time() * 1000), config_key(features), 0 if reason else 1, reason or "", (text or "")[:160] if reason else ""))
    return reason


def applied_ms(path: Path | None = None) -> int:
    """When the running settings were applied: answers given before that belong to an earlier run of the same configuration and are not counted against it."""
    try:
        return int(json.loads((path or CONFIG_FILE).read_text()).get("applied_ms", 0))
    except (OSError, ValueError, TypeError):
        return 0


def verdict(db: sqlite3.Connection, features: dict, since_ms: int = 0) -> dict | None:
    """Whether the running configuration has earned a turn-down: its latest answers, newest first, three bad in a row or at least 20% of at least ten. None when it is fine (or too soon to say)."""
    ensure(db)
    rows = db.execute("SELECT ok, reason FROM local_calls WHERE config_key=? AND ts_ms>=? ORDER BY id DESC LIMIT ?", (config_key(features), since_ms, WINDOW)).fetchall()
    if len(rows) < MIN_CALLS:
        return None
    streak = 0
    for ok, _ in rows:
        if ok:
            break
        streak += 1
    bad = sum(1 for ok, _ in rows if not ok)
    if streak >= BAD_STREAK or (len(rows) >= MIN_RATE_CALLS and bad / len(rows) >= BAD_RATE):
        return {"bad": bad, "of": len(rows), "streak": streak, "reasons": sorted({r for ok, r in rows if not ok})}
    return None


def good_since_failure(db: sqlite3.Connection, features: dict, since_ms: int = 0) -> int:
    """How many good answers the running configuration has given since its last bad one."""
    ensure(db)
    n = 0
    for (ok,) in db.execute("SELECT ok FROM local_calls WHERE config_key=? AND ts_ms>=? ORDER BY id DESC", (config_key(features), since_ms)):
        if not ok:
            break
        n += 1
    return n


def adapt(db: sqlite3.Connection, features: dict, state: dict, now_ms: int | None = None, since_ms: int = 0) -> tuple[dict, dict | None]:
    """The learning step. ``features`` are the running settings, ``state`` the memory of past failures (per setting: ``ceiling`` the level that failed, ``failures``, ``retry_after_ms``).
    Returns the (possibly changed) levels and what was done: ``{"action": "down"|"up"|"none-left", ...}`` or None. Never bans: a turned-down setting is tried again after its cool-down."""
    now = now_ms if now_ms is not None else int(time.time() * 1000)
    levels = levels_of(features)
    found = verdict(db, features, since_ms)
    if found:
        for name in RISK_ORDER:
            if levels[name] > 0:
                entry = state.setdefault(name, {"failures": 0})
                entry["ceiling"] = levels[name]
                entry["failures"] = int(entry.get("failures", 0)) + 1
                entry["retry_after_ms"] = now + min(COOLDOWN_MAX_MS, COOLDOWN_BASE_MS * 2 ** (entry["failures"] - 1))
                entry["last_evidence"] = found
                down = {**levels, name: levels[name] - 1}
                if name == "ane" and down["ane"] == 0:
                    down["ane_dual"] = 0
                return down, {"action": "down", "setting": name, "level": down[name], "failed_level": levels[name], "evidence": found, "retry_after_ms": entry["retry_after_ms"]}
        return levels, {"action": "none-left", "evidence": found}
    for name in ("mtp", "ane", "ane_dual"):
        entry = state.get(name)
        if entry and entry.get("ceiling") and levels[name] < entry["ceiling"] and now >= entry.get("retry_after_ms", 0) and good_since_failure(db, features, since_ms) >= GOOD_BEFORE_RETRY:
            up = {**levels, name: levels[name] + 1}
            return up, {"action": "up", "setting": name, "level": up[name], "failed_level": entry["ceiling"]}
    return levels, None


def start_levels(desired: dict, state: dict, now_ms: int) -> dict:
    """The levels to start the server with: what was asked for, held below a level that failed while its cool-down is still running."""
    levels = levels_of(desired)
    for name, entry in state.items():
        if name in levels and entry.get("ceiling") and now_ms < entry.get("retry_after_ms", 0):
            levels[name] = min(levels[name], entry["ceiling"] - 1)
    if levels["ane"] == 0:
        levels["ane_dual"] = 0
    return levels


def lesson_text(action: dict, key: str) -> str:
    """What the adaptation taught, as a sentence for the shared brain."""
    ev = action.get("evidence") or {}
    if action["action"] == "down":
        return (f"With the local model's settings {key}, {ev.get('bad')} of its last {ev.get('of')} answers were bad ({', '.join(ev.get('reasons', []))}); {action['setting']} was turned down from level "
                f"{action['failed_level']} to {action['level']} and will be tried at {action['failed_level']} again after its cool-down, only if the answers stay good.")
    if action["action"] == "up":
        return f"After a cool-down and a run of good answers, {action['setting']} on the local model was tried again at level {action['level']} (it had failed at {action['failed_level']})."
    return f"The local model gave bad answers ({', '.join(ev.get('reasons', []))}) with every acceleration already off: look at the model itself."


PROBES = (
    ('Reply with ONLY this JSON, filling in the reason: {"stance": "support", "reason": "..."}', True),
    ("In two short sentences: what does a trailing stop do?", False),
    ('Reply with ONLY a JSON list of two numbers between 1 and 5: [a, b]', True),
)


def wait_until_answering(complete, attempts: int = 24, pause_s: float = 5.0, sleep=time.sleep) -> bool:
    """After a settings change the server reloads the model and answers "conflict" or nothing for a while: wait until a trivial prompt gets an answer (up to attempts x pause_s)."""
    for _ in range(attempts):
        try:
            if (complete("Reply with the word ready.") or "").strip():
                return True
        except Exception:  # noqa: BLE001 - not up yet
            pass
        sleep(pause_s)
    return False


def probe(complete, db: sqlite3.Connection, features: dict, now_ms: int | None = None) -> int:
    """Runs the canned prompts through the local model and records the raw verdicts; returns how many were bad. A prompt that gets no answer at all (the server busy or reloading) is an error of the
    server, not a mistake of the configuration: it is not counted."""
    bad = 0
    for prompt, expect_json in PROBES:
        try:
            text = complete(prompt)
        except Exception:  # noqa: BLE001
            continue
        if record(db, features, text, expect_json, now_ms):
            bad += 1
    return bad

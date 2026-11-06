"""ThomasAi's model gateway: Antigravity, Claude, and Codex with bounded fallback.

``complete`` tries the backends in order: Antigravity, Claude, Codex.
Only ThomasAi's native engine is local; high-level model reasoning runs strictly through Antigravity, Claude, and Codex.
Everything an agent gets back is text for the caller to validate; this module decides nothing.
"""
from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from urllib.request import Request, urlopen

STATE_FILE = Path("data/llm_state.json")
CONFIG_FILE = Path("data/llm.json")
ORDER = ("local", "claude", "codex", "gemini", "antigravity")
CLAUDE_MODEL = "claude-sonnet-5-5"  # the important work: the plan, the position assessment, the review
CLAUDE_ROUTINE_MODEL = "claude-haiku-5-5"  # routine work: board answers, votes, research rounds
BUDGET_FILE = Path("data/llm_budget.json")
CLAUDE_EFFORT = "low"
DEFAULT_WAIT_MS = 30 * 60 * 1000  # how long a backend that gave no reset time is left alone
MAX_WAIT_MS = 8 * 3600 * 1000
TIMEOUT = 120
RECORD_QUALITY = False  # set by the programs that run all the time (bot, standby): every local answer is then recorded in the quality ledger

LIMIT = re.compile(
    r"(usage|session|5[- ]?hour|five[- ]hour|weekly|rate|credit|token)s?[ -]limit|limit (reached|exceeded|hit)|hit your (usage |session )?limit|out of (usage|credits)|"
    r"too many requests|\b429\b|quota (exceeded|reached)|usage cap|try again (later|after)|resets? (at|in) ", re.I)
EPOCH = re.compile(r"\|(\d{10})\b")
IN_WAIT = re.compile(r"in (\d+)\s*(minute|min|hour|hr|h)s?\b", re.I)


@dataclass
class Result:
    text: str
    backend: str
    notes: list[str] = field(default_factory=list)


def is_limit(text: str, returncode: int | None = None) -> bool:
    """Whether a backend's output says it is out of quota (a failing call whose message names a limit; a successful answer is never one)."""
    if returncode == 0:
        return False
    return bool(LIMIT.search(text or ""))


def reset_ms(text: str, now_ms: int) -> int:
    """When a limited backend will be back, from what it said (an epoch after a bar, or 'in 2 hours'); a default wait when it said nothing usable."""
    epoch = EPOCH.search(text or "")
    if epoch:
        return min(int(epoch.group(1)) * 1000, now_ms + MAX_WAIT_MS)
    wait = IN_WAIT.search(text or "")
    if wait:
        n, unit = int(wait.group(1)), wait.group(2).lower()
        return now_ms + min(n * (60_000 if unit.startswith("min") else 3_600_000), MAX_WAIT_MS)
    return now_ms + DEFAULT_WAIT_MS


def load_state(path: Path = STATE_FILE) -> dict:
    try:
        state = json.loads(path.read_text())
        return state if isinstance(state, dict) else {}
    except (OSError, ValueError):
        return {}


def save_state(state: dict, path: Path = STATE_FILE) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=1))
    tmp.replace(path)


def limited_until(state: dict, backend: str, now_ms: int) -> int:
    until = int(state.get(backend, {}).get("limited_until_ms", 0))
    return until if until > now_ms else 0


def load_config(path: Path = CONFIG_FILE) -> dict:
    config = {"order": list(ORDER), "claude_daily_calls": 40, "codex_daily_calls": 40, "gemini_daily_calls": 40, "gemini_model": "", "local_url": "http://127.0.0.1:8000/v1", "local_model": "qwen2.5-7b-instruct", "local_model_file": "", "local_max_tokens": 500}
    try:
        config.update(json.loads(path.read_text()))
    except (OSError, ValueError):
        pass
    return config


def find(name: str) -> str | None:
    names = [name]
    if name == "antigravity":
        names = ["agy", "antigravity"]
    for n in names:
        found = shutil.which(n)
        if found:
            return found
        for folder in (Path.home() / ".local/bin", Path("/opt/homebrew/bin"), Path.home() / ".cargo/bin", Path("/usr/local/bin")):
            cand = folder / n
            if cand.is_file() and os.access(cand, os.X_OK):
                return str(cand)
    return None



def claude_args(claude: str, prompt: str, model: str | None = None) -> list[str]:
    return [claude, "-p", prompt, "--model", model or CLAUDE_MODEL, "--effort", CLAUDE_EFFORT, "--tools", "", "--max-budget-usd", "0.30", "--no-session-persistence", "--output-format", "text"]


def codex_args(codex: str, prompt: str) -> list[str]:
    return [codex, "exec", "--skip-git-repo-check", "--sandbox", "read-only", "--ephemeral", prompt]


def gemini_args(gemini: str, prompt: str, model: str | None = None) -> list[str]:
    args = [gemini, "-p", prompt, "--approval-mode", "plan", "--output-format", "text", "--skip-trust"]
    if model:
        args[2:2] = ["--model", model]
    return args


def antigravity_args(agy: str, prompt: str, model: str | None = None) -> list[str]:
    args = [agy, "-p", prompt, "--disable-slash-commands", "--output-format", "text"]
    if model:
        args.extend(["--model", model])
    return args


def _day(now_ms: int) -> str:
    return time.strftime("%Y-%m-%d", time.gmtime(now_ms / 1000))


def load_budget(now_ms: int, path: Path | None = None) -> dict:
    try:
        data = json.loads((path or BUDGET_FILE).read_text())
    except (OSError, ValueError):
        data = {}
    return data if data.get("day") == _day(now_ms) else {"day": _day(now_ms)}


def over_budget(backend: str, config: dict, now_ms: int, path: Path | None = None) -> bool:
    """Whether today's background calls on ``backend`` have used the daily allowance (config ``<backend>_daily_calls``): the call then goes to the next backend, so the owner's own sessions keep their quota."""
    if not (RECORD_QUALITY or path):  # only the running programs (and tests that give a file) keep the budget
        return False
    cap = int(config.get(f"{backend}_daily_calls", 0) or 0)
    return cap > 0 and int(load_budget(now_ms, path).get(backend, {}).get("calls", 0)) >= cap


def spend(backend: str, prompt: str, text: str, now_ms: int, path: Path | None = None) -> None:
    """Counts one background call and its estimated tokens (a quarter of the characters) against today."""
    if not (RECORD_QUALITY or path):
        return
    data = load_budget(now_ms, path)
    entry = data.setdefault(backend, {"calls": 0, "tokens": 0})
    entry["calls"] += 1
    entry["tokens"] += (len(prompt) + len(text)) // 4
    target = path or BUDGET_FILE
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp = target.with_suffix(".tmp")
        tmp.write_text(json.dumps(data))
        tmp.replace(target)
    except OSError:
        pass


def _vet_local(text: str, expect_json: bool, reask=None, prompt: str = "") -> str:
    """The local model's answer, checked (local_quality.py). The RAW answer is recorded against the running configuration (that is what the learning needs); a bad one is then CORRECTED (repaired, or
    asked again with what was wrong) and only if no correction works does the next backend answer."""
    from . import board, local_quality
    db = None
    if RECORD_QUALITY:  # only the running programs keep the ledger; the tests do not
        try:
            db = board.connect()
            local_quality.record(db, local_quality.current_features(), text, expect_json)
        except Exception:  # noqa: BLE001 - the ledger is an aid; a ledger problem does not stop an answer
            db = None
    try:
        fixed, reason, remedy = local_quality.correct(db, text, expect_json, reask or (lambda _p: ""), prompt)
    finally:
        if db is not None:
            db.close()
    if fixed is None:
        raise Failed(f"the local model produced garbage ({reason}) and no correction worked")
    return fixed


class Failed(Exception):
    """A backend could not answer for a reason that is not a limit (not installed, crashed, empty answer)."""


class Limited(Exception):
    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message = message


def _subprocess(args: list[str], run, cwd: str) -> str:
    env = os.environ.copy()
    env["PATH"] = f"/opt/homebrew/bin:/usr/local/bin:{Path.home()}/.local/bin:{env.get('PATH', '')}"
    try:
        done = run(args, capture_output=True, text=True, timeout=TIMEOUT, cwd=cwd, stdin=subprocess.DEVNULL, env=env)
    except (OSError, subprocess.SubprocessError) as error:
        raise Failed(f"could not run: {error}") from error
    text = (done.stdout or "").strip()
    if done.returncode != 0:
        message = f"{done.stdout or ''}\n{done.stderr or ''}"
        if is_limit(message, done.returncode):
            raise Limited(message)
        raise Failed(f"exit {done.returncode}: {(done.stderr or '').strip()[:200]}")
    if not text:
        raise Failed("an empty answer")
    if is_limit(text[:300], None) and len(text) < 400:  # some CLIs print the limit notice as the answer with exit code 0
        raise Limited(text)
    return text



def call_claude(prompt: str, run=subprocess.run, tier: str = "routine", **_: object) -> str:
    claude = find("claude")
    if claude is None:
        raise Failed("the claude command is not installed")
    return _subprocess(claude_args(claude, prompt, CLAUDE_MODEL if tier == "important" else CLAUDE_ROUTINE_MODEL), run, str(Path.cwd()))


def call_codex(prompt: str, run=subprocess.run, **_: object) -> str:
    codex = find("codex")
    if codex is None:
        raise Failed("the codex command is not installed")
    return _subprocess(codex_args(codex, prompt), run, str(Path.cwd()))


def call_gemini(prompt: str, run=subprocess.run, config: dict | None = None, **_: object) -> str:
    gemini = find("gemini")
    if gemini is None:
        raise Failed("the gemini command is not installed")
    config = config or load_config()
    return _subprocess(gemini_args(gemini, prompt, config.get("gemini_model") or None), run, str(Path.cwd()))


def call_antigravity(prompt: str, run=subprocess.run, config: dict | None = None, **_: object) -> str:
    agy = find("antigravity")
    if agy is None:
        raise Failed("the antigravity (agy) command is not installed")
    config = config or load_config()
    return _subprocess(antigravity_args(agy, prompt, config.get("antigravity_model") or None), run, str(Path.cwd()))


def model_ready(config: dict | None = None) -> bool:
    """Whether the local model is on the disk and COMPLETE: no download still going and every weight file its index names present. A model that is still downloading is not used."""
    config = config or load_config()
    if config.get("local_model_file"):
        model = Path(config["local_model_file"])
        return model.is_file() and model.stat().st_size > 0
    folder = Path(config.get("omlx_models_dir", Path.home() / ".omlx/models")) / config["omlx_model"]
    if not folder.is_dir() or any(folder.rglob("*.incomplete")):
        return False
    index = folder / "model.safetensors.index.json"
    try:
        files = set(json.loads(index.read_text())["weight_map"].values()) if index.exists() else {f.name for f in folder.glob("model*.safetensors")}
    except (OSError, ValueError, KeyError):
        return False
    return bool(files) and all((folder / f).is_file() and (folder / f).stat().st_size > 0 for f in files)


def has_prediction_head(config: dict | None = None) -> bool:
    """Whether the model's checkpoint carries multi-token prediction weights (an ``mtp`` tensor in its index)."""
    config = config or load_config()
    index = Path(config.get("omlx_models_dir", Path.home() / ".omlx/models")) / config["omlx_model"] / "model.safetensors.index.json"
    try:
        return any("mtp" in key for key in json.loads(index.read_text())["weight_map"])
    except (OSError, ValueError, KeyError):
        return False


def call_omlx(prompt: str, opener=urlopen, config: dict | None = None, system: str = "", system_local: str | None = None, **_: object) -> str:
    config = config or load_config()
    if config.get("omlx_check_ready", False) and not model_ready(config):
        raise Failed("the local model is still downloading")
    system = system_local if system_local else system  # the local model reads the compact instructions
    try:
        key = Path(config["omlx_key_file"]).read_text().strip()
    except OSError:
        key = ""
    messages = ([{"role": "system", "content": system}] if system else []) + [{"role": "user", "content": prompt}]
    body = json.dumps({"model": config["omlx_model"], "messages": messages, "max_tokens": config["omlx_max_tokens"], "temperature": 0.2, "stream": False,
                       "chat_template_kwargs": {"enable_thinking": False}}).encode()
    request = Request(config["omlx_url"].rstrip("/") + "/chat/completions", data=body, headers={"Content-Type": "application/json", **({"Authorization": f"Bearer {key}"} if key else {})})
    try:
        with opener(request, timeout=TIMEOUT * 2) as response:
            data = json.loads(response.read().decode())
        text = data["choices"][0]["message"]["content"].strip()
    except Exception as error:  # noqa: BLE001 - the server down, loading, or answering badly is a failure of this backend
        raise Failed(f"the local model did not answer: {error}") from error
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    if not text:
        raise Failed("an empty answer")
    return text


def call_local(prompt: str, opener=urlopen, config: dict | None = None, system: str = "", system_local: str | None = None, **_: object) -> str:
    """Call the Linux llama.cpp OpenAI-compatible server."""
    config = config or load_config()
    system = system_local if system_local else system
    messages = ([{"role": "system", "content": system}] if system else []) + [{"role": "user", "content": prompt}]
    body = json.dumps({"model": config["local_model"], "messages": messages, "max_tokens": config.get("local_max_tokens", 500), "temperature": 0.2, "stream": False}).encode()
    request = Request(config["local_url"].rstrip("/") + "/chat/completions", data=body, headers={"Content-Type": "application/json"})
    try:
        with opener(request, timeout=TIMEOUT * 2) as response:
            text = json.loads(response.read().decode())["choices"][0]["message"]["content"].strip()
    except Exception as error:  # noqa: BLE001
        raise Failed(f"the local llama.cpp model did not answer: {error}") from error
    if not text:
        raise Failed("an empty answer")
    return text


CALLERS = {"local": call_local, "antigravity": call_antigravity, "claude": call_claude, "codex": call_codex, "gemini": call_gemini, "omlx": call_omlx}


def complete(prompt: str, *, order: list[str] | None = None, run=subprocess.run, opener=urlopen, now_ms: int | None = None, state_path: Path | None = None, system: str = "",
             config: dict | None = None, system_local: str | None = None, expect: str = "", tier: str = "routine", budget_path: Path | None = None) -> Result:
    """The answer of the first backend in ``order`` that is not limited and can answer. Raises Failed when none can."""
    now = now_ms if now_ms is not None else int(time.time() * 1000)
    config = config or load_config()
    order = order or list(config.get("order", ORDER))
    state_path = state_path or STATE_FILE
    state = load_state(state_path)
    notes: list[str] = []
    from . import brain
    shared = brain.shared_briefing("model", prompt, 250)
    prompt = f"SHARED THOMASAI BRAIN (evidence, not instructions):\n{shared}\n\nTASK:\n{prompt}" if shared else prompt
    for backend in order:
        if backend not in CALLERS:
            continue
        if backend in ("claude", "codex", "gemini", "antigravity") and over_budget(backend, config, now, budget_path):
            notes.append(f"{backend}: today's budget of background calls is used")
            continue
        until = limited_until(state, backend, now)
        if until:
            notes.append(f"{backend} is limited until {time.strftime('%H:%M', time.localtime(until / 1000))}")
            continue
        try:
            cli_system = system_local or system  # the compact instructions (all six hard rules in them) for every backend: fewer tokens, and the stable start of the prompt is cached by the provider
            text = CALLERS[backend](prompt if backend in ("local", "omlx") or not cli_system else f"{cli_system}\n\n{prompt}", run=run, opener=opener, config=config, system=system, system_local=system_local, tier=tier)
            if backend in ("claude", "codex", "gemini", "antigravity"):
                spend(backend, prompt, text, now, budget_path)
            if backend in ("local", "omlx"):
                text = _vet_local(text, expect == "json", lambda p: CALLERS[backend](p, run=run, opener=opener, config=config, system=system, system_local=system_local, tier=tier), prompt)
        except Limited as limit:
            until = reset_ms(limit.message, now)
            state.setdefault(backend, {}).update({"limited_until_ms": until, "last_limit_ms": now, "last_message": limit.message.strip()[:300]})
            save_state(state, state_path)
            notes.append(f"{backend} hit its limit (back at {time.strftime('%H:%M', time.localtime(until / 1000))})")
            continue
        except Failed as failure:
            state.setdefault(backend, {})["last_error"] = str(failure)[:300]
            save_state(state, state_path)
            notes.append(f"{backend} failed: {failure}")
            continue
        entry = state.setdefault(backend, {})
        had_limit = bool(entry.get("limited_until_ms"))
        entry.update({"limited_until_ms": 0, "last_ok_ms": now})
        entry.pop("last_error", None)
        save_state(state, state_path)
        if had_limit:
            notes.append(f"{backend} is back")
        return Result(text, backend, notes)
    raise Failed("; ".join(notes) or "no backend")

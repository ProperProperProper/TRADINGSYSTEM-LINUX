"""How many local-model agents this machine can carry (owner order 2026-10-21: as many as it tolerates without severe performance problems).

The local oMLX server keeps ONE copy of the model in memory and serves several requests at once, so an extra agent costs cache memory and a share of the GPU, not another model. The number allowed
is decided from what the machine has NOW, every time work is handed out:
* free memory (the same measure as ``thomas headroom``): below ``PAUSE_BELOW_FREE_PCT`` no research worker runs at all (the owner's rule keeps at least 16% free; this stops earlier);
  above it, one more worker for every ``pct_per_worker`` percent of free memory above that line, up to the benchmarked ceiling;
* idle CPU: below ``MIN_CPU_IDLE_PCT`` no worker runs;
* the benchmarked ceiling ``max_workers`` (``data/omlx_capacity.json``, written by ``python -m papercompmoomoo.capacity bench`` when the server is up: the highest number of simultaneous requests
  whose total speed still rose and whose answers stayed fast), never above ``HARD_MAX_WORKERS``.
The commander and the lead research agent always get a seat of their own (they are not counted among the workers).
"""
from __future__ import annotations

import json
import re
import subprocess
from pathlib import Path

PAUSE_BELOW_FREE_PCT = 22.0
MIN_CPU_IDLE_PCT = 25.0
OPEN_SESSION_PAUSE_BELOW_FREE_PCT = 30.0
OPEN_SESSION_MIN_CPU_IDLE_PCT = 45.0
OPEN_SESSION_MAX_WORKERS = 1
HARD_MAX_WORKERS = 6
CAPACITY_FILE = Path("data/omlx_capacity.json")
DEFAULTS = {"max_workers": 2, "pct_per_worker": 4.0}


def load_config(path: Path | None = None) -> dict:
    config = dict(DEFAULTS)
    try:
        config.update(json.loads((path or CAPACITY_FILE).read_text()))
    except (OSError, ValueError):
        pass
    return config


def parse_meminfo(text: str) -> float | None:
    """``/proc/meminfo`` -> ``MemAvailable / MemTotal`` in percent (Linux)."""
    vals = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] in ("MemTotal:", "MemAvailable:"):
            try:
                vals[parts[0]] = float(parts[1])
            except ValueError:
                return None
    total, avail = vals.get("MemTotal:", 0.0), vals.get("MemAvailable:", -1.0)
    return 100.0 * avail / total if total > 0.0 and 0.0 <= avail <= total else None


def parse_proc_stat(text: str) -> tuple[int, int] | None:
    """First ``cpu `` line of ``/proc/stat`` -> ``(idle + iowait, total)`` jiffies."""
    for line in text.splitlines():
        if line.startswith("cpu "):
            try:
                nums = [int(x) for x in line.split()[1:]]
            except ValueError:
                return None
            if len(nums) >= 4:
                return nums[3] + (nums[4] if len(nums) > 4 else 0), sum(nums)
    return None


def cpu_idle_between(a: tuple[int, int] | None, b: tuple[int, int] | None) -> float | None:
    if not a or not b:
        return None
    di, dt = b[0] - a[0], b[1] - a[1]
    return 100.0 * di / dt if dt > 0 and 0 <= di <= dt else None


def memory_free_pct(runner=subprocess.run, meminfo: Path = Path("/proc/meminfo")) -> float | None:
    """Free memory as a percentage of the total, the way the system itself reports it (``memory_pressure`` on macOS, ``/proc/meminfo`` on Linux: the measure ``thomas headroom`` uses); from ``vm_stat``
    (free, inactive and speculative pages) when neither is available."""
    try:
        said = runner(["memory_pressure"], capture_output=True, text=True, timeout=20).stdout
        found = re.search(r"free percentage:\s*([\d.]+)%", said)
        if found:
            return float(found.group(1))
    except (OSError, subprocess.SubprocessError):
        pass
    try:
        pct = parse_meminfo(meminfo.read_text())
        if pct is not None:
            return pct
    except OSError:
        pass
    try:
        out = runner(["vm_stat"], capture_output=True, text=True, timeout=10).stdout
        total = int(runner(["sysctl", "-n", "hw.memsize"], capture_output=True, text=True, timeout=10).stdout.strip())
    except (OSError, ValueError, subprocess.SubprocessError):
        return None
    page = int((re.search(r"page size of (\d+) bytes", out) or [0, 16384])[1])
    pages = sum(int((re.search(rf"Pages {name}:\s+(\d+)", out) or [0, 0])[1]) for name in ("free", "inactive", "speculative"))
    return 100.0 * pages * page / total if total else None


def cpu_idle_pct(runner=subprocess.run, stat: Path = Path("/proc/stat"), pause: float = 0.2) -> float | None:
    try:
        out = runner(["top", "-l", "1", "-n", "0"], capture_output=True, text=True, timeout=20).stdout
        found = re.search(r"([\d.]+)% idle", out)
        if found:
            return float(found.group(1))
    except (OSError, subprocess.SubprocessError):
        pass
    try:
        import time
        a = parse_proc_stat(stat.read_text())
        if a:
            time.sleep(pause)
            return cpu_idle_between(a, parse_proc_stat(stat.read_text()))
    except OSError:
        pass
    return None


def allowed_workers(free_pct: float | None, idle_pct: float | None, config: dict | None = None) -> int:
    """How many research workers may run right now (0 when the machine cannot spare them or cannot be measured)."""
    config = config or load_config()
    if free_pct is None or idle_pct is None:
        return 0
    if free_pct < PAUSE_BELOW_FREE_PCT or idle_pct < MIN_CPU_IDLE_PCT:
        return 0
    extra = int((free_pct - PAUSE_BELOW_FREE_PCT) // max(0.5, float(config["pct_per_worker"])))
    return max(1, min(int(config["max_workers"]), HARD_MAX_WORKERS, 1 + extra))


def now_allowed(config: dict | None = None, runner=subprocess.run, open_session: bool | None = None) -> int:
    """During open trading sessions, QoS gives strict priority to live trading functions and features:
    research workers are paused unless >=30% RAM is free and >=45% CPU is idle, and capped at 1 worker."""
    from . import darwin_qos
    free = memory_free_pct(runner)
    idle = cpu_idle_pct(runner)
    base = allowed_workers(free, idle, config)
    if base <= 0:
        return 0
    is_open = darwin_qos.in_open_session() if open_session is None else open_session
    if is_open:
        if free is None or idle is None or free < OPEN_SESSION_PAUSE_BELOW_FREE_PCT or idle < OPEN_SESSION_MIN_CPU_IDLE_PCT:
            return 0
        return min(base, OPEN_SESSION_MAX_WORKERS)
    return base


def bench(url: str, model: str, key: str, levels=(1, 2, 3, 4, 6), tokens: int = 120) -> dict:
    """Measures the server's total speed at 1, 2, 3, ... simultaneous requests and the memory left, and writes the ceiling: the highest level whose total speed still rose by at least 15% over the
    one before, and whose single-request speed stayed above half of the lone request's speed."""
    import threading
    import time
    from urllib.request import Request, urlopen

    def one(results: list, i: int) -> None:
        body = json.dumps({"model": model, "messages": [{"role": "user", "content": "Explain in detail how a trailing stop works, with an example."}], "max_tokens": tokens,
                           "temperature": 0.2, "chat_template_kwargs": {"enable_thinking": False}}).encode()
        request = Request(url.rstrip("/") + "/chat/completions", data=body, headers={"Content-Type": "application/json", "Authorization": f"Bearer {key}"})
        started = time.time()
        with urlopen(request, timeout=300) as response:
            data = json.loads(response.read().decode())
        used = data.get("usage", {}).get("completion_tokens", tokens)
        results[i] = (used, time.time() - started)

    table = {}
    for level in levels:
        results: list = [None] * level
        started = time.time()
        threads = [threading.Thread(target=one, args=(results, i)) for i in range(level)]
        [t.start() for t in threads]
        [t.join() for t in threads]
        wall = time.time() - started
        total = sum(r[0] for r in results if r)
        table[level] = {"total_tokens_per_s": round(total / wall, 1), "per_request_tokens_per_s": round(min(r[0] / r[1] for r in results if r), 1), "free_pct_after": memory_free_pct()}
    best = 1
    for level in levels[1:]:
        before = table[levels[levels.index(level) - 1]]["total_tokens_per_s"]
        if table[level]["total_tokens_per_s"] >= 1.15 * before and table[level]["per_request_tokens_per_s"] >= 0.5 * table[1]["per_request_tokens_per_s"]:
            best = level
        else:
            break
    out = {"max_workers": max(1, best - 1), "measured": table, "note": "one seat is the commander's; the rest are research workers"}
    CAPACITY_FILE.parent.mkdir(parents=True, exist_ok=True)
    CAPACITY_FILE.write_text(json.dumps(out, indent=1))
    return out


if __name__ == "__main__":
    import sys
    if len(sys.argv) > 1 and sys.argv[1] == "bench":
        from . import llm
        cfg = llm.load_config()
        key = Path(cfg["omlx_key_file"]).read_text().strip()
        print(json.dumps(bench(cfg["omlx_url"], cfg["omlx_model"], key), indent=1))
    else:
        print({"free_pct": memory_free_pct(), "cpu_idle_pct": cpu_idle_pct(), "allowed_workers": now_allowed()})

"""Install this checkout's jobs as launchd agents (macOS) or systemd user units (Linux).

* Candidate and holding candles are fetched directly from Moomoo by each bot.
* ``local.papercompmoomoo.supervisor``: starts the competition bot immediately and restarts it after a failure; kept alive. Run only ONE.

Each plist or systemd unit is rendered for the paths of the checkout it is run from, so the same command works on any machine.
"""

from __future__ import annotations

import os
import json
import subprocess
import sys
import time
from pathlib import Path
from .account_ids import account_id
from xml.sax.saxutils import escape

PREFIX = "local.papercompmoomoo."
# job -> (module, log folder: "logs" inside the project or /tmp, log file stem)
JOBS = {
    "supervisor": ("papercompmoomoo.supervisor", "tmp", "papercompmoomoo-supervisor", False),
    "standby": ("papercompmoomoo.standby", "tmp", "papercompmoomoo-standby", True),  # the commander: board, hand-over, rules, council
    "codex-exit": ("papercompmoomoo.codex_exit_worker", "tmp", "papercompmoomoo-codex-exit", False),
    "research-replay": ("papercompmoomoo.research_replay_worker", "tmp", "papercompmoomoo-research-replay", True),
}


def is_linux() -> bool:
    return sys.platform.startswith("linux")


def systemd_quote(s: str) -> str:
    return '"' + s.replace("%", "%%").replace("\\", "\\\\").replace('"', '\\"') + '"'


def systemd_dir() -> Path:
    return Path.home() / ".config" / "systemd" / "user"


def job_plist(job: str, project: Path, python: Path) -> str:
    """The launchd plist text for ``job`` of the checkout at ``project`` using ``python``."""
    module, where, stem, background = JOBS[job]
    if where == "project":
        out = err = escape(str(project / "logs" / f"{stem}.log"))
    else:
        out, err = escape(f"/tmp/{stem}.log"), escape(f"/tmp/{stem}.err")
    low_priority = "<key>Nice</key><integer>10</integer>\n<key>ProcessType</key><string>Background</string>\n" if background else ""
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{PREFIX}{job}</string>
<key>ProgramArguments</key><array><string>{escape(str(python))}</string><string>-m</string><string>{module}</string></array>
<key>WorkingDirectory</key><string>{escape(str(project))}</string>
<key>EnvironmentVariables</key><dict><key>PYTHONPATH</key><string>src</string></dict>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
{low_priority}<key>StandardOutPath</key><string>{out}</string>
<key>StandardErrorPath</key><string>{err}</string>
</dict></plist>
"""


def job_service(job: str, project: Path, python: Path) -> str:
    """The systemd user service unit for ``job`` of the checkout at ``project`` using ``python``."""
    module, where, stem, background = JOBS[job]
    if where == "project":
        out = err = str(project / "logs" / f"{stem}.log").replace("%", "%%")
    else:
        out, err = f"/tmp/{stem}.log", f"/tmp/{stem}.err"
    exec_start = " ".join(systemd_quote(a) for a in (str(python), "-m", module))
    work_dir = str(project).replace("%", "%%")
    nice = "Nice=10\n" if background else ""
    return (
        f"[Unit]\nDescription=papercompmoomoo {PREFIX}{job}\n\n"
        f"[Service]\nType=simple\nWorkingDirectory={work_dir}\nExecStart={exec_start}\n"
        f"Environment=PYTHONPATH=src\nEnvironment=PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin\n"
        f"Restart=always\nRestartSec=10\n{nice}"
        f"StandardOutput=append:{out}\nStandardError=append:{err}\n\n"
        f"[Install]\nWantedBy=default.target\n"
    )


OMLX_LABEL = "local.omlx"
QWEN_LABEL = "local.qwen"
QWEN_SERVER = Path.home() / "llama-bin" / "build" / "bin" / "llama-server"
QWEN_MODEL = Path.home() / "models" / "Qwen2.5-7B-Instruct-Q4_K_M.gguf"
OMLX_MEMORY_GUARD_GB = 7  # keep the quantized model/cache below the documented machine ceiling; training stops oMLX first


def omlx_plist(home: Path, api_key: str) -> str:
    """The launchd plist of the local oMLX server: loopback only, API key required, the models of ~/.omlx/models, a memory ceiling, started at login and kept alive."""
    omlx = home / ".omlx" / "venv" / "bin" / "omlx"
    args = [str(omlx), "serve", "--model-dir", str(home / ".omlx" / "models"), "--host", "127.0.0.1", "--port", "8000", "--memory-guard-gb", str(OMLX_MEMORY_GUARD_GB),
            "--max-concurrent-requests", "1"]
    items = "".join(f"<string>{escape(a)}</string>" for a in args)
    return f"""<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{OMLX_LABEL}</string>
<key>ProgramArguments</key><array>{items}</array>
<key>EnvironmentVariables</key><dict><key>OMLX_API_KEY</key><string>{escape(api_key)}</string></dict>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
<key>Nice</key><integer>10</integer>
<key>ProcessType</key><string>Background</string>
<key>StandardOutPath</key><string>/tmp/omlx.log</string>
<key>StandardErrorPath</key><string>/tmp/omlx.err</string>
</dict></plist>
"""


def omlx_service(home: Path, api_key: str) -> str:
    """The systemd user service of the local oMLX server."""
    omlx = home / ".omlx" / "venv" / "bin" / "omlx"
    args = [str(omlx), "serve", "--model-dir", str(home / ".omlx" / "models"), "--host", "127.0.0.1", "--port", "8000", "--memory-guard-gb", str(OMLX_MEMORY_GUARD_GB),
            "--max-concurrent-requests", "1"]
    exec_start = " ".join(systemd_quote(a) for a in args)
    return (
        f"[Unit]\nDescription=papercompmoomoo {OMLX_LABEL}\n\n"
        f"[Service]\nType=simple\nExecStart={exec_start}\n"
        f"Environment={systemd_quote(f'OMLX_API_KEY={api_key}')}\n"
        f"Restart=always\nRestartSec=10\nNice=10\n"
        f"StandardOutput=append:/tmp/omlx.log\nStandardError=append:/tmp/omlx.err\n\n"
        f"[Install]\nWantedBy=default.target\n"
    )


def qwen_service(server: Path = QWEN_SERVER, model: Path = QWEN_MODEL) -> str:
    """Linux llama.cpp service for the existing local Qwen Trading Savant GGUF."""
    binary_dir = server.parent
    args = [str(server), "--model", str(model), "--host", "127.0.0.1", "--port", "8000", "--alias", "qwen2.5-7b-instruct", "--device", "none", "--ctx-size", "4096", "--threads", "5", "--parallel", "1", "--n-gpu-layers", "0"]
    return (
        f"[Unit]\nDescription=ThomasAi local Qwen engine\n\n"
        f"[Service]\nType=simple\nExecStart={' '.join(systemd_quote(a) for a in args)}\n"
        f"Environment={systemd_quote(f'LD_LIBRARY_PATH={binary_dir}')}\n"
        f"Restart=always\nRestartSec=10\nNice=0\n"
        f"StandardOutput=append:/tmp/qwen-server.log\nStandardError=append:/tmp/qwen-server.err\n\n"
        f"[Install]\nWantedBy=default.target\n"
    )


def install_qwen() -> None:
    if not is_linux():
        raise SystemExit("the llama.cpp Qwen service is the Linux local-model path")
    if not QWEN_SERVER.is_file() or not QWEN_MODEL.is_file():
        raise SystemExit(f"missing Qwen runtime or model: {QWEN_SERVER} / {QWEN_MODEL}")
    unit = systemd_dir() / f"{QWEN_LABEL}.service"
    unit.parent.mkdir(parents=True, exist_ok=True)
    unit.write_text(qwen_service())
    # Retire earlier ad-hoc names and the macOS-only server path before binding the port.
    systemctl("disable", "--now", "qwen.service", check=False)
    systemctl("disable", "--now", f"{OMLX_LABEL}.service", check=False)
    systemctl("daemon-reload")
    systemctl("enable", "--now", f"{QWEN_LABEL}.service")
    systemctl("restart", f"{QWEN_LABEL}.service")
    print(f"installed {QWEN_LABEL}")


def install_omlx() -> None:
    """Creates the API key once (~/.omlx/api_key, readable only by the owner), writes the plist/unit and starts the server."""
    import secrets
    home = Path.home()
    key_file = home / ".omlx" / "api_key"
    if not key_file.exists():
        key_file.parent.mkdir(parents=True, exist_ok=True)
        key_file.write_text(secrets.token_urlsafe(32))
        key_file.chmod(0o600)
    if is_linux():
        unit = systemd_dir() / f"{OMLX_LABEL}.service"
        unit.parent.mkdir(parents=True, exist_ok=True)
        unit.write_text(omlx_service(home, key_file.read_text().strip()))
        unit.chmod(0o600)
        systemctl("daemon-reload")
        systemctl("enable", "--now", f"{OMLX_LABEL}.service")
        systemctl("restart", f"{OMLX_LABEL}.service")
        print(f"installed {OMLX_LABEL}")
        return
    plist = home / "Library" / "LaunchAgents" / f"{OMLX_LABEL}.plist"
    plist.parent.mkdir(parents=True, exist_ok=True)
    domain = f"gui/{os.getuid()}"
    subprocess.run(["launchctl", "bootout", f"{domain}/{OMLX_LABEL}"], capture_output=True)
    plist.write_text(omlx_plist(home, key_file.read_text().strip()))
    plist.chmod(0o600)
    for attempt in range(10):
        if subprocess.run(["launchctl", "bootstrap", domain, str(plist)], capture_output=True).returncode == 0:
            print(f"installed {OMLX_LABEL}")
            return
        time.sleep(1.0 + attempt)
    subprocess.run(["launchctl", "bootstrap", domain, str(plist)], check=True)


def target(job: str) -> Path:
    if is_linux():
        return systemd_dir() / f"{PREFIX}{job}.service"
    return Path.home() / "Library" / "LaunchAgents" / f"{PREFIX}{job}.plist"


def systemctl(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["systemctl", "--user", *args], capture_output=True, text=True, check=check)


def bootstrap(domain: str, job: str, attempts: int = 10) -> None:
    """``launchctl bootstrap`` for one job. A job that was just booted out is still being torn down for a moment and bootstrap then fails with "Input/output error":
    wait until the old one is gone and try again."""
    for attempt in range(attempts):
        gone = subprocess.run(["launchctl", "print", f"{domain}/{PREFIX}{job}"], capture_output=True).returncode != 0
        if gone:
            done = subprocess.run(["launchctl", "bootstrap", domain, str(target(job))], capture_output=True, text=True)
            if done.returncode == 0:
                return
        time.sleep(1.0 + attempt)
    subprocess.run(["launchctl", "bootstrap", domain, str(target(job))], check=True)


def install(project: Path, jobs: list[str]) -> None:
    python = project / ".venv" / "bin" / "python"
    if not python.exists():
        raise SystemExit(f"{python} does not exist: create the environment first (python3 -m venv .venv && .venv/bin/pip install -e .)")
    (project / "logs").mkdir(exist_ok=True)
    if is_linux():
        u_dir = systemd_dir()
        u_dir.mkdir(parents=True, exist_ok=True)
        if "codex-exit" in jobs:
            # Qwen does not own exits. Preserve any stale unit file as evidence,
            # but stop and disable it before starting the exclusive Codex worker.
            systemctl("disable", "--now", f"{PREFIX}qwen-exit.service", check=False)
            mode = project / "data" / "codex_exit_mode.json"
            mode.parent.mkdir(parents=True, exist_ok=True)
            state = {"exclusive": True, "paper_only": True,
                     "accounts": {"gainer": account_id("gainer"), "orb": account_id("orb")},
                     "activated_ms": time.time_ns() // 1_000_000,
                     "decision_agent": "codex"}
            temporary = mode.with_name(f"codex_exit_mode-{time.time_ns()}.tmp")
            with temporary.open("x") as stream:
                json.dump(state, stream)
            temporary.replace(mode)
        for job in jobs:
            (u_dir / f"{PREFIX}{job}.service").write_text(job_service(job, project, python))
        systemctl("daemon-reload")
        for job in jobs:
            unit = f"{PREFIX}{job}.service"
            systemctl("enable", "--now", unit)
            systemctl("restart", unit)
            print(f"installed {PREFIX}{job}")
        return
    domain = f"gui/{os.getuid()}"
    for job in jobs:
        if job == "codex-exit":
            subprocess.run(["launchctl", "bootout", f"{domain}/{PREFIX}qwen-exit"], capture_output=True)
            old_target = target("qwen-exit")
            if old_target.exists():
                retired = old_target.with_suffix(old_target.suffix + ".disabled")
                if not retired.exists():
                    old_target.rename(retired)
                else:
                    old_target.unlink()
        subprocess.run(["launchctl", "bootout", f"{domain}/{PREFIX}{job}"], capture_output=True)
        target(job).parent.mkdir(parents=True, exist_ok=True)
        target(job).write_text(job_plist(job, project, python))
        bootstrap(domain, job)
        print(f"installed {PREFIX}{job}")


RETIRED = ["dashboard"]  # the competition bot's own monitor on :9001; the TradingSystem portal (:8790) replaced it


def remove(jobs: list[str]) -> None:
    all_jobs = jobs + ["learner", "feedback"] + RETIRED
    if is_linux():
        u_dir = systemd_dir()
        for job in all_jobs:
            for ext in ("timer", "service"):
                systemctl("disable", "--now", f"{PREFIX}{job}.{ext}", check=False)
                (u_dir / f"{PREFIX}{job}.{ext}").unlink(missing_ok=True)
            print(f"removed {PREFIX}{job}")
        systemctl("daemon-reload", check=False)
        return
    for job in all_jobs:
        subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{PREFIX}{job}"], capture_output=True)
        target(job).unlink(missing_ok=True)
        print(f"removed {PREFIX}{job}")


def main() -> None:
    action = sys.argv[1] if len(sys.argv) > 1 else ""
    if action == "all":
        remove([])  # a leftover retired job is unloaded
        project = Path.cwd()
        # Full project installation must never start a competing exit model.
        install(project, list(JOBS))
    elif action == "qwen":
        raise SystemExit("Qwen cannot be installed as a live exit decision service; Codex owns paper exits.")
    elif action == "omlx" and not is_linux():
        install_omlx()
    elif action in JOBS:
        install(Path.cwd(), [action])
    elif action == "remove":
        remove(list(JOBS))
    else:
        raise SystemExit("usage: python -m papercompmoomoo.install all|supervisor|standby|codex-exit|remove   (run from the project folder)")


if __name__ == "__main__":
    main()

"""Bounded Codex CLI decision call using a read-only, per-decision MCP tool."""
from __future__ import annotations

import json
import hashlib
import os
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

PACKAGE = Path(__file__).resolve().parent
SCHEMA = PACKAGE / "codex_exit.schema.json"
MCP_SERVER = PACKAGE / "exit_context_mcp.py"
TIMEOUT_SECONDS = 24

class CodexDecisionError(RuntimeError):
    def __init__(self, message, evidence):
        super().__init__(message)
        self.evidence=evidence


def _codex():
    found = shutil.which("codex")
    if found:
        return found
    for candidate in (Path.home() / ".local/bin/codex", Path("/usr/local/bin/codex")):
        if candidate.is_file() and os.access(candidate, os.X_OK):
            return str(candidate)
    raise RuntimeError("Codex CLI is unavailable")


def decide(bot: str, brief: dict) -> tuple[dict, dict]:
    """Ask Codex for a structured decision; it receives one read-only MCP tool."""
    started = time.monotonic_ns()
    with tempfile.TemporaryDirectory(prefix="tradingsystem-codex-exit-") as temp:
        root = Path(temp)
        context_path = root / "live-exit-context.json"
        read_marker = root / "live-exit-context-read"
        with context_path.open("x") as stream:
            json.dump({"bot": bot, "brief": brief}, stream, separators=(",", ":"), allow_nan=False)
        prompt = (
            "You are the strategy HOLD/EXIT decision maker for this simulated position. Your objective is to "
            "maximize realized and risk-adjusted returns, not to defend a HOLD. First call "
            "read_live_exit_context exactly once. Base the strategy decision on price trend and "
            "closed current-session candles: price versus session VWAP, VWAP slope, 3/5-bar returns, "
            "higher-low/lower-low structure, candle close location, relative volume and signed volume flow, "
            "ATR, and SPY/QQQ regime. The order book is not a strategy signal; it is only used later to "
            "validate executable paper-order pricing. Missing or stale evidence is unknown. Weigh executable-bid "
            "P&L, peak giveback, entry risk, position age, and whether the remaining upside justifies the downside. "
            "VWAP is one reference, not permission to hold a losing trade: current-session VWAP cannot erase an "
            "overnight gap or a broken entry thesis. Protect open profits when momentum fades, buyers stop "
            "supporting higher lows, selling volume expands, or reward-to-risk deteriorates. HOLD only when "
            "remaining upside is supported by trend/volume and downside remains controlled. EXIT on confirmed "
            "trend deterioration, failed reclaim, repeated structure loss, distribution, or poor remaining "
            "reward-to-risk; do not wait for every indicator to agree or exit on one noisy print alone. "
            "For ORB positions, judge whether the opening-range breakout remains accepted: track opening "
            "range high/low and setup risk for this exact candidate, closes relative to its breakout level, "
            "entry-signal VWAP versus current session VWAP, retests, and whether current volume confirms "
            "buying or selling. Never transfer one candidate's opening-range parameters to another. Prefer "
            "HOLD while price holds this candidate's breakout structure and VWAP. "
            "EXIT when a candle-confirmed failed breakout or VWAP loss is reinforced by lower lows, weak closes, "
            "or rising down-candle volume; do not exit on one noisy print or P&L threshold. "
            "For Gainers, explicitly assess the configured stop/trailing stop, high-water price, and peak giveback "
            "in the provided position context. An independent deterministic risk guard enforces those levels "
            "even if you choose HOLD. When evidence weakens, favor protecting realized gains over waiting for "
            "a complete collapse. Choose HOLD or EXIT and select the applicable signal; use momentum_decay for weakening "
            "reward-to-risk or profit giveback. p_up is an uncalibrated "
            "estimate that future price exceeds the latest closed-candle price at your stated horizon. "
            "Return only the required schema JSON. You have no order, broker, shell, or write tools; the "
            "paper execution controller independently refreshes candles and validates any EXIT."
        )
        codex = _codex()
        args = [codex, "exec", "--ephemeral", "--sandbox", "read-only", "--skip-git-repo-check",
                "--ignore-user-config", "--cd", temp, "--output-schema", str(SCHEMA),
                "-c", f"mcp_servers.live_exit_context.command={json.dumps(os.sys.executable)}",
                "-c", f"mcp_servers.live_exit_context.args={json.dumps([str(MCP_SERVER), str(context_path), str(read_marker)])}",
                "-c", "mcp_servers.live_exit_context.required=true",
                "-c", 'mcp_servers.live_exit_context.tools.read_live_exit_context.approval_mode="approve"', "-"]
        environment = os.environ.copy()
        try:
            result = subprocess.run(args, input=prompt, capture_output=True, text=True,
                timeout=TIMEOUT_SECONDS, cwd=temp, env=environment, check=False)
        except subprocess.TimeoutExpired as exc:
            stdout=exc.stdout.decode(errors='replace') if isinstance(exc.stdout,bytes) else (exc.stdout or '')
            stderr=exc.stderr.decode(errors='replace') if isinstance(exc.stderr,bytes) else (exc.stderr or '')
            evidence={'prompt':prompt,'prompt_sha256':hashlib.sha256(prompt.encode()).hexdigest(),
                'stdout':stdout,'stderr':stderr,'timeout_seconds':TIMEOUT_SECONDS,
                'schema_sha256':hashlib.sha256(SCHEMA.read_bytes()).hexdigest(),
                'mcp_context_read':read_marker.exists()}
            raise CodexDecisionError(f"Codex exit decision exceeded {TIMEOUT_SECONDS}s",evidence) from exc
        latency_ms = (time.monotonic_ns() - started) // 1_000_000
        evidence={'prompt':prompt,'prompt_sha256':hashlib.sha256(prompt.encode()).hexdigest(),
            'stdout':result.stdout,'stderr':result.stderr,'returncode':result.returncode,
            'schema_sha256':hashlib.sha256(SCHEMA.read_bytes()).hexdigest(),
            'mcp_context_read':read_marker.is_file()}
        if result.returncode:
            detail = (result.stderr or result.stdout or "Codex CLI failed").strip()[:500]
            raise CodexDecisionError(f"Codex exit decision failed (exit {result.returncode}): {detail}",evidence)
        if not read_marker.is_file():
            raise CodexDecisionError("Codex did not read the required live exit context MCP tool",evidence)
        try:
            decision = json.loads(result.stdout.strip())
        except (json.JSONDecodeError, TypeError) as exc:
            raise CodexDecisionError("Codex returned non-JSON exit decision",evidence) from exc
        return decision, {"backend": "codex", "latency_ms": latency_ms,
                          "mcp_context_used": True, "model": "codex-cli-account-default",
                          **evidence}

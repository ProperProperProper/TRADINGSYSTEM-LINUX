#!/usr/bin/env python3
"""Allowlisted systemd user-service controls exposed over MCP stdio.

No arbitrary command, unit name, or system service is accepted. Run this from
the host user's Codex session, where that user can access its systemd bus.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RESTART_SCRIPT = ROOT / "ops" / "restart-market.sh"
SERVICES = {
    "supervisor": "local.papercompmoomoo.supervisor.service",
    "codex-exit": "local.papercompmoomoo.codex-exit.service",
    "standby": "local.papercompmoomoo.standby.service",
    "portal": "com.thomasai.portal.service",
    "qwen-inference": "local.qwen.service",
}

TOOLS = [
    {
        "name": "restart_market_services",
        "description": "Run the project's approved market restart helper. It installs/restarts the Codex exit worker and restarts the supervisor, standby, and portal. It does not restart Qwen inference.",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
    },
    {
        "name": "restart_project_service",
        "description": "Restart one allowlisted TradingSystem user service and report its active state.",
        "inputSchema": {
            "type": "object",
            "properties": {"service": {"type": "string", "enum": sorted(SERVICES)}},
            "required": ["service"],
            "additionalProperties": False,
        },
    },
    {
        "name": "project_service_status",
        "description": "Read active state for an allowlisted TradingSystem user service, or all services when service is omitted.",
        "inputSchema": {
            "type": "object",
            "properties": {"service": {"type": "string", "enum": sorted(SERVICES)}},
            "additionalProperties": False,
        },
    },
]


def _run(args: list[str], timeout: int = 30) -> dict:
    try:
        result = subprocess.run(args, cwd=ROOT, capture_output=True, text=True,
                                timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "timed out", "command": args}
    return {"ok": result.returncode == 0, "returncode": result.returncode,
            "stdout": result.stdout[-12000:], "stderr": result.stderr[-4000:]}


def _status(service: str) -> dict:
    unit = SERVICES[service]
    result = _run(["systemctl", "--user", "is-active", unit])
    return {"service": service, "unit": unit,
            "active": result["ok"] and result.get("stdout", "").strip() == "active",
            "result": result}


def call(name: str, arguments: dict) -> dict:
    if name == "restart_market_services":
        if arguments:
            raise ValueError("this tool takes no arguments")
        result = _run(["bash", str(RESTART_SCRIPT)], timeout=240)
        return {"restart": result,
                "services": [_status(key) for key in SERVICES if key != "qwen-learning"]}
    if name == "restart_project_service":
        if set(arguments) != {"service"} or arguments["service"] not in SERVICES:
            raise ValueError("service must be one of the advertised allowlisted names")
        service = arguments["service"]
        unit = SERVICES[service]
        restart = _run(["systemctl", "--user", "restart", unit], timeout=90)
        status = _status(service) if restart["ok"] else {"service": service, "active": False}
        return {"restart": restart, "status": status}
    if name == "project_service_status":
        if set(arguments) - {"service"}:
            raise ValueError("unsupported status arguments")
        if "service" in arguments:
            if arguments["service"] not in SERVICES:
                raise ValueError("service must be one of the advertised allowlisted names")
            return _status(arguments["service"])
        return {"services": [_status(key) for key in SERVICES]}
    raise ValueError("unknown service-control tool")


def send(message_id, result=None, error=None):
    payload = {"jsonrpc": "2.0", "id": message_id}
    payload["error" if error is not None else "result"] = error if error is not None else result
    sys.stdout.write(json.dumps(payload, separators=(",", ":"), allow_nan=False) + "\n")
    sys.stdout.flush()


def serve():
    for line in sys.stdin:
        request = {}
        try:
            request = json.loads(line)
            method = request.get("method")
            params = request.get("params") or {}
            ident = request.get("id")
            if method == "initialize":
                send(ident, {"protocolVersion": params.get("protocolVersion", "2025-03-26"),
                             "capabilities": {"tools": {}},
                             "serverInfo": {"name": "tradingsystem-service-control", "version": "1.0.0"}})
            elif method == "notifications/initialized":
                continue
            elif method == "ping":
                send(ident, {})
            elif method == "tools/list":
                send(ident, {"tools": TOOLS})
            elif method == "tools/call":
                try:
                    result = call(params.get("name", ""), params.get("arguments") or {})
                    send(ident, {"content": [{"type": "text", "text": json.dumps(result, allow_nan=False)}],
                                 "isError": not result.get("restart", result).get("ok", True)
                                 if isinstance(result.get("restart", result), dict) else False})
                except Exception as exc:
                    send(ident, error={"code": -32602, "message": str(exc)})
            elif ident is not None:
                send(ident, error={"code": -32601, "message": "method not found"})
        except Exception as exc:
            if request.get("id") is not None:
                send(request["id"], error={"code": -32603, "message": type(exc).__name__})


if __name__ == "__main__":
    serve()

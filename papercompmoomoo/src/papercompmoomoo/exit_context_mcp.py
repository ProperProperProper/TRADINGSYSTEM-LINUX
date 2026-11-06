"""Read-only MCP tool exposing one prebuilt live exit context to Codex."""
from __future__ import annotations

import json
import sys
from pathlib import Path

TOOL = {
    "name": "read_live_exit_context",
    "description": "Read the current Moomoo-only position, executable book, closed candles, indicators, and SPY/QQQ context for this one paper exit decision. Read-only; cannot place or alter orders.",
    "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
}


def _reply(message_id, result=None, error=None):
    response = {"jsonrpc": "2.0", "id": message_id}
    response["error" if error is not None else "result"] = error if error is not None else result
    sys.stdout.write(json.dumps(response, separators=(",", ":"), allow_nan=False) + "\n")
    sys.stdout.flush()


def serve(context_path: Path, read_marker: Path):
    context = json.loads(context_path.read_text())
    if context.get("brief", {}).get("schema") != "live_market_brief_v1":
        raise ValueError("expected normalized live Moomoo brief")
    for raw in sys.stdin:
        try:
            request = json.loads(raw)
            method, params = request.get("method"), request.get("params") or {}
            ident = request.get("id")
            if method == "notifications/initialized":
                continue
            if method == "initialize":
                _reply(ident, {"protocolVersion": params.get("protocolVersion", "2025-03-26"),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "tradingsystem-live-exit-context", "version": "1.0.0"}})
            elif method == "ping":
                _reply(ident, {})
            elif method == "tools/list":
                _reply(ident, {"tools": [TOOL]})
            elif method == "tools/call":
                if params.get("name") != TOOL["name"] or params.get("arguments") not in ({}, None):
                    _reply(ident, error={"code": -32602, "message": "unknown tool or arguments"})
                    continue
                with read_marker.open("x") as marker:
                    marker.write(str(context["brief"].get("symbol", "")))
                _reply(ident, {"content": [{"type": "text", "text": json.dumps(context,
                    separators=(",", ":"), allow_nan=False)}], "isError": False})
            elif ident is not None:
                _reply(ident, error={"code": -32601, "message": "method not found"})
        except Exception as exc:  # malformed client messages are reported without leaking paths
            if isinstance(locals().get("request"), dict) and request.get("id") is not None:
                _reply(request["id"], error={"code": -32603, "message": type(exc).__name__})


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("expected a context JSON path and read-marker path")
    serve(Path(sys.argv[1]), Path(sys.argv[2]))

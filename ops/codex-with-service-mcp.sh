#!/bin/bash
set -euo pipefail
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PYTHON_BIN="$(command -v python3)"
CODEX_BIN="$(command -v codex)"
exec "$CODEX_BIN" \
  -c "mcp_servers.service_control.command=\"$PYTHON_BIN\"" \
  -c "mcp_servers.service_control.args=[\"$PROJECT_ROOT/ops/service_control_mcp.py\"]" \
  -c "mcp_servers.service_control.enabled=true" \
  "$@"

#!/bin/sh
# Mirror the lab (qwen-lab master) into lab/ and the MCP server into mcp/, then push.
# One-line stub: the logic is Rust (`bot-ops sync-lab`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" sync-lab "$@"

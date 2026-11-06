#!/bin/sh
# Compact health check of the moomoo bot. Usage: ./check.sh [minutes of log to scan for warnings/errors, default 60]
# One-line stub: the logic is Rust (`bot-ops check`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" check "$@"

#!/bin/sh
# Scheduled monitor for the moomoo bot: ops/monitor.sh check (hourly) | improve (weekday review).
# One-line stub: the logic is Rust (`bot-ops monitor`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" monitor "$@"

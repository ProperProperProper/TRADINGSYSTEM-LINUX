#!/bin/sh
# Hourly promotion job: applies a fully validated strategy promotion from the lab in code.
# One-line stub: the logic is Rust (`bot-ops promote`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" promote "$@"

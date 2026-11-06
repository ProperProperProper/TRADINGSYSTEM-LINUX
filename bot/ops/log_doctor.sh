#!/bin/sh
# Hourly log doctor: warnings ledger scan, a Claude session fixes what is open, docs/WARNINGS.md published.
# One-line stub: the logic is Rust (`bot-ops log-doctor`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" log-doctor "$@"

#!/bin/sh
# Daily: add fresh backtests to the research archive the lab learns from.
# One-line stub: the logic is Rust (`bot-ops lab-export`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" lab-export "$@"

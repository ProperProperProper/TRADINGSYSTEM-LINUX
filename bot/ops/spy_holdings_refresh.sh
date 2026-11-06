#!/bin/sh
# Refresh the bot's universe from SPY's official daily holdings file.
# One-line stub: the logic is Rust (`bot-ops spy-holdings-refresh`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" spy-holdings-refresh "$@"

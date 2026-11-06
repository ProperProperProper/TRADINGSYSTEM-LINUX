#!/bin/sh
# Push guarantee: every commit in the bot repo and the lab repo reaches GitHub; idle work is committed.
# One-line stub: the logic is Rust (`bot-ops push-guard`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" push-guard "$@"

#!/bin/sh
# Publish the lab's working state into state/ (screened for secrets and account data).
# One-line stub: the logic is Rust (`bot-ops publish-state`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" publish-state "$@"

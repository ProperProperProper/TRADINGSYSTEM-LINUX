#!/bin/sh
# Daily fix report: docs/fixes/<date>.md from the commit messages, checked by code.
# One-line stub: the logic is Rust (`bot-ops fix-report`, src/bin/bot-ops/) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" fix-report "$@"

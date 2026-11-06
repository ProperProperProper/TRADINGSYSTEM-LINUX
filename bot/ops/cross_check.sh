#!/bin/sh
# Developer helper: proves the crate still compiles for Linux and Windows from any machine, without rustup.
# One-line stub: the logic is Rust (`bot-ops cross-check`, src/bin/bot-ops/crosscheck.rs) and runs the same on macOS, Linux and Windows.
D="$(cd "$(dirname "$0")/.." && pwd)"; B="$D/target/release/bot-ops"; [ -x "$B" ] || (cd "$D" && cargo build --release --bin bot-ops >/dev/null 2>&1); exec "$B" cross-check "$@"

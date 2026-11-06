#!/bin/sh
# Quant supervisor (one Claude Code session four times a day).
# One-line stub: the logic is Rust (`qwen-ops supervisor`, src/supervisor.rs) and runs the same on macOS, Linux and Windows.
B="$(dirname "$0")/../target/release/qwen-ops"; [ -x "$B" ] || B="$HOME/.local/bin/qwen-ops"; exec "$B" supervisor "$@"

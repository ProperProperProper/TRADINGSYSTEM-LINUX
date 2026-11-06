#!/bin/sh
# Lab + bot monitor (every 15 minutes).
# One-line stub: the logic is Rust (`qwen-ops monitor`, src/monitor.rs) and runs the same on macOS, Linux and Windows.
B="$(dirname "$0")/../target/release/qwen-ops"; [ -x "$B" ] || B="$HOME/.local/bin/qwen-ops"; exec "$B" monitor "$@"

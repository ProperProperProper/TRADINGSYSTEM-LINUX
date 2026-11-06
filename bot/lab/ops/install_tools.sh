#!/bin/sh
# Builds and installs the lab's Rust helper binaries (qwen-tools, qwen-ops, qwen-notify, qwen-guard).
# One-line stub: the logic is Rust (`qwen-ops install-tools`, src/opsctl.rs) and runs the same on macOS, Linux and Windows.
B="$(dirname "$0")/../target/release/qwen-ops"; [ -x "$B" ] || B="$HOME/.local/bin/qwen-ops"; exec "$B" install-tools "$@"

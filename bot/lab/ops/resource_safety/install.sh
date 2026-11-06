#!/bin/sh
# Installs everything: helper tools, the lab binary, every background job and the webhook-protection git hooks (see docs/RESOURCE_SAFETY.md, docs/SETUP_*.md).
# One-line stub: the logic is Rust (`qwen-ops install`, src/opsctl.rs) and runs the same on macOS, Linux and Windows.
B="$(dirname "$0")/../../target/release/qwen-ops"; [ -x "$B" ] || B="$HOME/.local/bin/qwen-ops"; exec "$B" install "$@"

#!/bin/sh
# Safe redeploy of the lab (build, stop the jobs, replace the binaries, register every job again, verify).
# One-line stub: the logic is Rust (`qwen-ops redeploy`, src/opsctl.rs) and runs the same on macOS, Linux and Windows.
B="$(dirname "$0")/../target/release/qwen-ops"; [ -x "$B" ] || B="$HOME/.local/bin/qwen-ops"; exec "$B" redeploy "$@"

#!/bin/sh
# One-line stub: the logic is Rust (`thomas setup`). Run it after cloning the repository on a new machine.
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
command -v cargo >/dev/null 2>&1 || { echo "install Rust first: https://rustup.rs" >&2; exit 1; }
cd "$(dirname "$0")" && exec cargo run --release -p thomas -- setup "$@"

#!/bin/bash
# Developer helper: proves the crate still compiles for Linux and Windows from any machine, without rustup and without touching the installed toolchain.
# It uses a private copy of the same rustc version in ~/.cache/xtargets/rust (official build plus the Linux and Windows standard libraries, unpacked from
# static.rust-lang.org/dist with install.sh --prefix; see docs/SETUP_LINUX.md "cross-checking"). `cargo check` needs no machine code, so the C compiler and
# archiver that libsqlite3-sys and ring call are replaced by stubs that write empty files. Usage: ops/cross_check.sh [target ...]  (default: Linux and Windows)
set -u
cd "$(dirname "$0")/.."
X="$HOME/.cache/xtargets"
[ -x "$X/rust/bin/rustc" ] || { echo "cross_check: $X/rust missing (see the header of this script)"; exit 2; }
cat > "$X/fakecc" <<'STUB'
#!/bin/bash
# stub C compiler / archiver: succeeds and writes an empty file wherever an output is asked for
out=""; prev=""; for a in "$@"; do [ "$prev" = "-o" ] && out="$a"; case "$a" in -o?*) out="${a#-o}";; esac; prev="$a"; done
[ -z "$out" ] && for a in "$@"; do case "$a" in *.a) out="$a";; esac; done
[ -n "$out" ] && : > "$out"
exit 0
STUB
chmod +x "$X/fakecc"
for t in "${@:-x86_64-unknown-linux-gnu x86_64-pc-windows-gnu}"; do for tt in $t; do
  echo "== cargo check --target $tt"
  u="${tt//-/_}"
  env RUSTC="$X/rust/bin/rustc" "CC_$u=$X/fakecc" "AR_$u=$X/fakecc" "CARGO_TARGET_DIR=target/xcheck" \
    cargo check --target "$tt" --lib --bins 2>&1 | grep -E "^(error|warning)|Finished|could not" -A7 | head -${LINES_MAX:-70}
done; done

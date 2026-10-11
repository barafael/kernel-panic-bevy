#!/usr/bin/env bash
# Build the probe for every locally available target, run each, and
# compare the bit streams. Exit status 1 if any `det:` probe differs.
#
#   net-experiments/m0-floatprobe/run-matrix.sh [out-dir]
#
# Targets: x86_64 glibc, x86_64 musl, x86_64 with -C target-cpu=native
# (FMA/AVX-512 hosts must not change a bit), wasm32 under Node, and the
# same wasm after `wasm-opt -O2` (what Trunk runs on the release build).
# Optional: x86_64-pc-windows-msvc under Wine when cargo-xwin and wine
# are installed (the MSVC binary's std math then comes from Wine's
# ucrtbase, not Microsoft's, so only its det: rows say anything).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="${1:-$here/out}"
mkdir -p "$out"
cd "$here"
T="${CARGO_TARGET_DIR:-$here/target}"

cargo build --release
cargo build --release --target x86_64-unknown-linux-musl
cargo build --release --lib --target wasm32-unknown-unknown
RUSTFLAGS="-C target-cpu=native" CARGO_TARGET_DIR="$T/native-cpu" cargo build --release

"$T/release/floatprobe" run "$out/linux-gnu.bin"
"$T/x86_64-unknown-linux-musl/release/floatprobe" run "$out/linux-musl.bin"
"$T/native-cpu/release/floatprobe" run "$out/target-cpu-native.bin"
wasm="$T/wasm32-unknown-unknown/release/m0_floatprobe.wasm"
node run.mjs "$wasm" "$out/wasm32-node.bin"
files=("$out/linux-gnu.bin" "$out/wasm32-node.bin" "$out/linux-musl.bin" "$out/target-cpu-native.bin")

if command -v wasm-opt >/dev/null; then
    wasm-opt -O2 --enable-bulk-memory --enable-sign-ext --enable-nontrapping-float-to-int \
        --enable-mutable-globals --enable-multivalue --enable-reference-types \
        "$wasm" -o "$out/opt.wasm"
    node run.mjs "$out/opt.wasm" "$out/wasm32-wasm-opt-O2.bin"
    files+=("$out/wasm32-wasm-opt-O2.bin")
fi

if command -v cargo-xwin >/dev/null && command -v wine >/dev/null; then
    cargo xwin build --release --target x86_64-pc-windows-msvc
    wine "$T/x86_64-pc-windows-msvc/release/floatprobe.exe" run "$out/windows-msvc-wine.bin" 2>/dev/null
    files+=("$out/windows-msvc-wine.bin")
fi

"$T/release/floatprobe" compare "${files[@]}"

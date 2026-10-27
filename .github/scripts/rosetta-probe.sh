#!/usr/bin/env bash
# Replay the golden logs under Rosetta 2, one process per log, all logs
# at once, for several rounds; compare every trail with the Linux
# reference, and where one diverges keep the dumps `--expect-trail`
# wrote: the units at the diverging tick and the one before, and the
# `.records` files, the part hashes' exact input bytes, which
# `kernel-panic --check-records` hashes again on any machine.
#
# Usage: rosetta-probe.sh <bin> <golden dir> <reference trail dir> <rounds> <out dir>
#
# Diagnoses the sporadic `macos-x86_64-rosetta diverges from
# linux-x86_64-gnu` of determinism.yml (2026-10-21 onwards): a
# per-process fault (the same binary passes and fails on the same host),
# in roughly one process in ten to fifty; one process per log, in
# parallel, gets the most process samples out of a runner-hour.
set -euo pipefail

bin=${1:?bin}; golden=${2:?golden dir}; ref=${3:?reference trail dir}; rounds=${4:?rounds}; out=${5:?out dir}
mkdir -p "$out"
logs=("$golden"/*.kpreplay)

sw_vers || true
# The host's chip: Rosetta's float handling is one implementation on
# M1 (which predates ARM's FEAT_AFP) and the hardware's on later chips
# (dougallj, "Why is Rosetta 2 fast?"), and the runner fleet may mix.
sysctl -n machdep.cpu.brand_string hw.model hw.ncpu hw.memsize || true
sysctl hw.optional.arm 2>/dev/null | grep -i "afp\|ebf16\|i8mm" || true
echo "arch: $(arch)"; file "$bin"
ls -la /Library/Apple/usr/libexec/oah/ 2>/dev/null || true

# Rosetta translates a fresh binary ahead of time in the background
# (`oahd`, cache under /var/db/oah) and runs it by JIT until that is
# done: which path a replay took is told by whether an .aot file for
# this binary exists before it starts and after it ends.
aot_state() {
  local n
  n=$(sudo find /var/db/oah -name '*.aot' -newer "$bin" 2>/dev/null | wc -l | tr -d ' ')
  echo "aot files newer than the binary: $n"
}

# 1. Rounds of one process per log, all at once.
for i in $(seq 1 "$rounds"); do
  echo "::group::round $i"
  echo "before round $i: $(aot_state)"
  t0=$SECONDS
  dir="$out/round-$i"; mkdir -p "$dir"
  pids=()
  for log in "${logs[@]}"; do
    stem=$(basename "$log" .kpreplay)
    # `--expect-trail`: the first tick whose hash differs from the
    # reference's dumps its units and the previous tick's, whatever
    # tick that turns out to be.
    arch -x86_64 "$bin" --replay "$log" --trail-dir "$dir" \
      --expect-trail "$ref" --dump-dir "$dir" > "$dir/$stem.log" 2>&1 &
    pids+=($!)
  done
  fail=0
  for p in "${pids[@]}"; do wait "$p" || fail=1; done
  for log in "${logs[@]}"; do
    stem=$(basename "$log" .kpreplay)
    echo "--- $stem"; cat "$dir/$stem.log"
  done
  echo "round $i took $((SECONDS - t0)) s (exit status $fail)"
  echo "after round $i: $(aot_state)"
  echo "::endgroup::"
done

# 2. Compare every process's trail with the reference.
: > "$out/divergences"  # "<round> <stem> <first diverging tick>" per diverging trail
status=0
processes=0
for i in $(seq 1 "$rounds"); do
  for trail in "$out/round-$i"/*.trail; do
    processes=$((processes + 1))
    name=$(basename "$trail"); stem=${name%.trail}
    r="$ref/$name"
    if [ ! -f "$r" ]; then echo "no reference $name"; continue; fi
    if cmp -s "$r" "$trail"; then
      echo "round $i $name: same as the reference"
    else
      # `cmp` exits 1 on a difference: under `pipefail` + `-e` that
      # ended run 40350317806's script right at its first divergence.
      line=$(cmp "$r" "$trail" 2>&1 | grep -o '[0-9]*$' || true)
      n=$(diff "$r" "$trail" | grep -c '^<' || true)
      echo "::warning::round $i $name: diverges from the reference at tick $line ($n lines differ)"
      echo "  reference: $(sed -n "${line}p" "$r")"
      echo "  round $i:  $(sed -n "${line}p" "$trail")"
      echo "$i $stem $line" >> "$out/divergences"
      status=1
    fi
  done
done
echo "$processes processes, $(wc -l < "$out/divergences" | tr -d ' ') diverged"

# 3. The dumps `--expect-trail` wrote, and the records re-hashed by
# this same binary in a fresh process: a `DIFFERENT` here is the
# dumping process having hashed its own bytes wrongly.
ls -la "$out"/round-*/*.txt "$out"/round-*/*.records 2>/dev/null || echo "no dumps: no process diverged"
for rec in "$out"/round-*/*.records; do
  [ -f "$rec" ] || continue
  arch -x86_64 "$bin" --check-records "$rec" || true
done
exit $status

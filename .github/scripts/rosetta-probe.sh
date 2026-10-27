#!/usr/bin/env bash
# Replay the golden logs under Rosetta 2 several times in one process
# each and compare every trail with the Linux reference and with each
# other; where a trail diverges, replay that log again with unit dumps
# at the diverging tick until a run reproduces the divergence.
#
# Usage: rosetta-probe.sh <bin> <golden dir> <reference trail dir> <runs> <out dir>
#
# Diagnoses the sporadic `macos-x86_64-rosetta diverges from
# linux-x86_64-gnu` of determinism.yml (2026-10-21 onwards): whether two
# runs of the same binary on the same machine differ (something
# nondeterministic in the process: thread timing, or Rosetta's
# translation), and which units differ when they do.
set -euo pipefail

bin=${1:?bin}; golden=${2:?golden dir}; ref=${3:?reference trail dir}; runs=${4:?runs}; out=${5:?out dir}
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
  sudo find /var/db/oah -name '*.aot' -newer "$bin" -exec ls -la {} \; 2>/dev/null | head -5
}

# 1. Several replays of every log, one process per run as determinism.yml runs it.
for i in $(seq 1 "$runs"); do
  echo "::group::run $i"
  echo "before run $i: $(aot_state)"
  t0=$SECONDS
  # `--expect-trail`: the first tick whose hash differs from the
  # reference's dumps its units and the previous tick's, whatever tick
  # that turns out to be.
  arch -x86_64 "$bin" --replay "${logs[@]}" --trail-dir "$out/run-$i" \
    --expect-trail "$ref" --dump-dir "$out/run-$i"
  echo "run $i took $((SECONDS - t0)) s"
  echo "after run $i: $(aot_state)"
  echo "::endgroup::"
done

# 2. Compare: each run with the reference, and with run 1.
: > "$out/divergences"  # "<stem> <first diverging tick>" per diverging trail
status=0
for i in $(seq 1 "$runs"); do
  for trail in "$out/run-$i"/*.trail; do
    name=$(basename "$trail"); stem=${name%.trail}
    r="$ref/$name"
    if [ ! -f "$r" ]; then echo "no reference $name"; continue; fi
    if cmp -s "$r" "$trail"; then
      echo "run $i $name: same as the reference"
    else
      # `cmp` exits 1 on a difference: under `pipefail` + `-e` that
      # ended run 40350317806's script right at its first divergence.
      line=$(cmp "$r" "$trail" 2>&1 | grep -o '[0-9]*$' || true)
      n=$(diff "$r" "$trail" | grep -c '^<' || true)
      echo "::warning::run $i $name: diverges from the reference at tick $line ($n lines differ)"
      echo "  reference: $(sed -n "${line}p" "$r")"
      echo "  run $i:    $(sed -n "${line}p" "$trail")"
      echo "$stem $line" >> "$out/divergences"
      status=1
    fi
    if [ "$i" -gt 1 ] && ! cmp -s "$out/run-1/$name" "$trail"; then
      echo "::warning::run $i $name: differs from run 1 at tick $(cmp "$out/run-1/$name" "$trail" 2>&1 | grep -o '[0-9]*$' || true)"
    fi
  done
done

# 3. The dumps `--expect-trail` wrote (the diverging tick and the one
# before, per diverging log).
ls -la "$out"/run-*/*.txt 2>/dev/null || echo "no dumps: no run diverged"
exit $status

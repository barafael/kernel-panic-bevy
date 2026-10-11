#!/usr/bin/env bash
# Compare per-tick state-hash trails from the determinism matrix.
#
# Usage: compare-trails.sh <dir> <reference platform>
#
# <dir> holds one sub-directory per platform, `trail-<platform>/`, each with
# one `<replay>.trail` file per golden log (the layout download-artifact
# produces for the `trail-*` artifacts of determinism.yml). A trail is one
# line per tick, `<tick> <hash> [part hashes…]`. Every platform's trail must
# be byte-identical to the reference platform's; otherwise this prints the
# first diverging line of each pair and exits 1. No trails at all (no golden
# logs yet) is a pass.
set -euo pipefail

dir=${1:?usage: compare-trails.sh <dir> <reference platform>}
ref=${2:?usage: compare-trails.sh <dir> <reference platform>}
summary=${GITHUB_STEP_SUMMARY:-/dev/null}

shopt -s nullglob
platform_dirs=("$dir"/trail-*/)
if [ ${#platform_dirs[@]} -eq 0 ]; then
  echo "::notice::No hash trails to compare (no golden replays yet)."
  echo "No hash trails to compare (no golden replays yet)." >> "$summary"
  exit 0
fi

ref_dir="$dir/trail-$ref"
ref_trails=("$ref_dir"/*.trail)
if [ ${#ref_trails[@]} -eq 0 ]; then
  echo "::error::no reference trails in $ref_dir"
  exit 1
fi

status=0
{
  echo "| platform | replay | result |"
  echo "|---|---|---|"
} >> "$summary"

for ref_trail in "${ref_trails[@]}"; do
  name=$(basename "$ref_trail")
  for platform_dir in "${platform_dirs[@]}"; do
    platform=${platform_dir%/}
    platform=${platform##*/trail-}
    [ "$platform" = "$ref" ] && continue
    other="$platform_dir$name"
    if [ ! -f "$other" ]; then
      echo "::error::$platform: no $name"
      echo "| $platform | $name | missing |" >> "$summary"
      status=1
      continue
    fi
    if cmp -s "$ref_trail" "$other"; then
      echo "same     $platform $name ($(wc -l < "$other") ticks)"
      echo "| $platform | $name | identical |" >> "$summary"
      continue
    fi
    # First differing line (a trail that is a strict prefix of the other
    # diverges at the line after its last).
    line=$(awk 'NR == FNR { a[FNR] = $0; n = FNR; next }
                { m = FNR; if (!(FNR in a) || a[FNR] != $0) { print FNR; found = 1; exit } }
                END { if (!found) print (m < n ? m : n) + 1 }' "$ref_trail" "$other")
    want=$(sed -n "${line}p" "$ref_trail")
    got=$(sed -n "${line}p" "$other")
    echo "::error::$platform diverges from $ref in $name at line $line"
    echo "  $ref: ${want:-<end of trail>}"
    echo "  $platform: ${got:-<end of trail>}"
    echo "| $platform | $name | diverges at line $line: \`${got:-<end>}\` vs \`${want:-<end>}\` |" >> "$summary"
    status=1
  done
done

# Every platform must have replayed every log the reference did and nothing
# else; a log only some platforms know points at a stale golden set.
for platform_dir in "${platform_dirs[@]}"; do
  for trail in "$platform_dir"*.trail; do
    if [ ! -f "$ref_dir/$(basename "$trail")" ]; then
      echo "::error::$(basename "$trail") exists for ${platform_dir%/} but not for $ref"
      status=1
    fi
  done
done

exit $status

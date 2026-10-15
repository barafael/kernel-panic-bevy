#!/bin/bash
# Screenshot and audit every launch-menu page at several window sizes,
# each inside its own virtual KWin session (`kwin_wayland --virtual`: the
# real GPU renders into an off-screen framebuffer; nothing opens on the
# desktop and the mouse cannot reach the window).
#
#   scripts/menu-layout-check.sh [--no-build] [WxH ...]
#
# Output: target/menu-layout/<WxH>/<page>.png, <page>.layout.txt (every
# frame's rect and the problems found) and issues.txt; a summary at the
# end. Exit code 1 if any page had an issue. See `ui::menu_shots`.
set -u
cd "$(dirname "$0")/.."
BUILD=1
if [ "${1:-}" = "--no-build" ]; then BUILD=0; shift; fi
SIZES=("$@")
[ ${#SIZES[@]} -eq 0 ] && SIZES=(800x600 1280x720 1920x1080 2560x1440 3440x1440)
if [ $BUILD = 1 ]; then cargo build -p kernel-panic || exit 1; fi
BIN="$(realpath "${CARGO_TARGET_DIR:-target}/debug/kernel-panic")"
OUT=target/menu-layout
mkdir -p "$OUT"
FAIL=0
for size in "${SIZES[@]}"; do
  dir="$OUT/$size"
  rm -rf "$dir"; mkdir -p "$dir"
  w=${size%x*}; h=${size#*x}
  echo "== $size"
  # Bevy's first frames sometimes fail a buffer validation and quit
  # ("Quitting the application due to Validation RenderError"); a run
  # that captured nothing is tried once more.
  for attempt in 1 2; do
    timeout 240 kwin_wayland --virtual --width "$w" --height "$h" --no-lockscreen --no-global-shortcuts \
      --exit-with-session "env KP_WINDOW=$size KP_MENU_SHOTS=$PWD/$dir KP_MENU_SHOTS_WARMUP=${KP_MENU_SHOTS_WARMUP:-150} $BIN" \
      > "$dir/stdout.txt" 2>&1
    code=$?
    pages=$(ls "$dir"/*.layout.txt 2>/dev/null | wc -l)
    [ "$pages" -gt 0 ] && break
    echo "   attempt $attempt captured nothing (see $dir/stdout.txt), retrying"
  done
  issues=$(wc -l < "$dir/issues.txt" 2>/dev/null || echo 0)
  echo "   exit $code, $pages pages audited, $issues issues"
  if [ "$issues" != "0" ]; then FAIL=1; sed 's/^/   /' "$dir/issues.txt"; fi
  if [ "$pages" -lt 9 ]; then FAIL=1; echo "   (not every page was captured)"; fi
done
exit $FAIL

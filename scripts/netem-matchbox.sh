#!/bin/sh
# A lockstep match over real WebRTC under emulated WAN conditions
# (net::matchbox_tests::netem_match; net-experiments/m2-transport.md):
# every peer a full headless game, all in one process, introduced by an
# in-process signaling server; their trails must agree.
#
# usage: scripts/netem-matchbox.sh [--sudo] "<netem args>" [cargo test args…]
#   e.g. scripts/netem-matchbox.sh "delay 40ms 10ms loss 1%"
#        scripts/netem-matchbox.sh --sudo "delay 50ms loss 2%" --release
# Size: KP_NET_PEERS (4), KP_NET_AI (0), KP_NET_SECS (30), KP_NET_MAP
# (Quad_Core). KP_NETEM_TEST=net::matchbox_tests::netem_probe runs the
# transport-only latency probe instead.
#
# Every packet crosses `lo` once, so the netem delay is the one-way
# delay (RTT ≈ 2 × delay); the signaling websocket is shaped too.
# - Default: inside `unshare -rn` (a user + network namespace, no root;
#   X-transport's setup). webrtc-rs skips loopback ICE candidates, so a
#   dummy interface provides the host address; traffic to it still goes
#   over the namespace's `lo`, where the qdisc sits.
# - `--sudo` (CI: Ubuntu runners refuse unprivileged user namespaces):
#   `sudo tc` on the machine's own `lo`, removed again on exit. Host
#   candidates are the runner's addresses, whose local traffic goes over
#   `lo` all the same.
set -eu
cd "$(dirname "$0")/.."
SUDO=0
if [ "${1:-}" = --sudo ]; then
  SUDO=1
  shift
fi
NETEM="$1"
shift
# Build outside the namespace (no network in there).
# `--color never`: CI sets CARGO_TERM_COLOR=always, whose escapes would
# break the match below.
BIN=$(cargo test --locked --color never -p kernel-panic --no-run "$@" 2>&1 \
  | sed -n 's/.*Executable unittests src\/main.rs (\(.*\))/\1/p')
if [ ! -x "$BIN" ]; then
  echo "no test binary (cargo test --no-run $*)" >&2
  exit 1
fi
export KP_NET_LABEL="netem $NETEM"
TEST="${KP_NETEM_TEST:-net::matchbox_tests::netem_match}"
if [ "$SUDO" = 1 ]; then
  sudo tc qdisc add dev lo root netem $NETEM limit 100000
  trap 'sudo tc qdisc del dev lo root' EXIT
  "$BIN" "$TEST" --exact --ignored --nocapture --test-threads 1
else
  exec unshare -rn sh -c '
    ip link set lo up
    ip link add d0 type dummy
    ip addr add 10.77.0.1/24 dev d0
    ip link set d0 up
    tc qdisc add dev lo root netem '"$NETEM"' limit 100000
    exec "$0" "$1" --exact --ignored --nocapture --test-threads 1
  ' "$BIN" "$TEST"
fi

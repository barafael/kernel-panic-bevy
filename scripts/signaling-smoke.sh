#!/bin/sh
# Smoke test of the deployed signaling server
# (net::matchbox_tests::deployed_signaling_smoke; net-experiments/m3-rooms.md):
# two native peers on this machine join a fresh room `smoke-<code>` on
# wss://omdurman-matchbox.fly.dev (or $KP_SIGNALING) with the default ICE
# servers (Google STUN), connect over WebRTC, exchange traffic on both
# channels and form a lobby room with a chat line. Prints how long the
# server took to answer (it sleeps when idle), the connection time and
# the round trips. Needs the internet; never run in CI.
#
# usage: scripts/signaling-smoke.sh [cargo test args…]
#   e.g. KP_SIGNALING=wss://my-server.example scripts/signaling-smoke.sh --release
set -eu
cd "$(dirname "$0")/.."
exec cargo test --locked -p kernel-panic "$@" net::matchbox_tests::deployed_signaling_smoke \
  -- --ignored --exact --nocapture

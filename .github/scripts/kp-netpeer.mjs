// The browser side of the browser<->native WebRTC test
// (net-experiments/m2-transport.md): runs the `replay` build of
// kernel-panic (bound with `wasm-bindgen --target web`, as for
// kp-replay.mjs) in a headless browser through Playwright, where its
// `kp_net_peer` export joins a room on a signaling server and plays one
// peer of a networked headless match against whoever else is in it,
// then writes that peer's hash trail.
//
// Started by `net::matchbox_tests::browser_peer`, which hosts the
// signaling server and the native peer and compares the trails:
//
//   KP_NETPEER_PKG=pkg cargo test -p kernel-panic browser_peer -- --ignored --nocapture
//
// Usage:
//   node kp-netpeer.mjs <runtime> <pkg dir> <maps dir> <signaling url> <room>
//                       <map> <humans> <ai> <seed> <secs> <out.trail>
//
// <map> "probe": no match, the transport probe `kp_net_probe` instead
// (`net::matchbox_tests::browser_probe`); the other arguments but the
// signaling URL, room and secs are ignored.
//
// KP_NETPEER_ICE: the browser's ICE servers (`KP_ICE` syntax, e.g.
// `stun:127.0.0.1:3478`); unset or empty: matchbox's default, Google
// STUN, whose binding requests may take ~40 s to time out where it is
// unreachable (the test hands in a local STUN server).
//
// <runtime>: chromium | firefox | webkit (WebRTC needs a browser; Node
// has none). Playwright and the KP_BROWSER_* knobs as in kp-browser.mjs.

import { writeFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { guardPage, launch, serve } from "./kp-browser.mjs";

const [runtime, pkgDir, mapsDir, signaling, room, map, humans, ai, seed, secs, outPath] =
  process.argv.slice(2);
if (!outPath) {
  console.error(
    "usage: kp-netpeer.mjs <runtime> <pkg> <maps> <signaling> <room> <map> <humans> <ai> <seed> <secs> <out>",
  );
  process.exit(2);
}

const server = await serve({
  "/index.html": "<!doctype html><meta charset=utf-8><title>kp-netpeer</title>",
  "/kernel-panic.js": resolve(pkgDir, "kernel-panic.js"),
  "/kernel-panic_bg.wasm": resolve(pkgDir, "kernel-panic_bg.wasm"),
  "/map.kpmap": resolve(mapsDir, `${map}.kpmap`),
});
const base = `http://127.0.0.1:${server.address().port}`;
// Chromium hides host candidates behind mDNS names (`<uuid>.local`)
// that a native webrtc-rs peer may not resolve in a CI container; show
// the real addresses instead.
const args = runtime === "chromium" ? ["--disable-features=WebRtcHideLocalIpsWithMdns"] : [];
const browser = await launch(runtime, { args });
let trail;
try {
  const page = await browser.newPage();
  const guarded = guardPage(page, runtime);
  await page.goto(`${base}/index.html`);
  const t0 = Date.now();
  const ice = process.env.KP_NETPEER_ICE ?? "";
  const run = page.evaluate(
    async ([signaling, ice, room, map, humans, ai, seed, secs]) => {
      const kp = await import("/kernel-panic.js");
      await kp.default({ module_or_path: "/kernel-panic_bg.wasm" });
      if (map === "probe") return await kp.kp_net_probe(signaling, ice, room, secs);
      const bytes = new Uint8Array(await (await fetch("/map.kpmap")).arrayBuffer());
      return await kp.kp_net_peer(signaling, ice, room, map, bytes, humans, ai, seed, secs);
    },
    [signaling, ice, room, map, Number(humans), Number(ai), Number(seed), Number(secs)],
  );
  trail = await guarded(run);
  const ticks = trail.split("\n").filter((l) => l).length;
  console.log(`  ${runtime}: ${ticks} ticks, ${((Date.now() - t0) / 1000).toFixed(1)} s wall`);
} finally {
  await browser.close();
  server.close();
}
await mkdir(dirname(resolve(outPath)) || ".", { recursive: true });
await writeFile(outPath, trail);

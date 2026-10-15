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
// <runtime>: chromium | firefox | webkit (WebRTC needs a browser; Node
// has none). Needs the `playwright` package resolvable from the working
// directory (or NODE_PATH) and the browser installed
// (`npx playwright install --with-deps <runtime>`, or KP_BROWSER_PATH).

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, extname, resolve } from "node:path";
import { createRequire } from "node:module";

const [runtime, pkgDir, mapsDir, signaling, room, map, humans, ai, seed, secs, outPath] =
  process.argv.slice(2);
if (!outPath) {
  console.error(
    "usage: kp-netpeer.mjs <runtime> <pkg> <maps> <signaling> <room> <map> <humans> <ai> <seed> <secs> <out>",
  );
  process.exit(2);
}

function serve(files) {
  const types = { ".js": "text/javascript", ".wasm": "application/wasm", ".html": "text/html" };
  const server = createServer(async (req, res) => {
    const path = new URL(req.url, "http://x").pathname;
    const file = files[path];
    if (file === undefined) {
      res.writeHead(404).end();
      return;
    }
    try {
      const body = file.startsWith("<!") ? file : await readFile(file);
      res.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" });
      res.end(body);
    } catch (e) {
      res.writeHead(500).end(String(e));
    }
  });
  return new Promise((ok) => server.listen(0, "127.0.0.1", () => ok(server)));
}

const require = createRequire(resolve(process.cwd(), "noop.js"));
const { chromium, firefox, webkit } = require("playwright");
const engine = { chromium, firefox, webkit }[runtime];
if (!engine) throw new Error(`unknown runtime ${runtime}`);

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
// KP_BROWSER_PATH: a browser binary of the engine's kind instead of
// Playwright's own download (e.g. a system Chromium).
const executablePath = process.env.KP_BROWSER_PATH || undefined;
const browser = await engine.launch({ headless: true, args, executablePath });
let trail;
try {
  const page = await browser.newPage();
  page.on("console", (m) => console.error(`[${runtime}] ${m.text()}`));
  page.on("pageerror", (e) => console.error(`[${runtime}] page error: ${e}`));
  await page.goto(`${base}/index.html`);
  const t0 = Date.now();
  trail = await page.evaluate(
    async ([signaling, room, map, humans, ai, seed, secs]) => {
      const kp = await import("/kernel-panic.js");
      await kp.default({ module_or_path: "/kernel-panic_bg.wasm" });
      const bytes = new Uint8Array(await (await fetch("/map.kpmap")).arrayBuffer());
      return await kp.kp_net_peer(signaling, room, map, bytes, humans, ai, seed, secs);
    },
    [signaling, room, map, Number(humans), Number(ai), Number(seed), Number(secs)],
  );
  const ticks = trail.split("\n").filter((l) => l).length;
  console.log(`  ${runtime}: ${ticks} ticks, ${((Date.now() - t0) / 1000).toFixed(1)} s wall`);
} finally {
  await browser.close();
  server.close();
}
await mkdir(dirname(resolve(outPath)) || ".", { recursive: true });
await writeFile(outPath, trail);

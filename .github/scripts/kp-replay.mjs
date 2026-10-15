// Golden replay on wasm32 (docs/multiplayer-plan.md M0.4): runs the
// `replay` build of kernel-panic (`headless::web`, bound with
// `wasm-bindgen --target web`) in Node or in a headless browser through
// Playwright, and writes the hash trail.
//
// Usage:
//   node kp-replay.mjs <runtime> <pkg dir> <maps dir> <log.kpreplay> <out.trail> [ticks]
//
// <runtime>: node | chromium | firefox | webkit. <pkg dir> holds
// kernel-panic.js + kernel-panic_bg.wasm; <maps dir> the baked
// <map>.kpmap files. ticks: 0 or absent = the whole recording. One log
// per invocation: every replay gets a fresh wasm instance (Node) or a
// fresh page (browsers).
//
// KP_DUMP_AT=<t1,t2…>: also write readable unit dumps at those ticks to
// <out>.dumps (bisecting a divergence against the native --dump-at).
//
// KP_WEBKIT_CONCURRENT_JIT=1: WebKit with JavaScriptCore's concurrent
// JIT (off by default here, see `runBrowser`).
//
// A panic, page error or crashed page ends the run with exit status 1
// at once.
//
// Browsers need the `playwright` package resolvable from the working
// directory (or NODE_PATH) and the browser installed
// (`npx playwright install --with-deps <runtime>`).

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { createServer } from "node:http";
import { dirname, extname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";

const [runtime, pkgDir, mapsDir, logPath, outPath, ticksArg] = process.argv.slice(2);
if (!runtime || !pkgDir || !mapsDir || !logPath || !outPath) {
  console.error("usage: kp-replay.mjs <node|chromium|firefox|webkit> <pkg> <maps> <log> <out> [ticks]");
  process.exit(2);
}
const ticks = Number(ticksArg ?? 0) >>> 0;
const dumpAt = new Uint32Array(
  (process.env.KP_DUMP_AT ?? "").split(",").filter((t) => t.trim() !== "").map(Number),
);
const pkgJs = resolve(pkgDir, "kernel-panic.js");
const pkgWasm = resolve(pkgDir, "kernel-panic_bg.wasm");

async function runNode() {
  const kp = await import(pathToFileURL(pkgJs).href);
  kp.initSync({ module: await readFile(pkgWasm) });
  const log = await readFile(logPath);
  const map = kp.kp_replay_map(log);
  const mapBytes = await readFile(resolve(mapsDir, `${map}.kpmap`));
  const t0 = performance.now();
  const [trail, summary, dumps] = await kp.kp_replay(log, mapBytes, ticks, dumpAt);
  return { trail, summary, dumps, wall: (performance.now() - t0) / 1000, ua: `node ${process.version}` };
}

// A static server over the few files the page needs.
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
      const body = typeof file === "string" && file.startsWith("<!") ? file : await readFile(file);
      res.writeHead(200, { "content-type": types[extname(path)] ?? "application/octet-stream" });
      res.end(body);
    } catch (e) {
      res.writeHead(500).end(String(e));
    }
  });
  return new Promise((ok) => server.listen(0, "127.0.0.1", () => ok(server)));
}

async function runBrowser() {
  const require = createRequire(resolve(process.cwd(), "noop.js"));
  const { chromium, firefox, webkit } = require("playwright");
  const engine = { chromium, firefox, webkit }[runtime];
  if (!engine) throw new Error(`unknown runtime ${runtime}`);

  const log = await readFile(logPath);
  // The map name is in the log; ask the wasm (Node-side) for it.
  const kp = await import(pathToFileURL(pkgJs).href);
  kp.initSync({ module: await readFile(pkgWasm) });
  const map = kp.kp_replay_map(log);

  const server = await serve({
    "/index.html": "<!doctype html><meta charset=utf-8><title>kp-replay</title>",
    "/kernel-panic.js": pkgJs,
    "/kernel-panic_bg.wasm": pkgWasm,
    "/log.kpreplay": resolve(logPath),
    "/map.kpmap": resolve(mapsDir, `${map}.kpmap`),
  });
  const base = `http://127.0.0.1:${server.address().port}`;
  // WebKit: JavaScriptCore compiles on the main thread. Its concurrent
  // IPInt -> BBQ tier-up corrupts the replay's linear memory now and then
  // (panics in bevy_ecs or dlmalloc, out-of-bounds traps, crashes, hangs;
  // ~5 % of pages, gone with this or without BBQ, which is ~14x slower:
  // net-experiments/m3-ci-flakes.md). Every tier still runs, at ~1.3x the
  // wall time. KP_WEBKIT_CONCURRENT_JIT=1: JSC's default.
  const env = { ...process.env };
  if (runtime === "webkit" && process.env.KP_WEBKIT_CONCURRENT_JIT !== "1") {
    env.JSC_useConcurrentJIT = "false";
  }
  const browser = await engine.launch({ headless: true, env });
  try {
    const page = await browser.newPage();
    // A panic inside the replay (a Rust panic is a wasm trap, thrown from
    // whatever callback was running: a timer, a promise reaction) never
    // settles `kp_replay`'s promise, so `evaluate` would wait forever
    // (CI sat 30 min to 4 h); the panic message, the page error or a
    // crashed page ends the run at once instead.
    const failed = new Promise((_, reject) => {
      page.on("console", (m) => {
        if (/^(panicked at|panic: )/.test(m.text())) reject(new Error(`${runtime}: the replay panicked`));
      });
      page.on("pageerror", (e) => reject(new Error(`${runtime}: page error: ${e}`)));
      page.on("crash", () => reject(new Error(`${runtime}: the page crashed`)));
    });
    failed.catch(() => {});
    page.on("console", (m) => console.error(`[${runtime}] ${m.text()}`));
    page.on("pageerror", (e) => console.error(`[${runtime}] page error: ${e.stack ?? e}`));
    await page.goto(`${base}/index.html`);
    const replay = page.evaluate(async ([ticks, dumpAt]) => {
      const kp = await import("/kernel-panic.js");
      await kp.default({ module_or_path: "/kernel-panic_bg.wasm" });
      const bytes = async (u) => new Uint8Array(await (await fetch(u)).arrayBuffer());
      const [log, map] = await Promise.all([bytes("/log.kpreplay"), bytes("/map.kpmap")]);
      const t0 = performance.now();
      const [trail, summary, dumps] = await kp.kp_replay(log, map, ticks, new Uint32Array(dumpAt));
      return { trail, summary, dumps, wall: (performance.now() - t0) / 1000, ua: navigator.userAgent };
    }, [ticks, Array.from(dumpAt)]);
    replay.catch(() => {}); // rejects again once the browser closes
    return await Promise.race([replay, failed]);
  } finally {
    await browser.close();
    server.close();
  }
}

const result = runtime === "node" ? await runNode() : await runBrowser();
await mkdir(dirname(resolve(outPath)) || ".", { recursive: true });
await writeFile(outPath, result.trail);
if (result.dumps) await writeFile(`${outPath}.dumps`, result.dumps);
console.log(`${result.summary}`);
console.log(`  ${runtime} (${result.ua}): ${result.wall.toFixed(1)} s wall; trail ${outPath}`);

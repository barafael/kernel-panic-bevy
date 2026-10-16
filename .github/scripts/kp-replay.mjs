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
// A panic, page error or crashed page ends the run with exit status 1
// at once.
//
// Browsers: Playwright and the KP_BROWSER_* / KP_WEBKIT_CONCURRENT_JIT
// knobs as in kp-browser.mjs.

import { readFile, writeFile, mkdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { guardPage, launch, serve } from "./kp-browser.mjs";

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

async function runBrowser() {
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
  const browser = await launch(runtime);
  try {
    const page = await browser.newPage();
    const guarded = guardPage(page, runtime);
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
    return await guarded(replay);
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

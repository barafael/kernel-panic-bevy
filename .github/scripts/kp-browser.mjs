// What the browser runners (kp-replay.mjs, kp-netpeer.mjs) share: the
// static server for the page, the Playwright browser launch and the
// page guard that ends a run on a panic or crash.
//
// KP_BROWSER_ARGS: more browser flags, space separated or a JSON array
// (for flags with spaces, e.g. `--host-resolver-rules=MAP a b`).
// KP_BROWSER_PATH: a browser binary of the engine's kind instead of
// Playwright's own download (e.g. a system Chromium).
// KP_WEBKIT_CONCURRENT_JIT=1: WebKit with JavaScriptCore's concurrent
// JIT (off by default, see `launch`).
//
// Needs the `playwright` package resolvable from the working directory
// (or NODE_PATH) and the browser installed
// (`npx playwright install --with-deps <runtime>`).

import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { extname, resolve } from "node:path";
import { createRequire } from "node:module";

// A static server over the few files the page needs: URL path -> file
// path, or the body itself for an HTML page (`<!doctype html>…`).
export function serve(files) {
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

// A headless browser of `runtime` (chromium | firefox | webkit), with
// `args` and KP_BROWSER_ARGS as its flags.
export async function launch(runtime, { args = [] } = {}) {
  const require = createRequire(resolve(process.cwd(), "noop.js"));
  const { chromium, firefox, webkit } = require("playwright");
  const engine = { chromium, firefox, webkit }[runtime];
  if (!engine) throw new Error(`unknown runtime ${runtime}`);
  const extra = (process.env.KP_BROWSER_ARGS ?? "").trim();
  args = [...args, ...(extra.startsWith("[") ? JSON.parse(extra) : extra.split(" ").filter((a) => a))];
  const executablePath = process.env.KP_BROWSER_PATH || undefined;
  // WebKit: JavaScriptCore compiles on the main thread. Its concurrent
  // IPInt -> BBQ tier-up corrupts our wasm's linear memory now and then
  // (panics in bevy_ecs or dlmalloc, out-of-bounds traps, crashes, hangs;
  // ~5 % of replay pages, gone with this or without BBQ, which is ~14x
  // slower: net-experiments/m3-ci-flakes.md). Every tier still runs, at
  // ~1.3x the wall time. KP_WEBKIT_CONCURRENT_JIT=1: JSC's default.
  const env = { ...process.env };
  if (runtime === "webkit" && process.env.KP_WEBKIT_CONCURRENT_JIT !== "1") {
    env.JSC_useConcurrentJIT = "false";
  }
  return await engine.launch({ headless: true, args, executablePath, env });
}

// Echoes the page's console and errors to stderr, and returns
// `guarded(work)`: `work` (the page's `evaluate` promise), or a
// rejection as soon as the page throws or crashes. A Rust panic is a
// wasm trap, thrown from whatever callback was running (a timer, a
// promise reaction), which may never settle the export's promise, so
// `evaluate` would wait forever (CI sat 30 min to 4 h); the panic hook
// (`headless::web`) throws it again as an uncaught error, a page error.
export function guardPage(page, runtime) {
  const failed = new Promise((_, reject) => {
    page.on("pageerror", (e) => reject(new Error(`${runtime}: page error: ${e}`)));
    page.on("crash", () => reject(new Error(`${runtime}: the page crashed`)));
  });
  failed.catch(() => {});
  page.on("console", (m) => console.error(`[${runtime}] ${m.text()}`));
  page.on("pageerror", (e) => console.error(`[${runtime}] page error: ${e.stack ?? e}`));
  return (work) => {
    work.catch(() => {}); // rejects again once the browser closes
    return Promise.race([work, failed]);
  };
}

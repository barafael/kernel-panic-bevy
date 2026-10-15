// The wasm copy micro-benchmark of `net-experiments/m3-speed.md` §2 in a
// browser started by hand (no automation attached), or in Playwright's
// Firefox:
//
//   wasm-as --enable-bulk-memory copy.wat -o copy.wasm   (binaryen)
//   node stock.mjs <browser binary> copy.wasm            a stock browser, headless
//   node stock.mjs - copy.wasm pw-firefox                Playwright's Firefox, page.goto
//   JSBENCH=1 node stock.mjs <binary> copy.wasm          a JS loop instead
//   USERJS='user_pref("…", true);' node stock.mjs …      Firefox prefs for the run
//
// The page runs by itself and POSTs its numbers back. The same page with
// the golden replay (kp-replay's page posting its summary) gave §2's
// per-tick table.
import { readFile, mkdtemp, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { createRequire } from "node:module";

const [bin, wasmPath, how] = process.argv.slice(2);
const wasm = await readFile(wasmPath);
const post = (expr) => `await fetch("/done", { method: "POST", body: navigator.userAgent + "\\n" + ${expr} });`;
const page = process.env.JSBENCH
  ? `<!doctype html><meta charset=utf-8><script type=module>
function f(n) { let a = new Float64Array(1024), s = 0; for (let i = 0; i < n; i++) { a[i & 1023] = a[(i * 7) & 1023] + i * 0.5; s += a[i & 1023]; } return s; }
f(100000); const t0 = performance.now(); f(50000000);
${post(`"js loop: " + ((performance.now() - t0) * 1e6 / 50000000).toFixed(2) + " ns/iter"`)}
</script>`
  : `<!doctype html><meta charset=utf-8><script type=module>
const { instance } = await WebAssembly.instantiate(await (await fetch("/copy.wasm")).arrayBuffer());
const e = instance.exports, out = [], N = 20000000;
for (const len of [8, 16, 64, 256]) for (const f of ["bulk_copy", "bulk_fill", "loop_copy"]) {
  e[f](100000, len); const t0 = performance.now(); e[f](N, len);
  out.push(f + " len " + len + ": " + ((performance.now() - t0) * 1e6 / N).toFixed(2) + " ns");
}
${post(`out.join("\\n")`)}
</script>`;
let done;
const fin = new Promise((ok) => (done = ok));
const server = createServer((req, res) => {
  if (req.url === "/done") {
    let b = "";
    req.on("data", (c) => (b += c));
    req.on("end", () => {
      res.end();
      done(b);
    });
  } else if (req.url === "/copy.wasm") {
    res.writeHead(200, { "content-type": "application/wasm" }).end(wasm);
  } else {
    res.writeHead(200, { "content-type": "text/html" }).end(page);
  }
});
await new Promise((ok) => server.listen(0, "127.0.0.1", ok));
const url = `http://127.0.0.1:${server.address().port}/`;
if (how === "pw-firefox") {
  const require = createRequire(resolve(process.cwd(), "noop.js"));
  const b = await require("playwright").firefox.launch({ headless: true });
  await (await b.newPage()).goto(url);
  console.log(await fin);
  await b.close();
} else {
  const prof = await mkdtemp(resolve(tmpdir(), "kpbench-"));
  if (process.env.USERJS) await writeFile(resolve(prof, "user.js"), process.env.USERJS);
  const args = bin.includes("firefox")
    ? ["--headless", "--no-remote", "--profile", prof, url]
    : ["--headless=new", `--user-data-dir=${prof}`, "--no-first-run", url];
  const child = spawn(bin, args, { stdio: "ignore" });
  console.log(await fin);
  child.kill("SIGTERM");
}
server.close();

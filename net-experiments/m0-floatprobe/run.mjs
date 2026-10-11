// Run the probe's wasm32 build under Node and write the same bit stream
// as `floatprobe run`: node run.mjs <m0_floatprobe.wasm> <out.bin>
import fs from 'fs';

const [wasmPath, out] = process.argv.slice(2);
const { instance } = await WebAssembly.instantiate(fs.readFileSync(wasmPath), {});
const { probe, probe_count, probe_n } = instance.exports;
const F = probe_count(), N = probe_n();
const buf = Buffer.alloc(F * N * 8);
let k = 0;
for (let f = 0; f < F; f++) {
  for (let i = 0; i < N; i++) {
    // i64 results arrive as BigInt (signed); store the raw 64 bits.
    buf.writeBigUInt64LE(BigInt.asUintN(64, probe(f, i)), k);
    k += 8;
  }
}
fs.writeFileSync(out, buf);

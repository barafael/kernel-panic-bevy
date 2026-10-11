import fs from 'fs';
const [wasmPath, out] = process.argv.slice(2);
const { instance } = await WebAssembly.instantiate(fs.readFileSync(wasmPath), {});
const N = 200000, F = 16;
const buf = Buffer.alloc(N * F * 4);
let k = 0;
for (let f = 0; f < F; f++) for (let i = 0; i < N; i++) { buf.writeUInt32LE(instance.exports.probe(f, i) >>> 0, k); k += 4; }
fs.writeFileSync(out, buf);

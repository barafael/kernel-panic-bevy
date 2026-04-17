# Web / WASM Asset Pipeline Plan

Goal: make a wasm build of kernel-panic possible by replacing legacy Spring-engine asset formats and runtime filesystem access with web-friendly formats served over HTTP.

## What's in the way of wasm today

The current loader paths assume native filesystem access in three places:

1. **`std::fs::read_dir` at startup**
   - [main.rs:114](kernel-panic/src/main.rs#L114) — map discovery scans `assets/maps/` for `.sd7`/`.sdz`.
   - [tdf_loader.rs:40](kernel-panic/src/units/tdf_loader.rs#L40) — loads every `.fbi`/`.tdf` from `upstream/Kernel-Panic/`.
   - [meshes.rs:148](kernel-panic/src/units/meshes.rs#L148) — path-search fallback for `.s3o` / `.tga` / `.cob`.
   Wasm has no filesystem; `read_dir` cannot be polyfilled.

2. **`.sd7` / `.sdz` archive extraction at runtime**
   - [spring-map/src/sd7_archive.rs](spring-map/src/sd7_archive.rs) reads the whole archive into memory and decompresses on the main thread.
   - 7z decompression is CPU-heavy — bad for wasm startup, blocks the main thread, defeats HTTP/2 multiplexing and the browser cache.

3. **Custom binary formats with no GPU/web friendliness**
   - `.tga` is uncompressed (huge over the wire, no GPU compression).
   - `.s3o` carries no quantization or meshopt.
   - `.cob` is a Total Annihilation bytecode interpreter — code we'd ship in the wasm binary for animations that glTF + ECS does natively.

## Recommended target formats (from SOTA research, April 2026)

| Asset | Today | Target | Why |
|---|---|---|---|
| Unit/weapon defs | `.fbi` / `.tdf` text | **RON in repo → postcard at runtime** (via Bevy asset processor) | Hand-editable source, fast parse, no parser shipped to wasm |
| 3D models | `.s3o` + raw `.tga` | **`.glb` + EXT_meshopt_compression + KHR_mesh_quantization** | Bevy-native loader, named nodes preserve COB piece hooks, ~10× smaller than raw, decoder is 20 KB |
| Textures (in models) | `.tga` (raw, paletted) | **KTX2 + Basis ETC1S** for diffuse, UASTC for normals, mipmapped | GPU-ready, transcodes to BC7/ASTC/ETC2 per-backend, Bevy 0.18 has the loader |
| UI buildpics (~64×64) | `.png` | keep `.png` (or WebP — marginal) | Absolute bytes saved are negligible at this size |
| Audio | `.wav` | **`.ogg` Vorbis** | bevy_audio default; rodio MP3 broken on wasm; Opus needs custom decoder |
| Maps | `.sd7` / `.sdz` (7z/zip + SMF/SMT/SMD/Lua) | **per-file fetch**: `.glb` for meshes/features, KTX2 for tilesets, RON for metadata, raw `.bin` for the heightmap | Kills runtime decompression; HTTP/2 multiplexes the small fetches; browser cache works |
| Animations | `.cob` bytecode | **glTF skeletal clips + Rust ECS state machines** | Drops the COB VM from the wasm binary; matches modern Bevy idiom |
| Archive / discovery | `read_dir` | **`assets.ron` manifest** listing every asset | Replaces directory walking with one initial fetch |
| Lua map gadgets | `mlua` interpreter | **compile to Rust** (game has only ~1 active gadget) | Drops Lua runtime from wasm bundle |

Transport: serve individual files over HTTP/2 with content-hashed names (`units/arm_peewee.a1b2c3.glb`) and `Cache-Control: immutable`. Don't double-compress KTX2/glb. Let the server do brotli on RON / glTF JSON / manifest.

Bevy plumbing: `bevy_web_asset` (upstreamed in Bevy 0.17) gives an HTTP `AssetReader` — set `AssetMetaCheck::Never` (meta files panic on wasm).

## Plan of attack

Build everything as one new workspace crate, **`kernel-panic-asset-pipeline`**, with one binary per converter. Source-of-truth stays in `upstream/Kernel-Panic/`; converters write a parallel `kernel-panic/assets/processed/` tree plus `assets.ron`. The runtime reads from `assets/processed/` only. Legacy paths stay behind a `legacy-assets` cargo feature for desktop development during the transition.

Ordered by wasm impact:

1. **`tdf-to-ron`** — converts every `.fbi` and `.tdf` to RON using the existing typed `spring-tdf` parsers (`UnitDefs`, `WeaponDefs`, `ExplosionDefs`). Replaces `read_dir` + per-file text parsing with `AssetServer::load("units/manifest.ron")`. Cleanest, no new deps.

2. **Manifest generator** — kills the remaining `read_dir` calls (maps, units, weapons, models). One RON file fetched first.

3. **`s3o-to-glb`** — converts `.s3o` hierarchical models to glTF with named nodes preserved (the COB animation hooks). Optional meshopt compression behind a flag. Brings in the `gltf` crate.

4. **`tga-to-ktx2`** — converts `.tga` textures to KTX2 / Basis. Needs `basis-universal` or `toktx` CLI. Acceptable fallback for v1: plain PNG (small unit textures).

5. **`map-extract`** — splits `.sd7` / `.sdz` into individual files at build time: heightmap `.bin`, tileset KTX2, RON metadata. Removes the `sevenz-rust` and `zip` runtime deps.

6. **Wire up the runtime** — add Bevy `WebAssetPlugin`, switch loaders to the new files, gate legacy paths behind `legacy-assets`.

7. **Long tail**: COB → glTF animations + ECS state machines. Deeper refactor — separate effort.

## First session scope

Steps 1–3 fit one focused block of work. They're the highest-impact wins (kill the directory walks, unblock per-file HTTP fetches for the bulk of assets) and don't need external CLIs or deeper refactors.

Skip for follow-ups:
- KTX2 (needs `basis-universal` toolchain decision)
- Map extraction (needs heightmap-format decision and the Lua-gadget rewrite)
- COB rewrite (needs animation system design)

## Sources

- [Bevy 0.18 release notes](https://bevy.org/news/bevy-0-18/)
- [KTX2 updates PR #18411](https://github.com/bevyengine/bevy/pull/18411)
- [Don McCurdy — Choosing texture formats for WebGL and WebGPU](https://www.donmccurdy.com/2024/02/11/web-texture-formats/)
- [Khronos Asset Creation Guidelines 2.0](https://www.khronos.org/blog/introducing-asset-creation-guidelines-2.0-siggraph-2025)
- [meshoptimizer / gltfpack](https://meshoptimizer.org/gltf/)
- [bevy_web_asset (upstreamed in 0.17)](https://github.com/johanhelsing/bevy_web_asset)
- [bevy_common_assets](https://github.com/NiklasEi/bevy_common_assets)
- [Bevy Cheatbook — WASM size optimization](https://bevy-cheatbook.github.io/platforms/wasm/size-opt.html)
- [postcard 1.0 (James Munns)](https://jamesmunns.com/blog/postcard-1-0-run/)
- [Bevy PR #3421 — OGG default over MP3](https://github.com/bevyengine/bevy/pull/3421)

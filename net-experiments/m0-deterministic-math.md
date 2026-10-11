# M0.3 — Deterministic math

Goal (docs/multiplayer-plan.md, "Will different hardware compute the same
results?"): every float that feeds the simulation is bit-identical on Linux,
Windows and macOS, x86_64 and arm64, and wasm32 in every browser, without
fixed-point.

Machine: AMD Ryzen 7 7840HS, Linux, rustc nightly 2026-10-09. The machine was
shared with other builds during the measurements (load average 30–60), so
timings carry several percent of noise.

## What changed

**Build configuration** (workspace `Cargo.toml`):

- bevy's `libm` feature → `bevy_math/libm`, `glam/libm`, and `libm` for
  `bevy_color`, `bevy_input`, `bevy_transform`, `bevy_window`.
  `bevy::math::ops::*` and glam's own `sin cos atan2 powf exp ...` call the
  pure-Rust `libm` crate (0.2.16) instead of the platform C library.
- glam `scalar-math`: Vec3A/Vec4/Quat/Mat3A/Mat4/Affine3A use the scalar code
  on every target, instead of SSE2 (x86_64), NEON (arm64) or scalar (wasm32
  without simd128). Vec3A/Mat3A/Affine3A keep their 16-byte alignment and
  size; Vec4/Quat/Mat2/Mat4 drop to 4-byte alignment (same size). Nothing in
  the game or Bevy relied on that alignment (the full test suite passes).
- `kernel-panic` (and the optional map viewer) depend on glam directly only
  so that Cargo's feature unification turns both features on for Bevy's copy.
- `cargo tree -e features` shows `glam feature "libm"`, `"scalar-math"` and
  `bevy_math feature "libm"` for x86_64-linux, wasm32, aarch64-apple-darwin
  and x86_64-windows-msvc; there is exactly one glam (0.33.12) in the tree.

**Call sites** routed through `bevy::math::ops` (game, f32), `libm::*` (the
crates without Bevy, and all f64), or `x * x` (`sim::sq`, local `sq` helpers)
for `powi(2)`. The first clippy run flagged 195 sites; 182 are converted, 13 are
offline/presentation code with an allow (below):

| crate / module | sites | notes |
|---|---|---|
| spring-map `hexfarm.rs` | 52 | Hex Farm generator (f64): sin/cos/atan2/tan/pow + 30 `powi(2)` |
| spring-map `smooth_mesh.rs` | 2 | test data only |
| kernel-panic `units/` | 43 | combat aim, missile flight, spawning rings, AI army spread, CEG spawns, beams (+2 left on std, below) |
| kernel-panic `interaction/` | 29 | ground movement (avoidance cosine, mass radius `cbrt`), formations, air bank |
| kernel-panic `ui/`, `rendering/`, `showcase.rs`, `bot.rs` | 21 | render/UI/dev: converted anyway, cost is nil |
| kernel-panic `map_loading/lua_compositing.rs` | 8 | Hex Farm tower geometry (`powi(4)` → `sq(sq(x))`) |
| kernel-panic `sim.rs` | 7 | `dir_of`, `heading_of`: every unit's heading |
| kernel-panic `map_events/` | 4 | Stack Overflow eruption ring, Hex Farm |
| spring-pathfinding | 12 | `max_slope_from_degrees` (cos), qtpfs `dist` |
| spring-tdf | 4 | `spray_sin`, dynamic-damage `powf`, `maxangledif1` cos |

`powi(2)` → `x * x` is bit-identical to what LLVM emits for it today; `powi(4)`
→ `sq(sq(x))` likewise (LLVM's repeated squaring). The other routes change
results by at most an ulp, so gameplay is unchanged apart from the butterfly
effect of those last bits (the unit tests' tolerances all hold).

Not converted, with a targeted `#[allow(clippy::disallowed_methods)]` and a
comment: the offline map generators `gen-circular-buffer` and
`gen-stack-overflow` (their outputs are the committed `.sdz`), the
spring-map-gen tests, the map viewer, the `kpmap_bench` example, and the CEG
particle update (`tick_ceg_particles`: presentation only, two `powf` per
particle per tick, where glibc's `powf` is ~5× faster than libm's).

**Load-time math counts as sim.** Native players load `.sd7`/`.sdz` directly
(or a locally baked `.kpmap`); the web build loads `.kpmap` baked on the
deploy runner. Both run the same Rust code (`spring_map::load_map` →
heightmap decode, Lua-gadget ports, Hex Farm generation), so `spring-map` is
converted wholesale and the bake on CI produces the same heights a Windows or
macOS player computes from the archive. The `.kpmap` decode itself is integer
and byte work (postcard/zstd) — identical everywhere. The remaining load-time
float work (SMF height scaling, speed maps from slopes, smooth ground,
normals) is `+ − × ÷ sqrt` only. Unit definitions are baked into the committed
`units.kpu`, so their derived floats are identical by construction.

**Enforcement**: workspace `clippy.toml` `disallowed-methods` bans
`f32::`/`f64::` `sin cos tan asin acos atan atan2 sin_cos exp exp2 exp_m1 ln
ln_1p log log2 log10 powf hypot cbrt sinh cosh tanh asinh acosh atanh`, plus
`powi` and `mul_add`. `cargo clippy --workspace --all-targets -- -D warnings`
and `cargo clippy --target wasm32-unknown-unknown -p kernel-panic` are clean.
`sim::tests::deterministic_math_is_configured` fails if either feature is
dropped (it compares `ops`/glam against `libm` bit for bit and checks glam's
scalar alignment).

**FPU mode check**: `sim::assert_float_env` runs at `FixedFirst` in debug
builds: on x86_64 it asserts MXCSR has FTZ, DAZ off and round-to-nearest; on
arm64 FPCR FZ, FZ16 off and RMode nearest. One register read per tick.

## Flag audit

- No `.cargo/config.toml` in the repo; the local `~/.cargo/config.toml` only
  sets the mold linker for x86_64-linux. No `target-cpu`, `target-feature` or
  fast-math anywhere. Rust has no fast-math flag and never contracts
  `a * b + c` into an FMA (unlike GCC, which is why Spring needs
  `-ffp-contract=off`); only an explicit `mul_add` fuses, and there are none.
- Profiles: `release` (thin LTO, panic=abort), `wasm-release` (opt-level 2),
  `dev` (opt 1). Optimisation level and LTO do not change IEEE results.
- wasm32-unknown-unknown target features: bulk-memory, multivalue,
  mutable-globals, nontrapping-fptoint, reference-types, sign-ext — no
  simd128, no relaxed-simd (whose fused/unfused choice is up to the engine).
- Trunk: `data-wasm-opt="2"`. Binaryen keeps IEEE semantics unless given
  `--fast-math`; verified: the probe after `wasm-opt -O2` (binaryen 116) is
  bit-identical to the unoptimised wasm. The deploy workflow downloads
  "latest" trunk, which picks its own binaryen; pinning both is advisable for
  reproducible web builds (not a determinism risk by itself).

## Float probe

`net-experiments/m0-floatprobe/run-matrix.sh` builds the probe (same glam,
bevy_math and libm versions and features as the game) for every local target
and compares 200 000 inputs per function. Result: **every `det:` route is
bit-identical across all six builds** — x86_64 glibc, x86_64 musl,
x86_64 `-C target-cpu=native` (AVX-512/FMA host), wasm32 in Node,
wasm32 after `wasm-opt -O2`, and x86_64-pc-windows-msvc under Wine — covering
`ops::{sin cos tan sin_cos atan2 atan asin acos powf hypot cbrt exp ln}`,
f64 `libm::{sin cos atan2 pow tan}`, sqrt, `x*x`, `rem_euclid`, rounding, and
glam `Quat*Vec3`, `Vec3A::normalize`/`dot`, `Affine3A::transform_point3`,
`Mat4*Mat4`, `slerp`, `from_euler`, `Vec2::from_angle`/`to_angle`,
`angle_between`, `Mat3::from_axis_angle`.

Control group (the now-banned std methods), differing inputs of 200 000:

| function | glibc vs wasm32 | glibc vs musl | glibc vs MSVC-under-Wine |
|---|---|---|---|
| f32 sin / cos | 1.25 % / 1.26 % | 1.25 % / 1.26 % | 1.23 % / 1.28 % |
| f32 tan | 14.2 % | 14.2 % | 14.2 % |
| f32 atan2 | 16.4 % | 16.4 % | 16.4 % |
| f32 asin / acos | 4.9 % / 8.5 % | 4.9 % / 8.5 % | 6.4 % / 7.5 % |
| f32 powf | 9.6 % | 0 | 0 |
| f32 hypot | 12.5 % | 12.5 % | 12.5 % |
| f64 sin / atan2 / powf | 3.1 % / 17.3 % / 9.8 % | 3.1 % / 17.3 % / 0.08 % | 3.1 % / 17.3 % / 0.08 % |
| f32 cbrt, powi(2) | 0 | 0 | 0 |

(Wine's `ucrtbase` is Wine's own math, not Microsoft's; real Windows is
covered by the M0.4 CI matrix. arm64 likewise: not available locally.)

## Cost

Per operation (`glambench`, ns per call, median of 5; scalar-math vs glam's
SSE2 paths, and libm vs glibc):

| op | scalar | SSE2 | | op | libm | glibc |
|---|---|---|---|---|---|---|
| Mat4 × Mat4 | 3.5 | 3.4 | | sin | 7.9 | 7.0 |
| Affine3A × Affine3A | 2.7 | 3.0 | | sin_cos | 9.9 | 10.8 |
| Affine3A::transform_point3 | 3.0 | 3.3 | | atan2 | 24.8 | 20.3 |
| Mat4::project_point3 | 4.7 | 4.5 | | acos | 6.1 | 11.6 |
| Mat4 × Vec4 | 3.7 | 3.4 | | powf | 71 | 13 |
| Quat × Quat / × Vec3 | 2.6 / 5.8 | 3.8 / 6.9 | | sqrt | 2.2 | 2.3 |
| Vec3A::normalize | 3.5 | 3.7 | | floor | 6.1 | 5.7 |
| Mat4::inverse | 42 | 21 | | | | |

Scalar glam costs about the same as SSE2 glam for everything the renderer
does per entity (transform propagation, culling, uniform building); only
`Mat4::inverse` (a few per view per frame) is 2× slower. The renderer impact is
therefore negligible. On wasm32 nothing changes at all: glam was already scalar
there and std's transcendentals already were Rust's libm.

Sim cost, measured with the X-sim headless harness (branch `worktree-agent-a54879b76d9ed827f`,
`headless::tests::bandwidth`, single-threaded `FixedUpdate`, release), built
once as-is (A) and once with this change merged (B), runs alternated A/B/A/B.
Mean wall time per tick from `FixedFirst` to `FixedLast`:

| scenario | A runs | B runs | B vs A |
|---|---|---|---|
| 4v4 Speed Balls, 9 000 ticks, ≤119 units, 3 rounds | 1.32 / 1.23 / 1.42 ms | 1.21 / 1.35 / 1.32 ms | −2 % |
| FFA Quad Core 8 AIs (difficulty 4), 12 000 ticks, ≤453 units, 2 rounds | 4.45 / 3.48 ms | 4.11 / 3.16 ms | −8 % |

Per unit-count bucket the B/A ratio scatters between 0.76 and 1.15. Run-to-run
noise on this shared machine is ±10–30 % (A against A), so the honest result
is **no measurable change in sim cost**: libm's transcendentals cost a few ns
each and the sim calls them a few thousand times per tick at most; the only
expensive one, `powf` (+58 ns), runs once per CEG spawn and beam, and the hot
per-particle `powf` stays on the platform version. (Absolute numbers are 2–4×
X-sim's because of the machine load, not the change.) Each variant reproduced
itself exactly across processes (identical per-tick unit/projectile/snapshot
series); A and B diverge from each other after tick 5 413 (Speed Balls) and
1 359 (Quad Core) — the butterfly effect of last-ulp differences, as expected.

## Still open / worries

- **`f32::min`/`max` with ±0.** IEEE leaves `max(+0, −0)` unspecified, and
  a sign-of-zero difference would flip `atan2(±0, x<0)` between ±π. The probe
  (`chk:` rows: `max`/`min`/`clamp` over ±0 and ±tiny, and
  `atan2(z.max(-0.0), -1)`) found 0 differences across all six builds, so
  LLVM's lowerings agree on x86_64 and wasm32. arm64 (`fmaxnm`) is for the
  CI matrix to confirm.
- **NaN payloads** differ on wasm (plan table); nothing hashes float bits yet,
  but the M0.4 state hash must canonicalise or assert no NaN.
- **arm64 and real Windows/macOS** are untested locally; the probe and the
  FPCR assertion are cfg-correct but only CI (M0.4) will run them.
- The FPU-mode assertion checks only the thread running `FixedFirst`; with
  the single-threaded `FixedUpdate` executor (decision 3) that is the whole sim.
- X-sim's harness (`kernel-panic/src/headless/encode.rs`) adds three std
  `atan2`/`asin` calls: clippy flags them when that branch is merged (they are
  snapshot encoding, not sim, so an `ops` route or an allow both work).
- Bevy's own crates follow the same rule (bevy uses `ops` internally under its
  own clippy config), and glam's methods are covered by its `libm` feature.
  Third-party crates that do their own float math outside glam would not be;
  none feeds the sim today.

# M0.4: the golden-replay determinism matrix

Goal (`docs/multiplayer-plan.md`, M0.4): the same match setup and command log give the same
per-tick state hash on every platform the game ships on: Linux (glibc, musl, arm64), Windows,
macOS (arm64 and x86_64 under Rosetta), and the web build in Node, Chromium, Firefox and
WebKit. This is the gate before any netcode.

Built on main after M0.2 (be6e54f). Local machine: AMD Ryzen 7 7840HS, Linux. CI: GitHub
Actions, `.github/workflows/determinism.yml`.

## Result

**The gate passes.** Every platform reproduces the Linux x86_64 (glibc) reference trail byte for
byte, every tick of every log, the query-order fuzz included:

| platform | runner | `4v4_speed_balls` (18 000) | `ffa8_quad_core` (18 000) | `commands_marble` (3 000) |
|---|---|---|---|---|
| linux-x86_64-gnu (reference) | ubuntu-latest | reference | reference | reference |
| linux-x86_64-gnu, storage shuffled every tick | ubuntu-latest | identical | identical | identical |
| linux-x86_64-musl | ubuntu-latest, Alpine container | identical | identical | identical |
| linux-arm64 | ubuntu-24.04-arm | identical | identical | identical |
| windows-x86_64 (MSVC) | windows-latest | identical | identical | identical |
| macos-arm64 | macos-latest | identical | identical | identical |
| macos-x86_64 under Rosetta 2 | macos-latest | identical | identical | identical |
| wasm32, Node 22 | ubuntu-latest | identical | identical | identical |
| wasm32, Chromium 141 (headless) | ubuntu-latest, Playwright | identical | identical | identical |
| wasm32, Firefox 142 (headless) | ubuntu-latest, Playwright | identical | identical | identical |
| wasm32, WebKit (Safari 26 engine) | ubuntu-latest, Playwright | identical | identical | identical |

Final hashes (tick 18 000 / 3 000): 4v4 `8818ce9ef996c648` (41 units), FFA `be0e5013ce7eccb1`
(152 units), commands `5e9abe71130ebe86` (25 units). No NaN anywhere. The browsers replay the
full logs (no shorter prefix was needed).

CI runs (branch `m0-determinism`, deleted afterwards):
- https://github.com/barafael/kernel-panic-bevy/actions/runs/38157446933 — the first run, all 19
  jobs green, every trail identical;
- FINAL_RUN

M0.3's float probe ran on the same runners: every `det:` row (the `libm` / `bevy::math::ops` /
scalar-glam paths the sim uses) is bit-identical on all seven targets. The `std:` rows show what
the platform libraries would have done instead, against glibc: Windows (MSVC CRT) `atan2` 16 %,
`cbrt` 29 %, `acos` 8 %; macOS arm64 `atan2` 17 %, `tan` 6 %; macOS x86_64 `acos` 20 %, `asin`
18 %, `f64::sin` 17 %; musl and wasm `tan` 14 %, `hypot` 13 %. One `chk:` row differs on both
arm64 targets: `f32::max`/`f32::min` with signed zeros (31 % of the probe's ±0 pairs return the
other zero; aarch64's `fmaxnm`/`fminnm` against x86's `maxss`/`minss`). The golden trails do not
hit it, but see Open.

## What was built

### Replay format: `.kpreplay` (`kernel-panic/src/headless/replay.rs`)

`b"kpreplay"`, the format version (u16 LE, now 1), then the postcard encoding of:

```rust
struct Replay {
    setup: MatchSetup,   // map stem, seats (faction, ally team, AI?), difficulty, seed, sandbox
    compat: Compat,      // SIM_VERSION, FNV-1a of the embedded units.kpu, FNV-1a of the baked .kpmap
    ticks: u32,          // length of the recording, the default replay length
    frames: Vec<CommandFrame>, // the human seats' frames, by tick then player
}
```

- **Recording** (`Replay::record`): from the `CommandBus` log of applied frames
  (`CommandBus::log`), keeping the frames of teams that `AiSeats::from_setup` does not give to
  the AI. The in-sim AI is part of the simulation, so an all-AI match records no frames at all
  (the 4v4 and FFA logs are 84 and 75 bytes).
- **Playing** (`replay::play`): the setup through the headless harness
  (`headless::apply_setup` writes the `GameSetup` with the seed; `begin_match` derives the AI
  seats and the match-scoped seed), the map catalog pinned to the one `.kpmap` the compat stamp
  names. Before each tick, the frames due on it go into `CommandBus::deliver`, the entry point
  the lockstep transport will use, and the ordinary `apply_sim_commands` applies them.
- **Compat**: a replay refuses to play if `SIM_VERSION`, the unit bundle or the map bytes
  differ (`--ignore-compat` overrides it for bisecting). `SIM_VERSION` is bumped by hand when a
  sim change is meant to change outcomes.
- **Seed**: the match seed is a field of the setup since M0.2 (`GameSetup::seed`); the replay
  stores it in `MatchSetup::seed`. `headless::apply_setup` is the one function that maps the
  setup onto the game.

The harness runs `FixedUpdate` on the single-threaded executor, the game's since M0.2.

### Hash trail

One line per tick, LF, `trails/<log stem>.trail`:

```
<tick> <sim hash> <positions> <health/flags> <orders> <projectiles> <units> <projectile count> <NaNs>
```

The sim hash is the X-sim state hash (`headless::state::capture`): units keyed by `NetId`
(pose from `SimPose`, health, flags, orders, queues), projectile positions, sim-entity counts.
Visual-only entities (CEG particles, beams, impact flashes) only go into a separate fx hash that
the trail leaves out. NaNs are hashed as one canonical quiet NaN (wasm engines may change NaN
payload bits) and counted in the last column; every golden run has 0.

### Native entry point

The harness is compiled into the game binary with the `replay` cargo feature (off for the game):

```
cargo build --release -p kernel-panic --features replay
kernel-panic --replay kernel-panic/tests/golden/*.kpreplay --trail-dir trails
             [--ticks N] [--dump-at t1,t2 --dump-dir d] [--shuffle 1] [--ignore-compat] [--memory-assets]
kernel-panic --record-golden [name…] [--out dir]
```

`main` hands over to `headless::cli::run` before any game setup when the first argument is one
of these. `--dump-at` writes the readable per-unit state (`state::describe`: every unit, its
mover state and path, query order, census, the archetype list) for bisecting.

### wasm32-unknown-unknown entry point

The same feature on wasm32 makes the module export `kp_replay_map(log)` and
`kp_replay(log, map, ticks, dump_at)` (`headless/web.rs`) instead of starting the game. The
runner `.github/scripts/kp-replay.mjs` passes the files in (wasm has no filesystem) and writes
the trail:

```
node .github/scripts/kp-replay.mjs <node|chromium|firefox|webkit> <pkg> <maps dir> <log> <out.trail> [ticks]
```

`node` loads the `wasm-bindgen --target web` output with `initSync`; the browsers run it in a
page served from a local HTTP server, through Playwright. One log per process / page.

What it took to run the full game harness in Node, which has no `window`:

- the asset server reads from an in-memory source (`AssetRoot::Memory`, Bevy's
  `MemoryAssetReader` registered as the default source before `AssetPlugin` builds); it holds
  the map only. The sounds, fonts and cursor images the frame loop asks for fail to load, which
  the sim never reads;
- no blocking anywhere: on the web Bevy runs its tasks (asset loads) as JS microtasks
  (`wasm_bindgen_futures::spawn_local`), so the harness is `async`, and every wait yields one
  macrotask to the JS event loop (`headless::pause`, a global `setTimeout(0)` promise that works
  in Node and browsers alike). It also yields every 256 ticks so browsers never flag the page
  as hung;
- `bevy::platform::time::Instant` instead of `std::time::Instant` (which panics on wasm).

Nothing else had to change: the plugin set is the same on both targets (`DefaultPlugins` minus
window, log and audio output, renderer disabled with `backends: None`, plus the whole game),
so every `FixedUpdate` system runs on both. Nothing is excluded.

The CI build matches the shipped web build: profile `wasm-release`, `wasm-bindgen --target web`
(CLI version taken from `Cargo.lock`), then `wasm-opt -O2` as Trunk's `data-wasm-opt="2"`.

### Golden logs (`kernel-panic/tests/golden/`)

| log | setup | ticks | content |
|---|---|---|---|
| `4v4_speed_balls` | Speed_Balls_16_Way, 2 ally teams × 4 AI seats, difficulty 2, seed `0x5eed` (X-sim's scenario) | 18 000 | 84 B, no frames; 41 units at the end |
| `ffa8_quad_core` | Quad_Core, 8 AI players, difficulty 4, seed `0xffa8` | 18 000 | 75 B, no frames; 152 units at the end |
| `commands_marble` | Marble_Madness_Map, three scripted human seats (System, Hacker, Network) and an AI System | 3 000 | 1 095 B, 59 frames, 88 commands |

The scripted match (`headless/golden.rs`) goes through every `SimCommand` kind: the factory
queue (add, add-front, remove newest/oldest), repeat, builds on a vent and a queued row, move,
queued attack-move (the second order goes out as `Units::Prev`), per-unit attack-moves, a drawn
path, patrol, guard, stop, auto-hold, deploy (Bugs to Exploits and back), manual targets,
attack-unit, queued attack-ground, enter, casts and self-destruct. The first human seat is the
local player and issues through `CommandBus::issue`, the input systems' path; the other two
arrive as remote frames. Recording replays the fresh log and requires the same trail as the
recording, so the bus log really captures the match.

Re-record after any change that alters the simulation (they need the baked maps):

```
cargo run --release -p kernel-panic --features replay -- --record-golden
```

## Query-order independence

The first wasm run (before M0.2 was merged) diverged from native at the first fights: tick 2 624
of the scripted log, 2 780 of the 4v4, 2 388 of the FFA. Orders stayed identical; health,
positions and projectiles diverged. The unit dumps showed why: the same units sat in different
archetypes, created in a different order, so every query iterated them in a different order.
Combat drew from one shared RNG in query order and summed damage in push order. M0.2 made those
loops canonical (NetId order, a sorted damage queue).

### Root cause: why wasm and native hold different archetype orders

Diffing the archetype lists (`state::archetypes`, in every dump) of the two builds at tick 1:

- **Platform plugins and resources** (in Bevy 0.20 resources are entities, so they take
  archetype ids too): native has `Gilrs`, `GilrsGamepads`, `RunningRumbleEffects`, and a
  `Gamepad` entity for every connected controller; the web build has none. The map loader is
  cfg'd per target: `PendingMapLoad` on native against `Assets<BytesAsset>`, its messages,
  `PendingWebMapLoad` and `PrefetchedWebMaps` on the web. `AssetSourceBuilders` registers at a
  different point once the harness sets its own source.
- **Map load timing**: native prepares the map on the async compute pool and the world appears
  in whichever frame the task finishes; the web prepares it synchronously in the frame the
  bytes arrive. So the unit-piece and terrain-chunk archetypes (`Aabb` is added a frame after
  spawning) are created in a different order, and the picking pointer's archetype moves.

Between two **native** peers the same thing happens: a plugged-in gamepad is enough to shift
every later archetype id. Audio output on or off, a window or not, and anything that spawns
render-only entities before the match does the same. None of this is sim state, so the sim
must simply never depend on query order.

### The fuzz test

`replay::shuffle_storage`, run before every tick with `--shuffle 1`, changes the ECS storage
order of every unit and projectile without touching sim state: a random half gets a marker
component (and the rest loses it), which splits every archetype and reorders the halves, then
every entity moves through a scratch archetype and back in a shuffled order, which gives it a
new row in its table. Component values, change ticks and hooks of the real components are
untouched, so a sim that does not depend on query order leaves the very same trail.

- CI: the `linux-x86_64-gnu` job replays every golden log a second time with `--shuffle 1` and
  uploads it as platform `linux-x86_64-gnu-shuffled`, which the compare job holds to the
  reference like any other platform. The 18 000-tick logs cost about a minute each.
- Locally: `cargo test --release -p kernel-panic headless::replay::tests::query_order_fuzz --
  --ignored --nocapture` (`KP_FUZZ_TICKS`, default 3 000; `KP_FUZZ_LOGS`).

### What it found after M0.2

One more query-order dependence, outside M0.2's systems: **`update_structure_layer`**
(`interaction/structures.rs`) stamped new structures into the nav grid in query order. Every
stamp bumps the nav revision with its own rectangle, and each QTPFS node layer replays the
rectangles in revision order when it catches up (`NodeLayer::update`), which shapes the quad
tree. Same blocked squares, different trees: with the storage shuffled, the very first path
search already ran on a different layer, and a Dos leaving its factory at tick ~1 600 of the
scripted log took the west side of an obstacle instead of the east (the hash saw it at tick
1 881). Fixed by stamping in `NetId` order (spawned and moved structures alike). After that,
all three logs keep identical trails with the storage shuffled every tick, and wasm matches
native.

### Other f32 accumulations

The fuzz test is the dynamic guard for every such sum; it reorders every query the sim makes.
Statically: neighbour sums (separation, avoidance, splash) walk `SpatialIndex` cells in grid
order and each cell's entries in `NetId` order (`rebuild_spatial_index`, M0.2); collision and
movement iterate in `NetId` order; damage is applied from M0.2's sorted queue. Kernel Panic has
no resource or heat economy to sum. Nothing else turned up in these three matches.

## Cost: wasm against native

Wall time inside `FixedUpdate` per tick ("sim") and of the whole headless frame ("frame",
the sim plus `Update`'s UI work with no window), averaged over each log, on the CI runners. The
native rows are the release build (thin LTO); the wasm rows are the `wasm-release` build after
`wasm-opt -O2`, as shipped.

| platform | 4v4 sim / frame (ms) | FFA sim / frame (ms) | FFA wall for 18 000 ticks |
|---|---|---|---|
| linux-x86_64-gnu | 0.20 / 1.40 | 0.66 / 2.20 | ~40 s |
| linux-arm64 | 0.41 / 2.60 | 1.24 / 4.08 | |
| windows-x86_64 | 0.46 / 3.20 | 1.47 / 5.25 | |
| macos-arm64 | 0.31 / 1.65 | 1.21 / 3.85 | |
| macos-x86_64 (Rosetta) | 0.93 / 5.96 | 2.05 / 8.05 | |
| wasm, Node 22 | 0.36 / 1.19 | 1.27 / 2.82 | 52 s |
| wasm, Chromium | 0.35 / 1.11 | 1.45 / 3.23 | 60 s |
| wasm, WebKit | 0.35 / 1.09 | 1.34 / 2.85 | 52 s |
| wasm, Firefox | 2.20 / 6.69 | 8.28 / 17.0 | 309 s |

(The x86_64 runners differ in CPU from job to job, so treat single numbers as ±20 %.)

- The sim costs about 1.8–2.2× native in V8 and JavaScriptCore, as X-sim estimated. The whole
  frame is *cheaper* in wasm: native runs the non-fixed schedules on the multi-threaded executor,
  whose hand-offs cost more than they save at this size, and loads real assets.
- Firefox (Playwright's build, headless) is 6× slower than the other engines on this module:
  8.3 ms per sim tick at ~350 units, still a quarter of the 33 ms tick budget. Worth a look on a
  real Firefox before relying on it for big games.
- Locally (Ryzen 7 7840HS, same builds): native FFA 0.97 / 2.54 ms, Node 0.92 / 2.04 ms.
- A whole 18 000-tick FFA replays in under a minute in Node, so the matrix runs every log in
  full everywhere; the slowest job is the Windows native build.

## Reproducing

```
# bake the maps the logs use (or all: .github/actions/bake-maps)
cargo build --release -p spring-map --bin bake-map
target/release/bake-map kernel-panic/assets/maps/Speed_Balls_16_Way.sdz   # and Quad_Core.sd7, Marble_Madness_Map.sd7
# native trails, and the query-order fuzz
cargo run --release -p kernel-panic --features replay -- --replay kernel-panic/tests/golden/*.kpreplay --trail-dir trails
cargo run --release -p kernel-panic --features replay -- --replay kernel-panic/tests/golden/*.kpreplay --trail-dir trails-shuffled --shuffle 1
# wasm in Node
cargo build --profile wasm-release -p kernel-panic --features replay --target wasm32-unknown-unknown
wasm-bindgen --target web --no-typescript --out-dir pkg --out-name kernel-panic target/wasm32-unknown-unknown/wasm-release/kernel-panic.wasm
node .github/scripts/kp-replay.mjs node pkg kernel-panic/assets/maps kernel-panic/tests/golden/ffa8_quad_core.kpreplay trails-wasm/ffa8_quad_core.trail
# compare
bash .github/scripts/compare-trails.sh <dir with trail-<platform>/ subdirs> linux-x86_64-gnu
```

A divergence: replay both sides with `--dump-at <t-2>,<t-1>,<t> --dump-dir d` (wasm:
`KP_DUMP_AT=…` for the runner, which writes `<out>.dumps`) and diff the unit lines; the mover
and path state on the `~` lines usually shows the cause a few hundred ticks before the hash
does.

## Open

- **`f32::max`/`min` with signed zeros** differ between x86_64 and aarch64 (float probe `chk:`
  row). Harmless while no sim value can be `-0.0` against `+0.0` in a `max`/`min` whose result's
  sign matters (a later division, `atan2`, or the hashed bits). The trails say it is not hit in
  these matches; a `clippy.toml` ban in sim code (in favour of a sign-canonical helper) would
  make it certain.
- **Firefox** runs the sim 6× slower than V8/JSC (above).
- **Coverage**: three matches. The fuzz test and the matrix run on every push, but they only see
  what these matches exercise: no Hex Farm (its towers and ownership), no SIGTERM airstrike, no
  rematch in one app. A Hex Farm log is the obvious next golden log.
- **Re-recording**: any intended sim change changes the trails; re-record the logs (command in
  "Golden logs") and bump `replay::SIM_VERSION` when the change should invalidate old replays.
  The compare job only checks platforms against each other, so a stale log still passes; it
  fails only when the map or unit bundle changed (the compat stamp).
- The `replay` feature compiles the whole harness into the game binary; the default build is
  unchanged (`headless` is `cfg(any(test, feature = "replay"))`).

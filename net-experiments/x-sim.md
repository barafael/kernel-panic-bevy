# X-sim: headless simulation harness, determinism, state size, command volume

Experiment branch `worktree-agent-a54879b76d9ed827f` (from `main` @ efd0724).
Machine: AMD Ryzen 7 7840HS (8C/16T), 28 GB, Linux, release builds.

## What was built

`kernel-panic/src/headless/` (test-only, `#[cfg(test)] mod headless`):

- `mod.rs`: `SimHarness` runs **the whole game** headless. It uses `crate::add_game` (the
  plugin list `main` now shares), on top of `DefaultPlugins` minus `WinitPlugin`, `LogPlugin`
  and `AudioPlugin`, with `WindowPlugin { primary_window: None }`.
  **How rendering is stubbed:** it isn't. `RenderPlugin` gets
  `WgpuSettings { backends: None }`, so no GPU device and no render sub-app are created, but every
  asset type (`Mesh`, `Image`, `StandardMaterial`, custom materials, shaders) stays registered.
  `Assets<T>` then act as plain CPU-side stores. Nothing in the game had to be cfg'd out: unit
  models, FX batches, terrain chunks and the minimap image are built and never uploaded. The
  clock is driven by hand with `TimeUpdateStrategy::ManualDuration`, so `step()` = one frame =
  exactly one 30 Hz `FixedUpdate` tick. `frame(n)` renders a frame that runs 0–3 ticks. The map
  loads through the normal skirmish path (`GameSetup` → `AppState::InGame` → `map_loading`,
  baked `.kpmap`), with the clock stopped while it loads, so every match starts at fixed time 0.
  All seats are AI and the local player spectates (`GameOverDismissed(true)`, so the spectator's
  "Defeat" doesn't freeze the sim). `rematch()` starts a second match in the same app.
- `state.rs`: a canonical, **entity-id-independent state hash**. Each unit becomes a record of
  kind, team, the f32 bits of its sim pose (`SimPose::sim()`, not the render-blended
  `Transform`), health, flags, orders, command queue and factory queue. References to other
  units are replaced by the referenced unit's (kind, pose) key. Records are sorted, then hashed
  with FNV-1a. There are three part hashes (positions / health / orders), plus projectiles, plus
  sim-entity counts. A separate "fx" hash covers visual-only entity counts. It also produces a
  census by component type, and a readable per-unit dump for diffing.
- `encode.rs`: snapshot encoders (full raw f32, full quantized, delta vs. baseline,
  zstd / lz4) and the lockstep command encoder.
- `tests.rs`: `determinism`, `rematch`, `bandwidth`, `fixed_ambiguities`, `frame_systems`,
  `unit_components`, `map_seats`, `smoke`. All `#[ignore]`d, all configured through env vars
  (`KP_NET_MAP`, `KP_NET_TEAMS`×`KP_NET_PER_TEAM`, `KP_NET_SEED`, `KP_NET_TICKS`,
  `KP_NET_EXEC=single|multi`, `KP_NET_PACING=uneven|multi|zeros|even`, `KP_NET_ID_SHIFT`,
  `KP_NET_DIFFICULTY`, `KP_NET_TRACE`, `KP_NET_CSV`, `KP_NET_DUMP`).

```
cargo test --release -p kernel-panic headless::tests::determinism -- --ignored --nocapture --test-threads=1
KP_NET_MAP=Quad_Core KP_NET_TEAMS=8 KP_NET_PER_TEAM=1 KP_NET_DIFFICULTY=4 KP_NET_TICKS=27000 \
  cargo test --release -p kernel-panic headless::tests::bandwidth -- --ignored --nocapture
```

Production-code changes not related to determinism: `main.rs` moves the game plugin list into
`add_game()`. `netprobe.rs` adds order-funnel probes: the closure only runs under `cfg(test)`,
so the game build is unchanged. `game_setup::test_seed` is a test-only `match_seed` override.
`SimPose::sim()` is a test-only accessor.

Scenarios. "4v4" means 8 AI seats on 2 ally teams (`TeamId` is the ally team, so each side is
**one** AI brain with 4 homebases). "FFA" means 8 seats on 8 teams, i.e. 8 independent AI
players. Maps with ≥ 8 start positions: Speed_Balls_16_Way, Central_Hub, Quad_Core,
Data_Cache_L1, Corrupted_Core, Dual_Core, DMA, Palladium, Spooler (16), Major_Madness (8),
Hex_Farm (procedural).

## 1. Determinism

Each tick's hash is logged at the end of the tick, inside `FixedLast`, so frames that run
several ticks log every tick. Default scenario: 4v4 on Speed_Balls_16_Way, seed `0x5eed`,
18 000 ticks (10 game minutes). That game is decided at about 6.5 min: one side is eliminated,
and then the winner's AI razes itself (`SuicideIfAlone`).

### Baseline (main, harness only)

| comparison | first divergent tick |
|---|---|
| same scenario twice in one process, multi-threaded `FixedUpdate` | none (18 000 identical) |
| two processes (`KP_NET_TRACE` diff) | none (18 000 identical) |
| FFA 8 teams, twice in one process | none (18 000) |
| single-threaded vs multi-threaded executor | **8 955** (health) |
| 2nd run with uneven render frames (0–3 ticks/frame) | **510** (orders) |
| 2nd run with 2–3 ticks per frame, no tickless frames | **508** |
| 2nd run with extra tickless frames only (`zeros`) | none (6 000) |
| 2nd run with entity ids shifted (101 empties spawned, every other one despawned) | **506** |
| different seed (sanity) | 496 |

So the sim reproduces bit-for-bit **as long as every peer renders the same frames**. It
depends on frame pacing and on entity ids, and real peers never share either.

### Sources found and what was done (file:line at main @ efd0724)

Fixed in this branch:

1. **Sim state updated on the render clock**: `terrain::geovent::reconcile_vent_claims` ran in
   `Update` with variable dt (`terrain/mod.rs:24`, `geovent.rs:342`). It inserts and removes the
   `VentClaim`s the AI's expansion reads, and it read `GlobalTransform`. Moved to `FixedUpdate`
   (before `ai_brain` and the movers) and switched to reading `Transform`.
2. **`Entity` used as the unit id**: `GroundMover::seeded` derives the SlowUpdate phase from
   `entity.index()` (`ground_move.rs:718`), and `HoverAir` seeds its RNG with `entity.to_bits()`
   (`air_movement.rs:757`). Entity indices are shared with every UI and FX entity the frame loop
   spawns, so they depend on the frame rate and the HUD. Added a match-scoped `SimSerial` (spawn
   order, Spring's unit id) through `SpawnContext`, and used it for both.
3. **`Changed<Transform>` polluted by the render interpolation**: `orient_stationary_to_terrain`
   (`movement.rs:764`) re-derived upright rotations whenever `Transform` "changed". The
   interpolation writes `Transform` once per *frame*, and each re-derivation can land an ulp off.
   An Exploit's rotation diverged by 1 ulp. Made idempotent: an already-upright rotation is left
   alone.
4. **ECS query order used as sim order**: the AI rolls dice per unit and per factory, and picks
   "first" homes and vents, all in query iteration order (`ai/mod.rs:359`, `:435`, the datavent
   loop). Two Packets swapped targets because archetype history differed. The snapshot, the
   factories and the vents are now sorted by `SimSerial` (vents by position).
5. **Unordered systems with conflicting access** (counted by the new `fixed_ambiguities` test:
   **21 pairs → 8**):
   - `ai_brain` vs `movement_system` / `hover_air_system` (MovePath);
   - `guard_follow_system` vs the movers;
   - `ground_collision_system` vs `hover_air_system`;
   - the spawners vs `drain_eruption_queue`.

   All are ordered now. The remaining 8 pairs touch only `Assets<StandardMaterial>` and the
   showcase.
6. (Preventive) The production pad position came from the pad piece's `GlobalTransform`
   (`production.rs:338/493`), which is propagated once per render frame. It is now composed from
   local transforms, the way `PieceLookup::muzzle` already did.

After the fixes:

| comparison | first divergent tick |
|---|---|
| uneven pacing, 4v4 Speed_Balls | none (18 000, whole game) |
| 2–3 ticks/frame | none (18 000) |
| uneven pacing, FFA 8, 15 min | none (27 000) |
| uneven pacing, 4v4 Central_Hub | none (18 000; at 4 687 before fix 5) |
| entity-id shift | none (18 000) |
| two processes, multi executor | none (18 000) |
| single- vs multi-threaded executor | **7 188** (health only, not root-caused) |
| rematch in the same app after 900 ticks of another match | **1 636** (same with the fixed clock reset to 0) |

Visual-only entities (CEG particles etc.) still differ under uneven pacing from about tick 1 700,
because `Update` spawns some of them (geovent smoke). They don't feed back into the sim hash.

Not fixed (listed for whoever does lockstep):

7. **Match seed**: `game_setup::match_seed()` uses the per-launch `clock_f64`
   (`game_setup.rs:523`). It seeds the AI dice (`ai/mod.rs:148`) and the Hex Farm layout
   (`map_loading/mod.rs:671`). The host must choose it and send it. The harness uses a
   test-only override.
8. **State that outlives a match**: system `Local`s act as RNGs and frame counters and are never
   reset: `production.rs:387-388` (`exit_rng`, `frame`), `combat/mod.rs:747`, `:1395`,
   `damage.rs:595`, `weapon_fx/spawn.rs:52`, `:347`, `ceg.rs:1031`, `ground_move.rs:1576`,
   `air_movement.rs:707`, `cloak.rs:144`. The `rematch` test diverges at tick 1 636 (first
   combat) whether or not the fixed clock is reset. The menu's attract demo runs the sim before
   any real match, so peers would differ from the first fight. They should become match-scoped
   resources seeded from the match seed.
9. **App-relative fixed clock**: `combat::sim_frame()` (`combat/mod.rs:614`), the AI's
   `elapsed_secs()` (`ai/mod.rs:262`) and `damage.rs:488` read `Time<Fixed>` elapsed time
   *since app start*. That includes menu and loading time, and `elapsed_secs()` is an f32 that
   loses precision over a long session. The sim needs a match tick counter. The harness only
   avoids the problem because every match starts at fixed time 0.
10. **std `HashMap` (RandomState) in sim state**: `ai/mod.rs:137-141`, `:189-195`, `:977`;
    `network_buffer.rs:47`; `game_over.rs:126`; `construction.rs:413`; `cloak.rs:149`, `:330`;
    `bookkeeping.rs`. Today they are used only for lookups and commutative folds, so they are
    harmless as measured (two processes stay identical), but they are a trap.
11. **Single- vs multi-threaded executor** still diverges (tick 7 188, a 1-ulp health
    difference). There are no data ambiguities left on sim state. Candidates are command-buffer
    and parallel entity-reservation order, and f32 damage summation order. Lockstep peers must
    all use the same executor (single-threaded is also cheaper, see §4).
12. `hex_farm_owners` runs in `Update` (`map_events/hex_farm.rs:554`): Hex Farm tower ownership
    is in the frame loop. Not tested; no test map used it.

### Native vs wasm32 float determinism (measured)

The sim calls: `sin` 16×, `cos` 23×, `tan` 6, `atan2` 7, `asin` 6, `acos` 2, `powf` 6,
`hypot` 4, `cbrt` 1, `sqrt` 39, `powi` 27 (in units/, interaction/, terrain/, map_events/,
sim.rs, spring-pathfinding, spring-cob). Through glam it also uses `Quat::from_rotation_x/y`,
`from_axis_angle`, `from_euler`, `looking_to`/`looking_at`, `angle_between`, `slerp`,
`Vec3A`/`Affine3A` (`GlobalTransform`).
Notable sites: `combat/aim.rs` (96-105, 379, 603, 635, 726, 739), `combat/mod.rs:2204-2206`,
`ground_move.rs` (936, 1011, 3178, 3196, 3571), `weapon_fx/flight.rs` (425-636),
`spring-cob/src/unit_values.rs:234-268`, `spring-pathfinding/src/cost.rs:364/384`, `ai/army.rs:267`.

`net-experiments/x-sim-floatprobe/` evaluates 200 000 inputs per function in game-like ranges.
It was built for x86_64-gnu (glibc libm), x86_64-musl, x86_64 with `-C target-cpu=native`, and
wasm32-unknown-unknown with and without `simd128`. The wasm builds ran in node 26. Share of
inputs whose result bits differ:

| function | glibc vs wasm | musl vs wasm | glibc vs glibc+native-cpu |
|---|---|---|---|
| sin / cos | 1.2 % / 1.3 % | 0 | 0 |
| tan | 14.2 % | 0 | 0 |
| atan2 | 16.4 % | 0 | 0 |
| asin / acos | 4.9 % / 8.5 % | 0 | 0 |
| powf | 9.0 % | 9.0 % | 0 |
| hypot | 12.5 % | 0 | 0 |
| cbrt, sqrt, powi | 0 | 0 | 0 |
| glam `Quat * Vec3` (from_rotation) | 2.1 % | 0 | 0 |
| glam `Vec3A::normalize` | 29 % (scalar wasm) / 0 (simd128) | 29 % / 0 | 0 |
| glam `Affine3A` transform | 1.3 % | 0 | 0 |
| glam `slerp` | 48 % | 48 % | **20 %** |

Every transcendental the sim uses differs between Linux-native and the web build on a few
percent to 16 % of inputs. Even `target-cpu` flags change glam's `slerp`. Windows (MSVC CRT)
and macOS libm weren't testable here; expect the same class of difference. One ulp is enough to
fork the game: above, a 1-ulp rotation difference at tick 8 820 had become different unit
orders by tick 10 044. **Cross-platform lockstep needs deterministic math in the
sim**: the `libm` crate (pure Rust, the same on every target) instead of std's f32 methods,
glam with `scalar-math` (or no glam transcendentals) in sim code, and pinned codegen flags.
Spring uses streflop for the same reason. `sqrt`, `powi` and the basic IEEE ops are safe.

## 2. State size and bandwidth (server- or host-authoritative)

**Replicated state per unit**, i.e. what a thin client needs to draw a unit and its HUD:

- a server-assigned u16 net id;
- kind (u8) and team (u8), sent on spawn only in deltas;
- position: x/z at 1/4 elmo in u16 (enough for 16 384 elmos), y at 1/16 elmo in u16;
- orientation: yaw u16, pitch i8, roll i8;
- health fraction u16;
- build/emerge progress u8;
- a flag byte: dying, being built, cloaked, stunned, moving, fight order, constructing,
  attack/guard order.

That is 18 B for a full record. **Projectiles**: id and position (8 B) every update.

**Not included**, and needed in a real protocol:

- weapon-fire / impact events. Measured: 88–354 beam starts/s, 10–36 laser bolts/s, up to 56
  new FX entities in one tick. At about 10 B each that adds 1–4 KB/s.
- COB piece poses (up to 10 000 piece entities). A thin client would have to run the unit
  scripts from replicated aim/fire state, or receive piece transforms, which would be far
  bigger. **This is the main open design problem for (a)/(b).**
- fog-of-war interest management, which would cut the payload.

Deltas are taken against the previous *sent* snapshot. A unit is skipped when unchanged after
quantization; changed units send id + mask + changed fields; spawns are full records; despawns
send their id. Each update is compressed on its own. Numbers below are over the **live** part of
each game (≥ 24 units on the map), per update to one client. "B/s" is the payload rate before
UDP / DTLS overhead.

| scenario | live min | peak units | median units | full raw f32 | full quant | full quant+zstd | **delta quant** | **delta quant+zstd** | delta+lz4 |
|---|---|---|---|---|---|---|---|---|---|
| A 4v4 Speed_Balls, diff 2 | 5.9 | 119 | 70 | 3 200 / 4 964 | 1 324 / 2 046 | 945 / 1 483 | 442 / 794 | 430 / 720 | 440 / 795 |
| B 4v4 Central_Hub, diff 4 | 4.7 | 194 | 106 | 4 716 / 7 828 | 1 934 / 3 214 | 1 367 / 2 250 | 835 / 1 439 | 768 / 1 284 | 835 / 1 442 |
| C FFA 8 Speed_Balls, diff 4 | 14.7 | 147 | 107 | 4 820 / 6 144 | 1 988 / 2 540 | 1 441 / 1 810 | 611 / 865 | 576 / 784 | 610 / 865 |
| **D FFA 8 Quad_Core, diff 4** | 11.3 | **414** | 310 | 13 816 / 17 048 | 5 674 / 7 012 | 4 058 / 5 081 | 2 378 / 3 139 | **2 202 / 2 894** | 2 385 / 3 148 |

(bytes per update, median / p95; units + projectiles)

- The delta is about 7 B per unit per update (5.3–7.1 median); a full quantized record is about
  18 B per unit. Most units move every tick, so deltas are only about 2.4× smaller than full
  snapshots.
- zstd saves 4–8 % on deltas and about 28 % on full snapshots. lz4 saves nothing on deltas:
  quantized positions are high-entropy.
- Lower rates barely shrink a single update. Scenario D, all ticks: 30 Hz 1 594 B, 15 Hz 1 642 B,
  10 Hz 1 678 B (medians), because nearly every moving unit changes between updates anyway. The
  rate is roughly the size × the update frequency.

Per client, payload, delta+zstd (median / p95):

| scenario | 30 Hz | 15 Hz | 10 Hz |
|---|---|---|---|
| A | 12.9 / 21.6 KB/s | ≈ 6.7 / 11 KB/s | ≈ 4.5 / 7.5 KB/s |
| B | 23.0 / 38.5 KB/s | ≈ 12 / 20 KB/s | ≈ 8 / 13 KB/s |
| C | 17.3 / 23.5 KB/s | 9.1 / 12.4 KB/s | 6.2 / 8.5 KB/s |
| D (414 units peak) | 66 / 87 KB/s | ≈ 34 / 45 KB/s | ≈ 23 / 30 KB/s |

(15 and 10 Hz values for A, B and D are scaled from 30 Hz using D's measured all-tick ratios.)

- A host sends this to 7 clients: in D that is 0.46 / 0.61 MB/s upload at 30 Hz, or about
  0.16 / 0.21 MB/s at 10 Hz.
- The p95 update in D (2.9 KB) needs 3 packets of about 1 200 B (the WebRTC/SCTP MTU) at 30 Hz.
- **Extrapolation** (not measured): a heavy KP game of about 200 units per seat (1 600 units)
  at 7 B/unit is about 11 KB/update. That is 340 KB/s per client at 30 Hz (2.4 MB/s host
  upload), or 110 KB/s at 10 Hz (0.8 MB/s host upload). The AI fairness cap kept every measured
  game well below that.

## 3. Command volume (lockstep input)

Probes sit at the AI's order funnels (`netprobe::record` in `units/ai/mod.rs`):

- every `replace_order` the AI issues (move, attack-move, build-at);
- factory queue appends and repeat-flag flips (flips counted only when the flag actually
  changes);
- command-fire casts, deploys, dispatches and self-destructs.

The internal promotions (`install_command` from the queue) are sim-internal and don't travel.

Wire format: a per-turn message header (tick u32, player u8, count u8 = 6 B), then per command
kind u8 + unit u16 + argument (position as 2×u16 x/z, unit u16, kind+count 2 B, build kind+site
5 B, or flag 1 B). That is 3–8 B per command.

| scenario | commands/s, all | per AI brain | per seat | B/s, all | ticks with any command | worst single tick, one brain |
|---|---|---|---|---|---|---|
| A 4v4 | 2.99 | 1.50 | 0.37 | 25.6 | 306 / 12 000 | 39 cmds, 123 B |
| B 4v4 | 2.14 | 1.07 | 0.27 | 17.4 | 252 / 18 000 | 87 cmds, 267 B |
| C FFA 8 | 5.09 | 0.64 | 0.64 | 48.6 | 818 / 27 000 | 16 cmds, 118 B |
| D FFA 8 | 9.93 | 1.24 | 1.24 | 79.9 | 623 / 27 000 | **170 cmds, 520 B** |

Mix in D: mostly attack-move (army dispatch, one command per unit with its own scattered
target), then move, factory enqueue, build-at, deploy and self-destruct.

- The AI thinks once a second, so its commands come in **bursts on one tick**, up to 170.
  Per-unit attack-moves to scattered targets don't group into a single command.
- In a real lockstep game the AIs would run on every peer as part of the sim and send nothing.
  Human input is what travels. Even a 300-APM player (5 group commands/s at about 10–20 B) is
  around 100 B/s.
- Either way **command bandwidth is negligible**: under 1 KB/s for all 8 players, against tens
  to hundreds of KB/s for state replication. Latency and determinism are the lockstep problems,
  not bytes.

## 4. Sim cost

The cost is the wall time from `FixedFirst` to `FixedLast` of each tick (the whole fixed-step
chain: AI, motion, combat, FX simulation and pose bookkeeping), measured in the headless app.
The whole frame adds `Update` (the UI with no window) on top: about 1.8–2.7 ms median.

| units on map | multi-threaded executor, median / p95 | single-threaded, median / p95 |
|---|---|---|
| 0–49 | 0.65 / 0.78 ms | 0.13 / 0.22 ms |
| 150–199 | 1.05 / 1.93 | 0.51 / 1.02 |
| 300–349 | 1.60 / 2.85 | 0.95 / 1.98 |
| 350–399 | 1.69 / 3.00 | 1.12 / 2.33 |
| 400–449 | 1.76 / 2.80 | 1.18 / 2.35 |

The single-threaded column is from a 6.3-min run of scenario D (peak 453 units).

- The late-game maximum single tick was 8.1 ms (multi) and 7.3 ms (single). Both are far below
  the 33.3 ms tick budget.
- The multi-threaded executor's overhead is larger than its parallel win at these sizes.
- A wasm client (single-threaded, typically 1.5–3× slower than native) should still keep up
  with a 400-unit 4v4 in lockstep. That is an estimate, not a measurement.
- The heavy-game extrapolation (1 600 units) is about 4–5 ms native per tick if cost stays
  roughly linear, but path searches and collisions may not scale linearly.

## Conclusions

- **Lockstep (c) is close within one build on one platform.** After six cheap fixes, a whole
  10-min 4v4 and a 15-min FFA stay bit-identical across uneven frame pacing, shifted entity
  ids and separate processes. Still needed:
  - a match tick counter;
  - match-scoped, seeded RNG state instead of system `Local`s (the `rematch` divergence);
  - a host-chosen seed;
  - one fixed executor;
  - a desync checker. The state hash here is a ready-made per-tick checksum to exchange.
- **Native ↔ web lockstep is the hard part.** libm and glam SIMD differ on 1–48 % of inputs per
  function. It needs a deterministic-math pass over the sim (the `libm` crate, scalar glam,
  pinned flags, no `slerp`/`from_euler` on sim state). The float probe can serve as a CI check.
- **Host/server authority (a)/(b)** costs a modest 13–66 KB/s per client at 30 Hz for the
  measured games, or 4.5–23 KB/s at 10 Hz plus interpolation. That works for a WebRTC star with
  8 players. A heavy 1 600-unit game would push a home host to about 0.8 MB/s upload at 10 Hz.
  The real cost is client-side animation: COB pieces and FX events are not part of the state
  above.

## Reproducing

```
# determinism (in-process; second run variant via KP_NET_PACING / KP_NET_ID_SHIFT / KP_NET_EXEC)
KP_NET_TICKS=18000 KP_NET_PACING=uneven cargo test --release -p kernel-panic headless::tests::determinism -- --ignored --nocapture
# cross-process: run twice with KP_NET_RUNS=1 KP_NET_TRACE=<file>, then diff the files
# rematch / ambiguity count
KP_NET_TICKS=6000 cargo test --release -p kernel-panic headless::tests::rematch -- --ignored --nocapture
cargo test --release -p kernel-panic headless::tests::fixed_ambiguities -- --ignored --nocapture
# bandwidth + commands + sim cost (scenario D)
KP_NET_MAP=Quad_Core KP_NET_TEAMS=8 KP_NET_PER_TEAM=1 KP_NET_DIFFICULTY=4 KP_NET_TICKS=27000 KP_NET_CSV=d.csv \
  cargo test --release -p kernel-panic headless::tests::bandwidth -- --ignored --nocapture
# float probe
cd net-experiments/x-sim-floatprobe && cargo build --release && cargo build --release --target x86_64-unknown-linux-musl \
  && cargo build --release --lib --target wasm32-unknown-unknown
target/release/floatprobe gnu.bin; node run.mjs target/wasm32-unknown-unknown/release/floatprobe.wasm wasm.bin
python3 compare.py gnu.bin wasm.bin
```

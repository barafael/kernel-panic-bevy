# M0.2: the remaining nondeterminism

Goal (`docs/multiplayer-plan.md`, M0.2): the fixed-tick simulation reproduces bit for bit from
the match setup alone (map, seats, seed, options, command log). That has to hold whatever the
app ran before (the menu's attract demo, an earlier match), whoever the local player is,
however the frames are paced and whatever entity ids the frame loop hands out.

Machine: AMD Ryzen 7 7840HS, Linux, release builds. The machine was shared with other builds
the whole time (load average 35–50), so timings are noisy.

## What changed (main @ edb3042 → this branch)

### 1. The match seed comes from the setup

| before | after |
|---|---|
| `game_setup::match_seed()` (`game_setup.rs:523`) drew a per-launch value from `clock_f64`; the AI (`ai/mod.rs:146`) and the Hex Farm roll (`map_loading/mod.rs:671`) called it | `GameSetup::seed: u64` (pub field). `begin_match` copies it into the match-scoped `sim::MatchSeed` *before* re-initialising the match resources, so their `FromWorld` seeds from it. Hex Farm rolls from `inputs.setup.seed`. |
| the headless harness pinned the seed through the process-global `game_setup::test_seed` (`game_setup.rs:537`) | `test_seed` is gone; the harness writes `Scenario::seed` into the setup |
| | the menus draw a seed when they *make* a setup (`game_setup::random_seed()`: `Default`, `demo_setup`, `showcase_setup`, `build_setup`, the bot's setup). The sim never draws entropy. |

API for replays (M0.4): build a `GameSetup { map, players, difficulty, demo, showcase, sandbox,
seed }`, insert it as a resource and enter `AppState::InGame` (or write `RunGame` from a match).
That is what `headless::enter_match` does.

### 2. System `Local`s that carried sim state are match-scoped

Every random stream is a `sim::SimRng<Stream>` resource: an xorshift32 state seeded from
`MatchSeed::stream(salt)` (splitmix64 of seed ^ salt), registered with `init_match_resource`.
Each system owns its own stream, so systems that run unordered never share a state.

| site (main) | before | after |
|---|---|---|
| `combat/mod.rs:757` `combat_system` | `Local<u32>`, `0xDEADBEEF` on first use | `ResMut<SimRng<TargetingRng>>` |
| `combat/mod.rs:1408` `attack_ground_system` | `Local<u32>` | `SimRng<GroundFireRng>` |
| `combat/damage.rs:596` `tick_burst_fire` | `Local<u32>` | `SimRng<BurstRng>` |
| `lifecycle/production.rs:407` `production_system` | `exit_rng: Local<u32>` | `SimRng<FactoryExitRng>` |
| `lifecycle/production.rs:408` | `frame: Local<u32>` (bugger-off phase) | `SimTick::frame()` |
| `interaction/ground_move.rs:1582` `movement_system` | `rng_state: Local<u32>` (impulse skid) | `SimRng<ImpulseRng>` |
| `interaction/ground_move.rs:2363` `ground_collision_system` | `seen_revision: Local<u64>` (a new map's nav revisions restart at 0) | match resource `TrapCheckRevision` |
| `interaction/air_movement.rs:708` `hover_air_system` | `frame: Local<u32>` (slow-update and `& 3` phases) | `SimTick::frame()` |
| `mechanics/cloak.rs:144` `update_cloak_detection` | `timer: Local<f32>` (feeds `DetectedBy`, i.e. targeting) | match resource `CloakDetectionTimer` |
| `mechanics/cloak.rs:312` `update_fog_visibility` | `Local<FogRefreshTimer>` | match resource (keeps its phase to the other scans) |
| `weapon_fx/spawn.rs:53`, `:348`, `weapon_fx/ceg.rs:1039` | `Local<u32>` (visual only) | `SimRng<WeaponVisualRng>`, `<ExplosionVisualRng>`, `<DelayedCegRng>`: a replay draws the same effects too |
| `ai/mod.rs:146` `AiTicker` | dice from `match_seed()` | `FromWorld`: `MatchSeed::stream(AI_DICE_SALT)` |

The `Local`s left in sim systems are scratch buffers that are cleared before every use
(`AiScratch`, the avoidance / collision grids, `spawns`, `los_candidates`, `flyers`,
`splash_hits`, `ordered`, `detected`, `in_sight`), a registry-derived cache
(`update_structure_layer`'s `classes`), and `hover_air_system`'s `awaiting_air`: a list of
freshly spawned aircraft that drops every entity it can no longer find, so an earlier
match's entries vanish on the first tick.

**Per-draw keyed streams.** The coordinator asked to prefer per-draw streams keyed on (seed, tick,
NetId, weapon, counter) wherever draws happen inside a query loop. I kept one stream per system
and made the *loops* canonical instead (§5): every stream above is drawn in `NetId` order.
Keyed streams would also remove the order dependence for the draws themselves; they are a
drop-in change in `sim::SimRng`, if M0.4's fuzz test asks for it.

### 3. Sim time is an integer match tick

`sim::SimTick(u64)`, match-scoped (0 at every swap), advanced by `sim::advance_sim_tick` in
`FixedFirst` (every fixed tick, frozen gameplay included, like the clock it replaces), so a
match's first tick is frame 1. `SimTick::secs()` = `frame / 30`.

| before | after |
|---|---|
| `combat::sim_frame(&Time)` (`combat/mod.rs:623`): `round(elapsed_secs_f64 · 30)` of the fixed clock since **app start** | removed; `combat_system`, `attack_ground_system`, `tick_burst_fire`, `DamageResolver` read `SimTick::frame()` |
| AI `now = time.elapsed_secs()` (`ai/mod.rs:287`), f32 since app start | `tick.secs()` |
| `TeamDamageLog` timestamps `time.elapsed_secs()` (`damage.rs:488`) | `tick.secs()` |

`delta_secs()` reads stay: inside `FixedUpdate` it is the constant 1/30 s.

### 4. Hash maps

| site (main) | after |
|---|---|
| `ai/mod.rs:134-138` `AiTicker` (`last_sigterm`, `last_dispatch: HashMap<Entity, f32>`, `no_enemies_since`) | `BTreeMap`; `last_dispatch` is keyed by `NetId` and pruned against the id-sorted snapshot |
| `ai/mod.rs:206-212` `AiScratch` (`counts`, `force`, `homes`), `ai/mod.rs:950` `TeamDamageLog` | `BTreeMap<u8, _>` |
| `network_buffer.rs:48` `PacketBuffer`, `bookkeeping.rs:121/160/235` unit counts, `game_over.rs:126` census | `BTreeMap<u8, _>` |
| `construction.rs:413` capped counts | bevy's fixed-hasher `HashMap` (lookup only) |
| `cloak.rs:149/330` `Local<std HashMap/HashSet<Entity>>` | `EntityHashMap` / `EntityHashSet` (cleared each scan, lookup only) |

Left as they are, lookup-only: `NetIdIndex` (`net/mod.rs:91`), `PathQueue`'s maps (bevy
fixed hasher, reset when the nav grid's epoch changes), `StructureLayer::stamps`, `VentGrid`, the
spatial / avoidance / collision grids (bucket lookup by cell), the CommandBus's per-player maps,
and visual caches (materials, the Hex Farm `%` labels and touched terrain chunks).

### 5. Iteration order (found by the rematch, not in X-sim's list)

With the `Local`s fixed, the long-warmup rematch still diverged: **sim hash at tick 2 614**
(projectiles first). A `Query` visits its tables in the order the **app** created them, which
depends on everything the app ran before (the earlier match, the attract demo) and on per-peer
components (selection, `Spotted`, cloak fades) that move units between tables. Dice drawn per
unit, spawns (each draws the next `NetId`), hits summed into health, path requests served in
turn, sequential collision responses: all of them followed that order.

Fix: order-dependent sim systems iterate in `NetId` order (`query.iter().sort::<&NetId>()`,
Spring's fixed unit-list order); the rule is in the `sim` module docs.

| system (main) | why its order matters |
|---|---|
| `spatial::rebuild_spatial_index` (`spatial.rs:175`) | every radius query (targeting, AI, splash, bugger-off) visits a cell's units in insertion order |
| `combat_system` (`combat/mod.rs:714`), `attack_ground_system` (`:1378`), `tick_burst_fire` (`damage.rs:581`) | dice per shot, damage-queue pushes |
| `production_system` (`production.rs:370`) | spawns (NetId order), exit dice |
| `start_construction` / `tick_construction` (`construction.rs:236/385`) | contested sites, unit caps, nanoframe spawns |
| `death_system` (`combat/lifecycle.rs:236`) | death blasts and Virus spawns queue in it |
| `movement_system` (`ground_move.rs:1567`, both loops) | avoidance sums neighbours in snapshot order; path requests queue |
| `ground_collision_system` (`ground_move.rs:2341`, both loops) | collider ties by snapshot index; responses see earlier units' moves |
| `hover_air_system` (`air_movement.rs:706`) | each aircraft's step sees the ones flown before |
| `tick_auto_dispatch` (`network_buffer.rs:158`) | teleporters share the team buffer |
| `advance_pending_casts`, `tick_nx_casts` (`command_fire.rs:515/827`) | cast events and their spawns |

The damage pipeline also resolves its inputs canonically, whoever queued them (projectiles and
zones have no `NetId`): `DamagePass::run` sorts the drained `DamageQueue` by (attacker id,
weapon, impact position bits, attacker distance bits, target id), and
`DamageResolver::deliver_due` sorts the due hits by (target id, attacker id, weapon, damage
bits, paralyze time). The sorts are stable, so exact duplicates keep their order.

Systems that update each unit from its own state alone keep the cheaper ECS order.

### 6. Hex Farm tower ownership in the fixed tick

`hex_farm_owners` ran in `Update` (`hex_farm.rs:554`, `:186`). It now runs in `FixedUpdate`
(`Cleanup`, after `hex_farm_sim`). A `FixedUpdate` reader of `RemovedComponents<Emerging>`
would miss removals whenever the frame rate exceeds the tick rate, so finished builds come
from an observer (`On<Remove<Emerging>>` → `HexFarmView::finished`). Same-tick finishers
claim in `NetId` order.

### 7. FixedUpdate ambiguities: 10 → 0

`fixed_ambiguities` listed 10 pairs (with the old `Update` ownership system gone):

- `showcase_director` vs `apply_sim_commands` (world) and vs `ai_brain` (`Producer`) →
  `.after(SimCommandSet).before(ai_brain)`;
- `reconcile_vent_claims` vs `apply_sim_commands` (world: a `Build` command claims vents) →
  `.after(SimCommandSet)`;
- `spawn_shield_shells` / `tick_shield_shells` vs `process_deploy`, `process_dispatch`,
  `production_system`, `install_fade_materials`, `tick_construction`, `emerge_system`,
  `drain_eruption_queue` (`Assets<StandardMaterial/Mesh/Image>`, `S3OModelCache`: asset ids
  are handed out in run order) → `.after(emerge_system).after(drain_eruption_queue)`.

`fixed_ambiguities` now reports **0 pairs**.

### 8. Single-threaded executor

`UnitsPlugin` always sets `SingleThreadedExecutor` on `FixedUpdate`. `KP_SIM_EXECUTOR` and
`DevOptions::sim_executor` are removed. The harness defaults to `Executor::Single`
(`KP_NET_EXEC=multi` still selects the multi-threaded one as an experiment).

### 9. `LocalTeam` out of the sim

| site (main) | after |
|---|---|
| `worm.rs:50/74` `AutoHold::initial(team, local)` = `team == LocalTeam` | `AutoHold::initial(ai_team)` = `!ai_team` (autohold.lua `isAiTeam`), from `AiSeats` |
| `ai/mod.rs:270/279/383` AI teams = "every homebase team except `LocalTeam`" unless `AiSeats` was inserted | `AiSeats` is always set by `begin_match` from the setup (`AiSeats::from_setup`: teams seated with AI players only); `ai_brain` no longer reads `LocalTeam` |

Remaining `LocalTeam` readers in `FixedUpdate`, and why they don't feed the sim:
- `update_cloak_visibility`, `update_fog_visibility`, `install_cloak_fade_materials`:
  `Visibility`, the `Spotted` marker, material swaps. They only change which tables units sit in,
  and the order-dependent systems no longer follow table order (§5). Measured below.
- `movement_system`'s arrival sound (`ground_move.rs`, `CGroundMoveType::Arrived`): audio only.
- `net::apply` seals the local player's commands as that player. That is the network identity,
  as intended.
- **M1:** `check_game_over` turns the `LocalTeam`'s defeat into `GameState::Defeat`, which
  gates the gameplay chain, and "Keep playing" resumes it. A peer that loses stops simulating.
  M1 decouples local defeat from the sim gate (the harness sidesteps it with
  `GameOverDismissed(true)`).

### Harness additions (`headless/tests.rs`, `headless/mod.rs`)

- `mod.rs`: the seed goes through `GameSetup::seed`; `test_seed` and `SimHarness`'s `Drop` are
  gone; `Executor::Single` is the default and `Multi` sets the multi-threaded executor
  explicitly. Nothing else changed.
- `KP_NET_LOCAL=<team>`: the second `determinism` run watches as that team (its `LocalTeam`;
  the seats stay all AI).
- `KP_NET_SELECT=1`: in the second run, the "player" selects a different seventh of the units
  every frame (`Selected` inserted / removed). This churns UI markers on sim entities.
- `KP_NET_WARMUP_MAP=<map>`: `rematch`'s earlier match on another map.
- `KP_NET_EXEC` defaults to `single`.

## Proof

All runs: release, single-threaded `FixedUpdate`, AI difficulty 2 unless noted, seed `0x5eed`
(24301).

| comparison | result |
|---|---|
| **determinism**: 4v4 Speed_Balls_16_Way, 18 000 ticks; 2nd run uneven pacing + `KP_NET_ID_SHIFT=101` | sim, positions, health, orders identical for all 18 000 ticks (fx from tick 2 051, as before: `Update` spawns some visuals) |
| the same + `KP_NET_SELECT=1` + `KP_NET_LOCAL=0` | identical for all 18 000 ticks |
| **8-AI FFA** Quad_Core, difficulty 4, 18 000 ticks, two processes (`KP_NET_TRACE`) | the two trace files are byte-identical (all 18 000 lines, fx hash included); final tick `b94ac530cdf139cf`, 155 units |
| FFA Quad_Core, 18 000 ticks, 2nd run 2–3 ticks/frame + `KP_NET_ID_SHIFT=37` + `KP_NET_LOCAL=5` + `KP_NET_SELECT=1` | identical for all 18 000 ticks (fx from 1 747); 155 units at the end |
| **rematch**, warmup 4 000 ticks (combat included) on the same map, 3 000 ticks | main: **1 561** (M0.1 re-run). `Local`s fixed, no canonical order: sim hash at **2 614** (units identical). All of M0.2: identical for all 6 000 ticks |
| rematch, warmup 4 000 ticks on **Central_Hub**, 6 000 ticks | identical for all 6 000 ticks |
| rematch, warmup 4 000 ticks, 18 000 ticks | identical for all 18 000 ticks |
| **local seat**: spectator vs team 0 (6 000 ticks, even pacing) | identical for all 6 000 ticks, fx included |
| spectator vs team 1, uneven pacing + ID shift, 9 000 ticks | identical for all 9 000 ticks |
| Hex_Farm_8 4v4, 9 000 ticks, uneven pacing + `KP_NET_SELECT=1` + `KP_NET_LOCAL=1` | identical for all 9 000 ticks (159 units at the end) |
| **two seeds** (24301 vs 24302), 4v4, cross-process traces | first sim divergence at tick **497** |
| `fixed_ambiguities` | **0** pairs (main: 10) |

### Sim cost of the canonical order

`match_stats`, 8-AI FFA Quad_Core, difficulty 4, 12 000 ticks. Both builds ran at the same time
on the loaded machine, so absolute times are about 3× X-sim's §4 numbers. "Before" is this
branch without the `NetId` sorts (spatial index only); the two games play out differently,
so the unit counts differ.

| build | units peak | sim tick median / p95 / p99 | last 2 min median / p95 |
|---|---|---|---|
| without the sorts | 425 | 3.24 / 6.05 / 7.29 ms | 4.24 / 6.74 ms |
| M0.2 (sorted) | 389 | 3.17 / 5.47 / 7.08 ms | 3.40 / 5.37 ms |

No measurable cost at this noise level: about 15 sorts of at most a few hundred `u32` keys per
tick.

## Open

- **M1:** local defeat flips `GameState`, which stops the loser's sim (§9).
- Visual-only entities still differ under uneven pacing (fx hash from about tick 2 000): the
  geovent smoke is spawned in `Update`. They don't feed the sim.
- `hover_air_system`'s `awaiting_air` and the scratch `Local`s stay (see §2); they hold nothing
  across a match swap that the sim reads.
- Per-draw keyed RNG streams (seed, tick, NetId, weapon, counter): not done; the loops are
  canonical instead (§2).
- New order-dependent systems must sort; nothing enforces it except the rematch,
  `KP_NET_SELECT` and `KP_NET_LOCAL` runs (and M0.4's archetype-perturbation fuzz test).

## Reproducing

```
# determinism, uneven pacing + entity-id shift (+ local seat, selection churn)
KP_NET_TICKS=18000 KP_NET_PACING=uneven KP_NET_ID_SHIFT=101 KP_NET_SELECT=1 KP_NET_LOCAL=0 \
  cargo test --release -p kernel-panic headless::tests::determinism -- --ignored --nocapture
# FFA across processes: run twice, then cmp the traces
KP_NET_MAP=Quad_Core KP_NET_TEAMS=8 KP_NET_PER_TEAM=1 KP_NET_DIFFICULTY=4 KP_NET_TICKS=18000 \
  KP_NET_RUNS=1 KP_NET_TRACE=ffa1.trace cargo test --release -p kernel-panic headless::tests::determinism -- --ignored --nocapture
# rematch after a combat warmup (optionally on another map)
KP_NET_WARMUP=4000 KP_NET_TICKS=6000 KP_NET_WARMUP_MAP=Central_Hub \
  cargo test --release -p kernel-panic headless::tests::rematch -- --ignored --nocapture
cargo test --release -p kernel-panic headless::tests::fixed_ambiguities -- --ignored --nocapture
```

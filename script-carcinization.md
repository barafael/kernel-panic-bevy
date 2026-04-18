# Script Carcinization

Findings from the investigation into removing the two scripting runtimes (Lua,
COB) from kernel-panic and re-implementing their responsibilities in Rust.

Both interpreters exist because the upstream Spring RTS engine needed to host
arbitrary user mods. kernel-panic ships a fixed set of maps and units, so that
genericity is pure cost — binary size, build complexity, missing behavior that
nobody wired up, and a scripting boundary that makes things harder to reason
about in the ECS.

---

## 1. Lua (mlua)

### 1.1 Where it lives

- Crate: [spring-map/Cargo.toml:13](spring-map/Cargo.toml#L13)
  — `mlua = { version = "0.11", features = ["lua51", "vendored"] }`
- Only Rust file that touches Lua: [spring-map/src/lua_heightmap.rs](spring-map/src/lua_heightmap.rs)
- Only call site: [spring-map/src/lib.rs:33-37](spring-map/src/lib.rs#L33-L37), inside `load_map()`.
- Data carrier: `LuaFile` struct in [spring-map/src/map_types.rs:279](spring-map/src/map_types.rs#L279),
  populated by [spring-map/src/sd7_archive.rs](spring-map/src/sd7_archive.rs).

All other "Lua" references in the Rust code are documentation comments citing
upstream `.lua` files as the source-of-truth for already-ported algorithms
(areadenial, infection, network_buffer, network_dispatch, kernelboost,
sidedata). No runtime Lua exec happens for those — they are plain Rust
systems now.

### 1.2 What the current stub does

`apply_lua_heightmap_gadgets` filters the archive's `.lua` files for any whose
source contains `setheightmap`, creates a fresh `Lua::new()` per gadget, stubs
a minimal Spring API (`Spring.SetHeightMap`, `Spring.SetHeightMapFunc`,
`Spring.GetGroundHeight`, `Spring.Echo`, `Spring.GetMapOptions`,
`Spring.GetAllFeatures`, `Spring.IsCheatingEnabled` etc., `Game.mapSize{X,Z}`,
`gadgetHandler:IsSyncedCode()`), loads the gadget source, and calls
`gadget:Initialize()` **exactly once**. The VM is dropped immediately after.

There is no `GameFrame` hook, no update tick, no storage of the VM beyond the
function scope. Grepping `spring-map/` for `GameFrame`, `gadget:Update`,
`game_frame` returns zero hits.

### 1.3 Which maps actually use Lua

Of the 16 shipped maps in [kernel-panic/assets/maps/](kernel-panic/assets/maps/),
**three** contain Lua (extracted and scanned directly from each archive):

| Map | Lua gadget | LOC | `setheightmap`? | Runs today? |
|-----|-----------|-----|-----------------|-------------|
| `Palladium_0.5_(beta).sd7` | `LuaRules/Gadgets/PalladiumHeight.lua` | 529 | yes | Phase 1 only |
| `Hex_Farm_8.sd7` | `LuaRules/Gadgets/HexFarm8.lua` | 2810 | yes | Phase 1 only |
| `pacman.sd7` | `LuaRules/Gadgets/tp_wraparoundmap.lua` | 127 | no | Not at all |

Palladium is documented in detail below because it's the archetype and the
simplest; Hex_Farm_8 follows the same shape but larger; pacman is a separate
kind of gadget (no heightmap writes, so the current filter skips it).

The three gadgets all have **multi-phase** structure with runtime callbacks
the current Rust stub silently drops. Only `Initialize()` runs; any
`GameFrame`, `UnitCreated`, `UnitDestroyed`, `UnitFinished`,
`AllowUnitBuildStep`, `Explosion`, `GameStart`, `RecvLuaMsg`,
`DrawWorldPreUnit` callbacks are never invoked.

#### Phase 1 — `Initialize()` (load-time, currently running)

`PalladiumHeight.lua:234-400` (`MakePalladiumBeginningHeightMap`) stamps:
- 4× mirrored **static platforms** (L246-248) at fixed tile rects, heights
  320/448/384/256 with per-side ramp/pyramid parameters.
- 4× mirrored **dynamic platforms** (L250-253) — the four "crosses" at
  (58..68, 13..23), (32..42, 35..45), (75..85, 50..60), (45..55, 75..85),
  each with ramps on all four sides.
- Ramps, ramp-sides, corner pyramids, then writes every cell through
  `Spring.SetHeightMap`.

This is what produces Palladium's characteristic platforms-with-ramps
terrain. The current Rust path handles this correctly.

#### Phase 2 — `GameFrame(f)` (per-tick, missing)

`PalladiumHeight.lua:414-423`:
- At `f == 3`: `ResetFeaturesHeight()` destroys every feature on the map and
  recreates it at the new ground height — features settle after Initialize.
- Each frame: drains a `MustRedo` queue, calling `ReMakeDynamicPlatforms(p)`
  for each entry to rewrite heightmap cells + manage `ReTexturedAreaList`.
  The queue indirection exists only because Spring requires heightmap writes
  to happen inside `SetHeightMapFunc`.
- Publishes `_G.PalladiumMap = { ReTexturedAreaList }` to the unsynced side
  every frame.

#### Phase 3 — `UnitCreated` / `UnitDestroyed` (event-driven, missing)

`PalladiumHeight.lua:425-466`. This is the core gameplay:

When a **large static structure** (Lua condition: `ud.canMove == false and
ud.xsize >= 5 and ud.zsize >= 5` — Spring footprint units, where 1 unit =
16 elmos, so roughly ≥ 80×80 elmos) is created or destroyed, and its position
is inside one of the four dynamic cross rects, compute `k` (1..8) from the
unit's heading (which cardinal ramp it faces), then based on the map option:

- `0` Static — no change
- `1` 1-way — keep only the faced ramp; lower the other three
- `2` 2-way — keep faced + opposite
- `3` 3-way — lower only the opposite ramp
- `4` All Up — restore all four ramps

Defaults: `palladium_unit_created = 1` (build → close off 3 ramps),
`palladium_unit_destroyed = 2` (rubble → reopen 2 ramps).

Each event pushes a `p2 = {x1,z1,x2,z2,h,r1,r2}` record onto `MustRedo`; the
next `GameFrame` lowers/raises the ramp wedges and updates the retexture
overlay.

`ReMakeDynamicPlatforms` (L147-230) is the worker: where `r2[k] >= 2` the
ramp stays full height, otherwise it lerps to `BaseHeight = 128` (the valley
floor) and the ramp sides re-blend. Retexture overlay rects get added for
lowered ramps, removed for restored ones.

#### Unsynced — `DrawWorldPreUnit()` (cosmetic, missing)

`PalladiumHeight.lua:488-525`. Draws `maps/32x32kpybs.png` ground-quad
overlays over lowered ramps using `gl.DrawGroundQuad`, with
visibility/depth-test tricks. Driven by `palladium_draw_ground_quad` option.
Pure polish — skip for v1.

### 1.4 Consequence today

On Palladium right now, placing a big structure on a cross does **nothing**;
the ramps stay put. The "changes over time" behavior is already silently
missing. Phase 1 is all that runs.

### 1.5 Removal plan

Two viable shapes:

**A. Bake + drop, skip the dynamic behavior.**
1. Run `apply_lua_heightmap_gadgets` once offline per shipped map, snapshot
   `parsed.heights` into a baked artifact (rewrite `.smf` heightmap block
   in-place, or sidecar).
2. Delete `lua_heightmap.rs`, drop `mlua` from `spring-map/Cargo.toml`, drop
   `LuaFile` + `lua_files` from `ExtractedArchive`, drop `.lua` extraction in
   `sd7_archive.rs`, remove the call in `lib.rs`.
3. Accept Palladium's dynamic ramps stay missing (they already are).

**B. Bake + port the dynamic behavior to Rust.**
Same as A, then additionally:
1. Hardcode the four dynamic-platform rects and ramp-direction wedges as a
   Rust const table (4 entries × 4 ramps each).
2. Bevy system on `UnitSpawned` / `UnitDestroyed` for static units with
   footprint ≥ 5×5: test against the four rects, compute ramp index from
   facing, mutate the heightmap + collision + ground mesh per the mode.
3. Expose the three options as map-specific config (const or per-map
   `MapRules` struct).
4. Skip the retexture overlay initially.

~200 lines of bounded, deterministic Rust for B. No Lua runtime needed either
way. B restores a gameplay feature the port lost.

---

## 2. COB (spring-cob)

### 2.1 Where it lives

- Crate: [spring-cob/](spring-cob/) — ~1,288 LOC across:
  - `vm.rs` — 794 LOC, thread scheduling, 30 opcodes, stack/call frames
  - `cob_file.rs` — 324 LOC, binary .cob parser (sections, string table,
    script metadata, piece names)
  - `opcodes.rs` — 93 LOC, opcode enum + repr mapping
  - `unit_values.rs` — 59 LOC, well-known Spring value keys
    (BUILD_PERCENT_LEFT, HEADING, …)
  - `lib.rs` — 18 LOC, re-exports
- 48 `.cob` files live in [upstream/Kernel-Panic/scripts/](upstream/Kernel-Panic/scripts/),
  loaded at runtime via `load_cob_cached` → `load_asset_from_disk`
  ([animation.rs:125-131](kernel-panic/src/units/animation.rs#L125-L131)).
  They are **not** in `kernel-panic/assets/` — removal does not need to
  prune shipped assets, and the game currently can't run without the
  `upstream/` submodule present.
- Human-readable `.bos` source files sit alongside the `.cob` binaries.

### 2.2 Integration surface — three touch points

Narrow. All uses in [kernel-panic/src/units/](kernel-panic/src/units/):

**1. Spawn** — [spawning/mod.rs:295-296](kernel-panic/src/units/spawning/mod.rs#L295)
Creates `CobVm::new()`, calls `Create()` script, resolves muzzle / gunbase /
hatch piece indices by name against the COB piece table.

**2. Events** — [script_triggers.rs](kernel-panic/src/units/script_triggers.rs)
plus a few adjacent sites. Fires scripts on state transitions. Complete
vocabulary invoked from Rust (grepped via `start_script\(&cob, "…"`):

- `Create` — spawning/mod.rs:296
- `Killed` — combat/lifecycle.rs:197 (args: `&[0, 0]`)
- `StartMoving` — script_triggers.rs:45
- `StopMoving` — script_triggers.rs:50
- `Activate` — script_triggers.rs:68
- `Deactivate` — script_triggers.rs:73
- `AimWeapon1` — script_triggers.rs:126 (args: `&[heading, pitch]`, Spring
  angular units: 65536 = 360°)
- `FireWeapon1` — script_triggers.rs:127
- `Open` — combat/aim.rs:118 (triggered by deploy state machine)
- `Close` — combat/aim.rs:112

Ten names total. No `QueryWeapon1` / `AimFromWeapon1` / `QueryPrimary` /
`AimPrimary` calls — muzzle piece is resolved heuristically by name match at
spawn, not by script callback. `QueryWeapon1` is referenced only in comments
([animation.rs:96](kernel-panic/src/units/animation.rs#L96)) noting that the
VM doesn't support the callout.

**3. Tick** — [animation.rs:228-449](kernel-panic/src/units/animation.rs#L228-L449)
`animation_system` in `GameplaySet::Animate` steps each VM per frame
(`animator.vm.tick(&cob, dt_ms)`), translates returned `AnimCommand`s
(Turn/TurnNow/Move/MoveNow/Spin/StopSpin/Show/Hide/Explode) into per-piece
interpolation state, applies Bevy `Transform` + `Visibility` each frame.
Handles Spring↔Bevy axis handedness via `cobwtf_*axis` functions at
animation.rs:149-177 (this is where the memory's per-op sign-flip rules
live: Turn → Z, Spin → X+Z, Move → X).

Completion events (`turn_finished`, `move_finished`) feed back into the VM so
`wait-for-turn` / `wait-for-move` opcodes wake.

### 2.3 BUILD_PERCENT_LEFT bridge

[animation.rs:179-218](kernel-panic/src/units/animation.rs#L179-L218),
`publish_unit_values`, ordered before `animation_system` in the same set.
Reads the `Emerging` component's `remaining/total`, converts to Spring's
100→0 scale, calls `animator.vm.set_unit_value(unit_values::BUILD_PERCENT_LEFT, percent)`.
Most `Create()` scripts loop on `while(get BUILD_PERCENT_LEFT)` to animate
emergence.

### 2.4 Partial Rust-side overrides already exist

Not a clean line — some animation is already Rust-authored, co-existing with
the VM:

- **Gunbase aim** — [combat/aim.rs:133+](kernel-panic/src/units/combat/aim.rs#L133)
  `aim_weapons_system` rotates the unit body toward `AimTarget` at the FBI
  `TurnRate` and writes the gunbase pitch **directly into**
  `CobAnimator::piece_rotations`, deliberately bypassing the COB
  `AimWeapon1` script. The docstring explains why: "our VM doesn't currently
  route HEADING reads/writes back to the unit transform, so the upstream
  `.bos` aim loop is inert". So the AimWeapon1 script still fires
  (script_triggers.rs:126) but its output would be inert; the Rust path is
  the real source of truth for gun orientation.
- **Deploy state machine** — same file, around aim.rs:107-122. The Rust
  `Deployable` state machine drives `Open` / `Close` calls into the VM,
  which in turn run the hatch animation in COB. The timing is Rust-owned.
- **Death particles** — [animation.rs:460-510](kernel-panic/src/units/animation.rs#L460-L510)
  spawn brief visual bursts, unrelated to piece animation.

Everything else — walk cycles, turret muzzle recoil, emerge, killed flinging
of pieces, factory animations — still comes out of the VM.

This matters for removal scope: the archetype list in §2.5 already has two
validated Rust-side precedents (aim, deploy), which makes path C a
continuation of work that is partly done rather than a net-new approach.

### 2.5 Three paths to removal

**A. Hand-port all 48 units to bespoke Rust animators.**
Delete spring-cob. Write a custom animator per unit. Highest fidelity,
highest effort — 48× small state machines plus timing tails (recoil, walk
phase sync, emerge cadence). Order-of-magnitude: weeks.

**B. Transpile .cob → Rust at build time.**
A `build.rs` reads each `.cob`, emits a generated Rust function per script
(branches/loops/waits as explicit state machines). Deletes `vm.rs` (794
LOC), keeps `cob_file.rs` (324 LOC) as a build-only dep. Preserves original
behavior mechanically. Generated code is ugly but faithful.

**C. 4–5 shared Rust animation archetypes + per-unit data tables. (recommended)**
Kernel-Panic's visual vocabulary is repetitive — most units fit one of:
- `EmergeUp { pieces, duration }` — every Create() is "pieces rise while
  BUILD_PERCENT_LEFT counts down"
- `Walk { leg_pieces, cycle_ms }` — StartMoving/StopMoving leg wobble
- `TurretAim { base, barrel }` — AimWeapon1 two-axis tracking
- `MuzzleFlash { muzzle, recoil_piece }` — FireWeapon1 kick + flash
- `KilledExplode { pieces, impulse }` — Killed() fling parts

Each unit becomes a `UnitAnimations` data row picking which archetypes apply
plus piece indices. ~5 Rust systems cover ~40/48 units; the handful of
oddballs (alice, assembler, carrier, special weapons) get bespoke code.

Effort estimate: ~1–2 weeks, mostly in step 3 (tuning timings to look
right).

### 2.6 Recommendation — path C

The engine-generic VM is doing more than needed. Spring had to interpret
arbitrary mods; kernel-panic knows its 48 units, they fall into a handful of
archetypes, and the `.bos` sources are readable so classifying them is an
afternoon of work.

Removal sequence:
1. Read all 48 `.bos` files at [upstream/Kernel-Panic/scripts/](upstream/Kernel-Panic/scripts/),
   bucket by archetype. Note the oddballs.
2. Implement 4–5 generic animation systems in Rust keyed on marker components.
3. Author `unit_animations: HashMap<UnitKind, AnimationProfile>` data table
   (piece indices + timings).
4. Hand-code the oddballs.
5. Rip out animation_system's VM tick loop, script_triggers.rs, `CobAnimator`
   / `CobVm` / `CobFileCache`, the `BUILD_PERCENT_LEFT` publisher, the
   spring-cob crate, the `.cob` assets.

Binary shrinks, data pipeline simplifies, animations become tweakable in
Rust without re-compiling `.bos`.

---

## 3. Combined removal order

Lua removal is hours; COB removal is 1–2 weeks. Independent changes —
different crates, different subsystems, different touch points. Can be done
in either order.

Suggested sequence:

1. **Lua, path B** — bake Palladium into a static .smf + port the dynamic
   ramps to a Bevy system. Drop `mlua` dep, delete `lua_heightmap.rs`, drop
   `LuaFile` / `lua_files`. Restores a lost gameplay feature in the process.
2. **COB, step 1** — classify 48 `.bos` files into archetypes, commit the
   bucket list.
3. **COB, steps 2–5** — implement the 4–5 archetype systems, author the
   data table, handle oddballs, delete spring-cob.

After both: two fewer runtime interpreters, the binary is plain Rust + Bevy
with a small Spring file-format library for `.smf` / `.sd7` / `.cob`-free
unit data, and the `upstream/` submodule becomes pure reference
documentation.

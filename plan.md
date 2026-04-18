# Kernel Panic — Remaining Features Plan

## Current State (April 2026)

**6 crates, ~20.4k lines, 133 tests, all passing.**

Working: map loading (14 maps including Showcase), original textures, S3O models, 3
factions (21 unit types — Flow/Gateway added), FBI-loaded unit stats (no hardcoded
values), TDF-loaded weapon stats, unit production with build queues and multi-emitter
build rays per faction, two-phase spawn with emerge lead-time (Rise/Fade), factory
building on datavents via mobile constructors (BeginPlacementEvent → PlacementMode ghost
→ PendingBuild → Constructing → spawn), selection with material-brightened highlight and
world-space health bars, movement with QTPFS pathfinding + flying-unit skip-navgrid +
ground terrain clamping, basic combat with infection/virus spawning, death animations
(COB `Killed()` + particle bursts), win/loss detection with GameState reset on map cycle,
COB script integration (movement/production/weapon/open-close callbacks, Scriptor linear
constants, piece remap by name, empty signal mask on start-script threads), weapon FX
(beams, projectiles, melee flashes), minimap, HUD with build menu, RTS camera with
map-sized fog/far-plane, Lua heightmap gadgets, map cycling.

---

## 1. Combat Mechanics

### 1.1 Armor-Type Damage Multipliers — ✅ DONE

`ArmorClass` enum (9 variants from upstream `armor.txt`), `UnitKind::armor_class()`
mapping, and `apply_damage` resolves via `DamageMap::for_type`. Logic Bombs do 3000 vs
Worms, Minekiller one-shots mines, RPS multipliers live for any weapon that defines
per-armor entries.

### 1.2 AOE / Splash Damage — ✅ DONE

Weapons with `area_of_effect > 48` splash-damage every unit in radius with linear
falloff via `edge_effectiveness`. Primary target still takes full damage; threshold
keeps single-target weapons (BugShot AoE=8, VirusBeam AoE=16) on the O(1) path.

### 1.3 Burst Fire — ✅ DONE

`BurstFire` component holds remaining shots + per-shot interval; `tick_burst_fire`
releases follow-up shots at `burst_rate` spacing. Aim point frozen at trigger. Active
on FlowMissile (burst=2) and MegaBeam (burst=4).

### 1.4 Command-Fire Gating — ✅ DONE

`combat_system` now skips weapons with `commandfire=1` during auto-target selection.
NX Flag, Obelisk's Infection gas, and Bug's FakeBugCannon will re-enter combat via the
§3.5 command-fire ability framework.

### 1.5 DOS Paralyze / Stun — ✅ DONE

Paralyzer weapons (DOS_Beam) accumulate on `StunCharge` instead of dealing HP damage;
when charge ≥ max_health, the unit is `Stunned` for `paralyzetime` seconds. Stunned
units skip combat and movement. Charge decays exponentially between hits. Bits fall
over in one hit; Bytes need many hits. Byte closed-state armor loss on stun (plan item
4) is deferred with the rest of the Byte-state COB integration.

### 1.6 Damage Modifiers — ✅ Partial

Done: `avoidfriendly=1` and `noselfdamage=1` filter the splash set; FBI `DamageModifier`
applied to every damage event (Socket/Window/Port/Firewall take 4×, homebases + Byte
are near-immune). Infection is already wired (`Infected` + `VirusSpawnQueue`).

Deferred: Byte closed-state armor (needs COB `SetUnitValue(ARMORED, ...)` integration
and a closed/open state distinct from the Pointer deploy cycle). `collidefriendly` on
projectile physics (weapons don't have projectile collision yet).

### 1.7 Auto-Heal — ✅ DONE

`IdleTimer` component tracks seconds since last damage / move order / aim target.
Once `IdleTime` (sim frames, 30/s) elapses, `auto_heal` regens at `IdleAutoHeal` HP/s.
Wires Byte's 400 HP/s after 20s, Worm's 300 HP/s after ~13s, homebase regen, etc.

---

## 2. Missing Units & Stat Corrections

### 2.1 New Unit Types — ✅ DONE

All roster gaps closed. SIGTERM turned out to be a weapon (fired by Signal /
Terminal air-strike) rather than a standalone unit, so the final additions are:

| Unit | Faction | Role |
|------|---------|------|
| Flow | Network | Air assault (added earlier) |
| Gateway | Network | Armed mobile constructor (added earlier) |
| Trojan | Hacker | Mobile constructor |
| Terminal | System | Special building, launches SIGTERM air strikes |
| Obelisk | Hacker | Special building, infection gas artillery |
| Debug | Shared | One-shot mine/wall clearer (FBI `mineblaster`) |
| BadBlock | Shared | Cheap destructible wall |

Constructor build lists updated per `SIDEDATA.TDF`. Terminal / Obelisk / Debug /
BadBlock still need art and ability wiring — that lives under §3.5 (command-fire)
and §4.3 (explosion effects).

### 2.2 Stat Corrections — ✅ DONE

Unit stats are now loaded at runtime from upstream `.fbi` files via `UnitRegistry`. The
hardcoded `UNIT_STATS` array has been removed. All HP, speed, build time, weapon binding,
and model filename values now come from FBI data. `spring-tdf` has a `UnitDef`/`UnitDefs`
parser, and `kernel-panic` has a `UnitRegistry` resource (parallel to `WeaponRegistry`).

Remaining stat-adjacent work:

- Connection HP reads 15k from FBI (it's a mobile unit in KP), not 40k — verify if this
  is correct or if sidedata.tdf overrides it
- `mesh_scale` and `buildable_units()` build menus are still hardcoded in game code (not
  in FBI files)

---

## 3. Faction Mechanics

### 3.1 Factory Building on Datavents — ✅ DONE

Full pipeline landed: `BeginPlacementEvent` → `PlacementMode` ghost preview →
`BuildAt` queued command → `PendingBuild` → constructor walks to site and emits
build-laser rays from multi-emitter factory pieces → `Constructing` → two-phase spawn
with emerge lead-time (Rise for factories, Fade for infantry) → optional
`Emerging.rally_point` drives post-emerge movement.

Remaining: Terminal/Obelisk/Firewall special-building abilities — deferred to §3.5
(Command-Fire Framework).

### 3.2 Network Packet Buffer & Teleportation — ✅ DONE

`PacketBuffer` resource holds per-team counters. Ports top up every ~5.5s; Port no
longer has a direct production queue. `T` hotkey with a teleporter (Port or
Connection) selected dispatches up to 12 Packets in a ring around it toward the
cursor. `R` hotkey with Packets selected absorbs any within 150 elmos of a friendly
teleporter back into the buffer. Freshly-dispatched Packets carry a 6s
`PacketSpawnStun` blocking re-entry.

Deferred: HUD buffer counter + dispatch command button (mechanic is live via hotkey).

### 3.3 Cloaking — ✅ DONE

`Cloaked` marker on Worm and Logic Bomb; `update_cloak_visibility` toggles
`Visibility::Hidden` for enemy-owned cloaked units unless a detector (Assembler/
Trojan/Gateway's FBI `RadarDistance`) is within range. Friendly cloaked units stay
visible so players can manage their own Worms. Full per-team vision is deferred to
§6.

### 3.4 Bug ↔ Exploit Morph — ✅ DONE

`E` hotkey transforms Bug↔Exploit in place with proportional HP. `WeaponDef` gained
`dyn_damage_exp/dyn_damage_range/dyn_damage_inverted/proximity_priority`;
BugCannon's Inverted=1/Range=700 scales damage linearly with distance. Target
selection picks the *farthest* enemy when `proximity_priority < 0`, matching
Exploit's anti-push role.

### 3.5 Command-Fire & Area Denial Framework — ✅ Partial

Framework in place: `CommandFireEvent` → `process_command_fire` spawns an
`AreaDenialZone` entity, `tick_area_denial` deals dps*dt to units in radius
with friendly-fire + infection flags, `CommandFireCooldown` gates recasts. Q
hotkey (interaction::ability) fires the selected caster's ability at the
cursor.

| Unit | Ability | Status |
|------|---------|--------|
| Pointer | NX Flag (r=120, 100 dps, 60s, friendly-fire) | ✅ wired |
| Obelisk | Infection Gas (r=400, 120 dps, 13s, infects) | ✅ wired |
| Firewall | Reflector Shield (r=300, 20s, 50% reduce + 50% reflect) | ✅ wired |
| Terminal | SIGTERM airstrike | needs air-bomber spawn |
| Byte | Mine Launcher | needs HP-cost + Logic-Bomb volley |

### 3.6 Infection Chain Refinement — ✅ DONE

`weapon_infection_duration()` maps the four infecting weapons to their upstream
frame-count windows. `apply_damage` keys infection on the weapon name (not the
attacker unit kind) so only Wormsplash / VirusBeam / VirusDeath / Infection trigger
it — direct Wormbite no longer infects, matching upstream. `death_system` sprays
VirusDeath at a dying Virus's corpse so the infection chain spreads via AoE.

### 3.7 Kernel Boost / Production Scaling — ✅ DONE

`production_system` multiplies homebase build progress by
`(1 + small_building_count × 0.2)` per team, reusing
`network_buffer::is_small_building` as the predicate.

### 3.8 Flow Dynamic Speed & Air Movement — ✅ DONE (mostly)

- ✅ Flying flag + `can_fly()` / `cruise_alt()`; flying units skip nav grid.
- ✅ Per-Flow `SpeedBoost` component refreshed every second from team small-building
  count, added on top of the registry's base speed in movement.
- ❌ Ground units with `NoChaseCategory=VTOL` — not yet; combat_system still targets
  Flows indiscriminately.

### 3.9 Mines & Walls — ✅ Partial

- ✅ **Logic Bomb**: already cloaked (§3.3); `tick_kamikaze` detonates it when an enemy
  enters the 64-elmo radius, queuing a `logic_bomb`-weapon self-hit so the existing
  splash + armor-class pipeline handles the blast (3000 vs Subterranean).
- ✅ **BadBlock**: spawned at 100 HP; being a building it blocks movement via the
  existing collision pipeline. Crushable by Bytes is deferred.
- ❌ **Debug**: the one-shot Minekiller placement/trigger flow isn't wired (upstream's
  Launcher gadget delivers Debugs via a MineLauncher weapon, not direct construction).

---

## 4. Weapon Visuals & Animation

### 4.1 Beam Textures (Important)

`arrow.tga` (Bit's `>>>>>`), `dosray.tga` (DOS's binary stream), `bytemegabeam.tga`
(Byte's grid), `circle.tga` (Bug's blob) exist on disk but beams render as flat-colored
cuboids.

- `scrollspeed` (DOS_Beam=4) should animate texture along the beam
- `beamdecay` should fade beams (PacketBeam, GaussCannon)
- `intensity` should control brightness (GaussCannon=0 flat, BuildLightning=5 bright)

### 4.2 Projectile Models (Important)

`model=octashot.s3o` (Pointer), `model=network_medium_missile.s3o` (Flow) exist but code
renders placeholder cubes in `spawn_projectile`.

### 4.3 Explosion / Impact Effects — ✅ Partial

`ImpactBurst` spawns a color-coded sphere at every hit, sized by the weapon's
`area_of_effect` and tinted by its `rgb_color`. Covers beams, projectiles, burst-beams,
and AoE splashes with a single code path; a pragmatic substitute for the full CEG
particle system. The ~40 upstream explosion TDFs are still not parsed (full per-weapon
CEG emitter stacks remain deferred).

### 4.4 Projectile Trails & Smoke (Low)

`cegTag` and `smoketrail=1` parsed but unused. BugCannon, FlowMissile, Geometric should
have visible trails.

### 4.5 Muzzle Flash (Low)

No visual feedback at the firing unit (except melee flash for Wormbite).

### 4.6 COB `QueryWeapon1` Callback (Low)

Returns weapon emit-point position. Currently beams originate from unit center instead of
the model's barrel/turret piece.

### 4.7 Shield System — ✅ DONE (mechanic; visual deferred)

`ShieldState` component holds radius / max_power / current_power / regen. `apply_hit`
soaks damage through the shield before it hits Health or StunCharge; with upstream's
`shieldpower=0` convention the shield is effectively infinite, matching the role of
minifac and homebase shields. `regen_shields` ticks finite shields toward max.

Remaining: visible shield sphere rendering (`shieldgoodcolor`/`shieldbadcolor`/
`shieldalpha`). Projectile *interception* (as distinct from damage absorption) waits
on §4.2 projectile physics.

---

## 5. AI Opponent

### 5.1 Basic AI — ✅ DONE

`ai_brain` ticks once/second per non-player team:

- **Build**: production queues stay ≤3 deep, mixing basic combat units with a
  constructor every fifth order.
- **Expand**: any idle friendly constructor gets routed to the nearest unclaimed
  datavent (no friendly building within 120 elmos) with a `PendingBuild` for the
  faction's secondary factory.
- **Defend**: any non-friendly unit within 700 elmos of a homebase triggers recall
  — idle combat units target the homebase instead of pushing out.
- **Attack**: with ≥8 idle combat units and no home threat, everybody charges the
  nearest enemy homebase.

### 5.2 Difficulty Levels (Low)

Easy (slower production), Normal, Hard (faster production, better targeting, multi-prong).

---

## 6. Fog of War (Medium)

Per-unit sight radius. Enemy units outside friendly sight are hidden. Terrain revealed
permanently once scouted. Worms invisible unless within enemy sight AND attacking.

Implementation: per-team visibility grid, shader/material override for hidden units.

---

## 7. Audio (Low)

`sound_start`/`sound_hit` parsed in every weapon but no audio system exists. Original KP
sound files are in the mod archive. Use `bevy_audio` for spatial sound: weapon fire, death,
unit acknowledgements, ambient, UI feedback.

---

## 8. WASM / Web Build (Medium)

### 8.1 Pre-Bake Map Format

Build step converts .sd7 → flat binary (heightmap + texture PNG + features + metadata).
WASM app loads via HTTP fetch — no filesystem, no 7zip. The SMF/SMT parsers already work
on `&[u8]`.

### 8.2 Deployment

GitHub Actions workflow: build WASM → `wasm-bindgen` → deploy to GitHub Pages with one
pre-baked map (Marble Madness).

### 8.3 Compatibility Constraints

- `sevenz-rust` won't compile to WASM — pre-baking avoids this
- `mlua` may need WASM special handling — pre-apply Lua gadgets during bake
- `spring-map` needs `#[cfg(not(target_arch = "wasm32"))]` on filesystem code

---

## 9. Multiplayer (Low)

Requires all gameplay to be deterministic first. `lightyear` or `bevy_replicon` for state
replication. Lockstep or server-authoritative. Lobby system with map/faction selection.

---

## Recommended Implementation Order

Done since last plan: §3.2 packet buffer, §3.3 cloaking, §3.4 Bug↔Exploit morph,
§3.5 command-fire (NX Flag + Infection + Firewall), §3.6 infection refinement,
§3.7 Kernel Boost, §3.8 Flow speed, §3.9 Logic Bomb detonation, §4.3 impact bursts,
§4.7 shields, §5.1 AI Expand + Defend.

| # | Item | Section | Rationale |
|---|------|---------|-----------|
| 1 | Terminal SIGTERM + Byte MineLauncher | 3.5 | Last command-fire gaps |
| 2 | Debug (Minekiller) placement | 3.9 | Last mine-kit gap |
| 3 | Beam textures + projectile models | 4.1–4.2 | Visual polish |
| 4 | Fog of war | 6 | Full visibility system |
| 5 | WASM pre-bake + deploy | 8 | Browser-playable |
| 6 | Audio | 7 | Weapon sounds highest priority |
| 7 | Multiplayer | 9 | Endgame feature |

---

## 10. UX / Polish Backlog

Collected from the in-flight todo list. Not blocking; each is its own
focused chunk when we're ready.

### 10.1 Selection / input

- Double-click to select every visible unit of the same kind.
- Unit groups: `Ctrl-1..9` to assign, `1..9` to recall (and center camera
  on the group).
- Builder placement UX pass — match the original Kernel Panic cursor
  behaviour when picking a datavent.

### 10.2 Visual polish

- Dedicated UI pass: match the original KP layout / styling as closely as
  possible (extends §4 work that's been pragmatic so far).
- Fix the skybox to match original KP.
- Audit the post-processing pipeline vs. Spring / upstream KP — identify
  what we're missing and what's cheap to add.
- Decide `glyph_zero` / `glyph_one`: keep the procedural baseline or
  ship a sprite asset. Benchmark first.

### 10.3 Fog-of-war clarification

§6 covers the full fog system; the MVP the original uses is simpler —
the entire map is always visible, but buildings / units are only
revealed when they've been built (i.e. no Line-of-Sight; it's a
"memory" system, not per-frame vision). Worth implementing that
cheaper variant first before the full per-team vision grid.

### 10.4 Profiling / performance

- Run `cargo-flamegraph` against a 3-team full map for 30 seconds;
  chase anything >0.5% of Update-phase time.
- Survey "what performance tweaks does Spring get away with that we
  can apply?" — engine comparison pass.
- General Bevy perf pass: archetype churn, command flush cost, render
  node count.

### 10.5 Testing

- Run `cargo llvm-cov` across the workspace and fill coverage gaps that
  would be high-value (weapon category / damage resolution edge cases,
  AI phase transitions, shield soak + Protected interactions).

---

## Crate Structure

```text
kernel-panic/          (binary — Bevy game app)
spring-tdf/            (lib — TDF format parser: weapons, units/FBI, generic sections)
spring-map/            (lib — SMF/SMT/SD7 map loader)
spring-unit-mesh/      (lib — S3O model parser)
spring-cob/            (lib — COB bytecode VM)
spring-pathfinding/    (lib — QTPFS quad-tree pathfinding)
```

Clean separation between engine-agnostic parsers (`spring-*`) and the Bevy game. The
`spring-*` crates have zero Bevy dependency and are independently testable.

**Issue:**

- [ ] No shared types crate — if `spring-tdf`'s `DamageMap` ever needs to understand
  `kernel-panic`'s `ArmorClass`, a shared types crate (or trait-based bridge) will be needed.
  (Remaining crate-structure issues moved to Technical Debt → Architecture.)

---

## Technical Debt

### Architecture

- [ ] `selection.rs` is 662 lines handling 6+ concerns (hover, click/drag, right-click
  commands, material highlight, health bars, move indicators) — split into focused modules
- [ ] `spawn_unit` takes 12 parameters — group into a Bevy `SystemParam` bundle
  that can be re-used by `map_loading::load_map`, `morph::process_morph`,
  `network_buffer::process_dispatch`, and the placement systems (they all
  thread the same 6–7 asset/cache resources)
- [ ] `buildable_units()` in `hud.rs` and `default_production()` in `production.rs` encode
  overlapping "what can X build?" data — consolidate into a shared source
- [ ] `movement.rs` uses `Option<ResMut<NavGrid>>` — consider making NavGrid always present
- [ ] `spring-pathfinding` is runtime game logic, not a format parser —
  rename to `qtpfs` or fold into `kernel-panic`
- [ ] `spring-map::smd_parser` duplicates ~40 lines of TDF parsing that
  now lives in `spring-tdf` — refactor to depend on `spring-tdf::Tdf::parse()`
- [ ] `kernel-panic` is a monolith — as AI, networking, audio, and fog
  of war land, the single binary crate will become unwieldy. Bevy
  plugins are the natural splitting point.

### Performance

- [ ] HUD systems despawn+respawn entire UI tree every frame (~30–50 entities) — use change
  detection (`Changed<Selected>`, `Changed<Health>`, `Changed<Producer>`) to update in-place
- [ ] `update_unit_highlight` clones and re-adds a `StandardMaterial` per selected/hovered
  unit every frame, leaking orphaned handles — cache per-faction+brightness
- [ ] `despawn_health_bars` is O(n×m) — use `HashSet` of removed units or query children
- [ ] Melee flash and projectile materials created per-attack instead of cached — extend
  `BeamMaterialCache` to cover all weapon FX
- [ ] Animation system allocates `Vec<(i32, i32)>` per animator per frame — use `SmallVec`
- [ ] Per-frame `UnitRegistry` lookups for immutable data (speed, weapon name) — cache as
  ECS components at spawn time (e.g. `Speed(f32)`, `WeaponBinding(&str)`)
- [ ] `AttackEvent::weapon_name` is `String` (heap alloc per attack) — introduce a `WeaponId`
  newtype (interned string or index into `WeaponRegistry`) so attack events carry a cheap
  `Copy` identifier
- [ ] `UnitRegistry::weapon()` returns raw TDF section name strings — return
  `Option<&WeaponDef>` directly so callers never see string keys, eliminating empty-string
  checks in combat.rs and hud.rs

### Testing & Tooling

- [ ] Run `cargo llvm-cov` workspace-wide; fill high-value coverage gaps
  (weapon category / damage resolution edge cases, AI phase transitions,
  shield soak + Protected interactions).
- [ ] Flamegraph a 3-team full map for 30s; chase anything >0.5% of
  Update-phase time.
- [ ] Survey which performance tricks Spring / upstream KP apply that
  we could adopt.

### Upstream-waiting workarounds

- [ ] `main.rs` carries three `TODO(windows-resize)` markers: Vulkan
  backend preference, `PresentMode::AutoNoVsync`, and a 320×240 resize
  floor. They compensate for the Bevy 0.18 + wgpu ~24 "resize freeze
  then crash" behaviour on Windows (DX12 swapchain reconfigure hang
  inside the Win32 modal loop + wgpu panicking on 0×0 surface
  reconfigure against HDR+Bloom intermediate targets). Revert each
  piece independently once the fix lands upstream — search Bevy's
  `Platform-Windows` issues and gfx-rs/wgpu for "DX12 resize hang",
  "WM_ENTERSIZEMOVE", and "surface reconfigure 0x0".

### Gameplay Bugs

- [x] ~~`GameState` not reset on map cycling~~ — fixed in a50fe8b
- [x] ~~Rally point / delivery point for factories~~ — `Emerging.rally_point` wired
- [x] ~~Terrain height not sampled during movement~~ — ground clamping in recent walking
  improvements (5046fd2) + spawn clamp (6e043ba)
- [ ] No unit collision avoidance — units overlap when crowded (partial: walking improvements
  address some cases, revisit)
- [ ] Attack-move (`A` hotkey) is wired in HUD but handler is empty (TODO at `hud.rs:849`)
- [ ] Feature rotation (`MapFeature.rotation_degrees()`) parsed but never applied when
  rendering map features
- [ ] Weapons ignore line-of-sight — `lineofsight=1` parsed but units fire through terrain
- [ ] Weapons never miss — `tolerance` parsed but ignored; perfect accuracy on all weapons
- [ ] Factory spawn offset hardcoded in `production.rs` — should use COB `QueryBuildInfo`
  callback for correct build-pad position

### Incomplete COB VM

- [x] ~~Scriptor linear constant per unit~~ — fixed in 5ffd072
- [x] ~~Start-script threads inherit signal mask~~ — fixed in 855d506 (empty mask)
- [x] ~~Piece remap by name at spawn~~ — added in 9f8553f
- [x] ~~`BUILD_PERCENT_LEFT` bridge from CobVm to Create()~~ — wired through production
- [ ] `GET` / `GET_UNIT_VALUE` still return 0 for most values — only select `springdefs.h`
  constants mapped; expand as needed
- [ ] `EmitSfx` and `SetValue` opcodes still largely unimplemented
- [ ] `PieceIndex` component: inner value set but never read (only used as marker)

### Visual Gaps

- [ ] Death particle effect is a simple expanding sphere — original uses per-piece
  shatter/fall trajectories from CEG definitions
- [ ] `.smd` parser ignores `startposy` — only X/Z parsed for start positions
- [ ] Atmosphere (`fog_start`, `fog_color`, `cloud_density`) parsed from .smd but never
  applied
- [ ] Move indicator torus uses fixed size regardless of unit count or formation spread

### Resource Leaks

- [ ] Map cycling: old minimap image handle leaks when `MinimapState` is overwritten
- [ ] `SelectionVolumeMaterial` recreated on every spawn instead of truly cached

### Upstream Bevy Issues

- [ ] Device-loss cascade panic — tracked in [bevyengine/bevy#21753](https://github.com/bevyengine/bevy/issues/21753)
  (regression in 0.17+, still open as of 2026-04-18). Closing one game instance while a
  second is running can lose the GPU device on the survivor: `prepare_windows` fails with
  "Couldn't get swap chain texture", then every render system (`prepare_view_uniforms`,
  `prepare_material_bind_groups`, bloom uniforms, `prepare_previous_view_uniforms`, SSR,
  light-probe upload, fog, cluster prep) `.unwrap()`s a `None` buffer and brings the whole
  render world down. Same cascade is reported on wake-from-sleep and window-hide.
  Bevy 0.18.1 + wgpu 27.0.1 on Windows. No workaround; revisit when upstream ships a fix.

  Related upstream threads:
  - [#21753 — Game crashes when resuming from sleep](https://github.com/bevyengine/bevy/issues/21753) (primary — same cascade list)
  - [#11863 — Hiding window leads to swap chain timeout](https://github.com/bevyengine/bevy/issues/11863)
  - [#12887 — Off-screen window crashes App](https://github.com/bevyengine/bevy/issues/12887)
  - [#13150 — Swap chain texture timeout panic](https://github.com/bevyengine/bevy/issues/13150)
  - [#11734 — 2D examples crash on exit with "Couldn't get swap chain texture"](https://github.com/bevyengine/bevy/issues/11734)
  - [#3606 — Panic in bevy_render when acquiring next swapchain texture](https://github.com/bevyengine/bevy/issues/3606)
  - [#3288 — Pipelined 3D examples crash with AMDVLK on Wayland](https://github.com/bevyengine/bevy/issues/3288)
  - [PR #16964 — Move swap-chain acquire as late as possible in the pipeline](https://github.com/bevyengine/bevy/pull/16964) (partial mitigation already merged)

### Missing TDF Fields

`WeaponDef` is missing fields used by upstream weapons: `scrollspeed`, `burnblow`,
`noexplode`, `fixedLauncher`, `highTrajectory`, `leadLimit`, `weapontimer`, `dance`,
`dynDamageExp`, `dynDamageRange`, `proximityPriority`, `minIntensity`, `laserflaresize`,
`texture3`, `texture4`, `explosionspeed`, `manualBombSettings`, and shield rendering fields
(`visibleshield`, `shieldalpha`).

### Dead Code

- [x] ~~`CobThread::local_function_id()` in spring-cob `vm.rs`~~ — removed in 745c22d
- [x] ~~`load_smt_from_archive()`~~ — removed in 745c22d
- [ ] `CallFrame::function_id` in spring-cob — never read
- [ ] `_weapon` param in `spawn_melee_flash()` — unused

### Compiler Warnings

- [x] ~~clippy warnings across workspace~~ — fixed in e23c987
- [ ] `PieceIndex` field `.0` never read

### Naming

- [ ] `DEEP_FEATURES.md` calls the Network homebase "Carrier" in 3 places but code uses
  `UnitKind::Connection` — upstream `sidedata.tdf` also uses "carrier"; consider aligning

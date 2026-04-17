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
| Terminal | SIGTERM airstrike | needs air-bomber spawn |
| Firewall | Reflector Shield | needs shield system (§4.7) |
| Byte | Mine Launcher | needs HP-cost + Logic-Bomb volley |

### 3.6 Infection Chain Refinement — ✅ DONE

`weapon_infection_duration()` maps the four infecting weapons to their upstream
frame-count windows. `apply_damage` keys infection on the weapon name (not the
attacker unit kind) so only Wormsplash / VirusBeam / VirusDeath / Infection trigger
it — direct Wormbite no longer infects, matching upstream. `death_system` sprays
VirusDeath at a dying Virus's corpse so the infection chain spreads via AoE.

### 3.7 Kernel Boost / Production Scaling (Low)

Homebases get +20% production speed per small building owned by team. Snowball mechanic.

### 3.8 Flow Dynamic Speed & Air Movement (Low — Partial)

- ✅ Flying flag + `can_fly()` / `cruise_alt()` on `UnitRegistry`; flying units skip nav
  grid in `movement.rs`
- Flow speed scaling = base + (team small building count × 30) — not done
- Ground units with `NoChaseCategory=VTOL` won't pursue Flows — not done

### 3.9 Mines & Walls (Low)

- **Logic Bomb**: cloaked, proximity detonation (r=64), 900 dmg AoE 512, max 64 per player
- **Debug**: one-shot, 5k dmg vs mines, 20 vs everything else, AoE 512
- **BadBlock**: 100 HP wall, blocks movement but not projectiles, crushable by Bytes

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

### 4.3 Explosion / Impact Effects (Important)

Every weapon has `explosiongenerator=custom:...` but nothing is spawned on hit. ~40
explosion TDFs in `upstream/Kernel-Panic/gamedata/explosions/` are never loaded.

### 4.4 Projectile Trails & Smoke (Low)

`cegTag` and `smoketrail=1` parsed but unused. BugCannon, FlowMissile, Geometric should
have visible trails.

### 4.5 Muzzle Flash (Low)

No visual feedback at the firing unit (except melee flash for Wormbite).

### 4.6 COB `QueryWeapon1` Callback (Low)

Returns weapon emit-point position. Currently beams originate from unit center instead of
the model's barrel/turret piece.

### 4.7 Shield Rendering (Important — 6 units have shields)

Shield weapons are parsed but never rendered or applied. Kernel, Hole, Socket, Window, Port,
Firewall all have shield weapons (Connection does not).

- Visible shield sphere with `shieldradius`, `shieldgoodcolor`/`shieldbadcolor`, `shieldalpha`
- Shield power pool with `shieldpower` / `shieldpowerregen`
- Projectile interception

---

## 5. AI Opponent

### 5.1 Basic AI — ✅ Partial

Build + Attack phases land (`ai_brain` ticks once/second per non-player team):

- **Build**: keep each homebase's production queue topped up (≤3 items) with the
  faction's basic combat unit (Bit / Bug / Packet).
- **Attack**: when the team has ≥8 idle combat units, send every idle unit toward
  the nearest enemy homebase.

Deferred: **Expand** (send constructors to datavents to build secondary factories)
and **Defend** (recall units when homebase is under attack). Both layer cleanly on
the existing tick without restructuring.

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
§3.5 command-fire (NX Flag + Infection), §3.6 infection refinement, plus earlier
§1/§2/§5.1 work.

| # | Item | Section | Rationale |
|---|------|---------|-----------|
| 1 | Flow dynamic speed | 3.8 | Network late-game (air movement already partial) |
| 2 | AI Expand + Defend | 5.1 | Round out the basic AI once play-tested |
| 3 | Shield system | 4.7 | Unblocks Firewall reflector, homebase/factory shields |
| 4 | Firewall reflector shield | 3.5 | Network defensive ability |
| 5 | Kernel Boost | 3.7 | Snowball mechanic |
| 6 | Mines & walls | 3.9 | Tactical depth |
| 7 | Impact/explosion effects | 4.3 | Load explosion TDFs |
| 8 | Beam textures + projectile models | 4.1–4.2 | Visual polish |
| 9 | Fog of war | 6 | Full visibility system |
| 10 | WASM pre-bake + deploy | 8 | Browser-playable |
| 11 | Audio | 7 | Weapon sounds highest priority |
| 12 | Multiplayer | 9 | Endgame feature |

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

**Issues:**

- [ ] `spring-pathfinding` is misnamed — the other `spring-*` crates parse Spring file formats,
  but pathfinding is runtime game logic, not a format parser. Consider renaming to `qtpfs` or
  moving into `kernel-panic`
- [ ] `spring-map` smd_parser duplicates ~40 lines of TDF parsing logic that now lives in
  `spring-tdf` — refactor to depend on `spring-tdf::Tdf::parse()` instead
- [ ] `kernel-panic` is a monolith — as AI, networking, audio, and fog of war are added, the
  single binary crate will become unwieldy. The `units/` module already houses combat,
  production, animation, spawning, and weapon FX. Bevy plugins are the natural splitting point
- [ ] No shared types crate — if `spring-tdf`'s `DamageMap` ever needs to understand
  `kernel-panic`'s `ArmorClass`, a shared types crate (or trait-based bridge) will be needed

---

## Technical Debt

### Architecture

- [ ] `selection.rs` is 662 lines handling 6+ concerns (hover, click/drag, right-click
  commands, material highlight, health bars, move indicators) — split into focused modules
- [ ] `spawn_unit` takes 12 parameters — group into a Bevy `SystemParam` bundle
- [ ] `buildable_units()` in `hud.rs` and `default_production()` in `production.rs` encode
  overlapping "what can X build?" data — consolidate into a shared source
- [ ] `load_map_at_index` takes 9 parameters — consider a `MapLoadContext` struct
- [ ] `movement.rs` uses `Option<ResMut<NavGrid>>` — consider making NavGrid always present

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

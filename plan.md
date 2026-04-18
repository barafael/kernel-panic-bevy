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
applied to every damage event, with values under `0.01` treated as the upstream Spring
engine-disable hack and normalised to `1.0` (so Bits / Worms / Bytes / homebases take
normal damage) while explicit design values like Socket/Window/Port/Firewall's `4×`
pass through unchanged. Infection is already wired (`Infected` + `VirusSpawnQueue`).

Deferred: Byte closed-state armor (needs COB `SetUnitValue(ARMORED, ...)` integration
and a closed/open state distinct from the Pointer deploy cycle). `collidefriendly` on
projectile physics (weapons don't have projectile collision yet). Homebase + Byte
near-immunity — if we want that gameplay back, it should come from a dedicated
per-kind multiplier table, not the FBI engine-hack value.

### 1.7 Auto-Heal — ✅ DONE

`IdleTimer` component tracks seconds since last damage / move order / aim target.
Once `IdleTime` (sim frames, 30/s) elapses, `auto_heal` regens at `IdleAutoHeal` HP/s.
Wires Byte's 400 HP/s after 20s, Worm's 300 HP/s after ~13s, homebase regen, etc.

### 1.8 Combat parity gaps vs upstream Spring

Non-blocking today but each is a measurable divergence from how the engine
actually resolves combat. Tracked here so we can close them deliberately
rather than ship a "mostly Spring" feel.

- **Volumetric hit box** — our `collision_radius` is the larger footprint
  dimension × 4 elmos (i.e. `footprint/2`). Spring's `CCollisionHandler`
  tests actual per-unit volumes (sphere / cylinder / piece-AABB). This is
  the reason §spray_angle's primary-damage miss gate is reverted —
  footprint-radius underestimates the hit box by 2-3× and dropped hit
  rate to ~5%. Fixing this unblocks per-shot miss semantics, shield
  *interception* (§4.7), and projectile physics (§4.2).
- **Target weighting** — `CWeapon::TargetWeight` scores candidates by
  threat/priority/health/weapon-match. We pick nearest (or farthest for
  `proximity_priority < 0`). Observable as "my Bits don't prefer soft
  targets" and "the Pointer ignores the wounded Byte in favour of the
  full-HP one two elmos closer."
- **LOS blockers beyond terrain** — our `Heightmap::has_line_of_sight`
  only samples terrain Y. Spring's per-weapon `HaveFreeLineOfFire` also
  tests unit bodies and map features, so you can hide a Flow behind a
  Byte or shoot through a gap between buildings. We currently don't.
- **Impulse / knockback** — `CExplosionParams` carries an impulse vector
  and Spring pushes units on hit (visible as the classic "ragdoll fling"
  on big AoE). We apply damage silently — blasts feel dead.
- **Delayed damage queue** — `GameHelper` propagates large explosions
  over multiple sim frames so the blast wave arrives in sequence; we
  apply instantly. Only observable on very large AoE (Byte MegaBlast).

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

### 3.1 Factory Building on Datavents — ✅ Partial

Full pipeline landed: `BeginPlacementEvent` → `PlacementMode` ghost preview →
`BuildAt` queued command → `PendingBuild` → constructor walks to site and emits
build-laser rays from multi-emitter factory pieces → `Constructing` → two-phase spawn
with emerge lead-time (Rise for factories, Fade for infantry) → optional
`Emerging.rally_point` drives post-emerge movement.

Remaining:

- Terminal/Obelisk/Firewall special-building abilities — deferred to §3.5
  (Command-Fire Framework).
- **Building-placement slope check** — each building's FBI `MaxSlope`
  (Socket/Firewall/Terminal/Obelisk=10, BadBlock=32, Kernel/Hole=60)
  governs whether a builder can drop the ghost there. Currently the
  placement ghost only checks vent overlap + `VentClaim` (not slope),
  so on hilly maps you can snap a factory onto an angled floor. Fold
  a `heightmap.max_slope_in_footprint(site, footprint) ≤ MaxSlope_deg`
  check into the ghost snap before the cursor colour turns green.

Done in this pass:

- ✅ **Datavent claiming**: `VentClaim` marker stamped on the target
  `GeoventSmoker` at `BuildAt` commit (`placement.rs`). The placement
  ghost's snap query filters by `Without<VentClaim>`, so a second
  constructor can't aim at a vent that already has a commit. Release is
  position-based via `release_stale_vent_claims` (terrain plugin):
  the claim holds while either a builder with matching
  `PendingBuild`/`Constructing.site` exists, or any building sits within
  16 elmos of the vent. The builder → building hand-off happens
  naturally without a dedicated transfer step.
- ✅ **Hide the datavent mesh once claimed**: `emit_geovent_smoke` now
  filters `Without<VentClaim>`, so claimed vents stop spawning puffs
  (which were the only thing a vent renders — there's no static mesh
  to hide). The green-digit stream resumes as soon as the claim
  releases.

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
| Terminal | SIGTERM airstrike (blast 900/10000, denial 350/2000/3s, 90s cd) | ✅ wired |
| Byte | Mine Launcher (6000 HP cost, 5-mine fan, 10s cd) | ✅ wired |

### 3.6 Infection Chain Refinement — ✅ DONE

`weapon_infection_duration()` maps the four infecting weapons to their upstream
frame-count windows. `apply_damage` keys infection on the weapon name (not the
attacker unit kind) so only Wormsplash / VirusBeam / VirusDeath / Infection trigger
it — direct Wormbite no longer infects, matching upstream. `death_system` sprays
VirusDeath at a dying Virus's corpse so the infection chain spreads via AoE.

Caveat: Spring's engine has no infection logic anywhere — the whole chain is
implemented by upstream KP in Lua gadgets under
[upstream/Kernel-Panic/LuaRules/](upstream/Kernel-Panic/LuaRules/). Our version
matches the observable behavior described in the readme but has never been diffed
against the actual gadget code. Worth a pass before we treat this as frozen.

### 3.7 Kernel Boost / Production Scaling — ✅ DONE

`production_system` multiplies homebase build progress by
`(1 + small_building_count × 0.2)` per team, reusing
`network_buffer::is_small_building` as the predicate.

### 3.8 Flow Dynamic Speed & Air Movement — ✅ DONE (mostly)

- ✅ Flying flag + `can_fly()` / `cruise_alt()`; flying units skip nav grid.
- ✅ Per-Flow `SpeedBoost` component refreshed every second from team small-building
  count, added on top of the registry's base speed in movement.
- ✅ Ground units with `NoChaseCategory=VTOL` — `UnitDef.no_chase_category` parses
  the raw token list, `UnitRegistry::no_chase_vtol` matches `VTOL`, and
  `combat_system` skips candidates whose `SpatialEntry.is_flying` is set.

### 3.9 Mines & Walls — ✅ Partial

- ✅ **Logic Bomb**: already cloaked (§3.3); `tick_kamikaze` detonates it when an enemy
  enters the 64-elmo radius, queuing a `logic_bomb`-weapon self-hit so the existing
  splash + armor-class pipeline handles the blast (3000 vs Subterranean).
- ✅ **BadBlock**: spawned at 100 HP; being a building it blocks movement via the
  existing collision pipeline. Crushable by Bytes is deferred.
- ✅ **Debug**: buildable by every constructor (Assembler / Trojan /
  Gateway build lists already include it) as a stationary Minekiller
  turret. Weapon1=Minekiller auto-fires through the regular
  `combat_system`, but target selection is gated by
  `UnitKind::targets_mines_only` + `UnitKind::is_minekiller_target`
  so Debug only aggros onto enemy Logic Bombs / BadBlocks — it won't
  tickle infantry with its 20-damage anti-spam shots. `SpatialEntry`
  gained a `kind: UnitKind` field so the filter runs without an
  extra ECS lookup per candidate. Upstream's Launcher-via-
  MineLauncher delivery path isn't reproduced; it's a buildable unit
  here since our Byte MineLauncher command-fire already covers the
  "drop a bunch of mines from a distance" use case.

---

## 4. Weapon Visuals & Animation

### 4.1 Beam Textures — ✅ Partial

- ✅ **texture1 wiring**: weapons that declare `texture1=arrow` /
  `dosray` / `bytemegabeam` now carry that bitmap on their beam
  material. `meshes::load_raw_bevy_texture` loads the TGA from
  `upstream/Kernel-Panic/bitmaps/kpsfx` on first use (added to
  `ASSET_DIRS`), uploads it as a Bevy `Image`, and caches the handle
  keyed by filename. `BeamMaterialCache` gained a texture slot in
  its key so a textured DOS beam and a flat Bit line land in
  different cache buckets. Core (inner bright stripe) stays
  untextured so the core stays legibly white over the atlased
  outer.
- ⏳ **`scrollspeed`**: DOS_Beam's 4-UV animation still not wired
  (upstream parses but doesn't animate either — matching Spring
  literally means still flat).
- ⏳ **`beamdecay`** per-frame RGBA fade (upstream applies to the
  full channel) — ours still fades only by scale shrink.
- ⏳ **`intensity`** — already feeds emissive strength on the
  material via `BeamMaterialCache`; no further work needed unless
  we want values > 10 to bloom harder.
- ⏳ **Two-quad edge + core** — still a single thick cuboid with a
  thin bright core cuboid layered on top when `corethickness`
  warrants. Upstream's ortho-quad geometry remains deferred.

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

No visual feedback at the firing unit (except melee flash for Wormbite). Spring
spawns `BitmapMuzzleFlame` at `weaponMuzzlePos`, which is derived from §4.6's
`QueryWeapon(n)` piece — §4.6 is half-done (beam origin now comes from the
muzzle piece), and adding the flame sprite at the same position is the
remaining work.

### 4.6 COB `QueryWeapon1` Callback (Low) — ✅ Partial

Beams and projectiles now originate from the unit's resolved muzzle piece
instead of the transform root, so Bit's `>>>>>` arrow shoots from the
barrel and Byte's MegaBeam leaves from `bp0` instead of the torso. The
`MuzzlePiece` component is attached at spawn via a name heuristic —
`gunpoint` → `bp0` → `flare` → `barrel` → `muzzle` — which covers every
KP unit declaring a recognised muzzle in its .bos, falling back to unit
origin for the rest (Worms / factories / turretless units).

Deferred:

1. Proper `call_script("QueryWeapon1", …)` consumption so the Byte's
   barrel rotates between shots. Our VM's `call_script` returns
   `ret_code` but .bos's `QueryWeapon1(piecenum) { piecenum = bp0; }`
   pattern writes to an out-param rather than `return`ing; wiring the
   param read-back unlocks per-shot barrel cycling on Byte (and any
   future multi-barrel unit).
2. Muzzle flash sprite at the same position — §4.5.

### 4.7 Shield System — ✅ DONE (mechanic; visual deferred)

`ShieldState` component holds radius / max_power / current_power / regen. `apply_hit`
soaks damage through the shield before it hits Health or StunCharge; with upstream's
`shieldpower=0` convention the shield is effectively infinite, matching the role of
minifac and homebase shields. `regen_shields` ticks finite shields toward max.

Remaining: visible shield sphere rendering (`shieldgoodcolor`/`shieldbadcolor`/
`shieldalpha`). Upstream renders this as a 10×6 grid of `ShieldSegmentProjectile`
billboards, not a sphere mesh — color updated in lock-step from repulser state.
Projectile *interception* (as distinct from damage absorption) waits on §4.2
projectile physics.

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

## 6. Fog of War — ✅ MVP (memory-only)

The §10.3 MVP landed: `cloak::update_fog_visibility` runs at 10 Hz in
the Animate set; any non-cloaked, non-friendly unit within a
player-team unit's FBI `SightDistance` gains the [`Spotted`] marker
and becomes visible. Once set, `Spotted` is never revoked — this is
the "memory" variant, not full LoS. Cloaked units (Worm / Logic
Bomb) keep their existing detector-based visibility via
`update_cloak_visibility`; the two systems partition on the
[`Cloaked`] marker so neither races the other's writes.

Terrain is always visible — no exploration mechanic yet.

Deferred to full §6: active revoke on sight loss, per-team
visibility grids (AI teams still have perfect information
internally), terrain chunks only revealed once scouted, Worm-while-
attacking reveal rules.

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
- [ ] **Command queue** — we express unit orders as `MoveTarget` /
  `MovePath` components plus implicit auto-attack. Spring's `CommandAI`
  holds a real queue (`commandQue`) that supports Shift-chain, Patrol,
  Guard, Wait, Attack-move, Fight, and repeat flags. Missing from our
  model and structurally blocks every multi-stage order. Introduce a
  `CommandQueue(VecDeque<Command>)` component with `Stop`, `Move(pos)`,
  `Attack(entity)`, `AttackGround(pos)`, `Patrol(pos)`, `Guard(entity)`
  variants, consumed by the movement + combat systems. Attack-move
  (§Gameplay Bugs) and future patrol/guard live on this.
- [ ] **Event hub** — `CommandFireEvent`, `MorphEvent`, `DispatchEvent`,
  `EnterEvent`, etc. are ad-hoc Bevy Messages per concern. Spring runs
  every lifecycle change through `EventHandler` callins
  (`UnitCreated`, `UnitDamaged`, `UnitDestroyed`, `WeaponFired`, …) so
  new systems (fog-of-war, AI hooks, Lua gadgets, achievements) can
  subscribe uniformly. Not blocking today; starts to hurt as more
  systems need "tell me when X dies/fires/spawns."
- [ ] **QTPFS terrain-change repathing** — upstream QTPFS has
  `TerrainChange()` / `NodeLayersChangeTrack` that invalidate nodes
  when the heightmap is edited (Lua gadgets do this when placing on
  geovents). We don't re-pave on edits, so in-flight paths through
  edited terrain can become wrong. Verify whether our QTPFS exposes a
  change hook; wire `map_loading::apply_heightmap_edit` into it if not.
- [ ] **Lua gadget audit (global)** — §3.6 flags the infection gadget
  specifically, but upstream KP has dozens of gadgets under
  `LuaRules/Gadgets/` that encode rules we've either re-implemented or
  silently skipped (packetbuffer, launcher, stun_armor, kernel_boost,
  explodeAs, area-damage, ward variants, …). None have been diffed
  line-by-line. Schedule a pass: read each gadget, decide "already in
  Rust" / "port" / "skip with rationale."

### Performance

- [ ] HUD systems despawn+respawn entire UI tree every frame (~30–50 entities) — use change
  detection (`Changed<Selected>`, `Changed<Health>`, `Changed<Producer>`) to update in-place
- [ ] `update_unit_highlight` clones and re-adds a `StandardMaterial` per selected/hovered
  unit every frame, leaking orphaned handles — cache per-faction+brightness
- [ ] `despawn_health_bars` is O(n×m) — use `HashSet` of removed units or query children
- [ ] Melee flash and projectile materials created per-attack instead of cached — extend
  `BeamMaterialCache` to cover all weapon FX
- [x] ~~Animation system allocates `Vec<(i32, i32)>` per animator per frame~~ — hoisted
  `turn_finished` / `move_finished` to `Local<Vec<_>>` parameters on `animation_system`,
  cleared at the start of each animator and drained at the end. Steady state: zero
  allocations.
- [ ] Per-frame `UnitRegistry` lookups for immutable data (speed, weapon name) — cache as
  ECS components at spawn time (e.g. `Speed(f32)`, `WeaponBinding(&str)`)
- [ ] `AttackEvent::weapon_name` is `String` (heap alloc per attack) — introduce a `WeaponId`
  newtype (interned string or index into `WeaponRegistry`) so attack events carry a cheap
  `Copy` identifier. `BurstFire.weapon` and `PendingDamage.weapon` are also `String` and
  clone per burst shot / damage event — they inherit from the same `WeaponId` change.
- [ ] `UnitRegistry::weapon()` returns raw TDF section name strings — return
  `Option<&WeaponDef>` directly so callers never see string keys, eliminating empty-string
  checks in combat.rs and hud.rs
- [ ] **Spatial hash** — shared `SpatialIndex` resource (XZ uniform grid, 256-elmo
  cells, matching upstream `CQuadField`) rebuilt at the head of `GameplaySet::Simulate`
  via `spatial::rebuild_spatial_index`. Retrofitted: `combat::combat_system` target
  selection, `combat::apply_damage` splash radius, `combat::tick_kamikaze` trigger
  check, `command_fire::tick_area_denial`. Still linear and pending retrofit:
  `command_fire::apply_firewall` (cold path — per-cast only),
  `interaction::movement::resolve_motion` + `unit_separation_system`,
  `cloak::update_cloak_visibility`, `ai::nearest_unclaimed_datavent`.
- [x] ~~`movement::movement_system` and `unit_separation_system` each allocate a fresh
  `Vec<UnitSnapshot>` over all units every frame~~ — both now take
  `Local<Vec<UnitSnapshot>>` / `Local<Vec<(Entity, Vec3, …)>>` params that are cleared and
  repopulated each tick. Steady-state allocation count is zero until the spatial hash
  replaces the snapshot entirely.
- [x] ~~`production::production_system` allocates `spawns: Vec<…>` every frame~~ — hoisted
  to a `Local<Vec<_>>`, cleared at the start of the system and drained after spawn.
- [ ] `animation::animation_system` calls `transforms.get_mut(piece_entities[p])` per piece
  per frame — many pieces have no active turn/spin/move and don't need the lookup. Track
  "dirty pieces" per animator (the set that had interpolation this tick) and query only
  those.
- [ ] `weapon_registry.get(weapon_name)` runs per unit per frame in `combat_system`,
  `tick_burst_fire`, `apply_damage`, and the command-fire paths, hashing a string each
  call. Intern weapon names into `WeaponId(u16)` during `WeaponRegistry::load()` and
  cache the ID as a component on attackers (paired with the `WeaponBinding` work above).
- [ ] `bookkeeping::count_small_buildings` scans all `UnitType` entities every 0.25s even
  though buildings are a small fraction and only change when one spawns or dies. Switch
  to event-driven counters: bump on spawn, drop on `Dying`, so per-tick cost is zero.
- [ ] `tick_deploy_state` walks every Deployable every frame even when nothing moved.
  Filter to `Changed<MoveTarget>` + in-flight transitions — the `Closed`/`Open` steady
  states don't need a tick.
- [ ] `ui::minimap::update_minimap` rewrites the full base image via
  `copy_from_slice(&state.base_pixels)` every 0.1s. Track a dirty-rect of the previous
  frame's unit dots + viewport rectangle and restore only those pixels, turning an
  O(W·H) memcpy into O(units + viewport_perimeter).
- [x] ~~`animation::publish_unit_values` pushed BUILD_PERCENT_LEFT to every animator every
  frame~~ — filtered to `With<Emerging>` + `RemovedComponents<Emerging>` so only
  mid-emerge animators pay the cost.
- [x] ~~`cloak::update_cloak_visibility` ran every frame~~ — throttled to 10 Hz via
  `CloakRefreshTimer` resource; visibility changes are well below perception threshold
  at that rate.

### Testing & Tooling

- [ ] Run `cargo llvm-cov` workspace-wide; fill high-value coverage gaps
  (weapon category / damage resolution edge cases, AI phase transitions,
  shield soak + Protected interactions).
- [ ] Flamegraph a 3-team full map for 30s; chase anything >0.5% of
  Update-phase time.
- [ ] Survey which performance tricks Spring / upstream KP apply that
  we could adopt.

### Upstream-waiting workarounds

- [ ] `main.rs` carries four `TODO(windows-resize)` markers
  compensating for the Bevy 0.18 + Windows "freeze on resize" bug
  stack:
  1. Vulkan-only backend (no DX12 fallback) — DX12 swapchain
     reconfigure hangs during `WM_ENTERSIZEMOVE`.
  2. `PipelinedRenderingPlugin` disabled — the second render thread
     deadlocks against the main thread's modal message pump.
  3. `PresentMode::AutoNoVsync` — vsync waits queue up during the
     modal pump and compound the stall.
  4. 320×240 min resize — wgpu panics on 0×0 surface reconfigure,
     which happens naturally during a fast drag-to-nothing.
  Revert each piece independently once the fix lands upstream — search
  Bevy's `Platform-Windows` issues and the gfx-rs/wgpu tracker for
  "DX12 resize hang", "WM_ENTERSIZEMOVE", "pipelined rendering
  deadlock", and "surface reconfigure 0x0".

### Gameplay Bugs

- [x] ~~**No unit ever dies**~~ (observed 2026-04-18, fixed same day).
  Two compounding scale mismatches — either alone would have kept
  health bars near-full; together they explained the observed
  zero-deaths behaviour exactly.
  1. Our scalar `collision_radius` (~8 elmos for a 2×2-footprint
     Bit) vs. Spring's volumetric hit box. The new `spray_angle` miss
     gate in `apply_damage` rejected any shot whose `impact_pos`
     landed outside `collision_radius` of the target; at typical KP
     `spray_angle=1024` (5.6°) and 350-elmo range that's a ~34-elmo
     offset, so ~95% of shots missed every target. Fix: reverted the
     primary-damage miss gate. Spray perturbation still jitters
     `impact_pos` for splash and visuals, but the primary target
     always takes full damage until we have real volumetric hit
     boxes. The miss-threshold-needs-a-proper-hit-volume concern is
     tracked under §Combat parity gaps below.
  2. **FBI `DamageModifier=0.000001` applied literally**. Upstream KP
     sets this near-zero value on every combat unit as a Spring
     engine-disable hack — the real damage math lives in KP's
     LuaRules gadget. Our reimplementation resolves damage directly,
     so multiplying every hit by `1e-6` meant even shots that did
     land delivered `80 × 1e-6 ≈ 8e-5` HP. `UnitRegistry::damage_modifier`
     now treats sub-threshold values (`< 0.01`) as the engine hack and
     returns `1.0`; explicit design values like Socket/Firewall's
     `4.0` pass through unchanged. If we ever want homebase / Byte
     near-immunity back it should come from a dedicated per-kind
     multiplier table rather than the FBI engine-disable value.
- [x] ~~**Movement ignores per-unit `MaxSlope`**~~ — done.
  - Terrain penetration: `ground_clamp_system` + `UnitKind::is_subterranean`.
  - Cliff climbing: `NavGridSet` holds one `NodeLayer` per distinct
    `MaxSlope` cap (built from `BTreeSet` of converted-to-ratio caps
    across `ALL_UNIT_KINDS` + the 45° default, so duplicate degrees
    collapse into one bucket). `compute_path` picks the tightest
    bucket whose cap ≥ the unit's via `NavGridSet::bucket_for`, and
    the `slope_mod` is kept constant across buckets so path costs
    order consistently. Lookup lives in `compute_path` (≤3 calls /
    frame, cheaper than a cached-component scheme) — the
    `NavBucket(u8)` cache drafted earlier turned out to be overkill.
  - Building-placement MaxSlope is still a separate concern (FBI
    values on Socket / Firewall / Terminal / Obelisk = 10, BadBlock
    = 32, Kernel / Hole = 60 govern where you can drop the build
    ghost, orthogonal to nav) — tracked under §3.1.

  Sub-bugs still watching:
  - QTPFS doesn't observe heightmap edits (Technical Debt →
    Architecture "QTPFS terrain-change repathing"). Lua gadgets that
    pave on build invalidate every bucket grid.
  - The nav-grid build needs to happen *after* upstream Lua gadgets
    run their init-time heightmap edits, otherwise `map_loading`'s
    view of terrain is stale. Verify ordering on map load.

  Data note: `gamedata/MOVEINFO.TDF` sets `MaxSlope=36` on all three
  mobile move classes (LIGHT / MEDIUM / HEAVY). Recoil's
  `DegreesToMaxSlope` (clamp × 1.5 → `1 − cos`) turns that into an
  effective ~54° cap upstream. If the per-unit buckets feel like
  overkill for KP specifically, collapsing back to a single 54°
  grid is always a valid simplification.
- [x] ~~`GameState` not reset on map cycling~~ — fixed in a50fe8b
- [x] ~~Rally point / delivery point for factories~~ — `Emerging.rally_point` wired
- [x] ~~Terrain height not sampled during movement~~ — ground clamping in recent walking
  improvements (5046fd2) + spawn clamp (6e043ba)
- [ ] No unit collision avoidance — units overlap when crowded (partial: walking improvements
  address some cases, revisit)
- [ ] Attack-move (`A` hotkey) is wired in HUD but handler is empty (TODO at `hud.rs:849`).
  Structural blocker: we have `MoveTarget` + implicit-attack, not a Spring-style
  command queue. Attack-move, Shift-queued orders, Patrol, Guard all share the
  same missing foundation. See `Sim/Units/CommandAI/MobileCAI`. Implementing a
  proper `CommandQueue` component once unlocks the whole family.
- [ ] Non-geovent map features (trees, rocks, debris) are dropped on load —
  only `feature.feature_type.is_geovent()` entries spawn, the rest are never
  rendered. `MapFeature.rotation_degrees()` is parsed but there is nothing to
  apply it to until we actually place the other feature types.
- [x] ~~Weapons ignore line-of-sight~~ — `combat_system` now rejects targets whose
  LOS is blocked by terrain (`Heightmap::has_line_of_sight` with `LOS_MARGIN=4`);
  ballistic weapons (`trajectory_height > 0`) skip the check since they lob over.
- [ ] **Weapons never miss** — `combat_system` perturbs `impact_pos` by
  `tan(spray_angle) × distance` (so splash and beam visuals sit off-target),
  but the primary-damage miss gate is *reverted*: our scalar `collision_radius`
  is ~3× smaller than Spring's volumetric hit box, which turned every shot
  into a miss and froze the sim. Reinstating the miss gate is blocked on
  §1.8 "Volumetric hit box". `tolerance` (aim-error fire gate, distinct from
  per-shot spread) also remains unimplemented — most KP weapons set it to
  3000-8000 short-units meaning "always allow firing", so the practical gap
  is small.
- [ ] `collidefriendly` on projectile physics (still blocked on §4.2 projectiles
  gaining actual collision).
- [ ] Factory spawn offset hardcoded in `production.rs` — should use COB `QueryBuildInfo`
  callback for correct build-pad position

### Incomplete COB VM

- [x] ~~Scriptor linear constant per unit~~ — fixed in 5ffd072
- [x] ~~Start-script threads inherit signal mask~~ — fixed in 855d506 (empty mask)
- [x] ~~Piece remap by name at spawn~~ — added in 9f8553f
- [x] ~~`BUILD_PERCENT_LEFT` bridge from CobVm to Create()~~ — wired through production
- [ ] `GET` / `GET_UNIT_VALUE` still return 0 for most values — only select `springdefs.h`
  constants mapped; expand as needed
- [ ] `EmitSfx` and `SetValue` opcodes still largely unimplemented — note that §4.3 CEG
  emitters and §4.5 muzzle flash both sit downstream of this. Landing `EmitSfx` unblocks
  both visual gaps at once.
- [ ] `PieceIndex` component: inner value set but never read (only used as marker)
- [ ] COB piece-space interpolation between sim frames — Spring's `LocalModelPiece` stores
  `modelSpaceTra` with a dirty flag and interpolates between sim ticks for smooth
  animation at render framerate. Worth checking whether our animator lerps or snaps;
  sim runs at 30 Hz, render at up to 240 Hz, so non-interpolated piece transforms will
  visibly pop on fast turrets.
- [ ] **`HitByWeaponId` damage callback** — confirmed called by `byte.bos`,
  `hole.bos`, `carrier.bos`, `expbase.bos` with signature
  `HitByWeaponId(headingZ, headingX, weaponId, damage)` returning a damage
  multiplier (30 = take 30%, 100 = full). Wiring it needs (1) synchronous
  `call_script` in `apply_hit` to consume the return value, (2) a
  weapon-name → `weapon_id: u16` mapping so scripts can discriminate (KP's
  id=168 is DOS which bypasses Byte's closed-armor). Shares the same u16
  interning key as the `WeaponId` performance todo — do both in one pass.
  This unlocks Byte closed-state armor (§1.5/1.6 deferred).
- [ ] **`AnimFinished(piece, axis)`** — Spring fires this back to scripts when
  turn/spin/move finish so `wait-for-turn` / `wait-for-move` opcodes can
  resume. Not verified in our VM; if absent, scripts that block on animation
  completion silently wedge. Quick audit against `script_triggers.rs`.
- [x] ~~**`ExplodeAs` death trigger**~~ — `death_system` now reads each unit's
  FBI `ExplodeAs` and queues a self-hit `PendingDamage` at the corpse with
  that weapon so AoE splash resolves through the existing pipeline. Virus
  gets VirusDeath (was hardcoded, now data-driven), Bit gets RetroDeath,
  Byte gets RetroDeathBig, etc. Missing weapons are silently skipped so we
  don't warn-spam on yet-unparsed explosion TDFs. `SelfDestructAs` gets the
  same field but stays unused until manual-destruct exists.

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
- [x] ~~`PieceIndex` field `.0` never read~~ — ZST'd in the simplification sweep

### Naming

- [ ] `DEEP_FEATURES.md` calls the Network homebase "Carrier" in 3 places but code uses
  `UnitKind::Connection` — upstream `sidedata.tdf` also uses "carrier"; consider aligning

---

## 11. Simplification Follow-Ups

Deferred from the April 2026 simplification sweep. Each item was flagged by
the review agents (reuse / quality / efficiency) but skipped because the
blast radius was larger than one session warrants. Ordered high → low impact.

### 11.1 Split the three kitchen-sink files

- [ ] `units/combat.rs` (1228 LoC) → `combat::aim` + `combat::damage` +
  `combat::lifecycle`, leaving a ~300-line `combat::core` for target selection.
  Re-export from a thin `combat::mod` so the public API stays stable.
- [ ] `units/spawning.rs` (709 LoC) → `spawning` (core + `SpawnContext`),
  `spawning::s3o_mount` (flatten / piece-to-mesh / ground-lift / DFS),
  `spawning::emerge` (`Emerging`, `EmergeStyle`, `FadeMaterials`, `emerge_system`),
  and `spawning::showcase`. `FactoryPieces` belongs with `production.rs`.
- [ ] `map_loading.rs` (524 LoC) → `map_loading::mipmap` +
  `map_loading::atmosphere`, leaving `load_map` as the orchestrator.

### 11.2 `SpawnContext` SystemParam

- [ ] `spawn_unit` takes 12 arguments and is called from 7 sites, each
  repeating the same 10-wide tail (plus `#[allow(clippy::too_many_arguments)]`).
  Fold `(&mut Commands, &mut Assets<Mesh>, &mut Assets<StandardMaterial>,
  &mut Assets<Image>, &mut S3OModelCache, &mut CobFileCache,
  &SelectionVolumeMaterial, &UnitRegistry)` into a
  `#[derive(SystemParam)] struct SpawnContext<'w, 's>`. Each call site shrinks
  to 4 args and every caller's `too_many_arguments` allow disappears.

### 11.3 Weapon IDs: kill the string allocs

- [ ] `PendingDamage.weapon`, `BurstFire.weapon`, and `AttackEvent.weapon_name`
  are all `String`. Every shot, every burst follow-up, and every factory
  build-laser ray (4× per Kernel per frame in steady state) allocates a
  `"BuildLaser".to_string()` / `weapon_name.to_string()`. Replace with
  `&'static str` or an interned `WeaponId(u16)` resolved at TDF load.
  Downstream `weapon_registry.get(&pending.weapon)` becomes an array lookup.
- Unblocks folding `weapon_infection_duration(&str)` into `strum::EnumString`.

### 11.4 HUD panels rebuild every frame

- [ ] `ui/hud/info_panel.rs`, `build_menu.rs`, and `order_palette.rs` each
  `despawn_all_children + spawn fresh` their entire subtree unconditionally on
  every Update. With ~10–30 UI entities per panel, that's ~1800 despawn+respawn
  ops/sec at 60 fps plus per-frame `format!("{:.0}", …)` allocs for every Text
  node. Gate on `Changed<Selected>` or a `LastSelectionHash` resource; for
  progress bar / HP refresh, update `Text` in place rather than respawn.

### 11.5 `MovementState` / `ProductionState` → change detection

- [ ] Both components exist solely to track "was moving/building last frame"
  and reimplement what Bevy gives you for free via `Added<MoveTarget>`,
  `RemovedComponents<MoveTarget>`, and friends. Delete both components + the
  per-frame commands-churn of `insert(MovementState {...})`. Saves 2
  components, 2 systems, ~100 LoC.

### 11.6 Cached per-unit stats

- [ ] `interaction/movement.rs` calls
  `unit_registry.collision_radius/speed/can_fly/turn_rate` four times per
  unit per frame — hash lookups on static data. Add a
  `UnitStats { radius, speed, turn_rate, can_fly, cruise_alt }` component
  written once at spawn. Combat-side equivalent:
  `CombatProfile { enforce_los, no_chase_vtol, weapon: Option<WeaponId> }`
  so `combat_system` stops string-keyed hashmap hits in its hot loop.

### 11.7 Drop `.chain()` where data-flow allows

- [ ] Every `GameplaySet` is `.chain()`ed internally, serializing systems that
  touch disjoint components (e.g. `tick_port_buffers`, `tick_spawn_stun`,
  `tick_flow_speed`, `animate_connection_hatch`; `tick_infections`,
  `tick_command_fire_cooldown`, `tick_protection`). Audit each set, replace
  the blanket `.chain()` with explicit `.after()` edges only where real data
  deps exist. Bevy schedules the rest on the worker pool. Rough target:
  1.5–2× Resolve-set throughput.

### 11.8 Smaller wins

- [ ] **`unit_separation_system`** ([interaction/movement.rs:549](kernel-panic/src/interaction/movement.rs#L549))
  is still O(N²) — builds a full snapshot and nested-loops it. Route each
  mobile unit through `SpatialIndex::query_radius` instead. Expected ~40×
  fewer distance checks at N=300 units.
- [ ] **`gunbase` / `body` piece-name scans** ([combat.rs:592](kernel-panic/src/units/combat.rs#L592),
  [production.rs:346](kernel-panic/src/units/production.rs#L346)) still
  case-insensitive-compare every frame (now via `CobAnimator::piece_index`).
  Cache the indices at spawn like `FactoryPieces::emitters/pad` does.
- [ ] **`spawn_projectile` / `spawn_melee_flash`** allocate a fresh
  `StandardMaterial` per shot. Route through `BeamMaterialCache` (which
  already handles beams + impact bursts) or a sibling `ProjectileAssets`.
- [ ] **`spawn_death_particle`** allocates mesh + material per explode. Lazy
  `DeathParticleAssets` resource (same pattern as `BuildSparkleAssets`),
  fade via scale rather than per-particle material mutation.
- [ ] **Observer-style one-shot markers**: `PendingFadeInstall` and
  `JustFired` are "send one message to this entity next frame". Convert to
  Bevy 0.18 entity-scoped events / observers — stops the component-churn
  that sparse-set storage is compensating for.
- [ ] **Three bespoke queue types** (`DamageQueue`, `VirusSpawnQueue`,
  `PendingAttacks`) are identical `Vec<T>`-wrapped resources. Either migrate
  them to Bevy `Events<T>` or unify under one `struct Queue<T>(Vec<T>)`.
- [ ] **`install_fade_materials` / `bookkeeping::count_small_buildings`**
  scan all units every frame; adopt `Added<T>` / `RemovedComponents<T>` for
  incremental maintenance.
- [ ] **Lifetime components dedup**: `BeamVisual`, `BurstSegment`,
  `ImpactBurst`, `BuildSparkle`, `DeathParticle`, `GeoventSmoke` each carry a
  `{ lifetime, max_lifetime }` pair plus a bespoke decay system. Extract a
  shared `Lifetime { remaining, total }` + generic `tick_lifetimes` despawn
  system; each specialized system keeps only its visual-specific curves.

### 11.9 Align with workspace guardrails

- [ ] `.ok()` swallowing errors at `terrain/geovent.rs` and
  `weapon_fx/tick.rs` (camera-lookup fallbacks to `Vec3::Y * 1000.0`). Switch
  to `.inspect_err(|error| warn!(%error, "no camera"))` so the error is
  visible in traces before we accept the fallback.
- [ ] `panic!("No map files found")` in `pick_map` could become a `thiserror`
  `MapLoadError` propagated out — minor, but aligns with the workspace
  "colocated thiserror enums" rule.

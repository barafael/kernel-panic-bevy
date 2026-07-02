# Kernel Panic — Remaining Features Plan

## Current State (April 2026)

**6 crates, ~32k lines, 303 tests, all passing.**

Working: map loading, original textures, S3O models, 3
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
over in one hit; Bytes need many hits. Stunned Bytes lose their closed-state armor
bonus (forced open) — see §1.6.

### 1.6 Damage Modifiers — ✅ DONE

`avoidfriendly=1` and `noselfdamage=1` filter the splash set; FBI `DamageModifier`
applied to every damage event, with values under `0.01` treated as the upstream Spring
engine-disable hack and normalised to `1.0` while explicit design values like
Socket/Window/Port/Firewall's `4×` pass through unchanged.

Byte closed-state armor (`byte.bos HitByWeaponId`'s 30%-when-closed rule) is wired
host-side via `byte_armor_multiplier` and the `ByteOpen` component (commit `8d81187`).
SIGTERM bombs and stunned Bytes bypass the gate, mirroring upstream's `id == 168`
exception and `lua_IsParalyzed` branch. Infection chain wired via `Infected` +
`VirusSpawnQueue`.

Deferred: `collidefriendly` on projectile physics (weapons don't have projectile
collision yet — §4.2). Generalising `HitByWeaponId` for Hole/Carrier/Expbase if those
turn out to need it.

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
  *Foundation landed*: `combat::CollisionVolume` enum (Sphere / Cylinder /
  Aabb) is cached at spawn from the S3O bounding sphere and exposes
  `contains` + `ray_segment_hit` for the three shapes. Every unit
  currently spawns a Sphere matching `hit_radius`.
  *Wired into projectile mid-flight*: both `LaserBolt` and `ProjectileVisual`
  in `weapon_fx::tick` now sweep their per-frame travel segment against
  the target's current `CollisionVolume` via `target_volume_hit` before
  falling back to the original `lead_raw >= total_distance` /
  `progress >= 1.0` triggers. Moving targets are caught the moment the
  bolt's segment crosses them; off-axis targets stay un-intercepted.
  *Broad-phase landed*: a second pass via the `SpatialIndex`
  (`broad_phase_volume_hit`) catches any non-friendly, non-attacker unit
  whose volume the segment crosses — closest-`t` wins, friendlies are
  skipped (matches upstream's default `collidefriendly=0`), and the
  resolved damage redirects to the interloper instead of the originally
  intended target. Tests:
  `laser_bolt_intercepts_target_volume_before_predicted_impact`,
  `laser_bolt_does_not_intercept_off_axis_target`, and
  `laser_bolt_broad_phase_intercepts_enemy_in_path_skips_friendly`.
  Remaining: route shield interception through `ray_segment_hit`
  (so shields can absorb projectiles mid-flight, not only at
  endpoint), and add per-unit Cylinder / Aabb classifiers (e.g. tall
  thin units like Pointer).
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
- ✅ **Building-placement slope check** — done. `Heightmap::max_slope_in_footprint`
  samples the steepest cell across the building's `FootprintX × FootprintZ`
  in Spring's `1 - cos(angle)` encoding; `update_ghost` (placement.rs)
  rejects the snap when it exceeds `unit_registry.max_slope_ratio(kind)`.
  Footprint values pulled from FBI via `UnitRegistry::footprint_elmos`.

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

### 3.5 Command-Fire & Area Denial Framework — ✅ DONE

All five command-fire abilities are live. Framework: `CommandFireEvent` →
`process_command_fire` spawns an `AreaDenialZone` entity, `tick_area_denial`
deals dps*dt to units in radius with friendly-fire + infection flags,
`CommandFireCooldown` gates recasts. `D` hotkey
(interaction::ability) fires the selected caster's ability at the cursor.

| Unit | Ability | Status |
|------|---------|--------|
| Pointer | NX Flag (r=120, 100 dps, 60s, friendly-fire) | ✅ wired |
| Obelisk | Infection Gas (r=400, 120 dps, 13s, infects, 40s cd) | ✅ wired |
| Firewall | Reflector Shield (r=300, 20s, 50% reduce + 50% reflect) | ✅ wired |
| Terminal | SIGTERM airstrike — two-stage `SigTermSignal` → `SigTermBomb` (blast 900/10000, denial 350/2000/3s, 90s cd) | ✅ wired |
| Byte | Mine Launcher (6000 HP cost, 5-mine fan, 10s cd) | ✅ wired |

SIGTERM matches upstream `airstrike.lua` two-stage flow (commit `8d81187`):
the bomber flies from the Terminal to the target, then drops the bomb which
gravity-falls and detonates. Both stages are untargetable by construction
(no `UnitType`), mirroring upstream's `Category=NUKE VTOL` filter.

### 3.6 Infection Chain Refinement — ✅ DONE

`weapon_infection_duration()` maps the four infecting weapons to their upstream
frame-count windows (diffed against `LuaRules/Gadgets/infection.lua` in commit
`8d81187`: virusbeam=90, virusdeath=180, wormsplash=200, infection=30, all in
sim frames @ 30 Hz). `apply_damage` keys infection on the weapon name (not the
attacker unit kind) so only Wormsplash / VirusBeam / VirusDeath / Infection trigger
it — direct Wormbite no longer infects, matching upstream. `death_system` sprays
VirusDeath at a dying Virus's corpse so the infection chain spreads via AoE.

Obelisk's gas zone now infects with the canonical 1 s window (upstream `infection`
weapon = 30 frames; was 6 s host-side default before).

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
- ✅ **`beamdecay`**: per-frame RGB multiplier wired through vertex
  colors. The shared `BeamMaterialCache` material keeps the
  weapon's authored RGB; each `BeamVisual` carries the weapon's
  `decay` and `tick_weapon_fx` writes `[d,d,d,1]` to all 4 vertex
  colors each frame, where `d = decay^(elapsed_frames)`. Network
  weapons (`beamdecay=0.8`) now read as fading streaks rather than
  a thickness-shrinking stripe. Default `1.0` falls back to the
  legacy sqrt-thickness fade so non-decay beams (Corruption /
  Retro flashes) still feel smooth at 60 Hz over a 1-2 frame
  `beamtime`.
- ✅ **Two-quad edge + core**: outer + core are independent
  camera-rewritten quads (`build_billboard_quad_mesh`) at different
  thicknesses; matches upstream's `BeamLaserProjectile::Draw` for
  the long-axis pass.
- ⏳ **`scrollspeed`**: DOS_Beam's 4-UV animation still not wired
  (upstream parses but doesn't animate either — matching Spring
  literally means still flat).
- ✅ **`texture2` endcaps**: `LaserBolt` gained an optional `caps`
  field that holds the lead + tail cap entities + their meshes;
  `build_bolt_caps` resolves `weapon.texture2` and spawns them if
  the texture is in `CegRegistry::resolve_texture` (today only
  Byte's `MegaBeam` with `bytelaser`). Tick rewrites both quads
  using `dir2 = to_cam.cross(dir1)` per upstream
  `LaserProjectile::Draw`; despawn cleans up cap entities.
- ⏳ **`intensity`** — already feeds emissive strength on the
  material via `BeamMaterialCache`; no further work needed unless
  we want values > 10 to bloom harder.

### 4.2 Projectile Models — ✅ DONE

`spawn_projectile` loads the weapon's `model=` field via `load_s3o_mesh`
([weapon_fx/spawn.rs](kernel-panic/src/units/weapon_fx/spawn.rs)) — Pointer's
`octashot.s3o`, Flow's `network_medium_missile.s3o`, Marisa's `marisa_shot.s3o`,
and SIGTERM's `sigterm.s3o` all render as their authored meshes; missing /
unparseable models fall back to a sphere sized by `weapon.size`. SIGTERM's
two-stage strike uses `signal.s3o` for the bomber and `sigterm.s3o` for the
falling shell, both cached lazily in `SigTermAssets`.

### 4.3 Explosion / Impact Effects — ✅ Partial

`ImpactBurst` spawns a color-coded sphere at every hit, sized by the weapon's
`area_of_effect` and tinted by its `rgb_color`. Covers beams, projectiles, burst-beams,
and AoE splashes with a single code path; a pragmatic substitute for the full CEG
particle system. The ~40 upstream explosion TDFs are still not parsed (full per-weapon
CEG emitter stacks remain deferred).

### 4.4 Projectile Trails & Smoke — ✅ Mostly done

`build_projectile_trail` already spawns a textured ribbon for any
weapon with `smoketrail=1` or a non-empty `cegTag`. Geometric
(`texture2=pointertrail`) and FlowMissile (`texture2=flowtrail`)
both render their authored trail textures now —`flowtrail` was the
missing entry in `CegRegistry::resolve_texture` and trails fell
back to an untextured strip; fixed.

BugCannon's `smoketrail=1` is commented out upstream, so it stays
trailless to match.

### 4.5 Muzzle Flash — ✅ DONE

`weapon_fx` now spawns a muzzle flash sprite at the resolved `MuzzlePiece`
position for every non-melee, non-build-laser fire (commit `5f0ff19`).
Spring's `BitmapMuzzleFlame` equivalent. Scale + duration read from the
weapon TDF. Melee flash (Wormbite) retains its dedicated orange burst at
the bite point.

### 4.6 COB `QueryWeapon1` Callback — ✅ DONE

Wired in commit `8d81187`. `CobVm::call_script_out_param` runs a
function and reads back local[0] — the BOS-idiomatic out-param slot
used by every `Query*`. `refresh_muzzle_pieces` runs each frame for
units with an `AimTarget` and stores the script-declared piece in
`MuzzlePiece`. Byte's `gp` static var cycles bp0..bp3 across the
4-shot MegaBeam burst, and the visual muzzle now follows.

`MuzzlePiece::resolve` is kept as a spawn-time fallback for units
whose scripts don't declare `QueryWeapon1` (factories, Worm,
turretless buildings).

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

## 6. Fog of War — ✅ Implemented, gated off

`cloak::update_fog_visibility` + `update_cloak_visibility` implement
active LoS from the [`PlayerTeam`]'s perspective: non-cloaked enemy
units within sight of any friendly are `Visible` + [`Spotted`],
otherwise `Hidden` and `Spotted` is revoked; cloaked enemies are
hidden unless a player-team unit with FBI `RadarDistance > 0` is in
range. The two systems partition on the [`Cloaked`] marker so
neither races the other's writes.

The systems are gated by the [`FogEnabled`] resource, which defaults
to `false` in the current sandbox build. The user can switch
perspectives across every faction, so hiding any team's units is a
UX bug — confirmed in playtest where everything except `PlayerTeam(0)`
went invisible. While `FogEnabled.0 == false`, both systems
short-circuit to "everything Visible + Spotted" and `install_cloak_fade_materials`
fades every cloaked unit.

Flip `FogEnabled` to `true` once a real player/AI ownership
distinction exists (faction-select / per-client PlayerTeam) — the
fog logic is ready; no further work needed on the LoS pipeline.

Terrain is always visible — no exploration mechanic yet.

Deferred: per-client `PlayerTeam` (currently a single global
resource), terrain chunks only revealed once scouted, Worm-while-
attacking reveal rules (blocked on autohold toggle / `CommandQueue`
so we know when a Worm is choosing to attack).

---

## 7. Audio (Low)

`sound_start`/`sound_hit` parsed in every weapon but no audio system exists. Original KP
sound files are in the mod archive. Use `bevy_audio` for spatial sound: weapon fire, death,
unit acknowledgements, ambient, UI feedback.

---

## 8. WASM / Web Build (Medium)

### 8.1 Pre-Bake Map Format — ✅ Done

`.kpmap` is the runtime form: `cargo run -p spring-map --bin bake_map -- INPUT.sd7`
produces a postcard-encoded blob (heightmap + metalmap + features + assembled
ground texture as raw RGBA + .smd / mapinfo.lua resolved to `MapInfo`) that
loads through `spring_map::baked::read_baked_map` with no archive / mlua /
SMT-tile dependencies. The runtime now prefers a `.kpmap` over the source
`.sd7` of the same stem — see `dedupe_prefer_baked` and `load_map_dispatch`.

Texture is currently raw RGBA, ~16 MB for a 2k² map. PNG/DXT compression is
deferred until §8.2 actually requires it (file size only matters when we ship
over HTTP).

### 8.2 Deployment (deferred)

GitHub Actions workflow: build WASM → `wasm-bindgen` → deploy to GitHub Pages with one
pre-baked map (Marble Madness). Held until gameplay is solid; the `.kpmap`
format is the prerequisite that's now in place.

### 8.3 Compatibility Constraints

- `sevenz-rust` won't compile to WASM — `.kpmap` sidesteps this entirely
- `mlua` heightmap gadgets run at bake time, not runtime — `.kpmap` consumers
  don't link mlua at all (still pulled by the source `.sd7` path; gated behind
  `#[cfg(not(target_arch = "wasm32"))]` when WASM lands)
- `spring-map` needs `#[cfg(not(target_arch = "wasm32"))]` on filesystem code
  (deferred until 8.2)

---

## 9. Multiplayer (Low)

Requires all gameplay to be deterministic first. `lightyear` or `bevy_replicon` for state
replication. Lockstep or server-authoritative. Lobby system with map/faction selection.

---

## Recommended Implementation Order

Done since last plan: §3.2 packet buffer, §3.3 cloaking, §3.4 Bug↔Exploit morph,
§3.5 all command-fire (incl. two-stage SIGTERM + Byte MineLauncher), §3.6 infection
refinement (diffed against `infection.lua`), §3.7 Kernel Boost, §3.8 Flow speed,
§3.9 Logic Bomb detonation + Debug placement, §4.1 `beamdecay`, §4.3 impact bursts,
§4.6 QueryWeapon1 script callback, §4.7 shields, §5.1 AI Expand + Defend,
§1.6 Byte closed-state armor, QTPFS slope cap matched to upstream's
`1 - cos(deg × 1.5)` encoding, §6 fog of war (full active LoS from `PlayerTeam`
perspective), §8.1 `.kpmap` pre-bake format.

| # | Item | Section | Rationale |
|---|------|---------|-----------|
| 1 | `texture2` endcaps + remaining beam polish | 4.1 | Visual polish |
| 2 | Audio | 7 | Weapon sounds highest priority |
| 3 | WASM deploy (gameplay-gated) | 8.2 | Browser-playable |
| 4 | Multiplayer | 9 | Endgame feature |

---

## 10. UX / Polish Backlog

Collected from the in-flight todo list. Not blocking; each is its own
focused chunk when we're ready.

### 10.1 Selection / input

- ✅ Double-click to select every visible unit of the same kind —
  filtered to the click target's `(UnitType, TeamId)` and to units
  currently on screen + not `Visibility::Hidden`. 300 ms recognition
  window; `last_click` tracked in `DragState`.
- ✅ Unit groups: `Ctrl-1..9` assigns the live selection, plain
  `1..9` recalls (replace), `Shift-1..9` recalls (additive). Lives
  in `interaction::selection::groups`. Camera centering on recall
  is still TODO; would slot into [`RtsCameraState`] when the group
  has any live members.
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

### 10.3 Fog-of-war (resolved)

Done — see §6. We landed full active LoS rather than the cheaper
"memory" variant the original uses; revoking visibility when the
observer dies or moves was easy on top of the same per-frame scan,
so there was no reason to ship the simpler version first.

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

- [x] ~~HUD systems despawn+respawn entire UI tree every frame (~30–50 entities) — use change
  detection (`Changed<Selected>`, `Changed<Health>`, `Changed<Producer>`) to update in-place~~
  Resolved via hash-based change detection: `info_panel`, `order_palette`, and
  `build_menu` each carry a `*StateHash(u64)` marker on the panel root, hash a
  `Snapshot` of the rendered content, and early-out when the hash matches.
  Steady-state cost is one snapshot + hash per frame; rebuilds only fire on
  selection / production / HP-bucket transitions.
- [x] ~~`update_unit_highlight` clones and re-adds a `StandardMaterial` per selected/hovered
  unit every frame, leaking orphaned handles — cache per-faction+brightness~~
  Resolved via the `Highlighted(f32)` marker — the system early-outs when the
  unit's currently-baked brightness already matches the desired factor, so
  `apply_brightness` only mints a new `StandardMaterial` on state transitions
  (not per frame). The previous bright handle is replaced by `MeshMaterial3d`,
  so its asset reclaims when the strong handle drops.
- [x] ~~`despawn_health_bars` is O(n×m) — use `HashSet` of removed units or query children~~
  Switched to walking each deselected unit's `Children` directly (`children_q.get(unit)`),
  filtering by an `Or<(With<HealthBarBg>, With<HealthBarFg>)>` query — O(removed ×
  children-per-unit ≈ 2-3) instead of O(removed × total-bars-in-world). The 122
  existing tests still pass; bar lifecycle is unchanged.
- [ ] Melee flash and projectile materials created per-attack instead of cached — extend
  `BeamMaterialCache` to cover all weapon FX
- [x] ~~Animation system allocates `Vec<(i32, i32)>` per animator per frame~~ — hoisted
  `turn_finished` / `move_finished` to `Local<Vec<_>>` parameters on `animation_system`,
  cleared at the start of each animator and drained at the end. Steady state: zero
  allocations.
- [ ] Per-frame `UnitRegistry` lookups for immutable data (speed, weapon name) — cache as
  ECS components at spawn time (e.g. `Speed(f32)`, `WeaponBinding(&str)`).
  *Partially done*: `WeaponBinding(WeaponId)` is now cached at spawn and read by
  `combat_system` + `attack_ground_system`; the `unit_registry.weapon()` /
  `weapon_registry.get()` string-hash chain is gone from those hot loops. `Speed(f32)`
  caching is still pending.
- [ ] `AttackEvent::weapon_name` is `String` (heap alloc per attack) — introduce a `WeaponId`
  newtype (interned string or index into `WeaponRegistry`) so attack events carry a cheap
  `Copy` identifier. `BurstFire.weapon` and `PendingDamage.weapon` are also `String` and
  clone per burst shot / damage event — they inherit from the same `WeaponId` change.
  *Partial*: `WeaponId` + `intern` / `by_id` API and the slot-0 `BUILD_LASER` sentinel
  exist on `WeaponRegistry`. The String fields on `AttackEvent` / `BurstFire` /
  `PendingDamage` / `DelayedHit` are still string-typed — converting them is a
  follow-up touching ~15 files that wasn't bundled with the per-frame fix.
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
  *Partially done*: `combat_system` and `attack_ground_system` now read a cached
  `WeaponBinding(WeaponId)` and call `weapon_registry.by_id()` (a `Vec` index) instead
  of `get(&str)`. Still pending: `tick_burst_fire`, `apply_damage`, and the command-fire
  paths — those carry the weapon as a `String` field on `BurstFire` / `PendingDamage`
  and need that field changed too.
- [x] ~~`bookkeeping::count_small_buildings` scans all `UnitType` entities every 0.25s
  even though buildings are a small fraction and only change when one spawns or dies.~~
  Replaced by `track_added_buildings` (`Added<UnitType>`) + `track_dying_buildings`
  (`Added<Dying>`); per-tick cost is now O(deltas), zero in steady state. Saturating
  drop guards against any spurious `Dying`-without-prior-Add path.
- [x] ~~`tick_deploy_state` walks every Deployable every frame even when nothing moved.
  Filter to `Changed<MoveTarget>` + in-flight transitions — the `Closed`/`Open` steady
  states don't need a tick.~~ *Lighter version landed*: an early-`continue` skips the
  match arm + animator borrow when `timer == 0` and `(state, is_moving)` is already a
  steady-state pair (Open + idle, Closed + moving). The query still iterates all
  Deployables — a marker-component split would require coordinating
  `RemovedComponents<MoveTarget>` and `Added<Deployable>`, which costs more than the
  optimization saves at the current ~handful-of-Pointers scale.
- [x] ~~`ui::minimap::update_minimap` rewrites the full base image via
  `copy_from_slice(&state.base_pixels)` every 0.1s. Track a dirty-rect of the previous
  frame's unit dots + viewport rectangle and restore only those pixels, turning an
  O(W·H) memcpy into O(units + viewport_perimeter).~~ Replaced the full-image
  `copy_from_slice` with a per-pixel dirty list (`dirty_byte_indices`) populated
  by `write_dirty_pixel` from both the dot loop and `draw_line`. Restore is now
  O(touched_pixels) — at ~50 visible units + viewport rect roughly 30× cheaper
  than the 200×200×4 memcpy. Capacity reused across refreshes (steady-state
  zero-alloc). Tested via `write_dirty_pixel_records_byte_offset`,
  `restore_loop_resets_only_dirty_pixels`,
  `draw_line_populates_dirty_list`, and
  `draw_line_clipped_writes_skip_dirty_list`.
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

### FEATURES.md Spec Gaps

Items where [FEATURES.md](FEATURES.md) describes behaviour the code does
not yet deliver. FEATURES.md is the source of truth for user-visible
behaviour; plan.md holds the engineering work to get there.

- [x] ~~**Pointer homing targets Flows**~~ — done.
  `UnitKind::homing_targets_air()` (currently only `Pointer`) now
  overrides the `NoChaseCategory=VTOL` filter in `combat_system`
  ([combat/mod.rs](kernel-panic/src/units/combat/mod.rs)). Unit test
  `only_pointer_homes_on_air_targets` in
  [content/definitions.rs](kernel-panic/src/units/content/definitions.rs)
  pins the list so widening it without intent fails loudly.
- [x] ~~**Friendly cloak fade**~~ — done. `install_cloak_fade_materials`
  clones each piece's `StandardMaterial` with alpha 0.5 on
  `Added<Cloaked>` for player-team units; `restore_cloak_fade_materials`
  reverts via `RemovedComponents<Cloaked>`. Mirrors the
  `install_fade_materials` / `FadeMaterials` pattern used for emerge
  fade-in.
- [ ] **Build menu tabs for multi-builder selection** — FEATURES.md §4
  says "When multiple builders are selected, the build pane has tabs on
  top. When only one is selected, there is still a tab saying the
  builder unit name." `build_menu.rs` picks the first producer via
  `producer_q.iter().next()` and shows its grid; no tabs. Structural:
  needs a focused-builder resource and per-tab click handlers.
- [x] ~~**Queue count on build icons**~~ — done.
  `spawn_build_icon` now takes a `queue_count` and renders a small
  bottom-left badge when non-zero. Per-kind counts are computed in
  `update_build_menu` from the producer's queue.
- [x] ~~**Minimap spotting filter**~~ — done. `update_minimap`'s unit
  query now carries `With<Spotted>`. Under the current blanket-Spotted
  spawn it's a no-op, but the plumbing is in place for when §6 fog is
  un-neutered.
- [x] ~~**Cursor animation 30 fps**~~ — done. `FRAME_PERIOD_SECS`
  raised 1/5 s → 1/30 s in [cursor.rs](kernel-panic/src/interaction/cursor.rs) to
  match FEATURES.md §25.
- [x] ~~**Camera yaw `Q`/`E` vs morph `E` hotkey collision**~~ — done.
  The morph system was renamed to `deploy` (see
  [units/mechanics/deploy.rs](kernel-panic/src/units/mechanics/deploy.rs)) and
  rebound to `D`. `D` now multiplexes across three disjoint selection
  sets: command-fire casters, teleporters, and Bug/Exploit — the
  eligibility predicates never overlap on one kind (covered by
  [`ability_and_deploy_labels_do_not_overlap`](kernel-panic/src/ui/hud/order_palette.rs)).
  Camera keeps `Q`/`E` unchanged. FEATURES.md §15 documents the `D`
  binding.
- [ ] **Missing unit hotkeys (structural)** — FEATURES.md §3 still
  reserves `F` (Fight), `T` (set target), `X` (unset target), `A`
  (attack ground), `P` (patrol). All share the same blocker as
  attack-move (Gameplay Bugs → "Attack-move"): they need the
  Spring-style `CommandQueue` component before implementation is
  coherent.
- [x] ~~**Self-destruct (`Ctrl+D`)**~~ — done, standalone path.
  `SelfDestructCountdown` component (lifecycle.rs) ticks at
  `SELF_DESTRUCT_DELAY = 5.0` seconds and then drops HP to zero;
  existing `death_system` + `ExplodeAs` handle the blast. `Ctrl+D` is
  guarded in `trigger_command_fire_on_hotkey` and
  `trigger_dispatch_on_hotkey` so the same keypress doesn't also fire
  NX Flag or Dispatch. `Stop` removes the countdown.
- [ ] **Signal unit** — FEATURES.md §20 Network table lists Signal as
  an "Air-strike caller (currently a stub)". Needs either an
  implementation (likely piggy-backing on SIGTERM's bomber code path)
  or removal from FEATURES.md.
- [x] ~~**Worm holds fire while cloaked (default)**~~ — done.
  `combat_system`'s attacker query now carries `Without<Cloaked>` so
  Worms / Logic Bombs no longer auto-attack while the cloak is up.
  The `autohold` toggle (manual override) remains deferred — that's
  the "autohold" half of FEATURES.md §20.
- [ ] **Worm `autohold` toggle** — FEATURES.md §20 says the player
  can toggle between default-hold and auto-attack-while-cloaked.
  Needs a toggle button in the order palette plus a `ForceAttack`
  marker that combat_system treats as "fire even while cloaked."
- [ ] **Build-menu progress bar removal** — ✅ DONE (today).
  `spawn_build_progress` replaced with `spawn_build_queue` which only
  shows the "Queue: 3x Bit" text. Unused `UI_PROGRESS_COLOR` constant
  dropped.
- [ ] **Repair cursor on builder + friendly hover** — ✅ DONE (today).
  `resolve_context_cursor` now distinguishes mover / weapon /
  constructor selection and picks `Attack` / `Move` / `Repair`
  accordingly.
- [ ] **ALT-Dispatch double-fire** — ✅ DONE (today). Holding ALT
  inserts the `AutoDispatch` marker without firing an immediate
  `DispatchEvent`; the first 12-batch goes out next frame via
  `tick_auto_dispatch`. Previously the first frame could drain up
  to 24 Packets in a single tick.
- [ ] **Dying units counted as movement candidates** — ✅ DONE (today).
  `movement_system` query now carries `Without<Dying>` so corpses no
  longer contribute to pathing / separation.
- [ ] **Obelisk `Infection` cooldown = 40 s** — ✅ DONE (today).
  `ability_for(UnitKind::Obelisk).cooldown` raised 30 → 40 s to match
  upstream `weapons.tdf` `Infection.reloadtime=40`.

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
  - Cliff climbing: caps and slope penalties match upstream's Spring
    encoding exactly (commit `8d81187`). `SpeedMap::from_heightmap`
    computes `1 - cos(angle)` per cell (matching `ReadMap::UpdateSlopemap`),
    `max_slope_from_degrees` applies the upstream `1.5×` pre-multiplier
    from `DegreesToMaxSlope`, and `slope_mod_from_max_slope` derives
    the slope penalty as `4 / (max_slope + 0.001)`. KP mobile units
    that lack an FBI MaxSlope fall back to `DEFAULT_MAX_SLOPE_DEGREES
    = 36` (MOVEINFO LIGHT/MEDIUM/HEAVY default), giving an effective
    54° geometric cap. `NavGridSet` still holds one bucket per
    distinct cap; `compute_path` picks the tightest bucket whose cap
    ≥ the unit's. The per-step rise gate in `movement_system` uses
    the same encoding via `slope_from_rise_run`.
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
- [x] ~~`EmitSfx` opcode~~ — wired via `dispatch_emit_sfx` in
  [animation.rs](kernel-panic/src/units/assets/animation.rs); spawns
  faction-coloured impact bursts at the script-declared piece. Full
  per-CEG emitter stacks (§4.3) still deferred but the pipeline is
  live. `SetValue` opcode remains unimplemented (only `ARMORED` /
  `ACTIVATION` are emitted from KP scripts and neither has a
  game-visible host effect today).
- [ ] `PieceIndex` component: inner value set but never read (only used as marker)
- [ ] COB piece-space interpolation between sim frames — Spring's `LocalModelPiece` stores
  `modelSpaceTra` with a dirty flag and interpolates between sim ticks for smooth
  animation at render framerate. Worth checking whether our animator lerps or snaps;
  sim runs at 30 Hz, render at up to 240 Hz, so non-interpolated piece transforms will
  visibly pop on fast turrets.
- [ ] **`HitByWeaponId` damage callback** — confirmed called by `byte.bos`,
  `hole.bos`, `carrier.bos`, `expbase.bos` with signature
  `HitByWeaponId(headingZ, headingX, weaponId, damage)` returning a damage
  multiplier (30 = take 30%, 100 = full). Byte's closed-state armor case is
  now handled host-side via `byte_armor_multiplier` (§1.6, commit `8d81187`),
  side-stepping the script wiring entirely. The remaining work — synchronous
  `call_script` in `apply_hit` plus a weapon-name → `weapon_id: u16` mapping
  — is only needed if Hole/Carrier/Expbase turn out to use `HitByWeaponId`
  for non-Byte rules. Shares the u16 interning key with §11.3 weapon IDs.
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

### 11.1 Split the three kitchen-sink files — ✅ DONE

Landed in commit `9db68bf`. `combat.rs` → `combat/{mod, aim, damage,
lifecycle}.rs`; `spawning.rs` → `spawning/{mod, emerge, s3o_mount}.rs`;
`map_loading.rs` → `map_loading/{mod, mipmap}.rs`. Largest remaining
file is `spawning/mod.rs` at ~580 LoC.

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
  are all `String` / `Cow<'static, str>`. Every shot, every burst follow-up,
  and every factory build-laser ray (4× per Kernel per frame in steady state)
  allocates or clones a weapon-name string. Replace with an interned
  `WeaponId(u16)` resolved once at TDF load; store `Vec<WeaponDef>` indexed
  by it. `weapon_registry.get(&str)` shrinks from a `BTreeMap<String, _>`
  lookup per shot to an array index. Keep `&str` only at the TDF boundary.
- Discussed April 2026 and deliberately deferred — current scale doesn't
  show the cost in profiles, and the change touches many types. Pick up
  the next time a feature lands in `WeaponDef` so the refactor amortises.
- Unblocks folding `weapon_infection_duration(&str)` into `strum::EnumString`,
  and lets the SIGTERM-name string compare in `byte_armor_multiplier`
  ([damage.rs](kernel-panic/src/units/combat/damage.rs)) become an integer
  compare.

### 11.4 HUD panels rebuild every frame

- [ ] `ui/hud/info_panel.rs`, `build_menu.rs`, and `order_palette.rs` each
  `despawn_all_children + spawn fresh` their entire subtree on state change.
  `build_menu` and `order_palette` already gate on a `LastSelectionHash`, so
  fully-static frames are free; `build_menu` no longer folds build-progress
  into its hash, so a single building being produced no longer churns the
  icon grid ~60×/sec (April 2026 sweep). The hash now seeds with FNV-1a
  offset basis and the click handler runs before the rebuild so a click
  on a freshly-spawned button isn't swallowed (commit `8d81187` — fixed
  the System homebase showing no build menu). What's still open: for the
  per-tick refreshes that DO fire (HP bars, progress bars, queue badges),
  mutate the retained `Text` / `Node.width` in place rather than
  despawn+respawn the whole subtree. Also drop the per-frame
  `format!("{:.0}", …)` allocs.

### 11.5 `MovementState` / `ProductionState` → change detection — ✅ DONE

Both components were already deleted; behaviour now drives off
`Added<MoveTarget>` / `RemovedComponents<MoveTarget>` (and the producer
equivalents) directly. No grep hits for either type anywhere in the tree.

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

- [ ] **`unit_separation_system`** ([interaction/movement.rs](kernel-panic/src/interaction/movement.rs))
  is still O(N²) — builds a full snapshot and nested-loops it. Route each
  mobile unit through `SpatialIndex::query_radius` instead. Expected ~40×
  fewer distance checks at N=300 units.
- [x] ~~**`gunbase` / `body` piece-name scans**~~ — done. `GunbasePiece`,
  `AimerPiece`, and `HatchPiece` are resolved once at spawn via
  `cob.piece_names.iter().position(...)` ([spawning/mod.rs](kernel-panic/src/units/lifecycle/spawning/mod.rs))
  and read every frame as cached `usize` indices.
- [x] **`spawn_projectile` / `spawn_melee_flash` / `spawn_beam`** used to
  allocate a fresh `Cuboid`/`Sphere` mesh per shot. Now share the unit-length
  primitives cached in `WeaponFxMeshes` and bake thickness+length into
  `Transform::scale`. `StandardMaterial` was already cached via
  `BeamMaterialCache`. (April 2026 simplification sweep.)
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

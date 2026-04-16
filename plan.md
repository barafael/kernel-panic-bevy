# Kernel Panic — Remaining Features Plan

## Current State (April 2026)

**6 crates, ~12,000 lines, 115 tests, all passing.**

Working: map loading (13 maps), original textures, S3O models, 3 factions, unit production, selection (with material-brightening highlight and world-space health bars), movement with QTPFS pathfinding, combat with infection/virus spawning, death animations (COB Killed() scripts + particle bursts), win/loss, COB animations, weapon FX, minimap, HUD, RTS camera, Lua heightmap gadgets, map cycling.

---

## Phase 4: Faction Mechanics

### 4.1 Factory Building on Datavents
**Priority: High — core gameplay loop**

- Assembler (System) / equivalent constructor units can build Sockets/Windows/Ports
- Building placement restricted to GeoVent positions (datavents)
- Build UI: select constructor → right-click datavent → ghost placement → build timer
- Socket/Window/Port become auto-producing once built
- Files: `kernel-panic/src/units/building.rs` (new), modify `production.rs`

### 4.2 Network Buffer System
**Priority: High — faction identity**

- Ports increment a virtual `BufferCount` resource per-team instead of spawning units
- Player clicks Port/Connection to materialize Packets from the buffer
- Packets can dematerialize back into buffer by entering a teleporter (Connection)
- UI: buffer counter display, materialize button or hotkey
- Files: `kernel-panic/src/units/network_buffer.rs` (new), modify `production.rs`

### 4.3 Special Abilities
**Priority: Medium — faction differentiation**

| Ability | Faction | Description | Status |
|---------|---------|-------------|--------|
| Virus Conversion | Hacker | Worm/Virus kills convert enemies to Viruses | **Done** — `Infected` component + `VirusSpawnQueue` in combat.rs, `spawn_queued_viruses` in spawning.rs |
| Virus Death Infection | Hacker | When a Virus dies, nearby enemies get infected (chain spread) | Not started — original uses VirusDeath AoE weapon; needs death-triggered AoE infection |
| DOS Stun | Hacker | Paralyzes target for N seconds | Not started — add `Stunned` component, skip movement + attack while active |
| Worm Cloak | Hacker | Invisible while not attacking, uncloaks to attack | Not started — requires fog of war / visibility system (Phase 6) |
| Bug → Exploit Morph | Hacker | Transform Bug into stationary artillery | Not started — despawn Bug, spawn Exploit at same position |
| Pointer NX Flag | System | Area denial fire lasting 60 seconds | Not started — area effect entity + periodic damage |
| Byte Closed State | System | Toggle 70% damage reduction, restricts movement | Not started — armor modifier component + animation state |

Files: `kernel-panic/src/units/abilities.rs` (new)

---

## Phase 5: AI Opponent

### 5.1 Basic AI
**Priority: High — single-player requires this**

- State machine: Expand → Build → Attack → Defend
- Expand: find nearest unoccupied datavent, send Assembler to build factory
- Build: let factories auto-produce, accumulate army
- Attack: when army size > threshold, send all units to nearest enemy homebase
- Defend: if homebase under attack, recall units
- One AI per non-player team
- Files: `kernel-panic/src/ai/mod.rs`, `ai/state_machine.rs`, `ai/tactics.rs`

### 5.2 Difficulty Levels
- Easy: slower production, smaller attack threshold
- Normal: standard timing
- Hard: faster production, better target selection, multi-pronged attacks

---

## Phase 6: Fog of War

### 6.1 Visibility System
**Priority: Medium — essential for Hacker faction**

- Each unit has a sight radius
- Enemy units outside any friendly sight radius are hidden
- Terrain is revealed permanently once scouted (explored fog vs unexplored)
- Worms stay invisible unless within enemy sight radius AND attacking
- Implementation: per-team visibility grid updated each frame, applied as shader/material override
- Files: `kernel-panic/src/units/visibility.rs` (new), shader modifications

---

## Phase 7: Sound

### 7.1 Audio System
**Priority: Low — gameplay works without it**

- Bevy's built-in `bevy_audio` for spatial sound
- Unit command acknowledgements (move, attack)
- Combat sounds (weapon fire, explosions, death)
- Ambient map sounds
- UI feedback sounds (selection click, build complete)
- Source: original KP ships sound files — extract from mod archive
- Files: `kernel-panic/src/audio/mod.rs` (new)

---

## Phase 8: WASM / Web Build

### 8.1 Pre-bake Map Format
**Priority: Medium — blocks web deployment**

- Build step: `cargo run --bin prebake` converts .sd7 → flat binary (heightmap + texture PNG + features + metadata)
- WASM app loads pre-baked files via HTTP fetch (no filesystem, no 7zip)
- The SMF/SMT parsers already work on `&[u8]` — just need a different loading path
- Files: `tools/prebake.rs` (new binary), `kernel-panic/src/wasm_loader.rs` (new)

### 8.2 GitHub Actions + Pages
- Workflow: build WASM target, run `wasm-bindgen`, deploy to GitHub Pages
- `index.html` shell with canvas, loading screen
- Embed one pre-baked map (Marble Madness) for instant play
- Files: `.github/workflows/deploy.yml`, `web/index.html`

### 8.3 WASM Compatibility
- `spring-map` is already bevy-free — needs `#[cfg(not(target_arch = "wasm32"))]` on filesystem code
- `sevenz-rust` won't compile to WASM — pre-baking avoids this
- `mlua` vendored builds may need special handling for WASM — skip Lua on WASM, pre-apply gadgets during bake

---

## Phase 9: Multiplayer

### 9.1 Networking
**Priority: Low — requires all gameplay to be deterministic first**

- lightyear or bevy_replicon for state replication
- Lockstep or server-authoritative architecture
- Input synchronization: each player sends commands, server validates
- Lobby system: host/join, map selection, faction pick
- Files: `kernel-panic/src/network/mod.rs` (new module tree)

---

## Recommended Order

1. **Factory building on datavents** — completes the core gameplay loop
2. **Basic AI** — makes single-player possible
3. **Easy abilities** — DOS stun, Byte armor, Bug morph (virus conversion already done)
4. **Network Buffer** — completes Network faction identity
5. **Fog of war** — enables Hacker stealth gameplay
6. **Virus death chain infection** — completes the Hacker infection loop
7. **Remaining abilities** — Worm cloak (needs fog of war), NX Flag
8. **WASM pre-bake + deploy** — makes it playable in browsers
9. **Sound** — polish
10. **Multiplayer** — endgame feature

---

## Technical Debt

### Architecture
- [ ] `selection.rs` is a 660-line god file handling 6 concerns: hover, click/drag selection, right-click move commands, material highlighting, 3D health bars, and move indicator visuals — split into `interaction/health_bars.rs`, `interaction/highlight.rs`, etc.
- [ ] `spawn_unit` takes 11 parameters — group asset params into a `SpawnContext` struct or make it a Bevy command
- [ ] `buildable_units()` in `hud.rs` and `default_production()` in `production.rs` encode overlapping "what can X build?" data with no shared source — consolidate into a `buildable: &[UnitKind]` field on `UnitStats`
- [ ] `movement.rs` uses `Option<ResMut<NavGrid>>` — consider making NavGrid always present
- [ ] `load_map_at_index` has too many parameters (9) — consider a `MapLoadContext` struct
- [ ] Empty stub directories: `kernel-panic/src/game/`, `kernel-panic/src/ai/` — remove or populate

### Performance
- [ ] HUD systems (`update_info_panel`, `update_build_menu`, `update_order_palette`) despawn+respawn their entire UI tree every frame (~30-50 entities) — use change detection (`Changed<Selected>`, `Changed<Health>`, `Changed<Producer>`) to update in-place
- [ ] `update_unit_highlight` clones and re-adds a `StandardMaterial` per selected/hovered unit every frame, leaking orphaned material handles — cache per-entity or per-faction+brightness and only create on `Added<Selected>`/`Added<Hovered>`
- [ ] `despawn_health_bars` is O(n*m) — for each deselected unit, iterates all bar entities; use a `HashSet` of removed units or query children directly
- [ ] Melee flash and projectile materials in `weapon_fx.rs` are created per-attack instead of cached like beam materials — extend `BeamMaterialCache` to cover all weapon FX
- [ ] Animation system allocates `Vec<(i32, i32)>` per animator per frame — use `SmallVec` or `Local<Vec<...>>`

### Gameplay Bugs
- [ ] No unit collision avoidance — units overlap when crowded
- [ ] Terrain height not sampled during unit movement (units float/sink on hills)
- [ ] No rally point / delivery point for factories
- [ ] `GameState` not reset on map cycling — win/lose persists across map switches
- [ ] Attack-move command (`A` hotkey) is wired through UI but handler is empty (TODO in hud.rs:849)
- [ ] Feature rotation (`MapFeature.rotation_degrees()`) parsed but never applied when rendering

### Resource Leaks / Cleanup
- [ ] Map cycling: old minimap image handle leaks when `MinimapState` is overwritten
- [ ] `SelectionVolumeMaterial` recreated on every spawn instead of truly cached
- [ ] `Selected`/`Hovered` components on units may cause brief query mismatches during despawn frame

### Build Failures

- [ ] `spring-pathfinding` crate has 2 errors in `search.rs`: wrong closure signature (`Fn(u32, u32)` vs `u32`) and wrong argument count — blocks full workspace build

### Incomplete Implementations
- [ ] COB VM: `GET`/`GET_UNIT_VALUE` opcodes always return 0 — `BUILD_PERCENT_LEFT` returning 0 makes every unit's Create() build-up animation skip instantly. Needs game-state callback integration so the VM can query real unit values (health, speed, build progress, etc.)
- [ ] COB VM: `springdefs.h` constants (ARMORED=20, BUILD_PERCENT_LEFT=17, YARD_OPEN=18, etc.) are not mapped — the VM has no way to translate these to real game state even when `GET`/`SET` are called
- [ ] COB VM: `EmitSfx` and `SetValue` opcodes unimplemented (catch-all `_ => {}` in animation_system) — `EmitSfx` is used by some BOS scripts for sound/particle triggers
- [ ] COB `Show` command works but is only triggered during death scripts — no idle/combat scripts currently call it
- [ ] Factory spawn offset hardcoded to `Vec3::new(40.0, 0.0, 40.0)` in production.rs — should use `QueryBuildInfo` COB callback to get the correct build pad position
- [ ] Atmosphere: `fog_start`, `fog_color`, `cloud_density` parsed from .smd but never applied
- [ ] `PieceIndex` component: inner value set but never read (only used as marker)
- [ ] Death particle effect is a simple expanding sphere — original uses per-piece shatter/fall trajectories from CEG definitions in `gamedata/explosions/`
- [ ] `.smd` parser ignores `startposy` — only X/Z parsed for start positions, which could matter for maps with varying altitude spawn points
- [ ] `[f32; 3]` arrays for colors/directions in smd_parser `Atmosphere`/`Lighting` structs — identified as a type improvement, deferred (a `Color3` newtype with `.to_linear_rgb()` would make consumption sites cleaner)
- [ ] Move indicator torus uses fixed size (inner=2, outer=4) regardless of unit count or formation spread

### Dead Code
- [ ] `mesh_scale` field on `UnitStats` — only used for fallback cylinders when s3o model loading fails; all 19 KP models load successfully, making this field unused in practice
- [ ] `load_smt_from_archive()` in sd7_archive.rs — public function, never called
- [ ] `CobThread::local_function_id()` in spring-cob vm.rs — never used
- [ ] `_vertical` param in `walk_edge()` — unused
- [ ] `_nodes` param in `refine_path()` — unused
- [ ] `_weapon` param in `spawn_melee_flash()` — unused

### Naming
- [ ] `DEEP_FEATURES.md` calls the Network homebase "Carrier" in 3 places but code uses `UnitKind::Connection` — the upstream sidedata.tdf also uses "carrier" as the commander name; consider aligning or documenting the discrepancy

### Compiler Warnings
- [ ] Unused import `Path` in movement.rs
- [ ] Unused import `Arc` in spawning.rs
- [ ] `CallFrame::function_id` never read in spring-cob

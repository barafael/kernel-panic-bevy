# Kernel Panic — Remaining Features Plan

## Current State (April 2026)

**6 crates, ~12,000 lines, 115 tests, all passing.**

Working: map loading (13 maps), original textures, S3O models, 3 factions, unit production, selection, movement with QTPFS pathfinding, combat, win/loss, COB animations, weapon FX, minimap, HUD, RTS camera, Lua heightmap gadgets, map cycling.

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

| Ability | Faction | Description | Complexity |
|---------|---------|-------------|------------|
| Worm Cloak | Hacker | Invisible while moving, uncloaks to attack | Medium — visibility system |
| Virus Conversion | Hacker | Worm kills convert enemies to Viruses | Low — already in combat.rs |
| DOS Stun | Hacker | Paralyzes target for N seconds | Low — add StunTimer component |
| Bug → Exploit Morph | Hacker | Transform Bug into stationary artillery | Medium — entity replacement |
| Pointer NX Flag | System | Area denial fire lasting 60 seconds | Medium — area effect + timer |
| Byte Closed State | System | Toggle 70% damage reduction | Low — armor modifier |

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
3. **Special abilities** (start with easy ones: DOS stun, Byte armor, virus conversion)
4. **Network Buffer** — completes Network faction identity
5. **Fog of war** — enables Hacker stealth gameplay
6. **WASM pre-bake + deploy** — makes it playable in browsers
7. **Sound** — polish
8. **Remaining abilities** (Worm cloak, NX Flag, Bug morph)
9. **Multiplayer** — endgame feature

---

## Technical Debt

- [ ] `movement.rs` uses `Option<ResMut<NavGrid>>` — consider making NavGrid always present
- [ ] No unit collision avoidance — units overlap when crowded
- [ ] Terrain height not sampled during unit movement (units float/sink on hills)
- [ ] No delivery point for factories (right-click on factory should set rally point)
- [ ] Map cycling doesn't clean up NavGrid/MinimapState properly on switch
- [ ] `load_map_at_index` has too many parameters — consider a `MapLoadContext` struct
- [ ] COB VM doesn't implement all opcodes yet
- [ ] Weapon definitions are hardcoded — should come from unit data files

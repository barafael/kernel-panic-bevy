# Kernel Panic: Deep Game Features Report

A reference for implementing the advanced mechanics of Kernel Panic beyond basic
combat and production. All stats come from the upstream `.fbi` unit files, `.tdf`
weapon files, and Lua gadgets in `upstream/Kernel-Panic/`.

---

## Table of Contents

1. [Missing Units](#1-missing-units)
2. [Flows (Network air unit)](#2-flows)
3. [Worms (Hacker cloaked ambusher)](#3-worms)
4. [Bytes (System heavy)](#4-bytes)
5. [Pointers (System artillery)](#5-pointers)
6. [Obelisks (Hacker special building)](#6-obelisks)
7. [Terminal / SIGTERM (System special building)](#7-terminal--sigterm)
8. [Firewall / Reflector Shield (Network special building)](#8-firewall--reflector-shield)
9. [Mines, Minesweepers, and Bad Blocks](#9-mines-minesweepers-and-bad-blocks)
10. [Denial of Service (Hacker stunner)](#10-denial-of-service)
11. [Bug → Exploit Morph](#11-bug--exploit-morph)
12. [Virus Chain Infection](#12-virus-chain-infection)
13. [Network Packet Buffer & Teleportation](#13-network-packet-buffer--teleportation)
14. [Kernel Boost (global production scaling)](#14-kernel-boost)
15. [Armor Classes & Damage Modifiers](#15-armor-classes--damage-modifiers)
16. [Implementation Status](#16-implementation-status)

---

## 1. Missing Units

The following units exist in upstream Kernel Panic but are **not yet in our
`UnitKind` enum** (`definitions.rs`):

| Unit | Faction | Role |
|------|---------|------|
| **Flow** | Network | Air assault unit, speed scales with territory |
| **Gateway** | Network | Mobile constructor (armed, unlike other constructors) |
| **Terminal** | System | Special building, launches SIGTERM bomber strikes |
| **Obelisk** | Hacker | Special building, fires infection gas artillery |
| **Trojan** | Hacker | Mobile constructor with long-range radar |
| **Debug** | Shared | One-shot mine/wall clearing device |
| **Bad Block** | Shared | Cheap destructible wall |
| **SIGTERM** | System | Air unit spawned by Terminal (not directly built) |

Our `Firewall` entry exists but is currently defined as a System building with
no weapon — in upstream, it's a **Network** building with a reflector shield
ability.

---

## 2. Flows

**Faction:** Network  
**Role:** Air assault unit  
**Upstream file:** `flow.fbi`

### Stats
| Stat | Value |
|------|-------|
| HP | 1,000 |
| Build time | 1,400 |
| Base speed | 1.0 (very slow) |
| Flight altitude | 140 |
| Armor class | spam |
| Auto-heal | 30 HP/s |

### Weapon: FlowMissile (Starburst Launcher)
| Stat | Value |
|------|-------|
| Damage | 160 per missile |
| Projectiles per burst | 2 |
| Burst rate | 2 missiles |
| Range | 350 |
| AoE | 128 |
| Reload | 1.5s |

### Dynamic Speed Mechanic
Implemented in upstream `network_flowspeed.lua`:
- **+1.0 speed per small building** (Socket, Window, Port, Terminal, Obelisk,
  Firewall) the owning team controls
- With 4 buildings: speed = 1.0 + 4.0 = **5.0** — making Flows extremely fast
  in the late game
- Speed recalculated whenever a building is built or destroyed

### Key Interactions
- Fragile (1,000 HP); vulnerable to Pointers (homing shots), DoS, and
  Connections
- Invulnerable during construction (DamageModifier ≈ 0)
- Most ground units have `NoChaseCategory=VTOL`, so they won't pursue Flows
- Built by the Carrier (homebase)

---

## 3. Worms

**Faction:** Hacker  
**Role:** Cloaked heavy ambusher  
**Upstream file:** `worm.fbi`

### Stats
| Stat | Value |
|------|-------|
| HP | 12,000 |
| Build time | 3,200 |
| Speed | 2.1 |
| Footprint | 4×4 |
| Armor class | subterranean |
| Idle auto-heal | 300 HP (kicks in after 400 frames idle) |
| Seismic sensor | 1,024 range |
| Own seismic signature | 0 (invisible to seismic) |

### Weapons

**Wormbite (primary, melee):**
| Stat | Value |
|------|-------|
| Damage | 3,200 |
| AoE | 140 |
| Range | 200 |
| Reload | 6s |
| Targets | "EDIBLE" category |

**Wormsplash (secondary, area):**
| Stat | Value |
|------|-------|
| Damage | 800 |
| AoE | 210 |
| Reload | 6s |
| Edge effectiveness | 1.0 (full damage across entire AoE) |
| Special | Infectious — kills become Viruses |

### Cloaking & AutoHold
- **Permanently cloaked** while moving and idle
- Surfaces only to attack, then re-cloaks
- **AutoHold ON** (default for humans): holds fire while cloaked, requiring
  manual attack orders — preserves stealth
- **AutoHold OFF**: auto-attacks, breaking stealth
- Newly produced Worms inherit the Security Hole's AutoHold setting

### Infection Mechanic
Via `infection.lua`: any enemy killed within 200 frames (~6.6s) of being hit by
Wormsplash is **converted into a Virus** on the Hacker's team. This is the core
snowball mechanic — a Worm ambushing a Bit/Bug swarm can generate dozens of
Viruses.

---

## 4. Bytes

**Faction:** System  
**Role:** Heavy attack unit  
**Upstream file:** `byte.fbi`

### Stats
| Stat | Value |
|------|-------|
| HP | 15,000 |
| Build time | 3,600 |
| Speed | 1.5 |
| Footprint | 4×4 |
| Movement class | HEAVY (crushes Bad Blocks) |
| Armor class | heavy |
| Idle auto-heal | 400 HP (after 600 frames idle) |

### Primary Weapon: MegaBeam
| Stat | Value |
|------|-------|
| Damage | 200 per shot × 4-shot burst |
| Total per volley | 800 |
| Range | 512 |
| AoE | 128 |
| Reload | 2s |
| Type | Green beam |

### Special Ability: Mine Launcher (Division Zero)
- **Manually activated** secondary weapon
- **Costs 6,000 HP** to fire (self-damage)
- Launches **5 Logic Bombs** in a forward arc
- Spray angle: 1,000
- Range: 1,100
- Reload: ~10s

### Damage Reduction
- Takes only **30% damage when closed** (normal state)
- Opens to fire, losing the reduction
- Paralyzed/stunned Bytes lose the armor bonus (opens them up)

---

## 5. Pointers

**Faction:** System  
**Role:** Artillery  
**Upstream file:** `pointer.fbi`

### Stats
| Stat | Value |
|------|-------|
| HP | 1,000 |
| Build time | 1,920 |
| Speed | 2.0 |
| Sight range | 768 (longest of System mobile units) |
| Armor class | arty |

### Primary Weapon: Geometric
| Stat | Value |
|------|-------|
| Damage | 4,000 |
| Range | 1,400 |
| AoE | 32 (very small) |
| Reload | 4s |
| Projectile | Homing (turnrate 20,000), trajectory arc |
| Bad vs | FAST units (Bits, Bugs, Packets) |
| Good vs | Buildings, heavy units (Kernels, Bytes, Connections) |

### Special Ability: NX Flag (Division Zero)
- **Manually activated** (command-fire)
- Range: 1,400
- Reload: 30s
- AoE: 240 (large)
- Direct damage: 200

**Area denial** (via `areadenial.lua`):
- Creates a **persistent fire zone** (radius 120) lasting **60 seconds**
- Deals ~100 DPS to **all units including friendlies**
- Excellent for blocking chokepoints or denying terrain

---

## 6. Obelisks

**Faction:** Hacker  
**Role:** Special building (infection gas artillery)  
**Upstream file:** `obelisk.fbi`  
**Built by:** Trojan (on datavents)

### Stats
| Stat | Value |
|------|-------|
| HP | 15,000 |
| Build time | 8,000 |
| DamageModifier | 4× (takes quadruple damage) |
| Idle auto-heal | 50 HP/s |

### Weapon: Infection
| Stat | Value |
|------|-------|
| Direct damage | 200 (+ 1,500 vs buildings) |
| Range | 2,000 |
| AoE | 700 (massive) |
| Reload | 40s |
| Activation | **Command-fire only** (manual targeting) |
| Trajectory | Ballistic |

### Area Denial Effect
- Creates a **poison gas cloud** (radius 400) lasting **~13 seconds**
- ~120 DPS to **enemies only** (does not hurt friendlies)
- Total potential damage: ~1,600 over full duration

### Virus Spawning
- 30-frame infection window after hit
- Any enemy dying within ~1 second of being hit becomes a **Virus**
- Devastating against swarms of low-HP spam units (Bits, Bugs, Packets)

### Visual Indicator
- Displays a **pink fire** on top when the weapon is charged and ready

---

## 7. Terminal / SIGTERM

**Faction:** System  
**Role:** Special building (air strike launcher)  
**Upstream file:** `terminal.fbi`  
**Built by:** Assembler (on datavents)

### Terminal Stats
| Stat | Value |
|------|-------|
| HP | 15,000 |
| Build time | 8,000 |
| DamageModifier | 4× |

### Mechanic
The Terminal doesn't have a traditional weapon. Instead, it spawns a **SIGTERM
bomber** (via gadget) on a 90-second cooldown:

### SIGTERM Bomber
| Stat | Value |
|------|-------|
| HP | 600 |
| Speed | 8 (very fast) |
| Type | Air unit, uncounterable |

### SIGTERM Bomb
| Stat | Value |
|------|-------|
| Damage | 10,000 |
| AoE | 900 diameter |
| Edge effectiveness | 0.8 |

### Area Denial After Detonation
- Radius 350 fire zone
- 2,000 total damage over ~3.3 seconds
- **Damages friendlies** — must be aimed carefully

---

## 8. Firewall / Reflector Shield

**Faction:** Network (NOT System — our codebase has this wrong)  
**Role:** Special building (protective shield caster)  
**Upstream file:** `network_super.fbi`  
**Built by:** Gateway (on datavents)

### Stats
| Stat | Value |
|------|-------|
| HP | 20,000 |
| Build time | 8,000 |

### Reflector Shield Ability
- **Manually activated**, 90-second cooldown
- Targets a location; all friendly units within **radius 300** gain a 20-second
  shield
- Shield effect:
  - **Halves all incoming damage**
  - **Reflects 50% back** at the attacker
- Visible as a green texture effect on shielded units

---

## 9. Mines, Minesweepers, and Bad Blocks

### Logic Bomb (Mine)
**Built by:** Assembler (System), Trojan (Hacker), Gateway (Network) — shared  
**Upstream file:** `mine.fbi`

| Stat | Value |
|------|-------|
| HP | 300 |
| Build time | 120 (very fast) |
| Cloaked | Yes (starts cloaked, zero cloak cost) |
| Trigger distance | 64 |
| Behavior | Kamikaze — detonates on proximity |

**Explosion:**
| Stat | Value |
|------|-------|
| Damage | 900 (default), 3,000 vs subterranean (Worms), 1 vs mines |
| AoE | 512 (very large) |
| Edge effectiveness | 0.8 |
| Friendly fire | No (NoSelfDamage) |
| Limit | 64 per player |

### Debug (Minesweeper)
**Built by:** Assembler, Trojan, Gateway — shared  
**Upstream file:** `debug.fbi`

- HP: 100, build time: 600
- **One-shot clearing device** — explodes immediately on placement
- Weapon: Minekiller
  - AoE: 512, edge effectiveness: 1.0
  - 5,000 damage vs mines, only 20 vs everything else
  - Clears all mines and Bad Blocks in a large area

### Bad Block (Wall)
**Built by:** All constructors — shared

- HP: 100
- Blocks movement of small units
- Does **not** block projectiles
- Can be **crushed by Bytes** or cleared by Debug

---

## 10. Denial of Service

**Faction:** Hacker  
**Role:** Paralyzer/stunner  
**Upstream file:** `dos.fbi`

### Stats
| Stat | Value |
|------|-------|
| HP | 1,700 |
| Speed | 2.7 |
| Build time | (from Security Hole) |
| Armor class | arty |

### Weapon: DOS_Beam
| Stat | Value |
|------|-------|
| Type | Paralyzer (stun, not damage) |
| Paralyze damage | 400 (125 vs spam, 0.1 vs buildings) |
| Paralyze time | 5 seconds per full stun |
| Range | 768 |
| Reload | 0.25s (nearly continuous beam) |
| Visual | Visible particle trail |

### Key Mechanics
- Multiple DoS units stack ("DDoS") to stun large targets faster
- Stunned Bytes **lose their 70% damage reduction** (forced open)
- Will not chase factories (`NoChaseCategory=FACTORY`)
- The visible beam trail means DoS positions are not secret

---

## 11. Bug → Exploit Morph

**Bug** (Hacker spam unit) can **morph into Exploit** via the Deploy command, and
back via Undeploy.

### Bug Stats
| Stat | Value |
|------|-------|
| HP | 400 |
| Speed | 3.8 (fastest non-Packet ground unit) |
| Damage | 130/shot, range 320, reload 0.5s |
| Firing arc | 270° forward (cannot shoot behind) |

### Exploit Stats (after morph)
| Stat | Value |
|------|-------|
| HP | 100 (extremely fragile) |
| Speed | 0 (stationary) |
| Range | 1,200 |
| Base damage | 130 (200 vs buildings) |

### Exploit Dynamic Damage
The Exploit's BugCannon uses `dynDamageExp=1, dynDamageInverted=1,
dynDamageRange=700`:
- Damage **increases with distance** — the farther the target, the more damage
- `proximityPriority=-5` means it **prefers distant targets**
- This makes Exploits excellent rear-line artillery but useless up close

---

## 12. Virus Chain Infection

**Viruses cannot be built.** They only spawn when enemies are killed by
infectious weapons.

### Virus Stats
| Stat | Value |
|------|-------|
| HP | 300 |
| Speed | 3.5 |
| Weapon | VirusBeam: 100 damage, range 220, reload 1s |
| Death explosion | VirusDeath: AoE 90, 50 damage |
| Armor class | infectious |

### Infection Chain
The `infection.lua` gadget tracks three sources of infection:
1. **Worm** Wormsplash kills → Virus
2. **Obelisk** Infection gas kills → Virus
3. **Virus** VirusBeam / VirusDeath kills → Virus

This creates an **exponential chain reaction**: Viruses killing enemies produce
more Viruses. A single Worm ambush on a Bit swarm can cascade into dozens of
Viruses within seconds.

### Infection Window
Each infectious weapon has a tracking window (frames after hit). If the target
dies within that window, it converts:
- Wormsplash: 200 frames (~6.6s)
- Obelisk Infection: 30 frames (~1s)
- Virus weapons: similar short window

---

## 13. Network Packet Buffer & Teleportation

The Network faction's defining mechanic. Implemented in upstream via
`network_buffer.lua`.

### How the Buffer Works
1. **Ports do not produce Packets directly** — they increment a team-wide
   **Packet Buffer** counter
2. Rate: 1 Packet added to Buffer every ~5.5 seconds per Port
3. The Carrier (homebase) also contributes to the Buffer

### Dispatch
- Player issues a **Dispatch** command at any Port or Connection
- **12 Packets** materialize instantly at that location
- Hold ALT for continuous dispatch until Buffer is empty
- Dispatched Packets have a **6-second cooldown** before they can re-enter a
  teleporter

### Enter (Dematerialize)
- Packets can **enter** a friendly Port or Connection to return to the Buffer
- This enables rapid redeployment: absorb Packets at one location, dispatch at
  another

### Strategic Implications
- Every Port is inherently defended — attack one and the Network player
  dispatches Packets instantly
- A lone Connection pushed deep into enemy territory becomes a forward spawn
  point
- The Buffer is a team-wide resource, not per-building

---

## 14. Kernel Boost

Implemented in upstream `kernelboost.lua`.

All homebases (Kernel, Security Hole, Carrier) receive a **+20% production speed
boost per small building** their team controls:

| Small Buildings Owned | Production Speed |
|----------------------|-----------------|
| 0 | 100% (base) |
| 1 | 120% |
| 3 | 160% |
| 5 | 200% |

"Small buildings" include: Socket, Window, Port, Terminal, Obelisk, Firewall.

This creates a snowball dynamic where controlling more datavents makes your
homebase produce faster, which in turn helps you take more datavents.

---

## 15. Armor Classes & Damage Modifiers

From upstream `armor.txt`:

| Armor Class | Units |
|-------------|-------|
| spam | Bit, Bug, Packet, Exploit, Virus, Fairy |
| heavy | Byte, Connection, Reimu |
| arty | Pointer, DoS, Marisa |
| subterranean | Worm |
| building | All structures |
| mine | Logic Bomb, Debug |
| infectious | Virus |

### Construction Invulnerability
All mobile units have `DamageModifier ≈ 0` during construction (effectively
invulnerable while being built).

### Building Vulnerability
Non-homebase buildings (Socket, Window, Port, Terminal, Obelisk, Firewall) have
`DamageModifier = 4` — they take **4× damage** from all sources.

---

## 16. Implementation Status

### Currently Implemented (in our codebase)
- [x] Basic `UnitKind` enum with core units (Bit, Bug, Packet, Byte, Pointer,
      Worm, Virus, DoS, Logic Bomb, Signal)
- [x] `UnitStats` with HP, speed, build time, weapon references
- [x] Simple combat system (nearest-enemy targeting, flat damage)
- [x] Factory production (continuous auto-spawn)
- [x] Weapon registry loading from upstream TDF files
- [x] Three factions with faction colors

### Not Yet Implemented
- [ ] **Missing units:** Flow, Gateway, Trojan, Terminal, Obelisk, SIGTERM,
      Debug, Bad Block
- [ ] **Cloaking system** (Worms, Logic Bombs)
- [ ] **Infection mechanic** (Worm/Obelisk/Virus kills → Virus conversion)
- [ ] **Paralysis/stun** (DoS beam)
- [ ] **Armor classes** and per-class damage multipliers
- [ ] **Byte damage reduction** (30% damage when closed)
- [ ] **Byte mine launcher** (self-damage secondary weapon)
- [ ] **Pointer NX Flag** (area denial secondary weapon)
- [ ] **Bug → Exploit morph**
- [ ] **Exploit dynamic damage** (increasing with range)
- [ ] **Network Packet Buffer** and dispatch/enter teleportation
- [ ] **Flow dynamic speed** (territory-based)
- [ ] **Obelisk infection gas** (command-fire, area denial, virus spawning)
- [ ] **Terminal SIGTERM strikes** (air bomber spawning)
- [ ] **Firewall reflector shield** (damage halving + reflection)
- [ ] **Kernel Boost** (production scaling with building count)
- [ ] **Mines** (cloaked proximity detonation)
- [ ] **Debug** (one-shot mine clearing)
- [ ] **Bad Blocks** (destructible walls)
- [ ] **Air units** and anti-air targeting
- [ ] **Auto-heal** (idle regeneration)
- [ ] **Construction invulnerability**
- [ ] **Building vulnerability** (4× damage modifier)
- [ ] **AoE damage** (splash weapons)
- [ ] **Command-fire weapons** (manually activated abilities)
- [ ] **Datavents** (building placement spots)

### Stat Corrections Needed
Several stats in `definitions.rs` differ from upstream values. Key discrepancies:

| Unit | Our Value | Upstream Value | Field |
|------|-----------|---------------|-------|
| Kernel | 10,000 HP | 40,000 HP | max_health |
| Hole | 10,000 HP | 40,000 HP | max_health |
| Connection | 10,000 HP | 40,000 HP | max_health |
| Bit | 150 HP | 600 HP | max_health |
| Bug | 150 HP | 400 HP | max_health |
| Bug | 80 dmg | 130 dmg | attack_damage |
| Worm | 2,500 HP | 12,000 HP | max_health |
| Worm | 1,500 dmg | 3,200 dmg | attack_damage |
| Exploit | 3,000 HP | 100 HP | max_health |
| Exploit | 200 dmg | 130 dmg | attack_damage |
| Exploit | 512 range | 1,200 range | attack_range |
| Virus | 200 HP | 300 HP | max_health |
| DoS | 1,500 HP | 1,700 HP | max_health |
| DoS | 256 range | 768 range | attack_range |
| Pointer | 2,000 HP | 1,000 HP | max_health |
| Socket | 5,000 HP | 20,000 HP | max_health |
| Window | 5,000 HP | 20,000 HP | max_health |
| Port | 5,000 HP | 20,000 HP | max_health |
| Firewall | faction: System | faction: Network | faction |
| Firewall | 8,000 HP | 20,000 HP | max_health |
| Packet | 300 HP | ~300 HP | (close) |
| Logic Bomb | 500 HP | 300 HP | max_health |

---

*Generated 2026-04-16 from upstream Kernel Panic 4.9 source analysis.*

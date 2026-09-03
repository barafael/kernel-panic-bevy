# Kernel Panic — Complete Game Design Reference

*A player-perspective description of the original Spring RTS mod "Kernel Panic" (v4.912, "Digital Warfare!"), compiled from the game's own readme, unit/weapon data, interface scripts and gamemode logic. This document intentionally avoids implementation detail — it describes the game the way a player experiences it: controls, units, balance, modes, and feel. It is intended as the design bible for any remake.*

---

## Table of Contents

1. [What is Kernel Panic?](#1-what-is-kernel-panic)
2. [The Core Design: No Economy](#2-the-core-design-no-economy)
3. [Starting a Game](#3-starting-a-game)
4. [The Factions](#4-the-factions)
5. [Unit Compendium](#5-unit-compendium)
   - [System (CPU)](#51-system-cpu)
   - [Hacker (Division Zero)](#52-hacker-division-zero)
   - [Network](#53-network)
   - [Shared Toolkit](#54-shared-toolkit)
   - [Secret & Special Factions](#55-secret--special-factions)
6. [How Combat Works](#6-how-combat-works)
7. [Special Abilities & Superweapons](#7-special-abilities--superweapons)
8. [The Network Buffer in Depth](#8-the-network-buffer-in-depth)
9. [Territory, Datavents and Maps](#9-territory-datavents-and-maps)
10. [User Interface & Controls](#10-user-interface--controls)
11. [Game Modes & Options](#11-game-modes--options)
12. [Winning and Losing](#12-winning-and-losing)
13. [AI Opponents](#13-ai-opponents)
14. [Missions](#14-missions)
15. [Balance Analysis](#15-balance-analysis)
16. [Strategy & Tactics Guide](#16-strategy--tactics-guide)
17. [Audiovisual Identity](#17-audiovisual-identity)
18. [Handicaps & Social Play](#18-handicaps--social-play)
19. [Design Evolution Highlights](#19-design-evolution-highlights)
20. [Appendix A: Hotkeys & Console Commands](#appendix-a-hotkeys--console-commands)
21. [Appendix B: Full Stat Tables](#appendix-b-full-stat-tables)

---

## 1. What is Kernel Panic?

Kernel Panic is a **fast-paced, action-oriented RTS about software fighting inside a computer**. Bits, bytes, packets and viruses wage war on a circuit-board battlefield drawn in a retro vector/oscilloscope graphical style. It was built as a mod for the Spring engine, but it plays nothing like traditional Spring games:

- **There is no metal or energy economy.** The only resources are *time* and *space*.
- **Every unit is free.** Every factory you own produces units continuously, automatically.
- **Factories can only be built on datavents** — a small number of special spots on the map. Territory *is* your economy.
- Games are short, aggressive and constant-pressure. There is no "turtling into a maxed economy" phase; there is only expansion, production throughput and battlefield control.

The design philosophy in one sentence: **strip the RTS down to positioning, production cadence and tactical micro, and theme it as digital warfare.**

The title is literal: each player's army is a computer process, and the "Kernel" (home base) is the heart of their operating system. Kill the enemy kernel (or all its factories) and its process crashes.

---

## 2. The Core Design: No Economy

This is the single most important design decision and everything else follows from it.

**What the player experiences:**

- There are **no resource bars** — the mod literally removes them from the interface.
- You never build extractors, never manage income, never queue units against a budget.
- A unit's only cost is its **build time**. A Bit takes ~8 worker-seconds, a Byte takes ~30, a Worm ~27. Production rate = (number of factories) × (their build power).
- Every completed factory **immediately starts producing units on repeat, forever**, unless you intervene. The intended default state of the game is *continuous unit spam from every factory you own*.

**What that means for strategy:**

- **Datavents are everything.** Each one is a potential factory. Controlling one more datavent is one more production line running at all times. The macro game of Kernel Panic is purely spatial: claim vents, deny vents.
- **The home base rewards expansion**: it gains roughly **+20% build speed for every small factory you own**. Expanding makes your existing base faster, too — the rich get richer, which keeps games decisive.
- **Buildings tick out a token trickle of resources** internally so the (invisible) cost of units is always affordable. The player never sees or thinks about this.
- **Unfinished buildings decay** if builders stop working on them — you cannot "bank" half-built structures.
- There is **no veterancy or XP**; units are disposable and stay disposable from minute one to the end.

The consequence is a game that *starts at maximum intensity*. From the first minute, spam units are streaming out of your base. A game typically resolves within 5–15 minutes.

---

## 3. Starting a Game

### 3.1 Setup rules

- **Faction pick**: System, Hacker or Network (plus hidden factions, see §5.5).
- **Start position must be "Fixed" or "Random"** — not "choose in game". Every player starts with exactly **one home base** (Kernel / Security Hole / Carrier) placed on their start spot.
- The home base is your **commander and first factory**: it can build every mobile unit of your faction from the very first second. There is no tech-up; the full roster is available immediately (special buildings require a constructor, though).
- Start resources are a nominal 1000/1000 — irrelevant by design.

### 3.2 Ways to play

- **Skirmish / single player**: an in-game menu (also reachable via Esc) offers one-click *Easy / Medium / Hard / Very Hard* skirmishes, or an **Advanced setup** where you pick map, your faction, enemy faction, team grouping presets (Spectate, Duel, Team Game, Outgunned, **Heroic**), and difficulty. AI-only games auto-restart ("attract mode") when they end.
- **Missions**: a built-in menu lists packaged scenarios with briefings (see §14).
- **Save/Load**: a pseudo-savegame system stores the full battle state (a "dump") and can restore or share it; saves double as the mission format.
- **Multiplayer** through any Spring lobby client.

### 3.3 The single-player menu

Pressing **Esc** (single player only) opens the Kernel Panic menu — skewed retro panels over a tiled background: *Missions, Skirmish, Load, Save, Restart, Credits, Readme, Quit*. On victory it shows "You won! (Keep on playing / Go to Menu)"; on defeat "You lost! (Keep on watching / Restart / Go to Menu)". If you don't touch the mouse the game auto-quits a few seconds after ending.

---

## 4. The Factions

Three playable factions, each with a strongly distinct mechanical identity. All three share the same skeleton: **one 40,000 HP home base that builds everything**, one **mobile constructor**, one **minifactory** (built on datavents, produces the spam unit), a **spam unit**, **heavy/fire-support units**, one **special building**, and the shared toolkit (wall, mine, mine-sweeper).

| | **System (CPU)** | **Hacker (Division Zero)** | **Network** |
|---|---|---|---|
| Theme | The machine itself: ordered, sturdy, direct | The intruder: chaos, stealth, infection, denial | The backbone: mobility, logistics, teleportation |
| Home base | Kernel | Security Hole | Carrier |
| Constructor | Assembler | Trojan | Gateway (armed!) |
| Minifactory | Socket (builds Bits) | Window (builds Bugs) | Port (fills the Buffer) |
| Spam | Bit | Bug | Packet |
| Heavy | Byte | Worm | Connection |
| Fire support | Pointer (artillery) | DoS (stunner) | Flow (air) |
| Special building | Terminal (SIGTERM airstrike) | Obelisk (infection artillery) | Firewall (reflect shield) |
| Play style | Straightforward force, strongest individual units | Assassination, area denial, zombie armies | Hit-and-run raids from anywhere, aerial harassment |
| Difficulty (per in-game setup descriptions) | "easiest" | "trickiest" | "most mobile" |

The game's own advice: **System is recommended for beginners** (its units are honest — fast, tough, strong), Hacker rewards patience and scheming, Network rewards aggressive logistics play and map awareness.

---

## 5. Unit Compendium

Numbers below are from the shipped balance (HP / build time / speed / weapon damage / reload / range). "Cost" is omitted because all units are free — **build time is the only cost**. Ranges are in map elmos; the map is typically ~2000–3000 wide, so a Pointer's 1400 range is "half the map".

### 5.1 System (CPU)

**Build tree:** Kernel → Bit, Pointer, Byte, Assembler · Assembler → Socket, Terminal, Bad Block, Logic Bomb, Debug · Socket → Bit

#### Kernel — home base
- 40,000 HP · builds all four mobile units · rapid self-repair when idle.
- Cannot move. Losing it (and everything else per the win rules, §12) ends you.
- Gains +20% build speed per minifac you own. Under ONS rules (§11) it is usually shielded.

#### Bit — spam unit
- 600 HP · 3.0 speed (fast) · **SPARCing laser**: 80 dmg / 0.5 s / range 256.
- Cheap, fast, weak individually. The bread and butter of every System army.
- Idle regeneration: damaged bits patch themselves up when out of combat — damaged units rotate to the back.
- Slightly tougher than enemy spam (Bug 400 HP, Fairy 300 HP) but slower than both.

#### Byte — heavy
- 15,000 HP · 1.5 speed (slow) · **"DOOM!!!" beam**: 200 dmg × 4-shot burst / 2 s / range 512.
- The faction's tank: soaks enormous punishment, crushes Bad Blocks and wreckage by driving over them.
- Its armor "closes" between shots — visually it folds shut; while closed it takes hugely reduced damage (open only to fire). Stunned bytes can't close and die fast — that's the counter.
- **Special: Launch Mines** — throws a fan of ~5 Logic Bombs forward at the cost of 6,000 of its own HP, 10 s cooldown. An emergency area-denial button.
- Carries a small radar and acts as a targeting-upgrade: more Bytes = more accurate allied fire.

#### Pointer — artillery
- 1,000 HP · 2.0 speed · **geometric shell**: 4,000 dmg / 4 s / range 1400, homing.
- The map's sniper: one or two volleys kill any base or heavy. Shots arc slowly and mostly miss fast movers — best against buildings and big targets.
- Made of glass (1,000 HP) and slow; needs a screen.
- **Special: NX Flag** — manual-fire weapon, 30 s cooldown: sets an area (~240 elmo blast) ablaze for a full minute (~100 dmg/s inside ~120 elmo). Burns friends and foes alike. Outstanding for zoning chokepoints and denying datavents.

#### Assembler — constructor
- 2,000 HP · 2.0 speed · unarmed · radar 500 (detects mines and cloaked units).
- Builds Sockets, Terminals, walls, mines, Debugs. Cannot assist-build other nanoframes quickly; its job is expansion.

#### Terminal — special building (datavent)
- 15,000 HP · **SIGTERM**: every ~90 s, dispatches an uncontrollable bomber that drops a 10,000 dmg, 900-elmo blast **anywhere on the map — there is no defense**.
- Wipes every unit in the zone except factories; home bases take heavily reduced damage and survive. A short firestorm lingers (~2,000 dmg/s for a few seconds).
- The build bar shows its charge; "Ready!" when armed. Use it on blobs, snipe enemy artillery, or delete an exposed expansion.

#### Virus — unbuildable
- 300 HP · 3.5 speed · infection beam: 100 dmg / range 220.
- Created when enemies die to infection (see §6). A free, contagious swarm unit for whoever triggered it.

### 5.2 Hacker (Division Zero)

**Build tree:** Security Hole → Bug, DoS, Worm, Trojan · Trojan → Window, Obelisk, Bad Block, Logic Bomb, Debug · Window → Bug

*(Easter egg: put a brand-new Security Hole on Hold Position before building anything and it reverts to the retro "Old Hacker" roster — old Bug with mine-form, old Worm, old Trojan.)*

#### Security Hole — home base
- 40,000 HP · builds Bug, DoS, Worm, Trojan · sets the default **AutoHold** stance its newborn Worms inherit (§7).

#### Bug — spam unit
- 400 HP · **3.8 speed (fastest ground spam)** · failure beam: 130 dmg / 0.5 s / range 320 (out-ranges the Bit).
- Strafes while attacking; regenerates unusually fast when idle (out-heals a Bit two-to-one).
- Cannot shoot behind itself or through friendly units — frontal brawler, melts to flanking.
- **Special: Deploy / Bombard** — morphs into the Exploit.

#### Exploit — deployed artillery
- 100 HP (a stiff breeze kills it) · **bug-spit cannon**: 130 dmg (200 vs buildings) / 2.2 s / range 1200, damage grows with distance to target.
- The Bug's artillery form. Bombard walks the bug into range, deploys and optionally fires; Undeploy morphs back. Classic glass cannon: devastates static targets, must retreat if anything sneezes at it.

#### Denial of Service (DoS) — stunner
- 1,700 HP · 2.7 speed · green stun beam: range 768, ticks every 0.25 s, **paralyzes for 5 s**; reduced damage vs spam so it stuns rather than slaughters, near-zero vs buildings.
- Not artillery — a *disabler*. Bigger targets need more uptime to lock down: stack DoSes ("perform a DDoS") to freeze Bytes, Worms, Connections in place for your Pointer/Byte line. Its particle trail is visible from across the map, so it can't sneak.

#### Worm — heavy ambusher
- 12,000 HP · 2.1 speed · permanently **cloaked** while traveling · **chomp()**: 3,200 dmg bite, plus a huge 210-elmo **splash that damages your own units too** (~800).
- Surfacing to attack is announced by its own visual effect; it surfaces only on manual attack orders while cloaked (AutoHold on, the default) — turn AutoHold off to let it attack autonomously.
- Kills by swallowing herds of spam; anything that dies to its splash (or the Obelisk's poison, or Virus attacks) **rises as a Virus on your side** (§6).
- Nearly immune to other Worms' splash and to Virus weapons (0 damage multiplier vs. fellow subterraneans) — Worm-on-Worm fights are grinds.
- Detects enemy movement seismically at long range while emitting no seismic signature itself: the perfect assassin. Countered by constructor radars.

#### Obelisk — special building (datavent)
- 15,000 HP · **slowest build in the game** (~8,000 build time — guard it while it goes up).
- **Infection ("WMD")**: manual-fire lobbed shell every ~40 s, range 2000, big 700-elmo impact; leaves a poison cloud (~400 elmo, ~120 dmg/s, ~13 s, enemies only). The pink flame on top shows it's loaded.
- Anything that dies inside becomes **your** Virus. One good shot into a spam herd converts it into an army. ~1,000–1,500 total damage; 1,500 direct vs buildings.

#### Trojan — constructor
- 2,000 HP · 2.0 speed · radar 768 (the faction's mine/cloak detector).
- Builds Windows, Obelisks, walls, mines, Debugs.

#### Virus — see §5.1; the Hacker manufactures them in bulk.

### 5.3 Network

**Build tree:** Carrier → Packet, Connection, Flow, Gateway · Gateway → Port, Firewall, Bad Block, Logic Bomb, Debug

The Network faction does not "produce" units the normal way. **Ports don't spawn units — they fill a shared team Buffer** of stored Packets, which are then materialized at teleporter buildings anywhere on the map (§8).

#### Carrier — home base
- 40,000 HP · builds Packet, Connection, Flow, Gateway.

#### Packet — spam unit
- 500 HP · **4.0 speed (fastest unit in the game)** · tight-turning · green beam: 130 dmg / 0.75 s / range 250.
- Weakest fighter of the three spams but unbeatable at raiding, kiting and rushing unguarded expansions.

#### Connection — mobile teleporter / heavy
- 15,000 HP · 1.5 speed · **Particle Whip**: 4,500 dmg / 4 s / range 450.
- Doubles as a Dispatch point and a Packet re-entry point — a mobile forward base. One-on-one it beats most heavies; it hates Pointer fire (4,000 dmg volleys vs 15,000 HP) and is inefficient against swarms.

#### Flow — air unit
- 1,000 HP · flying (only buildable aircraft in the game) · missile volley: 160 dmg × 2 / 1.5 s / range 350, homing.
- Starts slow but **flies faster for every building you control** — on a well-expanded map it's a blur. Ignores terrain entirely.
- Anti-spam and anti-fire-support role; folds instantly to concentrated return fire. Pointers, DoSes and Connections shred it if it gets hit.

#### Gateway — armed constructor
- 2,000 HP · 2.0 speed · radar 500 · **pale-green beam: 100 dmg** — the only constructor in the game that shoots back.
- Builds Ports, Firewalls, walls, mines, Debugs.

#### Port — minifactory / teleporter
- 20,000 HP · adds **+1 Packet to the team Buffer every ~5.5 s** · can Dispatch (§8) · cannot build units directly.

#### Firewall — special building (datavent)
- 20,000 HP · **Firewall ability** every ~90 s: cast on an area (radius 300) — all allied units inside are protected for **20 s**: incoming damage halved, and the other half is **reflected back at the attacker**.
- The ultimate force-field: cast it on a brawl and your swarm becomes unkillable while the enemy kills itself. The build bar shows the charge countdown; a green shimmer marks protected units.

### 5.4 Shared Toolkit

Every faction's constructor can build the same three utility units:

| Unit | Stats | Purpose |
|---|---|---|
| **Bad Block** | 100 HP wall, 1×1 | Blocks *movement*, not shots. Funnels enemy spam into killzones. Bytes crush through them. Removed instantly by the Debug. (Banned in default single-player — the AI can't handle them.) |
| **Logic Bomb** (mine) | 300 HP, permanently cloaked | 900 dmg blast with big radius: one-shots spam, ~3,000 vs Worms. **Does not chain-detonate**, but does hit *your own* units. Limited to **64 per team**. Detected by constructor radar. |
| **Debug** (mineblaster) | one-shot, invisible, untargetable | A placed charge that erases all Logic Bombs and Bad Blocks in its area. The designated mine-clearer; a "Debug" button appears in your build bar. |

### 5.5 Secret & Special Factions

Not exposed in the standard faction pick, but present and playable via setup tricks, missions and mod options:

#### Touhou (fan faction, "added for lulz" — characters © ZUN)
- **Magical Circle** (base) → Fairy, Reimu, Marisa, **Alice** (constructor, 3,000 HP — tankiest builder) · **Small Circle** (minifac) → Fairies.
- **Fairy**: 300 HP, double bullet thrower (2 × 40 dmg, 3-round bursts) — danmaku-style swarm.
- **Reimu**: 5,000 HP anti-swarm fighter, sprays wide 5-shot volleys. Shreds herds.
- **Marisa**: 1,800 HP, must stop and chant to charge her **Master Spark** — a homing 4,500 dmg laser. Devastating anti-armor sniper.
- In Heroes mode, the Touhou hero is **Cirno** (Mega Fairy).

#### Rock-Paper-Scissors (RPS)
- **Hand** factory → Rock, Paper, Scissors. The three units are *statistically identical* (1,000 HP, fast, 256 range): the entire faction is a mind-game triangle — each deals **12× damage (600)** to its prey, 1/10 damage to its predator, 20 to its own kind. Rock beats Scissors beats Paper beats Rock.
- Unique gimmick: RPS units that walk onto an unclaimed datavent **transform into a Hand factory** that self-constructs over 60 s. Expansions are automatic — you just flood units at vents. Has its own 3-mission campaign.

#### Old Hacker (retro side)
- Security Hole, Window, Bug (can toggle into a mine form!), DoS, Worm, Trojan — the pre-"Division Zero" roster, kept playable as an easter egg (§5.2).

#### Experiment (dev faction, unreleased)
- Skeleton faction with placeholder models: unarmed Scout, mobile artillery (4,000 HP), a static artillery with an enormous 4,000 range, and a base that can spawn mobile artillery behind itself. Never enabled in normal play.

---

## 6. How Combat Works

### 6.1 Damage & armor classes

Every unit belongs to an **armor class**, and individual weapons define per-class damage multipliers. The classes: `spam` (Bit, Bug, Packet, Exploit, Fairy), `arty` (Pointer, DoS, Marisa), `heavy` (Byte, Connection, Reimu), `flyer` (Flow), `subterranean` (Worm), `constructor`, `building` (all bases/minifacs), `mine`, `infectious` (Virus), and the RPS trio.

Key interactions every player learns:

| Weapon | Special multipliers |
|---|---|
| DoS stun beam | 125 vs spam (stuns without one-shotting) · **0.1 vs buildings** |
| Pointer shell | 4,000 flat — kills most non-heavies outright |
| Exploit cannon | 200 vs buildings |
| Obelisk shell | 1,500 vs buildings |
| Worm splash | ~0 vs other subterraneans/Worms · 10× vs Viruses |
| Logic Bomb blast | 3,000 vs Worms · 1 vs other mines |
| Debug charge | ~5,000 vs mines, token damage vs everything else |

Generic rule of thumb: **AoE weapons hurt spam, sniper weapons hurt heavies, stun hurts everything.**

### 6.2 Death is noisy and productive

- Units explode in retro beam-bursts; **bases explode violently** (~5,000 dmg over a ~380-elmo radius) — killing a base next to your own army hurts.
- **Infection**: units killed by Worm splash, Virus beams/deaths, or the Obelisk cloud spawn **Viruses for the attacker**. The Virus is a full unit (300 HP, fast, infection beam of its own) and re-infects on its own kills: one Worm surfacing in a spam herd can snowball into a self-replenishing zombie swarm fighting for the Hacker.
- **Stun**: DoS beams paralyze for 5 s per lock; a paralyzed unit is helpless, can't close its armor (see Byte) and can be eaten at leisure.

### 6.3 Survival mechanics

- **Spawn protection**: freshly produced mobile units are effectively invulnerable for a few seconds while they materialize (longer for heavies). You cannot kill units inside their own factory's exit — factory-camping is impossible.
- **Unfinished buildings are fragile**: a building under construction takes **4× damage** until completed. Killing enemy expansions mid-build is a core skill; so is protecting your constructors.
- **Idle regeneration**: damaged units slowly self-repair when out of combat (heavies fastest — a damaged Byte repairs at a remarkable rate if you disengage it). Formation play — damaged units to the back — is genuinely rewarded.
- **Home bases self-repair** quickly when idle and have 40,000 HP: raiding one requires committed force, not a hit-and-run.
- **No friendly fire from most weapons** — but note the deliberate exceptions: Worm splash, NX fire, Logic Bombs and SIGTERM's firestorm all damage allies. Position accordingly.
- **No veterancy, no capture** — the board resets unit-by-unit, and only buildings matter long-term.

---

## 7. Special Abilities & Superweapons

The special buildings and hero abilities that give Kernel Panic its spice. All are optional ("Evilless" mode removes them all for a purer spam game).

| Ability | Unit | Cooldown | Effect (player summary) |
|---|---|---|---|
| **SIGTERM** | Terminal | ~90 s | Unstoppable bomber nukes any point: ~16,000 damage over a huge area; kills all units, spares factories, hurts home bases lightly; brief lingering firestorm (friendly fire). |
| **NX Flag** | Pointer | 30 s | Sets ~120-elmo area on fire for **60 s** (~100 dmg/s, friendly fire). Zone control / expansion denial. |
| **Infection** | Obelisk | ~40 s | Poison cloud (~400 elmo, ~13 s) that damages enemies and converts every kill into a friendly Virus. |
| **Firewall** | Firewall | ~90 s | 20 s aura (radius 300): allies inside take 50% damage; the other 50% is reflected to attackers. |
| **Launch Mines** | Byte | 10 s | Costs 6,000 HP; throws ~5 Logic Bombs forward. Emergency minefield. |
| **Deploy / Bombard** | Bug → Exploit | — | Morph into stationary long-range cannon; damage increases with range. Undeploy to flee. |
| **Dispatch / Enter** | Port, Connection / Packet | — | Materialize/absorb Packets from/to the Buffer (§8). |
| **AutoHold** | Worm (inherited from Hole) | toggle | Cloaked worms hold fire (stealth preserved) vs attack automatically. |
| **Burrow** | Old-Hacker Bug/Worm | toggle | The retro units' mine/cloak stance. |
| **Deploy** | ExpScout/Exploit (Experiment) | — | Unused dev content. |

**Ability cadence is a real strategic clock.** Experienced players track enemy Terminal/Obelisk/Firewall charges ("is their SIGTERM up?") and time pushes accordingly — the UI shows all three on the build bar with visible countdowns.

---

## 8. The Network Buffer in Depth

The Network faction replaces per-factory production with a **packet buffer** — an army in cyberspace:

1. **Filling**: every finished Port adds +1 packet every ~5.5 s. Buffers accumulate up to the team unit cap.
2. **Storing**: a Packet that right-clicks (or presses **E** toward) any friendly Port or Connection *dissolves back into the Buffer* — withdraw a wounded raiding party to safety instantly. (Freshly dispatched packets can't re-enter for 6 s, to prevent abuse.)
3. **Dispatching**: Port or Connection given a **Dispatch** order materializes **up to 12 packets** in a ring around itself and auto-orders them to the target point. **Hold Alt to drain the whole buffer.** This is a *surprise reinforcement teleport*: three Ports on your side of the map can dump 36 Packets onto an enemy expansion in two seconds.
4. **Death in the buffer doesn't count**: buffered packets can't be killed; losing a Port only stops the inflow. In Save-Our-Mem mode, running out of memory *empties the buffer* — a double punishment for Network.

The Buffer count is displayed in the tooltip ("Bufferised Packets: N") and the port tooltips coach new players through the loop. The result: Network games revolve around *where to concentrate your teleporters*, not where your factories are.

---

## 9. Territory, Datavents and Maps

- **Datavents** (glowing map features, thematically "geovents" of the machine) are the only legal construction sites for minifacs and special buildings. Custom maps place them deliberately: clusters, rings, center hotspots, chokepoint patterns.
- While placing a building (or in "metal view"), the game **highlights all datavents as blinking green squares** on ground and minimap, and draws a faint "geo web" connecting nearby vents — the map's strategic skeleton made visible.
- If a map has no geovents, a fallback converts metal spots into datavents ("Datavent spots" option: Auto/Metal/Geo/Both) so any map is playable — but KP is designed for its own maps.
- **The official maps** are legendary in the community and named for computer parts: *Marble Madness*, *Direct Memory Access*, *Major Madness*, *Speed Balls 16-Way*, *Digital Divide*, *Spooler Buffer*, *Data Cache L1*, *Palladium*, *Central Hub*, *Corrupted Core*, *Dual Core*, *Quad Core*, *Memory Bank*, *Pacman*, *Hex Farm 8*, *FireStorm*. Several support 4–16 players in compact, vent-dense layouts that keep the spam colliding constantly.
- All units are amphibious; water is mostly a visual/movement-speed modifier, not a strategic barrier.
- Start boxes are shown on the minimap during placement; at game start the camera auto-centers on your base and auto-selects it.

---

## 10. User Interface & Controls

Kernel Panic ships a fully custom interface. No resource bar; a green-on-black terminal aesthetic throughout.

### 10.1 Screen layout

| Area | Content |
|---|---|
| **Top-right** | **Build Bar** — one icon per owned factory/special building. The central production UI (below). |
| **Right edge** | Player list (Allies / Enemies / Spectators) with faction icons, ping, and ally resource/give buttons. |
| **Left edge** | Slim 3×9 command-button grid for the selected unit (state buttons have LED indicators). |
| **Bottom-left** | **KP Tooltip** (unit stats, terrain info, buffer counts, ONS status) + stacked **O.N.S. help** and cyan **Tip Dispenser** boxes. |
| **Over units** | Health bars, build progress, EMP/stun blink, reload bars, shield indicators. |
| **Center-bottom (SoS games)** | The green→red sector-divided **Memory bar** (§11.3). |

### 10.2 The Build Bar (signature UI)

A persistent row of factory icons at screen top-right:

- **Hover** a factory icon → its build menu unfolds; left-click an icon to queue a unit (Shift/queue modifiers work).
- The icon of a busy factory shows **the unit currently being built with a pie-slice progress overlay**; queued units appear as small counts.
- **Clicking a factory icon selects it in the world** — you can drive your home base from anywhere without ever scrolling to it.
- **Terminal / Firewall icons show their special-charge countdown** ("47s" … "Ready!") and clicking the icon *casts the ability directly*.
- **Middle-click** an icon → camera jumps to that building.
- **Lazy/greedy waypoint modes**: with a factory icon held (right-click on the bar enters "greedy" mode), right-clicks on the map issue move orders for that factory's products, with the queue lines drawn in-world. Any left-click exits.
- Resizable with the mouse wheel over the bar; draggable in tweak mode.

### 10.3 Automation

- **Autospam**: your home base starts with *repeat* on, and every completed minifac is automatically set to **spam its unit forever**. Production takes care of itself; the player commands armies, not queues. (Toggleable per factory like normal Spring repeat.)
- **Autoquit/attract**: idle end-screens auto-restart or quit.

### 10.4 Mouse

- **LMB**: select / drag selection box / place buildings / **fire (hero mode)**.
- **RMB**: smart default order. Context intelligence: with Packets selected, right-clicking a Port/Connection = **Enter (into buffer)**; with only Ports selected, right-click = **Dispatch**. Otherwise standard move/attack context rules.
- **Hold RMB and drag** (nothing selected): draw a **formation line** — units assign themselves along the drawn line (CustomFormations). The first mission teaches exactly this.
- **Drag while attack/move order active**: area orders. **Shift** queues orders.
- **Wheel**: zoom; over build bar/tooltip/tip box: resize them.

### 10.5 Keyboard (Kernel Panic custom binds)

| Key | Action |
|---|---|
| **Numpad 2/4/6/8** (double-tap) | Set build facing S/W/E/N **and** queue a minifactory — one-key base expansion toward a chosen direction |
| **D** | Faction-agnostic "special": DGun-style cursor — SIGTERM target (Terminal), NX Flag (Pointer), **Deploy** (Bug), **Launch Mines** (Byte), **Dispatch** (Port/Connection) |
| **Shift+D** | Queue the above |
| **U** | **Undeploy** (Exploit → Bug) |
| **E** | **Enter** (packet into Port/Connection → buffer) |
| **Arrow keys** | Move hero (Heroes mode) |
| **Ctrl+D** | Hero self-destruct |
| **Esc** | Kernel Panic menu (single player) |

### 10.6 Coaching systems

- **Tip Dispenser**: a cyan box at bottom-center serves a new contextual tip every ~10 s, **with per-faction voice-over** ("Click that blue box to disable tips voice over"). Tips cover selection, formations, unit roles, the Buffer, hero controls — a built-in interactive tutorial.
- **O.N.S. helper**: attack a shielded (invulnerable) enemy building and a billboard appears over it explaining *why it's invulnerable and what to do about it*; hovering shielded buildings shows the owner's exact shield coverage.
- **Mission briefing** box docks next to the minimap; mouse-over expands the full colored briefing text.
- **Team-elimination messages** are flavored: *"Team 2 (Rafael) followed a null pointer"*, *"was abo!_/\…NO CARRIER"*, *"got DELETED!"*.
- Console commands are quality-of-life translated (`/cw 5`, `/ons 0`, `/sos 4096` — no `/luarules` prefix needed; see Appendix A).

### 10.7 Hero-mode controls (Heroes of Mainframe)

- **Arrow keys** move your hero (streams movement goals); **mouse aims**, hover to target, **LMB fires**; **Ctrl+D** self-destructs.
- The normal cursor is replaced by a gun-aim cursor — the mouse literally is your weapon.
- **RMB commands the rest of your army** (if enabled): ground = move all, ally = guard, enemy = attack, **datavent = build a minifac with an idle constructor**. Orders render as floating world-text markers ("Move", "Build", "No builder available!").
- Camera locks onto your hero at weapon range; enemy heroes get a halo + name tag clamped to screen edges; a kill-streak scoreboard tracks players.
- One hero per human player per team; heroes are built from the home base and respawn there (strongest faction spam unit scaled up: ×20 HP, ×10 damage, ×2 speed, ground-hugging shots, fast regeneration).

---

## 11. Game Modes & Options

The base game ("kill all factories") is layered with mutually-compatible special modes set as Mod Options in the battle room (or team-level overrides).

### 11.1 O.N.S. — shielded home networks (newbie protection)
Wraps buildings in **invulnerability shields** so veterans can't base-race new players. Four strengths:

- **Homebase only**: base immune while you own any minifac.
- **Weak**: your network's *extremities* are attackable, interior buildings immune.
- **Strong**: only your single furthest-out building is attackable.
- **Ultra**: only buildings *adjacent to enemy buildings* are attackable.

Shielded buildings are visually wrapped in team-colored translucent hex-shields and connected by glowing **link beams** to their network parent — the base network is rendered as a literal network graph. The only way in is through the exposed frontier, so ONS converts KP into a frontline-push game. Team-colored gauge/status info is shown in the ONS helper widget. Can be toggled off mid-game per team (`/ons 0`) — famously used as a self-handicap.

### 11.2 Color Wars
A doom timer. When it hits zero, **every unit on the map freezes** and colored blocks bloom out from under every unit footprint, flooding the entire map. When the flood finishes, the player with the most blocks wins; losers' units are deleted. Global vision is switched on for the finale. `/cw` edits the timer mid-game. A pure "most territory at time T" mode with a spectacular finish.

### 11.3 Save Our Mem (SoS)
Memory-leak survival: every unit you lose leaves a **team-colored "memory leak" circle** on the ground. Each completed building = one filled memory **sector**. Mature leaks drain your memory bar every second; a friendly ground unit touching a leak frees it (allies can help). **Run out of memory and every mobile unit you own dies and your buildings are crippled** (the negative option makes it instant total death instead). Adds constant cleanup logistics and punishes reckless trading; the drain bar is a big screen-edge UI element.

### 11.4 King of the Hill
A center-hold mode. The clock **starts at first blood** (no frame-1 rush), and 3D translucent gauges rise at map center — every second, the team owning the unit nearest dead-center fills its gauge. First full gauge wins instantly: a SIGTERM effect wipes all opposition off the map.

### 11.5 Heroes of Mainframe (shoot'n'run)
The RTS becomes an arcade hero shooter layered over the RTS (§10.7). Each human player gets one **Mega** hero (Megabit, Megabug, Megapacket, Cirno, Super Virus…) built from the home base; AI-only teams get none. "Heroic" is a preset grouping in the skirmish generator. Heroes are strong but not unkillable, respawn at the base, and the army keeps fighting on AI orders around you. Pairing with ONS or "Kill homebase" win rule is recommended so the game can actually end.

### 11.6 Invasion (co-op survival)
The Invasion AI cheats: it spawns ever-larger **announced waves** of attackers (with on-map warnings), and its aggression scales with how hard you hit its bases and how many superweapons you build. The players defend. Wave composition is cost-weighted (SIGTERMs and Worms are the scary end-game waves).

### 11.7 Other options

- **Pre-placed Minifacs**: start with all datavents evenly pre-covered (+ one special building per team if it holds ≥4 vents). Standard for Hero games; also speeds up any match.
- **Evilless / "Remove special buildings and abilities"**: no Terminals/Obelisks/Firewalls, no NX, no mine launcher, no Deploy/Bombard, no Dispatch-from-Connection — the "pure 1.0 spam game" preserved as an option.
- **Force all factions to System**: every home base becomes a Kernel — mirror matches only.
- **Rebalancing Formula**: a one-line stat-rewriting mini-language in the mod options (e.g. `socket window port maxdamage /4 buildtime x0.25`, `unit hero maxvelocity /3 maxdamage x5`) with aliases (`light`, `heavy`, `arty`, `hero`, `fac`, `home`, `all`). The community used it to hot-patch balance without re-releasing the mod — a remarkably player-accessible balancing tool.
- **Engine options**: win rule (see §12), unit cap (default 1000 — also caps the Network Buffer), game speed clamps, fixed alliances, ghost enemy buildings after LOS loss.

---

## 12. Winning and Losing

Four configurable end conditions, checked once per second:

| GameMode | Rule |
|---|---|
| 0 — Kill everything | You die when your last unit dies (mines don't count) |
| **1 — Kill all factories** (default) | You die when your **home base and all minifacs** are dead |
| 2 — Kill homebase | You die when your base dies — all your surviving units die with it (lineage death, even gifted units) |
| 3 — Never ends | No elimination; for sandbox/KOTH games |

Since mines are ignored and buffered packets aren't real units, the elimination check is forgiving of edge cases. On elimination a team is removed (flavored chat message); the game ends when only allied teams remain — or earlier via Color Wars count, KOTH gauge, or SoS instant-death cascade.

**Read as game design**: the default rule means *you win by erasing territory, not by wiping armies*. A player with zero units but one hidden Socket is still alive; a player with fifty Bits and no factories is already dead. Raids on vents are lethal in a way army battles rarely are.

---

## 13. AI Opponents

- **Kernel Panic AI (KPAI)** — the standard bot. Plays the actual game: claims datavents with constructors, keeps minifacs spamming, groups spam/heavy/arty, snipes with Pointers, fires NX Flags at enemy positions, calls SIGTERMs on blobs, deploys Bugs into Exploits at range, un-deploys under threat, bufferizes idle packets, uses Firewall when a fight turns bad, and in ONS mode correctly attacks minifacs before shielded bases. Debug output via `/kpai`.
- **Fair KPAI** — deliberately caps its own production to match the opponents' output, so it never snowballs a weaker player. The recommended single-player opponent.
- **Invasion AI** — the cheating wave-spawner (§11.6).
- **Regenerative AI** — an unlisted utility AI used by missions: snapshots the starting army and endlessly rebuilds exactly what it loses, restoring orders and stances; also manages Network auto-dispatch.
- External Spring AIs (Shard, Baczek's KP AI, NTai) partially work — they play "harder but blinder", ignoring most special abilities.
- Difficulty in the skirmish generator scales AI count/aggression rather than cheating.

---

## 14. Missions

Shipped scenarios (each a saved-state start script with briefing, triggers and scripted AIs):

1. **Challenge 1 — Bug Squashing**: tutorial on selection boxes and formation-line movement; keep your Bits in formation against Bug waves ("keep damaged bits at the back. Idle bits will slowly heal.").
2. **Challenge 2 — Herd and Pick**: using AoE vs herds.
3. **Challenge 3 — Charge of the Hero**: the HOMF mode as a mission ("Move with arrow keys / Aim with mouse / Fire with left mouse button").
4. **Challenge 4 — Heavy Divide**: heavy-unit micro.
5. **Challenge 5 — Tight Camp**: breaking a fortified position.
6. **Challenge 6 — Navigating through NX flags**: pathing through burning denial zones.
7. **R.P.S. 1–3**: a mini-campaign for the secret Rock-Paper-Scissors faction ("A Rock, a Paper and a pair of Scissors", "A hand facing another hand", "Bad Starting Hand").
8. **Script examples**: Spooler Buffer, Data Cache, Pathfinder Test — documented templates for building your own missions (the `/dump` command turns any game state into a mission file).

Missions show a "Mission: …" briefing box next to the minimap; hover to read the full briefing.

---

## 15. Balance Analysis

### 15.1 The role triangle

KP runs a clean three-way counter system executed through armor classes and weapon profiles:

- **Spam** (Bit/Bug/Packet/Fairy): cheapest, fastest to field, wins by numbers and map presence. Countered by **AoE and area denial** (Worm splash, Pointer near-misses don't splash, Exploit, Reimu, Obelisk, mines, NX fire).
- **Heavy** (Byte/Worm/Connection/Reimu/Marisa): high HP + high single-shot damage; wins head-on fights. Countered by **focus fire, stun locks (DoS stacks), and artillery sniping** (Pointer 4,000 dmg volleys).
- **Artillery/fire support** (Pointer/DoS/Flow/Marisa/Exploit): enormous reach or control. Countered by **fast flankers getting into their minimum range** — a Packet swarm that reaches a Pointer battery kills it in seconds.

Every faction ships one of each role plus a special building, so matchups become "whose artillery survives the spam war long enough to kill the heavies".

### 15.2 Numbers that define the feel

| Dimension | Value | Consequence |
|---|---|---|
| Spam build time | 240–270 (~8–9 s) | Armies self-regenerate; losses are noise |
| Spam HP | 300–600 | Any AoE hit kills; healing while idle rewards formation |
| Heavy build time | 3,200–3,600 | A lost Byte/Worm/Connection actually stings |
| Heavy HP | 12,000–15,000 | Needs artillery or several volleys to remove |
| Artillery range | 1,400 (Pointer) vs spam 250–320 | Arty kills bases from off-screen; must never be caught |
| Base HP | 40,000 + fast idle repair | Bases die to sieges, not raids |
| Minifac HP | 20,000 but **4× fragile while building** | Expansions are decided mid-construction |
| Specials | 90 s / 40 s / 30 s cooldowns | Ability cadence drives push timing |
| Mine cap | 64 per team | Persistent minefields, but finite |
| Unit cap | 1,000 (configurable) | Also the Network Buffer ceiling |

### 15.3 Faction asymmetry (intended strengths/weaknesses)

- **System**: best raw stats per unit (toughest spam, true tank, hardest-hitting sniper). Weakness: nothing subtle — no stealth, no stun, no mobility trick; loses to out-maneuvering.
- **Hacker**: worst individual stats (frail Bug, no tank), best tools. A Hacker player wins with Worm ambushes, virus snowballs, minefields and DDoS lockdowns — information warfare. Weakness: every trick is countered by radar constructors and spread formations.
- **Network**: highest mobility (fastest unit, the only aircraft, teleport logistics, fastest expansions). Weakness: lowest per-unit power; needs buffer infrastructure and map control; Ports are dead weight in a direct siege.

The in-game difficulty descriptions ("easiest / trickiest / most mobile") are honest.

### 15.4 Balance history (from the changelog — design lessons)

- Spam ratios were repeatedly tuned so *no faction spams faster than another* ("bugs are made about as fast as bits… Use your wits to win, not your spam!").
- The Worm was the perennial boogeyman: nerfed across at least five versions (damage, HP, build time, self-heal removal) until the "ambusher that eats herds but loses to attention" niche stuck.
- The Connection migrated from melee (which launched units into the air) to a long-range beam, then was range/reload-tuned into its "duelist" role.
- The Pointer gained homing specifically to let it hit Connections — artillery vs the mobile teleporter was a deliberate matchup to preserve.
- Spawn protection was added because factory-camping degenerated early games.
- The Byte's "closed armor" was introduced to make it a siege tank you *stun* (DoS) rather than chip down.
- Terminal reload moved 120 s → 90 s when it proved too passive; Obelisk damage/radius was buffed when infection proved underwhelming.
- Flows were made territory-scaling (NRC9) after early-Flow rushes warped small maps.
- Packet HP/ROF/range micro-tuned many times; its ability to shoot through friendlies toggled twice — final state: can, at reduced damage.

---

## 16. Strategy & Tactics Guide

*(Distilled from the tip system, mission briefings and the AI's own playbook.)*

**Macro**
1. Your first Assembler/Trojan/Gateway goes to the nearest datavent. Every vent = permanent army inflow. Deny the enemy's constructors with a patrol or two of spam.
2. Remember the base bonus: each minifac also accelerates your home base (+20%).
3. Track enemy superweapon charges; push right after their SIGTERM/Firewall fires, never right before.
4. Under ONS, you must chew the frontier: kill exposed minifacs to progressively unshield the network toward the base.

**Army composition**
5. Standard mix: a spam screen, 1–2 heavies, 1–2 fire support, one constructor trailing to expand or repair.
6. Keep damaged units at the back — idle regen rotates them back to full for free.
7. Spread against Worms and Pointers (they kill clusters), clump against Bugs and Fairies (they kill scattered targets).

**Faction-specific**
8. **System**: Pointer volleys kill minifacs before they finish (unfinished = 4× damage). Save NX Flags for enemy expansions. Keep one Byte home as a mine-launcher alarm system.
9. **Hacker**: a Worm in the enemy's production heart is worth five Bytes — but turn AutoHold off or order attacks manually, and never splash your own swarm. Stack 3+ DoSes to permastun a Byte. An Obelisk shot into a spam herd literally recruits it.
10. **Network**: dispatch *behind* the enemy line and Enter (re-buffer) when the raid is done — the buffer means your losses are optional. Flows need vent count first: expand, then fly.

**Anti-cheese**
11. Build the radar constructor (Assembler/Trojan/Gateway) early — mines and Worms are invisible without it.
12. A Debug charge clears a mined chokepoint; a Byte simply drives through Bad Block walls.

**Heroes mode**
13. Your hero is a raiding boss, not a main battle unit: kill constructors, snipe unfinished minifacs, retreat to heal (heroes regenerate fast). Arrow-key stutter-step keeps you out of Pointer arcs.

---

## 17. Audiovisual Identity

- **Graphics**: flat-shaded primitive solids in saturated team colors over dark circuit-board terrain — deliberately evoking vector monitors and demoscene visuals. Shots are bright geometric lines/shapes (the Pointer literally fires 3D geometric shapes); explosions are bursts of chunky retro pixels/beams. Distinct silhouettes per unit class (octahedron Byte, segmented Worm, cube-ish Socket).
- **Sound**: bleepy computer SFX per unit (fire, hit, death), obelisk whooshes, SIGTERM air-raid hits, teleports, built-sounds; units answer orders with beeps.
- **Voices**: Eva & Panda voice the per-faction tip narration.
- **Music**: the readme's own recommendation: *"For an enhanced Kernel Panic experience, listen to demoscene or chiptune music"* — the shipped widget quietly plays whatever the player drops into the music folder.
- **Flavor text everywhere**: elimination messages ("followed a null pointer", "NO CARRIER", "got BAH-LEETED!"), tooltips written in Unix-pun style (weapons named `chomp()`, `/.`, `SIGTERM`), the whole game reads like a loving hacker in-joke.

---

## 18. Handicaps & Social Play

For veterans vs. newcomers (all designed to be *invisible* to the receiver's dignity):

- **Lobby bonus**: host grants a team a damage-reduction bonus vs. stronger teams (difference-based, never amplifies; 100 = immunity).
- **`/ons 0`**: turn only *your own* ONS shields off in a shielded game.
- **`/sos N`**: subject only yourself to memory-leak decay.
- **Fair KPAI** as a self-limiting practice opponent.

Team play: shared visual language (team-colored link beams, leak circles, gauges), ally leak-freeing in SoS, unit gifting through the player list, and spectator features (auto-director camera, per-team spectate buttons, take-over for abandoned teams).

---

## 19. Design Evolution Highlights

A brief history, because it explains *why* the design is what it is:

- **KP 1.0–1.5 "Corruption"**: single faction (System), retro graphics; the Hacker side added as "the corruption" — its identity was *trickery vs. force* from day one.
- **KP 2.x "Division Zero"**: the Hacker faction rebuilt around infection, deployable artillery and the Worm ambusher; Byte closed-armor; specials (Terminal, Obelisk) introduced as datavent buildings; ONS becomes a mod option.
- **KP.net**: the **Network faction** — the experiment of replacing production with logistics (Buffer/Dispatch/Enter). Flow speed tied to territory.
- **KP 3.x**: gamemode explosion — SoS, Color Wars, ONS overhaul, the in-game skirmish generator, missions system, HOMF heroes.
- **KP 4.x**: KOTH, Invasion merge, per-unit stat rebalancing language, engine upgrades, Touhou/RPS as secret content.
- Standing design constants across all versions: **no economy, vent-locked factories, self-spamming production, 40k base as commander, and specials as datavent buildings.**

---

## Appendix A: Hotkeys & Console Commands

**Console** (type with `/` prefix; a translation widget drops the `/luarules`):

| Command | Effect |
|---|---|
| `/cw [+-]h m s` | Set/add/subtract the Color Wars timer (`/cw 2.5`, `/cw -0'30`, `/cw + 3 45`, `/cw 1:15:00`) |
| `/ons [team] 0/1` | Refresh / disable / re-enable ONS shields (own team without cheat) |
| `/sos`, `/som [team/*] N` | Set Save-Our-Mem sector size (negative = instadeath; 0 = off) |
| `/dump [name]` | Save game state as a startscript/mission |
| `/kpai 0/1` | KPAI debug output |

**Key bindings**: see §10.5. Hero binds are rebindable (`hero_north/south/east/west`).

## Appendix B: Full Stat Tables

### B.1 Home bases & builders (all factions identical skeleton)

| Unit | HP | Build time | Speed | Notes |
|---|---|---|---|---|
| Kernel / Hole / Carrier / Mag. Circle | 40,000 | — | 0 | WorkerTime 128, idle self-repair, +20%/minifac build boost |
| Socket / Window / Port / Small Circle | 20,000 | 3,840 | 0 | WorkerTime 64; Port fills Buffer instead of building |
| Terminal / Obelisk / Firewall | 15,000 / 15,000 / 20,000 | 3,840 / 8,000 / 3,200 | 0 | Special buildings (§7) |
| Assembler / Trojan / Gateway / Alice | 2,000 | 1,600 (Gateway 2,000) | 2.0 | Radar 500 / 768 / 500 / 768; Gateway armed (100 dmg); Alice 3,000 HP |

### B.2 Combat units

| Unit | Faction | HP | Build | Speed | Weapon | Dmg | Reload | Range | Special |
|---|---|---|---|---|---|---|---|---|---|
| Bit | System | 600 | 240 | 3.0 | laser | 80 | 0.5 s | 256 | idle regen |
| Byte | System | 15,000 | 3,600 | 1.5 | DOOM beam (×4 burst) | 200×4 | 2 s | 512 | closed armor, crushes walls, Launch Mines |
| Pointer | System | 1,000 | 1,920 | 2.0 | homing shell | 4,000 | 4 s | 1,400 | NX Flag |
| Bug | Hacker | 400 | 270 | 3.8 | beam | 130 | 0.5 s | 320 | strafing, fast idle regen, Deploy |
| Exploit | Hacker | 100 | — | 0 | cannon | 130 (200 bldg) | 2.2 s | 1,200 | dmg ↑ with range |
| DoS | Hacker | 1,700 | 1,280 | 2.7 | stun beam | 400 (125 spam) | 0.25 s | 768 | 5 s paralysis |
| Worm | Hacker | 12,000 | 3,200 | 2.1 | bite + splash | 3,200 / 800 | 6 s | 200/210 AoE | cloak, infection, seismic sense |
| Obelisk | Hacker | 15,000 | 8,000 | 0 | infection shell | 200 (1,500 bldg) | 40 s | 2,000 | poison cloud, zombie spawn |
| Packet | Network | 500 | 240 | 4.0 | beam | 130 | 0.75 s | 250 | Enter/Dispatch |
| Connection | Network | 15,000 | 3,300 | 1.5 | particle whip | 4,500 | 4 s | 450 | mobile teleporter |
| Flow | Network | 1,000 | 1,400 | 1.0+ | homing missiles (×2) | 160 | 1.5 s | 350 | flying, speed scales with territory |
| Fairy | Touhou | 300 | 240 | 2.8 | 2× bullets (3-burst) | 40 | 1 s | 300 | — |
| Reimu | Touhou | 5,000 | 1,500 | 2.2 | 5-shot volley | 20 | 0.5 s | 300 | anti-swarm |
| Marisa | Touhou | 1,800 | 1,600 | 2.4 | homing spark | 4,500 | 3 s | 600 | must stop to charge |
| Rock/Paper/Scissors | RPS | 1,000 | 1,000 | 3.0 | beam | 50 (600 prey / 5 predator / 20 kin) | 0.5 s | 256 | auto-build Hands on vents |
| Virus | any (spawned) | 300 | — | 3.5 | infection beam | 100 | 1 s | 220 | infects on kill & death |

### B.3 Utility

| Unit | HP | Build | Effect |
|---|---|---|---|
| Bad Block | 100 | 160 | wall: blocks movement only |
| Logic Bomb | 300 | 120 | 900 dmg blast (3,000 vs Worm), cloaked, cap 64, friendly fire, no chains |
| Debug | — | 600 | one-shot mine/wall clearer |
| SIGTERM bomber | 600 | — | 10,000 dmg / 900 AoE + firestorm; spares factories |

### B.4 Armor classes

`spam`: bit, bug, packet, fairy, exploit · `arty`: pointer, dos, marisa · `heavy`: byte, connection, reimu · `flyer`: flow · `subterranean`: worm · `constructor`: assembler, trojan, gateway, alice · `building`: all bases/minifacs/specials · `mine`: logic bomb, debug · `infectious`: virus · `rocky/papery/irony`: RPS trio

---

*End of document. Compiled from the original game's readme, unit and weapon definitions, interface and gamemode scripts, and mission files — describing what the player sees and feels, not how the code works.*

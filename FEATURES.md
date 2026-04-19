# Kernel Panic — User-Visible Feature List

Everything below is something the player can see, do, or react to in
the running game. Internal plumbing (file formats, CLI flags,
scheduling, allocation, helper components) is omitted. Where a
recognizable technical term applies, it's noted in parens.

Each entry is written so a tester can verify it in the running game
without reading source code — observe the screen, press the input,
read the resulting state.

## 1. Map presentation

- The map's original ground texture covers the terrain, sharp up close
  and progressively softer with distance (mipmapping + anisotropic
  filtering).
- Hills and valleys are visible from the heightmap; terrain can also
  reflect map-script edits, meaning some maps change shape during load
  (Lua heightmap gadgets).
- The sky is uniform black (no skybox).
- The entire map is rendered every frame — no fog-of-war hides any
  region.
- **Geovents ("datavents")** spawn animated streams of rising "0/1"
  digit puffs — neon-green, additive blend, randomly jittered
  (camera-billboarded sprite particles, additive alpha). Each
  geovent emits about 18 puffs per second; each puff lives ~1.7–1.9 s
  while it drifts upward, grows, and fades to nothing.
- Three faction homebases (System=Kernel, Hacker=Hole, Network=
  Stationary Connection) are possible. A toml file read on startup
  contains info about which player is which faction. The homebases are
  then placed on the map's start positions. Right now, there are only
  AI players. Player names are in the toml.
- The game loads one map at startup and never switches maps. To play
  a different map you have to close the game and reopen it.

## 2. Camera

- A bloom glow blossoms on bright surfaces (HDR + bloom post-process).
- The game launches taking the full primary monitor without a window
  border (borderless-fullscreen window).
- The camera is centered on the map at game start.
- Pan, zoom, and rotate the camera at any time; the camera can't leave
  the map's bounds (RTS-style camera with bounds clamping).
- **Pan**: Arrow keys (Up/Down/Left/Right). Pan speed scales with
  zoom distance — panning at max zoom-out covers more ground per second
  than at max zoom-in.
- **Zoom**: mouse wheel (up = zoom in, down = zoom out); zoom distance
  is clamped at minimum and maximum bounds.
- **Orbit**: middle-mouse drag rotates yaw + pitch. Pitch is clamped
  so the camera can't flip through the ground or look straight up.
- **Yaw rotate hotkeys**: `Q` rotates left, `E` rotates right. Both
  rotate as long as the key is held, independent of unit selection.

## 3. Selection & orders

- Left-click a unit to select it; left-click empty terrain to deselect
  (single-pick mouse selection).
- Left-click and drag to box-select multiple units (rubber-band /
  marquee-select).
- Shift + left-click to add or remove units from the current selection
  (additive selection).
- Selection persists across order issuance — issuing a move/attack
  order does not clear the selection.
- Hovered units brighten on hover (material-tint hover highlight); the
  highlight clears as soon as the cursor leaves the unit.
- Selected units brighten more strongly to mark the selection
  (material-tint selection highlight).
- Right-click empty ground → units walk there (move order).
- Right-click an enemy → units engage it (attack-move auto-target
  pickup). Units also keep the target unit selection and chase it, as
  long as it is in their field of view.
- Shift + any command enqueues an additional command after the current
  one rather than replacing it (command queue).
- A dashed line connects each selected unit to its current move
  target and through any queued commands, lifted slightly above the
  terrain so it doesn't fight with the ground (command-line gizmo,
  z-fight offset).
- Selected units get health bars overhead, ramping red → yellow →
  green with HP fraction (world-space health-bar billboards). Bars
  hide when the unit is deselected.
- The hardware cursor changes shape based on what the cursor is over /
  what action is queued (hardware-cursor swap by order context).
- Hotkeys: Stop=S, Fight=F *(reserved)*, D for the context-sensitive
  ability (NX Flag / Infection / Protect / Mine Launch / SIGTERM on
  the corresponding caster; Dispatch on a teleporter; Deploy/Pack Up
  on a Bug or Exploit), R for Packet re-enter / repair on builders
  *(repair reserved)*, T for set target *(reserved)*, X for
  unset-target *(reserved)*, `Ctrl+D` for self-destruct with a
  5-second countdown (cancelled by `Stop`), A for attack-move
  *(reserved)*, P for patrol *(reserved)*.
- A `Stop` order halts the unit immediately and clears the queue.
- On production, the builder unit sets a waypoint for the produced
  unit to move straight out of the factory.

## 4. HUD

- **Bottom-left info panel**: shows the selected unit's name in faction
  color, HP bar (red→yellow→green) and HP text, weapon name, and speed.
  Multi-select switches to a unit-count summary. Hides when nothing is
  selected.
  The panel is attached to the bottom left corner with no right or
  bottom padding.
- **Top-left order palette**: context-sensitive Stop / Fight / other
  ability buttons. Hides when no unit is selected.
- **Mid-left build menu**: faction-colored icons for what the selected
  factory or constructor can produce. When units are queued, the
  number of queued units of that kind is displayed as a small badge
  in the icon's bottom-left. Hides when no factory/constructor is
  selected.
  - When multiple builders are selected, the build pane has tabs on
    top. When only one is selected, there is still a tab saying the
    builder unit name.
- **Top-right minimap**: shows the ground texture as a tiny overview,
  the camera viewport as an outlined rectangle (frustum outline), and
  dots for friendly and enemy units coloured by faction (green /
  red / blue). Gated on the fog-of-war `Spotted` marker — unspotted
  enemies stay off the minimap too.

## 5. Movement

- Units find their way around terrain instead of through it (QTPFS
  pathfinding).
- Each unit obeys its own slope cap: some units refuse cliffs that
  others roll straight up (per-unit `MaxSlope` nav-grid buckets).
- Units in a crowd push each other apart instead of overlapping
  (XZ spatial-hash collision separation).
- A crowd converging on the same waypoint bunches up at its boundary
  instead of jittering on top of one another (waypoint deadlock
  breaker).
- Ground units don't go into the terrain. They can be blown away by
  some weapons; flying units (Flow) hover at a somewhat fixed altitude
  and pass over hills (cruise-altitude hover); the Worm is
  subterranean — it is allowed to sink below ground level while
  cloaked and surfaces to attack.
- Ground units smoothly tilt their pitch and roll to match the slope
  they're standing on (terrain-normal slope tilt).
- Units with high turn rate snap toward a new heading; sluggish ones
  (Pointer, Worm, Dos) visibly pivot before driving forward, and lose
  forward speed during the turn (turn-rate gating with cos(error)
  forward-speed scaling).
- Buildings can't move. A move order on a **factory** sets the
  delivery point (rally point) for newly produced units; queueing
  multiple delivery points works. Mobile constructors (which are units,
  not buildings) actually move and accept normal move/build orders.
- Stunned (DOS-paralyzed) units freeze in place and can't fire
  (paralysis lockdown).
- The Byte traveling visibly reads as a moving pyramid with a
  rectangular base.

## 6. Combat — target picking

- Armed units shoot at the nearest enemy in their weapon range
  (auto-target). Range is per-weapon from FBI data; outside the range,
  no engagement.
- Exploit's BugCannon (anti-swarm artillery) instead picks the
  *farthest* enemy (negative `proximityPriority`).
- "Friendly" = same team **or** same faction; allies don't shoot each
  other (faction + team friend-or-foe).
- Most ground weapons don't chase flying targets — Flow can zip past
  them safely (`NoChaseCategory=VTOL`). The pointer is the exception —
  its projectile is homing (for air and ground units).
- Debug (mineblaster) only shoots mines and walls
  (`OnlyTargetCategory1=VOID` filter).
- Direct-fire weapons need clear line of sight over the terrain; if a
  ridge is in the way they don't fire (terrain LOS check).
- Ballistic weapons (lobbed shots) skip the LOS check and arc over
  ridges (`trajectoryHeight > 0`).
- A unit picks the best available target every frame — if its current
  target dies, leaves range, or hides behind terrain, the next frame's
  shot is aimed at whoever's nearest instead. No target lock-on.

## 7. Combat — shots fired

- Visible aim: turreted units rotate their body / barrel to face their
  target before firing; the gun keeps tracking even during cooldown
  (host-driven aim, gunbase pitch).
- Shots can visibly miss small targets when scattered outside the
  target's hit volume (per-shot `sprayangle` perturbation, volumetric
  hit radius).
- Lobbed shots travel a visible parabolic path (ballistic arc).
- Burst-fire weapons fire a small flurry at fixed spacing; all shots
  land on the same spot regardless of target motion (`burst > 1`,
  frozen aim-point).
- Units have various weapon reload times (per-unit attack cooldown).
  Reload is observable as the visible pause between shots from the
  same unit.
- Each shot produces a muzzle flash sprite at the unit's resolved
  barrel piece (muzzle-piece-anchored CEG-style flash).
- There are different weapon classes. They aren't always laser-beam
  like, some look different. The Packet has a green laser, the Bit
  fires a `>>>>` arrow, the Bug fires a red blob, etc.
- Some beams are atlased with a glyph texture (beam-texture atlas:
  `arrow`, `dosray`, `bytemegabeam`).
- Burst-spray beam weapons (PacketBeam) render as a fan of thin
  segments instead of a single beam (`beamburst`).
- Projectile weapons render as flying spheres or small cubes that
  travel from origin to target (projectile visuals). Pointer has a
  red trail.
- Melee weapons (Wormbite) flash with a short orange burst at the
  bite point (melee-flash visual).
- Each impact pops a colored burst at the hit point, scaled to the
  weapon's AoE (impact burst, AoE-scaled).
- Each unit fires one weapon at a time — there's no per-unit
  multi-weapon stacking.

## 8. Combat — damage & defense

- Armor classes matter: Logic Bombs shred Worms (3000 dmg vs Worm
  armor); Minekiller one-shots mines; ordinary weapons see normal
  damage tables (per-armor-class damage table, RPS-style multipliers).
- Some buildings are deliberately fragile: Socket / Window / Port /
  Firewall take 4× damage (FBI `DamageModifier`).
- BugCannon's per-shot damage scales **up** with attacker → target
  distance — point-blank shots barely scratch, hits at the weapon's
  reference range deal full damage (dynamic damage by distance,
  `dynDamageInverted=1`).
- AoE weapons splash to nearby units with linear falloff to the edge
  (`area_of_effect` + `edge_effectiveness`). `avoidfriendly` weapons
  skip allies in the splash; `noselfdamage` weapons skip the attacker
  itself.
- Shields on secondary factories, Firewall, Terminal, Obelisk,
  Kernel, and Hole soak damage first (shield absorption pass). Finite
  shields regenerate over time when not being hit (`shieldPowerRegen`);
  infinite shields (Kernel, Hole) never break (`shieldpower=0` →
  unlimited).
- A Firewall-protected unit takes only a fraction of incoming damage
  and reflects the rest back at the attacker (Firewall protection +
  damage reflection).
- DOS paralysis: paralyzer hits don't deplete HP — they fill a stun
  meter. When the meter passes max-HP the target freezes and stops
  firing for `paralyzeTime`. The meter bleeds off when the target
  stops getting hit, so a few stray DOS pings won't add up to a
  lockdown later (`paralyzer=1` weapons, `StunCharge` accumulator,
  exponential decay).
- Bits fall over from one DOS hit; Bytes need many.
- A unit idle long enough (no move order, no aim target) starts
  regenerating HP. Any incoming damage resets the timer (FBI
  `IdleAutoHeal` + `IdleTime`). Visible as the unit's health-bar fill
  creeping back up while the unit stands still.
- Damage taken visibly drops the world-space HP bar and shifts its
  color toward red.

## 9. Combat — death

- A unit at zero HP plays its `Killed()` animation — pieces scatter,
  fall, or hide as the script directs (COB `Killed()` callback).
- Big units have a death AoE: their FBI `ExplodeAs` weapon
  (RetroDeath / RetroDeathBig / VirusDeath …) fires at their corpse,
  damaging anything nearby (FBI `ExplodeAs` self-hit AoE).
- Pieces flagged for `Explode` in the death script disappear and
  spawn a faction-colored particle burst at that piece's world
  position (per-piece explosion particles): green for System, red for
  Hacker, blue for Network.
- After the animation finishes (or after a 2 s timeout) the corpse
  despawns (death-anim timeout). The unit also disappears from the
  minimap and from the multi-select count at this moment.

## 10. Production

- Each factory has a build queue (FIFO). The queue is unbounded —
  stack as many orders as you like.
- The player left-clicks an icon in the build menu to add a unit to
  the queue (or chains placement orders for mobile constructors).
- The factory builds the queue in order. There is no progress bar.
  The only progress indicator is the "health bar" of the unit in the
  factory, which goes up to 100%.
- The Kernel (System homebase) builds faster as the team controls more
  small buildings — Sockets, Windows, Ports, Terminals, Obelisks,
  Firewalls (Kernel Boost: +0.2× per small building). Visible as the
  in-progress unit's HP bar filling faster.
- **Two-phase emergence**:
  - **System units (Kernel-built)** rise out of the ground at the
    factory's spawn pad, easing up to the surface (Rise emerge
    style — eased Y-lerp from underground to ground level).
  - **Hacker / Network units (Hole / Connection / Window / Port)**
    materialize at-surface with an alpha fade-in (Fade emerge style —
    per-unit material clones with alpha ramped 0→1).
- While producing, factories emit visible build rays from per-faction
  emitter pieces (multi-emitter build-laser visuals):
  - Kernel: 4 rays from its 4 pillar tips.
  - Socket: 2 rays from orbiting blasers.
  - Hole / Window / Port: a single nano-emitter.
  - Connection: a placeholder ray above the structure (Connection's
    upstream model has no emitter pieces).
- A **mobile constructor** building a structure shows a different
  ray pattern: one ray from the constructor to the building plus two
  vertical rays about 40 elmos high that rotate around the building
  in progress.
- Connection's body piece lifts up while producing, drops back down
  when idle (host-driven hatch animation).

## 11. Mobile constructors

- Selecting a constructor and left-clicking a building in the build
  menu enters **placement mode**.
- A translucent ghost of the chosen building follows the cursor
  (placement-ghost preview entity).
- The ghost snaps to the nearest unclaimed datavent within ~48 elmos
  (snap-to-feature placement).
- Ghost tints **green** on a valid datavent, **red** otherwise
  (validity tint).
- Left-click on a green ghost commits the order; the constructor walks
  to the datavent and erects the building (visible build ray from the
  constructor while building) (`PendingBuild → Constructing → spawn`
  pipeline).
- While constructing, the builder is pinned facing the build site —
  the beam leaves its muzzle piece forward, never out of its side or
  back.
- Shift + left-click queues additional placements (placement queue).
- Right-click / Escape / picking a different unit cancels (placement
  cancel).
- Once a builder commits to a vent, no second constructor can stack
  on the same vent (`VentClaim` exclusivity).
- On issuing a build order, the selection of the builder is not lost.
  If shift was held during placement, the next placement ghost
  immediately appears under the cursor for chained orders.

## 12. Pointer (system artillery unit)

- Idle Pointers automatically open up (deploy state machine
  Closed → Opening → Open).
- Issuing a move order makes them close before they can drive
  (auto-Close on move order).
- A Pointer can't fire while not fully open (deploy-gate fire lock).
- A Pointer waits until its body has rotated to face the target before
  firing — visible as a noticeable "swing then shoot" rhythm
  (heading-tolerance fire gate).
- The pointer projectile tracks its target. It can target Flows.

## 13. Network — packet teleporters

- Each Port slowly ticks a shared per-team **packet buffer** (~ a
  packet every 5.5 s) (`PacketBuffer` resource, per-Port timer). Means
  more Ports = more buffer increment.
- The player can `Dispatch` packets from any teleporter (Port or
  Connection); a ring of Packet units appears around the teleporter
  and they each take a brief stun (~6 s) before they can re-enter
  (Dispatch ability, spawn-stun).
- A plain Dispatch sends one batch of up to 12 Packets, then ends.
- An ALT-modified Dispatch keeps re-firing the batch (up to 12 per
  frame) until the team's Packet Buffer is empty — drains the whole
  Buffer in one ramp.
- Packets can `Enter` a friendly spawn point and top the buffer back
  up (Enter ability, `ENTER_DISTANCE` proximity check). This order
  can be enqueued.

## 14. Network — Flow speed scaling

- Flow is the only flying unit (hovers above terrain, ignores ground
  collision) (VTOL / cruise-alt hover).
- Flow's speed visibly scales with the team's small-building count:
  more Sockets / Ports / Firewalls / etc. → faster Flow
  (`SpeedBoost` per small building).

## 15. Hacker — Bug ↔ Exploit morph

- A Bug can morph into an Exploit and back. The unit re-spawns in
  place as the new kind (mutual-morph pair, in-place re-spawn). An
  Exploit cannot move.
- Hotkey: `D`. Also surfaced in the order palette as a **Deploy** /
  **Pack Up** button while a Bug or Exploit is selected. The Bug ↔
  Exploit selection never overlaps with the command-fire ability set
  (Pointer / Obelisk / Firewall / Byte / Terminal) or the teleporter
  set (Port / Connection), so `D` resolves unambiguously per
  selection.

## 16. Infection chain

- Worm / Virus / Obelisk weapons (`VirusBeam`, `VirusDeath`,
  `Wormsplash`, `Infection`) tag victims as `Infected` for a
  per-weapon duration (per-weapon infection window).
- An infected unit that dies spawns a Virus on the attacker's team at
  the death location (`VirusSpawnQueue` drain).
- Virus's own death AoE (`VirusDeath`) is itself an infection weapon,
  so outbreaks chain (chain infection via `ExplodeAs`).
- A spray-angle miss on the primary target skips the infection
  (infection gated on landed primary hit).
- An existing Virus can't be re-infected.

## 17. Cloak & detection

- Logic Bombs and Worms spawn cloaked — invisible until revealed
  (`Init_Cloaked=1`).
- Friendly cloaked units are always visible to the controlling
  player, rendered semi-transparent / faded so the player can tell
  they're cloaked vs. uncloaked.
- Detector units (Assembler / Trojan / Gateway) reveal cloaked
  enemies inside their detection range (FBI `RadarDistance` detector).

## 18. Mines & walls

- **Logic Bomb**: cloaked mine that triggers when an enemy enters its
  proximity radius — kamikazes with a big AoE explosion (kamikaze
  proximity trigger, FBI `kamikazeDistance`).
- **Bad Block**: cheap destructible wall. Blocks small units' movement;
  does **not** block shots. Cleared by Debug or crushed by a Byte /
  Connection.
- **Debug** (mineblaster): one-shot weapon that only targets mines and
  walls (Minekiller weapon, `OnlyTargetCategory1=VOID`).

## 19. Command-fire abilities (D hotkey)

- **NX Flag** (Pointer): area ability — sets a wide circle ablaze for
  ~1 minute, dealing constant damage to anything inside. Has a
  multi-second cooldown after firing.
- **Infection** gas (Obelisk): an area-denial gas projectile that
  covers a circle in a poison cloud (~1000 dmg over its duration);
  any enemy that dies in the cloud turns into a Virus. Has a ~40 s
  cooldown; visible "pink fire on top" on the Obelisk while the
  weapon is ready.
- **Protect** (Firewall): casts a 20-second damage-halving bubble on
  a friendly target at the click position; half of incoming damage
  is reflected back at the attacker.
- **Mine Launch** (Byte): lobs 5 Logic Bombs in a spread toward the
  target point at the cost of HP (self-damage).
- **SIGTERM** (Terminal): calls a nuclear bomber that drops on the
  target point for ~10,000 damage over a wide area, plus a brief
  denial zone. ~90 s cooldown; no defense.

## 20. Unit roster

### System (green)

| Unit | Role |
| --- | --- |
| **Kernel** | Homebase. Builds all System mobile units. Rapid auto-heal, lots of health |
| **Assembler** | Mobile constructor (builds Sockets, Terminals, plus shared Bad Block / Logic Bomb / Debug); slow, fragile; detector for mines + cloaked units; cannot assist-build |
| **Bit** | Basic spam unit, cheap, fast, fragile; SPARCling laser |
| **Byte** | Heavy attacker, slow, lots of HP, powerful gun; more armored when "closed"; can plow through Bad Blocks. While traveling the model reads as a moving pyramid with a rectangular base. Auto-heals at idle. Ability: mine launcher (lobs 5 Logic Bombs at the cost of HP) |
| **Pointer** | Slow, frail artillery (Open → fire); homing projectile that tracks ground *and* air targets; ability: NX Flag (sets a wide area ablaze for ~1 minute, constant damage to anything inside) |
| **Socket** | On-datavent secondary factory; builds only Bits, slower than Kernel; auto-heals; decent HP |
| **Terminal** | On-datavent special building. Calls a nuclear bomber every ~90 s for ~10000 dmg over a wide area — destroys everything except factories. Bomber can strike anywhere on the map; no defense. Less effective vs Kernel / Hole |
| **Debug** | One-shot mine/wall clearer (Minekiller weapon) |

### Hacker (red)

| Unit | Role |
| --- | --- |
| **Hole** ("Security Hole") | Homebase. The Hacker Kernel-equivalent |
| **Trojan** | Mobile constructor (builds Windows, Obelisks, plus shared Bad Block / Logic Bomb / Debug); detector for mines + cloaked units; same loadout as Assembler with Hacker counterparts |
| **Bug** | Hacker spam unit. Weaker than the Bit, more range. Can sense movement outside its LOS but can't shoot through friendly units or behind itself. Morphs into Exploit (deploy / bombard command) |
| **Exploit** | Deployed / morphed form of Bug. Stationary anti-swarm artillery (BugCannon: prefers far targets, *more* damage at range). Even frailer than the Bug |
| **Worm** | Cloaked stealth assassin. Surfaces to fire a large-AoE bite that turns slain units into Viruses. Splash avoids friendlies (`avoidfriendly=1`). Does practically no damage vs other Worms or Viruses. By default holds fire while cloaked (auto-attack only on manual order; toggle with autohold) |
| **Virus** | Cannot be built. Spawned when units killed by Virus / Worm / Obelisk weapons die. Crappy little swarm unit |
| **Dos** ("Denial of Service") | Stun-beam unit (DOS_Beam). Bigger targets need longer to stun → use multiple in parallel (a "DDoS"). Target unfreezes quickly once the DOS stops firing. Faster than the Pointer but leaves a long visible particle trail |
| **Window** | On-datavent secondary factory; builds only Bugs |
| **Obelisk** | On-datavent special building. Stationary infection artillery (Infection shot every ~40 s; visible pink fire on top when ready). Covers a large area in poison cloud (~1000 dmg) and turns enemies that die in the cloud into Viruses. Short range; best against herds of Bits / Bugs |

### Network (blue)

The Network faction is built around mobility: small factories (Ports)
don't produce units openly — instead they tick a virtual counter (the
**Buffer**). Packets in the Buffer can be materialised at any
teleporter (Port or Connection) with the Dispatch command, and can be
moved back into the Buffer by entering a teleporter.

| Unit | Role |
| --- | --- |
| **Connection** (as homebase) | Homebase / main factory. Substitutes for upstream's "Carrier" base building; in our build it's the same model that doubles as the mobile teleporter. Visible body-piece "hatch" lifts up while producing |
| **Connection** (mobile) | Mobile teleporter — Dispatch + Enter just like a Port. Decent armor and an arc beam with good range and high single-target damage. Wins most 1v1 against large units, but folds to Pointer fire and is bad against swarms |
| **Gateway** | Lightly-armed mobile constructor (builds Ports, Firewalls, plus shared Bad Block / Logic Bomb / Debug); detector for mines + cloaked units |
| **Port** | On-datavent production building. Ticks the team's Packet Buffer. Dispatch sends up to 12 Packets at once; ALT-modified Dispatch drains the Buffer in 12-per-frame batches |
| **Packet** | Basic light spam unit. Weaker than Bit / Bug in combat, but much faster. Spawned by Dispatch and can re-Enter the buffer |
| **Signal** | Air-strike caller (currently a stub) |
| **Flow** | Air unit. Slow but crosses any terrain. Built to attack light targets (spam units, fire support); highly vulnerable to return fire — Pointers, DOS units and Connections shred Flows |
| **Firewall** | Special building. Casts a 20-second protective bubble on friendly units in a target radius — halves incoming damage and reflects the other half back at the attacker |

### Shared (any side via constructor)

| Unit | Role |
| --- | --- |
| **Bad Block** | Tiny wall, built by Assembler / Trojan / Gateway. Blocks small units' movement; does **not** block shots. Cleared by Debug or crushed by a Byte / Connection |
| **Logic Bomb** | Cloaked kamikaze mine, built by any constructor; also launchable by Bytes via the mine-launcher ability. One-shots Bits / Bugs, decent damage radius, does **not** chain-explode but does hurt your own units. Cap of ~32 per team |
| **Debug** | One-shot mine/wall clearer, built by any constructor |

## 21. Animation

- COB scripts drive each unit's per-piece motion: barrels rotate, gun
  arms extend, hatches open, halves split apart and rejoin, etc.
  (COB virtual machine, per-piece interpolated turn / move / spin).
- `Open()` / `Close()` cycles play visibly on Pointers when they
  deploy / pack up (deploy-state COB callbacks).
- `StartMoving()` / `StopMoving()` hooks animate per-unit motion
  flourishes (movement-state COB callbacks).
- `Activate()` / `Deactivate()` hooks animate factory production
  start / stop, e.g. Connection's hatch (production-state COB
  callbacks).
- `AimWeapon1()` / `FireWeapon1()` hooks animate aim and recoil per
  shot (per-shot COB callbacks).
- Build sparkles ("nanoframe pixels") drift up at the build target,
  face the camera, and shrink to nothing (camera-billboarded sprite
  particles, oldskool_build CEG approximation).
- Faction colors are consistent across health bars, particles, and
  unit highlights: System = green, Hacker = red, Network = blue.

## 22. Game state

- The player's team is `0` by default. Defeat = the player team has
  no homebases left. Victory = every other team has no homebases left.
- On defeat, a centered red `DEFEAT` headline (~120 pt) appears at
  ~35 % from the top of the screen. On victory, the same layout but
  green and `VICTORY`.
- Once the game-over screen is shown, all gameplay systems stop
  ticking (units, animations, AI, combat). The camera still works.
- A sandbox map with no homebases at all skips the game-over check
  entirely (no auto-defeat on the first frame).

## 23. AI opponent

- Every non-player team runs an AI brain that ticks once per second
  (per-team AI tick at 1 Hz).
- The AI keeps its homebase build queue topped up with combat units,
  inserting one constructor every few combat units (build phase).
- Idle constructors get sent to the nearest unclaimed datavent to put
  down the appropriate secondary factory (expand phase).
- If an enemy is within ~700 elmos of a friendly homebase the AI
  recalls idle combat units to defend the threatened base (defend
  phase).
- Once it has built up an army of ~8+ idle units, the AI sends them
  at the nearest enemy homebase (attack phase).

## 24. Fog of war (currently neutered)

- The fog system is wired up but every spawn marks the unit visible,
  so in-game everything is always visible regardless of who controls
  the team (`Spotted` blanket-applied at spawn).
- When re-enabled: cloaked units stay hidden until a detector is in
  range; non-cloaked enemy units stay hidden until any player-team
  unit comes within their FBI `SightDistance`, then visible
  permanently (memory-only "spotted-once" fog model).

## 25. Cursor sprites

The hardware cursor swaps between several variants depending on
context. Most variants animate over their frames at a fixed rate
(~30 fps), one variant is static.

| Variant | Frames | When shown |
| --- | --- | --- |
| **Normal** | 1 (static) | Default cursor over empty terrain or the HUD |
| **Move** | 10 | Hovering empty ground while a movable unit is selected |
| **Attack** | 9 | Hovering an enemy unit while an armed unit is selected |
| **Repair** | 9 | Hovering a friendly unit while a constructor is selected |
| **Patrol**, **Defend**, **Reclamate**, **Revive**, **Capture**, **Pickup**, **Unload** | 8–20 | Reserved sprites for Spring orders not yet wired up |

## 26. Window & process

- The game launches in borderless fullscreen on the primary monitor
  at the desktop resolution.
- Alt-Tab returns to the desktop without crashing the game.
- Closing the window (clicking the OS close affordance, Alt+F4)
  quits the game cleanly.
- One process = one game session. There is no main menu, no
  in-game restart, and no save/load.

## 27. Audio

- None.

## 28. Multiplayer / persistence

- None — single-process, single-session, local only.

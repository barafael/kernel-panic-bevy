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
- **Hex Farm 8** is generated anew every match, like the original's
  Lua gadget: a random lattice of hexagonal towers (random tower size,
  spacing, bridge width/slope, lattice angle and boundary shape —
  circle, hexagon, star, rectangle, losange or triangle) joined by
  bridges, standing in a black void (no ground is drawn). Towers wear
  the "Digital" skin: hex-tiled tops (a green datavent emblem on vent
  towers) and red-circuit walls fading to black; one match in five the
  towers are greyscale, tinted with the colour of the side whose big
  building stands on them. Each side starts on its own tower; only the
  starting towers exist at first.
  - Standing on a tower makes the neighbouring towers rise out of the
    void over 10 s (top sliding up, fading in); a rising tower nobody
    stands next to sinks back. Once two neighbouring towers stand, the
    bridge between them swings up over 5 s. Datavents appear on risen
    vent towers.
  - Every explosion wears down the tower or bridge it hits; after
    1000× a typical unit's health, it sinks (a tower takes its bridges
    down with it) and its datavent vanishes. Sunk polygons are rebuilt
    by building or producing units on a neighbouring tower (1000× a
    typical build time of work). A "NN%" label over the polygon shows
    the damage (red→yellow) or rebuild progress (cyan→green).
  - Ground units can't path or be pushed into the void; any that end
    up there (the ground sank under them) are pushed back onto a nearby
    tower or bridge, or self-destruct ("fell into the void").
  - The minimap shows the standing towers, bridges and vents.
  - Flows fly a level bridged over the pits (the map's own radial
    flight profile) instead of diving into the void between towers;
    where towers later rise or sink, the flight surface re-forms over
    the following seconds.
- The entire map is rendered every frame — no fog-of-war hides any
  region.
- **Geovents ("datavents")** spawn animated streams of rising "0/1"
  digit puffs — neon-green, additive blend, randomly jittered
  (camera-billboarded sprite particles, additive alpha). Each
  geovent emits about 18 puffs per second; each puff lives ~1.7–1.9 s
  while it drifts upward, grows, and fades to nothing.
- Three faction homebases (System=Kernel, Hacker=Hole, Network=
  Carrier) are possible. The skirmish menu picks your faction, the
  enemy faction, the grouping (Duel = 1 AI, Outgunned = 2–5 allied AI
  seats) and the difficulty. Seat *i* gets its side's homebase on the
  map's *i*-th start position (upstream `game_spawn.lua`); seats past
  the map's start positions are spread on a ring around the centre.
- Friend-or-foe is by ally team only, so mirror matches (System vs
  System) fight normally.
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
- Right-click empty ground → units walk there (move order); right-drag
  draws a line the selection lines up along (formation move).
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
- Hotkeys (every one goes through the same command activation as a
  click on the matching command-panel button, §4): Stop=S, Attack=A,
  Move=M, Fight=F, Patrol=P, Guard=G, AutoHold=H (Worm), Enter=R
  (Packet), Undeploy=U (Exploit), `Ctrl+D` self-destruct with a
  5-second countdown (cancelled by `Stop`), set target=T and
  unset target=X (remake extras: no panel button, as in KP), `,` / `.`
  previous / next panel page, and keypad 2/4/6/8 arm the selected
  constructor's minifac (Socket / Window / Port, `kp_hotkeys.lua`).
  `D` fires the context-sensitive ability at the cursor at once (NX
  Flag / Infection / Protect / Mine Launch / SIGTERM on the caster,
  Dispatch on a teleporter, Deploy / Pack Up on a Bug or Exploit).
  Pressing an armed command's hotkey again disarms it; Escape or a
  right-click anywhere disarms whatever command is armed, and that
  right-click issues no order.
- A `Stop` order halts the unit immediately and clears the queue.
- On production, the builder unit sets a waypoint for the produced
  unit to move straight out of the factory.

## 4. HUD

- **Command panel** (Spring's engine control panel as configured by
  KP's `KP_CtrlPanel.txt`): a 3×9 grid glued to the left edge, from
  14.7 % to 68.7 % up the screen; each button is 6 % of the window
  width by 6 % of its height, so the panel scales with the window. No
  background frame. It lists every order the selection supports in
  Spring's order — Stop, Attack, the state buttons (Repeat on
  factories, AutoHold on Worms), Move / Patrol / Fight / Guard for
  mobile units, the KP gadget commands (NX Flag, Deploy, Undeploy,
  Launch Mines, SIGTERM, Firewall, Dispatch, Enter; the Obelisk's
  Infection sits on its "Attack" button) — then the build options.
  With a mixed selection each command appears once (first unit wins)
  and all build options come last. It stays up as long as the unit is
  selected; clicking a button never closes it.
  - Build options and NX Flag / SIGTERM / Launch Mines show a picture
    stretched over the whole button; other commands show their name
    in white, scaled to fill the button, in a faint frame. State
    buttons show their current option ("Repeat on") above option LEDs
    (red = off, green = on).
  - A factory's queued count of each unit is printed in the bottom-left
    corner of its button.
  - Hovered button: blue wash + white outline (red outline while a
    mouse button is down); the armed command: red wash + yellow
    outline; disabled commands are darkened — Logic Bomb at the team
    cap, SIGTERM / Firewall while recharging (the button then reads the
    seconds left, e.g. "42s").
  - A command runs when the mouse is *released* over the button it was
    pressed on (press on one button, release on another: nothing).
    Clicks on the panel never reach the map (no deselect, no box
    select, no stray order); clicks on empty slots pass through.
  - Instant commands (Stop, Deploy, Undeploy, Enter) run at once;
    state buttons step to their next option for the whole selection;
    targeted commands and build options *arm* — the next map click
    supplies the target (§11).
  - Factory build options: left click queues one, right click removes
    one (newest first), Shift ×5, Ctrl ×20, Shift+Ctrl ×100; Alt puts
    the new orders at the front of the queue (behind a unit already in
    progress) or, with a right click, removes the oldest.
  - More than 25 commands page: the last two slots become
    previous / next arrows (`,` / `.`). A new selection starts on its
    first page.
  - Not listed because the remake doesn't simulate them yet: Wait,
    Repeat on mobile units, builder Repair / Reclaim, factory rally
    orders, the Bug's Bombard. Fire state / Move state / Cloak are
    hidden as in KP (`hide_commands.lua`).
- **Build bar** (KP's `kp_buildbar.lua`): a row of icons along the top
  edge, right-aligned — Terminals and Firewalls first, then the
  homebase. Icons are `55 + (width − 800) / 38` px wide, ¾ as tall,
  with a green border.
  - The homebase icon shows the unit it is building with a clockwise
    progress pie (else the homebase itself); below it the next queued
    units, up to three, with their counts.
  - A Terminal / Firewall shows its recharge pie and the seconds left,
    or "Ready!".
  - Hovering the homebase opens its build options below it (they stay
    open until a click elsewhere); clicking one edits that homebase's
    queue with the command panel's rules. Clicking an icon selects
    that unit (replacing selected units of the same type); on a
    Terminal / Firewall it also arms SIGTERM / Firewall.
- **Tooltip box** (KP's `kp_tooltip.lua` over `bitmaps/tooltipbg.png`),
  always in the bottom-left corner, sized to its text (font
  `max(8, 4 + height / 100)` px):
  - over a command button: its tooltip with the action name in green,
    then "Hotkeys: …" in orange;
  - over a build option: "N units selected", then the unit's name and
    description, build time, health and speed;
  - otherwise: "One unit selected" / "N units selected" and the unit
    under the cursor (or the last selected unit): name, description,
    health, speed, and a friendly teleporter's buffered packets.
- **Top-left minimap**: shows the ground texture as a tiny overview,
  the camera viewport as an outlined rectangle (frustum outline), and
  dots for friendly and enemy units coloured by faction (green /
  red / blue). Gated on the fog-of-war `Spotted` marker — unspotted
  enemies stay off the minimap too.

## 5. Movement

Ground movement is a port of Spring's `CGroundMoveType` with KP's
modrules (QTPFS, `allowUnitCollisionOverlap=1`,
`allowPushingEnemyUnits=1`) and MOVEINFO move classes.

- Units find their way around terrain and buildings instead of through
  them (grid pathfinding with QTPFS's structure blocking). Buildings
  block the squares of their yardmap at Spring scale — a homebase's
  yard cross and a Socket's middle lane stay walkable, Terminals /
  Firewalls / Obelisks / Ports are solid — and paths keep a unit's
  footprint clear of them (Bits one square, Bytes/Pointers/Assemblers
  three). A path that a new building (or Hex Farm terrain) cuts is
  re-searched.
- Each unit obeys its own slope cap: some units refuse cliffs that
  others roll straight up (per-unit `MaxSlope` nav-grid buckets).
  Climbing slows a unit down (a 30° ramp to ~0.43× speed); going
  downhill doesn't (directional slope speed mod).
- Units get up to speed in a few frames and stop within a few elmos
  (FBI `Acceleration` / `BrakeRate` per sim frame²). With more orders
  shift-queued they don't brake between legs: each leg ends ~32–45
  elmos early and the next one starts at full speed.
- Turning costs speed but never stops a unit: the sharper the turn
  still to make, the slower it drives (down to 10% of top speed), so
  units arc through corners; heavy units (Byte, Pointer, Worm) swing
  round slowly, light ones quickly (FBI `TurnRate` per frame, with
  turn inertia). Near a waypoint a unit cuts the corner to the next
  one when it can see it.
- A new order or a chase repath doesn't make a unit stand still: it
  keeps following its old path until the new one is ready.
- Right-clicking one spot sends the whole selection to that spot; the
  group spreads out around it by pushing, and units that bump into
  already-arrived units on the goal consider themselves arrived.
  Drawing a line (right-drag) lines the units up along it, each taking
  the nearest free spot without paths crossing.
- Units in a crowd may overlap a little but push each other apart,
  heavier/faster units shoving lighter/slower ones aside; idle units
  are pushed out of the way. Units walking at each other steer apart
  early (obstacle avoidance). Units slide round buildings instead of
  grinding into them.
- A unit that can't get anywhere repaths, and after ~8 s of no
  progress gives the order up. An unreachable goal is walked to the
  closest reachable point and then abandoned.
- Heavy units (Byte, Connection) drive straight over Bad Blocks and
  crush them; lighter units have to go round.
- Ground units stay on the drawn terrain surface (also when idle and
  when Hex Farm terrain sinks); flying units (Flow) cruise at their
  altitude above a smoothed version of the terrain (Spring's smooth
  height mesh), so they glide over hills and pits instead of bobbing
  along every bump, climbing early for ridges ahead; the Worm burrows
  its head below ground while cloaked and surfaces to attack.
- Ground units tilt to the slope they're on, moving or idle; buildings
  stand upright.
- Everything the 30 Hz simulation moves (units, their animated parts,
  projectiles, particles) is drawn smoothly between sim frames, at any
  frame rate (render interpolation).
- Buildings can't move. A move order on a **factory** sets the
  delivery point (rally point) for newly produced units; queueing
  multiple delivery points works. Mobile constructors (which are units,
  not buildings) actually move and accept normal move/build orders.
- Stunned (DOS-paralyzed) units coast to a stop and can't fire
  (paralysis lockdown).
- The Byte traveling visibly reads as a moving pyramid with a
  rectangular base.

## 6. Combat — target picking

- Armed units shoot at the nearest enemy in their weapon range
  (auto-target). Range is per-weapon from FBI data; outside the range,
  no engagement.
- Exploit's BugCannon (anti-swarm artillery) instead picks the
  *farthest* enemy (negative `proximityPriority`).
- "Friendly" = same ally team; allies don't shoot each
  other (team friend-or-foe; factions don't matter).
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
- The player clicks a unit's button in the command panel (or the build
  bar's homebase menu) to queue it: left +1, right −1, Shift ×5, Ctrl
  ×20, Alt at the front (§4). The queued count shows on the button.
- A factory's **Repeat** state button (LEDs) toggles repeat: finished
  units go back to the end of the queue.
- **Minifac autospam** (upstream `kp_autospam.lua`, applied to every
  team): a finished Socket starts building Bits and a finished Window
  Bugs, on repeat — each finished unit goes back to the end of the
  queue, so the factory keeps spamming forever. Orders the player adds
  join the cycle. A minifac does not produce while it is still being
  built. Ports don't produce; they fill the packet buffer (§13).
- The factory builds the queue in order. There is no progress bar.
  The only progress indicator is the "health bar" of the unit in the
  factory, which goes up to 100%.
- The Kernel (System homebase) builds faster as the team controls more
  small buildings — Sockets, Windows, Ports, Terminals, Obelisks,
  Firewalls (Kernel Boost: +0.2× per small building). Visible as the
  in-progress unit's HP bar filling faster. Only *finished* buildings
  count (upstream `UnitFinished`); one still under construction — or
  destroyed before completion — adds nothing.
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

- Selecting a constructor lists its buildings in the command panel.
  Clicking one (or keypad 2/4/6/8 for the minifac) *arms* placement —
  the button turns red with a yellow outline; nothing is placed yet.
- A translucent ghost of the chosen building follows the cursor
  (placement-ghost preview entity), tinted **green** on a valid site
  and **red** otherwise.
- Small buildings (Socket / Window / Port / Terminal / Obelisk /
  Firewall — KP's `SmallBuilding` set, whose yardmaps need a
  geothermal vent) snap to the nearest unclaimed datavent within ~64
  elmos; while one is armed every datavent blinks with a green square
  (`kp_geoshighlight.lua`, 0.4 s on / 0.4 s off). Bad Blocks, Logic
  Bombs and Debuggers go anywhere on Spring's 16-elmo build grid that
  isn't too steep or occupied by a building.
- The *second* click places it: the building is ordered on release of
  the map click; the constructor walks there and erects it (visible
  build ray from the constructor while building) (`PendingBuild →
  Constructing → spawn` pipeline). The constructor stays selected and
  the panel stays up.
- A plain click places one building and disarms. Shift+click queues it
  and keeps the command armed for the next one; once Shift is let go
  the next click only disarms (Spring's `needShift`).
- Shift+drag lays a row of buildings from the press point to the
  release point, spaced by the footprint (Ctrl: axis-aligned; Alt:
  filled rectangle; Alt+Ctrl: its outline) — all queued. For datavent
  buildings every sample snaps to a vent of its own.
- A click on an invalid site places nothing and keeps the command
  armed. Right-click / Escape / deselecting the constructor cancels.
- While constructing, the builder is pinned facing the build site —
  the beam leaves its muzzle piece forward, never out of its side or
  back.
- Once a builder commits to a vent, no second constructor can stack
  on the same vent (`VentClaim` exclusivity).

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

- Flow is the only flying unit, flown like Spring's hovering aircraft:
  it lifts off after being built, accelerates / brakes / turns at its
  FBI rates (it can side-slip while its nose catches up) and banks into
  turns; a move order ends within 64 elmos of the target, after which
  it hovers in place (it never lands). It holds its cruise altitude
  above the smoothed terrain; Flows that bump into each other are
  pushed apart, and one flying straight at another changes height.
- Flow's speed visibly scales with the team's small-building count:
  more Sockets / Ports / Firewalls / etc. → faster Flow
  (`SpeedBoost` per small building). Same finished-buildings-only
  count as Kernel Boost.

## 15. Hacker — Bug ↔ Exploit morph

- A Bug can morph into an Exploit and back. The unit re-spawns in
  place as the new kind (mutual-morph pair, in-place re-spawn). An
  Exploit cannot move.
- Hotkey: `D` (and `U` to undeploy). Also on the command panel as the
  **Deploy** (Bug) / **Undeploy** (Exploit) button. The Bug ↔
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
  proximity trigger, FBI `kamikazeDistance`). Capped at 64 per team
  (`logic_bomb.fbi UnitRestricted=64`, nanoframes included): at the cap
  the build icon greys out, constructors refuse to start one, and
  launched mines beyond it are simply not created (`Launcher.lua`).
- **Bad Block**: cheap destructible wall. Blocks small units' movement;
  does **not** block shots. Cleared by Debug or crushed by a Byte /
  Connection.
- **Debug** (mineblaster): one-shot weapon that only targets mines and
  walls (Minekiller weapon, `OnlyTargetCategory1=VOID`).

## 19. Command-fire abilities (D hotkey)

- `D` casts at the cursor at once; each ability also has its command
  panel button (NX Flag, "Attack" on the Obelisk, Firewall, Launch
  Mines, SIGTERM, Dispatch — §4), which arms it so the next map click
  casts it for the selected units of that kind only.
- Weapon-backed abilities honour their weapon's TDF range: NX Flag
  1400, Infection 2000, Mine Launch 1100 elmos. A mobile caster
  (Pointer, Byte) ordered beyond range walks toward the target and
  casts as soon as it is in range; any new order (move, attack, Stop…)
  cancels the pending cast. A stationary Obelisk refuses an
  out-of-range order. SIGTERM and Protect are Lua commands upstream
  with no range check — both reach anywhere on the map.
- **NX Flag** (Pointer): area ability — sets a wide circle ablaze for
  ~1 minute, dealing constant damage to anything inside. Has a
  multi-second cooldown after firing.
- **Infection** gas (Obelisk): an area-denial gas projectile that
  covers a circle in a poison cloud (~1000 dmg over its duration);
  any enemy that dies in the cloud turns into a Virus. Has a ~40 s
  cooldown; visible "pink fire on top" on the Obelisk while the
  weapon is ready.
- **Protect** (Firewall): casts a 20-second damage-halving bubble on
  every friendly unit within 300 elmos of the click position; half of
  incoming damage is reflected back at the attacker. 96 s recharge
  (upstream `90 * 32` frames), which also runs from the moment the
  Firewall is created — a new Firewall isn't ready right away.
- **Mine Launch** (Byte): lobs 5 Logic Bombs in a spread toward the
  target point at the cost of HP (self-damage). Mines past the team's
  Logic Bomb cap are not created.
- **SIGTERM** (Terminal): calls the Signal bomber, which flies to the
  target point and drops a bomb for ~10,000 damage over a wide area,
  plus a brief denial zone. 96 s recharge that, like the Firewall's,
  starts when the Terminal is created; no defense.

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
| **Carrier** | Homebase / main factory. Builds Packet, Connection, Flow and Gateway (`SIDEDATA.TDF [carrier]`). Visible body-piece "hatch" lifts up while producing |
| **Connection** (mobile) | Mobile teleporter — Dispatch + Enter just like a Port. Decent armor and an arc beam with good range and high single-target damage. Wins most 1v1 against large units, but folds to Pointer fire and is bad against swarms |
| **Gateway** | Lightly-armed mobile constructor (builds Ports, Firewalls, plus shared Bad Block / Logic Bomb / Debug); detector for mines + cloaked units |
| **Port** | On-datavent production building. Ticks the team's Packet Buffer. Dispatch sends up to 12 Packets at once; ALT-modified Dispatch drains the Buffer in 12-per-frame batches |
| **Packet** | Basic light spam unit. Weaker than Bit / Bug in combat, but much faster. Spawned by Dispatch and can re-Enter the buffer |
| **Signal** | The SIGTERM bomber a Terminal sends (§19). Not buildable — no factory lists it; untargetable while it flies |
| **Flow** | Air unit. Slow but crosses any terrain. Built to attack light targets (spam units, fire support); highly vulnerable to return fire — Pointers, DOS units and Connections shred Flows |
| **Firewall** | Special building. Casts a 20-second protective bubble on friendly units in a target radius — halves incoming damage and reflects the other half back at the attacker |

### Shared (any side via constructor)

| Unit | Role |
| --- | --- |
| **Bad Block** | Tiny wall, built by Assembler / Trojan / Gateway. Blocks small units' movement; does **not** block shots. Cleared by Debug or crushed by a Byte / Connection |
| **Logic Bomb** | Cloaked kamikaze mine, built by any constructor; also launchable by Bytes via the mine-launcher ability. One-shots Bits / Bugs, decent damage radius, does **not** chain-explode but does hurt your own units. Cap of 64 per team (§18) |
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

- The player's team is `0` by default. Once a second, any team left
  without a factory — homebase (Kernel / Hole / Carrier) or MiniFac
  (Socket / Window / Port), unfinished ones included — is out: all its
  remaining units die through the normal death path (upstream
  `game_over.lua` gamemode 1 + `KillTeam`). Defeat = the player team
  is out. Victory = every other team is out.
- On defeat, a centered red `DEFEAT` headline (~120 pt) appears at
  ~35 % from the top of the screen. On victory, the same layout but
  green and `VICTORY`.
- Once the game-over screen is shown, all gameplay systems stop
  ticking (units, animations, AI, combat). The camera still works.
- A sandbox map with no factories at all skips the game-over check
  entirely (no auto-defeat on the first frame); so do showcase mode
  and the menu demo. After "Keep on playing", factoryless teams are
  still wiped out but the result screen doesn't re-open.

## 23. AI opponent

A port of upstream's Lua AI (`KPAI.lua`, in its "Fair KPAI" flavour).

- Every non-player team that owns a homebase runs an AI brain that
  ticks once per second. A team with several homebases (several AI
  seats on one ally team) runs one brain driving all of them.
- **Fairness**: each tick the AI works out how far ahead of its
  enemies it is allowed to be — a head start of 8 spam units, 2
  "medium" units (constructors, heavies, artillery) and 1 building,
  plus every enemy unit of that kind, minus every one of its own. The
  menu difficulty widens the head start (Easy is exactly upstream's
  Fair KPAI). Once a budget is spent, that kind of production pauses.
- **Homebase production** (upstream `OrderHomeBase`): whenever a
  homebase's queue runs down, the AI rolls a d1000. With *n*
  constructors, a roll above 200·*n* builds another constructor (so
  there are never more than five); otherwise a roll in the top
  20 × (army size + packet buffer) builds one heavy or one artillery
  unit (coin flip) — big armies shift toward them; otherwise it queues
  three spam units. Kernel: Assembler / Bit / Pointer / Byte; Hole:
  Trojan / Bug / Dos / Worm; Carrier: Gateway / Packet / Flow /
  Connection.
- **Minifacs** autospam for every team (§10); the AI only switches
  their repeat off while its spam budget is spent.
- **Expansion**: every idle constructor picks a free datavent (a random
  sample of the free vents, nearest of the sample — so expansion
  spreads out) and builds its minifac there. Once the team owns three
  minifacs, one build in three is the faction special instead
  (Terminal / Obelisk / Firewall). The vent is claimed immediately.
- **Defend**: if enemy units come within ~700 elmos of a homebase,
  every idle army unit fights its way to them.
- **Attack**: once 8+ idle army units wait at home they attack-move
  (stop and fight anything met en route) at the nearest enemy minifac,
  falling back to the nearest enemy homebase. With an army over 50
  (packet buffer included) they instead rush the homebase of an enemy
  owning fewer than four small buildings. Idle units already out in
  the field rejoin the push immediately. Destinations are scattered
  on a 40–130 elmo ring so the group doesn't pile onto one point.
- **Network**: a teleporter with an enemy within 300 elmos dispatches
  packets at it (≥3 buffered, at most every 5 s per teleporter); a
  full 12-packet buffer is dispatched from the teleporter nearest the
  push target.
- **Specials**: a ready Terminal SIGTERMs the densest enemy cluster
  (15+ enemies within 300 elmos, not tangled up with too many of its
  own units), at most once per 15 s; a ready Obelisk gasses the
  nearest enemy within 1500; a ready Pointer plants an NX Flag on a
  crowd of 8+ within its 1400 range.
- **Hacker**: idle Bugs deploy into Exploits when the nearest enemy is
  600–1000 elmos away; Exploits pack back up when nothing is within
  1100 or an enemy closes inside 500.

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

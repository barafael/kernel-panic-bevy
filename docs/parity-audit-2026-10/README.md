# Parity audit: the remake against Kernel Panic 4.912 on Recoil (2026-10)

A comparison of the remake (`kernel-panic/src`, `spring-*`) with the game it
ports: the mod in `upstream/Kernel-Panic` (KP 4.912, "Fixes for 105") and
the engine that interprets it, `upstream/RecoilEngine`. Each area was read
on both sides — the mod's data, scripts, gadgets and widgets, the engine
code that gives them meaning, and the remake's implementation — and the
differences are listed with `file:line` evidence.

**Verdict.** The port of the engine's *formulas* is very close: damage
falloff, armour, ARMORED windows, build times, construction decay, movement
(`CGroundMoveType`, QTPFS), the Pointer/Byte script gates, the AI's numbers
all check out. What remains is mostly *engine behaviour around those
formulas that was never ported* — the command AI's idle and fight behaviour,
LOS and radar as game rules, target priority, the engine's water costs — plus
a few concrete bugs (Wormsplash, Flow targeting, the paralysis meter, the
Bad Block texture) and some deliberate deviations worth revisiting.

How to read this: §1 is the short list, ranked by what a player notices;
**every item in §1 was re-checked by hand on both sides**. §2 lists more
findings from the area reports, which were spot-checked. The area reports
(§4) have the full evidence, fix sketches, doc drift and "checked and
matching" lists. Paths: engine = `upstream/RecoilEngine/rts/`, mod =
`upstream/Kernel-Panic/`, remake = `kernel-panic/src/` unless noted.

Two upstream facts decide several findings:

- KP's `LuaUI/Widgets/kp_settings.lua` enables a whitelist of widgets and
  disables the rest — camera shake, build ETA, the team-colour override and
  the relative minimap are **off** in KP, so their absence is correct — and
  sets engine options: `CamMode=1` (overhead camera), `UnitIconDist=500`,
  `GroundDecals=1`, `MouseDragScrollThreshold=0` (`:14-79`).
- Recoil leaves cloaking to the Lua callin `AllowUnitCloak`, which KP doesn't
  define, so `isCloaked = wantCloak`: `MinCloakDistance` and cloak costs have
  no effect (`Unit.cpp:2521-2536`, `EventHandler.cpp:315-319`).

## Status (2026-10-26)

**Done** (on main):
- §1 #2 Flows, #3 Wormsplash, #4 the paralysis meter, #9 the dispatched
  Packet's ARMORED window, #20 the Bad Block texture (56c2ec2, 55e6ea9;
  SIM_VERSION 16).
- Selected units glow in KP's health colour (`gui_hilight_selectedunits.lua`,
  controls-hud #26; 4c42897). The texture fix had made the remake's
  brightening invisible on KP's black-bodied models. KP's 3 px outline is
  still missing (wgpu draws no wide lines).
- From the testbox runs (a Network seat played through the HUD; the bot's
  mouse and keyboard, 542a971):
  - Dispatch runs as `network_dispatch.lua`: one pass over the 12 spots per
    slow update, taken spots skipped for free, Packets facing out; Alt
    until the buffer is empty; the buffer clamped to the unit limit
    (controls-hud #20; 51eb783, SIM_VERSION 18).
  - `D` arms NX Flag and Dispatch, `A` reaches SIGTERM, Firewall and the
    Obelisk, `U` alone undeploys (controls-hud #10); right-click with only
    Ports dispatches (#1); ability cursors as the gadgets give them, the
    Unload cursor for Ports, the arrow over panel buttons and build bar
    icons (7f70d36).
  - "Buffer fill state: N" and its hint on screen; the minimap under the
    command panel; the player list at the bottom right (8fefd71).

**Decided:** `D` on a Bug deploys and `B` arms Bombard — a documented
divergence (the user's choice, 2026-10-26; KP's `D` arms Bombard).

**Closed since:** targeted Enter (controls-hud #11, SIM_VERSION 24),
Shift-queued gadget commands (#22, SIM_VERSION 25) and Packets refused on an
enemy teleporter (#27, SIM_VERSION 25). What is still open is in
`fix-order.md`.

## 1. The short list

*These are the findings of the 2026-11-08 review, written in the present
tense about the code as it was then. Most were fixed in SIM_VERSION 21-25;
`fix-order.md` has the batches, the commits and the gaps still open.*

### Combat and game rules

1. **Weapons fire at what nobody can see.** A weapon may only auto-target,
   or keep shooting, a unit its ally team has in LOS or radar
   (`Weapon.cpp:1039-1040`, `GameHelper.cpp:719-726`); radar-only contacts get
   ×10 worse priority and an aim error of up to 96 elmos, and an attack order
   on a mobile target is dropped 4 slow updates after LOS and radar are lost
   (`CommandAI.cpp:1469-1485`). The remake's only visibility gate is cloak
   (`units/spatial.rs:89-91`, `units/combat/mod.rs:1131-1171`); the fog is
   computed for the local player's screen and nothing else
   (`units/mechanics/cloak.rs:32-33`). Exploits (sight 300, range 1200) and
   Pointers (sight 768, range 1400) fire blind out to full range.
   → [sensors-cloak #1](sensors-cloak.md), [weapons #3](weapons.md)

2. **Flows are immune to almost every ground weapon.** `NoChaseCategory`
   only stops the command AI from *chasing*: its one use in the sim is
   `MobileCAI.cpp:583`, and `CWeapon::TestTarget` (`Weapon.cpp:1020-1085`) has
   no such filter. The remake reads `NoChaseCategory=VTOL` as "don't target
   flyers" (`combat/mod.rs:1023`, `:1148`), so only Pointers and DOS shoot
   Flows. Upstream, Bits, Bugs, Packets, Bytes, Gateways, Connections, Viruses
   and Exploits all do; KP's readme: "Pointers, DoSes and Connections make
   short work of Flows". → [weapons #1](weapons.md)

3. **The Worm's splash never happens.** worm.bos ends each bite with
   `emit-sfx 4097` from `head` and `end`, which detonates Weapon2 (Wormsplash)
   as a full explosion whatever its weapon type (`UnitScript.cpp:788-821`):
   800 damage within 105 elmos, friend and foe, plus the infection that turns
   Worm kills into Viruses. The remake queues it, but `explode` returns early
   for any `Melee` weapon (`combat/damage.rs:753`, `:828`) — and Wormsplash is
   `WeaponType=Melee`. The test that covers it builds a def without that type.
   → [damage #1](damage.md), [weapons #2](weapons.md)

4. **The paralysis meter follows a different model; one or two DOS can't stun
   a heavy.** Engine (`Unit.cpp:1265-1302`, `:991-999`; `ModInfo.cpp:97-98`):
   the meter is capped at maxHP·(1 + paralyzeTime/40), decays *linearly* at
   2.5 % of max HP per second (also while stunned), and the unit unstuns when
   it drops below max HP — so the next beam re-stuns it. DOS_Beam also falls
   off to 50 % at max range (`BeamLaser.cpp:418-423`, no `minIntensity`).
   Remake (`damage.rs:474-487`, `combat/lifecycle.rs:115`, `:163-189`):
   uncapped charge, *exponential* decay (τ = 4 s) only while unstunned, a
   fixed 5 s stun, charge reset to 0 afterwards, no falloff. One DOS (1600/s
   against "heavy") settles at 6400 < 15000, so it never stuns a Byte or a
   Connection (engine: ~12–28 s); two never do either. Small units go the
   other way. → [damage #2](damage.md)

5. **Idle units don't engage enemies just outside their range.** Units keep
   the default Maneuver move state (KP hides the button), so every idle unit
   gives itself an attack order on any visible enemy within weapon range + 200
   and chases it up to range + 500 from where it stood, 5 s per order
   (`MobileCAI.cpp:337-358`, `:1196-1265`, `:886-901`; `MoveType.cpp:76`;
   `Unit.cpp:349-361`). The remake only shoots what is already in range. The
   Pointer is exempt upstream (`pointer.bos:158` holds position).
   → [weapons #7](weapons.md), [controls-hud #5](controls-hud.md)

6. **Fight and Guard don't chase.** Engine Fight diverts to the nearest
   visible enemy within range + 100 of the path, chases it and returns to the
   line (`MobileCAI.cpp` `ExecuteFight`); Guard attacks whatever hit the
   guarded unit in the last 40 frames (`ExecuteGuard`). The remake's fight
   only halts while an enemy is already in range
   (`interaction/movement.rs:46-50`, `:701-712`), its guard only trails
   (`movement.rs:731-764`), and nothing tracks a last attacker.
   → [controls-hud #5, #12](controls-hud.md)

7. **Target choice is nearest-first.** The engine scores candidates by
   facing, distance, (damage per second + target health), damage against the
   target's armour × its ARMORED multiplier, and cost (`power` = metal cost),
   with ×100 for a bad category, ×4 for stunning an already stunned target and
   ×0.5 for the last attacker (`GameHelper.cpp:657-790`). The remake ranks bad
   category, then distance (`combat/mod.rs:269-293`). Units pour fire into
   freshly built units that are still ARMORED (×0.000001 damage), ignore
   wounded targets, and DOS keep zapping stunned units instead of spreading.
   → [weapons #5](weapons.md)

8. **Buildings under construction can't be hurt.** Two things compound. The
   remake keeps every nanoframe out of the spatial index — no targeting, no
   splash, no area denial (`units/spatial.rs:201-206`, documented as
   deliberate) — and spawns it at full health (`lifecycle/spawning/mod.rs:485`),
   where the engine starts it at 0.1 HP and adds max HP × build step
   (`Unit.cpp:270`, `:1997-2020`). Upstream a Socket frame at 10 % has about
   2000 HP and takes ×4 damage (ARMORED with `DamageModifier=4`): killing
   expansions mid-build is a basic KP tactic the remake removes.
   → [units-construction](units-construction.md)

9. **Dispatched Packets are near-invulnerable for 5 s instead of 0.5 s.**
   `network_dispatch.lua:56-62` calls each new Packet's `Spawned()`, which
   clears ARMORED after `sleep 500` (`packet.bos:55-58`). The remake gives
   every Packet the 5 s grace of `Create()` (`damage.rs:255-311`).
   → [game-rules #1](game-rules.md), [animation #1](animation.md)

10. **The Obelisk's Infection lands instantly and does half its damage.**
    Upstream it is a high-arc ballistic shell (`ballistic=1`, velocity 800,
    `highTrajectory=1` on the Obelisk; 1–5 s of flight after a ~1 s aim). Its
    impact explosion deals 200 to units and 1500 to buildings within 350
    elmos, allies included, and infects; only then does the gas zone start
    (`corruptionweapons.tdf:386-425`, `areadenial.lua:29-49`). The remake drops
    the zone at the click on the same tick: no shell, no blast
    (`mechanics/command_fire.rs:818-850`).
    → [game-rules #3](game-rules.md), [damage #3](damage.md), [weapons #4](weapons.md)

11. **The Bug↔Exploit morph is instant.** bug.bos `Deploy()` waits until
    `morphable` (2 s after spawn, ~5 s after a factory build), pins the Bug
    (`MAX_SPEED 1`), stops it firing and plays ~2.9 s before the swap;
    exploit.bos `Undeploy()` waits for `idle` (5 s after every shot) and plays
    ~2.9 s. The remake swaps on the event's tick (`mechanics/deploy.rs:110-158`).
    Upstream the swapped unit is also made non-blocking and keeps the old
    heading and orders (`LuaCOB.lua:37-48`) — see §2.
    → [game-rules #2](game-rules.md), [animation #2](animation.md)

12. **Units walk through poisoned water at full speed — the AI included.**
    The engine prices water into every ground square: speed ×
    `1/(1 + 0.1·waterDamagePerSlowUpdate)` below height 0 (0.29 on
    DigitalDivide, where 27 % of the map is reachable shallow water; Speed
    Balls' water is ringed by cliffs) and × `1/(1 + 0.1·depth)` in any water
    (`MoveDefHandler.cpp:158-160`, `GroundMoveMath.cpp:25-26`, `:49-50`). That
    feeds both QTPFS costs and driving speed, so units path around the water
    and crawl through it (17–29 % speed). The remake's speed and path costs are slope ×
    terrain type only (`interaction/ground_move.rs:2154-2188`,
    `spring-pathfinding/src/cost.rs`). Porting the two terms is also the
    engine-faithful answer to `docs/next.md`'s "teach the bot player about
    water": upstream's AI has no water logic of its own.
    → [movement #1](movement.md), [ai-audio "Water awareness"](ai-audio.md)

13. **Constructors can't repair, and right-click lacks Guard, Repair and
    Dispatch.** Builders repair by default (`canRepair = builder &&
    repairSpeed > 0`, repair speed = WorkerTime, `UnitDef.cpp:366,395`): an
    Assembler restores a Socket at ~667 HP/s, a Kernel at ~1600 HP/s. A
    right-click on a friendly unit is Repair for a constructor when it is
    damaged, else Guard (`BuilderCAI.cpp:1343-1378`), Guard for other mobile
    units (`MobileCAI.cpp:974-997`), and a Ports-only selection dispatches at
    the click (`kp_defaultcommands.lua:39-71`). The remake resolves Enter,
    Clear, attack and move only (`interaction/selection/right_click.rs:355-492`);
    its cursor already shows Repair. → [controls-hud #1, #6](controls-hud.md)

14. **Worms and Logic Bombs are hidden too early.** The engine never cloaks a
    unit while it is being built (`Unit.cpp:1049-1055`), and the Worm has no
    `Init_Cloaked`: worm.bos sets `WANT_CLOAK` 5 s + 1.33 s after completion,
    and its radar and seismic sense start with `ACTIVATION` at 5 s. The remake
    cloaks both at spawn (`lifecycle/spawning/mod.rs:534-540`), so a Worm leaving
    the Hole or a mine being laid is never seen.
    → [sensors-cloak #7](sensors-cloak.md), [game-rules #9](game-rules.md)

15. **Homebases start with Repeat off.** `kp_autospam.lua:57-139` (whitelisted)
    turns Repeat on for the local player's Kernel, Hole and Carrier; the remake
    starts them off and credits that to kp_autospam
    (`lifecycle/production.rs:42-45`, `:269-272`).
    → [game-rules #4](game-rules.md), [units-construction #1](units-construction.md)

### Sensors and fog

16. **There is no radar.** Upstream, active Assemblers and Gateways (500 →
    480 elmos on the 32-elmo radar grid), Trojans (768) and the Worm (from 5 s
    after completion) show every non-stealth enemy in range — cloaked ones
    included — as an icon at a drifting position, on map and minimap, and
    weapons may target it there (`LosHandler.cpp:951-971`, `:171-186`). Byte and
    Connection `RadarDistance=200` does nothing: their scripts never set
    `ACTIVATION` (`activateWhenBuilt` defaults to false). The remake's
    stand-in gives every `RadarDistance > 0` unit, nanoframes and stunned ones
    included, a "detector" that reveals cloaked units as full models,
    targetable at their exact position; ordinary enemies beyond sight show
    nothing (`lifecycle/bookkeeping.rs:104-106`, `mechanics/cloak.rs`).
    → [sensors-cloak #2](sensors-cloak.md)

17. **Scouted enemy buildings vanish.** `GhostedBuildings` is on by default
    (`EngineOptions.lua:115-119`): a building seen once stays as a dim ghost on
    screen and minimap until the spot is seen again. The remake's fog has no
    memory. → [sensors-cloak](sensors-cloak.md), [world-rendering #10](world-rendering.md)

### AI

18. **The AI stops playing when its homebase dies.** Upstream the AI runs every
    seat it was given, minifacs or not; the remake only runs teams that own a
    homebase (`units/ai/mod.rs:516-518`, `:812`), while its census keeps such a
    team alive. → [ai-audio #1](ai-audio.md)

19. **The default AI is the wrong flavour.** KP's launcher seats the
    unbudgeted "Kernel Panic AI" by default, Fair KPAI only for the easiest
    duel, and Baczek's C++ AI for harder duels
    (`LuaRules/Gadgets/libs/GenerateGame.lua:292-310`). The remake runs the
    Fair budget at every difficulty, its slack ladder on top.
    → [ai-audio #2](ai-audio.md)

### Look

20. **Bad Blocks render nearly black — the cause of `docs/next.md`'s "bad
    blocks render wrongly".** The model shader treats tex1 alpha as the
    team-colour mask, `rgb = mix(tex1.rgb, team, tex1.a)`
    (`cont/base/springcontent/shaders/GLSL/ModelFragProgGL4.glsl:101`).
    `badblock.tga` has alpha 0 on all 65 536 texels (black 56 %, red 25 %,
    yellow 7 %), so KP draws its plain red/yellow art. The remake's bake treats
    RGB black as team colour and alpha as brightness
    (`units/assets/meshes.rs:397-433`, `scale = 25 + α·230/255`), so the block
    comes out at ~10 % brightness. The same inverted rule turns the Logic Bomb
    white, the Signal's gold body near-black and the Window hourglass into a
    blob, and gives every unit's black areas a 10 % owner glow.
    → [world-rendering #1, #4](world-rendering.md)

21. **Everything is drawn unlit and then tonemapped.** Unit and terrain
    materials are `unlit: true` (`meshes.rs:84-88`, `terrain/material.rs`), so
    the map's sun and ambient lights (`map_loading/mod.rs:1191-1216`) light
    nothing, and tex2 self-illumination and specular are ignored. The HDR
    camera keeps Bevy's default tonemapping (no `Tonemapping` override in
    `src`), which shows white at about 196/255 and washes out KP's saturated
    colours. → [world-rendering #2, #3, #7](world-rendering.md)

22. **Health bars belong on every damaged unit.** `unit_healthbars.lua` (on in
    KP) draws a bar over every visible unit below full health — enemies too —
    a build bar over every nanoframe, a paralysis bar with seconds left, and a
    reload bar for weapons of ≥ 4 s (`:39`, `:633-751`). The remake shows a
    health bar over selected units only
    (`interaction/selection/health_bars.rs`). → [world-rendering #6](world-rendering.md)

23. **No building ground decals.** The Kernel, Carrier, Hole and every minifac
    sit on `UseBuildingGroundDecal` plates, and the Debug's only visual is a
    512-elmo red reticle; the remake has no decal code.
    → [world-rendering #5](world-rendering.md)

24. **The Firewall's "ready" sparkle never plays.** `network_reflectorshield.lua`
    calls the script's `Charge()` when Protect is ready, which emits
    `custom:network_charge` every 120 ms (`firewall.bos`); the remake's
    Firewall has no animation driver (`animation/units/mod.rs:74`).
    → [animation #9](animation.md), [effects-visuals #8](effects-visuals.md)

25. **Directional CEG particles are rotated 90°.** The engine spans a
    `directional=1` particle with `ydir = zdir × speed`, `xdir = ydir × zdir`,
    so the texture's U runs *along* the motion
    (`Rendering/Env/Particles/Classes/SimpleParticleSystem.cpp:144-148`); the
    remake builds the pair the other way round (`weapon_fx/ceg.rs:1046-1053`).
    The `hline` speed lines of the Byte, Packet, DOS and Pointer hits and of the
    medium/large death blasts come out as rings of tangential dashes instead
    of radial streaks. → [effects-visuals #1](effects-visuals.md)

26. **Every DOS shot carries a 240-elmo green lens flare.** DOS_Beam sets
    `laserflaresize=0` (and `largebeamlaser=1`, scrolling 200-elmo tiles);
    `laser_flare_size` reads the explicit 0 as "absent" and uses the default
    15 (`weapon_fx/spawn.rs:623-630`), so a `thickness·15` flare sits on every
    DOS. → [effects-visuals #2](effects-visuals.md)

27. **Deaths spawn glowing team-coloured spheres KP never draws.** Per exploded
    piece the engine adds only a heat cloud (`UnitScript.cpp:904-906`), and KP
    maps `heatcloud` to `kpsfx/black.tga` (`RESOURCES.TDF:62`), so nothing
    shows but the flying pieces and the `ExplodeAs` CEG. The remake adds a
    team-coloured additive sphere per piece (`animation/mod.rs:974-992`,
    `:1216`). → [effects-visuals #4](effects-visuals.md)

### Controls

28. **The camera isn't KP's.** KP sets `CamMode=1`, the overhead ("TA")
    controller: north-up, tilted 0.464 rad from vertical, wheel zooms 17.5 %
    per notch toward the cursor (Ctrl+wheel tilts), middle-drag pans with no
    threshold, screen-edge scrolling, and it zooms out to 9.5 × the map's
    width in squares (`Game/Camera/OverheadController.cpp`,
    `kp_settings.lua:66-71`). The remake is a smoothed orbit camera with Q/E
    yaw and middle-drag rotation, 150–3000 elmos.
    → [controls-hud #16-18, #34](controls-hud.md)

## 2. More findings worth knowing

From the area reports, spot-checked (the starred ones re-checked by hand).
Severity as the reports rate it.

- **Morph results block movement*** — KP's morph makes the new Exploit or Bug
  non-blocking (`LuaCOB.lua:42` `SetUnitBlocking(nu, false)`), so units walk
  through deployed Exploits; the remake stamps them as obstacles. High.
  → [movement #2](movement.md)
- **A build order claims its datavent for every team*** — `VentClaim` has no
  team; the first *click* wins, not the first nanoframe, and the ghost leaks
  hidden enemy orders and mines. High. → [units-construction #2](units-construction.md)
- **Builders can dismantle buildings upstream*** — KP marks every mobile unit
  `Reclaimable=0` but no building, and Recoil lets builders reclaim
  reclaimable enemy units by default (`ModInfo.cpp:76-82`,
  `BuilderCAI.cpp:1063`). An Assembler takes down an enemy Socket in 30 s
  with nothing gained. KP's readme never mentions it, so it may be engine
  behaviour KP inherited rather than intended. High, by the engine.
  → [controls-hud #7](controls-hud.md)
- **The minimap ignores the mouse** — no click/drag camera move, no orders on
  the minimap; clicks fall through to the world behind it. High.
  → [controls-hud #2](controls-hud.md)
- **The AI's army model, Debug use and Virus handling differ** — KP sends
  MOVE waves and arty attack-grounds by mission, clears blocking features with
  Debugs, and walks Viruses to a minifac. Medium.
  → [ai-audio #3, #4, #8](ai-audio.md)
- **DOS and Gauss beams land as one instant hit** — upstream a non-beamburst
  BeamLaser fires `beamtime·30` one-frame sub-beams along a fixed line; DOS
  cycles every 8 frames, not 7. Medium. → [weapons #6](weapons.md)
- **Arcing weapons never test their arc against terrain** — Exploits and
  Obelisks fire into ridges. Medium. → [weapons #8](weapons.md)
- **The Bug never aims** — bug.cob names its callins `AimPrimary`, which the
  engine maps to weapon 1; the remake's driver has no aim. Medium.
  → [weapons #9](weapons.md), [animation #3](animation.md)
- **Slopes slow units by an extra cos(pitch)*** — the engine regulates the
  horizontal speed (`GroundMoveType.cpp:3508-3509` projects onto
  `flatFrontDir`), the remake the 3D speed (`ground_move.rs:1428-1455`).
  Medium. → [movement #3](movement.md)
- **The Signal flies as a fighter, not a bomber*** — bombers ignore
  `Acceleration` (`maxAcc` defaults to 0.065) and double the 500 turn radius
  (`UnitDef.cpp:636-641`); the remake uses 2.0 and 500
  (`command_fire.rs:258-265`), so SIGTERM lands 3–4 s earlier. Medium.
  → [movement #4](movement.md)
- **Socket and Window yardmaps aren't rotated with the build facing***
  (`interaction/structures.rs:289-316` indexes the yardmap with no facing).
  Medium. → [movement #6](movement.md)
- **Placement slope rule** — immobile buildees test `|Δh| ≤ 40·tan(MaxSlope)`
  per footprint square, not a MoveDef slope; Logic Bombs go on ~70° ramps in
  KP, 30° in the remake. Medium. → [units-construction #5](units-construction.md)
- **Logic Bombs and Debugs block construction** (their `y` yardmap blocks
  nothing upstream); **a factory's current build can't be cancelled**; **the
  exit spot is chosen at build start**. Medium.
  → [units-construction #3, #6, #7](units-construction.md)
- **Alt-dispatch drains 12 Packets per frame instead of per 15 frames**, and
  dispatch ignores blocked spawn spots and stunned or unfinished teleporters.
  Medium. → [game-rules #6](game-rules.md)
- **LOS ignores terrain**, aircraft are spotted at 1× sight in 3D instead of
  1.5× in 2D, enemy nanoframes are never visible, and the minimap keeps
  tracking re-buried Worms. Medium. → [sensors-cloak #3-6](sensors-cloak.md)
- **Enemy-built Bad Blocks show through fog** (`featureVisibility=2`). Medium.
  → [world-rendering #9](world-rendering.md)
- **Late explosion fronts credit a deleted owner**, so VirusDeath infects out
  to 45 elmos instead of ~13–19. Medium. → [damage #4](damage.md)
- **D casts at the cursor instead of arming**, A doesn't reach SIGTERM /
  Firewall / Obelisk, and KP's gadget commands can't be Shift-queued. Medium.
  → [controls-hud #10, #22](controls-hud.md)
- **Self-destruct is a toggle upstream** (a second Ctrl+D cancels; Stop
  doesn't). Medium. → [controls-hud #21](controls-hud.md)
- **Script sleeps round up to whole 33 ms ticks** (`sleep N` lasts
  floor(N/33)+1 ticks): the Hole's fade is 3.07 s, not 2.3 s; the Carrier keeps
  its build stance after the first lift. Medium. → [animation #4, #6](animation.md)
- **Sound mixer limits** — at most 17 copies of one effect and one unit reply
  at a time upstream; the remake caps only the total. Medium.
  → [ai-audio #5](ai-audio.md)
- **Firewall Protect lasts 640 frames (21.3 s)***, not 20 s
  (`network_reflectorshield.lua:20,103`, `protectionTime * 32`). Low.
  → [game-rules #5](game-rules.md)
- **Shots, beams and explosions show through the fog** (the engine draws a
  projectile only for allies or inside the viewer's LOS). High.
  → [effects-visuals #5](effects-visuals.md), [sensors-cloak #8](sensors-cloak.md)
- **Impact CEGs of traveling shots point straight up** instead of along the
  shot (the Pointer's spray fires into the ground); **build beams always use
  the System BuildLaser** (the Trojan's is a thick scrolling lightning ray);
  **no Firewall protection overlay**; **no Flow missile flare**. Medium.
  → [effects-visuals #6, #7, #9, #10](effects-visuals.md)
- **Beam and bolt materials premultiply by texture alpha** where the engine
  adds the full texel: the Byte's MegaBeam draws at ~56 % brightness; beams
  also lack their along-the-beam fade and default `coreThickness 0.25`.
  Medium. → [effects-visuals #11, #15, #19](effects-visuals.md)
- **CEG semantics:** Recoil starts every `CSimpleParticleSystem` particle at
  the explosion point (no `pos` offset, one roll per system), so the NX fire
  is a column, not a 60×60 field; and the CEG parser drops `r 60` (space
  after the op), which the TDF cross-check can't catch. Medium/low.
  → [effects-visuals #12, #20](effects-visuals.md)

## 3. Deliberate deviations worth a second look

Documented in the remake as choices; the evidence suggests revisiting them.

- **Nanoframes untargetable** (`units/spatial.rs:201-206`) — see §1.8.
- **The defeated side's units blow up within 4 s** ("the user's rule",
  `lifecycle/game_over.rs:38-41`). Upstream `Spring.KillTeam` only makes the
  players spectators (`Team.cpp:255-281`); the units stay and keep fighting,
  which matters in free-for-alls. The code comment at `game_over.rs:111-115`
  claims upstream destroys them; it doesn't.
- **SIGTERM `id==168` treated as intended** (`combat/damage.rs:203-209`).
  Under Recoil no weapon has that id, so in real KP 4.912 homebases take full
  SIGTERM damage and closed Bytes take 30 %.
- **Fair KPAI for every seat** — see §1.19.
- **Orbit camera and per-faction colours** — KP plays with the overhead camera
  and per-player colours; the remake's choices change the feel.
- **Flow speed bonus always applied** — in Recoil the bonus only reaches a
  Flow through a plain player order, frozen at that moment's speed; plan.md's
  reason ("Recoil clamps MAX_SPEED") is wrong. → [movement #7](movement.md)

## 4. Area reports

| Report | Scope | Findings |
|---|---|---|
| [damage.md](damage.md) | impact → health, paralysis, death, healing | 9 |
| [weapons.md](weapons.md) | targeting, fire gating, aim, projectiles | 14 |
| [game-rules.md](game-rules.md) | KP's synced gadgets, setup data | 15 |
| [units-construction.md](units-construction.md) | FBI stats, construction, factories, placement | 11 |
| [controls-hud.md](controls-hud.md) | commands, right-click, keys, camera, minimap, HUD | 39 |
| [effects-visuals.md](effects-visuals.md) | weapon looks, CEGs, debris, build beams | 22 |
| [world-rendering.md](world-rendering.md) | materials, team colour, terrain, decals, icons, overlays | 28 |
| [animation.md](animation.md) | unit scripts vs the Rust drivers | 16 |
| [movement.md](movement.md) | ground/air movement, pathing, water, yardmaps | 10 |
| [sensors-cloak.md](sensors-cloak.md) | LOS, radar, fog, cloak, seismic | 11 |
| [ai-audio.md](ai-audio.md) | KPAI port, sound mixer, replies | 12 |

Each report also has a **doc drift** section (FEATURES.md, plan.md,
DEEP_FEATURES.md and code comments that no longer match the code), a
**checked and matching** list, and **unverified leads**. Helper tools used:
`cargo xtask cob` (a COB disassembler; the compiled `.cob` files fixed several
constants) and `cargo xtask s3o` (an S3O piece-tree dump).

## 5. How this was done, and claims dropped

Eleven area audits each read the engine, the mod and the remake for one
area; their reports are above. The short list was then re-checked by hand.
Along the way these claims were checked and found **wrong**, so they need no
work: construction decay too slow, build range measured to the centre,
`CanAssist=0` builders resuming frames, factories not waiting for a clear pad,
the death blast credited to the killer, a fixed 5 s self-destruct, Firewall
reflection applying armour, the Worm's re-cloak ignoring re-aims, AI-dispatched
Packets standing still, the Pointer missile's turn rate, impact effects drawn
at half scale, a missing default explosion for weapons without a CEG (every
KP weapon names one), and missing camera shake / build ETA / team-colour
widgets (all disabled by `kp_settings.lua`).

# Units, construction & production parity audit

Scope: FBI stat reading, mobile construction, factories (homebases, minifacs), build queue, nanoframes, build speed, factory exit, placement, unit limits. Engine = `upstream/RecoilEngine/rts`, mod = `upstream/Kernel-Panic`, remake = `kernel-panic/src` unless prefixed.

## Findings (most important first)

### 1. Homebases start with Repeat off — severity: high · confidence: verified
- Original: `LuaUI/Widgets/kp_autospam.lua:5` "Sets the kernel and hole to repeat from the start"; `:63-64` (kernel), `:72-73` (hole), `:90-91` (carrier) give `CMD.REPEAT {1}` in `UnitCreated`, and `:117-131` re-issue it on frame 2 for the starting homebases (the widget's own note, `:108-111`, says first-frame orders "are ignored"). Runs for the local player's own team (`:60`); `kp_settings.lua:35` force-enables the widget. Only KPAI turns repeat off on its homebases (`KPAI.lua:729-732`).
- Remake: homebases get `Producer::new()` with `repeat: false` (`units/lifecycle/production.rs:76-87, 269-272`); the doc comment `production.rs:42-46` ("homebases start with it off", attributed to kp_autospam) and test `:1074-1089` lock that in. Nothing else turns it on except an explicit `SimCommand::SetRepeat`.
- In play: a human queues 3 Bits + 1 Assembler on the Kernel. In KP the Kernel cycles that queue for the rest of the game; in the remake it builds four units and goes idle.
- Fix: spawn human seats' homebases with `repeat = true` (AI seats stay off, as KPAI); fix the comment and test.

### 2. A build order reserves its datavent against every team, and site checks see through fog — severity: high · confidence: verified
- Original: there is no reservation. `CBuilderCAI::GiveCommandReal` refuses an order only when the site is already blocked by a building or nanoframe (`Sim/Units/CommandAI/BuilderCAI.cpp:477-481`). On the way there, the site is re-checked every 5th CAI SlowUpdate (`:710-715`), and only against objects in the builder's allyteam LOS (`Game/GameHelper.cpp:1470, 1480`). The race goes to whoever starts a nanoframe first; the other order then ends on arrival.
- Remake: any build order claims the vent (`net/apply.rs:1342-1347`). `reconcile_vent_claims` keeps the claim while any team's `PendingBuild`, `Constructing` or queued `BuildAt` targets the vent, or any immobile unit (a cloaked Logic Bomb included) stands within 16 elmos of it (`terrain/geovent.rs:47, 318-405`). The ghost won't snap to a claimed vent unless the selected builders' own orders claim it (`ui/hud/placement.rs:385-392, 464-467`), and the AI drops claimed vents from its pick list (`units/ai/mod.rs:311`). Separately, `start_construction` ends an order on the tick an overlapping structure appears, at any distance and with no LOS test (`units/lifecycle/construction.rs:426-439`). The ghost's obstacle list also ignores LOS (`placement.rs:468-484`). FEATURES.md:501-502 describes the exclusivity, but not as a deviation from KP.
- In play: player A sends an Assembler across the map to a contested vent. Player B's Trojan is standing next to that vent, yet B can't even place a Window there: the first click wins, not the first arrival. Allies and the AI are locked out the same way. A ghost that refuses to snap, or turns red, reveals an enemy order or a hidden enemy building or mine in fog.
- Fix: remove cross-team claims (at most hide a vent from its own team's UI), keep resolving races in `start_construction`, and limit the "site taken" and ghost-obstacle tests to structures the allyteam can see.

### 3. The unit a factory is building can't be cancelled or pre-empted; Alt-inserted orders join the repeat cycle — severity: medium · confidence: verified
- Original: right-click replaces matching queue entries with `CMD_STOP`, back to front, the front included (`Sim/Units/CommandAI/FactoryCAI.cpp:247-266`). `SlowUpdate` then executes the STOP at the front (`:401-415`), and `CFactory::StopBuild` kills the buildee (`Sim/Units/UnitTypes/Factory.cpp:316-320`, `KillUnit(…, -DAMAGE_FACTORY_CANCEL)`). Alt+click with repeat off pushes the order to the very front, and `BuildeeChangeCheck` kills the buildee when the new front is a different kind (`FactoryCAI.cpp:155-160, 268-283`). With repeat on, the order goes in behind the current build as `INTERNAL_ORDER`, and `FinishCommand` never re-appends those (`CommandAI.cpp:1667-1671`).
- Remake: `remove_kind` skips index 0 once `unit_spawned` is set (`production.rs:189-211`), and `unit_spawned` is set on the build's very first tick (`production.rs:641-705`), so the build in progress can never be removed. `enqueue_front` always inserts behind a started build (`:162-167`). `complete_front` re-appends every finished order, Alt-inserted ones included (`:114-120`).
- In play: right-click the Byte icon 10 s into a 28 s Byte. KP cancels it; the remake finishes it. Alt+click an Assembler on a repeating Kernel: KP builds one, the remake adds it to the cycle forever.
- Fix: let the front be removed (despawn the buildee the way a factory-cancel does, reset progress). Alt without repeat: insert at index 0 and cancel a buildee of a different kind. With repeat: mark Alt-inserted orders one-shot.

### 4. Queued mobile builds aren't paced by the builder's 15-frame CAI SlowUpdate — severity: medium · confidence: likely
- Original: `ExecuteBuildCmd` only runs from the builder's SlowUpdate, every 15 frames and staggered per unit (`Sim/Units/UnitHandler.cpp:348-366`, `BuilderCAI.cpp:499-540`). The nanoframe is only created if the script already reports `INBUILDSTANCE` (`Sim/Units/UnitTypes/Builder.cpp:823`); otherwise it retries at the next SlowUpdate (`BuilderCAI.cpp:664`). A finished build is only noticed at the next SlowUpdate (`BuilderCAI.cpp:630-638`; `StopBuild` → `StopBuilding` first). Then `StartBuild` restarts the script. assembler.bos `StartBuilding` waits for nozzle/rotor moves before setting the stance (`scripts/assembler.bos:43-53`, and `StopBuilding` has already begun retracting). trojan.bos always `sleep`s at least once in its heading loop (`trojan.bos:39-53`). So for those two the first attempt fails and the one 15 frames later succeeds. gateway.bos sets the stance synchronously (`gateway.bos:70-75`).
- Remake: the next queued `BuildAt` is promoted on the next tick (`interaction/movement.rs:683-699`). `start_construction` and `tick_construction` spawn the frame at once (`construction.rs:554-559, 696-751`), and power flows from the tick the driver reports the stance (`construction.rs:764-777`; e.g. `assembler.rs:154-168`, ~3–4 frames once the nozzle is already out).
- In play (estimate): a Shift-dragged row of Logic Bombs (BuildTime 120 at WorkerTime 128 = 29 frames each) takes about 29 + 2–16 + 15 ≈ 53 frames per mine with an Assembler or Trojan in KP, and about 38 with a Gateway. The remake takes about 32–35 for all three. A Bad Block wall (38 frames each) is about 62 vs 44 frames per block. Mine fields and walls go up roughly 1.2–1.6× faster than in KP.
- Fix: run constructor order logic (arrival test, `StartBuild`, finish detection) on a per-unit 15-frame phase, and create the nanoframe only at such a tick once the stance is set.

### 5. Placement uses a MoveDef-style slope test instead of the engine's height-difference test — severity: medium · confidence: verified
- Original: for immobile buildees, `CheckTerrainConstraints` tests each footprint square: `std::abs(wantedHeight - groundHeight) <= unitDef->maxHeightDif` (`GameHelper.cpp:1634-1637`). It is called per square from `TestBuildSquare` (`:1440`) with the square-centre height. `maxHeightDif = 40·tan(MaxSlope)` (`Sim/Units/UnitDef.cpp:423-427`). There is no slope angle in this test, and the 1.5× multiplier belongs to MoveDefs only.
- Remake: `slope_ok` requires `max_slope_in_footprint ≤ max_slope_ratio(kind)`, i.e. `1 − cos(1.5·MaxSlope)` against the surface normal at every footprint grid point (`placement.rs:291-295`, `terrain/heightmap.rs:229-253`, `unit_registry.rs:220-225`, `spring-pathfinding/src/cost.rs:295-299`).
- In play: Logic Bomb (MaxSlope 20, 1×1 footprint): KP allows |Δh| ≤ 14.6 elmos at square centres 4–5.7 elmos out, i.e. ground up to ~69–75°. The remake stops at 30°. Bad Block (MaxSlope 32): ~56–64° in KP vs 48°. On the original maps, 4–12 % of squares are 30–48° (Quad Core 12.1 %, Marble Madness 6.9 %, Dual Core 5.9 %, computed from the SMF heightmaps). Those are the ramps between levels, where KP players mine chokepoints; the remake shows a red ghost there. Datavent buildings are unaffected because every vent sits on flat ground.
- Fix: in `resolve_site`, compute the build height (ground height at the centre for `LevelGround=0` kinds) and accept the site if every footprint square's centre height is within `40·tan(MaxSlope)` of it.

### 6. Logic Bombs and Debugs block construction (their `y` yardmap blocks nothing in KP) — severity: medium · confidence: verified
- Original: `units/logic_bomb.fbi:30` `YardMap=y` and `mineblaster.fbi:36` `yardmap=y` map to `YARDMAP_OPEN = 0` (`UnitDef.cpp:849`, `Sim/Objects/SolidObject.h:37`). `AddGroundBlockingObject` skips cells where `(yardmapState & mask) == 0` (`Sim/Misc/GroundBlockingObjectMap.cpp:102`), so these units never occupy the blocking map. A buildee whose yardmap square is ≤ `YARDMAP_GEOSTACKABLE` can also be placed over immobile objects (`GameHelper.cpp:1481-1484`).
- Remake: both kinds are `is_building` (`CanMove=0`, `unit_registry.rs:585-588`). That makes them footprint obstacles for the ghost (`placement.rs:468-471`), and any build order whose footprint overlaps one ends (`construction.rs:429-432`). Within 16 elmos of a vent they also claim it (#2). Movement already treats `y` as open (`interaction/structures.rs:299-306`).
- In play: an enemy's cloaked Logic Bomb on a datavent turns your Socket ghost red, which reveals the mine, and silently cancels build orders there. In KP the Socket goes up over the mine, and the mine (a kamikaze that triggers on any enemy unit within 89 elmos, nanoframes included) blows up the fragile frame. You also can't lay mines across your own building footprints or over existing mines.
- Fix: test overlaps against yardmap-blocking squares rather than the FBI footprint, and let all-`y` buildees ignore immobile objects.

### 7. The factory exit spot is picked when the build starts, and without a passability test — severity: medium · confidence: verified
- Original: `FinishBuild` → `AssignBuildeeOrders` → `SendToEmptySpot` (`Factory.cpp:255-273, 443-454, 349-441`) runs when the unit completes. Each arc candidate must pass both `NoSolidsExact` and `moveDef->TestMoveSquare` (`:379-385`; the random fallback also checks `TestMoveSquare`, `:409-413`).
- Remake: `empty_exit_spot` runs when the buildee spawns, i.e. at build start (`production.rs:651-679`). The result is stored in `Emerging.rally_then` (`:734-741`) and used at completion (`spawning/emerge.rs:149-180`). Candidates are tested for bounds, front half and solids only (`production.rs:310-358`).
- In play: a Kernel building Bytes (28 s each) with no rally point. In KP each finished Byte sees the earlier ones parked and takes the next free spot on the arc. In the remake each Byte's spot was picked just after the previous Byte left the pad, while that spot was still empty, so they all head for the same straight-ahead spot and shove each other. Minifac Bits pair up the same way. Next to cliffs or void (Hex Farm towers) the remake can pick spots the unit can't stand on.
- Fix: compute the spot in `emerge_system` when the unit completes, and add the mover's slope/passability test.

### 8. Datavent factories face toward the map centre from the vent, not KP's team-wide build facing — severity: low · confidence: verified
- Original: kp_autospam sets a single `buildfacing` the first time a build command is armed. It is derived from the player's homebase position: `|mapSizeX−2x| > |mapSizeZ−2z|` → west/east, else north/south (`kp_autospam.lua:117-158`). All later buildings use it until the player rotates it ([ / ]).
- Remake: each vent's facing is baked as "toward the map centre from this vent", plus a terrain veto (`spring-map/src/datavent.rs:171-213`). `tick_construction` uses it (`construction.rs:721-727`). There is no rotate control.
- In play: a forward Socket past the map centre faces back toward its own base, so its Bits pour out homeward. In KP it faces onward like the player's other buildings.
- Fix: derive one facing per team from its homebase (the terrain veto can stay), and add the rotate keys.

### 9. Buildees spawn at full health — severity: low · confidence: verified
- Original: `health = beingBuilt ? 0.1f : unitDef->health` (`Sim/Units/Unit.cpp:270`); each build step adds `maxHealth·step` (`:2016`), and decay removes `maxHealth·buildDecay` (`:1038`).
- Remake: every unit, buildees included, spawns with `Health::full(max_health)` (`spawning/mod.rs:485`), and nothing ties health to progress.
- In play: a selected nanoframe or factory buildee shows a full health bar from the first frame. FEATURES.md:439-441 calls the rising health bar the only progress indicator. Damage is moot while buildees can't be targeted (see the one-liners below).
- Fix: while Emerging, set health = 0.1 + progress·max, and lower it on decay.

### 10. Two SlowUpdate timings assume 16 frames — severity: low · confidence: verified
- Original: `UNIT_SLOWUPDATE_RATE = 15` (`Sim/Misc/GlobalConstants.h:60`). The factory BuggerOff fires when `(frameNum & (UNIT_SLOWUPDATE_RATE >> 1)) == 0`, i.e. every 8th frame (`Factory.cpp:133`). A builder gives up after 31 failed `StartBuild`s 15 frames apart, ≈ 15.5 s (`BuilderCAI.cpp:664`).
- Remake: `tick.frame() & 8 == 0` is true 8 frames out of every 16 (`production.rs:461`; the comment says so). `BUILD_WAIT_GIVE_UP = 30·16/30 = 16 s`, and it only counts frames while the footprint is occupied (`construction.rs:110-114`). By contrast, `DEACTIVATE_DELAY = 7 s` does use 15 >> 1.
- Fix: use `frame & 7 == 0`, and 31·15/30 s.

### 11. Unit cap: factories go through the build animation, and script-spawned units ignore the team cap — severity: low · confidence: verified
- Original: at the cap, `QueueBuild` keeps the order but never sets `curBuildDef` (`Factory.cpp:296-297`), so no `Activate` and no `StartBuilding` run. `Spring.CreateUnit` refuses at the team cap (`Lua/LuaSyncedCtrl.cpp:1871`, `CanBuildUnit`), and that path creates Viruses (`infection.lua:36`), launched Logic Bombs (`Launcher.lua:37`) and SIGTERM bombers (`airstrike.lua:48`).
- Remake: `production_system` sets `building = true` before bailing out on `at_limit` (`production.rs:617-649`), so the Kernel driver emits build beams every frame (`assets/animation/units/kernel.rs:355`) and the yard opens on any queued order (`script_triggers.rs:156`). `spawn_queued_viruses` and `spawn_queued_mines` check no team cap (`spawning/mod.rs:803-853`).
- In play: only at 1000 units. Factories beam with nothing being built, and Viruses and mines keep appearing past the cap.
- Fix: check `at_limit` before marking a build step, and gate script spawns on `TeamUnitCounts::at_limit`.

## Doc drift
- FEATURES.md:200-201 lists "factory rally orders" as not simulated. They are simulated: `FactoryOrders` (`production.rs:58-73`), `net/apply.rs:600-618`, `emerge.rs:155-170`.
- FEATURES.md:439-441 says "no progress bar … the only progress indicator is the health bar … goes up to 100 %". Buildees spawn at full HP (#9), and the build bar does have a progress pie (`production.rs:122-132`, `ui/hud/build_bar.rs:230`).
- FEATURES.md:442 says "The Kernel (System homebase) builds faster". The code, like upstream (`kernelboost.lua:25`, `isHomeBase`), boosts all three homebases (`production.rs:611-616`).
- `production.rs:45` credits "homebases start with it [repeat] off" to kp_autospam. Upstream sets it on, and the remake's own `docs/kernel-panic-game-design.md:382` says "starts with *repeat* on".
- `production.rs:171` (`cancel_all`) says "as a right-click couldn't cancel it either". The engine's right-click does cancel the build in progress (#3).
- `unit_registry.rs:142` says CanAttack=0 is set by "every constructor in the shipped data". `gateway.fbi` doesn't set it: it carries Weapon1=GatewayBeam (range 350), and the code correctly arms it.
- `spring-map/src/datavent.rs:3-8` attributes the "toward the map centre from the vent" rule to kp_autospam. kp_autospam measures from the homebase, not the vent; `hand_autobuild.lua` belongs to the RPS side.
- `construction.rs:110-113` says "(16 frames)", and the comment at `production.rs:459-460` misreads `UNIT_SLOWUPDATE_RATE >> 1` (#10).

## Checked and matching (one line each)
- Build lists = `SIDEDATA.TDF [CANBUILD]`, order included: Kernel/Hole/Carrier/Socket/Window rosters (`production.rs:229-255`) and the three constructors (`construction.rs:83-108`); Signal and Virus are never buildable.
- Build step: factories add `WorkerTime/30` per frame against `BuildTime`, builders likewise (`Builder.cpp:109`, `Factory.cpp:84`, `Unit.cpp:2002`). Table below; the only difference is the engine's whole-frame finish versus the remake's carried-over fraction (< 1 frame).

  | build | BuildTime | WorkerTime | engine frames | remake frames |
  |---|---|---|---|---|
  | Bit / Packet at Kernel / Carrier | 240 | 128 | 57 | 56.25 |
  | Bit at Kernel with 5 finished small buildings (×2) | 240 | 256 | 29 | 28.1 |
  | Bit at Socket / Bug at Window | 240 / 270 | 64 | 113 / 127 | 112.5 / 126.6 |
  | Bug / DOS / Worm / Trojan at Hole | 270/1280/3200/1600 | 128 | 64/300/750/375 | 63.3/300/750/375 |
  | Pointer / Byte / Assembler at Kernel | 1920/3600/1600 | 128 | 450/844/375 | 450/843.75/375 |
  | Socket/Window/Port/Terminal; Obelisk; Firewall (constructor) | 3840; 8000; 3200 | 128 | 900; 1875; 750 | same |
  | Bad Block / Logic Bomb / Debug | 160/120/600 | 128 | 38/29/141 | 37.5/28.1/140.6 |
- Kernel Boost: ×(1 + 0.2·finished small buildings) on every homebase (`kernelboost.lua:23-31` → `SetUnitBuildSpeed`; `production.rs:611-617`). The set is Socket/Window/Port/Terminal/Obelisk/Firewall. Buildings count at UnitFinished, a nanoframe's death is not subtracted, and Take/Give re-tallies (`bookkeeping.rs`). The engine applies a change one frame late (the speed is set inside `AllowUnitBuildStep`): negligible.
- Factory throughput per unit: the engine waits for the factory's next CAI SlowUpdate (1–15 frames, `FactoryCAI.cpp:374-428`, `Factory.cpp:292-293`) or for the pad to clear (`Factory.cpp:185`). The remake has no SlowUpdate wait but needs the previous unit a full radius sum (12+12) off the pad. Bit at Kernel works out ≈ 68 vs ≈ 68 frames per unit (estimate).
- Construction decay: `constructionDecayTime = 0.1 s` → 3 frames; speed 0.001 is floored to 0.01 (`ModInfo.cpp:254-255, 410`). 1/(BuildTime·0.01) per SlowUpdate (Bad Block 62.5 %, Logic Bomb 83 %, Socket 2.6 %) is applied smoothly (`construction.rs:801-840`, `Unit.cpp:1030-1051`); a fully decayed frame dies without a blast.
- Build range = `BuildDistance` (384; default 128, floor 38) + buildee model radius (s3o header radius, e.g. Socket 39.72), measured in 2D (`buildRange3D` off) (`BuilderCAI.cpp:245-264, 624-628`; `unit_registry.rs:536-560`).
- `CanAssist=0`: a site that already holds a building or nanoframe ends the order, and a builder can't resume its own abandoned frame (`BuilderCAI.cpp:307-369`, `construction.rs:426-439`).
- Clearing a feature off the site: Bad Block `reclaimTime = max(1, 64·6) = 384`, so 90 frames at 128; one builder per feature per frame (`multiReclaim 0`); a Bad Block ordered on a Bad Block feature finishes the order; nothing is gained (`Feature.cpp:317-384`, `construction.rs:453-493, 563-622`).
- The Bad Block is a unit while being built and turns into its `badblock` feature (400 HP) on completion, with no blast (`Unit.cpp:440-454`, `features.rs:302-339`).
- Unit limits: MaxUnits 1000 (`EngineOptions.lua:62-66`; the clamp to 32000/teams never bites). At the cap factories keep their orders and builders wait in range (`Factory.cpp:296`, `BuilderCAI.cpp:656`). Logic Bomb `UnitRestricted=64`: at the limit `StartBuild` refuses and the order is dropped (`Builder.cpp:820`, `construction.rs:698-709`).
- Shift-queueing on one's own sites: `GetCancelQueued` / `GetOverlapQueued` formulas (`CommandAI.cpp:1312-1466`) = `queue_build` (`construction.rs:320-370`). The `Pos2BuildPos` 16-elmo snap = `build_pos` (`GameHelper.cpp:983-1003`, `placement.rs:150-159`).
- Factory gates: waits for the yard to open and `INBUILDSTANCE`; a stunned factory or builder adds no power and its frame decays; a factory's death kills its buildee; a buildee's death restarts the front order; the yard closes 7 s after the last step (`Factory.cpp:105-169, 69-78, 336-345`).
- `SendToEmptySpot` geometry (4R+4r arc, 100 steps, 1.5r clearance, front half only, random fallback, R+r first waypoint); factory move/fight/patrol/guard orders are copied to new units and Stop clears them.
- Minifac autospam: Socket→Bit and Window→Bug on repeat from creation, starting once the building is finished (`kp_autospam.lua:66-79`, `production.rs:89-98, 269-284`).
- Datavent requirement: yardmap `g` ⇒ `needGeo`, covering exactly the six small buildings (`UnitDef.cpp:847`, `GameHelper.cpp:1299-1322`). The engine accepts a centre up to 27 elmos off the vent while the remake snaps onto it exactly (sub-square cosmetic).
- `LevelGround` (default 1 for all six datavent buildings) is not ported (no terraform), but every vent on the 16 original maps sits on perfectly flat ground: the terraform cost over the 9×9 corners is 0 for all vents, checked from the SMF heightmaps. No effect.
- `MetalToGeo.lua` is not ported. It only adds vents when a map has fewer than 4 geo spots or fewer spots than players. All originals have ≥ 6 spots (pacman 6; Circular Buffer and DigitalDivide 8), so it only matters with ≥ 7 or ≥ 9 seats on those maps.
- Stat reading: `MaxVelocity`×30, `Acceleration`/`BrakeRate` per frame² (defaults 0.5 / maxAcc), `TurnRate`×2π/65536×30, `IdleAutoHeal` (default 10/s), `IdleTime` in frames (default 600; only damage resets it), `AutoHeal`/s, self-destruct (2n+1)·15 frames, `KamikazeDistance`+25, mass defaults to the metal cost, unitname = file name, seismic default (`UnitDef.cpp:329-508`). No duplicate keys in the default FBIs except `icontype`.

## Documented deviations I find questionable (one line each)
- Buildees/nanoframes are left out of the spatial index (`units/spatial.rs:201-207`, `production.rs:636-640`). In KP a datavent nanoframe starts at 0.1 HP and takes ×4 damage (DamageModifier=4 while ARMORED), and a mine near the vent triggers on it. Killing expansions under construction is a core tactic, and here it is impossible.
- Builder Repair is not simulated (FEATURES.md:200-201). In KP every constructor has it (`canRepair` defaults to `Builder`, RepairSpeed = WorkerTime: `UnitDef.cpp:366, 395`), repairing maxHealth·128/BuildTime HP per second: Socket ≈ 667 HP/s, Kernel ≈ 1600 HP/s.

## Unverified leads
- The engine's `UpdateBuild` adds no power while the builder is out of range (`Builder.cpp:303`), and `MoveInBuildRange` walks it back in. `tick_construction` has no range check. This only matters if the builder gets pushed away.
- Where the builder stops: the engine sets the goal at 0.9·range and the CAI checks range every 15 frames, so it ends anywhere in [0.9R, R]; the remake stops at exactly R (`construction.rs:440-445`).
- The engine lets you build inside an open factory yard: `c` squares leave the blocking map when the yard opens and `BLOCK_BUILDING` isn't set (`GroundBlockingObjectMap.cpp`, the commented-out line near `:91`). The remake blocks the whole factory footprint. Exploit-level edge case.
- A built Bad Block's feature keeps the builder's team/allyteam in the engine (`Unit.cpp:446`), and with `featureVisibility=2` enemy Bad Blocks are hidden outside LOS. Remake features are `FEATURE_TEAM`. Needs the LOS auditor.
- Rally first waypoint: the engine's `insertBuiltUnitMoveCommand` point is unit.pos + front·2·factoryRadius (Kernel: 140 elmos), copied move orders get up to 2π elmos of jitter, and flyers get no exit waypoint (`Factory.cpp:436-509`). The remake uses R+r with no jitter, flyers included.
- `network_build.lua`'s scrolling build texture on Gateway/Carrier buildees is not ported (cosmetic; rendering auditor).
- Builders snap their yaw to the site every tick (`construction.rs:786-797`). In the engine the move type turns the unit at its turn rate, and the Trojan's script turns its body in 720-unit steps.

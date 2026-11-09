# Movement and pathing parity audit

Scope: ground move type, Flow (`CHoverAirMoveType`), Signal (`CStrafeAirMoveType`), collisions, crushing, terrain/slope/water speed, QTPFS inputs, MOVEINFO, arrival/stop/give-up, yardmaps, scripts and gadgets that change movement. Engine paths are relative to `upstream/RecoilEngine/rts/`, KP paths to `upstream/Kernel-Panic/`, remake paths to the repo root.

Short version: the port of `CGroundMoveType` itself is very close to the engine (formulas, constants, order of operations). The gaps are in what feeds it: speed modifiers for water and slopes, which objects block, yardmap orientation, and two script/gadget behaviours (the morph swap, the Signal's flight constants).

## Findings (most important first)

### 1. Water squares carry no speed penalty (DigitalDivide_PT2) — severity: high · confidence: verified
- **Original:** every ground speed modifier, both the driving one and the one QTPFS uses, multiplies a square whose highest corner is below 0 by the water-damage cost and by the depth modifier:
  - `Sim/MoveTypes/MoveMath/GroundMoveMath.cpp:25-26` and `:49-50`: `speedMod *= ((height < 0.0f)? waterDamageCost: 1.0f); speedMod *= moveDef.GetDepthMod(height);`
  - `height` is `maxHeightMap`, the highest of the square's four corners: `MoveMath.cpp:91-93`, `Map/ReadMap.cpp:635`.
  - `waterDamageCost = 1/(1 + water.damage·0.1)` (`MoveDefHandler.cpp:159-160`). `water.damage` is stored per slow update: `MapInfo.cpp:236` multiplies the map value by 15/30.
  - `GetDepthMod` = `1/clamp(1 + 0.1·depth, 0.01, ∞)` (`MoveDefHandler.cpp:741-763`). The 0.1 is the KBot/Tank default `linearCoeff` (`:222-227`); `MOVEINFO.TDF` sets no depth parameters.
  - QTPFS builds its node costs from the same `GetPosSpeedMod` (`Sim/Path/QTPFS/NodeLayer.cpp:190-192`).
- **Remake:** the driving modifier is slope × terrain type only (`kernel-panic/src/interaction/ground_move.rs:2154-2194`). The pathfinder's `SpeedMap` is the same product (`spring-pathfinding/src/cost.rs` `from_slopes`/`update_region`; `kernel-panic/src/terrain/terrain_speed.rs:72-88`; `interaction/movement.rs:318-337`). Nothing in pathing or driving reads height < 0. Water exists in the remake only as damage (`map_events/water.rs:26`).
- **In play:**
  - On DigitalDivide_PT2 (water damage 50/s), 27.4% of the squares are under water. The water is at most 7.1 elmos deep, and every water square can be reached from land over passable slopes (I checked the shipped heightmap).
  - In KP a unit in that water moves at 0.29 × 0.59..1 ≈ **17–29% speed**, and QTPFS costs water 3.5–6× per elmo, so paths go round it.
  - In the remake units take the straight line through the water at full speed, taking 25 HP every half second.
  - Speed_Balls_16_Way also has water (61% of squares), but cliffs ring it, so it cannot be reached in either game.
- **Fix:**
  - Fold `waterDamageCost` (from the map's water damage) and `1/(1 + 0.1·depth)` into both the `SpeedMap` build and `ground_speed_mod`, for squares whose highest corner is below 0.

### 2. Exploits made by the Bug's deploy block movement; in KP they are non-blocking — severity: high · confidence: verified
- **Original:**
  - `bug.bos:46` calls `lua_SwitchToExploit`, which queues the replace in `LuaRules/Gadgets/LuaCOB.lua:85-87`.
  - `LuaCOB.lua:37-42` then creates the Exploit and calls `Spring.SetUnitBlocking(nu, false)`, which UnBlocks it: `Lua/LuaSyncedCtrl.cpp:633-638`.
  - An unblocked object is skipped by every movement test. `CMoveMath::IsNonBlocking`: `if (!collidee->IsBlocking()) return true;` (`MoveMath.cpp:231`).
    - Unit collisions skip it (`GroundMoveType.cpp:2835`).
    - QTPFS and `UpdatePos` read the blocking map, which no longer contains it.
  - Nothing re-blocks a static unit (`StaticMoveType.cpp:31-47`).
  - So units, friend or foe, walk through deployed Exploits.
- **Remake:**
  - `units/mechanics/deploy.rs:145-160` spawns the Exploit with a plain `spawn_unit`.
  - `interaction/structures.rs:445-457` stamps every immobile unit into the structure layer, so the Exploit's `oo oo` footprint closes path nodes and pushes movers (`ground_move.rs:2636-2647`).
- **In play:** a line of deployed Exploits in a lane (common Hacker play on Dual/Quad Core) is a wall in the remake: your own units detour and enemies cannot pass. In KP they walk straight through.
- **Fix:**
  - Spawn morph results without a `StructureStamped` stamp, and skip them in the collision pass.
  - A Bug made by packing up re-blocks once it moves a square (`MoveType.cpp:104-115`). It is already a mover in the remake.

### 3. Slopes slow units by an extra cos(pitch) — severity: medium · confidence: verified
- **Original:** the engine regulates the *horizontal* speed.
  - `currentSpeed` is set from `newSpeedVector.dot(flatFrontDir)`: `GroundMoveType.cpp:3509-3510`, applied by `UpdateOwnerSpeed` via `:675` and `:3602`.
  - The next frame's vector is built from the 3D magnitude `owner->speed.w` plus the delta: `:447`, `:463` `return owner->frontdir * (vel + hAcc)`.
  - `ChangeSpeed` compares the target with the horizontal `currentSpeed` (`:1408`).
  - Uphill, the 3D speed therefore settles at target/cos φ, so the ground speed equals the target.
  - Downhill, the rolling-resistance branch above `maxSpeed` (`:447-461`) settles the ground speed at maxSpeed·cos φ/(0.05 + cos φ), about 0.93–0.95 × maxSpeed.
- **Remake:**
  - `ground_move.rs:1428-1456` builds `velocity = tilted front · new_speed` and stores `current_speed = new_speed`, the 3D magnitude, so the 3D speed is what gets regulated.
  - The test at `ground_move.rs:3266-3291` pins `v.length() == current_speed`.
- **In play:** ground speed on ramps is lower than in KP.
  - Uphill it is cos φ of KP's: −13% at 30°, −29% at 45°, −36% at 50°.
  - Downhill it is (0.05 + cos φ) of KP's: −8% at 30°, −24% at 45°.
  - 13% of Data_Cache_L1 and about 13% of Major_Madness are 20–54° ramps.
- **Fix:**
  - Set `current_speed` to the horizontal component (`new_speed · |front.xz|`, i.e. `velocity.xz()·front`).
  - Feed the next frame from the 3D length, with the existing over-max drag branch keyed on the 3D speed.

### 4. Signal flies with the wrong acceleration and turn radius — severity: medium · confidence: verified
- **Original:**
  - The Signal (`canFly`, no `hoverAttack`, Weapon1 `SigTerm` is an `AircraftBomb`, so it counts as an aircraft weapon: `WeaponDef.cpp:533-539`, `WeaponDef.h:30-36`) is a bomber, `IsBomberAirUnit` (`UnitDef.h:76-79`).
  - For bombers `UnitDef.cpp:640-641` overrides both values:
    - `turnRadius *= 2` while it is the default 500, so 1000;
    - `maxAcc = udTable.GetFloat("maxAcc", 0.065f)`, which ignores `Acceleration=2`.
  - So `accRate` ≈ 0.065·(0.99..1.01) (`AAirMoveType.cpp:107`). Drag is `0.065/(8·1.1) − wingAngle²·wingDrag` (`StrafeAirMoveType.cpp:1320`), giving a top speed of about 8.7 elmos/frame (262/s) that it approaches over ~4.5 s.
  - Take-off climbs at 0.065/frame² too (`:901-941`).
- **Remake:** `units/mechanics/command_fire.rs:257-266` uses `SIGTERM_ACC = 2.0` ("signal.fbi `Acceleration=2`"), which gives a top speed of 6.8 reached within a few frames, and `SIGTERM_TURN_RADIUS = 500`.
- **In play:**
  - The straight-run impact time after the cast, KP vs remake, is about:
    - 8.5 s vs 4.5 s at 800 elmos;
    - 11.6 s vs 7.9 s at 1500;
    - 15.7 s vs 12.8 s at 2500.
  - So targets get 3–4 s less warning in the remake.
  - The bomb leaves at a different speed (5.8–8.6 instead of 6.8), and the go-round decisions use half the engine's turn radius.
- **Fix:** acc 0.065, drag from that value, turn radius 1000 (`nearGoal` uses 500/1000).

### 5. Bug↔Exploit deploy is instant; KP holds the unit about 2.5 s — severity: medium · confidence: verified
- **Original:**
  - `CMD_DEPLOY` runs COB `DeployNoTarget` (`LuaRules/Gadgets/specialattack.lua:53-65, 169`).
  - `bug.bos:20-46` `Deploy()` waits for `morphable` (2 s after the build). It then sets `MAX_SPEED to 1`; 1/65536 is clamped to 0.001 elmos/frame (`UnitScript.cpp:1664-1666`, `MoveType.h:49`).
  - It then plays about 0.6 s of body moves and a 67 × 30 ms fade, and only then swaps (next GameFrame).
  - `exploit.bos:18-45` `Undeploy` waits for idle, plays the 2 s fade plus body moves, then swaps.
- **Remake:** `net/apply.rs:766-776` writes `DeployEvent` at once, and `units/mechanics/deploy.rs:110-160` swaps in the same tick.
- **In play:**
  - A Bug turns into an Exploit, or back, instantly.
  - In KP it sits still and vulnerable for ~2.5 s, and an Exploit under fire cannot just pop out and run.
- **Fix:** hold the unit (zero max speed) for the scripted duration, then swap.

### 6. Asymmetric factory yardmaps are stamped unrotated — severity: medium · confidence: verified
- **Original:**
  - `CSolidObject::GetGroundBlockingMaskAtPos` (`Sim/Objects/SolidObject.cpp:276-306`) rotates the yardmap by `buildFacing` (`front = rotationDirs[buildFacing]`). `AddGroundBlockingObject` uses it per square (`Sim/Misc/GroundBlockingObjectMap.cpp:82-102`).
  - Socket `yardmap=gccg gccg gccg gccg` and Window `gggg cccc cccc cccc` (`units/socket.fbi:37`, `units/window.fbi:37`) therefore keep their solid `g` walls at the sides (Socket) or at the back (Window) whichever way they face.
- **Remake:**
  - `interaction/structures.rs:289-316` `squares_of` indexes `(sx/2) + (sz/2)·fx` with no facing.
  - Minifacs on datavents spawn facing the vent (`units/lifecycle/construction.rs:721-728`), which is mostly not south.
- **In play:**
  - An east- or west-facing Socket has its open corridor running side to side instead of front to back.
  - A north-facing Window has its wall in front. Units built there leave through the back or sides and loop round to the exit point, and passing traffic cuts through the wrong side.
- **Fix:** rotate the yardmap index by the building's facing (same mapping as `GetGroundBlockingMaskAtPos`, including the rotation corrections).

### 7. Flow speed bonus: the deliberate deviation rests on a wrong premise — severity: low · confidence: verified
- **Original:**
  - `network_flowspeed.lua:31` sets COB `MAX_SPEED` (variable 75) to `65536·(30 + 30n)/30`, i.e. `SetMaxSpeed(1+n)` elmos/frame (`UnitScript.cpp:1664-1666`). It is not clamped (`MoveType.h:49`).
  - The hover move type flies at `min(goalSpeed, maxWantedSpeed)` (`HoverAirMoveType.cpp:552-565`).
  - `maxWantedSpeed` starts at the def speed (`MoveType.cpp:74`). Only plain player orders raise it, to the current `maxSpeed`: `Game/SelectedUnitsAI.cpp:52`, `:120`, `:257`.
  - Orders that never raise it:
    - CustomFormations2 formation orders (`spGiveOrderToUnit`, `LuaUI/Widgets/unit_customformations2.lua:619`, which goes through `AINetOrder`, `SelectedUnitsHandler.cpp:791`);
    - gadget/AI orders (`LuaSyncedCtrl.cpp:5932`);
    - factory rally orders.
  - So the bonus applies only to a right-click, frozen at the speed of that moment; fresh, formation-dragged and AI Flows fly at 30 elmos/s.
- **Remake:** `interaction/air_movement.rs:485-495, 813` always uses the current base+bonus. plan.md §3.8 justifies this with "Recoil clamps COB `MAX_SPEED` raises at the def speed", which the engine does not do.
- **In play:** with 3 ports a just-built Flow heading to its rally point flies at 120 elmos/s instead of 30, and a Flow speeds up mid-flight when a port finishes.
- **Fix:** keep the choice if intended, but correct the stated reason. Otherwise latch a wanted speed at plain player orders.

### 8. The goal-blocked test checks only the centre square's terrain — severity: low · confidence: verified
- **Original:**
  - `StartMoving` sets `extraRadius = deltaRadius·(1 − TestMoveSquare(q, goalPos, 0, true, true))` (`GroundMoveType.cpp:980`).
  - `centerOnly` is left at its default `false` (`MoveDefHandler.h:72-83`), so terrain is tested on every footprint square (`MoveDefHandler.cpp:636-664`).
- **Remake:** `ground_move.rs:1737` and `:2210` pass `!map.square_open(g)`, which tests terrain at the centre only (`:1165-1169`).
- **In play:** a goal within 1–3 squares of a cliff edge gets no `extraRadius` (9 / 31.6 elmos). Remake units drive right to the edge point; KP units stop ownerRadius short.
- **Fix:** test terrain over the whole `xsizeh` window (step 1) for the goal.

### 9. A failed move ends the order immediately; KP waits for the next CAI SlowUpdate — severity: low · confidence: verified
- **Original:**
  - `Arrived` calls `owner->commandAI->SlowUpdate()` (`GroundMoveType.cpp:2479`); `Fail` does not (`:2489-2504`).
  - The CAI runs before the move type each slow update (`Unit.cpp:1059-1061`) and finishes the failed command at its next one (`MobileCAI.cpp:406-415`), 1–15 frames later.
- **Remake:** `ground_move.rs:1980-1991` and `:2774-2778` end the leg at once. The comment claims "`Arrived` / `Fail` run the CAI's SlowUpdate right away".
- **In play:** the next queued command after an unreachable move starts up to 0.5 s earlier.
- **Fix:** defer the failed-leg `end_move_leg` to the unit's next slow update.

### 10. `Sign(0)` is +1 in the port, −1 in the engine — severity: low · confidence: verified
- **Original:** `System/SpringMath.h:182-183` (`(v > 0)·2 − 1`, "zero is negative!").
- **Remake:** `ground_move.rs:992-995` (`x >= 0 → 1`).
- **In play:** exact ties are mirrored:
  - the push slide sign (engine `GroundMoveType.cpp:2995`, remake `:2344`);
  - the avoidance turn signs (`:1957`, remake `:2114-2115`);
  - the HSOC strafe/bounce signs (`:2735-2736`, remake `:2860-2861`).
  So a perfectly aligned head-on pair dodges the other way.
- **Fix:** use `if x > 0 { 1 } else { -1 }` in those places.

## Doc drift
- plan.md "Ground movement / Deviations": "Path searches run synchronously (2 ms budget/frame…)". The code uses a 6000-node budget per frame and resumes deferred searches (`ground_move.rs:102`, `PathQueue::service`).
- plan.md: "Path heat (`PathHeat`) is kept although QTPFS ignores it". No PathHeat remains in the code.
- plan.md Technical Debt "QTPFS terrain-change repathing… We don't re-pave on edits". Node layers are updated per changed rectangle (`ground_move.rs:221-270`), and dirtied paths re-search after 30 frames (`:1784-1837`).
- movement-parity-plan.md status line: "path sharing / exit-only / path repair were left out". Sharing and repair are implemented (`ground_move.rs:293-358, 388-398`), as the reference doc says.
- plan.md §3.8 "(Recoil clamps COB `MAX_SPEED` raises at the def speed)". This is wrong (finding 7).
- `units/mechanics/network_buffer.rs:320-323` "Upstream caps Flow at MAX_SPEED=75". In `network_flowspeed.lua:19` the 75 is the COB variable index, not a cap. It is harmless, because the cap needs 74 buildings.
- `command_fire.rs:257` "Thrust: signal.fbi `Acceleration=2`". Bombers ignore that tag (finding 4).
- `ground_move.rs:1980` comment: only `Arrived` runs the CAI at once (finding 9).
- `unit_registry.rs:404-418` "Until we parse MOVEINFO.TDF…". MOVEINFO is parsed (`moveinfo.rs`), but its MaxSlope is unused. That is equivalent for KP, where all classes are 36.
- plan.md "Factory yards: Spring opens them only while the factory builds (`YARD_OPEN` in the scripts)". The engine opens any factory's yard itself while it has something to build and closes it 7 s after the last build (`Sim/Units/UnitTypes/Factory.cpp:138-166`). Only kernel.bos and socket.bos touch `YARD_OPEN`. The conclusion ("rarely differs") still holds.
- spring-movement-reference.md "Known remaining differences": accurate for what it lists, but it omits findings 1–6.

## Checked and matching (one line each, so coverage is visible)
- MOVEINFO classes:
  - KBot speed class by name rule;
  - LIGHT xsize 3/xsizeh 1/r 12/owner 16.97/crush 40;
  - MEDIUM and HEAVY 7/3/28/39.6, crush 60/300;
  - maxSlope 1−cos 54°, slopeMod 9.68;
  - MaxWaterDepth 25000, so the depth limit is never hit;
  - the MoveDef footprint overrides the FBI footprint.
- Per-unit constants: speed = MaxVelocity·30; accRate = max(0.01, Acceleration, default 0.5); decRate = BrakeRate (default accRate); turnRate clamped to [1, 32767]; turnAccel 0.333·turnRate; mass = metal cost. No KP default unit sets TurnInPlace*, Upright, MaxSlope (mobile), rSpeed or Mass (except the Signal).
- Turn-in-place (default on, limit 0): `targetSpeed = maxSpeed·clamp(maxTurn/reqTurn, 0.1, 1)`. The end-of-path cap and `limitSpeedForTurning` match `ChangeSpeed`.
- `GetDeltaHeading` with inertia and short casts; `ChangeHeading` is skipped when stopping or stunned.
- `WAIT_FOR_PATH=0`: the unit drives at a temporary waypoint one square toward the goal (`QTPFS PathManager.cpp:1754-1757`).
- FollowPath: atGoal tests, idle counters, tolerance `(r+extra)·(n+1)` for move/fight. `UNIT_HAS_MOVE_CMD` includes FIGHT.
- CanSetNextWayPoint: allowSkip < 7 elmos, turn-diameter test, 0.995 straight-ahead test, raw search to the next waypoint, lastWaypoint/partial-path fail.
- SlowUpdate: idle repath at `numIdlingUpdates > 32768/turnRate`, give-up at 16 idle slow updates, repath delay 60 and max rate 150 frames, last-waypoint re-attempt with /8.
- UpdatePos: centre-square terrain plus footprint objects (step 2); ±1..8 sideways tries; diagonal corner test; axis-aligned slide by facing; positionStuck pass-through.
- Obstacle avoidance: every 3rd frame; weights 1 / 0.5 / 0.7, where the "last direction" is the desired one, as in the engine; 120° cone; idle allies skipped; 3D prediction.
- CalculatePushVector with UCO=1. allowPushingEnemyUnits=1, so enemies and allies are pushed alike. maxCollisionPushMultiplier = ∞, so there is no clamp. Pushes are gated through UpdatePos.
- HandleUnitCollisionsAux (Done/Active branches, 1.45 diagonal factor). Being-built units are never pushed and skip their own pass.
- HSOC yardmap branch: bounce plus strafe, `min(maxSpeed, max(0.1, −pen/2))`, limitSpeedForTurning 2. The push-out term is dead code except within ~6 squares of map (0,0).
- Bad Block crushing: feature crushResistance = 0.4·64 + 0.1·400 = 65.6. Only HEAVY (300) crushes. Features are crushable, units never are, so allowCrushingAlliedUnits does not matter. Crushable features stay open for HEAVY in QTPFS.
- Terrain-type kbot speed in both driving and pathing. QTPFS clamps it to [0,2]. Driving uses `wanted×max(mod,1)` and the rolling-resistance drag above maxSpeed.
- Directional slope mod is uphill-only, with a half-resolution slope map. The zero-speed fallback looks one square ahead.
- QTPFS search budget `max(8192, 0.25·open)/2` and the SetNodeSearchLimit ease-out.
- Move goals clamped to [0, (mapx−1)·8]. Goal radius 8 + extraRadius. cancelDistance = clamp((staticTurnRadius+16)², 1024, 2048). StopMoving keeps momentum and brakes at decRate.
- Flow hover:
  - GetGoalRadius 64;
  - wantedHeight cruiseAlt + rand·5;
  - take-off to 0.8 of that height;
  - UpdateVerticalSpeed;
  - dontLand (AirHoverFactor 0);
  - smooth-mesh / above-water height functions;
  - turn 1280 units/frame;
  - acc 0.3, dec 0.9.
  It uses CMobileCAI like the engine (`UnitLoader.cpp:62-64`).
- The Worm halves MAX_SPEED while aiming and restores it after ResetAim (`worm.bos:163,150`; `units/mechanics/worm.rs`).
- Signal drop window `max(1, salvoSize−1)·salvoDelay·v·0.5` = 9·v; cruise height cruiseAlt·1.5; it spawns facing the Terminal's facing.
- slideTolerance 0, so there is never slope sliding. allowGroundUnitGravity 0 (`CalcSpeedVectorExclGravity`). No KP weapon has impulse.
- Yardmap codes used by default units (o, c, g, y) map to blocked / yard / blocked / open.

## Unverified leads
- **Idle auto-chase (for the combat/CAI auditor).** `CMobileCAI::GenerateAttackCmd` (`MobileCAI.cpp:1196-1262`, `AUTO_GENERATE_ATTACK_ORDERS=1`) makes idle units take a 5 s temporary attack on enemies within `maxRange + 200·moveState²` and drive to them. The move state is maneuver, inherited from the factory (`Unit.cpp:349-361`), and `pointer.bos:158` sets hold position. I found no equivalent in `units/combat/` by grep; it needs a proper look.
- **CTRL group-speed and group-locked moves** (`Game/SelectedUnitsAI.cpp:150-240`) do not appear in `interaction/selection/right_click.rs`. They are an input feature, but they set the movers' wanted speed.
- **The SlowUpdate out-of-bounds clamp** (`GroundMoveType.cpp:891-892`) is not ported. It is reachable only through positionStuck steps or skids.
- **`RangeIsBlocked` near the map's low edges.** The engine clamps `xmin` to 0 before stepping by 2 (`MoveMath.cpp` `RangeIsBlocked`), which shifts the sampled-square parity there. `structures.rs:119-127` does not clamp first. It only matters for structures at the map edge.
- **Hover `flying_base`** uses the exact height (`hm.sample`) where the engine uses `GetApproximateHeight` (`AAirMoveType.cpp:59`). Probably negligible.

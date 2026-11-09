# Sensors, line of sight, fog, cloak and detection: parity audit

Paths: engine = `upstream/RecoilEngine/rts/`, mod = `upstream/Kernel-Panic/`, remake = `kernel-panic/src/`.

What the engine actually resolves for KP (it sets the numbers used below):
- The LOS grid uses 16-elmo squares, the air-LOS grid 16 and radar/seismic 32. KP's `gamedata/sensors.tdf` (`LosMipLevel=2`) never takes effect; see finding 11.
- Only active units have radar: Assembler, Trojan and Gateway from the moment they finish, the Worm 5 s after it finishes. The Byte's and Connection's `RadarDistance=200` does nothing (finding 2).
- Recoil leaves the cloak decision to the Lua callin `AllowUnitCloak`, and KP defines none. So `isCloaked = wantCloak`: there is no cloak cost, `MinCloakDistance` is ignored, and nothing nearby can force a decloak.

What each visibility state allows in the engine:
- **LOS**: the model is drawn and the unit can be targeted at its true `aimPos`.
- **Radar only**: an icon is drawn at a position that drifts up to 96 elmos off; the unit can be auto-targeted at that position but with 10× worse priority, and an attack order stays alive.
- **`PREVLOS` + `leavesGhost`** (buildings): a ghost is drawn and shown on the minimap, but it is not targetable.
- **Seismic**: a ground ping only.
- **None**: nothing is drawn, nothing can target it, and an attack order on a mobile unit ends after 4 slow updates.

The remake has only "in a 3D sight sphere → drawn" and "cloaked, inside a `RadarDistance` sphere → fully revealed". Targeting ignores both.

## Findings (most important first)

### 1. Auto-targeting and attack orders ignore line of sight and radar for uncloaked enemies — severity: high · confidence: verified
- **Original:**
  - `CWeapon::TestTarget` refuses any unit target its allyteam neither sees nor has on radar: `if ((trg.unit->losStatus[owner->allyteam] & (LOS_INLOS | LOS_INRADAR)) == 0) return false;` (`Sim/Weapons/Weapon.cpp:1039-1040`). The check runs again before every shot (`TryTarget`, `Weapon.cpp:475`).
  - `CGameHelper::GenerateWeaponTargets` drops every other unit: `if (targetLOSState & LOS_INLOS) {…} else if (targetLOSState & LOS_INRADAR) {…} else { continue; }` (`Game/GameHelper.cpp:719-726`).
  - An attack order on a mobile target ends 4 slow updates after the target leaves LOS and radar: `static const int TARGET_LOST_TIMER = 4;` (`Sim/Units/CommandAI/CommandAI.cpp:48`), `UpdateTargetLostTimer` (`CommandAI.cpp:1469-1485`), then `StopMoveAndFinishCommand` (`MobileCAI.cpp:951`). Until then the chase heads for `GetErrorPos` (`MobileCAI.cpp:715`). Orders on immobile targets never time out (`CommandAI.cpp:1478-1479`).
- **Remake:**
  - The only auto-target gate is `targetable_by` = `!self.feature && !hidden_from(self.cloaked, self.detected_by, team)` (`units/spatial.rs:89-91`, applied at `units/combat/mod.rs:1145`). The module says so itself: "The fog only affects rendering" (`units/mechanics/cloak.rs:32-33`).
  - Shots aim at the target's exact `candidate.mid_pos()` (`combat/mod.rs:1159`).
  - `attack_target_system` chases through `target_q: Query<(&GlobalTransform, Option<&UnitStats>), Without<Dying>>` with no visibility check (`combat/mod.rs:1765, 1806`).
  - Its `TargetLost` timer only covers an *immobile attacker out of range* (`combat/mod.rs:88-102, 1848-1862`). That is not the engine rule, and it counts "slow updates of 16 frames" where the engine's are 15 (`Sim/Misc/GlobalConstants.h:60`).
- **In play:**
  - A deployed Pointer (Geometric range 1400, sight 768) and an Exploit (BugCannon range 1200, sight 300, so 288 in the engine) fire at anything in range through the fog. The Exploit even picks the farthest target. In KP they need a spotter.
  - Ballistic Exploit shells arc over ridges onto units nobody sees.
  - Units ordered to attack a fleeing unit track it perfectly through the fog until it dies. In the original they give up about 2 s after losing sight and radar.
- **Fix:** keep a per-allyteam "in LOS or radar" set (the fog scan, run for every team) and require it in `targetable_by`, `visible_pos` and the attack-order chase. Port `UpdateTargetLostTimer` for mobile targets.

### 2. Radar is not modelled: "detectors" fully reveal cloaked units, the wrong units detect, and enemies on radar show nothing — severity: high · confidence: verified
- **Original, what radar shows:**
  - A cloaked enemy is never in LOS: `if (unit->isCloaked && unit->allyteam != allyTeam) return false;` (`Sim/Misc/LosHandler.cpp:879-883`). Its model is never drawn for enemies.
  - Radar shows every non-stealth unit, cloaked or not: `if (unit->stealth && !unit->beingBuilt) return false; return (radar.InSight(unit->pos, allyTeam) && !InJammer(unit, allyTeam));` (`LosHandler.cpp:967-970`). The Logic Bomb is not stealth (`units/logic_bomb.fbi:39` `//	Stealth=1;`).
  - A radar contact is drawn as an icon (`else if ((losStatus & LOS_INRADAR) != 0) unit->currentIconIndex = …GetDefaultIconIdx();`, `Rendering/Units/UnitDrawerData.cpp:285`) at the drifting error position, on the minimap too (`GetObjDrawErrorPos`, `Rendering/Units/UnitDrawer.cpp:448`). Minimap clicks accept LOS or radar contacts (`Game/UI/MiniMap.cpp:862`).
- **Original, how radar contacts are targeted:**
  - The aim point is `GetUnitPositionWithError` (`GameHelper.cpp:722`). The error is `posErrorVector * GetAllyTeamRadarErrorSize` (`Sim/Units/Unit.cpp:571`), with `defBaseRadarErrorSize = 96.0f` (`LosHandler.h:319`): up to 96 elmos, drifting.
  - Priority is 10× worse: `targetPriority *= tgtPriorityMults[1]` (`GameHelper.cpp:723`, table at line 685).
- **Original, who has radar:**
  - A unit needs to be built, activated and not stunned: `if (unit->isDead || unit->beingBuilt) return;` and `const bool noSensors = (!unit->activated || unit->IsStunned()); if (!sightOnly && noSensors) { RemoveUnit(unit); …}` (`LosHandler.cpp:173, 182-186`).
  - `activateWhenBuilt` defaults to false (`Sim/Units/UnitDef.cpp:668`). Only `scripts/assembler.bos:15`, `trojan.bos:22`, `gateway.bos:18` and `worm.bos:110` (5 s after completion) do `set ACTIVATION to 1`. `byte.bos` and `connection.bos` never do, and neither unit is `onoffable`.
  - Radar is raycast against terrain (`LosHandler.cpp:92`) on 32-elmo squares (`radarMipLevel = 2`, `ModInfo.cpp:119`; `(radarRadius / SQUARE_SIZE) >> mipLevel`, `LosHandler.cpp:141`). Assembler and Gateway: 500 → 480 elmos.
- **Original, design intent in the readme:** the Assembler is "Equipped with a radar to detect mines and other cloacked units" (`Kernel_Panic_readme.txt:443`), the Trojan is a "radar platform (needed for detecting mines and cloaked units)" (line 470), and Connections "have proper inaccuracy vs cloaked targets" (line 829).
- **Remake:**
  - Every unit with `RadarDistance > 0` gets a `Detector` at spawn (`units/lifecycle/bookkeeping.rs:104-106`): Byte, Connection and Worm included, nanoframes included. The scan's only filter is `Without<Dying>` (`cloak.rs:207`), so stunned detectors keep working.
  - A cloaked enemy inside the detector's 3D radius (`cloak.rs:229-234`) is set `Visibility::Visible` (`cloak.rs:273-276`) at full opacity, because enemies skip the fade (`cloak.rs:333-335`).
  - That unit is then auto-targeted at its exact midpoint with normal priority (`combat/mod.rs:1145, 1159`).
  - Uncloaked enemies out of sight are simply hidden (`cloak.rs:466-473`). There are no radar icons anywhere, and the minimap only draws `Spotted` units (`ui/minimap.rs:182`).
- **In play:**
  - One Byte or Connection in an army turns every Logic Bomb and buried Worm within 200 elmos into a visible model that Bits snipe precisely.
  - In the original, mines near an active Assembler, Trojan or Gateway show only as wandering dots. Bits fire at a point up to ~96 elmos off and rarely hit a 1×1 mine, so mines stay a threat (Debug and area damage clear them).
  - A DOS stun on the constructor switches its radar off in the original.
  - A Trojan (sight 384, radar 768) shows enemy movement out to 768 as dots in the original. The remake shows nothing beyond 384.
- **Fix:** add per-allyteam radar coverage from active units only (Assembler, Trojan and Gateway once finished, the Worm 5 s after it finishes; none while under construction or stunned), using 32-elmo squares. Draw icons, not models, for enemies in coverage, at a drifting offset of up to 96 elmos. Let weapons target those icons at the offset, with lower priority than units in sight.

### 3. Enemy units under construction are invisible even in plain sight, while your own nanoframes see and detect — severity: medium · confidence: verified
- **Original:**
  - A nanoframe is an ordinary unit for LOS: `CLosHandler::InLos(const CUnit*)` has no `beingBuilt` test (`LosHandler.cpp:867-901`). Enemies who see it get a minimap icon, a tooltip and the model rising or fading in as KP's `Create()` scripts drive it.
  - A nanoframe itself has no sensors: `if (unit->isDead || unit->beingBuilt) return;` (`LosHandler.cpp:173`), and its LOS radius is only set in `FinishedBuilding` (`ChangeLos(realLosRadius, realAirLosRadius);`, `Unit.cpp:427`).
- **Remake:**
  - The spatial index drops `Emerging` units (`(Without<Dying>, Without<Emerging>)`, `units/spatial.rs:200-207`). This is documented, but only as protection from targeting.
  - `update_fog_visibility` finds enemies in sight only through that index (`cloak.rs:428-442`), yet hides every enemy in `targets_q`, which has no `Emerging` filter (`cloak.rs:386-395, 466-473`). So an enemy nanoframe is always `Hidden` and never `Spotted`.
  - In the other direction, `viewers_q` (`cloak.rs:385`) and `detectors_q` (`cloak.rs:207`) include your own nanoframes. Factories spawn the buildee when production starts (`units/lifecycle/production.rs:726`).
- **In play:**
  - An enemy Assembler builds a Socket in front of your army for ~30 s (`BuildTime=3840`, `WorkerTime=128`) and you see only the Assembler; units in enemy factories appear only once finished.
  - Starting your own Socket on a distant datavent gives you its 512-elmo view at once. In the original you get nothing until it finishes.
- **Fix:** have the fog scan include `Emerging` enemies (a separate query, or an index flag the targeting code ignores), and drop `Emerging` units from `viewers_q` and `detectors_q`.

### 4. Minimap keeps tracking re-buried enemy Worms and never shows your own Logic Bombs — severity: medium · confidence: verified
- **Original:** minimap icons use `currentIconIndex` (`UnitDrawer.cpp:400-449`). It is always valid for your own and allied units. For enemies it is valid only in LOS, on radar, or as a ghost building (`UnitDrawerData.cpp:263-288`), so a buried Worm that is not on your radar has no icon.
- **Remake:**
  - The minimap draws `Query<…, With<Spotted>>` (`ui/minimap.rs:182`).
  - `Spotted` is inserted and removed only in `update_fog_visibility`, whose query is `Without<Cloaked>` (`cloak.rs:386-395, 444-474`).
  - Nothing removes `Spotted` when `Cloaked` comes back (`worm.rs:138` `.insert(Cloaked)`), and nothing adds it to a unit that is cloaked from spawn (`spawning/mod.rs:536-539`).
- **In play:**
  - An enemy Worm that surfaced once in your sight keeps a live dot on your minimap after it buries itself, so you can follow it around the map.
  - Your own Logic Bombs never appear on your minimap, and a new Worm appears only after its first bite. Spectators lose all of them too.
- **Fix:** have `update_cloak_visibility` set or clear `Spotted` together with `Visibility` (friendly or detected → on, else off).

### 5. Aircraft are spotted at plain sight radius measured in 3D instead of 1.5× sight in 2D — severity: medium · confidence: verified
- **Original:**
  - Airborne units are tested against the enemy's air-LOS map: `owner->useAirLos = (newState != AIRCRAFT_LANDED);` (`Sim/MoveTypes/HoverAirMoveType.cpp:184` for the Flow, `HoverAttack=1`; `StrafeAirMoveType.cpp:1217` for the Signal), and `if (unit->useAirLos) return (InAirLos(unit->pos, allyTeam) || InAirLos(unit->pos + unit->speed, allyTeam));` (`LosHandler.cpp:892-893`).
  - Every unit's air sight is `airLosRadius = udTable.GetFloat("airSightDistance", 1.5f * losRadius);` (`UnitDef.cpp:481`), and no KP unit file sets `airSightDistance`.
  - The air-LOS map is a flat 2D circle (`LOS_ALGO_CIRCLE`, `LosHandler.cpp:92`), so terrain doesn't block it.
- **Remake:** one radius for every target, the viewer's `SightDistance`, compared as a 3D distance: `candidate.pos.distance_squared(vp) <= sight * sight` (`cloak.rs:433-437`). Aircraft fly at `cruise_alt` (`spawning/mod.rs:357`, `interaction/air_movement.rs:138`): Flow 140, Signal 200.
- **In play:**

  | Viewer | Target | Original (horizontal) | Remake (horizontal) |
  |---|---|---|---|
  | Bit (sight 512) | Flow | 768 elmos | ~490 (√(512²−140²)) |
  | Bit (sight 512) | Signal | 768 | ~471 |
  | Pointer (sight 768) | Flow | 1152 | ~755 |

  Air raids arrive with about 40% less warning.
- **Fix:** test flying targets with 2D distance against 1.5× the viewer's sight, and use 2D distance for all sight tests.

### 6. Line of sight ignores terrain — severity: medium · confidence: verified (how much it matters depends on the map)
- **Original:**
  - LOS and radar are raycast over the heightmap: `algoType = ((type == LOS_TYPE_LOS || type == LOS_TYPE_RADAR) ? LOS_ALGO_RAYCAST : LOS_ALGO_CIRCLE);` (`LosHandler.cpp:92`).
  - Rays start at `std::max(unit->midPos.y + emitHeight, 0.0f)` (`LosHandler.cpp:160`), where `losEmitHeight` defaults to 20 (`UnitDef.cpp:477`).
  - A square is lit only if its angle `(height + LOS_BONUS_HEIGHT − emitHeight) / dist` (`LosMap.cpp:16, 654`; `LOS_BONUS_HEIGHT = 5`) is not below the steepest one before it on the ray (`CastLos`, `LosMap.cpp:525-550`).
  - A unit is seen when the square under it is lit (`LosHandler.cpp:901`).
- **Remake:** one 3D sphere per viewer, no heightmap involved (`cloak.rs:428-442`). FEATURES.md §24 describes LOS as plain circles.
- **In play:** KP maps are terraced. Heights from the original SMF files (via `git show a8567de~1:`):
  - Dual Core, Quad Core and Corrupted Core have tiers about 90, 190, 240 and 310 elmos above the lowest ground, with near-vertical steps up to 398 elmos between neighbouring 8-elmo vertices.
  - Major Madness has tiers at about +90, +140 and +240.
  - Data Cache L1 and Direct Memory Access have cliffs of 279 and 429 elmos.
  - Hex Farm 8 and Palladium are flat.

  Example: a Bit 200 elmos from the foot of a 100-elmo tier sees only the first ~16 elmos of the top. Units on top have a blind band at the foot of the cliff. The remake shows everything inside the circle.
- **Fix:** keep a per-allyteam 16-elmo LOS grid filled by the engine's raycast (emit height `midPos.y + 20`, +5 bonus), and test the square under each target.

### 7. The Worm is cloaked, and its sensors are on, from the moment production starts — severity: medium · confidence: verified
- **Original:**
  - `worm.fbi` has no `Init_Cloaked`. The cloak comes from `worm.bos` `Create()`: wait for `BUILD_PERCENT_LEFT` to reach 0 (line 101), `sleep 5000` (108), `set ACTIVATION to 1` (110), lower the head for `sleep 1333` (115), then `set WANT_CLOAK to TRUE` (116).
  - `isCloaked` follows at the next slow update (`const bool newCloak = wantCloak && GetNewCloakState(stunCheck);` and `isCloaked = newCloak;`, `Unit.cpp:2525, 2535`, called from `SlowUpdate` at 1102). While `beingBuilt`, the slow update calls `ScriptDecloak(nullptr, nullptr)` instead (`Unit.cpp:1054`). The same applies to a Logic Bomb under construction.
  - In those first seconds, AutoHold puts the Worm on fire-at-will: `if(!get CLOAKED) set STANDINGFIREORDERS to 2;` (`worm.bos:77-78`).
  - Radar (256) and seismic (1024) wait for `activated` (`LosHandler.cpp:182-186`), i.e. 5 s after completion.
- **Remake:**
  - `if kind == UnitKind::Worm || unit_registry.init_cloaked(kind) { … insert(Cloaked) }` runs at spawn (`units/lifecycle/spawning/mod.rs:533-539`), and the factory spawns the buildee when production starts (`production.rs:726`).
  - `Detector` and `SeismicDetector` are stamped at spawn (`bookkeeping.rs:104-110`). The seismic listener only skips `Emerging`, `Dying` and `Stunned` units (`units/weapon_fx/seismic.rs:107-110`), so it is not delayed by activation.
- **In play:** an enemy watching your Hole sees the Worm being built (~25 s) and for ~6.3–6.8 s after it finishes, and in that window it auto-attacks. In the remake it is invisible and on hold-fire throughout. Its pings and detection start 5 s early.
- **Fix:** spawn the Worm uncloaked and add `Cloaked` 5 + 1.333 s after it finishes (skip if it aimed meanwhile, as `if(!aiming)` does). Turn its `Detector` and `SeismicDetector` on at +5 s. Give the Logic Bomb `Cloaked` when it finishes, not when it is placed.

### 8. Fog hides no shots, explosions or debris — severity: low · confidence: verified
- **Original:**
  - Projectiles and particles are drawn only for the owner's allies or inside the viewer's LOS: `return (gu->spectatingFullView || (th.IsValidAllyTeam(allyTeam) && th.Ally(allyTeam, gu->myAllyTeam)) || lh->InLos(pro, gu->myAllyTeam));` (`Rendering/Env/Particles/ProjectileDrawer.cpp:599`).
  - Weapon projectiles use air LOS (`useAirLos = true;`, `Sim/Projectiles/WeaponProjectiles/WeaponProjectile.cpp:84`), and so do debris pieces more than 10 elmos above the ground (`PieceProjectile.cpp:108`).
- **Remake:**
  - Only `cloak.rs` and `seismic.rs` read `FogEnabled`.
  - Beams, bolts, lightning, CEG quads and ground flashes are spawned and batched for everyone (`units/weapon_fx/spawn.rs:485-530, 736-912, 1262`; `batch.rs`).
  - Death debris spawns with `Visibility::Visible` (`units/assets/animation/debris.rs:370, 417`).
- **In play:**
  - Pointer shells are visible along their whole arc from deep in the fog.
  - Hidden enemy constructors give themselves away with their build beams.
  - Kills inside an unseen base throw visible debris.
- **Fix:** skip drawing an effect for the local player unless its owner is allied or the effect is within allied air sight.

### 9. Enemy-built Bad Blocks are visible through fog — severity: low · confidence: verified
- **Original:**
  - `modrules.tdf` `[FEATURELOS] featureVisibility=2;` resolves to `case CModInfo::FEATURELOS_GAIAALLIED: return (isGaia || allyteam == argAllyTeam || losHandler->InLos(pos, argAllyTeam));` (`Sim/Features/Feature.cpp:144-145`).
  - A finished Bad Block becomes a feature owned by the builder's team and allyteam (`CreateWreckage({…, -1, team, allyteam, …})`, `Unit.cpp:451`).
  - The drawer skips features not in LOS (`Rendering/Features/FeatureDrawerData.cpp:136`), with no memory. Map (Gaia) features are always visible.
- **Remake:** every feature spawns with `Visibility::default()` (`units/features.rs:184, 208`), and no fog query touches features.
- **In play:** you see every wall the enemy builds anywhere on the map.
- **Fix:** hide features owned by an enemy allyteam unless they are in allied LOS. Map features stay visible.

### 10. Dying units lose their sight immediately; the engine keeps it 1.5 s — severity: low · confidence: verified
- **Original:** at death, `KillUnit` fires `UnitDestroyed` (`Unit.cpp:479-485`). That calls `lt->RemoveUnit(const_cast<CUnit*>(unit), true);` (`LosHandler.cpp:785`), and `DelayedUnrefInstance` keeps the LOS, radar and seismic coverage until `gs->frameNum + (GAME_SPEED + (GAME_SPEED >> 1))`, i.e. 45 frames (`LosHandler.cpp:337-343`).
- **Remake:** `viewers_q` and `detectors_q` are `Without<Dying>` (`cloak.rs:385, 207`).
- **In play:** when a scout dies, the original still shows its surroundings, and its killer, for 1.5 s. The remake goes dark at once.
- **Fix:** keep a dying unit as a viewer and detector for 45 frames after death.

### 11. Sensor radii are not snapped to the engine's grids, and KP's `LosMipLevel` is never applied — severity: low · confidence: verified
- **Original:**
  - Radii are whole grid squares: `(unit->losRadius / SQUARE_SIZE) >> mipLevel` (`LosHandler.cpp:139-141`, integer maths), drawn as a midpoint circle around the unit's square and tested on the square containing the target. A radius of 0 means no coverage at all (`if (radius <= 0.0f)`, `LosHandler.cpp:214`).
  - The grid sizes are the engine defaults `losMipLevel = 1; airMipLevel = 1; radarMipLevel = 2;` (`Sim/Misc/ModInfo.cpp:117-119`).
  - KP's `gamedata/sensors.tdf` (`[Sensors]{[Los]{LosMipLevel=2;…}}`) never gets through. The basecontent loader does `modRules.sensors = sensors` with the whole parsed file (`cont/base/springcontent/gamedata/modrules.lua:39-43`), so the data sits at `sensors.sensors.los`, while `ModInfo` reads `sensors.SubTable("los")` (`ModInfo.cpp:371, 378-379`). Its key `AirLosMipLevel` wouldn't match `airMipLevel` anyway.
  - Resulting radii: Exploit 300 → 288; Logic Bomb 10 → 0 (no sight); Assembler and Gateway radar 500 → 480. All other KP values come out exact.
- **Remake:** exact float radii (`cloak.rs:226-232, 433-437`).
- **In play:** barely noticeable. The Exploit sees 12 elmos less, and the Logic Bomb's 10-elmo sight doesn't exist.
- **Fix:** snap radii when computing them: sight and air sight `(r/8 >> 1)·16`, radar and seismic `(r/8 >> 2)·32`.

### Documented deviations worth a second look (one line each)
- No ghosted buildings (`LOS_PREVLOS`). The remake says "no memory" (`cloak.rs:28, 367-368`; FEATURES.md §24). In the engine every KP factory and building leaves a ghost (`leavesGhost = …GetBool("leavesGhost", IsBuildingUnit())`, `UnitDef.cpp:678`), drawn by `DrawGhostedBuildings` (`UnitDrawer.cpp:861-884`) and shown on the minimap at 0.8 dimming (`UnitDrawer.cpp:65`). This is questionable: scouted enemy bases vanish from screen and minimap in the remake but stay drawn in Spring.
- Terrain is always lit (no darkening of unseen ground). This is cosmetic and fine. It is separate from finding 6.
- The 10 Hz visibility scan versus per-frame updates (`cloak.rs:149-153`): fine.
- Nanoframes are not targetable (`spatial.rs:200-206`): fine as a design choice, but see finding 3 for its fog side effect.

## Doc drift
- **FEATURES.md:582-583 (§17):** "Logic Bombs and Worms spawn cloaked … (`Init_Cloaked=1`)". `worm.fbi` has no `Init_Cloaked`; the Worm's cloak comes from `worm.bos`, as `spawning/mod.rs:533-535` itself notes.
- **FEATURES.md:587-589 (§17), `cloak.rs:14-18` and plan.md:222-228 (§3.3):** these name Assembler, Trojan and Gateway as the detectors (plan.md §6 correctly says "`RadarDistance > 0`"). The code gives a `Detector` to every unit with `RadarDistance > 0`, so Byte, Connection and Worm too (`bookkeeping.rs:104-106`; `cloak.rs:193-195` admits it).
- **FEATURES.md:792-797 (§24):** "an enemy unit is drawn … and shown on the minimap only while inside some allied unit's FBI SightDistance". This is false for enemy nanoframes, which are never drawn (finding 3), and for Worms that bury themselves again, which stay on the minimap (finding 4).
- **plan.md §6 (lines ~484-503):** "defaults to `false` in the current sandbox build", "Flip `FogEnabled` to `true` once …", and "Deferred: per-client `PlayerTeam` … Worm-while-attacking reveal rules (blocked on autohold toggle …)" are all stale. `sync_fog_enabled` derives the flag every tick (`cloak.rs:69-79`), `worm.rs` implements surfacing and AutoHold, and `PlayerTeam` is now `LocalTeam`.
- **`seismic.rs:10-12`:** "a detector still being built, dead or stunned has none (`LosHandler.cpp:167-190`)". The same lines also require `unit->activated`, which for the Worm means 5 s after it finishes.
- **`combat/mod.rs:88-102`:** says `TARGET_LOST_TIMER` is about a target "out of its reach" and slow updates are "of 16 frames". The engine timer counts lost LOS and radar contact (`CommandAI.cpp:1469-1485`), never expires for immobile targets, and slow updates are 15 frames.
- **Upstream (for reference only):** the readme says the Bug "can sense movement outside of its LOS" (`Kernel_Panic_readme.txt:462`), but `bug.fbi` has `//SeismicDistance=1024;` commented out and the changelog says "Removed bug seismic detection" (line 836). The remake correctly gives the Bug none.

## Checked and matching
- Cloaked enemies stay hidden unless something reveals them, and your own and allied cloaked units are drawn translucent (engine `IsAlpha(co) → co->IsCloaked()`, `UnitDrawerData.h:125`; remake alpha 0.5, `cloak.rs:287, 314-343`).
- Cloak cost with no energy: Recoil hands the decision to `AllowUnitCloak` (`Unit.cpp:2517`) and no KP gadget defines it, so cloaking is free. The Logic Bomb has `CloakCost=0`; the remake charges nothing.
- Logic Bomb `MinCloakDistance=1` (`UnitDef.cpp:501`) has no effect without an `AllowUnitCloak` gadget, so the bomb is never decloaked by an enemy nearby. In the remake it also stays cloaked until it dies.
- `Init_Cloaked` is mapped to `initcloaked` (basecontent `parse_fbi.lua:211-215`; `UnitDef.cpp:500`), so the Logic Bomb starts with `wantCloak` set (`Unit.cpp:295`). Remake: `init_cloaked`.
- Decloak on fire (`decloakOnFire` default true, `UnitDef.cpp:503`; `Weapon.cpp:587-588`): the only cloaked shooter is the Worm, and it already drops its cloak in `AimWeapon1` (`set CLOAKED to FALSE; set WANT_CLOAK to FALSE;`, `worm.bos:164-165`; both set `wantCloak`, `UnitScript.cpp:1673-1679`). The remake drops `Cloaked` on the first aim tick (`worm.rs:116-133`). The engine applies it at the next slow update, at most 15 frames later.
- The Worm buries itself again after `ResetAim` (`sleep 2000`, head move 16/12 s, `WANT_CLOAK`, `worm.bos:145-151`), matching `RESET_AIM_DELAY = 2.0 + 16.0/12.0` (`worm.rs:31-35`). The engine adds at most 15 frames of slow-update delay.
- A stunned unit keeps its cloak (`SlowUpdateCloak(true)` → `AllowUnitCloak(this, this)` → true). The remake doesn't decloak on stun either.
- AutoHold defaults to on for humans and off for AI (`autohold.lua` `init = {human=true,bot=false}`) and holds fire while cloaked (`worm.bos:71-82`), matching `worm.rs:47-53` and the combat gate.
- `Burrow.lua` only covers `wormold` and `bugold` (secret units). It is out of scope and correctly not ported.
- Seismic pings match `CUnit::DoSeismicPing` (`Unit.cpp:1475-1505`, called on every slow update while `progressState == Active`, `Unit.cpp:1105-1106`): signature > 0, not in local LOS, inside friendly seismic coverage (a circle, not a raycast), ±48 elmos per axis, `CSeismicGroundFlash(pingPos, 30, 15, 0, pingSize, 1, (0.8,0,0))`. 1024 on the 32-elmo grid is exact.
- `SeismicSignature` defaults to `sqrt(mass/100)` for tank and kbot move classes (`UnitDef.cpp:624-633`), the Worm has 0, and the Bug has no seismic detection. All match the remake.
- The kamikaze trigger ignores LOS (`kamikazeUseLOS` default false, `UnitDef.cpp:509`; `GetEnemyUnitsNoLosTest`, `Unit.cpp:1132-1134`) and fires on cloaked units too, matching `tick_kamikaze` (`units/combat/lifecycle.rs:201-222`).
- Spectators and defeated players get full view (`spectatingFullView`), matching `fog_applies` (`cloak.rs:60-62`).
- Vision is shared across the allyteam, matching `alliances.friendly` and `vision_mask`.
- Map (Gaia) features are always visible, matching the remake.
- Hidden enemies can't be picked, hovered or double-click-selected (`interaction/selection/core.rs:399-410`, hidden root unpickable). The engine requires LOS or radar.
- AI information: upstream KPAI and Fair KPAI are synced gadgets that list units with `GetAllUnits` and `GetUnitsIn*` and never check LOS (no `IsUnitInLos` or `GetUnitLosState` in `KPAI*.lua`). The remake's AI also reads the world directly (`cloak.rs:370-371`). Both are omniscient; noted only.

## Unverified leads
- **Sonar under water.** The same `modrules.lua` overwrite drops `modrules.tdf` `[SENSORS] requireSonarUnderWater=0`, so the engine runs with `true` (`ModInfo.cpp:121, 373`). A fully submerged unit would then be invisible without sonar. This matters only where a unit can sink below the water plane (Speed Balls 16 Way has −200 pits with `WaterDamage=100`; DigitalDivide goes down to −7). To settle: check whether any KP unit can become `IsUnderWater` there, and how the remake handles water.
- **Seismic pings from stunned units.** The engine never pings while a unit is stunned (`SlowUpdate` returns early at `Unit.cpp:1003-1017`). The remake's `movers` query (`seismic.rs:111-121`) doesn't exclude `Stunned`. It would ping if a stunned unit's `GroundMover.progress` stays `Active`. To settle: check whether a stun resets `progress`.
- **How often radar-blip shots still hit.** Large area weapons (Byte MegaBeam AoE 128, Exploit) may still hurt a mine at a 50–96-elmo error. This affects how strongly finding 2 bites per unit type. To settle: compare beam traces against the error vector.
- **Ghost of a mine seen under construction.** A Logic Bomb is `IsBuildingUnit` (yardmap `y`), so it leaves a ghost, and live ghosts are drawn whether or not the unit is cloaked (`UnitDrawer.cpp:884`). An enemy who saw the nanoframe may keep seeing a ghost of the mine. I did not trace `UnitLeftLos` through the cloak transition end to end.

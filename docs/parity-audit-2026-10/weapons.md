# Weapons (targeting, firing, aiming, projectiles) parity audit

Paths: engine = `upstream/RecoilEngine/rts/`, KP = `upstream/Kernel-Panic/`, remake = `kernel-panic/src/`.

## Findings (most important first)

### 1. Ground units never auto-target Flows: `NoChaseCategory=VTOL` is treated as a targeting filter. Severity: high. Confidence: verified
- **Original:** `noChaseCategory` has one sim use, `CMobileCAI::IsValidTarget` (engine `Sim/Units/CommandAI/MobileCAI.cpp:583` `if (owner->unitDef->noChaseCategory & enemy->category) return false;`). Only the CAI calls that, to decide whether to *chase*. `rg noChaseCategory rts/` finds nothing in `Weapon.cpp` or `GameHelper.cpp`.
  - Weapon target choice (`CWeapon::TestTarget` `Weapon.cpp:1020-1085`, `GenerateWeaponTargets` `Game/GameHelper.cpp:657-786`) filters only on `onlyTargetCategory`, LOS/radar, ally status and water.
  - The Flow is `Category=UNIT TARGET NOTFACTORY VTOL` (KP `units/flow.fbi:13`). That matches `OnlyTargetCategory1=TARGET` (Bit, Bug, Byte, Packet, Gateway, Connection, Virus, Exploit) and `UNIT` (DOS).
  - Heightmod 0.2 costs only about 2 elmos of reach against a Flow at cruiseAlt 140.
  - The KP readme agrees (`Kernel_Panic_readme.txt:485`): "Pointers, DoSes and Connections make short work of Flows". The Connection has `NoChaseCategory=VTOL`. Readme line 632 changed the DoS's NoChaseCategory "to increase DoS efficiency against Flow", i.e. so it would chase them.
- **Remake:**
  - `units/combat/mod.rs:1023` `let skip_flying = stats.no_chase_vtol && !unit_type.0.homing_targets_air();`
  - `mod.rs:1148` `if skip_flying && candidate.is_flying { return; }`
  - The only exception is the Pointer (`content/definitions.rs:306-314`). FEATURES.md:322-324 states the same rule.
- **In play:** Flows hover over Bits, Bugs, Packets, Bytes, Gateways, Connections, Viruses and Exploits without drawing fire. Only Pointers and DOSes shoot back. In the original all of these shoot Flows that come within their range, and Bytes (478), Connections (450) and Exploits (1200) outrange the Flow's 350.
- **Fix:** Drop `skip_flying` from the weapon auto-target scan. Keep NoChaseCategory for chase or auto-engage decisions only (see #7).

### 2. Wormsplash detonations do nothing (no 2×800 splash, no infection). Severity: high. Confidence: verified
- **Original:**
  - worm.bos `FireWeapon1` (`scripts/worm.bos:187-200`) lashes the head out, `wait-for-move head` (112 elmos at 336 e/s, about 10 frames), then `emit-sfx 4097 from head; emit-sfx 4097 from end;`.
  - 4097 is `SFX_DETONATE_WEAPON (4096) + 1` (`Sim/Units/Scripts/CobDefines.h:18-19`). `UnitScript.cpp:788-820` turns that into `helper->Explosion(params)` with Wormsplash's damages and AoE, whatever the weapon type: 800 dmg, radius 105, edgeEffectiveness 1, no team filter.
  - `infection.lua` lists Wormsplash as an infector (200 frames).
- **Remake:**
  - `combat/mod.rs:1322-1330` → `mechanics/worm.rs:150-166` queues two `PendingDamage { target: None, weapon: Wormsplash }`.
  - But `combat/damage.rs:753` `let melee = weapon_def.category() == Melee;` (Wormsplash is `WeaponType=Melee`), and `damage.rs:828` `if melee { return; }` returns before any splash. With `target: None` there is no primary hit either. The test `damage.rs:1593-1630` even asserts "a target-less bite does nothing".
  - This regressed when the one-DoDamage path landed (64f9d9d) after 54fb0e2 had wired the splash.
- **In play:** Every Worm bite loses its 800 splash at the bite point and at the tail (1600 where both overlap). It also loses the 6.7 s infection that turns its kills into Viruses. Allies next to the Worm no longer take friendly fire.
- **Fix:**
  - Resolve script detonations (`emit-sfx 4096+n`) as an Explosion regardless of weapon type, e.g. a flag on `PendingDamage` that skips the melee early-return.
  - Place them at the `head` and `end` piece world positions about 10 frames after the bite.

### 3. Weapons ignore line of sight and radar: artillery fires on unspotted targets out to full range. Severity: high. Confidence: verified
- **Original:**
  - `CWeapon::TestTarget` (`Weapon.cpp:1039-1040`): `if ((trg.unit->losStatus[owner->allyteam] & (LOS_INLOS | LOS_INRADAR)) == 0) return false;`
  - `GenerateWeaponTargets` (`GameHelper.cpp:719-726`) skips units out of LOS and radar. Radar-only units are aimed at `GetUnitPositionWithError` with ×10 priority.
  - Sight: Exploit `SightDistance=300` vs BugCannon 1200; Pointer 768 vs 1400; Obelisk 512 vs 2000.
  - Radar: Assembler and Gateway 500, Trojan 768, Byte and Connection 200, Worm 256.
- **Remake:**
  - The only visibility test is cloak: `spatial.rs:89-91` `targetable_by` = not a feature and not an undetected cloaked unit; `combat/mod.rs:1145`, `mod.rs:315-323` (`visible_pos`).
  - The fog of war (`cloak::update_fog_visibility`, plan.md §6) is display-only, from the local player's view.
- **In play:** A lone Exploit or Pointer shells anything in weapon range. In the original it needs an ally's sight or a radar blip (inaccurate) on the target. Attack orders on units that slip out of vision also never time out (engine `CommandAI.cpp:1481-1485`: lost after 4 slow updates out of radar).
- **Fix:** Keep a per-ally-team LOS/radar state in the sim and require it in the auto-target scan and when picking a target. Use the radar error position for radar-only targets.

### 4. The Obelisk's Infection is an instant zone, not a lobbed shell. Severity: high. Confidence: verified
- **Original:**
  - `[Infection]` (`weapons/corruptionweapons.tdf:386-425`) is a Cannon: `ballistic=1`, 800 e/s, `myGravity=.4`, `commandfire=1`.
  - On impact it explodes with `areaofeffect=700` (radius 350) and `edgeeffectiveness=1`, dealing default 200 and building 1500, and spawns `explosiongenerator=custom:corruption_infection`. The trail CEG is `cegTag=corruption_WMDtrail`.
  - `areadenial.lua:29-49` creates the 400-radius zone from that `gadget:Explosion`.
  - `obelisk.fbi:63` `highTrajectory=1` sets `useHighTrajectory` (`Sim/Units/Unit.cpp:307`). `CCannon::SlowUpdate` (`Cannon.cpp:162-168`) copies it because the weapon's `highTrajectory` defaults to 2 (`WeaponDef.cpp:114`), so it fires a high arc. At 1000 elmos that is about 73°, roughly 4.2 s of flight.
  - `AimWeapon1` waits about 1 s for `segf` (`scripts/obelisk.bos:53-63`).
  - It fires from a normal Attack order: manualfire gives `noAutoTarget` (`WeaponDef.cpp:571`), the Obelisk has no `canManualFire`, and the order finishes after one salvo (`CommandAI.cpp:1760-1761`).
- **Remake:**
  - `mechanics/command_fire.rs:818-849` and `ability_for` (`:1630-1643`): pressing D spawns the `AreaDenialZone` at the click instantly.
  - No projectile, no impact explosion or damage, no CEG. The comment at `:1622-1627` claims "Infection's gas has no CEG upstream".
  - `aim.rs:135-139` assumes "no Kernel Panic weapon sets highTrajectory".
- **In play:**
  - The WMD lands the instant it is cast instead of about 5 s later.
  - Targets get no warning arc or trail.
  - Buildings within 350 never take the 1500 hit, and units never take the 200 hit. Today only the 120 dps zone acts.
  - A right-click attack with the Obelisk does nothing.
- **Fix:** Fire Infection as a real Cannon projectile (high arc for highTrajectory=1 units) after the 1 s aim. Explode with weapon damage and CEG at impact and spawn the zone there.

### 5. Target choice is nearest-first; the engine weighs health, damage vs armour, cost, facing and stun. Severity: high (known gap). Confidence: verified
- **Original:** `GameHelper.cpp:716-769`. Lower priority wins.
  - The priority is `angleMul × rangeMul × (secDamage + health) / (damage(armour)·curArmorMultiple · power)`.
  - `angleMul = (1 + (1 − weaponDir·tgtDir))²`, range 1–9, measured against the current muzzle direction. `rangeMul = dist2D·proximityPriority + 0.4·modRange + 100`.
  - Multipliers: ×100 for a bad category, ×4 for a paralyzer weapon on an already-stunned target, ×100000 beyond base range, ×0.5 for the last attacker (only if ≥200 frames ago, as coded).
  - `power` = BuildCostMetal (e.g. Bit 10, Byte 100, Kernel 500).
  - With `proximityPriority=-5`, BugCannon priorities go negative beyond about 116 elmos, so it prefers far, high-health, cheap targets.
- **Remake:** `combat/mod.rs:269-293`, `1163-1170`: `TargetRank { bad, dist_sq }` (3D feet-to-feet; negated for proximityPriority < 0). plan.md:119-123 tracks it but credits `CWeapon::TargetWeight`, which is an unused script callin.
- **In play:**
  - The engine focuses wounded and expensive units in front of the muzzle. Bits pick a 100 HP Bit at 200 over a full one at 100, and a Bug over a Byte at equal range.
  - DOSes spread stuns (×4 on stunned targets); the remake keeps zapping the nearest stunned unit.
  - Freshly built ARMORED units (damageMul floored at 0.0001) are ignored in the original but shot in the remake.
- **Fix:** Port the `GenerateWeaponTargets` priority as the sort key, using 2D distance from the owner position.

### 6. DOS_Beam and GaussCannon land as one instant full hit instead of beamtime×30 sub-beams; DOS lacks range falloff. Severity: medium. Confidence: verified
- **Original:**
  - `CBeamLaser::Init` (`BeamLaser.cpp:99-113`): non-`beamburst` beams get `salvoSize = int(beamtime·30)` (DOS 8, Gauss 12), `salvoDelay = 0`, each dealing `damage / salvoSize`.
  - Every sub-beam after the first reuses the first one's world direction (`GetFireDir` `:196-213` `dir = oldDir`), with fresh spray.
  - Damage scales by `max(minIntensity, 1 − d/(2·range))` (`:413-423`). DOS_Beam sets no `minIntensity`, so it deals 50% at 768. PacketBeam, GatewayBeam and Gauss set `minIntensity=1`.
  - Leading: `GetPredictedImpactTime = salvoSize·0.5` frames (`:160-165`).
  - The trace (`TraceRay` with `collisionFlags`, `:330`) stops at the first enemy or feature on the ray, not just the target.
  - CanFire waits for `salvoLeft == 0`, so a DOS salvo repeats every 8 frames, not every 7-frame reload.
- **Remake:** `combat/mod.rs:669-692`: one `PendingDamage` per salvo shot with the full damage. `hitscan_trace` (`:401-445`) tests only the intended target, features and ground. No lead for hitscan (`:460-462`). `spring-tdf` doesn't parse `minIntensity`.
- **In play:**
  - The remake's DOS stuns about 1.14× faster point-blank and about 2.3× faster at 700–768 elmos.
  - A Connection's 4500 lands in one frame. In the original it spreads over 0.4 s along a fixed line, so fast movers (Bits, Packets) escape part of it.
  - Remake beams pass through enemies standing in front of their target.
- **Fix:** Model non-beamburst BeamLasers as an N-shot, 1-frame salvo with `oldDir`, the intensity falloff and minIntensity, the `salvoSize/2` lead, and a first-enemy trace.

### 7. Idle units never move to engage enemies just outside range (Maneuver auto-attack missing). Severity: medium. Confidence: verified
- **Original:**
  - `CMobileCAI::SlowUpdate` with an empty queue calls `MobileAutoGenerateTarget`, which calls `GenerateAttackCmd` (`MobileCAI.cpp:337-354`, `1163-1172`, `1196-1259`).
  - It picks a weapon target, or `GetClosestValidTarget` within `maxRange + 200·moveState²`, and issues a temp CMD_ATTACK with a 5 s timeout. The unit closes in up to `maneuverLeash (500)·moveState + maxRange` (`:891-901`, `MoveType.cpp:76`).
  - Mobile units inherit Maneuver from their factory (`UnitDef.cpp:465`, `Unit.cpp:349-361`). KP hides the move-state button (`LuaUI/Widgets/hide_commands.lua`).
  - The Pointer opts out with `set STANDINGMOVEORDERS to 0` (`pointer.bos:158`). NoChaseCategory excludes Flows from this chase (#1).
- **Remake:** No equivalent. `combat_system` only fires at targets already in range, and `attack_target_system` handles explicit orders only.
- **In play:** In the original an idle Bit group rushes a Byte or Bug that comes within about 440 elmos. In the remake it stands until the enemy is within 239, even while being outranged.
- **Fix:** Add the idle GenerateAttackCmd pass (slow-update cadence, `maxRange + 200` search, 500 leash, 5 s temp order, NoChaseCategory filter, hold-pos for Pointer).

### 8. Arcing weapons never check their arc against terrain or features. Severity: medium. Confidence: verified
- **Original:**
  - `CCannon::HaveFreeLineOfFire` (`Cannon.cpp:58-100`) walks the parabola with `CGround::TrajectoryGroundCol` (`Map/Ground.cpp:590-620`, 8-elmo steps over 15/16 of the distance). It refuses the shot when terrain is above the arc, then runs `TestTrajectoryCone` for features (avoidFeature defaults on).
  - Trajectory missiles (Pointer `Geometric`, NX) check their pursuit curve against ground and features (`MissileLauncher.cpp:72-200`).
  - `AutoTarget`'s TryTarget therefore skips targets behind ridges.
- **Remake:** `combat/mod.rs:154-161` `straight_line_of_fire` returns false for `Cannon` and for MissileLauncher with `trajectory_height > 0`, so `ground_line_of_fire` and `free_line_of_fire` never run for them. FEATURES.md:329-330 says lobbed shots "skip the LOS check".
- **In play:** Exploits (which prefer the farthest target) and Obelisks fire into hills and the shell bursts on the ridge. In the original they hold, or pick a target their arc can reach.
- **Fix:** Port TrajectoryGroundCol and TestTrajectoryCone for Cannon, and the pursuit-curve check for trajectory missiles.

### 9. Turret aiming: bodies turn to targets (the Exploit snaps), the Bug skips its aim script, and the Bug's rear arc is only checked at scan time. Severity: medium-low. Confidence: verified
- **Original:**
  - Only pointer.bos turns the unit (`set HEADING`). The other scripts turn pieces at scripted speeds and block firing until done (Exploit turret 180°/s `exploit.bos`; Bug body 720°/s).
  - bug.cob names its callins `AimPrimary`, `QueryPrimary` and `AimFromPrimary`. The engine maps these to weapon 1 (`Sim/Units/Scripts/CobScriptNames.cpp:82-84`), and `AimPrimary` waits for the body turn (`bug.bos:98-106`).
  - `MaxAngleDif1=270` is enforced in every TryTarget (`TestRange` → `CheckTargetAngleConstraint`, `Weapon.cpp:1108`) and in `CallAimingScript`.
- **Remake:**
  - `combat/aim.rs:497-591` turns every idle ground unit without an `aimer` piece toward its target. `TurnRate` 0 (Exploit) gives `max_turn = TAU`, an instant snap (`:551-555`), which also cancels the turret traverse that should gate the shot.
  - `animation/units/mod.rs:308-330` `has_aim_weapon` omits the Bug, and `bug.rs:30-33` says "the script has no AimWeapon1". Result: no AimScript, the Bug fires before facing, and its body piece never swivels.
  - The firing arc is applied only in the scan (`combat/mod.rs:1126-1137`), not to cached or explicit targets, so a fleeing Bug keeps shooting backward.
- **Fix:** Turn the body only for the Pointer, give the Bug its AimPrimary driver and AimScript, and test the arc on every kept or explicit target and before each shot.

### 10. Retarget timing: immediate in the remake, next slow update in the engine. Severity: low. Confidence: verified
- **Original:** `AutoTarget` runs only from the weapon `SlowUpdate`, staggered every 15 frames (`Weapon.cpp:780-812`, `Sim/Units/UnitHandler.cpp:348-377`). A dead target is dropped (`:828-833`), and a new one is chosen 0–14 frames later (`UnitDef.cpp:56` "without waiting for slow update" is opt-in `fastAutoRetargeting`). A held good target is re-scanned when `frameNum > lastTargetRetry + 65`, i.e. at the 5th slow update (+75).
- **Remake:** `combat/mod.rs:1089-1112` rescans the same tick a target is gone or invalid. `TARGET_RESCAN_FRAMES = 65` (`:247`). `AimScript::ready`, computed for the old target, can let a ready weapon fire at the new one that tick without turning (only the Pointer gunbase and Byte aimer are re-checked live, `:1400-1489`).
- **Fix:** Run scans on a per-unit 15-frame phase and use `> retry + 65` at slow-update granularity. Clear `ready` on a target change.

### 11. The Worm's bite range is measured to the target's centre, not its surface (Melee targetBorder=1). Severity: low. Confidence: verified
- **Original:** Melee defaults `targetBorder=1` and `cylinderTargeting=1` (`WeaponDef.cpp:382-385`). TryTarget and TestRange use `GetTargetBorderPos`, the near collision surface (`Weapon.cpp:891-951`, `1428-1442`). The CAI closes in to `target − radius·0.8` (`MobileCAI.cpp:748`, `825`). So ordered and held targets are bitten from 200 + radius.
- **Remake:** `aim.rs:250-256` and `combat/mod.rs:1822-1838` measure to `mid_pos`.
- **In play:** Worms must walk about one collision radius closer to bite homebases and minifacs.
- **Fix:** For Melee, measure range to the target sphere's near surface.

### 12. The Flow's Starburst range applies heightmod once instead of twice. Severity: low. Confidence: verified
- **Original:** `TestRange` and `GenerateWeaponTargets` pass `heightDiff·heightmod` (`Weapon.cpp:1097`, `GameHelper.cpp:728`), and `CStarburstLauncher::GetRange2D` multiplies by `heightmod` again (`StarburstLauncher.cpp:58-62`). The result is `350 + 0.04·dy`, about 345 elmos against ground targets from cruise height.
- **Remake:** `aim.rs:246`, `264`: `range + 0.2·dy`, about 323 elmos.
- **Fix:** `range + ydiff * height_mod` for StarburstLauncher.

### 13. Immobile units drop attack orders after 64 frames out of range. Severity: low. Confidence: verified
- **Original:** `CCommandAI::ExecuteAttack` (`CommandAI.cpp:1489-1500`) finishes only when the target dies or `UpdateTargetLostTimer` hits 0. That timer runs only while a mobile target is out of radar (`:1469-1486`), 4 slow updates of 15 frames, and range is irrelevant. An Exploit keeps the order and fires when the target returns.
- **Remake:** `combat/mod.rs:88-102` and `1848-1865`: `TargetLost` drops the order after `TARGET_LOST_FRAMES = 64` ("slow updates of 16 frames") out of reach.
- **Fix:** Keep the order while the target is alive and visible.

### 14. The SIGTERM release window is 18 frames instead of 12. Severity: low. Confidence: verified
- **Original:** `weapondefs_post.lua:121-133` rewrites `manualBombSettings` AircraftBombs: with `reloadtime ≥ 0.5`, `burstrate = min(0.4, reload)` and `burst = 2`. `CBombDropper::TestRange` uses `dropDist = max(1, salvoSize−1)·salvoDelay·speed·0.5` = 6·speed. The bomb nudge only corrects the lateral offset.
- **Remake:** `command_fire.rs:279-282` `SIGTERM_SALVO_FRAMES = 18` (from `.6`) = 9·speed. `:1462-1466` also nudges along-track.
- **In play:** Negligible against AoE 450.
- **Fix:** Use 12 frames.

## Doc drift
- FEATURES.md:322-324 "ground weapons don't chase flying targets — Flow can zip past them safely" describes a targeting rule the original doesn't have (#1). The same misreading appears in `definitions.rs:306-311` and `command_fire.rs:249-252`: the Signal is untargetable because of `OnlyTargetCategory` (it lacks TARGET, UNIT and EDIBLE), not NoChaseCategory.
- FEATURES.md:329-330 "Ballistic weapons skip the LOS check and arc over ridges" is false for the original (#8).
- plan.md:119-123 attributes target weighting to `CWeapon::TargetWeight`; it is `GenerateWeaponTargets`.
- plan.md:124-127 "LOS blockers beyond terrain… We currently don't [test units/features]" is stale: `free_line_of_fire` now tests features and avoidfriendly allies.
- `worm.rs:2,14-16,146-149` claims the Wormsplash runs "splash + armor + infection". `damage.rs:828` prevents it (#2).
- `bug.rs:30-33` "script has no AimWeapon1" is wrong (AimPrimary).
- `command_fire.rs:1622-1627` "Infection's gas has no CEG upstream" is wrong.
- `aim.rs:135-136` "no KP weapon sets highTrajectory" is wrong (obelisk.fbi:63).
- `combat/mod.rs:88-102` `TARGET_LOST_FRAMES` describes "16-frame" slow updates and the wrong timer semantics.
- `command_fire.rs:279-282` derives the window from `burstrate=.6`, which the post-processor rewrites.

## Checked and matching
- Reload: `max(1, int(reload·30))`, starting at salvo open; shared by auto and ground fire.
- `salvoDelay = int(burstRate·30)`. Per projectile: Shot1 → QueryWeapon1 → fire, then EndBurst.
- Byte bp0..bp3, Flow gp cycling and Virus gun alternation follow the scripts.
- `predictSpeedMod` is uniform in [0,2] (predictBoost 0), re-rolled per slow update.
- Lead = velocity·dist/projectileSpeed·mult for traveling weapons; no leadLimit.
- sprayAngle = sin(x·π/0xafff), uniform-ball `NextVector`. `accuracy`, `movingAccuracy` and `targetMoveError` are absent, so 0.
- LaserCannon: range rounded to whole speed steps, ttl = min(ceil, floor−1) (burnblow defaults on). The MineLauncher's `burnblow=0` flies its full range. Bolts fade at ttl without exploding.
- Per-type heightmod (Cannon 0.8, BeamLaser 1.0, others 0.2). Sphere TestRange; Melee cylinder.
- Cannon ballistic solve with range and height boost, ttl = 2·predict, spray on the launch direction.
- MissileFlight (extraHeight, turnrate gain, ttl, fuel-out drag and gravity) and StarburstFlight (uptime, turn stage, tracking, flighttime, fixedLauncher, +2 elmo start).
- Straight-weapon ground line of fire with AoE-radius tolerance. Ray or cone ally test for avoidfriendly=1; feature avoid on by default.
- Good targets before bad; bad targets re-picked every slow update. OnlyTargetCategory1 gates auto targets and orders.
- Fire state: fire-at-will everywhere. Worm AutoHold (human on, AI off) holds fire only while cloaked, and AimWeapon1 surfaces the Worm before it bites.
- Projectiles collide with the first non-allied unit or feature on each frame segment (collideFriendly=0), then the ground.
- A buried muzzle can't fire. Stunned units don't fire, and their bursts pause.
- NX: single shot, weapon 1 blocked about 2 s, range 1400, 30 s reload.
- Mine launch: tube emit directions, spray, 6000 HP cost, 10 s reload, mine cap.
- Connection: gp2 slide at 90 e/s and 24-frame lockAim. Pointer: open gate and gunbase pitch.
- Debug: one Minekiller blast at its feet after the build.
- SIGTERM: one bomb (FireWeapon1 destroys the Signal), unowned explosion, then the zone.

## Unverified leads
- **Underwater targets:** `CanFire` refuses when `aimFromPos.y <= 0` (`Weapon.cpp:447`), and `TestTarget` rejects fully underwater targets (`Weapon.cpp:1075-1082`). Neither is modelled. Settle with the water depths on DigitalDivide_PT2 and Speed_Balls_16_Way.
- **Collision order:** The engine collides with the first unit in quad order (`ProjectileHandler.cpp:440-465`). The remake tries the intended target first, then the nearest. Only matters in crowds.
- **Mid-burst target loss:** If the target dies mid-burst, the engine keeps firing at its last led position. The remake fires at the position from when the salvo opened (`SalvoShot::aim_pos`). Small for MegaBeam's 21-frame burst; a test would settle it.
- **Aim height clamp:** `GetUnitLeadTargetPos` clamps aim to ground+2 and to the water surface (`Weapon.cpp:1436-1439`); the remake doesn't. Probably negligible.

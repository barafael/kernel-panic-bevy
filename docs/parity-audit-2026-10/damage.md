# Damage model parity audit

Scope: from impact (explosion / `DoDamage`) to health, paralysis, death and healing. Engine = `upstream/RecoilEngine/rts`, mod = `upstream/Kernel-Panic`, remake = `kernel-panic/src` unless noted.

## Findings (most important first)

### 1. Wormsplash detonations deal no damage and infect no one — severity: high · confidence: verified
- Original: worm.bos `FireWeapon1` ends with `emit-sfx 4097 from head; emit-sfx 4097 from end;` (scripts/worm.bos:199-200), i.e. "detonate weapon index 1" = `Weapon2=Wormsplash` (units/worm.fbi:66). The engine turns a detonate-sfx into a full `helper->Explosion(params)`, whatever the weapon type, with `.ignoreOwner = weaponDef->noSelfDamage` (Sim/Units/Scripts/UnitScript.cpp:788-819). `[Wormsplash]` is `WeaponType=Melee` (weapons/corruptionweapons.tdf:154-157) but that only matters for normal firing; the detonation is a real explosion: radius 105 (areaofeffect 210), edge 1, 800 damage (subterranean 0.000001, infectious 10), instant (explosionspeed 65535). It is an infector: `infector[wormsplash] = 200` frames (LuaRules/Gadgets/infection.lua:28).
- Remake: the bite queues two target-less `PendingDamage` for Wormsplash (units/mechanics/worm.rs:150-166, called from units/combat/mod.rs:1322-1330). `DamagePass::explode` computes `let melee = weapon_def.category() == Melee` (units/combat/damage.rs:753). With `target: None` the primary branch is skipped (816) and `if melee { return; }` (828) returns before the splash loop. The real Wormsplash def is `Melee`, so both detonations do nothing. The test that "proves" Wormsplash infects (damage.rs:1363-1405) builds a Wormsplash def *without* `weapon_type: "Melee"`, so it doesn't cover the shipped TDF.
- In play: a Worm bite deals only Wormbite's 3200 to its target. Missing: 800 to every unit (friend or foe) within 105 elmos of the head and of the tail, and the 6.7 s infection that turns Worm victims into Viruses. FEATURES.md:662 and DEEP_FEATURES.md §Infection describe this as the Worm's core mechanic, but it doesn't happen. The Worm's own idle-heal clock is also never reset by its splash (upstream it takes 0.000001).
- Fix: in `explode`, treat script detonations as explosions whatever the category. For example, carry a "detonated" flag on `PendingDamage` (set by `queue_wormsplash`) and apply the melee short-circuit only to real melee fire. Upstream detonates at the `head` piece about 0.33 s after the bite (`wait-for-move head`) and at the `end` piece, not at the target and worm origins.

### 2. DoS paralysis follows a different model (accumulation, decay, end of stun, range falloff, reload) — severity: high · confidence: verified
- Original:
  - KP's modrules.tdf has no `[PARALYZE]` section, so `paralyzeDeclineRate=40` and `paralyzeOnMaxHealth=true` (Sim/Misc/ModInfo.cpp:97-98, 316-320), giving `empDeclineRate = 1/40` (Unit.cpp:160).
  - Accumulation: in `ApplyDamage`, `maxParalysisDamage = max(maxHP + maxHP/40·paralyzeTime − paralyzeDamage, 0)`. The hit is clamped to that, so paralysis caps at 1.125×maxHP for DOS_Beam's `paralyzetime=5`. The unit is stunned once `paralyzeDamage >= maxHP` (Unit.cpp:1275-1291).
  - Decay: every slow update `paralyzeDamage -= maxHP·(15/30)·(1/40)`, which is linear at 2.5 % of max HP per second. It runs whether or not the unit is stunned (Unit.cpp:991-999).
  - End of stun: the unit unstuns at the first slow update with `paralyzeDamage <= maxHP` (Unit.cpp:1008-1013). Its paralysis stays at about maxHP, so the next beam stuns it again.
  - Beam range falloff: DOS_Beam is a BeamLaser with no `minIntensity` (default 0, WeaponDef.cpp:141; tdf:78-121). Each beam is scaled by `max(minIntensity, 1 − dist/(2·range))`, 100 % point-blank and 50 % at 768 (BeamLaser.cpp:418-423). The Network beams set `minIntensity=1` and are unaffected.
  - Fire rate: the 0.27 s beamtime makes an 8-frame salvo that must finish before the next one (BeamLaser.cpp:104-111, Weapon.cpp:434), so one salvo every 8 frames, not 7.
  - Reload while stunned: `CUnit::Update` adds 1 to every weapon's `reloadStatus` each stunned frame, so reload is frozen (Unit.cpp:680-687).
- Remake: `charge += damage` with no cap. Reaching max HP inserts `Stunned { remaining: paralyze_time }`, and every later hit at or above the threshold restarts the 5 s (units/combat/damage.rs:475-485). `tick_stun` zeroes the charge when the stun ends. Between stuns the charge decays exponentially, 25 % of the current charge per second, and only while not stunned (units/combat/lifecycle.rs:113-115, 165-190). There is no beam falloff (explode only applies `dyn_damage_multiplier`, damage.rs:771-772), one full 400 hit every 7 frames (`after_reload`, units/combat/mod.rs:756-761), and `AttackCooldown` keeps counting while stunned.
- In play (simulated both models with continuous DoS fire; Byte/Connection 15000 HP, Worm 12000; DoS does 400 to these classes):
  - With exponential decay a lone DoS tops out at about 7050 charge. One or two DoS can never stun an open Byte or a Connection, and one can never stun a Worm. Upstream a lone DoS stuns them in 13 s point-blank to 28 s at 600 elmos (Worm 10–19 s), and a pair does it in 6–10 s.
  - Small units go the other way, because the falloff is missing: Bits and Pointers are stunned up to about 2× faster at range than upstream.
  - A burst that just crosses the threshold freezes the target for a full 5 s in the remake. Upstream the stun lasts (P − maxHP)/(maxHP/40) seconds, from a fraction of a second up to 5 s.
  - After a stun, upstream a single extra beam stuns again; the remake needs a full max HP of charge again.
  - A Pointer stunned just after firing upstream still waits its remaining 4 s reload after it recovers; in the remake it fires immediately.
- Fix: port `ApplyDamage`/`SlowUpdate`: cap at maxHP·(1+t/40), subtract maxHP/80 every 15 frames always, unstun when ≤ maxHP, and keep the charge. Apply `1 − d/(2·range)` to non-`minIntensity` BeamLaser hits. Push back `AttackCooldown.ready_frame` by one frame per stunned frame.

### 3. The Obelisk's Infection shell never explodes (no 200/1500 blast) — severity: medium · confidence: verified
- Original: the Obelisk's `Weapon1=Infection` (units/obelisk.fbi:62) is a real manual-fire ballistic weapon (weaponvelocity 800). On impact it is an ordinary `CGameHelper::Explosion`: areaofeffect 700 gives radius 350, edge 1, default 200, building 1500 (weapons/corruptionweapons.tdf:386-425). The blast has no team filter and is itself an infector for 30 frames (infection.lua:29). areadenial.lua then adds the gas zone (areadenial.lua:30-50, 92). Damage from a gadget's `Explosion` callin is applied regardless of its return value (GameHelper.cpp:269, 303).
- Remake: for the Obelisk, `process_command_fire` spawns the `AreaDenialZone` at the click instantly (units/mechanics/command_fire.rs:818-847; `ability_for` 1630-1644, `explosion_ceg: None`). Nothing pushes a `PendingDamage` for `WeaponKind::Infection` anywhere, so there is no blast and no shell flight. The NX path does have a shell and blast (weapon_fx/tick.rs:826-845).
- In play: upstream an Infection volley also hits every building within 350 elmos for 1500 and every unit for 200, the Obelisk's own allies included (allied buildings take 1500 too). That nearly doubles its damage on a base (1500 + 1600 gas = 3100 per building vs 1600 in the remake). The gas also arrives after the shell's flight, not at the moment of the order.
- Fix: launch Infection like the NX shell, with `delayed_hit` and an `ImpactEffect` that spawns the zone, and queue the `Infection` explosion on impact. Flight and timing belong to the firing auditor; the missing blast is the damage gap.

### 4. Late explosion fronts keep crediting their dead owner, so VirusDeath infects out to 45 elmos instead of about 13–19 — severity: medium · confidence: verified
- Original: a splash victim farther than `4·explosionSpeed` is queued as a `WaitingDamage` holding only the owner's id (GameHelper.cpp:165-171). At delivery the attacker is looked up again, `unitHandler.GetUnit(wd.attackerID)`, which returns null once the unit has been deleted (GameHelper.cpp:74-80). A dying Virus has a Killed script with no waits, so `deathScriptFinished` is set at once and the unit is deleted within about 2 frames (UnitHandler.cpp:442-451, `QueueDeleteUnit`/`DeleteUnits`; Game.cpp:1759-1768 order). With a null attacker, infection.lua gets `attackerID = nil` (LuaUtils.cpp:1955-1966) and does not infect (infection.lua:71). VirusDeath (default 50) uses the default speed of about 3.23 elmos per frame, so only victims within about 12.9 elmos (immediate) or about 19 elmos (≤ 2 frames late) of the blast are infected.
- Remake: delayed splash `Hit`s capture `attacker: Some(Attacker{..})` when queued (damage.rs:802-809, 879-886) and `deliver` infects from that capture with no liveness check (damage.rs:499-510). The Firewall reflection does check liveness (448). The Virus corpse is despawned at the end of its death frame (lifecycle.rs:407-421), yet hits up to 10 frames later still infect.
- In play: a dying Virus marks everything within its full 45-elmo radius for 6 s instead of only what touches it, so Virus chains snowball noticeably harder than in KP.
- Fix: when a `DelayedDamage` hit comes due, keep `attacker` only if the entity still exists (allowing for the engine's roughly 2-frame corpse lifetime), otherwise deliver with `attacker: None`.

### 5. Infected units removed without dying never release their Virus — severity: low · confidence: verified
- Original: infection.lua spawns the Virus from `UnitDestroyed` (infection.lua:46-62). That callin fires for every `DestroyUnit`, including the reclaimed-style removals:
  - a Packet entering the buffer: `Spring.DestroyUnit(u, false, true, u)` (network_enter.lua:44);
  - Bug↔Exploit morphs (LuaCOB.lua:49);
  - a finished Bad Block turning into its feature: `KillUnit(.., reclaimed)` (Unit.cpp:459, 485).
- Remake: these paths call `despawn()` directly, skipping the `Infected` check that only `death_system` makes:
  - units/mechanics/network_buffer.rs:310;
  - units/mechanics/deploy.rs:147;
  - units/features.rs:336.
- In play: a Packet hit by an enemy Virus beam and rushed into a Port/Gateway within 3 s releases a hostile Virus at the teleporter in KP but not in the remake. The same goes for a Bug infected less than 6.7 s before it deploys.
- Fix: in those three removals, push a `VirusSpawn` when the entity carries a live `Infected`.

### 6. Firewall protection is 1.3–1.8 s too short and uses a 3-D radius — severity: low · confidence: verified
- Original: `protectionTime = 20` stamped as `GetGameFrame() + protectionTime * 32` = 640 frames, removed only on frames where `(t+3) % 16 == 0` (network_reflectorshield.lua:20, 51, 103), so 21.3–21.8 s. Targets come from `GetUnitsInCylinder(x, z, 300)`, a 2-D test on `midPos` (LuaSyncedRead.cpp:3075-3089, 2948).
- Remake: `FIREWALL_DURATION = 20.0` s (units/mechanics/command_fire.rs:203). The reload got the ×32/30 conversion, but this duration did not. `apply_firewall` uses 3-D `distance_squared(center)` (command_fire.rs:1192).
- In play: protected units lose the 50 % shield about 7 % early. Flows in the air or units on a different terrain level near the 300 edge aren't protected.
- Fix: `FIREWALL_DURATION = 20.0 * 32.0 / 30.0`, ideally ending on the 16-frame check, and test the XZ distance.

### 7. The NX fire zone appears even when the Pointer died before its shell landed — severity: low · confidence: verified
- Original: areadenial.lua only adds a zone if `ownerID and Spring.ValidUnitID(ownerID)` (or the weapon is SIGTERM) and returns early otherwise (areadenial.lua:39-49). A projectile's owner is looked up by id at impact (Projectile.cpp:199-206) and is passed as `nil` once the unit has been deleted (LuaHandle.cpp:2283-2287).
- Remake: `ImpactEffect::NxZone` always spawns the zone (weapon_fx/tick.rs:834-845). `tick_area_denial` then hands it to a homebase (command_fire.rs:1256-1257).
- In play: a Pointer killed during its NX shell's flight (up to about 3.5 s at 1400 elmos) still leaves a 60 s, 100 dps fire zone in the remake; in KP it leaves only the shell's 200-damage blast.
- Fix: skip `nx_zone` when the attacker entity no longer exists at impact.

### 8. A unit already at 0 HP keeps taking hits for the rest of the frame, and death chains advance one frame per link — severity: low · confidence: verified
- Original: `DoDamage` returns immediately for `isDead`, and `KillUnit` runs inside the lethal `DoDamage` (Unit.cpp:1315, 1369-1372). Later hits that frame are ignored, and a death explosion's victims die, and explode, within the same frame.
- Remake: `deliver` skips only units with the `Dying` marker (damage.rs:415). `Dying` is inserted by `death_system`, which runs after `apply_damage` (units/mod.rs:279-293). Within one pass, hits on a unit already at or below 0 HP still insert `Infected`, queue Firewall reflections and feed the AI log. Victims killed by death blasts are only noticed by the next frame's `death_system`.
- In play: occasional "posthumous" infections (a Bit's lethal shot followed by a Virus beam in the same frame spawns a Virus that wouldn't spawn upstream), and Virus or building chain reactions resolve one frame per link.
- Fix: in `deliver`, also return when `health.current <= 0`, or mark the kill at once.

### 9. Self-destruct and kamikaze timers ignore paralysis, and a second Ctrl+D re-arms instead of cancelling — severity: low · confidence: verified
- Original: `SlowUpdate` returns in its stunned branch before both the `selfDCountdown` decrement and `SlowUpdateKamikaze` (Unit.cpp:1003-1017, 1019-1028, 1103). A second `CMD_SELFD` cancels a running countdown (`if (owner->selfDCountdown != 0) owner->selfDCountdown = 0;`, CommandAI.cpp:964-972). `ExecuteStop` doesn't touch it (CommandAI.cpp:1531-1541). The kamikaze check also runs only every 15 frames.
- Remake: `tick_self_destruct` and `tick_kamikaze` have no `Stunned` filter, and the kamikaze runs every frame (lifecycle.rs:201-230, 392-403). `SimCommand::SelfDestruct` always inserts a fresh countdown (net/apply.rs:758-764), and `Stop` removes it (net/apply.rs:693-699).
- In play: pressing Ctrl+D again restarts the countdown instead of aborting it. A paralysed self-destructing unit or Logic Bomb still blows up. (Stop-cancels is documented in FEATURES.md §3 as a remake choice; the toggle and the stun pause are not.)
- Fix: make Self-D toggle; skip both timers while `Stunned`; kamikaze on the 15-frame slow-update cadence.

Deliberate or documented deviations (one line each):
- SIGTERM `id==168` checks (homebases ×0.1, a closed Byte taking full damage) are kept as the authors' intent (damage.rs:194-221). In Recoil they are dead code: ids come from the sorted name list (WeaponDefHandler.cpp:28-39), and KP has only about 46 TDF weapons plus the Heroic copies, so no weapon has id 168. In real KP 4.912 homebases take full SIGTERM damage and closed Bytes take 30 %.
- Mine Launch needs ≥ 6100 HP (`MINELAUNCHER_HP_FLOOR`, command_fire.rs:224-228); byte.bos only needs 6000 and lets the Byte die from it.
- Area-denial stand-in owner is the lowest NetId instead of `math.random`, for determinism (command_fire.rs:1297-1316).
- Game over: the losing ally team's units fuse-detonate within 4 s, by the user's rule (lifecycle/game_over.rs:41-71). See Doc drift for the upstream claim.

## Doc drift
- units/lifecycle/game_over.rs:115 says "Upstream destroys a dead team's units". `Spring.KillTeam` → `CTeam::Died` only marks the team dead and turns its players into spectators (Sim/Misc/Team.cpp:255-281). KP loads no `game_end`/`unit_dead` gadget: GADGETS_DIR is `LuaRules/Gadgets/` (LuaGadgets/gadgets.lua:27). So a killed team's units stay alive upstream.
- FEATURES.md §3 (line 128-129): "`Ctrl+D` self-destruct with a 5-second countdown". The code arms (2n+1)·0.5 s: 1.5 s for every KP mobile unit (`SelfDestructCountdown=1`) and 5.5 s for buildings. plan.md:947-950 still says `SELF_DESTRUCT_DELAY = 5.0`; the code has 5.5 plus the per-unit value.
- FEATURES.md §8 "Bits fall over from one DOS hit" and plan.md §1.5 "Bits fall over in one hit": a Bit (600 HP) takes 125 per hit, so about 5–6 hits.
- FEATURES.md §8 shields paragraph ("Shields on secondary factories, Firewall, Terminal, Obelisk, Kernel, and Hole soak damage first") doesn't say these exist only with the ONS mod option. Default KP strips them (gamedata/anydefs_post.lua:9-27) and the remake gates them behind `OnsMode`.
- plan.md §1.6: "DamageModifier applied to every damage event, with values under 0.01 … normalised to 1.0". The code applies the raw value only while ARMORED (damage.rs:423-425, unit_registry.rs:688-690).
- FEATURES.md:662 and DEEP_FEATURES.md (Infection Mechanic, §12) describe Worm splash and Worm-made Viruses, which the code currently doesn't produce (Finding 1).
- combat/lifecycle.rs:129-130: "Units whose FBI lacks `IdleAutoHeal` (value 0) carry no marker". An absent tag defaults to 10/s after 600 frames (spring-tdf/src/unit.rs:333-334), so the Flow and Logic Bomb do idle-heal; only the Debug's explicit `IdleAutoHeal=0` opts out. The behaviour is right; the comment is wrong.

## Checked and matching (one line each, so coverage is visible)
- AoE radius `areaofeffect/2`, at least 1 (WeaponDef.cpp:71, GameHelper.cpp:263) ↔ damage.rs:834.
- AoE distance = distance from the explosion to the default collision sphere's surface (sphere of model radius around `midPos`; CollisionVolume.cpp:304-307, Unit.cpp:237) ↔ spatial.rs:100-102.
- Falloff `(R+.001−d)/(R+.001−d·edge)`, full damage at edge 1 or d < 1 (GameHelper.cpp:147-150) ↔ damage.rs:558-563.
- Directly hit unit: one full-damage `DoDamage` (impact on its surface, so d < 1), excluded from the splash loop, no double count ↔ damage.rs:816-827, 845.
- Splash has no team filter; only `noSelfDamage` spares the owner (GameHelper.cpp:122) ↔ damage.rs:854.
- Default `explosionSpeed` formula and late-front delay `int(d/s)−3` beyond `4·s` (WeaponDef.cpp:439-444, GameHelper.cpp:165-171) ↔ spring-tdf weapon.rs:278-284, damage.rs:879-886.
- Impulse: every default-game weapon sets `impulsefactor=0`, so nothing is pushed (GameHelper.cpp:91-105) ↔ damage.rs:783.
- Armor classes of all 27 default units, signal and badblock → `default` (armor.txt; DamageArrayHandler.cpp:88-96) ↔ definitions.rs:357-380.
- Per-armor damage entry, else `default` (WeaponDef.cpp:416-437) ↔ weapon.rs:217-219. Every KP weapon sets `default` and `areaofeffect`, so the absent-tag defaults (1 / 8) never apply.
- `DamageModifier` only while the script holds `ARMORED`, and every default unit's Create() grace period (19 .bos files checked) ↔ damage.rs:255-281, 423.
- Any positive hit, paralysis included, resets `restTime` (Unit.cpp:1327-1337) ↔ damage.rs:422-433.
- DoDamage order: armor multiple → `UnitPreDamaged` (Firewall) → `HitByWeaponId` → apply → `UnitDamaged` (infection) (Unit.cpp:1320-1347) ↔ damage.rs:400-510.
- `HitByWeaponId` returns a percentage (CobInstance.cpp:288-299); a closed Byte takes 30 % unless stunned, with LUA2 = `IsStunned()` (byte.bos:225-231, LuaSyncedRead.cpp:4208-4210) ↔ damage.rs:210-221.
- Firewall: 50 % taken, 50 % reflected next frame as flat `AddUnitDamage` to a living attacker of another *team*, paralyzers exempt, allies by ally team (network_reflectorshield.lua:121-131; LuaSyncedCtrl.cpp:4385-4405) ↔ damage.rs:444-468, command_fire.rs:1180-1199.
- Infection: weapons and durations 90/180/200/30 frames, `unitTeam ~= attackerTeam`, Viruses immune, any damage, latest infector wins (infection.lua:24-29, 67-78) ↔ weapons.rs:266, damage.rs:499-510.
- Death explosion: `ExplodeAs` at `pos`, owner = the dying unit, hits everyone, a unit being built neither explodes nor runs Killed (Unit.cpp:473-528) ↔ lifecycle.rs:288-354. No KP Killed script has waits or EXPLODE_ON_HIT debris, so immediate removal is right.
- Kamikaze (Logic Bomb) fires a single `SelfDestructAs` (= `ExplodeAs`); trigger distance `kamikazeDistance+25` (UnitDef.cpp:508) ↔ lifecycle.rs:201-230.
- Self-destruct length (2n+1) slow updates, default n = 5 (UnitDef.cpp:438, CommandAI.cpp:971, Unit.cpp:1019-1023) ↔ unit_registry.rs:656-659.
- Auto-heal: `AutoHeal` always, `IdleAutoHeal` once `restTime > IdleTime` (frames), defaults 10/s and 600 frames, none while stunned or being built (UnitDef.cpp:329-331, Unit.cpp:672-689, 1096-1100) ↔ lifecycle.rs:139-160, spring-tdf unit.rs:332-334. Per-frame vs per-15-frame steps average the same.
- Experience (modrules `experienceMult=0`) and flanking (`defaultMode=0`, no FBI override) are off upstream and not modelled.
- Features: Bad Block feature has 400 HP, takes the `default` damage, is immune to paralyzers (Feature.cpp:403-427; features/corpses/badblock.tdf) ↔ damage.rs:404-413, features.rs:202. A finished Bad Block unit turns into the feature without exploding ↔ features.rs:309-338.
- Melee Wormbite damages only its target, with no Explosion event (MeleeWeapon) ↔ damage.rs:828-830 (correct for the bite itself).
- Debug: its Minekiller shot kills the Debug itself (mine armor → 5000), which then fires its own `ExplodeAs=Minekiller` (mineblaster.bos, mineblaster.fbi) ↔ combat/mod.rs:953-973 plus the splash and death path.
- Area denial: radius/dps/ttl/friendly-fire table, flat `AddUnitDamage` through the full pipeline, credited to the owner or a homebase/minifac, SIGTERM zone credited via `LastSignalTeam` (areadenial.lua:24-93) ↔ command_fire.rs:494-505, 1240-1300.
- Mine Launch cost: `AddUnitDamage(u, 6000, 0, -1, sigterm)`, no attacker (LuaCOB.lua:116-118) ↔ command_fire.rs:1167-1176.
- BugCannon dynamic damage, inverted, exp 1, range 700 (DamageArray.cpp:82-115) ↔ weapon.rs:523-537. Minor: the remake uses the fire-time 3-D muzzle→aim distance; the engine uses the 2-D launch→impact distance (WeaponProjectile.cpp:197).
- Network beams (Packet, Gateway, Gauss) set `minIntensity=1`, so they have no range falloff, matching the remake.
- ONS shields: stripped by anydefs_post.lua when `ons` is off; the remake attaches them only under `OnsMode` (shield.rs).

## Unverified leads
- Firing side, for the firing auditor: non-`beamburst` BeamLasers spread damage over `int(beamtime·30)` frames and can't start a new salvo until it ends (BeamLaser.cpp:104-111, Weapon.cpp:434). DOS_Beam cycles every 8 frames (remake 7), and GaussCannon's 4500 lands over 12 frames rather than at once. The remake fires one full hit per reload (combat/mod.rs:586-692).
- Area denial measures the unit's base position (command_fire.rs:1280) where `GetUnitsInSphere` uses `midPos` (LuaSyncedRead.cpp:2948), and a zone ticks ttl−1 times instead of ttl. Both are probably negligible; a per-frame trace would settle it.
- Logic Bomb `CheckForSurvivors` → `emit-sfx 4096` (end_game_logic_bomb, mine=2000, then a chain of logic_bomb death blasts) isn't ported. The remake's game-over fuse rule may make it moot; it would matter only if a team keeps factories but loses every non-mine unit.
- The upstream Signal is a 600 HP unit and can be killed by splash (big death blasts, SIGTERM) before it drops its bomb. The remake's Signal isn't a unit. Settle by checking `airstrike.lua` flight altitude against KP blast radii.

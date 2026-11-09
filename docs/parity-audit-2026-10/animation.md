# Unit scripts and animation parity audit

Scope: all 27 default-game scripts (no FBI has a `script=` tag, so every unit uses `scripts/<unitname>.cob`), compared entry point by entry point with the Rust drivers in `kernel-panic/src/units/assets/animation/units/`, the host-side code that stands in for script entry points, and the engine (`upstream/RecoilEngine/rts`). Constants were checked against the compiled `.cob` files with a small disassembler (`tools/cobdis.py`), and model hierarchies with `tools/s3otree.py`. Linear scale per compiled script: 163840 for kernel, assembler, bit, byte, socket, badblock and logic_bomb; 65536 for pointer, hole, terminal, worm, connection, carrier, dos, bug, exploit, obelisk and signal. The remaining scripts contain no `move`.

Engine timing used throughout: scripts tick with `static constexpr int tickMs = 1000 / GAME_SPEED;` (Game.cpp:1777, 33 ms). `SLEEP` sets `wakeTime = cobEngine->GetCurrTime() + r1;` (CobThread.cpp:261), and a thread only wakes when `currentTime` has passed `wakeTime` (`if (zzzThread->GetWakeTime() >= currentTime)` breaks, CobEngine.cpp:149). So `sleep N` lasts floor(N/33)+1 ticks: 30→1, 60→2, 100→4, 120→4, 200→7, 250→8, 500→16 ticks.

Quotes are verbatim substrings (leading whitespace dropped). `.bos` files have CRLF line endings.

## Findings (most important first)

### 1. Packets dispatched from a Port keep ARMORED (×0.000001 damage) for 5 s instead of ~0.53 s — severity: high · confidence: verified
- Original: `network_dispatch.lua:61` calls `Spring.CallCOBScript(nu, "Spawned", 0)` on every Packet it creates. `packet.bos:55-58` `Spawned() {` / `emit-sfx 1025 from base;` / `sleep 500;` / `set ARMORED to 0;` drops ARMORED 16 ticks (0.53 s) after the dispatch. Without that call, `Create()` would keep it until `sleep 5000;` / `set ARMORED to 0;` (packet.bos:17-18). `packet.fbi:54` has `DamageModifier=0.000001;`, and `ARMORED` multiplies damage by it (UnitScript.cpp:1655-1657).
- Remake: `units/mechanics/network_buffer.rs:266` `let spawned = crate::units::lifecycle::spawning::spawn_unit(` spawns the Packet with no `Spawned()` equivalent. `units/combat/damage.rs:270` `| UnitKind::Packet` (in the 5.0 s group) plus `damage.rs:309` `after_build: script.after_build,` arm every new Packet, dispatched ones included, for 5 s.
- In play: a Network player dispatches a Packet swarm out of a Port next to enemies. Upstream the Packets can be shot after half a second. In the remake they take a millionth of the damage for 5 s, enough to reach and fire on defences almost unhurt.
- Fix: in `process_dispatch`, give dispatched Packets `Armored { after_build: 16.0 / 30.0 }` (and emit `network_spawn` at `base`, see finding 9).

### 2. Bug ↔ Exploit morph is instant; upstream it runs the scripts' Deploy()/Undeploy() animations and gates — severity: high · confidence: verified
- Original: the Deploy command calls the COB function `CobFuncName = "DeployNoTarget",` (specialattack.lua:64), which runs `Deploy()` (bug.bos:20-47):
  - It waits `while(!morphable){` / `sleep 500;` (bug.bos:24-25). `morphable=1;` only comes after `sleep 3000;` (post-build) plus `sleep 2000;` (bug.bos:75, 83-84): about 5 s after a Bug leaves its factory, or 2 s after an Exploit unpacks into a Bug.
  - Then it sets `deployed=1;` (bug.bos:27), which makes `AimPrimary` return 0 so the Bug stops firing, and `set MAX_SPEED to 1;` (bug.bos:31), which stops it.
  - It flips the body: y +8 at 32 elmos/s, then −12 at 64 elmos/s, about 21 ticks with waits.
  - It dissolves through `for (i = 72; i > 5 ; --i) {` with `sleep 30` each (67 ticks).
  - Only then `call-script lua_SwitchToExploit();` (bug.bos:46) queues the swap (`table.insert(replaceQueue, {unit=u, team=team, target="exploit" })`, LuaCOB.lua:86, done in the next GameFrame). Total ≈ 88 ticks ≈ 2.9 s.
  - Reverse: `Undeploy()` sets `deployed=1` (Exploit stops aiming) and waits `while(!idle) sleep 30;` (exploit.bos:23). `FireWeapon1` sets `idle=0;` / `sleep 5000;` / `idle=1;` (exploit.bos:91-93), so an Exploit can only pack up 5 s after its last shot. Then `for (i = 5; i < 72 ; ++i) {` (67 ticks) and the body moves, then `call-script lua_SwitchToBug();` (exploit.bos:44): again ≈ 2.9 s.
- Remake: `units/assets/animation/units/bug.rs:1-2` "The morph itself is" / "host-side (despawn + spawn with carried HP)". `units/mechanics/deploy.rs:147` `ctx.commands.entity(event.entity).despawn();` then `deploy.rs:149` `let spawned = spawn_unit(` in the same tick the order arrives. There is no `morphable` wait, no `idle` lock, no stop or fire-stop during the transition, and no animation. The pair also spawns with the default heading, while upstream copies the old unit's heading (`Spring_SetUnitCOBValue(nu, 82, Spring.GetUnitHeading(u.unit))`, LuaCOB.lua).
- In play: a Bug in the remake can deploy the moment it leaves the factory and switch form instantly. A firing Exploit can pack up and run at once. Upstream the Bug has to wait ~5 s after it is built, then stands still and silent for ~2.9 s while deploying; an Exploit under fire can't retreat until 5 s after its last shot plus ~2.9 s.
- Fix: run the morph as a timed state on the source unit: wait for `morphable`/`idle`, hold speed ≈ 0 and aim false for the 88-tick sequence, then swap and keep the heading.

### 3. Bug's `AimPrimary` (the engine's AimWeapon1) is not ported: no body swivel, no yaw fire gate, no 3 s return — severity: medium · confidence: verified
- Original: the engine maps the legacy name to weapon 1: `scriptMap.emplace("AimPrimary",` (CobScriptNames.cpp:82). bug.bos:98-107:
  - `AimPrimary(h,p) {`
  - `if(deployed)` return 0
  - `start-script ResetAim();`
  - `turn body to y-axis h speed <720>;`
  - `turn body to x-axis 0-p speed <360>;`
  - `wait-for-turn body around y-axis;`
  - `return (!deployed);`

  `ResetAim()` turns the body back to 0 at 360°/s after `sleep 3000;` (bug.bos:89). In bugNEW.s3o, `body` (60 verts, the visible shell) carries `nose`, the `QueryPrimary` muzzle. So upstream the Bug's body swivels onto each target at 720°/s, a shot waits for that yaw turn, and the body swings home 3 s after the last aim.
- Remake: `units/assets/animation/units/bug.rs:31-32` "aim is body-driven by the host (the" / "script has no `AimWeapon1`)". `BugAnim` has no `aim()`. `units/assets/animation/units/mod.rs:308` `pub fn has_aim_weapon(kind: UnitKind) -> bool {` lists Assembler, Bit, Byte, Pointer, Exploit, Worm, Virus, Dos, LogicBomb, Obelisk, Connection, Packet, Signal, Gateway and Flow, but not Bug, so no `AimScript` is inserted and nothing gates or animates the Bug's shots.
- In play: the Hacker swarm unit never turns its body toward what it shoots, including while running past targets. Upstream the body visibly snaps around (up to 0.25 s for 180°) before each first shot and settles back 3 s later.
- Fix: add `Bug` to `has_aim_weapon` and give `BugAnim` an `aim()` that turns `body` y→h at 720°/s and x→−p at 360°/s, returns `turn_settled(body, Y)`, and runs a 3 s ResetAim (91 ticks).

### 4. Carrier: upstream `INBUILDSTANCE` stays set after the first Activate, so later reactivations build at once; the remake waits for the 2 s pad lift every time — severity: medium · confidence: verified
- Original: carrier.bos:97-99 `move mover to y-axis [16] speed [8];` / `wait-for-move mover along y-axis;` / `set INBUILDSTANCE to 1;`. `Deactivate()` (carrier.bos:104-111) only does `move mover to y-axis 0 speed [12];` and never clears the stance. The only engine write of the flag is the script's (`unit->inBuildStance = (param != 0);`, UnitScript.cpp:1582). On reactivation, `CFactory::Update` calls `script->Activate(); // set buildStance` (Factory.cpp:141) and then, in the same update, `if (yardOpen && inBuildStance && !IsStunned()) {` (Factory.cpp:149), so `StartBuild` runs immediately while the pad is still rising.
- Remake: `units/assets/animation/units/carrier.rs:36` `self.in_stance = self.active && rig.at_target(self.pieces.mover, Axis::Y);` and `carrier.rs:49` `self.in_stance = false;`, gated by `units/lifecycle/production.rs:537` `if !producer.unit_spawned && animator.is_some_and(|a| a.driver.is_open() == Some(false)) {`.
- In play: whenever a Carrier has idled 7 s (yard closed) and gets a new order, the remake loses up to 2 s (pad sinks over 1.33 s, rises over 2 s) before the next Packet starts. Upstream production resumes on the spot after the very first lift. With Packets taking ~2 s each, that is about one Packet lost per idle cycle.
- Fix: in `CarrierAnim`, latch `in_stance = true` once the first lift completes and don't clear it in `deactivate()`.

### 5. Trojan's build stance waits for the wrong thing, and every builder's body is snapped to the site — severity: medium · confidence: verified (Trojan gating, body snap) / likely (Assembler nozzle mis-aim)
- Original:
  - Trojan, trojan.bos:42-53: `turn center to x-axis 0-p speed <120>;` (never waited on), then `while (TURNRATE > h) {` / `set HEADING to 0-TURNRATE + get HEADING;` / `sleep 30;` and the mirror loop, then `set INBUILDSTANCE to TRUE;`. `#define TURNRATE 720` (trojan.bos:5) is raw COB units per tick, 3.955°/tick ≈ 119°/s. The body turns toward the site at that rate, and the stance (and so `CBuilder::StartBuild` creating the nanoframe) waits for that turn alone: a site at 90° costs 22 ticks, one behind the Trojan 48 ticks (1.6 s).
  - Assembler, assembler.bos:49-50: `turn rotor to y-axis heading speed <120>;` / `wait-for-turn rotor around y-axis;`. Its body never turns; the rotor does.
  - Gateway turns nothing.
  - The engine itself never turns a builder's body for building; `ScriptStartBuilding` only passes body-relative `h`/`p` (Builder.cpp:925-935).
- Remake:
  - `units/assets/animation/units/trojan.rs:80` `fn start_building(&mut self, rig: &mut AnimRig, _heading: f32, pitch: f32) {` ignores the heading, and `trojan.rs:89` `let wait = turn_time(` (of the `center` pitch) feeds `trojan.rs:96` `self.building = Some(wait + 1.0 / 30.0);`, so the stance waits for the pitch turn the script never waits for.
  - Meanwhile `units/lifecycle/construction.rs:786` `// Pin the builder's yaw to face the build site so the beam leaves` / `construction.rs:796` `transform.rotation = Quat::from_rotation_arc(Vec3::Y, up) * yaw;` snaps every builder (Assembler, Trojan, Gateway) to face the site on every tick.
  - Likely side effect: `units/lifecycle/script_triggers.rs:66` `gtf.rotation(),` computes the StartBuilding heading from the `GlobalTransform`, which is only refreshed from `Transform` in `FixedFirst` (rendering/interpolation.rs:14-15). So on the tick the nanoframe is placed and the yaw pinned, the Assembler gets the pre-snap relative heading and its rotor turns by it on top of the already-turned body. Its nozzle (and the `emit-sfx 2048` beam from `tip`) then points that far off the site for the whole build.
- In play:
  - A Trojan told to build beside or behind itself starts the structure up to ~1.5 s earlier than upstream and never shows its ~119°/s turn.
  - Assemblers and Gateways spin their whole hull to face each site, where upstream they stand still (the Assembler swings only its nozzle turret).
  - An Assembler ordered to build next to itself while standing still likely sprays its beam off to the side.
- Fix: drop the yaw pin for builders. Give `TrojanAnim` the heading loop (step `GroundMover::heading` by 720 per tick until within 720, then set the stance; don't wait for the pitch). Compute the StartBuilding heading from the sim `Transform`, not `GlobalTransform`.

### 6. Hole fade-in/out takes 92 ticks (3.07 s) upstream, 2.3 s in the remake, so production starts 0.77 s early — severity: medium · confidence: verified
- Original: hole.bos:113-119 `for (i=240; i >= 20; i=i - 10) {` with `sleep 100;` inside (23 iterations × 4 ticks), then `call-script lua_SetAlphaThreshold(1);` / `set INBUILDSTANCE to 1;`. `Deactivate()` mirrors it (`for (i=20; i <= 240; i=i+10) {`, hole.bos:143-146) before the arm hides.
- Remake: `units/assets/animation/units/hole.rs:26-27` `/// Activate()/Deactivate(): 23 × `sleep 100` alpha steps.` / `const FADE_TIME: f32 = 2.3;`, used by `Phase::FadeIn | Phase::FadeOut => FADE_TIME,` (hole.rs:85). The Pointer driver does round correctly (`const OPEN_DELAY: f32 = 7.0 / 30.0;`, pointer.rs:33), so this is a per-driver slip.
- In play: every time a Hole (re)opens its yard, its first Bug/Worm starts 0.77 s earlier than upstream, and the nano-arm vanishes 0.77 s early on close. The same sleep rounding gives smaller drifts elsewhere: Window hourglass lap 1.5 s vs 1.375 s; Worm wave step 133 vs 120 ms (finding 10); Obelisk reload 1213 ticks (40.43 s) vs `RELOAD_SECS` 40.0.
- Fix: `FADE_TIME = 92.0 / 30.0`. More generally, convert every `sleep N` to `(N/33 + 1)` ticks.

### 7. A fresh Worm is cloaked (and crouched) from spawn; upstream it cloaks ~6.4 s after its build — severity: medium · confidence: verified
- Original: worm.fbi has no cloak tags. worm.bos:108-116 `sleep 5000;` / `set ARMORED to 0;` / … / `move head to y-axis [-16] speed [12];` / `sleep 1333;` / `set WANT_CLOAK to TRUE;`. The engine cloaks on `const bool newCloak = wantCloak && GetNewCloakState(stunCheck);` (Unit.cpp:2525). So a new Worm stands visible with its head up for 5 s (ARMORED), sinks over 1.33 s, and only then cloaks (next SlowUpdate).
- Remake: `units/lifecycle/spawning/mod.rs:534` "Logic Bombs spawn cloaked per their FBI; the Worm starts its" / `spawning/mod.rs:536` `if kind == UnitKind::Worm || unit_registry.init_cloaked(kind) {` inserts `Cloaked` at spawn. `units/assets/animation/units/worm.rs:140` `&& ctx.build_percent <= 0` crouches the head the moment the build ends.
- In play: upstream an enemy watching a Hole/Window sees each new Worm come out and can target it for ~1.3 s once ARMORED drops. In the remake new Worms are invisible to undetecting enemies from the first frame.
- Fix: insert `Cloaked` (and start the crouch) 5 s + 1.33 s after `Emerging` ends, unless an aim surfaced it first.

### 8. Assembler nozzle extends 8 elmos at 16/s; the bytecode says 20 elmos at 40/s (163840 scale) — severity: low · confidence: verified
- Original: assembler.bos:47 `move nozzle to z-axis [8] speed [16];` compiles to `pushc 2621440 / pushc 1310720 / move nozzle z` (40 elmos/s to 20 elmos). assembler.bos:69 `move nozzle to z-axis 0 speed [8];` is 1310720 (20 elmos/s). The same file's emerge `move base to y-axis ([-8]*(get BUILD_PERCENT_LEFT)/100) now;` (assembler.bos:10) is −1310720 = −20, which the driver gets right.
- Remake: `units/assets/animation/units/assembler.rs:14-17` `const NOZZLE_OUT: f32 = 8.0;` / `const NOZZLE_OUT_SPEED: f32 = 16.0;` / `const NOZZLE_HOME_SPEED: f32 = 8.0;`, while `assembler.rs:117` uses `emerge_lift(rig, self.pieces.base, 20.0, ctx.build_percent);`.
- In play: the System constructor's nozzle pokes out 2.5× less than upstream, so its build beam starts 12 elmos further back. Timings are unchanged (0.5 s out, 1 s back).
- Fix: `NOZZLE_OUT = 20.0`, `NOZZLE_OUT_SPEED = 40.0`, `NOZZLE_HOME_SPEED = 20.0`.

### 9. Script-emitted CEGs are drawn as generic puffs or not at all; Socket con-lasers get a sparkle they shouldn't — severity: low · confidence: verified
- Original (`emit-sfx 1024+n` plays `explosiongenerator<n>`; `2048+n` fires weapon n+1):
  - DoS: `emit-sfx 1024 from ground;` while moving (dos.bos:36), with `explosiongenerator0=custom:corruption_dust;` (dos.fbi:76).
  - Gateway: per `BuildFX` pass, four `emit-sfx 1024 from center;` at 0/90/180/270° plus `emit-sfx 1025 from center;` (gateway.bos:52-59), with `explosiongenerator0=custom:network_build;` / `explosiongenerator1=custom:network_build1;` (gateway.fbi:83-84).
  - Firewall: `Charge() {` / `charged=1;` … `emit-sfx 1024 from base;` / `sleep 120;` (firewall.bos:24-28), called by `Spring.CallCOBScript(u, "Charge", 0)` when the ability is ready (network_reflectorshield.lua:63), with `explosiongenerator0=custom:network_charge;`.
  - Packet: `Dissolve()` emits 1024 (network_enter.lua:48 → `network_dissolve`); `Spawned()` emits 1025 (`network_spawn`).
  - Logic Bomb: `emit-sfx 1024 from base;` every tick while built (logic_bomb.bos:27, `oldskool_build`).
  - Socket con-lasers: `emit-sfx 2051 from claser0;` (socket.bos:41) fires weapon 4, `Weapon4=BuildLaserNoEffect;` (socket.fbi:72), whose `explosiongenerator=custom:none`.
- Remake:
  - `dos.rs:70` `rig.emit(self.pieces.ground, SfxKind::Puff);`.
  - `gateway.rs:67` / `:70` `rig.emit(self.pieces.center, SfxKind::Puff);`, one direction per 250 ms step.
  - Firewall `Port | Firewall | Debug => Box::new(NoAnim),` (units/mod.rs:72).
  - No Packet dissolve/spawn emit anywhere: `CegKind::CorruptionDust`, `NetworkBuild`, `NetworkBuild1`, `NetworkCharge`, `NetworkDissolve` and `NetworkSpawn` are defined in `ceg_defs/mod.rs` but never emitted.
  - Logic Bomb driver emits nothing.
  - Socket `socket.rs:168` `rig.emit(claser, SfxKind::BuildBeam);` is drawn as `weapon: crate::units::content::weapons::WeaponKind::BuildLaser,` (animation/mod.rs:1038), and `fn spawns_build_sparkle(weapon: WeaponKind) -> bool {` / `weapon == WeaponKind::BuildLaser` (weapon_fx/spawn.rs:36-37) adds the `oldskool_build` square at every con-laser hit.
- In play: no corruption dust behind moving DoS units; Gateway construction shows small faction puffs instead of the network build sparks in four directions; no glow marking a charged (ready) Firewall, which is gameplay information; no flash when Packets enter or leave a Port; producing Sockets sprinkle build squares upstream doesn't draw.
- Fix: emit `SfxKind::Ceg(..)` with the FBI's generators (Gateway: all four directions and the 1025 each pass). Give the Firewall a driver with `charge`/`discharge` hooks driven by the Firewall ability state. Emit dissolve/spawn in `process_enter`/`process_dispatch`. Let `SfxKind::BuildBeam` carry the weapon index so 2051 maps to `BuildLaserNoEffect`.

### 10. Worm crawl wave: wrong period, step length and tail phase; aim flattens the wave, stop cuts it — severity: low · confidence: verified
- Original:
  - worm.bos:24-47 `while (doMove) {` runs 7 steps, each followed by `sleep WALK_INTERVAL` (`#define WALK_INTERVAL 120`, 4 ticks): a 28-tick (0.93 s) lap.
  - `move head to z-axis 0-WALK_WAVEDIST*3 speed WALK_WAVESPEED;` and `move ring0 to z-axis WALK_WAVEDIST speed WALK_WAVESPEED;` run on step 1, each later ring one step behind, and `move end to z-axis WALK_WAVEDIST speed WALK_WAVESPEED;` on step 7. So `end` trails ring5 by one step.
  - The loop condition is only re-checked at the top, so after `StopMoving` the lap finishes.
  - `AimWeapon1` does not stop the walk; only `FireWeapon1` does `call-script ForceStopWalk();` (worm.bos:188).
- Remake:
  - `worm.rs:14` `const WAVE_INTERVAL: f32 = 0.12;`, `worm.rs:98` `let s = self.phase % 6;` (6-step, 0.72 s lap), and `worm.rs:117` `if (s + 5) % 6 < 3 { -WAVE_DEPTH } else { 0.0 },`, which puts `end` in phase with ring1.
  - `aim()` calls `self.flatten(rig);` (worm.rs:221), snapping every segment flat.
  - `fn stop_moving` (worm.rs:198) resets at once.
- In play: the Worm's crawl ripples ~30% faster with a different tail pattern, and a crawling Worm that starts aiming pops flat instead of rippling on until it bites.
- Fix: port the 7-step table with 4-tick steps; don't flatten on aim; let a stop finish the lap before the three `move … 0` lines.

### 11. Wormsplash detonates at once at the target and the Worm origin; upstream ~11 ticks later at the telescoped head and the tail — severity: low · confidence: verified
- Original: worm.bos:190-200 `move head to z-axis -7*ATTACK_LENGTH speed 7*ATTACK_SPEED;` (head out 112 elmos at 336/s), `wait-for-move head along z-axis;`, then `emit-sfx 4097 from head;` / `emit-sfx 4097 from end;` (detonate weapon 2, Wormsplash, AoE 210). In wormNEW.s3o, head→ring0→…→end is a parent chain, so at that moment `head` is 136 elmos in front of `body` (along the aim yaw) and `end` 24 elmos behind it.
- Remake: `units/combat/mod.rs:1323-1328` `queue_wormsplash(` … `target_pos,` / `attacker_pos,` runs on the fire tick.
- In play: the splash (and its infection window) is centred on the bitten unit, not on a fixed point 136 elmos ahead, and comes 0.37 s early. Bystanders near the target but beyond the head's reach (or vice versa) are hit differently.
- Fix: queue the two hits 11 ticks after the bite at the head and end piece world positions (`DelayedDamage`).

### 12. Connection on a ground target: upstream `gp2` slides back to ~0, the remake slides it out to the ground point — severity: low · confidence: verified
- Original: connection.bos:43-44 `if (get TARGET_ID(1))` / `move gp2 to z-axis (get XZ_HYPOT(get UNIT_XZ(get TARGET_ID(1)) - get PIECE_XZ(gp))) speed [90];`. For a ground target, `TARGET_ID` returns -2 (`case Target_Pos:       return -2;`, UnitScript.cpp:1193), which is truthy, and `UNIT_XZ` of an id ≤ 0 is the unit itself (`return PACKXZ(unit->pos.x, unit->pos.z);`, UnitScript.cpp:1062). `gp` sits straight above the origin (network_big.s3o: `correcter` at (0,48,0)), so `gp2` returns to ~0 and the shot drops from 200 elmos above the Connection.
- Remake: `units/assets/animation/units/connection.rs:94-95` `if let Some(dist) = ctx.target_xz_dist {` / `rig.move_to(self.pieces.gp2, Axis::Z, dist, GP2_SPEED);`. The distance is set for every aim target (`target_xz_dist: Some((target.pos - gtf.translation()).xz().length()),`, combat/aim.rs:436).
- In play: on attack-ground the remake's beam origin hangs over the target instead of over the Connection, and the first shot waits for a different slide (from the last extension out to the ground range, not back to 0 at 90 elmos/s).
- Fix: pass 0 (distance to self) when the aim target is a position.

### 13. Kernel/Socket Deactivate: the script's `YARD_OPEN` loop never runs upstream, but the remake adds 300 ms — severity: low · confidence: verified
- Original: kernel.bos:277-280 `while(get YARD_OPEN)` … `sleep 300;`, and the same in socket.bos:105-107. The engine closes the yard before the callin (`groundBlockingObjectMap.CloseBlockingYard(this);` / `script->Deactivate();`, Factory.cpp:164-165), and `CloseBlockingYard` sets `object->yardOpen = false;` (GroundBlockingObjectMap.cpp:245). `YARD_OPEN` reads `unit->yardOpen` (UnitScript.cpp:1109-1110), so the loop is skipped: the Kernel's first pillar starts folding and the Socket's `building=0` happens at once.
- Remake: `kernel.rs:34` `const YARD_CLOSE_DELAY: f32 = 0.3;` used as `timer: YARD_CLOSE_DELAY,` (kernel.rs:386). `socket.rs:33` same constant, `self.closing = Some(YARD_CLOSE_DELAY);` (socket.rs:181).
- In play: Kernels fold 0.3 s late; Sockets fire ~5 extra con-laser pairs after closing.
- Fix: drop the 0.3 s in both drivers.

### 14. ResetAim returns missing for Packet and Exploit turrets; Obelisk segments close at the end of the reload instead of 2 s after the last aim — severity: low · confidence: verified
- Original:
  - packet.bos:29-37 `sleep 1500;` / `turn turret to y-axis 0 speed <720>;`, started by `start-script ResetAim();` in every `AimWeapon1`.
  - exploit.bos:62-64 `sleep 3000;` / `turn turret to y-axis 0 speed <180>;` (started at exploit.bos:80).
  - obelisk.bos:41-43 `sleep 2000;` / `move segf to z-axis 0 speed [8];`, started at obelisk.bos:57. `AimWeapon1` returns before restarting it while reloading (`if(reloading) return 0;`, obelisk.bos:54), so the segments close ~2 s after the shot even though the reload is `sleep 40000;`.
- Remake: `packet.rs` and `bug.rs` (`ExploitAnim`) have only `aim()`, no reset timer. `obelisk.rs:74-79` closes the segments only when the reload ends (`if self.reloading {` … `self.segments_to(rig, 0.0, 8.0);`), and never after an aim that didn't fire.
- In play: Packet and Exploit turrets keep pointing at their last target. Obelisks stay split open for the whole 40 s reload, or forever after an aborted aim.
- Fix: add ResetAim timers (Packet 46 ticks, Exploit 91, Obelisk 61 from the last aim call).

### 15. Bad Block build-laser sweep pauses 0.75 s at home every lap and skips the first leg — severity: low · confidence: verified
- Original: badblock.bos:21-31 loops x-out (`move blaser0 to x-axis [-12] speed [32];`, 30 elmos at 80/s), z-out (`wait-for-move blaser0 along z-axis;`), snap home (`move blaser1 to z-axis 0 now;`), then straight back to x-out: an ~0.8 s lap with no pause.
- Remake: `units/assets/animation/units/mod.rs:153` `self.leg = (self.leg + 1) % 4;` with legs 2 and 3 both `// snap home (x 0 now; z 0 now) and hold one leg` (mod.rs:164), giving a 1.5 s lap parked at home half the time. `timer: half_width / speed,` (mod.rs:133) without issuing leg 0 means the first x-out never happens.
- In play: the beams of a Bad Block under construction sit on one spot half the time instead of tracing the square continuously.
- Fix: two moving legs then the snap with zero hold, i.e. Socket's `% 3` pattern; issue leg 0 at start.

### 16. Bit StopMoving snaps the ball upright; upstream it stops where it is — severity: low · confidence: verified
- Original: bit.bos:32 `stop-spin body around x-axis;`. With decel 0 the engine removes the spin and leaves the angle (`if (decel <= 0.0f) {` / `RemoveAnim(ASpin, animInfoIt);`, UnitScript.cpp:491-492).
- Remake: `units/assets/animation/units/bit.rs:62-64` `// StopMoving(): stop-spin; snap the seam back upright.` / `rig.turn_deg(self.pieces.body, Axis::X, 0.0, 0.0);`.
- In play: every Bit that stops pops its rolled shell back to the same orientation.
- Fix: delete the `turn_deg` line.

## Doc drift
- `docs/script-and-weapon-parity.md:98` "extends the nozzle 8 elmos": it is 20 (finding 8).
- `docs/script-and-weapon-parity.md:102` "`CBuilder::UpdateBuild` adds build power only while in stance.": the engine has `if (!(inBuildStance || true))` (Builder.cpp:339), so UpdateBuild always adds power. Only `StartBuild` (creating the nanoframe) waits for the stance.
- `docs/script-and-weapon-parity.md:137` "Hole 2.3 s, Carrier" / "2 s": the Hole takes 3.07 s (finding 6); the Carrier only waits on its first activation (finding 4).
- `docs/script-and-weapon-parity.md:141` "scripts' `YARD_OPEN` loop adds ≥ 300 ms.": it adds nothing (finding 13).
- `docs/script-and-weapon-parity.md:165` "Hacker and Network buildees still alpha-fade in": the code no longer fades anyone (`pub fn emerge_style(self) -> EmergeStyle {` / `EmergeStyle::Rise`, units/components.rs:95-96).
- `docs/script-and-weapon-parity.md:182-184` "`ARMORED` windows (Pointer 4 s, Byte 6 s after build, buildings under" … "are not modelled.": they are modelled (`units/combat/damage.rs` `Armored` / `update_armored`). The Pointer's `STANDINGMOVEORDERS` part is still true.
- `units/assets/animation/mod.rs:19-20` "an X translation is mirrored (Spring's engine negates piece X offsets" / "when loading s3o; our parser keeps them verbatim)": Recoil's S3OParser keeps X (`piece->offset.x = fp->xoffset;`). The negation is `CobInstance::Move`'s COBWTF (`if (axis == 0)` / `destination = -destination;`, CobInstance.h). The result is right, the reason given is not.
- `units/assets/animation/units/bug.rs:31-32` "the script has no `AimWeapon1`": it has `AimPrimary`, which the engine maps to weapon 1 (finding 3).
- `units/assets/animation/units/connection.rs:31-32` "upstream only slides" / "for unit targets; on a ground order `gp2` keeps its last extension": upstream slides to ~0 (finding 12).
- `units/assets/animation/units/hole.rs:4` "fade in for 2.3 s (an alpha-threshold": the delay is 3.07 s.
- `units/assets/animation/units/mod.rs:116` "The two-laser square sweep shared by Socket and BadBlock": only BadBlock uses it now.

## Checked and matching (one line each)
- Angle/axis mapping: engine piece rotation `FromEulerYPRNeg(-r)` = Std·Ry·Rx·Rz in s3o space with COB z-turn/z-spin and x-move negated (CobInstance.h). The remake's `Quat::from_euler(EulerRot::YXZ, r[1], r[0], -r[2])` plus X-move negation reproduces it.
- Turns take the short arc (`TurnToward`); move/turn speed is per-second ÷ 30 per tick; `turn … now` with no speed snaps; wait-for latency is one tick (drivers add `WAIT_LATENCY`).
- No default script turns a spinning piece on the same axis, so the framework's missing turn↔spin override never triggers. The only `accelerate` (Pointer `<3600>`) reaches full speed in one tick.
- Kernel: Create rise (−20/−80 at 163840, bases @30, pillars @60/heads @40 after 400 ms, `canbuild`), GetPillarNReady/Rest re-speed cascades, INBUILDSTANCE at the end of GetPillar3Ready, 60 ms tip beams while building, no Killed.
- Byte: 163840 values (base 60 @120, blades ±10 @40, rotor 45 @90, close rotor @480/base @300, aimer 0 @70), Open/Close signal masks, the `isOpen` window used by HitByWeaponId, bp0–bp3 cycling, launcher pose, Killed.
- Pointer: Close/Open timeline (2/7-frame sleeps), isOpen, `<90>-p` pitch, host body turn at FBI TurnRate 270 (= the script's 270/tick), Killed.
- Socket build sweep (±60 @80) and con-laser bob (35 @40); Terminal sweep and body sink 75; Window hourglass/flap/INBUILDSTANCE; Obelisk ±16 build splay, aim part ±8, ChargeFX every 2 ticks, 40 s reload gate.
- Hole arm loop values (−16i @64, mover 0 @256 / −55 @128, return @128) and `corruption_const` strokes.
- DoS slash/center aim and ResetAim; Virus turret and gun toggle; Flow base aim, gp0–gp3 per Shot1, EndBurst, wing spins, ResetAim; Packet turret aim; Connection turns, gp2 slide (unit targets), 24-tick lockAim.
- Signal Create pose and wing unfold (65536); Logic Bomb mine sink −10 (163840); Debug's single Minekiller blast host-side; Gateway stance and lua_BuildArc cadence.
- `ARMORED` windows per kind in `damage.rs::armored_script` match every `Create()` (Bit 2.5, Bug 3 only if built, Pointer/DoS 4, 5 s group, Byte 6, buildings 0; none for homebases, Exploit, Virus, Logic Bomb, Signal, Bad Block, Debug), except finding 1.
- Killed(): explode pieces/flags (SHATTER/FALL) and hides match for every unit that has one. Implicit `return 0` (Scriptor's trailing `clv; return`) means no corpse unless the FBI has one; only `badblock` does, and it's handled as an `IsFeature` conversion. Units dying `beingBuilt` skip Killed and the death blast (`death_system`).
- HitByWeaponId: closed-Byte 30% (unless paralyzed) and homebase 10% vs SIGTERM ported. Virus ids 234/538 rightly not ported.
- StartMoving/StopMoving use the engine's 0.01 speed threshold (`UpdateOwnerSpeed`); factory Activate/Deactivate cadence (210-frame close) matches; `ShowNanoSpray=0` everywhere, so there is no engine nano spray to port.

Deliberate deviations (documented in code; questionable ones marked):
- `id==168` (SIGTERM) treated as intended (damage.rs:203-209). Questionable: under Recoil, weapon-def ids are the sorted-key index (`weaponDefsVector.emplace_back(wdTable, name, wid);`, WeaponDefHandler.cpp) and KP has ~45 defs, so upstream homebases and closed Bytes take full SIGTERM damage.
- `exit_heading` overrides `TurnTowardBarycenter` when the barycentre side lacks room (spawning/mod.rs).
- `Emerging` doubles as spawn protection.
- Connection `correcter` counter-turn not reproduced.
- `turn_settled` 0.09 rad aim tolerance.
- The Terminal's Signal is a host-flown non-unit (`SignalAnim` is unused on that path).

Candidates dropped after re-reading:
- Socket/Terminal/BadBlock laser emitters ending in different places after construction: the pieces have no geometry (0 verts), so it isn't visible.
- Worm `shooting` window shorter (0.66 s vs ~1.07 s): Wormbite `reloadtime=6`, so it has no effect.
- Turn-vs-spin override and spin acceleration missing in the framework: no default script exercises either.

## Unverified leads
- **Alpha threshold (`lua_SetAlphaThreshold`)**, possibly large but unconfirmed. LuaCOB.lua passes `alpha/255`; `gl.AlphaTest` reads it with `luaL_checkint` (LuaOpenGL.cpp:3605, `glAlphaFunc((GLenum)luaL_checkint(L, 1), (GLfloat)luaL_checkint(L, 2));`). So 255 becomes GL_GREATER 1.0 (no fragment passes) and anything below 255 becomes GREATER 0 (opaque). If the compatibility-profile alpha test applies to the legacy model shader (whose output alpha is the tex2 one-bit mask), these are invisible upstream:
  - Packet, Connection, Flow, Gateway, Port and Firewall during their whole build (`call-script lua_SetAlphaThreshold(255);` … `(0)` on completion);
  - the Hole homebase whenever it is idle (`lua_SetAlphaThreshold(255)` in `Create()` and at the end of `Deactivate()`).

  The remake draws them all opaque (components.rs:95-96, parity doc says "inert on Recoil"). Settle: run KP 4.912 on Recoil and look at an idle Hole and a Port under construction.
- Aim cadence: the remake calls `aim()` every tick (combat/aim.rs `drive_aim_script`); upstream re-aims every 15 frames (`CWeapon::CanCallAimingScript`). For example, a Byte's first shot after `Open()` waits for the next re-aim upstream (up to 0.5 s). Combat area; settle by checking whether `AimScript.ready` is refreshed only on the 15-frame cadence.
- `set STANDINGMOVEORDERS to 0` (pointer.bos:158): the remake has no move states and no `CMobileCAI::GenerateAttackCmd` idle chase, so the Pointer's hold position matches by accident while Maneuver units (everyone else) never auto-chase. CAI area.
- Stunned factories: upstream the Kernel's `StartBuilding` thread keeps emitting tip beams while stunned (`UpdateBuild` bails but the thread runs); the remake's `ctx.building` goes false when stunned. Needs a DoS-on-factory check.
- Builder nanoframes appear before INBUILDSTANCE in the remake (construction.rs places them on the first tick and gates progress); the engine creates them only after the stance. Build-time equal; the sunk building shows up ~0.5–1.5 s early.
- Logic Bomb `CheckForSurvivors()` (detonate when no allied non-mine unit is left) is not ported. It looks moot under the default "kill all factories" game-over, which kills the team first.

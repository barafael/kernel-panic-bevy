# Game rules (synced Lua gadgets) parity audit

Scope: `upstream/Kernel-Panic/LuaRules/Gadgets/*.lua` as they run in a default game (GameMode 1, no mod options set), plus `kp_autospam.lua`, the setup data, and the engine code that interprets the calls. Remake paths are relative to `kernel-panic/src/` unless noted.

## Findings (most important first)

### 1. Dispatched Packets keep 5 s of spawn armour instead of 0.5 s — severity: high · confidence: verified
- Original: `network_dispatch.lua:58-62` creates each Packet, gives it the move order, then `Spring.CallCOBScript(nu, "Spawned", 0)`. `scripts/packet.bos:55-58` `Spawned() { emit-sfx 1025 from base; sleep 500; set ARMORED to 0; }`. `Create()` (`:10-18`) only clears ARMORED after `sleep 5000`, so for a dispatched Packet, `Spawned` ends the armour after 0.5 s. `packet.fbi DamageModifier=0.000001`: while ARMORED is set, the Packet takes no real damage (`Unit.cpp:1335 baseDamage *= curArmorMultiple`).
- Remake: `process_dispatch` (`units/mechanics/network_buffer.rs:238-281`) spawns plain Packets. `update_armored` (`units/combat/damage.rs:255-311`) gives every new Packet `Armored { after_build: 5.0 }` (`:270-273`). Nothing models `Spawned()` (no match for "Spawned" in the sim).
- In play: every Dispatch (12 Packets per teleporter, any number of times with Alt) puts Packets into a fight that cannot be hurt for 5 s instead of 0.5 s. This makes "dispatch on an attacker" a much stronger defence than in the original.
- Fix: give Packets created by dispatch a 0.5 s `Armored` (or remove it 15 frames after spawning) instead of the `Create()` grace period.

### 2. The Bug↔Exploit morph is instant — severity: high · confidence: verified
- Original: `CMD_DEPLOY`/`CMD_UNDEPLOY` (`specialattack.lua:53-80`, `:148-185`) call the COB functions `DeployNoTarget` and `SpecialAttack`. `bug.bos:20-47` `Deploy()` first waits until `morphable`, which `Create()` sets 2 s after spawn, or 3 s + 2 s after a factory build (`:58-85`). Then the body moves (≈0.56 s), an alpha loop runs 67 × `sleep 30` (≈2.2 s), and only then `lua_SwitchToExploit`. The Bug is pinned in place meanwhile (`set MAX_SPEED to 1`). `exploit.bos:18-45` `Undeploy()` waits `while(!idle)`, and `FireWeapon1` (`:88-94`) keeps `idle=0` for 5 s after each shot. It then runs the same 67-frame loop plus about 0.56 s of body moves before `lua_SwitchToBug`, and stops aiming while it waits. `LuaCOB.lua:34-54` swaps the unit on the next frame.
- Remake: `SimCommand::Deploy` (`net/apply.rs:766-776`) and `tick_bombard` write a `DeployEvent`. `process_deploy` (`units/mechanics/deploy.rs:110-168`) despawns the unit and spawns its pair in the same tick (`units/mod.rs:194-196`). `animation/units/bug.rs:1-3`: "The morph itself is host-side".
- In play: Bugs become Exploits the moment they leave the Window or on demand. Exploits under attack flee as Bugs instantly, instead of being stuck for 2.8 s, or up to about 7.8 s if they just fired. The glass-cannon trade-off of the Exploit is gone.
- Fix: run the morph as a timed state: wait for `morphable`/`idle`, keep the unit immobile and still targetable for the scripted duration, then swap.

### 3. The Obelisk's Infection lands instantly, with no shell and no blast — severity: high · confidence: verified
- Original: Infection is the Obelisk's real `Weapon1` (`obelisk.fbi`), fired by manual fire (`commandfire=1`). `obelisk.bos:53-62` takes about 1 s to aim (`wait-for-move segf`). The shell is ballistic (`corruptionweapons.tdf:386-427`: `weaponvelocity=800`, `myGravity=.4`, so 1.3–3.1 s of flight). It explodes with `areaofeffect=700` (radius 350), `edgeeffectiveness=1`, `default=200`, `building=1500`, and the CEG `custom:corruption_infection` with trail `corruption_WMDtrail`. Only then does `areadenial.lua:30-52` start the gas zone, at the point of impact.
- Remake: `process_command_fire` (`units/mechanics/command_fire.rs:818-850`) handles the Obelisk with `ability_for` (`:1637-1641`, `explosion_ceg: None`). It spawns the `AreaDenialZone` at the click point on the same tick. There is no projectile, no damage on impact, no CEG and no fire or hit sound. The comment at `:1625` ("Infection's gas has no CEG upstream") is wrong.
- In play: Infection now acts 1–4 s earlier. It also loses its opening blast: 1500 to every building and 200 to every other unit within 350 elmos. That blast is about half of what an Infection does to a base.
- Fix: fire `WeaponKind::Infection` as a ballistic shot from `tip`, the same way `tick_nx_casts` fires the NX shell, and attach an impact effect that spawns the zone where it lands.

### 4. Homebases start with Repeat off — severity: medium · confidence: verified
- Original: `LuaUI/Widgets/kp_autospam.lua:58-105` gives the local player's `kernel`/`hole`/`carrier` `CMD.REPEAT {1}` in `UnitCreated`, and again at frame 2 for the start units (`:113-139`). The widget is enabled by default (`:31`).
- Remake: `default_production` (`units/lifecycle/production.rs:269-272`) gives homebases `Producer::new()` with `repeat: false`. The doc at `:42-45` and the test at `:1087` assert it. `docs/kernel-panic-game-design.md:382` correctly says the home base starts with repeat on.
- In play: a human player who queues units at the Kernel, Hole or Carrier gets them once in the remake, but on an endless loop in the original.
- Fix: start human homebases with `repeat = true`, and keep it off for AI seats, which KPAI handles itself.

### 5. Firewall protection lasts 20 s instead of 640 frames (21.3 s) — severity: medium · confidence: verified
- Original: `network_reflectorshield.lua:20,103` `protected[u] = GetGameFrame() + protectionTime * 32` with `protectionTime = 20`, which is 640 frames. Expired entries are cleared only on frames where `(t+3) % 16 == 0` (`:51,71-75`), so protection lasts 640–655 frames (21.3–21.8 s). The same 32-frames-per-second convention gives the 2880-frame cooldown, which the remake already uses.
- Remake: `FIREWALL_DURATION: f32 = 20.0` seconds (`units/mechanics/command_fire.rs:203`), applied at `:1196` and counted down in `tick_protection`. `LUA_ABILITY_RELOAD` (`:195`) already applies the ×32/30 factor, but the duration does not.
- In play: half damage and reflection end 1.3–1.8 s (7–8 %) early.
- Fix: `FIREWALL_DURATION = 20.0 * 32.0 / 30.0`.

### 6. Alt-dispatch drains 15× too fast, and dispatch ignores blocked spawn spots — severity: medium · confidence: verified
- Original: Dispatch is a `CommandFallback` (`network_dispatch.lua:91-121`). The engine calls it when the order arrives (`CommandAI.cpp:1065-1068`) and then from `CCommandAI::SlowUpdate` (`:1588-1594`, `MobileCAI.cpp:384`), which runs every 15 frames. With Alt the command stays queued, so the buffer leaves at 12 Packets per 15 frames (24/s). Each of the 12 spots is a square ring of ±48/±16 elmos (`:22-35`). A spot is skipped, and its Packet stays in the buffer, if `GetGroundBlocked` finds any unit, building or feature in the 16×16 square, or if `TestBuildOrder` says the terrain is not buildable (`:99-115`). `CUnit::SlowUpdate` also returns before `commandAI->SlowUpdate()` while a unit is stunned or under construction (`Unit.cpp:1004-1059`). A paralysed Connection or an unfinished Port therefore dispatches nothing until it recovers or finishes.
- Remake: `tick_auto_dispatch` (`units/mechanics/network_buffer.rs:215-233`) sends a new `DispatchEvent` every sim frame (12 per frame, 360/s). `process_dispatch` (`:252-279`) always places `min(buffer, 12)` Packets on a circle of radius 48, with no blocking or terrain test and no stun or nanoframe check.
- In play: an Alt-dispatch of 100 Packets appears in 9 frames, stacked at the same 12 spots, instead of streaming out over about 4 s. Packets can appear inside neighbouring buildings, on cliffs or in water. A paralysed Connection can still dispatch.
- Fix: repeat Alt-dispatch on the teleporter's 15-frame slow-update phase. Port the offset table and skip spots that are blocked or not buildable, leaving those Packets in the buffer. Ignore stunned or `Emerging` teleporters.

### 7. NX fire and mines appear even when the caster died before impact — severity: low · confidence: verified
- Original: the projectile's owner is `unitHandler.GetUnit(ownerID)` (`Projectile.cpp:199-205`), and the `Explosion` call-in receives `nil` once the unit has been deleted (`LuaHandle.cpp:2283-2287`). The Pointer's and Obelisk's `Killed` scripts end immediately (`pointer.bos:169-177`). `areadenial.lua:40-50`: without a valid owner, an `nx`/`infection` explosion creates no zone and no CEG; only `sigterm` falls back to `LastSignalTeam`. `Launcher.lua:26-31` likewise creates a Logic Bomb only `if ... and owner`.
- Remake: the impact handler (`units/weapon_fx/tick.rs:834-863`) always spawns the NX zone and always queues the mine for the team recorded at launch.
- In play: a Pointer killed while its NX shell is in the air (several seconds on the high arc) still leaves 60 s of fire. A Byte killed right after `LaunchMines` still lays its mines.
- Fix: in `ImpactEffect::NxZone`/`LogicBomb`, skip the zone or mine when `hit.attacker` is no longer a live unit.

### 8. The Firewall reaches a 3D sphere instead of a 2D cylinder — severity: low · confidence: verified
- Original: `network_reflectorshield.lua:101` `GetUnitsInCylinder(x, z, 300)`. This compares only the horizontal distance from each unit's `midPos` (`LuaSyncedRead.cpp:3087-3088, 2948`).
- Remake: `apply_firewall` (`units/mechanics/command_fire.rs:1192`) uses `gtf.translation().distance_squared(center)`, a full 3D distance from the click point on the ground.
- In play: on slopes, plateaus or ramps, allies within 300 elmos horizontally but more than a few dozen elmos above or below the click are not protected.
- Fix: use `flat_dist_sq` in `apply_firewall`.

### 9. The Worm is cloaked from creation — severity: low · confidence: verified
- Original: `worm.fbi` has no `Init_Cloaked`. `worm.bos:95-117` `Create()` waits out the build, then `sleep 5000` with ARMORED, then lowers its head, then `sleep 1333`, then `set WANT_CLOAK to TRUE`. The engine applies the cloak at the unit's next slow update (`Unit.cpp:2521-2536`). A new Worm is therefore visible to the enemy for about 6.3–6.8 s, the last 1.3 s of it without armour, and in that window `AutoHold` (`:70-83`) lets it fire at will.
- Remake: `spawn_unit` inserts `Cloaked` for every Worm at spawn (`units/lifecycle/spawning/mod.rs:534-540`, comment "starts ... submerged (worm.bos Create)").
- In play: the enemy never sees Worms coming out of the Hole, and a fresh Worm cannot be shot in its 1.3 s exposed window.
- Fix: insert `Cloaked` 5 s + 1.33 s after the build completes, unless the Worm is aiming, as `ResetAim` does.

### 10. Units created by Lua scripts ignore the unit cap, and the Packet buffer is never clamped — severity: low · confidence: verified
- Original: `Spring.CreateUnit` returns nothing when `CanBuildUnit` fails, that is when the team is at its limit or at `maxThisUnit` (`LuaSyncedCtrl.cpp:1871-1872`, `UnitHandler.cpp:495-502`). This covers dispatched Packets, infection Viruses, launcher mines and the SIGTERM Signal. `network_buffer.lua:45-51` also clamps every team's buffer to `MaxUnits - GetTeamUnitCount(team)` every 8 frames, so packets beyond the cap are lost (the HUD shows "MAXED").
- Remake: only factory production (`units/lifecycle/production.rs:645`) and construction (`construction.rs:446`) check `TeamUnitCounts::at_limit`. `process_dispatch`, `spawn_queued_viruses` (`spawning/mod.rs:803-816`) and the Signal don't. `PacketBuffer::add` (`network_buffer.rs:108`) has no clamp.
- In play: once a team reaches 1000 units, Ports keep filling the buffer and Dispatch, infection and mines push it past the cap.
- Fix: clamp the buffer every 8 frames as upstream does, and refuse Lua-path spawns while `at_limit`.

### 11. Enter: a spawn-stunned Packet's order is dropped — severity: low · confidence: verified
- Original: `network_enter.lua:74-90`. While the 180-frame `spawnStun` is active, or while the teleporter is more than 150 elmos away (2D), the command is kept (`return true,false`) and retried every slow update, so the Packet enters as soon as it may. `AllowCommand` (`:63-72`) refuses targets that are stunned or still being built. A stunned Packet does not run its orders at all (see 6).
- Remake: `tick_enter_orders` (`units/mechanics/network_buffer.rs:74-95`) removes `EnterOrder` at 112.5 elmos and sends the event. `process_enter` (`:297`) then silently ignores a Packet with `PacketSpawnStun`. The direct `SimCommand::Enter` (`net/apply.rs:778-785`) absorbs even a paralysed Packet, and nanoframe teleporters count.
- In play: dispatch Packets, then send them back within 6 s: they walk up to the Port and stand there for good.
- Fix: keep `EnterOrder` until the stun expires, and filter out stunned Packets and stunned or `Emerging` teleporters.

### 12. A Firewall Protect issued during recharge is dropped — severity: low · confidence: verified
- Original: `network_reflectorshield.lua:94-95` returns `true,false` while recharging, so a queued Protect (for example a second shift-queued one) fires by itself when the Firewall is ready. SIGTERM, by contrast, is rejected and dropped (`airstrike.lua:101-102,119-120`).
- Remake: `process_command_fire` drops any cast while `CommandFireCooldown > 0` (`units/mechanics/command_fire.rs:692`), for the Firewall as for the others.
- Fix: for the Firewall, keep a pending Protect target and fire it when the cooldown ends.

### 13. Minifac build facing follows a different rule — severity: low · confidence: verified
- Original: `kp_autospam.lua:113-161` sets one `buildfacing` for all of a player's buildings: toward the map centre along the dominant axis, measured from the barycentre of the player's homebases.
- Remake: each datavent has its own stored facing, pointing from the vent toward the map centre, with a terrain veto (`spring-map/src/datavent.rs:1-18`, read by `construction.rs:667`). That doc credits kp_autospam with a per-vent rule, which is inaccurate.
- In play: minifacs on the far half of the map face back toward their builder's side, so their spam leaves toward home rather than toward the enemy.
- Fix: default a human's build facing to the kp_autospam rule, computed from the start position; keep the terrain veto only as a fallback.

### 14. Side effects of the morph and of Enter are skipped — severity: low · confidence: verified
- Original: both swaps go through `Spring.DestroyUnit(u, false, true)`. That still runs `UnitDestroyed`, so an infected Bug, Exploit or Packet spawns a Virus for the infector (`infection.lua:46-59`). `LuaCOB.lua:45-48` also copies the whole command queue to the new unit and makes it non-blocking (`SetUnitBlocking(nu,false)`, `:42`).
- Remake: `process_deploy` and `process_enter` despawn without any infection check. `process_deploy` passes on only the Bombard attack (`deploy.rs:164-165`).
- Fix: run the infected-death Virus spawn on these despawns, and copy the queue on morph.

### 15. Cosmetic gadget output is missing — severity: low · confidence: verified
- Original: `taken_the_lead.lua:97-135` broadcasts "TeamN's <Side> has taken the lead with K kills!" whenever the team with the most kills changes. `Launcher.lua:29` returns `true`, which suppresses the CEG of the mine bolt's impact.
- Remake: the lead message is absent. The mine bolt's impact plays `corruption_shot1` (`weapon_fx/tick.rs:868-881`).

## Deliberate deviations (documented in the remake; questionable ones marked ?)
- ? A defeated or departing ally team's units are fused to blow up within 4 s, and leavers' units stand idle for allies to take (`units/lifecycle/game_over.rs:104-141`). Upstream `Spring.KillTeam` only makes the team's players spectators (`Team.cpp:255-281`); the units stay and keep firing. This matters in free-for-all games.
- Unit caps are not redistributed on death (no `UpdateTeamUnitLimitsOnDeath`; `bookkeeping.rs:185-188`).
- ? Nanoframes (`Emerging`) are left out of the spatial index (`units/spatial.rs:201-207`), so units under construction are immune to splash and area-denial ticks and cannot be targeted. Upstream buildees can be hit.
- Flow: `maxWantedSpeed` follows the boosted `maxSpeed` (`interaction/air_movement.rs:492-494`). In Recoil the bonus is capped by the wanted speed captured at the player's last order (`SelectedUnitsAI.cpp:43-53`, `HoverAirMoveType.cpp:565`), so Flows sent by their factory or by Lua fly at base speed.
- Mine Launcher: the remake keeps a 100 HP floor and refuses to fire at the Logic Bomb cap (`command_fire.rs:225-228,757-762`). Upstream needs only 6000 HP and pays it even when no mine can be laid (`byte.bos:165-183`).
- The area-denial stand-in owner is the lowest NetId rather than `math.random`. Homebases pick their exit heading by heuristic rather than `TurnTowardBarycenter`.

## Doc drift
- `production.rs:42-45`: "homebases start with it off (kp_autospam.lua)". The widget turns it on (finding 4).
- `game_over.rs:115`: "Upstream destroys a dead team's units". It does not (`Team.cpp:255-281`).
- `network_buffer.rs:320-323`: "Upstream caps Flow at MAX_SPEED=75". 75 is the COB variable *index* `MAX_SPEED` (`CobDefines.h:63`), not a cap. `FLOW_MAX_SPEED = 2250` elmos/s is harmless but meaningless. `:331-334`: upstream also updates instantly on building finish/death, not only every 27 frames.
- `command_fire.rs:1625` "Infection's gas has no CEG upstream". It has one (finding 3). `spawning/mod.rs:534-535` "the Worm starts ... submerged (worm.bos Create)". It doesn't (finding 9). `spring-map/src/datavent.rs:3-6` describes the kp_autospam rule wrongly (finding 13).
- `kernel-panic/DEEP_FEATURES.md` §8: "90-second cooldown", "20-second shield", `network_super.fbi`, build time 8000. Actual: 96 s and 21.3 s, `firewall.fbi`, `BuildTime=3200`. §13: "The Carrier also contributes to the Buffer" (it doesn't), "~5.5 s" (5.6 s). §16's implementation checklist is entirely stale. `plan.md:250-251` "Firewall 20s", "SIGTERM 90s cd" (code 96 s). `production.rs:280` "every 5.5s" (5.6 s).

## Checked and matching
- game_spawn: one start unit per side (kernel/hole/carrier) at seat *i*'s start position. The remake clamps to 100 elmos from the edge and does not snap to 8 elmos; I decoded every `.kpmap` and only Spooler Buffer's seats 9 and 10 sit closer than 100.
- game_over: `GameMode` defaults to 1. Life sustainers are the homebase plus Socket/Window/Port, nanoframes included. The census runs at `f%30==25`, the end check at the next census, and the remake reproduces the quirk that the list walk skips the team after a removed one.
- kernelboost: `buildSpeed × (1 + 0.2 × finished small buildings)` for all three homebases. Small buildings are Socket, Window, Port, Terminal, Obelisk, Firewall. Counted on finish, uncounted on death unless still being built, moved with Take.
- network_buffer: +1 per finished Port each 168 frames (164 rounded up to the 8-frame check), the first one on completion, into a team-wide buffer.
- dispatch: up to 12 per order, 180-frame re-entry stun, a move order to the click, non-Alt ends after one batch, Alt runs until the buffer is empty.
- enter: 150-elmo radius, own-team Port or Connection only, walk to 112.5.
- flowspeed formula: base 30 + 30 elmos/s per finished small building, no cap.
- reflectorshield: 300 radius, ally team only, 50 % taken and 50 % reflected next frame to a live attacker of another *team*, paralysers ignored, 2880-frame cooldown started in `UnitCreated` (the nanoframe).
- airstrike: 2880-frame cooldown from `UnitCreated`; refused while recharging; the Signal starts at the Terminal with its build facing and an attack order, and is independent of the Terminal afterwards; it drops its bomb when it dies or after 120 s.
- areadenial table: SIGTERM 350 / 2000 / 100 frames, hurts allies; NX 120 / 100 / 1800 frames, hurts allies; Infection 400 / 120 / 400 frames, enemies only. Damage is `damage/30` per frame through the full damage path (ARMORED, Firewall, infection). The stand-in owner is a homebase, else a minifac, else none.
- infection: windows of 90/180/200/30 frames (virusbeam/virusdeath/wormsplash/infection); the check compares teams, not ally teams; anything but a Virus can be infected, buildings included; the Virus spawns at the death spot for the infecting team.
- Launcher: one Logic Bomb per bolt while under `UnitRestricted=64`. Mine launch: 5 tubes, 6000 HP via `AddUnitDamage` credited to `sigterm`, 10 s reload, blocked while ARMORED.
- specialattack NX: reload 30 s. autohold: on for humans, off for AI teams, inherited from the Hole. bug_bombard: 1150 elmos (3D), deploys, and attacks unless Alt is held.
- MaxUnits 1000 (EngineOptions default).
- kp_autospam minifacs: Socket→Bit and Window→Bug on repeat; the remake applies this to all teams, documented.
- Not active in a default game, or no rule effect: Burrow.lua (wormold/bugold only); Armor_Bonus (removes itself without a lobby bonus; the remake has no bonus option); evilless (options off); hotfixes, pseudo_orders, spawn_units, LuaUI_force, set_alpha_threshold, init_start_point_remover, new_cmd_id (shims, mission or cosmetic); RegenAI (AI only).
- MetalToGeo `auto`: every shipped map has at least 6 merged geo spots (decoded from the `.kpmap` files; Hex Farm makes its own), so no metal-to-geo conversion happens for up to that many players.

## Unverified leads
- The remake's zone countdown (`remaining -= dt; <= 0 → despawn` before damage) probably gives 99 ticks against upstream's 100. Whether it does depends on float rounding; a headless count would settle it.
- Area-denial distances use the unit's base position (`command_fire.rs:1280`), while `GetUnitsInSphere` uses `midPos` (`LuaSyncedRead.cpp:2948`). This shrinks the NX reach on tall buildings by a few percent; the per-kind `mid_y` values would settle it.
- `LastSignalTeam` (`areadenial.lua:24-27`) can credit one team's SIGTERM zone to another team whose Signal died later. This matters only for owner substitution and reflection; the remake binds the zone to the right team. An overlap test would show it.
- With no `MaxUnits` key in the start script the engine default is 32000, clamped to `MAX_UNITS / numTeams` (`GameSetup.cpp:632`). The remake's fixed 1000 assumes a lobby wrote the default.

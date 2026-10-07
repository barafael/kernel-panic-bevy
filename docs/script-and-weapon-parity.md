# Weapon, script and factory details: engine reference for the port

Condensed from a source study of `upstream/RecoilEngine/rts` and
`upstream/Kernel-Panic` (2026-10-07), following
`spring-movement-reference.md`. Port code: `kernel-panic/src/units/combat/`,
`units/weapon_fx/`, `units/lifecycle/production.rs`,
`units/assets/animation/units/*.rs`.

## LaserCannon shots (`Sim/Weapons/LaserCannon.cpp`, `Projectiles/WeaponProjectiles/LaserProjectile.cpp`)

- `projectileSpeed = weaponVelocity / 30` elmos per frame. MegaBeam: 34.13.
- Range rounds *down* to a multiple of the speed
  (`CLaserCannon::UpdateRange`): MegaBeam 512 → 477.9.
- Aim point every frame: the target's `aimPos` (model midpoint, a Bit's
  ball centre 16 above its feet) plus `velocity · predictTime ·
  predictSpeedMod`, with `predictSpeedMod` uniform in `[0, 2]`
  (`predictBoost` 0) re-rolled every `UNIT_SLOWUPDATE_RATE` = 15 frames.
- `FireImpl`: `dir += NextVector() · sin(sprayAngle · π / 0xafff)` then
  normalise; `NextVector` is uniform in the unit ball. MegaBeam 1024 →
  0.0713. No `accuracy`, so no salvo error.
- `ttl = min(ceil(dist / speed), floor(range / speed) − 1)`; the bolt
  checks unit collisions (segment vs. collision sphere, enemies and
  neutrals, `NOFRIENDLIES`) and ground for `(ttl + 1) · speed` elmos,
  explodes where it hits, and simply fades when it hits nothing.
- Every unit within `areaOfEffect / 2` of the explosion takes
  `damage · (1 − d / R)` with `d` the distance to its collision sphere
  (`GetPointSurfaceDistance`); the struck unit has `d ≈ 0` and takes the
  full amount. Nothing is excluded for having been the target.
- Line of fire: a ground ray from the aim position blocks the shot only
  when it hits terrain farther than the explosion radius from the target.
- `BadTargetCategory` multiplies the pick priority by 100
  (`GenerateWeaponTargets`); it does not exclude the target.

Port: `combat::fire_salvo_shot`, `weapon_fx::tick` (bolt collision and
fade), `damage::apply_damage` (no miss gate), `UnitStats::mid_y` /
`CollisionVolume::center`, `WeaponDef::{effective_range, spray_sin,
laser_travel}`, `TargetRank`.

## Byte (`scripts/byte.bos`, linear constant 163840)

- No `StartMoving`/`StopMoving`, no `MAX_SPEED` changes: the octahedron
  slides and turns (TurnRate 200 → 33°/s) and nothing animates while it
  drives. It opens and fires while moving.
- `Open()`: base y → 60 @120 (0.5 s, waited); rotor → 45° @90°/s
  (unwaited) and blades ±10 @40 (0.25 s, blade0 waited); `isOpen = 1`.
- `AimWeapon1`: closed → `start-script Open(); return 0`; open → aimer x
  → −90° − p and y → h at 270°/s, waited, then `Close()` restarts.
- `Close()`: `sleep 3000`; aimer → 0 @70°/s (waited); `isOpen = 0`;
  blades → 0 (waited); rotor → 0 @480°/s and base → 0 @300 (unwaited).
  Any new `AimWeapon1` kills it.
- `HitByWeaponId` returns 30 while `!isOpen` (except SigTerm), so the
  armour is off during the aimer's return.
- The four shots of a burst leave bp0..bp3 in turn (`gp`), 7 frames
  apart (`burstrate 0.25`), reload 60 frames from the first.

## Pointer (`scripts/pointer.bos`, `cube.s3o`, linear constant 65536)

- `Create()`: hide gun, gunpoint x −90, gunbase x 90; `ARMORED` until
  4.03 s after the build; opens from Create only if already built.
- `StartMoving()`: `sleep 50` (2 frames); `Close()`: `isOpen = 0`,
  gunbase x → 90 and y → 0 @50°/s (only y waited), gun y → 0 @20 (1.0 s,
  waited), plates → 0 @20 (0.5 s, waited), hide gun; then `spin body x
  <180>`. The unit drives throughout (`set MAX_SPEED` is commented out).
- `StopMoving()`: body x 0 now, stop-spin, `sleep 200` (7 frames);
  `Open()`: show gun, plates ±10 @20 (0.5 s, waited), gun y → 20 @20
  (1.0 s, waited), `isOpen = 1` at about 1.73 s.
- `AimWeapon1`: only `if (isOpen && !aimingSpecial)`: gunbase x → 90 − p
  @50°/s, body `HEADING` stepped 270 units a frame (44.5°/s), return 1
  once the gunbase turn is done.
- NX (weapon 2, `commandfire`): `specialattack.lua` sets `aimingSpecial`
  and issues an attack on the ground; `AimWeapon2` needs the cube open;
  `FireWeapon2` emits CEG 0 from `gunpoint` and blocks weapon 1 for
  `sleep 2000` (61 frames). The shell is a 400 elmo/s MissileLauncher
  with `trajectoryheight 1`; `system_nx` on impact; reload 30 s. The
  area-denial gadget adds the 120-radius, 100 dps, 60 s zone.
- Movement scripts fire on the mover's speed crossing 0.01
  (`CGroundMoveType::UpdateOwnerSpeed`), not on orders.

Port: `animation/units/pointer.rs` (owns the deploy cycle),
`aim::sync_deploy_state`, `script_triggers::trigger_movement_scripts`,
`command_fire::{NxCast, tick_nx_casts}`.

## Construction and factories (`Sim/Units/UnitTypes/Factory.cpp`)

- Every KP unit has `ShowNanoFrame=0` and `ShowNanoSpray=0`: a buildee is
  drawn complete and in place from frame one; all construction motion is
  its script's `BUILD_PERCENT_LEFT` loop (`int((1 − progress) · 100)`,
  truncated). Build lasers are the scripts' own `emit-sfx 2048`
  (`BuildLaser`, a 256-elmo white beam along the piece direction).
- Build speed: `workerTime / 30` build points a frame. Kernel, Hole,
  Carrier 128; Socket, Window 64 (a Bit takes 1.9 s vs 3.75 s).
- `StartBuild` waits for `yardOpen && inBuildStance` (the script's
  `INBUILDSTANCE`: Kernel after `GetPillar3Ready`, Hole 2.3 s, Carrier
  2 s, Window 0.75 s, Socket at once) and returns while the pad is
  `GroundBlocked` by the previous unit.
- `Deactivate` only after 7 s (210 frames) without a build step, then the
  scripts' `YARD_OPEN` loop adds ≥ 300 ms.
- `SendToEmptySpot`: a waypoint at `pos + frontdir · (R + r)`, then the
  first free spot on the half-circle of radius `4R + 4r` in front,
  scanning 100 steps from straight ahead toward the right; a random spot
  on the arc if none is free. The buildee faces the pad's heading.
- `TurnTowardBarycenter` (kernel, hole, carrier): the base's front snaps
  in 90° steps toward the barycenter of all units two ticks after Create.
- Per script: Socket `BuildLasers` (±60 square, 1.5 s) only during its
  own construction, `ConLasers` (inner pair bobbing to 35 @40) forever
  after with beams while `building`; Terminal body from 75 under, lasers
  sweeping a 0.5/1.0/0.5 s rectangle; Hole arm stepping −16/−32/−48/−64
  with `corruption_const` on each stroke; Window hourglass loop while
  built; Obelisk charge glow only once complete; Kernel head lifts at 15
  (heads 0/1) and 20 (heads 2/3) elmos/s, `canbuild` at ≈1.07 s.

Port: `lifecycle/production.rs`, `spawning/emerge.rs` (no entity rise),
`spawning/mod.rs` (`spawn_unit_facing`, `barycenter_heading`),
`script_triggers::DEACTIVATE_DELAY`, the drivers.

## Known remaining differences (2026-10-07)

- Hacker and Network buildees still alpha-fade in; the engine draws them
  opaque (their `SetAlphaThreshold` dissolve is inert on Recoil) and
  Network buildees carry the `network_build.lua` scrolling-band overlay.
- Buildees are untargetable while `Emerging` (spawn protection); the
  engine's can be shot, refund `cost · progress` when cancelled and die
  with their factory.
- The free-spot search checks units only, not terrain passability
  (`TestMoveSquare`).
- An NX cast that is never able to fire (the Pointer is kept moving)
  waits indefinitely; the engine's attack order would be abandoned by
  the player's next command, which the port also does, but a unit that
  is pushed around keeps the cast.
- Lead uses the muzzle as `aimFromPos`; the engine uses the
  `AimFromWeapon` piece (`aimer` / `base`), a few elmos off.
- Hitscan beams are not led (the engine leads them by a negligible
  amount). A sprayed hitscan ray is tested against its target's sphere
  and the ground only, not against other units in its path.
- `ARMORED` windows (Pointer 4 s, Byte 6 s after build, buildings under
  construction) and `STANDINGMOVEORDERS=0` (Hold Position) on the Pointer
  are not modelled.
- The Byte's `LaunchMines` choreography (open, 500 ms retry, launcher
  pieces) is not animated; mines spawn directly.

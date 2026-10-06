# Spring/Recoil ground movement and pathing: reference for the port

Condensed from a source study of `upstream/RecoilEngine/rts` and
`upstream/Kernel-Panic` (2026-10-06). File:line references point into
the engine checkout. The Rust port lives in
`kernel-panic/src/interaction/{ground_move,movement,structures}.rs` and
`spring-pathfinding/`.

## What Kernel Panic actually configures

- `modrules.tdf`: `pathFinderSystem=1` (QTPFS), `allowPushingEnemyUnits=1`,
  `allowUnitCollisionOverlap=1`, `allowCrushingAlliedUnits=0`,
  `allowUnitCollisionDamage=0`, `allowGroundUnitGravity=0`,
  `experienceMult=0`, flanking off.
- `MOVEINFO.TDF`: three classes, all `MaxSlope=36`, all KBot speed
  class (name fallback). LIGHT: footprint 2 → xsize 3 squares, xsizeh 1,
  collision radius 12, owner radius 16.97, crush 40. MEDIUM / HEAVY:
  footprint 4 → xsize 7, xsizeh 3, radius 28, owner radius 39.6, crush
  60 / 300. `maxSlope = 1 − cos(36° · 1.5) = 0.412215`,
  `slopeMod = 4 / (maxSlope + 0.001) = 9.68`.
- The MoveDef footprint overrides the FBI footprint for every ground
  unit (`GroundMoveType.cpp:530`). Units are never crushable; only
  features (Bad Block corpses) are.
- No KP weapon has impulse (except modoption heroes), so skidding never
  happens in KP.
- Units: `MaxVelocity` elmos/frame (×30 → elmos/s), `Acceleration` /
  `BrakeRate` elmos/frame², `TurnRate` in 65536-per-circle heading
  units per frame, `turnInPlace` defaults true, `turnInPlaceAngleLimit`
  0. `UNIT_SLOWUPDATE_RATE = 15`.

## Terrain (`Sim/MoveTypes/MoveMath`, `Map/ReadMap.cpp`)

- The slope map is **half resolution**: one value per 2×2 block of
  squares (`UpdateSlopemap`, ReadMap.cpp:741). From the eight face
  normals of the block (`fnTL = normalize(-(hTR-hTL), 8, -(hBL-hTL))`,
  `fnBR = normalize(hBL-hBR, 8, hTR-hBR)`): `avg`, `min` of their `y`,
  `s = min + (avg − min) · (min / avg)`, `slope = 1 − s`.
- `GetPosSpeedMod(md, x, z)`: `slope = slopeMap[(x>>1) + (z>>1)·hmapx]`,
  `height = maxHeightMap[x,z]`; 0 if `slope > maxSlope`; else
  `1 / (1 + slope · slopeMod)`. The directional variant used by
  `ChangeSpeed` penalises only uphill:
  `1 / (1 + max(0, slope · dirSlopeMod) · slopeMod)` with
  `dirSlopeMod = −moveDir · centerNormal2D`.
- Blocking: `BLOCK_STRUCTURE` only from immobile crush-resistant
  objects (and stopped push-resistant units). Mobile units never block
  movement; in the pathfinder they only scale the speed mod, and KP's
  multipliers default to 1.0, so idle units cost nothing extra.
- Footprint object tests (`RangeIsBlocked`) sample offsets
  `−h, −h+2, …, h` on both axes (`FOOTPRINT_XSTEP = 2`).
- A node is passable for the pathfinder when the square's own terrain
  speed mod is non-zero and no structure square lies in the footprint
  window.

## GroundMoveType per frame (`Sim/MoveTypes/GroundMoveType.cpp`)

1. `UpdateTraversalPlan`: swap in a finished path, obstacle avoidance
   (every 3rd frame per unit), `FollowPath` (goal / waypoint
   bookkeeping, wanted heading).
2. `ChangeHeading`: turn with inertia, `turnAccel = turnRate · 0.333`.
3. `UpdateUnitPosition`: `ChangeSpeed` (turn-limited target speed,
   braking distance when nothing is queued, terrain speed mod with the
   one-square-ahead fallback), then `UpdateOwnerPos` → `UpdatePos`
   (terrain / structure filter: centre square open, footprint free of
   structures; checked only on square transitions unless
   `positionStuck`; sideways tries ±1..8 elmos; axis-aligned slide).
4. `HandleObjectCollisions`: unit pushes (`CalculatePushVector`),
   static yardmap strafe (`HandleStaticObjectCollision`), feature crush,
   `forceStaticObjectCheck` → `positionStuck`.
5. `Update`: apply the collision push, `OwnerMoved` idle test;
   `SlowUpdate` every 15 frames: idle repath / give up after 16 idle
   slow updates, deferred repath after 60 frames with a 150-frame rate
   limit.

Key formulas: `ChangeSpeed` turn cap `maxSpeed · clamp(maxTurnDeg /
reqTurnDeg, 0.1, 1)`; end-of-path cap `currWayPointDist · π /
(65536 / turnRate)`; `CanSetNextWayPoint` skips the current waypoint
only inside `2 · turnRadius` with a clear raw line to the next one;
goal tolerance `(goalRadius + extraRadius) · (numIdlingSlowUpdates + 1)`
for move orders.

`positionStuck`: set when the unit stands on a closed square or inside
a structure footprint (checked after construction, after an idle repath
and when a structure appears over the unit). While stuck every step is
re-checked and, with no open square beside it, taken anyway — that is
how factory-produced units leave a yard. A unit two or more squares
deep in impassable terrain still stalls, because the speed mod and its
one-square fallback are both 0.

## Pathing: QTPFS (`spring-pathfinding/src/qtpfs.rs`)

Kernel Panic selects QTPFS (`pathFinderSystem=1`). The port keeps one
quad tree per mover class (nav bucket × footprint × crush class) over
the per-square speed modifiers, with structure squares closed through
the footprint mask:

- root nodes of 64 squares (32 when the map is not a multiple of 64);
  every node larger than 16 squares is split, smaller nodes split only
  while they mix closed and open squares (`QTNode::UpdateMoveCost`);
- a node's move cost is `1 / mean(relative speed)` (relative = speed
  mod / 2, as `u8(rel · 255)`), `2²⁴ · closed / xsize²` with closed
  squares, infinite when all are closed;
- neighbours: edge-adjacent passable leaves plus corner leaves reached
  past two fully open edge leaves; transition points are the midpoints
  of shared edges (`QTPFS_MAX_NETPOINTS_PER_NODE_EDGE = 1`);
- search: one forward and one backward expansion per iteration, the
  move cost of the node being left times the distance between
  transition points, the cheapest leaf's cost as the heuristic
  multiplier, a per-direction budget of `qtMaxNodesSearched / 2`
  (lowered when the backward search runs dry), an early exit inside
  the goal radius, partial paths toward the node that came closest, a
  goal inside a closed node redirected to the nearest open leaf within
  `max(goalRadius / 8, 16)` squares;
- a clear straight line is answered as a two-point raw path first;
- `TracePath` then one `SmoothPathIter` pass; `NextWayPoint`'s
  first-call scan picks the first waypoint; long partial paths carry a
  repath trigger past their midpoint;
- terrain or structure changes re-tesselate the touched 16-square
  blocks and relink the leaves around them; a path whose remaining
  nodes (from two before the current waypoint) touch the change is
  re-searched 30 frames later (`QueueDeadPathSearches`).

Path sharing (`GenerateHash` / `SharedFinalize`: a request with the
same source and goal leaves takes another unit's finished path, with its
own ends and one smoothing step) and path repair (`LoadRepairPath`: a
dirtied path is re-searched only up to its first clean waypoint, inside
the box around unit and waypoint, and the remainder spliced back) are
ported too. Not ported: exit-only yardmap squares (none in KP) and the
per-thread plumbing.

## Damage model (for orientation; not changed by the movement port)

`dmg = weapon.damages[armorClass(unit)] · dynDamage · beamFraction ·
aoeFalloff · (armored ? DamageModifier : 1)`, then Lua gadgets
(Armor_Bonus, Firewall reflector) and COB `HitByWeaponId` (Byte closed:
30%). AoE falloff `(R − d) / (R − d · edge)` with `d` the distance to
the collision-volume surface and `R = areaOfEffect / 2`; hits further
than `4 · explosionSpeed` are delayed `d / explosionSpeed − 3` frames
(all ported).
KP: no experience, no flanking, no cratering, no impulse; every mobile
unit is `ARMORED` (×1e-6) for a few seconds after construction;
minifacs take ×4 while being built. Paralysis (DOS) caps at
`health · (1 + paralyzeTime / 40)` and decays `health / 40` per second.

## Known remaining differences (2026-10-06, after the parity phases)

- Exit-only yardmap squares are not ported (Kernel Panic has none).
- Transition points use the engine's single edge midpoint; the port
  keeps duplicate points a smoothing step would have removed (the
  follower skips a waypoint within one square anyway).
- A repair search seeds its backward half from the first clean leaf
  only, where the engine preloads the whole clean tail; the result is
  the same path when the repair succeeds.
- Impulse exists but no shipped Kernel Panic weapon applies one; the
  hero modoption is not ported.

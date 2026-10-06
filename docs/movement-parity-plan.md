# Movement parity plan: closing the remaining engine differences

Follow-up to `spring-movement-reference.md` ("Known remaining
differences", 2026-10-06). Four gaps remain between the port and
Recoil's `CGroundMoveType` + QTPFS. Ordered by effort; each phase is
independently shippable and ends with the harness tests and the
`KP_BOT` scenarios green.

| # | Gap | Size | Player-visible effect |
|---|-----|------|-----------------------|
| 1 | Avoidance ignores velocity `y` | ~50 lines | None measurable; completeness |
| 2 | f32 radian headings vs 16-bit units | ~400 lines | Turn timing identical to the frame; aim/turret code shares the type |
| 3 | Grid A* instead of QTPFS | ~3,500 lines | Path *shapes*: quad-edge waypoints, smoothing, path sharing, dead-path refresh |
| 4 | No impulse / skidding | ~700 lines | Only the `homf` hero modoption fires impulse weapons |

## Phase 1: 3D velocity in avoidance (small)

Engine: `GetObstacleAvoidanceDir` predicts separation as
`(avoider.pos + avoider.speed) - (avoidee.pos + avoidee.speed)`, where
`speed` is the full velocity vector (tilted along slopes).

- Add `velocity: Vec3` to `GroundMover`: the displacement `step_mover`
  actually produced this frame including the ground-height change
  (`UpdateOwnerSpeed` stores the raw vector; `OwnerMoved` zeroes it when
  nothing moved — already done for `current_speed`).
- `Avoidee.vel` / the avoider's `vel` become that vector; drop the
  `Vec3::new(vel.x, 0.0, vel.y)` stand-ins.
- Test: two Bits on a 30° ramp heading at each other predict the same
  separation as on flat ground within 1 elmo (the y component cancels
  for equal slopes), and a Bit on a ramp vs one on the plateau predicts
  a larger separation than the 2D value.

## Phase 2: 16-bit headings (medium)

Engine: `heading` is a `short` (65536 per turn, 0 = +Z), `turnRate`
and `turnSpeed` are in those units per frame, `GetHeadingFromVector`
is an atan2 *approximation*, `GetVectorFromHeading` a 4096-entry table
(`SpringMath.inl:38-82`, `SpringMath.cpp` table init). Every heading
delta wraps as i16, so a turn quantises to whole units per frame.

- `sim.rs`: `ShortHeading(i16)` newtype with `wrapping_sub`,
  `from_vector(dx, dz)` (the approximation: `d = dx / (dz + 1e-6·(2dz − 1))`,
  `h = ±π/2 − d/(d²+0.28)` or `d/(1+0.28d²)`, `±π` fixups, `int(h·32768/π)`,
  `ih += (ih == -32768)`, `ih %= 32768`), `to_vector()` via a lazily
  built 4096-entry `(sin, cos)` table indexed `h/16 + 2048`, and
  `to_radians()` for rendering.
- `GroundMover.heading: ShortHeading`, `turn_speed: f32` in heading
  units, `FrameStats.turn_rate = clamp(FBI TurnRate, 1, 32767)` as f32
  units/frame (no radian conversion), `turn_accel = turn_rate · 0.333`.
- `delta_heading` → `GMTDefaultPathController::GetDeltaHeading` with
  the `short()` casts kept: `stopH = short(old + brake·Sign(ts)·bdf)`,
  `cd = short(new − stopH)`, return `short(turnSpeed)`.
- `ChangeSpeed`: `reqTurnAngle = |180 · short(heading − wanted) / 32768|`,
  `maxTurnAngle = turnRate / 65536 · 360`, `turnDeltaHeading` as a short
  compare (`!= 0` rather than a float epsilon), `framesToTurn =
  65536 / turnRate`, `limitSpeedForTurning` offset as `short`.
- `CanSetNextWayPoint` turn radius `cur · (65536 / turnRate) / 2π`,
  `cancel_distance_sq` the same; SlowUpdate idle threshold
  `numIdlingUpdates > 32768 / turnRate`.
- `attitude()` takes the table vector; `front()` / `right()` from it.
- `UpdatePos`'s `GetFacingFromHeading`: `(heading + 8192) / 16384 & 3`
  → S/E/N/W exactly as the engine (replaces the `|fx| > |fz|` proxy).
- Air movement (`air_movement.rs`) and the aim code (`combat/aim.rs`,
  `combat/mod.rs`) read `heading` for turret yaw and hover facing; give
  them `to_radians()` so they are unaffected, then convert
  `HoverAirMoveType`'s own heading in a follow-up (it is also a short
  in the engine).
- Tests: `GetHeadingFromVector` against a table of engine values
  (compute from the C++ formula in the test), a Bit's 180° turn takes
  the engine's frame count (`ceil(32768/480)` plus inertia ramp), and
  the existing turn/arc harness tests keep passing with their
  tolerances tightened to one frame.

## Phase 3: QTPFS port (large)

Engine sources: `Sim/Path/QTPFS/{NodeLayer,Node,PathSearch,PathManager,
PathCache}.cpp` (8,750 lines with headers; the per-thread registry,
load screen and tracing can be dropped). Keep the grid A* behind a
`PathBackend` resource until the new one passes the same tests, then
delete it.

### 3a. Reference pass (half a day)
Read the five files and extend `spring-movement-reference.md` with the
QTPFS model: node layer per MoveDef (KP: three layers that are
identical — LIGHT/MEDIUM/HEAVY share slope, differ only in footprint
blocking), root node grid of `QTPFS_MAX_NODE_SIZE = 64` squares, speed
bins (`numSpeedModBins`, default 10 → `SpeedMap::speed_to_bin` already
mirrors `NodeLayer::GetSpeedModBin`), the tesselation rule (split while
a node's squares fall into more than one bin, down to `minNodeSizeX`
= 1 square), the neighbour cache (`QTPFS_CORNER_CONNECTED_NODES`), the
search (`PathSearch::Execute`: bidirectional A*, `SEARCH_DIRS = 2`,
one net point per shared edge, `hCostMult`, raw-search shortcut,
partial paths), `SmoothPath` (one iteration), path sharing, and the
manager's dead-path refresh (`QueueDeadPathSearches`,
`deadPathsToUpdatePerFrame = 1`, damage blocks of 16 squares).

### 3b. `spring-pathfinding::qtpfs::NodeLayer` (1–2 days)
- Build from a `SpeedMap` + `BlockMask` per mover class: per-square bin
  (`NUM_SPEEDMOD_BINS` closed, `+1` unrestricted), nodes in a pool with
  `child_base_index`, root grid, `moveCostAvg = area / Σ speedMod`
  with closed nodes at `QTPFS_CLOSED_NODE_COST`.
- `Tesselate` / `PreTesselate` on a rectangle (map load: whole map;
  runtime: the changed rectangle dilated by the footprint), `Split` /
  `Merge`, `UpdateNeighborCache` with corner connectivity.
- Tests: a flat map is one leaf per root; a 1-square wall splits down
  to 1-square leaves along it and merges back when removed; node count
  on Data_Cache_L1 within 10% of the engine's logged ratio (run Recoil
  once with `/debugpath` if a reference is wanted, else skip).

### 3c. `PathSearch` (2 days)
- Bidirectional A* over leaf nodes: open heap keyed on `f`, per-node
  search state offset so no clearing between searches, net point =
  midpoint of the shared edge clamped to the smaller node
  (`GetNeighborEdgeTransitionPoint` with `alpha`), `g += dist(prev
  netpoint, netpoint) · moveCost`, `h = dist(netpoint, goal) ·
  hCostMult`, goal test when the goal square's node is popped, partial
  path to the node nearest the goal when the goal is unreachable
  (`QTPFS_SUPPORT_PARTIAL_SEARCHES`), node limit
  `qtMaxNodesSearched = 8192`.
- `ExecuteRawSearch`: straight-line walk over squares (reuse
  `line_clear`) tried first when `heur ≤ pfRawDistMult` scaled as the
  manager does.
- `TracePath` (points at net points, start and goal appended) and
  `SmoothPath` (`SmoothPathPoints` slides each interior point along
  its edge toward the straight line between neighbours while staying
  inside both nodes; one iteration).
- Tests: on a uniform map the path is the straight line (raw search);
  around a wall the smoothed path's length is within 2% of the grid
  A* + LOS result; a partial path ends in the node nearest the goal;
  unreachable start returns no path.

### 3d. Manager integration (1 day)
- `PathQueue` keeps its request / budget / epoch handling but drives
  `PathSearch`; the per-frame budget counts node pops.
- `PathUpdated`: when a terrain or structure change touches a node a
  live path crosses, flag the path dead and re-run its search over the
  next frames (`deadPathsToUpdatePerFrame`), replacing today's
  segment-by-segment `raw_search` re-check. `GroundMoveType` already
  handles the `atEndOfPath && !atGoal → !PathUpdated` and
  `CanSetNextWayPoint` re-fetch rules; wire them to the flag.
- Temporary waypoints (`y = -1`) while a search is pending: keep the
  current "one square toward the goal" stand-in, which is the
  manager's `createTempWaypoint`.
- Hex Farm: terrain edits call `NodeLayer::update(rect)` for every
  layer instead of `SpeedMap::update_region` + `mask_void` only.
- Delete `ComponentLabels` if the partial-path logic makes the
  reachability pre-check redundant (measure first: it is what keeps a
  flood search for an unreachable goal cheap).

### 3e. Validation (half a day)
- Harness: every existing movement test, plus path-length parity tests
  from 3c run through the game-side wrapper.
- `KP_BOT` scenarios: ramp routing (c1), firewall gap (g1), crowd
  (s4b), head-on (s6), Kernel walk-through (s2); compare `path` dumps
  before/after.
- Perf: `KP_PROFILE=1` attract demo on Hex_Farm_8 (largest map, live
  terrain edits) — path service time per tick must stay under the
  current budget.

## Phase 4: impulse and skidding (medium, gated on `homf`)

No shipped KP weapon has `impulseFactor ≠ 0`; only the `homf` hero
modoption (anydefs_post.lua) sets `impulsefactor = 3`. Do this phase
only if hero units are ported; otherwise it is dead code.

- `spring-tdf`: parse `impulseFactor` (default 1), `impulseBoost` (0).
- Damage: `CalcImpulseScale`: `impulse = normalize(volPos − expPos) ·
  clamp(impulseFactor · mod · (D_default + impulseBoost), ±1e4)`;
  `ApplyImpulse(impulse · impulseMult / mass)` with the into-ground
  component removed (`imp − n · min(0, imp·n)` while on ground).
- `GroundMover`: `CanApplyImpulse` → `StartSkidding` when
  `SignedSquare(v·skidDir) + 0.01 < |v|² · 0.95`, `StartFlying` when
  `v·groundNormal > 0.2`; `skid_rot_vector`, `skid_rot_speed/accel`.
- `UpdateSkid` per frame while skidding (runs instead of path
  following): drag `GetDragAccelerationVec` (atmospheric 1.0 on a
  sphere of radius `cbrt(3m/(4π·8000))`, ground friction 0.01·mass·|v|,
  per-component clamp, full cancel below 0.5 elmo/s), `speedReduction
  0.35`/frame on flat ground, slope sliding off (slideTolerance 0),
  ground contact (`v·n > 0 → ×0.95`, else `v += n·(|v·n| + 0.1); ×0.8`),
  `CalcSkidRot` (rs ×0.999, ra ×0.95, Rodrigues rotation of the frame),
  `CheckCollisionSkid` (immobile: `v += sepDir·impact·1.8`; mobile: mass
  split `m1/(m1+m2)`), `StopSkidding` → `ChangeHeading(heading)`.
- Rendering: apply the skid rotation to the model transform.
- Tests: an impulse along the facing never skids; a side impulse of
  2·maxSpeed skids and stops within the frames the 0.35 reduction
  predicts; a skidding unit sliding into a wall bounces.

## Order and estimate

1 → 2 → 3 → (4). Phases 1 and 2 together are about two days; phase 3
about a week including validation; phase 4 two days if wanted. Commit
per phase; each phase leaves `cargo test`, clippy and the bot scenarios
green and updates `spring-movement-reference.md`'s "Known remaining
differences" list.

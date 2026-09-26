use bevy::prelude::*;

use spring_pathfinding::{BlockMask, SpeedMap, find_path_masked};

use super::selection::Selected;
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{
    AttackGroundOrder, AttackTargetOrder, CHASE_REPATH_DISTANCE, Dying, ForcedTarget,
};
use crate::units::components::{UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

pub use super::ground_move::{GroundMover, ground_collision_system, movement_system};

/// Dedicated gizmo config for command-line overlays so the dashed path
/// renders thinner than the default 2-px gizmo width used elsewhere.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct CommandLineGizmos;

/// When present, the unit will move toward this world position.
#[derive(Component)]
pub struct MoveTarget(pub Vec3);

/// The per-unit movement-state components every spawned unit carries.
/// Shared by `spawn_unit` and the headless movement harness so both
/// build movers identically.
pub(crate) fn ground_mover_components(kind: UnitKind, registry: &UnitRegistry) -> impl Bundle {
    GroundMover::new(kind, registry, &UnitStats::from_registry(kind, registry, 0.0))
}

/// The fixed-tick ground-movement chain, in the order
/// [`super::InteractionPlugin`] runs it. The headless harness runs the
/// same list.
#[cfg(test)]
pub(crate) fn add_ground_sim_systems(schedule: &mut Schedule) {
    schedule.add_systems(
        (
            update_path_heat,
            movement_system,
            ground_collision_system,
            ground_clamp_system,
            orient_stationary_to_terrain,
        )
            .chain(),
    );
}

/// Marks an active attack-move order. While it is present AND the unit
/// has an `AimTarget` (an in-range hostile), movement halts so the unit
/// stops and fights; it resumes marching when the threat clears.
#[derive(Component)]
pub struct AttackMoveActive;

/// Player-issued order to guard (follow and protect) another unit.
/// [`guard_follow_system`] trails the target at `GUARD_DISTANCE`; while
/// close the guard holds position and the normal auto-attack path engages
/// anything that comes into weapon range.
#[derive(Component, Clone, Copy)]
pub struct GuardTarget(pub Entity);

/// How far behind the guarded unit a guard trails (elmos, beyond the
/// target's own footprint).
const GUARD_DISTANCE: f32 = 48.0;

/// Per-unit slope tilt, smoothed across frames. Kept as a component rather
/// than extracted from `Transform::rotation` each frame because tilt rotates
/// the forward vector off the XZ plane, and re-reading it would leak into
/// yaw on the next steering pass.
#[derive(Component, Default)]
pub struct SlopeTilt(pub Quat);

/// A computed path the unit follows waypoint-by-waypoint.
#[derive(Component, Clone, Debug)]
pub struct MovePath {
    pub waypoints: Vec<Vec3>,
    /// Index of the current waypoint (`currWayPoint`); `>= len` means
    /// the leg is complete.
    pub current: usize,
    /// The goal this path was searched for. A `MoveTarget` that differs
    /// makes the movement system search a new path, following this one
    /// until it arrives (Spring's `nextPathId` swap).
    pub goal: Vec3,
    /// `false` when the goal is unreachable and the path ends at the
    /// closest reachable point (QTPFS partial path): reaching that end
    /// fails the order instead of arriving.
    pub reached_goal: bool,
    /// [`NavGridSet::revision`] the path was checked against.
    pub revision: u64,
}

impl MovePath {
    /// A path through `waypoints` toward `goal`.
    pub fn new(waypoints: Vec<Vec3>, goal: Vec3) -> Self {
        Self {
            waypoints,
            current: 0,
            goal,
            reached_goal: true,
            revision: 0,
        }
    }

    /// An empty path: tells the movement system the leg is over (it
    /// promotes the next queued command, or stops).
    pub fn finished() -> Self {
        Self::new(Vec::new(), Vec3::ZERO)
    }
}

/// A queued command waiting to become the unit's active order.
/// Consumed in FIFO order when the current order completes.
#[derive(Clone, Copy, Debug)]
pub enum QueuedCommand {
    Move(Vec3),
    /// Walk to `site`, then erect a building of `kind` there. Issued by
    /// the placement flow: the build menu arms a `PlacementMode`, the
    /// ghost click commits a `BuildAt` to every selected constructor.
    BuildAt {
        kind: UnitKind,
        site: Vec3,
    },
    /// Walk to `target`, then return to the origin, repeating. Issued by
    /// the patrol (P) order.
    Patrol(Vec3),
    /// Walk to `target`, then fight anything hostile on the way. The
    /// attack-move keybind was removed upstream; the variant remains so
    /// AI / future keybinds can issue it.
    #[allow(dead_code)]
    AttackMove(Vec3),
    /// Hold position at `point`, engaging hostile units that come within
    /// weapon range. The guard keybind was removed upstream; the variant
    /// remains so AI / future keybinds can issue it.
    #[allow(dead_code)]
    Guard(Vec3),
    /// Walk to `pos`, then attack the unit `target` (shift-chained
    /// right-click attack). `pos` is the target's position at enqueue
    /// time, used only for command-line drawing; the live chase targets
    /// the unit itself via `AttackTargetOrder`.
    AttackUnit {
        target: Entity,
        pos: Vec3,
    },
}

impl QueuedCommand {
    pub fn position(&self) -> Vec3 {
        match self {
            QueuedCommand::Move(p)
            | QueuedCommand::Patrol(p)
            | QueuedCommand::AttackMove(p)
            | QueuedCommand::Guard(p) => *p,
            QueuedCommand::AttackUnit { pos, .. } => *pos,
            QueuedCommand::BuildAt { site, .. } => *site,
        }
    }
}

/// FIFO queue of follow-up commands. When the unit finishes its current
/// order (e.g. reaches its `MoveTarget`), the next command is popped and
/// promoted to the active order.
#[derive(Component, Default)]
pub struct CommandQueue {
    pub commands: Vec<QueuedCommand>,
}

impl CommandQueue {
    pub fn push(&mut self, cmd: QueuedCommand) {
        self.commands.push(cmd);
    }
}

/// One pathfinding grid plus the max-slope cap (dy/dx ratio) it was
/// built for. Grids with smaller caps flag more cells as impassable.
pub struct NavBucket {
    pub max_slope: f32,
    pub speed_map: SpeedMap,
}

/// A set of pathfinding grids, one per distinct `MaxSlope` in the unit
/// roster. Units with a tighter slope cap pick a grid whose cells they
/// can actually traverse; units with looser caps share the most permissive
/// grid. Sorted ascending by `max_slope`.
///
/// Upstream Spring does the same thing via per-`MoveDef` grids
/// (`rts/Sim/MoveTypes/MoveDefHandler.cpp`); see plan.md §Gameplay Bugs
/// "Movement ignores per-unit `MaxSlope`" for the full motivation.
#[derive(Resource, Default)]
pub struct NavGridSet {
    pub buckets: Vec<NavBucket>,
    /// Bumped whenever the structure layer changes, so paths made
    /// before can be re-checked (QTPFS `PathUpdated`).
    pub revision: u64,
}

impl NavGridSet {
    /// Pick the tightest bucket whose cap ≥ `cap`. If none qualifies
    /// (the unit needs a looser grid than any we built), return the
    /// loosest bucket. Panics if empty — the map loader always pushes
    /// at least the default 45° bucket.
    pub fn bucket_for(&self, cap: f32) -> usize {
        debug_assert!(!self.buckets.is_empty(), "NavGridSet::bucket_for: empty");
        self.buckets
            .iter()
            .position(|b| b.max_slope >= cap)
            .unwrap_or(self.buckets.len() - 1)
    }

    /// Can a ground unit with slope cap `cap` stand on the heightmap
    /// square under `(x, z)`? Off-map squares are not. With no grids
    /// built everything is.
    pub fn passable(&self, cap: f32, x: f32, z: f32) -> bool {
        if self.buckets.is_empty() {
            return true;
        }
        let map = &self.buckets[self.bucket_for(cap)].speed_map;
        let (cx, cz) = ((x / 8.0).floor(), (z / 8.0).floor());
        cx >= 0.0 && cz >= 0.0 && map.get(cx as u32, cz as u32) > 0.0
    }

    /// The grid of the bucket for slope cap `cap`.
    fn speed_map(&self, cap: f32) -> Option<&SpeedMap> {
        (!self.buckets.is_empty()).then(|| &self.buckets[self.bucket_for(cap)].speed_map)
    }

    /// Does a structure block any square of a `xsizeh`-footprint mover
    /// centred at `(x, z)` (`MoveDef::TestMovePositionForObjects`)?
    pub fn footprint_blocked(&self, _x: f32, _z: f32, _xsizeh: i32, _crush_strength: f32) -> bool {
        false
    }

    /// The structure mask a mover class paths against, if any.
    pub fn block_mask(&self, _xsizeh: i32, _crush_strength: f32) -> Option<&BlockMask> {
        None
    }

    /// `MoveDef::DoRawSearch`: can a mover drive straight `a → b`?
    /// Every square the segment crosses must be passable terrain and
    /// open for the mover's footprint.
    pub fn line_clear(&self, cap: f32, xsizeh: i32, crush_strength: f32, a: Vec2, b: Vec2) -> bool {
        let Some(map) = self.speed_map(cap) else {
            return true;
        };
        spring_pathfinding::line_clear(
            a.to_array(),
            b.to_array(),
            map,
            self.block_mask(xsizeh, crush_strength),
            None,
        )
    }

    /// Terrain speed multiplier for a mover at `pos` heading `dir`.
    pub fn speed_mod(&self, _cap: f32, _pos: Vec2, _dir: Vec2) -> f32 {
        1.0
    }
}

/// Spring `MOVEINFO.TDF` HeatMapping: a shared congestion grid the
/// pathfinder reads as extra cost. Walking ground units deposit heat on
/// the cell they stand in; later paths bend around hot cells, so
/// columns marching the same line fan out instead of braiding into a
/// single rut. Upstream keeps per-class decay rates; a single shared
/// grid decays at the most persistent class (LIGHT, `HeatMod=0.10`)
/// while MEDIUM/HEAVY keep their much higher deposits.
#[derive(Resource)]
pub struct PathHeat(pub spring_pathfinding::HeatMap);

/// Per-second heat retention of the shared grid (upstream LIGHT
/// `HeatMod=0.10`).
const HEAT_RETENTION_PER_SECOND: f32 = 0.1;
/// Full-grid decay runs in steps of this many seconds (a plain
/// multiply at `retention^period` — same curve, memcpy cost).
const HEAT_DECAY_PERIOD: f32 = 0.5;

/// Deposit + decay pass for [`PathHeat`]. Runs before
/// `movement_system` so freshly-issued paths see this tick's
/// congestion. No-op when no map (and therefore no grid) is loaded.
#[allow(clippy::type_complexity)]
pub fn update_path_heat(
    time: Res<Time>,
    movers: Query<(&GlobalTransform, &UnitType, &UnitStats), With<MovePath>>,
    registry: Res<UnitRegistry>,
    mut heat: Option<ResMut<PathHeat>>,
    mut decay_timer: Local<f32>,
) {
    let Some(heat) = heat.as_deref_mut() else {
        return;
    };
    let dt = time.delta_secs();
    for (gtf, unit, stats) in &movers {
        // Flyers never touch the nav grid — they leave no trail.
        if stats.can_fly || stats.speed <= 0.0 {
            continue;
        }
        let produced = registry.heat_produced(unit.0);
        let pos = gtf.translation();
        heat.0.add_heat([pos.x, pos.z], produced * dt);
    }

    *decay_timer += dt;
    if *decay_timer >= HEAT_DECAY_PERIOD {
        *decay_timer = 0.0;
        heat.0
            .decay(HEAT_RETENTION_PER_SECOND.powf(HEAT_DECAY_PERIOD));
    }
}

/// How far above the sampled ground height to draw command-line gizmos
/// so they don't z-fight with the terrain.
const GIZMO_LIFT: f32 = 1.5;

/// Exact surface-aligned orientation: local up = terrain `normal`,
/// local forward = `forward_xz` projected into the surface plane.
///
/// Replaces the old pitch/roll Euler composition, which used a
/// mislabeled perpendicular (it was the left vector), composed the two
/// rotations about world axes (only exact when the fall line aligns
/// with the body — diagonal slopes leaned sideways), and — worst of
/// all — slerped a *local-space* tilt, so a buffered downhill pitch
/// applied after a yaw change read as a sideways lean mid-turn.
pub fn surface_aligned_rotation(forward_xz: Vec3, normal: Vec3) -> Quat {
    // Degenerate normals fall back to flat so the basis stays
    // invertible. The threshold is deliberately strict: normals steeper
    // than 60° from vertical only exist at cliff bases / walls (units
    // never stand there — passable cells cap at 54°, normal.y ≈ 0.59).
    // Projecting the heading onto such an extreme plane deflects its XZ
    // direction every tick, which used to pin units at cliff bases —
    // facing a fixed wrong heading at full throttle, never aligning
    // with the waypoint.
    let up = if normal.y > 0.1 {
        normal.normalize()
    } else {
        Vec3::Y
    };
    let mut f = Vec3::new(forward_xz.x, 0.0, forward_xz.z);
    if f.length_squared() < 1e-6 {
        f = Vec3::Z;
    }
    let f = f.normalize();
    // Shear the heading VERTICALLY onto the slope plane: the body's
    // world-space forward keeps the requested XZ heading exactly while
    // gaining the pitch the slope demands. (The previous
    // closest-point projection *rotated the heading itself* on tilted
    // ground — by up to ~11° per tick at cliff bases, exactly the
    // per-tick turn budget, so units were pinned facing a fixed wrong
    // heading at full throttle, never able to align with their
    // waypoint.)
    let shear = (f.x * up.x + f.z * up.z) / up.y;
    let fwd = Vec3::new(f.x, -shear, f.z).normalize();
    // Right-handed frame: for f=+Z and up=+Y this yields right=−X —
    // facing +Z with Y up, your right hand points toward −X.
    let right = fwd.cross(up).normalize();
    Quat::from_mat3(&Mat3::from_cols(right, up, -fwd))
}

/// An order leg is done: promote the next queued command to the active
/// order, or — queue empty — drop the order entirely. Shared by the
/// ground walker ([`movement_system`]) and the aircraft
/// (`air_movement::hover_air_system`). `translation` is where the unit
/// is now (the patrol return point).
pub(crate) fn promote_next_command(
    commands: &mut Commands,
    entity: Entity,
    translation: Vec3,
    mut queue: Option<&mut CommandQueue>,
) -> Option<Vec3> {
    let next = queue.as_deref_mut().and_then(|q| {
        if q.commands.is_empty() {
            None
        } else {
            Some(q.commands.remove(0))
        }
    });
    let goal = next.and_then(|c| match c {
        QueuedCommand::AttackUnit { .. } => None,
        c => Some(c.position()),
    });
    match next {
        Some(QueuedCommand::Move(pos) | QueuedCommand::Guard(pos)) => {
            commands
                .entity(entity)
                .insert(MoveTarget(pos))
                .remove::<crate::units::lifecycle::construction::PendingBuild>();
        }
        Some(QueuedCommand::Patrol(pos)) => {
            // Patrol shuttles between two points forever: the unit
            // just arrived at `pos`'s predecessor, so record where it
            // is now and re-queue that as the opposing waypoint.
            // Mirrors upstream CommandAI.cpp pushing `owner->pos` as
            // the first patrol point when none is queued.
            let origin = Vec3::new(translation.x, 0.0, translation.z);
            commands
                .entity(entity)
                .insert(MoveTarget(pos))
                .remove::<crate::units::lifecycle::construction::PendingBuild>();
            if let Some(queue) = queue {
                queue.commands.push(QueuedCommand::Patrol(origin));
            }
        }
        Some(QueuedCommand::AttackMove(pos)) => {
            commands
                .entity(entity)
                .insert(MoveTarget(pos))
                .insert(AttackMoveActive)
                .remove::<crate::units::lifecycle::construction::PendingBuild>();
        }
        Some(QueuedCommand::AttackUnit { target, .. }) => {
            // Explicit attack supersedes a manual (T) designation,
            // and the attack system owns movement from here — no
            // MoveTarget, so the finished leg can't re-route.
            commands
                .entity(entity)
                .remove::<crate::units::lifecycle::construction::PendingBuild>()
                .remove::<MoveTarget>()
                .remove::<crate::units::combat::ForcedTarget>()
                .insert(crate::units::combat::AttackTargetOrder { target });
        }
        Some(QueuedCommand::BuildAt { kind, site }) => {
            commands
                .entity(entity)
                .insert(MoveTarget(site))
                .insert(crate::units::lifecycle::construction::PendingBuild { kind, site });
        }
        None => {
            commands.entity(entity).remove::<MoveTarget>();
            commands.entity(entity).remove::<CommandQueue>();
            commands
                .entity(entity)
                .remove::<crate::units::lifecycle::construction::PendingBuild>()
                .remove::<AttackMoveActive>();
        }
    }
    goal
}

/// Guard orders: trail [`GuardTarget`] at `GUARD_DISTANCE` beyond the
/// target's footprint. Farther than that, chase (repathing only when the
/// current path endpoint lags the target, sharing [`CHASE_REPATH_DISTANCE`]
/// with the attack-chase); inside it, settle and let the auto-attack path
/// engage anything in range. When the target dies the guard stands down.
pub fn guard_follow_system(
    mut commands: Commands,
    guards: Query<(Entity, &GlobalTransform, &UnitStats, &GuardTarget), Without<Dying>>,
    targets: Query<(&GlobalTransform, &UnitStats), Without<Dying>>,
    move_path_q: Query<&MovePath>,
) {
    for (entity, gtf, stats, guard) in &guards {
        // Buildings have nobody to trail.
        if stats.speed <= 0.0 {
            continue;
        }
        let Ok((target_gtf, target_stats)) = targets.get(guard.0) else {
            // Target gone: stand down and clear movement.
            commands
                .entity(entity)
                .remove::<GuardTarget>()
                .remove::<MoveTarget>()
                .remove::<MovePath>();
            continue;
        };

        let target_pos = target_gtf.translation();
        let keep = target_stats.radius + GUARD_DISTANCE;
        let dist = gtf.translation().distance(target_pos);

        if dist > keep {
            // Trail the target; repath when there's no path yet or the
            // path endpoint lags the target's current position.
            let stale = move_path_q.get(entity).map_or(true, |p| {
                p.waypoints
                    .last()
                    .is_none_or(|w| w.distance(target_pos) > CHASE_REPATH_DISTANCE)
            });
            if stale {
                // The old path is followed until the new one is ready.
                commands.entity(entity).insert(MoveTarget(target_pos));
            }
        } else {
            commands
                .entity(entity)
                .remove::<MoveTarget>()
                .remove::<MovePath>();
        }
    }
}

/// Rotate `from` (normalized, XZ plane) toward `to` by at most `max_turn`
/// radians. If `to` is nearly zero we keep the current heading.
pub fn rotate_toward_xz(from: Vec3, to: Vec3, max_turn: f32) -> Vec3 {
    let to_len_sq = to.length_squared();
    if to_len_sq < 1e-6 {
        return from;
    }
    let to_n = to / to_len_sq.sqrt();
    let dot = from.dot(to_n).clamp(-1.0, 1.0);
    let angle = dot.acos();
    if angle <= max_turn || angle < 1e-4 {
        return to_n;
    }
    // Signed turn direction: y component of `from × to_n` gives us the
    // left/right sense in the XZ plane (Y is up, right-handed).
    let cross_y = from.x * to_n.z - from.z * to_n.x;
    let sign = if cross_y >= 0.0 { -1.0 } else { 1.0 };
    let rot = Quat::from_axis_angle(Vec3::Y, sign * max_turn);
    (rot * from).normalize()
}

/// Re-clamp every ground unit's Y to the heightmap surface. The
/// in-loop clamp in `movement_system` only runs for units actively
/// walking a path, and the clamp in `unit_separation_system` only fires
/// for units that got pushed this frame — an idle unit standing on a
/// slope can still end up below the mesh (spawn rounding, map heightmap
/// edits, feature removal). Doing it here, unconditionally once per
/// frame, guarantees no non-flying unit is ever rendered inside
/// terrain.
///
/// Exceptions: flying units (flown by `air_movement`) and
/// subterranean units (the Worm, which
/// intentionally sinks below the surface — see
/// [`UnitKind::is_subterranean`]).
pub fn ground_clamp_system(
    heightmap: Option<Res<Heightmap>>,
    mut units: Query<(&UnitType, &UnitStats, &mut Transform)>,
) {
    let Some(heightmap) = heightmap else {
        return;
    };
    for (unit_type, stats, mut transform) in &mut units {
        if stats.can_fly || unit_type.0.is_subterranean() {
            continue;
        }
        let ground = heightmap.sample(transform.translation.x, transform.translation.z);
        if transform.translation.y < ground {
            transform.translation.y = ground;
        }
    }
}

/// Orient every stationary unit (no active move order) to the terrain
/// normal, preserving its current yaw. Counterpart to the slope-tilt
/// block inside `movement_system`: that one only fires for units with
/// a live `MovePath`, so factories and idle mobile units would stay
/// axis-aligned and read as floating off sloped ground.
///
/// Flying units skip — `air_movement` owns their attitude.
#[allow(clippy::type_complexity)]
pub fn orient_stationary_to_terrain(
    heightmap: Option<Res<Heightmap>>,
    mut q: Query<(
        Entity,
        &mut Transform,
        Option<&mut SlopeTilt>,
        &UnitStats,
        &UnitType,
        Option<&MoveTarget>,
        Option<&MovePath>,
    )>,
    mut commands: Commands,
) {
    let Some(heightmap) = heightmap else {
        return;
    };
    for (entity, mut transform, mut slope_tilt, stats, unit_type, move_target, move_path) in &mut q
    {
        if stats.can_fly || unit_type.0.is_subterranean() {
            continue;
        }
        // If the unit is actively pathing, `movement_system` owns its
        // rotation this frame — skip so we don't fight that system's
        // slerp toward the steering target.
        if move_target.is_some() || move_path.is_some() {
            continue;
        }

        let pos = transform.translation;
        let normal = heightmap.normal(pos.x, pos.z);

        // Preserve yaw: read the current forward, flatten to XZ, and
        // let `surface_aligned_rotation` rebuild the orientation from
        // it. Stationary buildings start at yaw=0 (facing -Z); mobile
        // units keep whichever yaw they finished their last move order
        // with.
        let forward = transform.forward().as_vec3();
        let forward_xz = {
            let f = Vec3::new(forward.x, 0.0, forward.z);
            if f.length_squared() < 1e-6 {
                -Vec3::Z
            } else {
                f.normalize()
            }
        };

        let target = surface_aligned_rotation(forward_xz, normal);

        match slope_tilt.as_deref_mut() {
            Some(t) => {
                t.0 = target;
            }
            None => {
                commands.entity(entity).insert(SlopeTilt(target));
            }
        }
        transform.rotation = target;
    }
}

/// Outcome of one path search.
pub(crate) enum PathOutcome {
    /// Follow this path. When the goal is unreachable it ends at the
    /// closest reachable point (`reached_goal == false`) — QTPFS hands
    /// out such partial paths and the unit fails the order on arrival
    /// there (`CGroundMoveType::CanSetNextWayPoint`'s `lastWaypoint`).
    Route(MovePath),
    /// No route from here at all (source enclosed).
    Unreachable,
}

/// Search a path through the nav bucket matching the unit's `MaxSlope`,
/// against the structure mask of its MoveDef footprint and crush
/// strength. `None` means nothing could be decided (no nav grid yet).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compute_path(
    nav_set: Option<&NavGridSet>,
    unit_registry: &UnitRegistry,
    kind: UnitKind,
    xsizeh: i32,
    crush_strength: f32,
    from: Vec3,
    to: Vec3,
    heat: Option<&spring_pathfinding::HeatMap>,
) -> Option<PathOutcome> {
    let nav = nav_set?;
    let speed_map = nav.speed_map(unit_registry.max_slope_ratio(kind))?;
    let mask = nav.block_mask(xsizeh, crush_strength);
    let Some(path) = find_path_masked(speed_map, mask, heat, [from.x, from.z], [to.x, to.z]) else {
        return Some(PathOutcome::Unreachable);
    };
    let waypoints: Vec<Vec3> = path.points.iter().map(|p| Vec3::new(p[0], 0.0, p[1])).collect();
    Some(PathOutcome::Route(MovePath {
        // Point 0 is the start position itself.
        current: 1.min(waypoints.len().saturating_sub(1)),
        waypoints,
        goal: to,
        reached_goal: path.reached_goal,
        revision: nav.revision,
    }))
}

/// Dash-pattern segment lengths (long dash, gap, short dot, gap), in elmos.
/// Drawn back-to-back they form a repeating `-.-.` run.
const DASH_PATTERN: [(f32, bool); 4] = [(16.0, true), (6.0, false), (4.0, true), (6.0, false)];

fn sample_at_ground(x: f32, z: f32, heightmap: Option<&Heightmap>) -> Vec3 {
    let y = heightmap.map(|h| h.sample(x, z)).unwrap_or(0.0);
    Vec3::new(x, y + GIZMO_LIFT, z)
}

/// Draw each selected unit's order overlay in Spring's per-command colors:
/// the active path polyline plus a disc at the destination, follow-up queue
/// segments, and — for unit-targeted orders — a line and ring on the
/// attack / guard target themselves. Mimics Spring's command-overlay
/// palette: move green, fight/attack red, patrol blue, guard white.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn draw_selected_command_lines(
    mut gizmos: Gizmos<CommandLineGizmos>,
    query: Query<
        (
            &Transform,
            Option<&MoveTarget>,
            Option<&MovePath>,
            Option<&CommandQueue>,
            Option<&AttackMoveActive>,
            Option<&AttackGroundOrder>,
            Option<&AttackTargetOrder>,
            Option<&GuardTarget>,
            Option<&ForcedTarget>,
        ),
        With<Selected>,
    >,
    targets: Query<&GlobalTransform>,
    heightmap: Option<Res<Heightmap>>,
) {
    const MOVE_COLOR: Color = Color::srgb(0.2, 1.0, 0.3);
    const BUILD_COLOR: Color = Color::srgb(1.0, 0.8, 0.2);
    const PATROL_COLOR: Color = Color::srgb(0.4, 0.7, 1.0);
    const FIGHT_COLOR: Color = Color::srgb(1.0, 0.3, 0.25);
    const GUARD_COLOR: Color = Color::srgb(0.85, 0.95, 1.0);
    const TARGET_COLOR: Color = Color::srgb(1.0, 0.65, 0.2);
    const DISC_RADIUS: f32 = 6.0;

    let hm = heightmap.as_deref();

    for (
        transform,
        target,
        path,
        queue,
        attack_move,
        attack_ground,
        attack_target,
        guard,
        forced,
    ) in &query
    {
        let unit_pt = sample_at_ground(transform.translation.x, transform.translation.z, hm);

        // Unit-targeted orders: line + ring on the target unit itself,
        // drawn regardless of whether the unit is currently moving.
        if let Some(atk) = attack_target
            && let Ok(t_gtf) = targets.get(atk.target)
        {
            let tp = sample_at_ground(t_gtf.translation().x, t_gtf.translation().z, hm);
            draw_dashed_polyline(&mut gizmos, &[unit_pt, tp], FIGHT_COLOR, hm);
            gizmos.circle(
                Isometry3d::new(tp, Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
                DISC_RADIUS,
                FIGHT_COLOR,
            );
        }
        if let Some(guard) = guard
            && let Ok(t_gtf) = targets.get(guard.0)
        {
            let tp = sample_at_ground(t_gtf.translation().x, t_gtf.translation().z, hm);
            draw_dashed_polyline(&mut gizmos, &[unit_pt, tp], GUARD_COLOR, hm);
            gizmos.circle(
                Isometry3d::new(tp, Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
                DISC_RADIUS,
                GUARD_COLOR,
            );
        }

        // Manual target designation (T): amber line + ring on the forced
        // target, independent of any movement order.
        if let Some(forced) = forced
            && let Ok(t_gtf) = targets.get(forced.0)
        {
            let tp = sample_at_ground(t_gtf.translation().x, t_gtf.translation().z, hm);
            draw_dashed_polyline(&mut gizmos, &[unit_pt, tp], TARGET_COLOR, hm);
            gizmos.circle(
                Isometry3d::new(tp, Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
                DISC_RADIUS,
                TARGET_COLOR,
            );
        }

        let Some(current) = target else {
            continue;
        };

        // Collect the sequence of polyline vertices: unit → remaining
        // waypoints. If no path exists yet (freshly-issued order), fall
        // back to unit → current target so the player sees something.
        let mut points: Vec<Vec3> = vec![unit_pt];
        if let Some(path) = path
            && path.current < path.waypoints.len()
        {
            for wp in &path.waypoints[path.current..] {
                points.push(sample_at_ground(wp.x, wp.z, hm));
            }
        } else {
            points.push(sample_at_ground(current.0.x, current.0.z, hm));
        }

        // Active-order line color: fight/attack red, patrol blue, guard
        // white — detected from the order markers riding along with the
        // plain `MoveTarget`.
        let has_patrol_queued = queue.is_some_and(|q| {
            q.commands
                .iter()
                .any(|c| matches!(c, QueuedCommand::Patrol(_)))
        });
        let active_color = if attack_move.is_some() || attack_ground.is_some() {
            FIGHT_COLOR
        } else if has_patrol_queued {
            PATROL_COLOR
        } else if guard.is_some() {
            GUARD_COLOR
        } else {
            MOVE_COLOR
        };

        draw_dashed_polyline(&mut gizmos, &points, active_color, hm);

        // Ring at the final point of the active order.
        let end = *points.last().unwrap();
        gizmos.circle(
            Isometry3d::new(end, Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
            DISC_RADIUS,
            active_color,
        );

        // Queued follow-ups: straight dashed segments between successive
        // targets plus a ring at each.
        if let Some(queue) = queue {
            let mut prev = end;
            for cmd in &queue.commands {
                let pos = cmd.position();
                let to = sample_at_ground(pos.x, pos.z, hm);
                let color = match cmd {
                    QueuedCommand::Move(_) => MOVE_COLOR,
                    QueuedCommand::BuildAt { .. } => BUILD_COLOR,
                    QueuedCommand::Patrol(_) => PATROL_COLOR,
                    QueuedCommand::AttackMove(_) => FIGHT_COLOR,
                    QueuedCommand::AttackUnit { .. } => FIGHT_COLOR,
                    QueuedCommand::Guard(_) => GUARD_COLOR,
                };
                draw_dashed_polyline(&mut gizmos, &[prev, to], color, hm);
                gizmos.circle(
                    Isometry3d::new(to, Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
                    DISC_RADIUS,
                    color,
                );
                prev = to;
            }
        }
    }
}

/// Draw a polyline in world space as a repeating `-.-.` dash pattern,
/// re-sampling Y from the terrain at each dash endpoint so the line hugs
/// the ground instead of cutting straight through hills.
fn draw_dashed_polyline(
    gizmos: &mut Gizmos<CommandLineGizmos>,
    points: &[Vec3],
    color: Color,
    heightmap: Option<&Heightmap>,
) {
    // Pattern walker: `cursor` is how far into the current pattern entry
    // we've consumed. Persisting across segments keeps the `-.-.` rhythm
    // continuous through waypoint corners.
    let mut pattern_idx = 0usize;
    let mut cursor = 0.0f32;

    for pair in points.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let dx = end.x - start.x;
        let dz = end.z - start.z;
        let seg_len = (dx * dx + dz * dz).sqrt();
        if seg_len < 1e-4 {
            continue;
        }
        let step_x = dx / seg_len;
        let step_z = dz / seg_len;

        let mut t = 0.0f32;
        while t < seg_len {
            let (entry_len, visible) = DASH_PATTERN[pattern_idx];
            let remaining = entry_len - cursor;
            let advance = remaining.min(seg_len - t);

            if visible {
                let a = sample_at_ground(start.x + step_x * t, start.z + step_z * t, heightmap);
                let b = sample_at_ground(
                    start.x + step_x * (t + advance),
                    start.z + step_z * (t + advance),
                    heightmap,
                );
                gizmos.line(a, b, color);
            }

            t += advance;
            cursor += advance;
            if cursor >= entry_len - 1e-4 {
                cursor = 0.0;
                pattern_idx = (pattern_idx + 1) % DASH_PATTERN.len();
            }
        }
    }
}

#[cfg(test)]
mod tilt_tests {
    use super::*;

    const EPS: f32 = 1e-4;

    fn local_axis(rot: Quat, axis: Vec3) -> Vec3 {
        rot * axis
    }

    /// 30° slope descending along +Z: surface normal tilts toward +Z.
    fn downhill_normal() -> Vec3 {
        Vec3::new(
            0.0,
            30.0_f32.to_radians().cos(),
            30.0_f32.to_radians().sin(),
        )
    }

    #[test]
    fn flat_terrain_is_identity_for_plus_z() {
        let rot = surface_aligned_rotation(Vec3::Z, Vec3::Y);
        // Facing +Z means a 180° yaw (Bevy forward is −Z) with zero tilt.
        assert!((rot * -Vec3::Z - Vec3::Z).length() < EPS);
        assert!((rot * Vec3::Y - Vec3::Y).length() < EPS);
    }

    #[test]
    fn downhill_up_matches_normal_exactly() {
        let n = downhill_normal();
        let rot = surface_aligned_rotation(Vec3::Z, n);
        assert!((local_axis(rot, Vec3::Y) - n).length() < EPS);
    }

    #[test]
    fn downhill_forward_stays_in_plane_without_sideways_tilt() {
        let n = downhill_normal();
        let rot = surface_aligned_rotation(Vec3::Z, n);
        let fwd = local_axis(rot, -Vec3::Z);
        // In-plane: perpendicular to the normal.
        assert!(
            fwd.dot(n).abs() < EPS,
            "forward not in plane: dot={}",
            fwd.dot(n)
        );
        // Descending along +Z: the in-plane forward points down (y < 0).
        assert!(fwd.y < 0.0);
        // No sideways lean: the body right axis stays perpendicular to
        // the normal AND horizontal on a pure downhill (fall line along
        // the movement axis).
        let right = local_axis(rot, Vec3::X);
        assert!(right.dot(n).abs() < EPS);
        assert!(right.y.abs() < EPS);
    }

    /// The reported-bug scenario: moving diagonally across a slope. The
    /// exact basis keeps every body axis in its plane; the old
    /// world-axis Euler composition could not.
    #[test]
    fn diagonal_descent_matches_plane() {
        let n = downhill_normal();
        let forward = Vec3::new(1.0, 0.0, 1.0).normalize();
        let rot = surface_aligned_rotation(forward, n);
        assert!((local_axis(rot, Vec3::Y) - n).length() < EPS);
        let fwd = local_axis(rot, -Vec3::Z);
        assert!(fwd.dot(n).abs() < EPS);
        let right = local_axis(rot, Vec3::X);
        assert!(right.dot(n).abs() < EPS);
    }

    /// Turning on a slope: same normal, opposite heading — both
    /// orientations keep the up axis on the normal (the old local-tilt
    /// slerp made the buffered pitch read as a sideways lean mid-turn).
    #[test]
    fn up_axis_stable_when_reversing_on_slope() {
        let n = downhill_normal();
        let a = surface_aligned_rotation(Vec3::Z, n);
        let b = surface_aligned_rotation(-Vec3::Z, n);
        assert!((local_axis(a, Vec3::Y) - n).length() < EPS);
        assert!((local_axis(b, Vec3::Y) - n).length() < EPS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spring_pathfinding::SpeedMap;

    fn bucket_with(cap: f32) -> NavBucket {
        // Tiny 2×2 speed map — we only care about the `max_slope` value
        // for selection tests, not the grid contents.
        NavBucket {
            max_slope: cap,
            speed_map: SpeedMap::uniform(2, 2, 1.0),
        }
    }

    #[test]
    fn bucket_for_picks_first_cap_at_or_above_unit_cap() {
        let mut set = NavGridSet::default();
        set.buckets
            .extend([bucket_with(0.2), bucket_with(0.5), bucket_with(1.0)]);

        // Bit with MaxSlope=21° (tan ≈ 0.384) gets the 0.5 bucket —
        // tightest grid whose cap still covers what the unit can climb.
        assert_eq!(set.bucket_for(0.384), 1);
        // Exact match picks that bucket.
        assert_eq!(set.bucket_for(0.5), 1);
        // Byte with MaxSlope=60° (tan ≈ 1.73) exceeds every cap; fall
        // back to the loosest so it's not falsely blocked.
        assert_eq!(set.bucket_for(1.73), 2);
        // A cap below the tightest still resolves to the tightest.
        assert_eq!(set.bucket_for(0.1), 0);
    }
}

#[cfg(test)]
mod heat_tests {
    use super::*;
    use spring_pathfinding::HeatMap;
    use crate::units::components::TeamId;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    /// Walking ground units deposit heat at their cell; a full decay
    /// step multiplies the grid down by `retention^period`. Flyers
    /// leave no trail.
    #[test]
    fn walkers_deposit_heat_and_decay_shrinks_it() {
        let mut world = World::new();
        world.init_resource::<Time>();
        world.insert_resource(UnitRegistry::empty());
        world.insert_resource(PathHeat(HeatMap::new(8, 8)));

        let walker = world
            .spawn((
                UnitType(UnitKind::Bit),
                UnitStats {
                    radius: 12.0,
                    hit_radius: 20.0,
                    speed: 90.0,
                    accel: 27.0,
                    brake: 60.0,
                    turn_rate: 3.0,
                    can_fly: false,
                    no_chase_vtol: true,
                },
                TeamId(0),
                GlobalTransform::from_xyz(32.0, 0.0, 8.0),
                MovePath::new(vec![Vec3::new(500.0, 0.0, 8.0)], Vec3::new(500.0, 0.0, 8.0)),
            ))
            .id();
        let flyer = world
            .spawn((
                UnitType(UnitKind::Flow),
                UnitStats {
                    radius: 12.0,
                    hit_radius: 20.0,
                    speed: 30.0,
                    accel: 9.0,
                    brake: 27.0,
                    turn_rate: 3.0,
                    can_fly: true,
                    no_chase_vtol: false,
                },
                TeamId(0),
                GlobalTransform::from_xyz(40.0, 0.0, 8.0),
                MovePath::new(vec![Vec3::new(500.0, 0.0, 8.0)], Vec3::new(500.0, 0.0, 8.0)),
            ))
            .id();

        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_millis(500));
        world.run_system_once(update_path_heat).unwrap();

        let walker_heat = world.resource::<PathHeat>().0.get([32.0, 8.0]);
        assert!(
            walker_heat > 0.0,
            "walker must deposit heat at its cell, got {walker_heat}"
        );
        assert_eq!(
            world.resource::<PathHeat>().0.get([40.0, 8.0]),
            0.0,
            "flyers must leave no heat"
        );
        let deposited = walker_heat;

        // Despawn the walkers so the next call is decay-only: a
        // standing unit would keep re-depositing, and the test wants
        // the isolated decay step.
        world.despawn(walker);
        world.despawn(flyer);
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(HEAT_DECAY_PERIOD));
        world.run_system_once(update_path_heat).unwrap();

        let decayed = world.resource::<PathHeat>().0.get([32.0, 8.0]);
        let expected_factor = HEAT_RETENTION_PER_SECOND.powf(HEAT_DECAY_PERIOD);
        assert!(
            (decayed - deposited * expected_factor).abs() < deposited * 0.05,
            "decay step must multiply by retention^{HEAT_DECAY_PERIOD}: {deposited} → {decayed}"
        );
    }

    /// The real FBI + MOVEINFO.TDF pair drives per-class heat: Bits
    /// (LIGHT) deposit 10/s, Bytes (HEAVY) 500/s.
    #[test]
    fn fbi_movement_class_selects_heat_params() {
        let registry = crate::units::content::unit_registry::UnitRegistry::load();
        assert!((registry.heat_produced(UnitKind::Bit) - 10.0).abs() < 1e-4);
        assert!((registry.heat_retention(UnitKind::Bit) - 0.10).abs() < 1e-4);
        assert!((registry.heat_produced(UnitKind::Byte) - 500.0).abs() < 1e-4);
        assert!((registry.heat_retention(UnitKind::Byte) - 0.002).abs() < 1e-4);
    }
}

#[cfg(test)]
mod cross_map_tests {
    use super::*;
    use spring_pathfinding::HeatMap;
    use crate::units::components::TeamId;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    /// Ground-truth diagnostic: load a real shipped map, build the nav
    /// grid the game builds, and drive a real Bit through the real
    /// `movement_system` to targets all over the map — including up
    /// plateau ramps. Reports which targets are reachable.
    #[test]
    #[ignore = "manual diagnostic — run with -- --ignored --nocapture"]
    fn drive_bit_across_real_maps() {
        for map_name in ["Central_Hub", "DigitalDivide_PT2"] {
            let path = format!("assets/maps/{map_name}.kpmap");
            let Ok(bytes) = std::fs::read(&path) else {
                eprintln!("skip {map_name}: no {path}");
                continue;
            };
            let baked = spring_map::baked::read_baked_map(&bytes).expect("baked map");
            let start_positions = baked
                .map_info
                .as_ref()
                .map(|info| info.start_positions.clone())
                .unwrap_or_default();
            let parsed = baked.parsed;
            let heightmap = Heightmap::from_parsed(&parsed);

            let cap = spring_pathfinding::max_slope_from_degrees(36.0);
            let speed_map = spring_pathfinding::SpeedMap::from_heightmap(
                &parsed.heights,
                parsed.header.heightmap_width() as u32,
                parsed.header.heightmap_height() as u32,
                cap,
                spring_pathfinding::slope_mod_from_max_slope(cap),
            );

            let mut nav = NavGridSet::default();
            nav.buckets.push(NavBucket {
                max_slope: cap,
                speed_map,
            });

            let mut world = World::new();
            world.init_resource::<Time>();
            world.insert_resource(UnitRegistry::load());
            world.insert_resource(nav);
            world.insert_resource(heightmap);
            let heat_dims = (
                parsed.header.heightmap_width() as u32 - 1,
                parsed.header.heightmap_height() as u32 - 1,
            );
            world.insert_resource(PathHeat(HeatMap::new(heat_dims.0, heat_dims.1)));

            // Start/target: the map's authored start positions — the
            // places the game actually spawns homebases. If units can't
            // march between those, the map is unplayable.
            let start = start_positions
                .first()
                .map(|sp| Vec3::new(sp.x, 0.0, sp.z))
                .unwrap_or(Vec3::new(2048.0, 0.0, 64.0));
            let enemy_spawn = start_positions.get(1).map(|sp| Vec3::new(sp.x, 0.0, sp.z));
            println!(
                "  {map_name}: start at ({:.0},{:.0}), enemy spawn {:?}",
                start.x,
                start.z,
                enemy_spawn.map(|e| (e.x, e.z)),
            );

            let unit = world
                .spawn((
                    UnitType(UnitKind::Bit),
                    UnitStats {
                        radius: 12.0,
                        hit_radius: 20.0,
                        speed: 90.0,
                        accel: 27.0,
                        brake: 60.0,
                        turn_rate: 6.0,
                        can_fly: false,
                        no_chase_vtol: true,
                    },
                    TeamId(0),
                    Transform::from_translation(start),
                    GroundMover::new(UnitKind::Bit, &UnitRegistry::load(), &UnitStats::from_registry(UnitKind::Bit, &UnitRegistry::load(), 20.0)),
                ))
                .id();

            let max_cell = (heat_dims.0 as usize).min(504);
            let mut reached = 0usize;
            let mut total = 0usize;
            for (tx, tz) in [
                (max_cell, max_cell),
                (max_cell, 8),
                (8, max_cell),
                (256, 256),
                (256, 8),
                (8, 256),
            ] {
                let (tx, tz) = (tx.min(max_cell), tz.min(max_cell));
                let target = Vec3::new(tx as f32 * 8.0, 0.0, tz as f32 * 8.0);
                total += 1;

                // Reset the unit for this leg.
                world
                    .entity_mut(unit)
                    .insert(MoveTarget(target))
                    .insert(Transform::from_translation(start));
                world
                    .resource_mut::<Time>()
                    .advance_by(Duration::from_secs(1));

                let mut arrived_at = None;
                for tick in 0..2400 {
                    world
                        .resource_mut::<Time>()
                        .advance_by(Duration::from_millis(33));
                    world.run_system_once(movement_system).unwrap();
                    let pos = world.get::<Transform>(unit).unwrap().translation;
                    let dxz = ((pos.x - target.x).powi(2) + (pos.z - target.z).powi(2)).sqrt();
                    if dxz < 16.0 {
                        arrived_at = Some(tick);
                        break;
                    }
                }
                let pos = world
                    .get::<Transform>(unit)
                    .map(|t| t.translation)
                    .unwrap_or_default();
                let hm = world.resource::<Heightmap>();
                let h_start = hm.sample(start.x, start.z);
                let h_target = hm.sample(target.x, target.z);
                match arrived_at {
                    Some(tick) => {
                        reached += 1;
                        println!(
                            "  {map_name} -> cell {tx},{tz} (dh={:+.0}): ARRIVED {} ticks ({:.0}s) y={:.1}",
                            h_target - h_start,
                            tick,
                            tick as f32 * 0.033,
                            pos.y,
                        );
                    }
                    None => {
                        let mt = world.get::<MoveTarget>(unit).is_some();
                        let mp = world
                            .get::<MovePath>(unit)
                            .map(|p| (p.current, p.waypoints.len()));
                        let dxz = ((pos.x - target.x).powi(2) + (pos.z - target.z).powi(2)).sqrt();
                        println!(
                            "  {map_name} -> cell {tx},{tz} (dh={:+.0}): STUCK at ({:.0},{:.0}) dxz {:.0} y={:.1} order_alive={mt} path={mp:?}",
                            h_target - h_start,
                            pos.x,
                            pos.z,
                            dxz,
                            pos.y,
                        );
                    }
                }
            }
            // The leg that matters: spawn-to-spawn.
            if let Some(espawn) = enemy_spawn {
                total += 1;
                world
                    .entity_mut(unit)
                    .insert(MoveTarget(espawn))
                    .insert(Transform::from_translation(start));
                world
                    .resource_mut::<Time>()
                    .advance_by(Duration::from_secs(1));
                let mut arrived_at = None;
                for tick in 0..3600 {
                    world
                        .resource_mut::<Time>()
                        .advance_by(Duration::from_millis(33));
                    world.run_system_once(movement_system).unwrap();
                    let pos = world.get::<Transform>(unit).unwrap().translation;
                    if ((pos.x - espawn.x).powi(2) + (pos.z - espawn.z).powi(2)).sqrt() < 20.0 {
                        arrived_at = Some(tick);
                        break;
                    }
                }
                match arrived_at {
                    Some(tick) => {
                        reached += 1;
                        println!(
                            "  {map_name} -> ENEMY SPAWN: ARRIVED {} ticks ({:.0}s)",
                            tick,
                            tick as f32 * 0.033,
                        );
                    }
                    None => println!("  {map_name} -> ENEMY SPAWN: FAILED TO ARRIVE"),
                }
            }
            println!("{map_name}: {reached}/{total} targets reached");
        }
    }
}

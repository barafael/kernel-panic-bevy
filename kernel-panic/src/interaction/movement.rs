use std::collections::HashMap;

use bevy::prelude::*;

use spring_pathfinding::{HeatMap, SpeedMap, find_path_with_heat, slope_from_rise_run};

use super::selection::Selected;
use crate::map_events::CircularFlow;
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{
    AimTarget, AttackGroundOrder, AttackTargetOrder, CHASE_REPATH_DISTANCE, DeployState,
    Deployable, Dying, ForcedTarget,
};
use crate::units::components::{UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

/// Rate at which the pitch/roll component of a unit's rotation relaxes
/// toward the slope-aligned target. Higher = snappier tilt, lower = more
/// sluggish. Yaw is set directly (unaffected by this constant).
const TILT_SMOOTH_RATE: f32 = 8.0;

/// Dedicated gizmo config for command-line overlays so the dashed path
/// renders thinner than the default 2-px gizmo width used elsewhere.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct CommandLineGizmos;

/// When present, the unit will move toward this world position.
#[derive(Component)]
pub struct MoveTarget(pub Vec3);

/// The unit's current longitudinal speed in elmos/s. Ramps from 0 at
/// `UnitStats::accel` while under way, brakes at `UnitStats::brake` when
/// the order completes or is dropped, and is clamped near the final
/// waypoint so a unit arrives at rest instead of overshooting —
/// Spring's `CGroundMoveType` speed control in miniature. Units without
/// the component (tests, pre-existing spawns) run speed directly.
#[derive(Component, Default)]
pub struct CurrentSpeed(pub f32);

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
#[derive(Component)]
pub struct MovePath {
    pub waypoints: Vec<Vec3>,
    /// Index of the next waypoint to reach.
    pub current: usize,
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

/// Per-frame snapshot of every unit, used by the movement pass to resolve
/// collisions and decide when a waypoint is blocked by an "arrived" unit.
pub struct UnitSnapshot {
    entity: Entity,
    pos: Vec3,
    radius: f32,
    /// Whether this unit kind is capable of moving (speed > 0).
    mobile: bool,
    /// Flying units pass over ground units without pushing or being pushed
    /// in the XZ plane, so the collision resolver skips air↔ground pairs.
    flying: bool,
    /// Whether this specific unit has no active move order right now — i.e.
    /// it has reached its goal (or never had one). Used by the deadlock
    /// breaker to skip waypoints that a stationary unit is standing on.
    stationary: bool,
}

/// How far above the sampled ground height to draw command-line gizmos
/// so they don't z-fight with the terrain.
const GIZMO_LIFT: f32 = 1.5;

/// Cap on the number of fresh paths `movement_system` will compute in a
/// single frame. Extra units keep their stationary state until a later
/// frame picks them up; prevents a 30-unit AI army launch from burning
/// one frame on pathfinding and causing a visible hang.
const PATHFIND_BUDGET_PER_FRAME: usize = 3;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
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

/// The movement god-query, bundled: one struct instead of a 14-slot
/// tuple plus seven loose params (the old signature needed
/// `clippy::too_many_arguments` + `clippy::type_complexity` waivers).
#[derive(bevy::ecs::system::SystemParam)]
#[allow(clippy::type_complexity)]
pub struct MovementQuery<'w, 's> {
    pub query: Query<
        'w,
        's,
        (
            Entity,
            &'static UnitType,
            &'static UnitStats,
            &'static mut Transform,
            Option<&'static MoveTarget>,
            Option<&'static mut MovePath>,
            Option<&'static mut CommandQueue>,
            Option<&'static Deployable>,
            Option<&'static mut SlopeTilt>,
            Option<&'static crate::units::combat::Stunned>,
            Option<&'static crate::units::mechanics::network_buffer::SpeedBoost>,
            Option<&'static AttackMoveActive>,
            Option<&'static AimTarget>,
            Option<&'static crate::units::assets::animation::UnitAnimator>,
            Option<&'static mut CurrentSpeed>,
        ),
        Without<Dying>,
    >,
    pub unit_registry: Res<'w, UnitRegistry>,
}

pub fn movement_system(
    mut commands: Commands,
    time: Res<Time>,
    nav_set: Option<Res<NavGridSet>>,
    heightmap: Option<Res<Heightmap>>,
    circular_flow: Option<Res<CircularFlow>>,
    path_heat: Option<Res<PathHeat>>,
    mut m: MovementQuery,
    // Reused across frames so the full-unit snapshot doesn't reallocate
    // each tick.
    mut snapshot: Local<Vec<UnitSnapshot>>,
    // Retained grid buckets over `snapshot` — same hoist trick.
    mut grid: Local<HashMap<(i32, i32), Vec<usize>>>,
) {
    let MovementQuery {
        ref mut query,
        ref unit_registry,
    } = m;
    let unit_registry = &**unit_registry;
    // Snapshot every unit's position and collision radius so each proposed
    // movement can be resolved against all others without query aliasing.
    // `mobile` is "this unit kind *could* move", `stationary` is "this
    // specific unit has no active move order right now" — the deadlock
    // breaker uses the latter to decide whether a blocker counts as
    // "already at its goal".
    snapshot.clear();
    snapshot.extend(
        query.iter().map(
            |(e, _, stats, tf, target, _, _, _, _, _, _, _, _, _, _)| UnitSnapshot {
                entity: e,
                pos: tf.translation,
                radius: stats.radius,
                mobile: stats.speed > 0.0,
                flying: stats.can_fly,
                stationary: target.is_none(),
            },
        ),
    );

    // Index the snapshot into a retained cell grid so each moving unit
    // resolves against its neighbours instead of the whole army. Buckets
    // clear (not reallocate) each frame, mirroring `unit_separation_system`.
    grid.clear();
    let mut max_ground_radius = 0.0_f32;
    for (idx, entry) in snapshot.iter().enumerate() {
        if !entry.flying {
            grid.entry(cell_of(entry.pos.x, entry.pos.z))
                .or_default()
                .push(idx);
            max_ground_radius = max_ground_radius.max(entry.radius);
        }
    }
    let grid = SnapshotGrid {
        entries: &snapshot,
        cells: &grid,
        max_ground_radius,
    };

    let mut pathfinds_used: usize = 0;

    for (
        entity,
        unit_type,
        stats,
        mut transform,
        move_target,
        move_path,
        mut queue,
        deployable,
        mut slope_tilt,
        stunned,
        speed_boost,
        attack_move_active,
        aim_target,
        animator,
        mut current_speed,
    ) in &mut *query
    {
        if stunned.is_some() {
            continue;
        }

        let speed = stats.speed + speed_boost.map_or(0.0, |b| b.0);
        if speed == 0.0 {
            // Buildings can't move — remove any movement components.
            commands.entity(entity).remove::<MoveTarget>();
            commands.entity(entity).remove::<MovePath>();
            commands.entity(entity).remove::<CommandQueue>();
            continue;
        }

        // No live order: coast down to rest so a fresh order starts
        // from a stopped state instead of teleporting into motion.
        if move_target.is_none() && move_path.is_none() {
            if let Some(cs) = current_speed.as_deref_mut() {
                cs.0 = (cs.0 - stats.brake * time.delta_secs()).max(0.0);
            }
            continue;
        }

        // Deployable units (e.g. Pointer) cannot move until they have fully
        // closed up — this is what makes them "stop, pack, then drive". The
        // state machine in `tick_deploy_state` triggers `Close()` as soon as
        // a move order arrives; movement waits for the close animation to
        // finish before stepping the unit forward.
        if let Some(d) = deployable
            && d.state != DeployState::Closed
        {
            continue;
        }

        let flying = stats.can_fly;

        // Attack-move: hold position and fight while a hostile is in
        // weapon range (an `AimTarget` is stamped by `combat_system`),
        // then resume marching toward the destination once it clears.
        if attack_move_active.is_some() && aim_target.is_some() {
            continue;
        }

        // If we have a MoveTarget but no MovePath, compute the path.
        // Flying units skip the nav grid entirely and take a straight XZ
        // line to the target — they can cross any terrain, so routing
        // around cliffs would only add noise. Ground pathfinds cost
        // real CPU, so cap how many we do per frame — surplus units
        // just wait one extra frame for their turn.
        if let Some(target) = move_target
            && move_path.is_none()
        {
            // Outcome of this frame's pathing attempt for this unit:
            // - `Route(waypoints)` → follow them.
            // - `Unreachable` → the goal cannot be reached at all;
            //   refuse the order (upstream `pathingFailed`) instead of
            //   walking into the nearest wall and camping there.
            // - `None` → nothing decided this frame (no nav grid yet
            //   mid-load, or the per-frame search budget ran out);
            //   keep the order and retry next frame.
            let outcome = if flying {
                Some(PathOutcome::Route(vec![Vec3::new(
                    target.0.x, 0.0, target.0.z,
                )]))
            } else if let Some(nav) = nav_set.as_deref() {
                if pathfinds_used < PATHFIND_BUDGET_PER_FRAME {
                    pathfinds_used += 1;
                    compute_path(
                        Some(nav),
                        unit_registry,
                        unit_type.0,
                        transform.translation,
                        target.0,
                        path_heat.as_deref().map(|h| &h.0),
                    )
                } else {
                    None
                }
            } else {
                None
            };
            match outcome {
                Some(PathOutcome::Route(waypoints)) if !waypoints.is_empty() => {
                    commands.entity(entity).insert(MovePath {
                        waypoints,
                        current: 0,
                    });
                }
                Some(PathOutcome::Unreachable) => {
                    commands.entity(entity).remove::<MoveTarget>();
                }
                _ => {}
            }
        }

        // Follow the path waypoint by waypoint.
        let Some(mut path) = move_path else {
            continue;
        };

        if path.current >= path.waypoints.len() {
            // Path complete — promote the next queued command if any.
            // The stop-distance clamp got us here at walking pace; zero
            // out the remaining momentum so the promoted order (or
            // standstill) starts from rest.
            commands.entity(entity).remove::<MovePath>();
            if let Some(cs) = current_speed.as_deref_mut() {
                cs.0 = 0.0;
            }
            let next = queue.as_mut().and_then(|q| {
                if q.commands.is_empty() {
                    None
                } else {
                    Some(q.commands.remove(0))
                }
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
                    let origin = Vec3::new(transform.translation.x, 0.0, transform.translation.z);
                    commands
                        .entity(entity)
                        .insert(MoveTarget(pos))
                        .remove::<crate::units::lifecycle::construction::PendingBuild>();
                    if let Some(queue) = queue.as_mut() {
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
            continue;
        }

        let current = transform.translation;
        let waypoint = path.waypoints[path.current];
        let goal = Vec3::new(waypoint.x, current.y, waypoint.z);
        let diff = goal - current;
        let distance = diff.length();

        let self_radius = stats.radius;

        // Arrival is "within my own footprint of the waypoint" — this lets
        // crowds converging on the same target settle at the boundary of
        // their neighbours rather than jittering on top of it.
        //
        // Deadlock breaker: if the waypoint is occupied by a unit that has
        // already stopped (no move order of its own), also count it as
        // reached so we don't keep pushing through a crowd that's arrived.
        let arrival_threshold = (self_radius + 2.0).max(8.0);
        if distance < arrival_threshold
            || waypoint_blocked_by_arrived_unit(entity, goal, self_radius, &grid)
        {
            // Completing the *final* waypoint ends the leg at rest
            // (mid-path waypoints stay fly-through — braking for those
            // would make every path choppy stop-and-go).
            let was_final = path.current == path.waypoints.len() - 1;
            path.current += 1;
            if was_final && let Some(cs) = current_speed.as_deref_mut() {
                cs.0 = 0.0;
            }
            continue;
        }

        let direction = diff / distance;
        let dt = time.delta_secs();

        // Rotate toward the desired heading at the unit's FBI TurnRate.
        // Spring ties forward motion to facing: while the unit is still
        // swinging around, it moves at reduced speed (falling to zero for a
        // full-reverse heading). We reproduce that with a cos(error) gate
        // so high-TurnRate units snap-and-drive and clunky ones (Pointer,
        // Worm, Dos) visibly pivot before committing to the new heading.
        let desired_forward = Vec3::new(direction.x, 0.0, direction.z);
        let current_forward = transform.forward().as_vec3();
        let current_xz = {
            let mut f = Vec3::new(current_forward.x, 0.0, current_forward.z);
            if f.length_squared() < 1e-6 {
                f = Vec3::Z;
            }
            f.normalize()
        };

        let turn_rate = stats.turn_rate;
        let max_turn = if turn_rate > 0.0 {
            turn_rate * dt
        } else {
            // TurnRate=0 means "no rotation delay in the FBI" — snap.
            std::f32::consts::TAU
        };
        let new_forward = rotate_toward_xz(current_xz, desired_forward, max_turn);

        // Apply the rotation before considering whether the unit translates.
        // Sharp turns (cos_err < 0.5 below) skip translation entirely so the
        // unit pivots in place; if we gated the rotation behind translation
        // too, the unit would freeze and never finish the turn.
        if new_forward.length_squared() > 1e-6 {
            // Target orientation: heading = rate-limited new_forward,
            // up = terrain normal — one exact basis (see
            // `surface_aligned_rotation`).
            let target = match heightmap.as_deref() {
                Some(hm) => {
                    let normal = hm.normal(transform.translation.x, transform.translation.z);
                    surface_aligned_rotation(new_forward, normal)
                }
                None => {
                    Transform::default()
                        .looking_to(new_forward, Vec3::Y)
                        .rotation
                }
            };
            // Smooth in WORLD space. A local-space tilt buffer applied
            // after a yaw change re-interprets a buffered downhill pitch
            // as a sideways lean mid-turn; smoothing the world
            // orientation keeps the unit glued to the surface plane
            // through turns.
            let blend = 1.0 - (-TILT_SMOOTH_RATE * dt).exp();
            let smoothed = match slope_tilt.as_deref_mut() {
                Some(t) => {
                    t.0 = t.0.slerp(target, blend);
                    t.0
                }
                None => {
                    commands.entity(entity).insert(SlopeTilt(target));
                    target
                }
            };
            transform.rotation = smoothed;
        }

        // Facing-gated forward speed. Within ~60° of the target heading,
        // drive at cos(err); beyond that, pivot in place (no translation)
        // so the unit doesn't arc wide during sharp turns.
        let cos_err = new_forward.dot(desired_forward);
        let align = if cos_err > 0.5 { cos_err } else { 0.0 };

        // Longitudinal speed control: accelerate toward the FBI speed,
        // brake for the final waypoint so the unit arrives at rest
        // (`v = √(2·a·d)` is the fastest speed that can still stop in
        // `distance`), and record the result for the next frame.
        let mut target_speed = speed;
        if path.current == path.waypoints.len() - 1 {
            let stop_speed = (2.0 * stats.brake * distance).sqrt();
            target_speed = target_speed.min(stop_speed);
        }
        let drive_speed = match current_speed.as_deref_mut() {
            Some(cs) => {
                if cs.0 < target_speed {
                    cs.0 = (cs.0 + stats.accel * dt).min(target_speed);
                } else {
                    cs.0 = (cs.0 - stats.brake * dt).max(target_speed);
                }
                cs.0
            }
            // No component (tests, legacy spawns): run at full speed.
            None => target_speed,
        };
        let mut step = drive_speed * dt * align;
        // Animation gate: a driver holds its unit in place while a fold
        // choreography must complete before driving (Byte: fold-to-move).
        step *= animator.map_or(1.0, |a| a.rig.move_gate);
        if let Some(flow) = circular_flow.as_deref() {
            step *= flow.step_multiplier(current, new_forward);
        }
        if step < 1e-4 {
            continue;
        }
        let desired = new_forward * step.min(distance);

        // Resolve desired motion against every other unit. Spring-style:
        // units push each other with radial + lateral slide, weighted by
        // mass/speed/head-on factor. See `resolve_motion`. Flying units
        // skip collision entirely — nothing on the ground obstructs them,
        // and they pass over each other freely too.
        let resolved = if flying {
            desired
        } else {
            resolve_motion(entity, current, desired, self_radius, speed, &grid)
        };

        // Slope gate for the no-navgrid straight-line fallback only.
        // Signed, so descents always pass — a unit can step off a
        // ledge it can't climb back up.
        //
        // Why not while following a real path: the pathfinder already
        // guarantees every crossed cell is within the unit's MaxSlope
        // (that's what the nav bucket encodes — same as upstream
        // Spring, which has no per-step slope re-check). This gate
        // used to run unconditionally and re-deriving slope from the
        // *bilinear* per-step sample measures stepped terrain at up to
        // twice the pathfinder's cell-average — so the first uphill
        // tick on a legal ramp got refused, the waypoint-skip burned
        // the whole path within a few ticks, and the unit froze at the
        // base of slopes it was supposed to cross. Nobody Advanced.
        if !flying
            && nav_set.is_none()
            && let Some(ref hm) = heightmap
        {
            let dxz = Vec3::new(resolved.x, 0.0, resolved.z).length();
            if dxz > 1e-4 {
                let proposed = current + resolved;
                let rise = hm.sample(proposed.x, proposed.z) - hm.sample(current.x, current.z);
                if rise > 0.0 {
                    let step_slope = slope_from_rise_run(rise, dxz);
                    let cap = unit_registry.max_slope_ratio(unit_type.0);
                    if step_slope > cap * 1.2 {
                        path.current += 1;
                        continue;
                    }
                }
            }
        }

        transform.translation += resolved;

        // Altitude: ground units hug the terrain; flying units hover at
        // their FBI `cruiseAlt` above it, so hills/cliffs pass underneath
        // without colliding. Ground is sampled either way so air units
        // rise over rolling terrain instead of staying at a fixed world Y.
        if let Some(ref hm) = heightmap {
            let ground = hm.sample(transform.translation.x, transform.translation.z);
            transform.translation.y = if flying {
                ground + stats.cruise_alt
            } else {
                ground
            };
        }
    }
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
                commands
                    .entity(entity)
                    .insert(MoveTarget(target_pos))
                    .remove::<MovePath>();
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

/// Resolve `desired` motion against neighbouring units using a Spring-style
/// push + lateral slide. Inspired by `CGroundMoveType::CalculatePushVector`
/// in the Recoil engine: units don't hard-stop at contact, they slide past
/// each other, with head-on collisions weighted more heavily (so the
/// side-crosser yields to the head-on runner) and heavier/faster units
/// pushing lighter/slower ones.
///
/// Returns the delta to add to the unit's position this frame. Only the XZ
/// plane is considered.
/// Cell size for the movement snapshot grid. Generous enough that a
/// typical query (self radius + largest footprint + one frame's step,
/// ~90 elmos worst case) touches at most the 5×5 block around its cell.
const SNAPSHOT_CELL: f32 = 64.0;

fn cell_of(x: f32, z: f32) -> (i32, i32) {
    (
        (x / SNAPSHOT_CELL).floor() as i32,
        (z / SNAPSHOT_CELL).floor() as i32,
    )
}

/// Uniform grid over a frame's [`UnitSnapshot`]s. `resolve_motion` and
/// `waypoint_blocked_by_arrived_unit` used to scan the entire snapshot
/// per moving unit — O(moving × N), the dominant cost in big battles.
/// A query visits only the cells its search circle overlaps, so the
/// scan shrinks to the units actually nearby.
struct SnapshotGrid<'a> {
    entries: &'a [UnitSnapshot],
    cells: &'a HashMap<(i32, i32), Vec<usize>>,
    /// Largest radius among non-flying entries — lets callers bound a
    /// search circle without a second pass.
    max_ground_radius: f32,
}

impl SnapshotGrid<'_> {
    /// Invoke `f` for every non-flying entry whose center lies within
    /// `radius` elmos of (x, z) *by cell distance* — i.e. a superset of
    /// the true circle. Callers do the exact distance test inside `f`.
    fn for_each_near(&self, x: f32, z: f32, radius: f32, mut f: impl FnMut(&UnitSnapshot)) {
        let (cx, cz) = cell_of(x, z);
        let r = (radius / SNAPSHOT_CELL).ceil() as i32;
        for dx in -r..=r {
            for dz in -r..=r {
                let Some(bucket) = self.cells.get(&(cx + dx, cz + dz)) else {
                    continue;
                };
                for &i in bucket {
                    f(&self.entries[i]);
                }
            }
        }
    }
}

fn resolve_motion(
    self_entity: Entity,
    origin: Vec3,
    desired: Vec3,
    self_radius: f32,
    self_speed: f32,
    snapshot: &SnapshotGrid,
) -> Vec3 {
    let desired_xz = Vec3::new(desired.x, 0.0, desired.z);
    let desired_len = desired_xz.length();
    if desired_len < 1e-6 {
        return desired;
    }
    let front = desired_xz / desired_len;

    // Self's momentum proxy. Spring uses `mass * max(1, speed)`; we use
    // area (radius²) as a stand-in for mass since the FBI data we surface
    // doesn't include an explicit mass.
    let self_mass = self_radius * self_radius;

    let mut push = Vec3::ZERO;

    // Contact is tested at the *end* of the desired step, so the search
    // circle must cover the step length too.
    let search_radius = desired_len + self_radius + snapshot.max_ground_radius;
    let new_origin = origin + desired_xz;
    snapshot.for_each_near(new_origin.x, new_origin.z, search_radius, |other| {
        if other.entity == self_entity || other.flying {
            // Skip self and any airborne unit: the caller only invokes
            // this for ground units (fliers bypass collision entirely),
            // and a flier overhead shouldn't obstruct a walker below.
            return;
        }
        let sum_r = self_radius + other.radius;

        // Penetration depth once we take the step. Capped at sum_r so
        // a deep overlap (e.g. from a spawn on top of someone) still
        // produces a bounded correction.
        let sep = Vec3::new(new_origin.x - other.pos.x, 0.0, new_origin.z - other.pos.z);
        let dist = sep.length();
        if dist >= sum_r {
            return;
        }
        let penetration = (sum_r - dist).min(sum_r);

        // Direction from the obstacle toward us. If we're exactly on
        // top of the obstacle, bias away from our front so the tie is
        // broken cleanly.
        let away = if dist > 1e-4 { sep / dist } else { -front };

        // Head-on factor: perpendicular approach ≈ 1, direct head-on
        // approach ≈ 6. The sign is from the obstacle's perspective, so
        // we dot our forward with the vector *toward* them (−away).
        let head_on = 1.0 + (1.0 - front.dot(-away).abs().min(1.0)) * 5.0;

        // Other's mass proxy and weight. Static obstacles (buildings,
        // speed=0) behave like infinite mass — our share of the push
        // is effectively zero, so we take the full correction.
        let other_mass = other.radius * other.radius;
        let weight_self = self_mass * self_speed.max(1.0) * head_on;
        let other_effective_speed = if other.mobile { 1.0 } else { 1e6 };
        let weight_other = other_mass * other_effective_speed * head_on;
        let total = weight_self + weight_other;
        let other_share = if total > 1e-6 {
            weight_other / total
        } else {
            0.5
        };

        // Radial push: shove ourselves out by our share of the penetration.
        push += away * penetration * other_share;

        // Lateral slide — this is the "deflection" piece. Pick the side
        // that aligns with our forward so we slide *past* the obstacle
        // instead of bouncing back. `right` is the XZ perpendicular of
        // `front`; slide amount scales with penetration so shallow
        // grazes barely nudge, deep head-ons slip noticeably sideways.
        let right = Vec3::new(front.z, 0.0, -front.x);
        let side_sign = if right.dot(away) >= 0.0 { 1.0 } else { -1.0 };
        let slide_strength = penetration * 0.6 * other_share;
        push += right * side_sign * slide_strength;
    });

    // The final displacement is the desired step plus the accumulated
    // push. Cap total motion to desired_len so the resolver never moves
    // us *faster* than our speed would allow.
    let combined = desired_xz + push;
    let combined_len = combined.length();
    let capped = if combined_len > desired_len {
        combined * (desired_len / combined_len)
    } else {
        combined
    };

    Vec3::new(capped.x, desired.y, capped.z)
}

/// Spring's deadlock breaker: if the unit we'd be walking toward is
/// already sitting on the next waypoint and has no move order of its
/// own, don't keep shoving — declare that waypoint reached and advance.
/// Prevents pile-ups when a group converges on a target and the lead
/// units arrive while followers keep pushing.
fn waypoint_blocked_by_arrived_unit(
    self_entity: Entity,
    waypoint: Vec3,
    self_radius: f32,
    snapshot: &SnapshotGrid,
) -> bool {
    let mut blocked = false;
    snapshot.for_each_near(
        waypoint.x,
        waypoint.z,
        self_radius + snapshot.max_ground_radius,
        |other| {
            if blocked || other.entity == self_entity || !other.stationary {
                return;
            }
            let r = self_radius + other.radius;
            let dx = other.pos.x - waypoint.x;
            let dz = other.pos.z - waypoint.z;
            if dx * dx + dz * dz < r * r {
                blocked = true;
            }
        },
    );
    blocked
}

/// Safety-net that unsticks mobile units that *are already overlapping*,
/// which can happen on spawn, when a factory ejects onto a busy tile, or
/// when the terrain pushes a unit into a building. `movement_system` now
/// does the primary hard-collision work, so this only needs to correct
/// residual overlap with a gentle nudge — not drive the main separation.
pub fn unit_separation_system(
    mut units: Query<
        (Entity, &mut Transform, &UnitStats),
        Without<crate::units::lifecycle::spawning::Emerging>,
    >,
    time: Res<Time>,
    heightmap: Option<Res<Heightmap>>,
    mut snapshot: Local<Vec<SeparationEntry>>,
    mut grid: Local<HashMap<(i32, i32), Vec<usize>>>,
    mut pushes: Local<Vec<(Entity, Vec3)>>,
) {
    let dt = time.delta_secs();
    let push_strength = 30.0_f32;

    // Snapshot + per-frame bucket grid. Buckets are retained between frames
    // (`Local`), only the contents clear, so the allocator stays quiet after
    // warmup. `SEP_CELL` matches the largest footprint we see in practice
    // (~32 elmos), so each ground unit touches ≤9 neighbouring cells and
    // the inner loop shrinks from O(N²) to ~O(N·k) with k≈8.
    const SEP_CELL: f32 = 32.0;
    let to_cell = |x: f32, z: f32| -> (i32, i32) {
        ((x / SEP_CELL).floor() as i32, (z / SEP_CELL).floor() as i32)
    };

    snapshot.clear();
    for bucket in grid.values_mut() {
        bucket.clear();
    }
    for (e, tf, stats) in units.iter() {
        let idx = snapshot.len();
        snapshot.push(SeparationEntry {
            entity: e,
            pos: tf.translation,
            radius: stats.radius,
            mobile: stats.speed > 0.0,
            flying: stats.can_fly,
        });
        if !stats.can_fly {
            // Flyers don't participate in ground separation — omit from
            // the bucket so ground units don't scan through them.
            let key = to_cell(tf.translation.x, tf.translation.z);
            grid.entry(key).or_default().push(idx);
        }
    }

    pushes.clear();
    for i in 0..snapshot.len() {
        let me = &snapshot[i];
        if !me.mobile || me.flying {
            continue;
        }
        let (cx, cz) = to_cell(me.pos.x, me.pos.z);
        let mut push = Vec3::ZERO;
        for dx in -1..=1 {
            for dz in -1..=1 {
                let Some(bucket) = grid.get(&(cx + dx, cz + dz)) else {
                    continue;
                };
                for &j in bucket {
                    if i == j {
                        continue;
                    }
                    let other = &snapshot[j];
                    let sum_r = me.radius + other.radius;
                    let diff = Vec3::new(me.pos.x - other.pos.x, 0.0, me.pos.z - other.pos.z);
                    let dist = diff.length();
                    if dist < sum_r && dist > 0.01 {
                        let overlap = sum_r - dist;
                        push += (diff / dist) * overlap;
                    }
                }
            }
        }
        if push.length_squared() > 0.01 {
            pushes.push((me.entity, push));
        }
    }

    for (entity, push) in pushes.drain(..) {
        if let Ok((_, mut tf, _)) = units.get_mut(entity) {
            tf.translation += push * push_strength * dt;
            if let Some(ref hm) = heightmap {
                tf.translation.y = hm.sample(tf.translation.x, tf.translation.z);
            }
        }
    }
}

/// One snapshot row for `unit_separation_system`. Named so the neighbour
/// lookup reads `.pos` / `.radius` instead of tuple indices.
pub struct SeparationEntry {
    entity: Entity,
    pos: Vec3,
    radius: f32,
    mobile: bool,
    flying: bool,
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
/// Exceptions: flying units (kept at cruise altitude by
/// `movement_system`) and subterranean units (the Worm, which
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
/// Flying units skip — they already ride `cruise_alt` above the
/// heightmap and shouldn't pick up a slope from the terrain below.
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

/// Compute a path through the nav bucket matching the unit's `MaxSlope`,
/// falling back to straight-line if no nav set is loaded or the unit kind
/// is blocked-everywhere in its bucket.
/// Outcome of one pathing attempt.
enum PathOutcome {
    /// A route exists — follow these waypoints.
    Route(Vec<Vec3>),
    /// The goal is unreachable (upstream `pathingFailed`): refuse the
    /// order instead of walking the unit into the nearest wall and
    /// parking it there.
    Unreachable,
}

/// `None` means nothing was decided this frame — no nav grid yet — and
/// the caller should keep the order and retry.
fn compute_path(
    nav_set: Option<&NavGridSet>,
    unit_registry: &UnitRegistry,
    kind: UnitKind,
    from: Vec3,
    to: Vec3,
    heat: Option<&spring_pathfinding::HeatMap>,
) -> Option<PathOutcome> {
    let nav = nav_set?;
    if nav.buckets.is_empty() {
        return None;
    }
    let cap = unit_registry.max_slope_ratio(kind);
    let idx = nav.bucket_for(cap);
    let speed_map = &nav.buckets[idx].speed_map;
    let path = find_path_with_heat(speed_map, heat, [from.x, from.z], [to.x, to.z])?;
    if !path.reached_goal {
        return Some(PathOutcome::Unreachable);
    }
    Some(PathOutcome::Route(
        path.points
            .iter()
            .map(|p| Vec3::new(p[0], 0.0, p[1]))
            .collect(),
    ))
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
mod inertia_tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    fn moving_unit(speed: f32, accel: f32, brake: f32, waypoint: Vec3) -> (World, Entity) {
        let mut world = World::new();
        world.init_resource::<Time>();
        world.insert_resource(UnitRegistry::empty());
        let unit = world
            .spawn((
                UnitType(UnitKind::Bit),
                UnitStats {
                    radius: 12.0,
                    hit_radius: 20.0,
                    speed,
                    accel,
                    brake,
                    turn_rate: 100.0,
                    can_fly: false,
                    cruise_alt: 0.0,
                    no_chase_vtol: true,
                },
                // Facing +X so the heading gate (align=1) doesn't gate
                // translation away from the speed math under test.
                Transform::from_rotation(Quat::from_rotation_arc(-Vec3::Z, Vec3::X)),
                MoveTarget(waypoint),
                MovePath {
                    waypoints: vec![waypoint],
                    current: 0,
                },
                CurrentSpeed::default(),
            ))
            .id();
        (world, unit)
    }

    fn tick(world: &mut World) {
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_millis(33));
        world.run_system_once(movement_system).unwrap();
    }

    /// A unit under way ramps its longitudinal speed at the FBI
    /// acceleration instead of teleporting to full speed: frame 1
    /// covers `accel·dt²`, and after ~4 s of 30 Hz ticks it cruises
    /// at the full 90 elmo/s.
    #[test]
    fn unit_accelerates_from_standstill() {
        let (mut world, unit) = moving_unit(90.0, 27.0, 60.0, Vec3::new(2000.0, 0.0, 0.0));

        tick(&mut world);
        let cs = world.get::<CurrentSpeed>(unit).unwrap().0;
        assert!(
            (cs - 27.0 * 0.033).abs() < 0.05,
            "one frame of accel from rest: {cs}"
        );
        let x1 = world.get::<Transform>(unit).unwrap().translation.x;
        assert!(x1 > 0.0 && x1 < 1.0, "first frame crawls: x={x1}");

        for _ in 0..140 {
            tick(&mut world);
        }
        let cs = world.get::<CurrentSpeed>(unit).unwrap().0;
        assert!(
            (cs - 90.0).abs() < 1.0,
            "after 4.6 s the unit cruises at max: {cs}"
        );
    }

    /// Approaching the final waypoint, the stop-distance clamp
    /// (`v = √(2·a·d)`) brakes a fast unit instead of letting it
    /// overshoot: a 90 elmo/s unit 24 elmos out may plan at most
    /// √(2·60·24) ≈ 53.7 elmo/s this frame, the speed decays toward
    /// that limit, and the unit never lands past the waypoint.
    #[test]
    fn unit_brakes_for_the_final_waypoint() {
        let (mut world, unit) = moving_unit(90.0, 27.0, 60.0, Vec3::new(24.0, 0.0, 0.0));
        world.get_mut::<CurrentSpeed>(unit).unwrap().0 = 90.0;

        tick(&mut world);

        let cs = world.get::<CurrentSpeed>(unit).unwrap().0;
        assert!(
            cs < 90.0,
            "speed must brake toward the stop-distance limit: {cs}"
        );
        let x = world.get::<Transform>(unit).unwrap().translation.x;
        assert!(x <= 24.0 + 1e-3, "must not overshoot the waypoint: x={x}");

        // Keep ticking until arrival: the unit lands on the waypoint
        // at rest instead of skidding past it.
        for _ in 0..40 {
            tick(&mut world);
        }
        let cs = world.get::<CurrentSpeed>(unit).unwrap().0;
        assert_eq!(cs, 0.0, "arrival ends the leg at rest: {cs}");
    }

    /// An idle unit (no order) coasts down to rest at the brake rate,
    /// so a fresh order starts from a stopped state.
    #[test]
    fn idle_unit_coasts_to_rest() {
        let (mut world, unit) = moving_unit(90.0, 27.0, 60.0, Vec3::new(2000.0, 0.0, 0.0));
        world.get_mut::<CurrentSpeed>(unit).unwrap().0 = 90.0;
        world
            .entity_mut(unit)
            .remove::<MoveTarget>()
            .remove::<MovePath>();

        for _ in 0..60 {
            tick(&mut world);
        }
        let cs = world.get::<CurrentSpeed>(unit).unwrap().0;
        assert_eq!(
            cs, 0.0,
            "2 s of braking from 90 at 60 elmo/s² stops the unit"
        );
    }
}

#[cfg(test)]
mod heat_tests {
    use super::*;
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
                    cruise_alt: 0.0,
                    no_chase_vtol: true,
                },
                TeamId(0),
                GlobalTransform::from_xyz(32.0, 0.0, 8.0),
                MovePath {
                    waypoints: vec![Vec3::new(500.0, 0.0, 8.0)],
                    current: 0,
                },
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
                    cruise_alt: 40.0,
                    no_chase_vtol: false,
                },
                TeamId(0),
                GlobalTransform::from_xyz(40.0, 0.0, 8.0),
                MovePath {
                    waypoints: vec![Vec3::new(500.0, 0.0, 8.0)],
                    current: 0,
                },
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
                        cruise_alt: 0.0,
                        no_chase_vtol: true,
                    },
                    TeamId(0),
                    Transform::from_translation(start),
                    CurrentSpeed::default(),
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

use bevy::prelude::*;

use spring_pathfinding::{NodeLayer, find_path};

use super::selection::Selected;
use crate::units::combat::{DeployState, Deployable};
use crate::units::components::UnitType;
use crate::units::unit_registry::UnitRegistry;

/// When present, the unit will move toward this world position.
#[derive(Component)]
pub struct MoveTarget(pub Vec3);

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
}

impl QueuedCommand {
    pub fn position(&self) -> Vec3 {
        match self {
            QueuedCommand::Move(p) => *p,
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

/// The pathfinding grid resource, built from the loaded map.
#[derive(Resource)]
pub struct NavGrid(pub NodeLayer);

/// Per-frame snapshot of every unit, used by the movement pass to resolve
/// collisions and decide when a waypoint is blocked by an "arrived" unit.
struct UnitSnapshot {
    entity: Entity,
    pos: Vec3,
    radius: f32,
    /// Whether this unit kind is capable of moving (speed > 0).
    mobile: bool,
    /// Whether this specific unit has no active move order right now — i.e.
    /// it has reached its goal (or never had one). Used by the deadlock
    /// breaker to skip waypoints that a stationary unit is standing on.
    stationary: bool,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn movement_system(
    mut commands: Commands,
    time: Res<Time>,
    mut nav_grid: Option<ResMut<NavGrid>>,
    mut query: Query<(
        Entity,
        &UnitType,
        &mut Transform,
        Option<&MoveTarget>,
        Option<&mut MovePath>,
        Option<&mut CommandQueue>,
        Option<&Deployable>,
    )>,
    unit_registry: Res<UnitRegistry>,
) {
    // Snapshot every unit's position and collision radius so each proposed
    // movement can be resolved against all others without query aliasing.
    // `mobile` is "this unit kind *could* move", `stationary` is "this
    // specific unit has no active move order right now" — the deadlock
    // breaker uses the latter to decide whether a blocker counts as
    // "already at its goal".
    let snapshot: Vec<UnitSnapshot> = query
        .iter()
        .map(|(e, ut, tf, target, _, _, _)| UnitSnapshot {
            entity: e,
            pos: tf.translation,
            radius: unit_registry.collision_radius(ut.0),
            mobile: unit_registry.speed(ut.0) > 0.0,
            stationary: target.is_none(),
        })
        .collect();

    for (entity, unit_type, mut transform, move_target, move_path, mut queue, deployable) in
        &mut query
    {
        let speed = unit_registry.speed(unit_type.0);
        if speed == 0.0 {
            // Buildings can't move — remove any movement components.
            commands.entity(entity).remove::<MoveTarget>();
            commands.entity(entity).remove::<MovePath>();
            commands.entity(entity).remove::<CommandQueue>();
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

        // If we have a MoveTarget but no MovePath, compute the path.
        if let Some(target) = move_target
            && move_path.is_none()
        {
            let path = compute_path(nav_grid.as_deref_mut(), transform.translation, target.0);
            commands.entity(entity).insert(MovePath {
                waypoints: path,
                current: 0,
            });
        }

        // Follow the path waypoint by waypoint.
        let Some(mut path) = move_path else {
            continue;
        };

        if path.current >= path.waypoints.len() {
            // Path complete — promote the next queued command if any.
            commands.entity(entity).remove::<MovePath>();
            let next = queue.as_mut().and_then(|q| {
                if q.commands.is_empty() {
                    None
                } else {
                    Some(q.commands.remove(0))
                }
            });
            match next {
                Some(QueuedCommand::Move(pos)) => {
                    commands.entity(entity).insert(MoveTarget(pos));
                }
                None => {
                    commands.entity(entity).remove::<MoveTarget>();
                    commands.entity(entity).remove::<CommandQueue>();
                }
            }
            continue;
        }

        let current = transform.translation;
        let waypoint = path.waypoints[path.current];
        let goal = Vec3::new(waypoint.x, current.y, waypoint.z);
        let diff = goal - current;
        let distance = diff.length();

        let self_radius = unit_registry.collision_radius(unit_type.0);

        // Arrival is "within my own footprint of the waypoint" — this lets
        // crowds converging on the same target settle at the boundary of
        // their neighbours rather than jittering on top of it.
        //
        // Deadlock breaker: if the waypoint is occupied by a unit that has
        // already stopped (no move order of its own), also count it as
        // reached so we don't keep pushing through a crowd that's arrived.
        let arrival_threshold = (self_radius + 2.0).max(8.0);
        if distance < arrival_threshold
            || waypoint_blocked_by_arrived_unit(entity, goal, self_radius, &snapshot)
        {
            path.current += 1;
            continue;
        }

        let direction = diff / distance;
        let step = speed * time.delta_secs();
        let desired = direction * step.min(distance);

        // Resolve desired motion against every other unit. Spring-style:
        // units push each other with radial + lateral slide, weighted by
        // mass/speed/head-on factor. See `resolve_motion`.
        let resolved = resolve_motion(entity, current, desired, self_radius, speed, &snapshot);

        transform.translation += resolved;

        // Face the direction we actually moved (post-slide), falling back
        // to the intended direction if we barely moved so idle-jitter
        // doesn't flip the unit's facing around.
        let resolved_xz = Vec3::new(resolved.x, 0.0, resolved.z);
        if resolved_xz.length_squared() > 0.5 {
            transform.look_to(resolved_xz.normalize(), Vec3::Y);
        } else if desired.length_squared() > 0.0001 {
            transform.look_to(Vec3::new(direction.x, 0.0, direction.z), Vec3::Y);
        }
    }
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
fn resolve_motion(
    self_entity: Entity,
    origin: Vec3,
    desired: Vec3,
    self_radius: f32,
    self_speed: f32,
    snapshot: &[UnitSnapshot],
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

    for other in snapshot {
        if other.entity == self_entity {
            continue;
        }
        let sum_r = self_radius + other.radius;

        // Predict the contact at the *end* of the desired step. This is
        // what converts "two units walking toward each other through
        // empty space" into an actual collision response this frame.
        let new_origin = origin + desired_xz;
        let sep = Vec3::new(new_origin.x - other.pos.x, 0.0, new_origin.z - other.pos.z);
        let dist = sep.length();
        if dist >= sum_r {
            continue;
        }

        // Penetration depth once we take the step. Capped at sum_r so
        // a deep overlap (e.g. from a spawn on top of someone) still
        // produces a bounded correction.
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
    }

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
    snapshot: &[UnitSnapshot],
) -> bool {
    snapshot.iter().any(|other| {
        if other.entity == self_entity || !other.stationary {
            return false;
        }
        let r = self_radius + other.radius;
        let dx = other.pos.x - waypoint.x;
        let dz = other.pos.z - waypoint.z;
        dx * dx + dz * dz < r * r
    })
}

/// Safety-net that unsticks mobile units that *are already overlapping*,
/// which can happen on spawn, when a factory ejects onto a busy tile, or
/// when the terrain pushes a unit into a building. `movement_system` now
/// does the primary hard-collision work, so this only needs to correct
/// residual overlap with a gentle nudge — not drive the main separation.
pub fn unit_separation_system(
    mut units: Query<(Entity, &mut Transform, &UnitType)>,
    time: Res<Time>,
    unit_registry: Res<UnitRegistry>,
) {
    let dt = time.delta_secs();
    let push_strength = 30.0_f32;

    // Snapshot position, radius, mobility.
    let snapshot: Vec<(Entity, Vec3, f32, bool)> = units
        .iter()
        .map(|(e, tf, ut)| {
            (
                e,
                tf.translation,
                unit_registry.collision_radius(ut.0),
                unit_registry.speed(ut.0) > 0.0,
            )
        })
        .collect();

    let mut pushes: Vec<(Entity, Vec3)> = Vec::new();
    for i in 0..snapshot.len() {
        if !snapshot[i].3 {
            continue; // buildings don't get pushed
        }
        let mut push = Vec3::ZERO;
        for j in 0..snapshot.len() {
            if i == j {
                continue;
            }
            let sum_r = snapshot[i].2 + snapshot[j].2;
            let diff = Vec3::new(
                snapshot[i].1.x - snapshot[j].1.x,
                0.0,
                snapshot[i].1.z - snapshot[j].1.z,
            );
            let dist = diff.length();
            if dist < sum_r && dist > 0.01 {
                let overlap = sum_r - dist;
                push += (diff / dist) * overlap;
            }
        }
        if push.length_squared() > 0.01 {
            pushes.push((snapshot[i].0, push));
        }
    }

    for (entity, push) in pushes {
        if let Ok((_, mut tf, _)) = units.get_mut(entity) {
            tf.translation += push * push_strength * dt;
        }
    }
}

/// Compute a path using the QTPFS nav grid, or fall back to straight-line.
fn compute_path(nav_grid: Option<&mut NavGrid>, from: Vec3, to: Vec3) -> Vec<Vec3> {
    if let Some(nav) = nav_grid {
        let src = [from.x, from.z];
        let dst = [to.x, to.z];
        let path = find_path(&mut nav.0, src, dst);

        if !path.is_empty() {
            return path
                .points
                .iter()
                .map(|p| Vec3::new(p[0], 0.0, p[1]))
                .collect();
        }
    }

    // Fallback: straight-line.
    vec![to]
}

/// Draw straight lines from each selected unit to its current move target,
/// then through any queued move/attack-move destinations.
#[allow(clippy::type_complexity)]
pub fn draw_selected_command_lines(
    mut gizmos: Gizmos,
    query: Query<(&Transform, Option<&MoveTarget>, Option<&CommandQueue>), With<Selected>>,
) {
    let y = 2.0;
    let move_color = Color::srgb(0.2, 1.0, 0.3);

    for (transform, target, queue) in &query {
        let Some(current) = target else {
            continue;
        };
        let mut prev = Vec3::new(transform.translation.x, y, transform.translation.z);
        let next = Vec3::new(current.0.x, y, current.0.z);
        gizmos.line(prev, next, move_color);
        prev = next;

        if let Some(queue) = queue {
            for cmd in &queue.commands {
                let pos = cmd.position();
                let to = Vec3::new(pos.x, y, pos.z);
                let color = match cmd {
                    QueuedCommand::Move(_) => move_color,
                };
                gizmos.line(prev, to, color);
                prev = to;
            }
        }
    }
}

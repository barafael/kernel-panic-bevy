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
    // movement can be clamped against all others without query aliasing.
    let snapshot: Vec<(Entity, Vec3, f32, bool)> = query
        .iter()
        .map(|(e, ut, tf, _, _, _, _)| {
            let radius = unit_registry.collision_radius(ut.0);
            let mobile = unit_registry.speed(ut.0) > 0.0;
            (e, tf.translation, radius, mobile)
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
        let arrival_threshold = (self_radius + 2.0).max(8.0);
        if distance < arrival_threshold {
            path.current += 1;
            continue;
        }

        let direction = diff / distance;
        let step = speed * time.delta_secs();
        let desired = direction * step.min(distance);

        // Resolve desired motion against every other unit: if the new XZ
        // position would overlap another unit's circle, cap the motion at
        // contact. This gives a hard stop rather than passing through. A
        // single pass is sufficient because later frames keep retrying.
        let resolved = resolve_motion(entity, current, desired, self_radius, &snapshot);

        transform.translation += resolved;

        // Only update facing when we actually moved, so a blocked unit
        // doesn't snap to NaN or a stale direction.
        if resolved.length_squared() > 0.0001 {
            transform.look_to(Vec3::new(direction.x, 0.0, direction.z), Vec3::Y);
        }
    }
}

/// Cap `desired` motion so the unit at `origin` with `radius` does not
/// penetrate any other unit in `snapshot`. Only the XZ plane is considered.
///
/// Strategy: for each other unit, advance `t ∈ [0, 1]` along `desired` until
/// the distance between circles would reach `r_self + r_other`, and keep the
/// smallest `t` across all obstacles. Reaching `t = 0` means we're already
/// at the contact distance and cannot move toward that obstacle — we allow
/// any residual motion that is perpendicular / away from it.
fn resolve_motion(
    self_entity: Entity,
    origin: Vec3,
    desired: Vec3,
    self_radius: f32,
    snapshot: &[(Entity, Vec3, f32, bool)],
) -> Vec3 {
    let desired_xz = Vec3::new(desired.x, 0.0, desired.z);
    if desired_xz.length_squared() < 1e-6 {
        return desired;
    }

    let mut t_max: f32 = 1.0;
    for (other, other_pos, other_radius, _) in snapshot {
        if *other == self_entity {
            continue;
        }
        let sum_r = self_radius + other_radius;
        let to_other = Vec3::new(other_pos.x - origin.x, 0.0, other_pos.z - origin.z);
        let dist_sq = to_other.length_squared();

        // Already overlapping — don't let this obstacle further restrict us,
        // just prevent motion *into* it. If desired points away from the
        // obstacle, it's fine; if it points toward, cap at t=0.
        if dist_sq < sum_r * sum_r {
            if desired_xz.dot(to_other) > 0.0 {
                t_max = 0.0;
            }
            continue;
        }

        // Solve |origin + t*desired - other_pos| = sum_r for smallest t ≥ 0.
        // Quadratic: a*t^2 + b*t + c = 0 where
        //   a = desired·desired, b = -2 * desired·to_other, c = |to_other|^2 - sum_r^2.
        let a = desired_xz.dot(desired_xz);
        let b = -2.0 * desired_xz.dot(to_other);
        let c = dist_sq - sum_r * sum_r;
        let disc = b * b - 4.0 * a * c;
        if disc <= 0.0 {
            continue; // no intersection along the ray
        }
        let sqrt_disc = disc.sqrt();
        let t = (-b - sqrt_disc) / (2.0 * a);
        if t >= 0.0 && t < t_max {
            t_max = t;
        }
    }

    // Leave a tiny gap so floating-point drift doesn't bury us inside
    // the obstacle on the next frame.
    let safe = (t_max - 1e-3).max(0.0);
    desired * safe
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

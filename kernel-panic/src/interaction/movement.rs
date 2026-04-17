use bevy::prelude::*;

use spring_pathfinding::{NodeLayer, find_path};

use super::selection::Selected;
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{DeployState, Deployable};
use crate::units::components::UnitType;
use crate::units::definitions::UnitKind;
use crate::units::unit_registry::UnitRegistry;

/// Rate at which the pitch/roll component of a unit's rotation relaxes
/// toward the slope-aligned target. Higher = snappier tilt, lower = more
/// sluggish. Yaw is set directly (unaffected by this constant).
const TILT_SMOOTH_RATE: f32 = 8.0;

/// When present, the unit will move toward this world position.
#[derive(Component)]
pub struct MoveTarget(pub Vec3);

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
    /// Walk to `site`, then erect a building of `kind` there. Used by
    /// constructor units (Assembler / Trojan / Gateway) after the player
    /// picks a building in the build menu and clicks on a datavent.
    BuildAt {
        kind: UnitKind,
        site: Vec3,
    },
}

impl QueuedCommand {
    pub fn position(&self) -> Vec3 {
        match self {
            QueuedCommand::Move(p) => *p,
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
    heightmap: Option<Res<Heightmap>>,
    mut query: Query<(
        Entity,
        &UnitType,
        &mut Transform,
        Option<&MoveTarget>,
        Option<&mut MovePath>,
        Option<&mut CommandQueue>,
        Option<&Deployable>,
        Option<&mut SlopeTilt>,
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
        .map(|(e, ut, tf, target, _, _, _, _)| UnitSnapshot {
            entity: e,
            pos: tf.translation,
            radius: unit_registry.collision_radius(ut.0),
            mobile: unit_registry.speed(ut.0) > 0.0,
            stationary: target.is_none(),
        })
        .collect();

    for (
        entity,
        unit_type,
        mut transform,
        move_target,
        move_path,
        mut queue,
        deployable,
        mut slope_tilt,
    ) in &mut query
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
                    commands
                        .entity(entity)
                        .insert(MoveTarget(pos))
                        .remove::<crate::units::construction::PendingBuild>();
                }
                Some(QueuedCommand::BuildAt { kind, site }) => {
                    commands
                        .entity(entity)
                        .insert(MoveTarget(site))
                        .insert(crate::units::construction::PendingBuild { kind, site });
                }
                None => {
                    commands.entity(entity).remove::<MoveTarget>();
                    commands.entity(entity).remove::<CommandQueue>();
                    commands
                        .entity(entity)
                        .remove::<crate::units::construction::PendingBuild>();
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

        let turn_rate = unit_registry.turn_rate(unit_type.0);
        let max_turn = if turn_rate > 0.0 {
            turn_rate * dt
        } else {
            // TurnRate=0 means "no rotation delay in the FBI" — snap.
            std::f32::consts::TAU
        };
        let new_forward = rotate_toward_xz(current_xz, desired_forward, max_turn);

        // Facing-gated forward speed. A unit that's pointed at its goal
        // runs full speed; one pivoting toward it still creeps forward so
        // it isn't frozen in place — the previous zero-floor meant slow-
        // turning units went completely static for long heading changes
        // (and never arrived at waypoints behind them). Minimum gate is
        // 30% of full speed; anything above that scales with cos(err).
        const PIVOT_SPEED_FLOOR: f32 = 0.3;
        let align = new_forward
            .dot(desired_forward)
            .clamp(0.0, 1.0)
            .max(PIVOT_SPEED_FLOOR);
        let step = speed * dt * align;
        if step < 1e-4 {
            continue;
        }
        let desired = new_forward * step.min(distance);

        // Resolve desired motion against every other unit. Spring-style:
        // units push each other with radial + lateral slide, weighted by
        // mass/speed/head-on factor. See `resolve_motion`.
        let resolved = resolve_motion(entity, current, desired, self_radius, speed, &snapshot);

        transform.translation += resolved;

        // Ride the terrain. The step itself is planar, so without this the
        // unit's Y would be frozen at its spawn height and it'd walk into
        // hills as the ground rises beneath it.
        if let Some(ref hm) = heightmap {
            transform.translation.y = hm.sample(transform.translation.x, transform.translation.z);
        }

        // Yaw set fresh from new_forward. Tilt computed in body-space as
        // pure pitch (rotation about body-right) + roll (about body-forward),
        // then smoothed in a dedicated component. Keeping tilt purely
        // pitch+roll means it can't bleed into the next frame's yaw read
        // of transform.forward().
        if new_forward.length_squared() > 1e-6 {
            let yaw_only = Transform::default()
                .looking_to(new_forward, Vec3::Y)
                .rotation;
            let target_tilt = match heightmap.as_deref() {
                Some(hm) => {
                    let normal = hm.normal(transform.translation.x, transform.translation.z);
                    // Body axes: forward = new_forward (XZ), right = right-hand
                    // perpendicular in XZ, up = world Y before tilt.
                    let body_right = Vec3::new(new_forward.z, 0.0, -new_forward.x);
                    // Slope angles. `pitch` = how much the ground rises ahead
                    // (positive = nose up). `roll` = how much it rises to the
                    // right (positive = right side up). `normal.y` is always
                    // positive on valid terrain; we divide by it to get tan.
                    let pitch = (-new_forward.dot(normal) / normal.y.max(1e-4)).atan();
                    let roll = (-body_right.dot(normal) / normal.y.max(1e-4)).atan();
                    // Pitch around body-right (local X), roll around body-
                    // forward (local -Z in Bevy). Composing them in body
                    // space keeps yaw exactly zero.
                    Quat::from_axis_angle(Vec3::X, pitch) * Quat::from_axis_angle(Vec3::Z, roll)
                }
                None => Quat::IDENTITY,
            };
            let blend = 1.0 - (-TILT_SMOOTH_RATE * dt).exp();
            let smoothed_tilt = match slope_tilt.as_deref_mut() {
                Some(t) => {
                    t.0 = t.0.slerp(target_tilt, blend);
                    t.0
                }
                None => {
                    let t = Quat::IDENTITY.slerp(target_tilt, blend);
                    commands.entity(entity).insert(SlopeTilt(t));
                    t
                }
            };
            transform.rotation = yaw_only * smoothed_tilt;
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
    heightmap: Option<Res<Heightmap>>,
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
            if let Some(ref hm) = heightmap {
                tf.translation.y = hm.sample(tf.translation.x, tf.translation.z);
            }
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
    let build_color = Color::srgb(1.0, 0.8, 0.2);

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
                    QueuedCommand::BuildAt { .. } => build_color,
                };
                gizmos.line(prev, to, color);
                prev = to;
            }
        }
    }
}

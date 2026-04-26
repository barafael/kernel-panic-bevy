use std::collections::HashMap;

use bevy::prelude::*;

use spring_pathfinding::{NodeLayer, find_path, slope_from_rise_run};

use super::selection::Selected;
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{DeployState, Deployable, Dying};
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
    /// Currently has no constructor caller — the placement UI that
    /// produced these was removed pending a rewrite.
    #[allow(dead_code)]
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

/// One pathfinding grid plus the max-slope cap (dy/dx ratio) it was
/// built for. Grids with smaller caps flag more cells as impassable.
pub struct NavBucket {
    pub max_slope: f32,
    pub layer: NodeLayer,
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
pub fn movement_system(
    mut commands: Commands,
    time: Res<Time>,
    mut nav_set: Option<ResMut<NavGridSet>>,
    heightmap: Option<Res<Heightmap>>,
    mut query: Query<
        (
            Entity,
            &UnitType,
            &UnitStats,
            &mut Transform,
            Option<&MoveTarget>,
            Option<&mut MovePath>,
            Option<&mut CommandQueue>,
            Option<&Deployable>,
            Option<&mut SlopeTilt>,
            Option<&crate::units::combat::Stunned>,
            Option<&crate::units::mechanics::network_buffer::SpeedBoost>,
        ),
        Without<Dying>,
    >,
    unit_registry: Res<UnitRegistry>,
    // Reused across frames so the full-unit snapshot doesn't reallocate
    // each tick. Dropped in favor of a spatial-hash neighborhood query
    // eventually, but the allocation hoist is a free win until then.
    mut snapshot: Local<Vec<UnitSnapshot>>,
) {
    // Snapshot every unit's position and collision radius so each proposed
    // movement can be resolved against all others without query aliasing.
    // `mobile` is "this unit kind *could* move", `stationary` is "this
    // specific unit has no active move order right now" — the deadlock
    // breaker uses the latter to decide whether a blocker counts as
    // "already at its goal".
    snapshot.clear();
    snapshot.extend(
        query
            .iter()
            .map(|(e, _, stats, tf, target, _, _, _, _, _, _)| UnitSnapshot {
                entity: e,
                pos: tf.translation,
                radius: stats.radius,
                mobile: stats.speed > 0.0,
                flying: stats.can_fly,
                stationary: target.is_none(),
            }),
    );

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
    ) in &mut query
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

        // If we have a MoveTarget but no MovePath, compute the path.
        // Flying units skip the nav grid entirely and take a straight XZ
        // line to the target — they can cross any terrain, so routing
        // around cliffs would only add noise. Ground pathfinds cost
        // real CPU, so cap how many we do per frame — surplus units
        // just wait one extra frame for their turn.
        if let Some(target) = move_target
            && move_path.is_none()
        {
            let path = if flying {
                Some(vec![Vec3::new(target.0.x, 0.0, target.0.z)])
            } else if pathfinds_used < PATHFIND_BUDGET_PER_FRAME {
                pathfinds_used += 1;
                Some(compute_path(
                    nav_set.as_deref_mut(),
                    &unit_registry,
                    unit_type.0,
                    transform.translation,
                    target.0,
                ))
            } else {
                None
            };
            if let Some(path) = path {
                commands.entity(entity).insert(MovePath {
                    waypoints: path,
                    current: 0,
                });
            }
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
                        .remove::<crate::units::lifecycle::construction::PendingBuild>();
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
                        .remove::<crate::units::lifecycle::construction::PendingBuild>();
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

        // Facing-gated forward speed. Within ~60° of the target heading,
        // drive at cos(err); beyond that, pivot in place (no translation)
        // so the unit doesn't arc wide during sharp turns.
        let cos_err = new_forward.dot(desired_forward);
        let align = if cos_err > 0.5 { cos_err } else { 0.0 };
        let step = speed * dt * align;
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
            resolve_motion(entity, current, desired, self_radius, speed, &snapshot)
        };

        // Slope gate: signed, so descents always pass — a unit can
        // step off a ledge it can't climb back up. The 20% margin
        // absorbs the mismatch between the pathfinder's per-cell
        // slope and movement's bilinear per-step sample. If the
        // step is refused, advance the waypoint so a straight-line
        // fallback exhausts cleanly instead of stalling.
        if !flying && let Some(ref hm) = heightmap {
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
        if other.entity == self_entity || other.flying {
            // Skip self and any airborne unit: the caller only invokes
            // this for ground units (fliers bypass collision entirely),
            // and a flier overhead shouldn't obstruct a walker below.
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
        // build a yaw-only rotation from it. Stationary buildings start
        // at yaw=0 (facing -Z); mobile units keep whichever yaw they
        // finished their last move order with.
        let forward = transform.forward().as_vec3();
        let forward_xz = {
            let f = Vec3::new(forward.x, 0.0, forward.z);
            if f.length_squared() < 1e-6 {
                -Vec3::Z
            } else {
                f.normalize()
            }
        };
        let yaw_only = Transform::IDENTITY.looking_to(forward_xz, Vec3::Y).rotation;

        let body_right = Vec3::new(forward_xz.z, 0.0, -forward_xz.x);
        let pitch = (-forward_xz.dot(normal) / normal.y.max(1e-4)).atan();
        let roll = (-body_right.dot(normal) / normal.y.max(1e-4)).atan();
        let target_tilt =
            Quat::from_axis_angle(Vec3::X, pitch) * Quat::from_axis_angle(Vec3::Z, roll);

        match slope_tilt.as_deref_mut() {
            Some(t) => {
                t.0 = target_tilt;
            }
            None => {
                commands.entity(entity).insert(SlopeTilt(target_tilt));
            }
        }
        transform.rotation = yaw_only * target_tilt;
    }
}

/// Compute a path through the nav bucket matching the unit's `MaxSlope`,
/// falling back to straight-line if no nav set is loaded or the unit kind
/// is blocked-everywhere in its bucket.
fn compute_path(
    nav_set: Option<&mut NavGridSet>,
    unit_registry: &UnitRegistry,
    kind: UnitKind,
    from: Vec3,
    to: Vec3,
) -> Vec<Vec3> {
    if let Some(nav) = nav_set
        && !nav.buckets.is_empty()
    {
        let cap = unit_registry.max_slope_ratio(kind);
        let idx = nav.bucket_for(cap);
        let layer = &mut nav.buckets[idx].layer;
        let path = find_path(layer, [from.x, from.z], [to.x, to.z]);
        if !path.is_empty() {
            // Why: a partial path (unreachable destination) ends at
            // the closest reachable node — same as upstream
            // `GroundMoveType::CanSetNextWayPoint`'s `pathingFailed`.
            return path
                .points
                .iter()
                .map(|p| Vec3::new(p[0], 0.0, p[1]))
                .collect();
        }
    }
    // Fallback: no nav grid yet, or flying.
    vec![to]
}

/// Dash-pattern segment lengths (long dash, gap, short dot, gap), in elmos.
/// Drawn back-to-back they form a repeating `-.-.` run.
const DASH_PATTERN: [(f32, bool); 4] = [(16.0, true), (6.0, false), (4.0, true), (6.0, false)];

fn sample_at_ground(x: f32, z: f32, heightmap: Option<&Heightmap>) -> Vec3 {
    let y = heightmap.map(|h| h.sample(x, z)).unwrap_or(0.0);
    Vec3::new(x, y + GIZMO_LIFT, z)
}

/// Draw the path each selected unit is walking (actual waypoint polyline,
/// hugging the terrain) plus a disc marker at each pending destination —
/// current `MoveTarget` and every `QueuedCommand`. The line follows the
/// computed `MovePath` when one exists, falling back to unit→target when
/// pathfinding hasn't run yet. Mimics Spring's green move-order overlay.
#[allow(clippy::type_complexity)]
pub fn draw_selected_command_lines(
    mut gizmos: Gizmos<CommandLineGizmos>,
    query: Query<
        (
            &Transform,
            Option<&MoveTarget>,
            Option<&MovePath>,
            Option<&CommandQueue>,
        ),
        With<Selected>,
    >,
    heightmap: Option<Res<Heightmap>>,
) {
    const MOVE_COLOR: Color = Color::srgb(0.2, 1.0, 0.3);
    const BUILD_COLOR: Color = Color::srgb(1.0, 0.8, 0.2);
    const DISC_RADIUS: f32 = 6.0;

    let hm = heightmap.as_deref();

    for (transform, target, path, queue) in &query {
        let Some(current) = target else {
            continue;
        };

        // Collect the sequence of polyline vertices: unit → remaining
        // waypoints. If no path exists yet (freshly-issued order), fall
        // back to unit → current target so the player sees something.
        let mut points: Vec<Vec3> = vec![sample_at_ground(
            transform.translation.x,
            transform.translation.z,
            hm,
        )];
        if let Some(path) = path
            && path.current < path.waypoints.len()
        {
            for wp in &path.waypoints[path.current..] {
                points.push(sample_at_ground(wp.x, wp.z, hm));
            }
        } else {
            points.push(sample_at_ground(current.0.x, current.0.z, hm));
        }

        draw_dashed_polyline(&mut gizmos, &points, MOVE_COLOR, hm);

        // Ring at the final point of the active order.
        let end = *points.last().unwrap();
        gizmos.circle(
            Isometry3d::new(end, Quat::from_rotation_arc(Vec3::Z, Vec3::Y)),
            DISC_RADIUS,
            MOVE_COLOR,
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
mod tests {
    use super::*;
    use spring_pathfinding::SpeedMap;

    fn bucket_with(cap: f32) -> NavBucket {
        // Tiny 2×2 speed map — we only care about the `max_slope` value
        // for selection tests, not the grid contents.
        let layer = NodeLayer::new(&SpeedMap::uniform(2, 2, 1.0));
        NavBucket {
            max_slope: cap,
            layer,
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

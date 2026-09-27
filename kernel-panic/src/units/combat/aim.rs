//! Deploy cycle + host-driven aim for armed units.
//!
//! Two concerns:
//! - **Deploy state machine** — [`Deployable`] / [`DeployState`] track the
//!   pack/unpack cycle for units that must unfold before firing (Pointer).
//!   [`tick_deploy_state`] flips between states in response to movement,
//!   firing the unit's `Open` / `Close` COB scripts so the visible model
//!   stays in sync with the logical state.
//! - **Aim** — [`aim_weapons_system`] points each unit's weapon at its
//!   [`AimTarget`]. The path branches on whether the unit's COB script
//!   declares an `aimer` piece: see the function docs for the upstream
//!   conventions each branch reproduces.

use bevy::prelude::*;

use super::Dying;
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::assets::animation::{AnimCtx, UnitAnimator};
use crate::units::components::UnitStats;

/// Deploy cycle for units that must unfold before firing (e.g. Pointer).
/// The COB script animates the legs/gun; this component gates combat so
/// the unit can only fire while `Open`, matching upstream Kernel Panic.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeployState {
    Closed,
    Opening,
    Open,
    Closing,
}

/// Attached to units with a deploy cycle. `timer` counts down through
/// transition states; the duration is the animation length in seconds.
#[derive(Component)]
pub struct Deployable {
    pub state: DeployState,
    pub timer: f32,
}

/// Stamped by `combat_system` each frame an armed unit has picked a target
/// it wants to fire at. Read by `aim_weapons_system` to rotate the body /
/// tilt the gun before combat actually commits the shot. Removed in
/// frames where the unit has no viable target so aim systems don't keep
/// steering toward a stale position.
#[derive(Component, Clone, Copy, Debug)]
pub struct AimTarget {
    pub pos: Vec3,
    /// Arc height for ballistic weapons (passed through from the
    /// WeaponDef so the gun elevates for the lob, not the direct line).
    pub arc_height: f32,
}

/// Max heading error (radians) at which a Deployable is allowed to fire.
/// ~5° — tight enough that the gun is visibly pointed at the target, loose
/// enough that the Pointer doesn't get stuck oscillating.
pub const AIM_HEADING_TOLERANCE: f32 = 0.09;

/// Max gunbase / aimer pitch error (radians) at which a unit is allowed
/// to fire. Same ~5° tolerance as heading — tight enough that the barrel
/// is visibly elevated correctly, loose enough that the Pointer can fire
/// at near-equal-altitude targets without the pitch slew lagging by half
/// a frame and gating every shot.
pub const AIM_PITCH_TOLERANCE: f32 = 0.09;

/// Open/Close animation length in seconds, matching the upstream COB
/// script timings (legs move over 0.5s, gun extends over another 1.0s).
pub const DEPLOY_DURATION: f32 = 1.5;

/// Host-side mirror of the byte's fold state, read by the damage
/// pipeline for the upstream 30 % closed-state damage reduction
/// (`byte.bos HitByWeaponId`). Written by [`sync_byte_fold_state`]
/// from the byte's animation driver — the single source of truth for
/// whether the byte is currently unfolded.
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct ByteOpen;

/// Spawn-time marker for the Byte kind, so the per-frame fold mirroring
/// only iterates Bytes instead of every animator in the world.
#[derive(Component)]
pub struct Byte;

/// Mirror the byte driver's fold state into the [`ByteOpen`] marker the
/// damage pipeline reads. The driver is the single source of truth: the
/// marker is present exactly while the fold state machine reports the
/// byte fully unfolded, so a closed (or mid-fold) byte keeps its armor.
pub fn sync_byte_fold_state(
    mut query: Query<(Entity, &UnitAnimator), With<Byte>>,
    open: Query<&ByteOpen>,
    mut commands: Commands,
) {
    for (entity, animator) in &mut query {
        let is_open = animator.driver.is_open();
        let currently = open.get(entity).is_ok();
        match is_open {
            Some(true) if !currently => {
                commands.entity(entity).insert(ByteOpen);
            }
            Some(false) | None if currently => {
                commands.entity(entity).remove::<ByteOpen>();
            }
            _ => {}
        }
    }
}

/// Aim-before-fire gate. Inserted at spawn on every unit whose
/// `.cob` declares `AimWeapon1`. `combat_system` blocks firing while
/// `ready == false`; `drive_aim_script` flips `ready` from the unit's
/// animation driver each frame.
#[derive(Component, Clone, Copy, Debug, Default)]
#[component(storage = "SparseSet")]
pub struct AimScript {
    pub ready: bool,
    pub last_heading_rad: f32,
    pub last_pitch_rad: f32,
}

/// Heading or pitch shift (radians) at which `drive_aim_script`
/// abandons a still-running aim cycle and re-drives it. ~11° — small
/// enough that a Packet dispatching behind a shooter forces a fresh
/// cycle, large enough that drifting targets don't churn.
pub const AIM_SCRIPT_RETARGET_THRESHOLD: f32 = 0.2;

/// `AimWeapon1(h, p)` arguments for a weapon aiming along `to_target`
/// from a unit oriented by `unit_rot` — upstream
/// `CWeapon::CallAimingScript` (`Weapon.cpp:410-421`): the wanted
/// direction is projected onto the unit's own `rightdir` / `updir` /
/// `frontdir`, so heading **and** pitch are relative to the body's full
/// orientation (a banking or pitching flyer, a unit on a slope), then
/// `h = -GetHeadingFromVector(localX, localZ)`, `p = asin(localY)`.
///
/// In this port a unit's front is local −Z (`Transform::looking_to`)
/// and the model root carries a 180° yaw, so a piece turned by `h`
/// around its Y axis points its S3O +Z at `atan2(−right, front)`:
/// `h = 0` is dead ahead whatever way the body faces. (Using the body's
/// Bevy yaw here — which is `heading + π` for a `looking_to` rotation —
/// turned every aiming piece 180° away from its target.)
///
/// `arc_height` adds the ballistic elevation bias the Pointer's gun
/// uses for its lob (`(4·h/d).atan()`), measured in the unit frame.
pub fn local_aim_angles(unit_rot: Quat, to_target: Vec3, arc_height: f32) -> (f32, f32) {
    let local = unit_rot.inverse() * to_target;
    let (right, up, front) = (local.x, local.y, -local.z);
    let heading = (-right).atan2(front);
    let horizontal = (right * right + front * front).sqrt();
    let direct_pitch = up.atan2(horizontal.max(1e-6));
    let arc_pitch = if arc_height > 0.0 && horizontal > 1.0 {
        (4.0 * arc_height / horizontal).atan()
    } else {
        0.0
    };
    (heading, direct_pitch + arc_pitch)
}

/// Advance every unit's aim cycle. Call the unit's animation driver
/// each frame with the current heading/pitch (its `AimWeapon1`
/// equivalent); the driver turns the relevant pieces and returns
/// whether the weapon may commit, which becomes [`AimScript::ready`].
/// Re-aims when the target shifts past
/// [`AIM_SCRIPT_RETARGET_THRESHOLD`].
#[allow(clippy::type_complexity)]
pub fn drive_aim_script(
    mut query: Query<
        (
            &mut AimScript,
            &mut UnitAnimator,
            &GlobalTransform,
            &AimTarget,
            Option<&MoveTarget>,
            Option<&MovePath>,
            Option<&Deployable>,
        ),
        Without<Dying>,
    >,
) {
    for (mut aim, mut animator, gtf, target, move_target, move_path, deployable) in &mut query {
        let (rel_heading, pitch_rad) =
            local_aim_angles(gtf.rotation(), target.pos - gtf.translation(), target.arc_height);

        let dh = (rel_heading - aim.last_heading_rad).abs();
        let dp = (pitch_rad - aim.last_pitch_rad).abs();
        let retarget = dh > AIM_SCRIPT_RETARGET_THRESHOLD || dp > AIM_SCRIPT_RETARGET_THRESHOLD;

        // The deploy state is what a Deployable's `AimWeapon1` checks
        // (pointer.bos `if (isOpen)`): without it the Pointer's aim
        // never reported ready.
        let ctx = AnimCtx {
            moving: move_target.is_some() || move_path.is_some(),
            aim_active: true,
            deploy: deployable.map(|d| d.state),
            ..AnimCtx::minimal()
        };
        let UnitAnimator { rig, driver, .. } = &mut *animator;
        aim.ready = driver.aim(rig, rel_heading, pitch_rad, ctx);
        if retarget || !aim.ready {
            aim.last_heading_rad = rel_heading;
            aim.last_pitch_rad = pitch_rad;
        }
    }
}

impl Deployable {
    /// Freshly-spawned deployable units start stowed (`Closed`). The
    /// `tick_deploy_state` system promotes them to `Opening` as soon as
    /// they're idle (i.e. have no move order), which triggers the COB
    /// `Open()` animation.
    pub fn initial() -> Self {
        Self {
            state: DeployState::Closed,
            timer: 0.0,
        }
    }
}

/// Drive the deploy state machine from movement state. Stopping
/// schedules `Open`; starting to move schedules `Close`. The visible
/// open/close choreography is the animation driver's job — it watches
/// the deploy state through its [`AnimCtx::deploy`] each frame.
///
/// [`AnimCtx::deploy`]: crate::units::assets::animation::AnimCtx::deploy
#[allow(clippy::type_complexity)]
pub fn tick_deploy_state(
    time: Res<Time>,
    mut query: Query<
        (
            &mut Deployable,
            Option<&MoveTarget>,
            Option<&MovePath>,
            Has<crate::interaction::movement::AttackMoveActive>,
            Has<AimTarget>,
        ),
        Without<Dying>,
    >,
) {
    let dt = time.delta_secs();
    for (mut deployable, move_target, move_path, attack_move, aiming) in &mut query {
        // A fight order pauses the move while there is something to
        // shoot (`CMobileCAI::ExecuteFight` stops the unit, which calls
        // the script's `StopMoving`): the Pointer opens and fires, then
        // resumes. `movement_system` holds the unit on the same
        // condition.
        let held = attack_move && aiming;
        let is_moving = (move_target.is_some() || move_path.is_some()) && !held;

        // Steady-state fast path: if no transition is in flight and the
        // deploy state already matches the movement state, there is
        // nothing to update. Skips the bulk of branch work in the
        // common case (a Pointer parked on a hill, every frame, for the
        // whole game).
        if deployable.timer == 0.0
            && matches!(
                (deployable.state, is_moving),
                (DeployState::Open, false) | (DeployState::Closed, true)
            )
        {
            continue;
        }

        if deployable.timer > 0.0 {
            deployable.timer = (deployable.timer - dt).max(0.0);
            if deployable.timer == 0.0 {
                deployable.state = match deployable.state {
                    DeployState::Opening => DeployState::Open,
                    DeployState::Closing => DeployState::Closed,
                    other => other,
                };
            }
        }

        match (deployable.state, is_moving) {
            (DeployState::Open, true) | (DeployState::Opening, true) => {
                deployable.state = DeployState::Closing;
                deployable.timer = DEPLOY_DURATION;
            }
            (DeployState::Closed, false) | (DeployState::Closing, false) => {
                deployable.state = DeployState::Opening;
                deployable.timer = DEPLOY_DURATION;
            }
            _ => {}
        }
    }
}

/// Steer armed units to face their current `AimTarget`, and tilt their
/// `gunbase` / rotate their `aimer` piece for the required heading + pitch.
///
/// Two distinct flavours of aim, picked per-unit by which piece markers
/// the spawn step attached:
///
/// - **Body-rotated aim** — units without an [`AimerPiece`] (Pointer, Bit,
///   etc.) turn the entire body to face the target via `look_to`. This is
///   the "non-upstream" path: it's a stand-in because we don't run the
///   .bos `AimWeapon1` script eagerly enough for those units' aim loops to
///   produce visible turret rotation in time.
/// - **Aimer-piece aim** — units with an [`AimerPiece`] (currently just
///   Byte's octahedron; WormOLD's turret if/when that ships) leave the
///   body alone and rotate only the aimer piece. Mirrors `byte.bos`'s
///   `AimWeapon1(h,p)`: `turn aimer to y-axis h speed <270>` followed
///   by `turn aimer to x-axis (<-90>-p) speed <270>`, where `h` is the
///   **absolute world heading**. Upstream's byte never turns its body
///   for aim — only the aimer-rooted firing assembly does.
///
/// Units currently moving (have a `MoveTarget`) are excluded — the
/// movement system owns their heading, and fighting movement for
/// rotation control makes Bits spin around mid-stride every frame.
/// Stand-in for Spring's `set HEADING` engine call: rotates a unit's
/// body around its world Y axis to face the current `AimTarget`.
///
/// Per-piece aim (gunbase / aimer / etc.) is left to the COB
/// `AimWeapon1` script — `drive_aim_script` runs it every frame and
/// the VM emits the corresponding `Turn` commands, so there's no
/// duplicate host-side rotation logic here.
///
/// Skipped for units carrying an `AimerPiece` (Byte's octaeder, etc.):
/// upstream's byte never rotates its body for aim — the aimer-rooted
/// firing assembly carries the heading on its own.
///
/// Units currently moving (have a `MoveTarget`) are excluded — the
/// movement system owns their heading.
#[allow(clippy::type_complexity)]
pub fn aim_weapons_system(
    time: Res<Time>,
    mut query: Query<
        (
            &mut Transform,
            &UnitStats,
            &AimTarget,
            Option<&crate::units::assets::animation::AimerPiece>,
        ),
        Without<crate::interaction::movement::MoveTarget>,
    >,
) {
    let dt = time.delta_secs();
    for (mut transform, stats, aim, aimer) in &mut query {
        // HoverAttack aircraft (Flow) never turn their body for an
        // auto-acquired target: `HoverAirMoveType` owns the heading (it
        // only faces `circlingPos` under an explicit attack order), and
        // flow.bos swings the `base` piece onto the target instead.
        // Turning the body here also fought `hover_air_system`, which
        // rewrites the attitude every tick.
        if aimer.is_some() || stats.can_fly {
            continue;
        }
        let to_target = Vec3::new(
            aim.pos.x - transform.translation.x,
            0.0,
            aim.pos.z - transform.translation.z,
        );
        let horizontal_dist = to_target.length();
        if horizontal_dist < 1e-4 {
            continue;
        }
        let desired_forward = to_target / horizontal_dist;

        let forward_vec = transform.forward().as_vec3();
        let current_xz = {
            let f = Vec3::new(forward_vec.x, 0.0, forward_vec.z);
            if f.length_squared() < 1e-6 {
                Vec3::Z
            } else {
                f.normalize()
            }
        };
        let max_turn = if stats.turn_rate > 0.0 {
            stats.turn_rate * dt
        } else {
            std::f32::consts::TAU
        };
        let new_forward =
            crate::interaction::movement::rotate_toward_xz(current_xz, desired_forward, max_turn);
        if new_forward.length_squared() > 1e-6 {
            transform.look_to(new_forward, Vec3::Y);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `AimWeapon1(h, p)` a unit receives must turn a piece's S3O +Z
    /// (under the 180° model root) onto the target, whatever the body's
    /// heading, bank or pitch — the old yaw-based heading was off by π
    /// (every turret aimed backwards) and ignored bank/pitch.
    #[test]
    fn local_aim_angles_point_a_turned_piece_at_the_target() {
        let root = Quat::from_rotation_y(std::f32::consts::PI);
        for heading in [0.0f32, 0.9, 2.6, -1.7] {
            for bank in [0.0f32, 0.35] {
                let front = crate::sim::dir3_of(heading);
                let right = front.cross(Vec3::Y);
                let up = (Vec3::Y * bank.cos() + right * bank.sin()).normalize();
                let body = Transform::default().looking_to(front, up).rotation;
                for to_target in [
                    front * 100.0,
                    Vec3::new(80.0, -60.0, 10.0),
                    Vec3::new(-30.0, 25.0, -90.0),
                ] {
                    let (h, p) = local_aim_angles(body, to_target, 0.0);
                    let piece = Quat::from_euler(EulerRot::YXZ, h, -p, 0.0);
                    let gun = body * root * piece * Vec3::Z;
                    assert!(
                        gun.dot(to_target.normalize()) > 0.9999,
                        "heading {heading} bank {bank}: gun {gun} vs {to_target}"
                    );
                }
                // Dead ahead is h = 0.
                let (h, _) = local_aim_angles(body, front * 50.0, 0.0);
                assert!(h.abs() < 0.2, "ahead should be ~0, got {h}");
            }
        }
    }
}

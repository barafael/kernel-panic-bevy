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
use crate::units::assets::animation::CobAnimator;
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

/// Host-side mirror of `byte.bos`'s `isOpen=0` state. Blocks firing
/// until the spawn-time `Open()` animation completes; the COB VM
/// doesn't surface script statics, so the gate lives in Bevy.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct OpeningDelay {
    pub remaining: f32,
}

/// Seconds before a recently-aimed Byte folds its blades back in.
/// Mirrors the `sleep 3000` at the top of `byte.bos Close()`.
pub const BYTE_CLOSE_DELAY: f32 = 3.0;

/// Present iff a Byte is currently in its `isOpen=1` state. Read by
/// the damage pipeline for the upstream 30 % closed-state damage
/// reduction (`byte.bos HitByWeaponId`).
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct ByteOpen {
    pub open_until: f32,
}

/// Tick down [`OpeningDelay`] timers; on expiry, remove the marker
/// and (for Byte) seed [`ByteOpen`] with the upstream
/// [`BYTE_CLOSE_DELAY`] window.
pub fn tick_opening_delay(
    time: Res<Time>,
    mut q: Query<(
        Entity,
        &mut OpeningDelay,
        &crate::units::components::UnitType,
    )>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    let now = time.elapsed_secs();
    for (entity, mut delay, unit_type) in &mut q {
        delay.remaining -= dt;
        if delay.remaining <= 0.0 {
            commands.entity(entity).remove::<OpeningDelay>();
            if unit_type.0 == crate::units::content::definitions::UnitKind::Byte {
                commands.entity(entity).insert(ByteOpen {
                    open_until: now + BYTE_CLOSE_DELAY,
                });
            }
        }
    }
}

/// Remove expired [`ByteOpen`] markers so the damage pipeline can
/// switch the Byte back to its 30 % incoming-damage discount. Runs
/// alongside `tick_opening_delay`; the per-aim refresh in
/// `combat_system` is what keeps an actively-fighting Byte open.
pub fn tick_byte_open(time: Res<Time>, q: Query<(Entity, &ByteOpen)>, mut commands: Commands) {
    let now = time.elapsed_secs();
    for (entity, state) in &q {
        if state.open_until <= now {
            commands.entity(entity).remove::<ByteOpen>();
        }
    }
}

/// Aim-before-fire gate. Inserted at spawn on every unit whose
/// `.cob` declares `AimWeapon1`. `combat_system` blocks firing while
/// `ready == false`; `drive_aim_script` keeps `thread_id` populated
/// while a script is running, [`update_aim_script`] flips `ready`
/// when the thread ends with `ret_code == 1`.
#[derive(Component, Clone, Copy, Debug, Default)]
#[component(storage = "SparseSet")]
pub struct AimScript {
    pub thread_id: Option<u32>,
    pub ready: bool,
    pub last_heading_rad: f32,
    pub last_pitch_rad: f32,
}

/// Heading or pitch shift (radians) at which `drive_aim_script`
/// abandons a still-running aim thread and re-spawns. ~11° — small
/// enough that a Packet dispatching behind a shooter forces a fresh
/// cycle, large enough that drifting targets don't churn threads.
pub const AIM_SCRIPT_RETARGET_THRESHOLD: f32 = 0.2;

/// Spring's COB heading/pitch unit: 65536 == TAU radians. Inverse
/// of [`SHORT_ANGLE_TO_RAD`](super::SHORT_ANGLE_TO_RAD).
const RAD_TO_SHORT_ANGLE: f32 = 65536.0 / std::f32::consts::TAU;

/// Advance every unit's `AimWeapon1` thread lifecycle. Spawn a
/// thread if none is running, or re-spawn if the target shifted
/// past [`AIM_SCRIPT_RETARGET_THRESHOLD`]. The thread itself ticks
/// in `animation_system`; [`update_aim_script`] reads its ret_code.
#[allow(clippy::type_complexity)]
pub fn drive_aim_script(
    mut query: Query<
        (
            &mut AimScript,
            &mut CobAnimator,
            &GlobalTransform,
            &AimTarget,
        ),
        Without<Dying>,
    >,
) {
    for (mut aim, mut animator, gtf, target) in &mut query {
        let attacker_pos = gtf.translation();
        let to_target = target.pos - attacker_pos;
        let heading_rad = to_target.x.atan2(to_target.z);
        let horizontal_dist = (to_target.x * to_target.x + to_target.z * to_target.z).sqrt();
        // Spring's pitch convention is `asin(localY)` (`Weapon.cpp:418`),
        // so positive pitch = target above horizon. Our previous
        // `(-to_target.y).atan2(...)` had the opposite sign and broke
        // the `(<90>-p)` arithmetic in pointer.bos.
        let direct_pitch = to_target.y.atan2(horizontal_dist.max(1e-6));
        let arc_pitch = if target.arc_height > 0.0 && horizontal_dist > 1.0 {
            (4.0 * target.arc_height / horizontal_dist).atan()
        } else {
            0.0
        };
        let pitch_rad = direct_pitch + arc_pitch;

        if aim.thread_id.is_some() {
            let dh = (heading_rad - aim.last_heading_rad).abs();
            let dp = (pitch_rad - aim.last_pitch_rad).abs();
            if dh <= AIM_SCRIPT_RETARGET_THRESHOLD && dp <= AIM_SCRIPT_RETARGET_THRESHOLD {
                continue;
            }
            aim.thread_id = None;
            aim.ready = false;
        }

        let heading = (heading_rad * RAD_TO_SHORT_ANGLE) as i32;
        let pitch = (pitch_rad * RAD_TO_SHORT_ANGLE) as i32;

        let cob = animator.cob.clone();
        if let Some(tid) = animator
            .vm
            .start_script(&cob, "AimWeapon1", &[heading, pitch])
        {
            aim.thread_id = Some(tid);
            aim.last_heading_rad = heading_rad;
            aim.last_pitch_rad = pitch_rad;
        }
    }
}

/// Drain the just-ended aim thread's `ret_code` from each unit's
/// VM and flip [`AimScript::ready`] accordingly. `ret_code == 1`
/// means upstream's `AimWeapon1` returned with the barrel locked
/// on; anything else clears `thread_id` so next frame re-spawns.
pub fn update_aim_script(mut query: Query<(&mut AimScript, &mut CobAnimator)>) {
    for (mut aim, mut animator) in &mut query {
        let Some(tid) = aim.thread_id else { continue };
        if let Some(ret_code) = animator.vm.take_thread_return_code(tid) {
            aim.ready = ret_code == 1;
            aim.thread_id = None;
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

/// Drive the deploy state machine from movement state, firing
/// the unit's `Open()` / `Close()` COB scripts so the visible model
/// matches the logical deploy state. Stopping schedules `Open`; starting
/// to move schedules `Close`.
#[allow(clippy::type_complexity)]
pub fn tick_deploy_state(
    time: Res<Time>,
    mut query: Query<
        (
            &mut Deployable,
            &mut CobAnimator,
            Option<&MoveTarget>,
            Option<&MovePath>,
        ),
        Without<Dying>,
    >,
) {
    let dt = time.delta_secs();
    for (mut deployable, mut animator, move_target, move_path) in &mut query {
        let is_moving = move_target.is_some() || move_path.is_some();

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
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Close", &[]);
            }
            (DeployState::Closed, false) | (DeployState::Closing, false) => {
                deployable.state = DeployState::Opening;
                deployable.timer = DEPLOY_DURATION;
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Open", &[]);
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
        if aimer.is_some() {
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

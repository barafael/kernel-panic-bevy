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
        let direct_pitch = (-to_target.y).atan2(horizontal_dist.max(1e-6));
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
///   body alone and rotate only the aimer piece. This matches upstream
///   exactly: `byte.bos`'s `AimWeapon1(h,p)` does
///       turn aimer to y-axis h speed <270>;
///       turn aimer to x-axis (<-90>-p) speed <270>;
///   where `h` is the **absolute world heading**. Forcing the body to
///   rotate (the previous behavior) was the visible "the byte spins
///   around to face you" bug; upstream's byte never turns its body for
///   aim — only the aimer-rooted firing assembly does.
///
/// Units currently moving (have a `MoveTarget`) are excluded — the
/// movement system owns their heading, and fighting movement for
/// rotation control makes Bits spin around mid-stride every frame.
#[allow(clippy::type_complexity)]
pub fn aim_weapons_system(
    time: Res<Time>,
    mut query: Query<
        (
            &mut Transform,
            &GlobalTransform,
            &UnitStats,
            &AimTarget,
            &mut CobAnimator,
            Option<&crate::units::assets::animation::GunbasePiece>,
            Option<&crate::units::assets::animation::AimerPiece>,
        ),
        Without<crate::interaction::movement::MoveTarget>,
    >,
) {
    let dt = time.delta_secs();
    for (mut transform, gtf, stats, aim, mut animator, gunbase, aimer) in &mut query {
        let attacker_pos = gtf.translation();
        let to_target = Vec3::new(aim.pos.x - attacker_pos.x, 0.0, aim.pos.z - attacker_pos.z);
        let horizontal_dist = to_target.length();
        if horizontal_dist < 1e-4 {
            continue;
        }

        let desired_forward = to_target / horizontal_dist;

        // Body heading: only when there's no aimer to do the work.
        // Spring's byte never rotates its body for aim — the aimer piece
        // (root of rotor → blade0..3 → bp0..3 in octaeder.s3o) carries
        // the firing assembly around. We follow that exactly.
        if aimer.is_none() {
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
            let new_forward = crate::interaction::movement::rotate_toward_xz(
                current_xz,
                desired_forward,
                max_turn,
            );
            if new_forward.length_squared() > 1e-6 {
                transform.look_to(new_forward, Vec3::Y);
            }
        }

        // Gunbase pitch: elevate the barrel. For a ballistic lob of peak
        // height h over distance d, the launch angle above horizontal is
        // roughly atan(4h/d); add that to the direct line-of-sight pitch
        // so mortar-type shots arc onto the target.
        let dy = aim.pos.y - attacker_pos.y;
        let direct_pitch = (dy).atan2(horizontal_dist);
        let arc_pitch = if aim.arc_height > 0.0 && horizontal_dist > 1.0 {
            (4.0 * aim.arc_height / horizontal_dist).atan()
        } else {
            0.0
        };
        let pitch = direct_pitch + arc_pitch;

        // pointer.bos sets gunbase's rest rotation to x-axis π/2 in Create
        // (so the barrel folds flat). AimWeapon1 rewrites it to (π/2 − p),
        // which is the same convention: higher pitch = smaller X rotation.
        // Since our VM doesn't actually run the aim loop, mirror it here.
        if let Some(gb) = gunbase
            && gb.0 < animator.piece_rotations.len()
        {
            let idx = gb.0;
            let target_x = std::f32::consts::FRAC_PI_2 - pitch;
            animator.target_rotations[idx][0] = target_x;
            // Reasonable pitch rate (~90°/sec) so the barrel visibly
            // swings instead of snapping. The COB script uses speed <50>
            // (50 ang-units/frame ≈ 8.2°/sec) which feels too sluggish
            // for a responsive host-driven aim; we split the difference.
            animator.turn_speeds[idx][0] = std::f32::consts::PI * 0.5;
        }

        // Aimer-piece rotation. We re-implement upstream's
        //     turn aimer to y-axis h
        //     turn aimer to x-axis (<-90>-p)
        // host-side because the COB script's `AimWeapon1` only fires on
        // JustFired (post-shot), so the aimer would always slew behind
        // each volley if we relied on it alone.
        //
        // Two upstream conventions to reproduce exactly:
        //
        // 1. `h` is **absolute world heading** (Spring's heading 0 = +Z).
        //    Upstream's body doesn't move for aim, so the script can hand
        //    `h` straight to the aimer. We may have a non-zero body yaw
        //    (e.g. byte spawned at some orientation); subtract it so
        //    `aimer_local + body_yaw = world heading`.
        //
        // 2. `(<-90>-p)` on the X axis is the modeler's "rest pose"
        //    convention: at p=0 the aimer is tipped forward 90° so its
        //    children — including the bp0..bp3 firing points at offset
        //    (0, -48, 0) from each blade — end up at the byte's *forward*
        //    edge (z = +48 in body frame) instead of dangling 48 elmos
        //    below the unit center. Without this, beams emerge from
        //    underneath the byte. The minus-pitch term elevates further.
        //
        // We unwrap the Y delta into (-π, π] so the slew always takes the
        // short way around — animation.rs's interp is naive linear and
        // would otherwise spin a full revolution when target wraps.
        if let Some(ap) = aimer
            && ap.0 < animator.piece_rotations.len()
        {
            let idx = ap.0;
            let target_world_heading = desired_forward.x.atan2(desired_forward.z);
            let body_yaw = transform.rotation.to_euler(EulerRot::YXZ).0;
            let current_y = animator.piece_rotations[idx][1];
            let raw_target = target_world_heading - body_yaw;
            let mut delta = raw_target - current_y;
            while delta > std::f32::consts::PI {
                delta -= std::f32::consts::TAU;
            }
            while delta < -std::f32::consts::PI {
                delta += std::f32::consts::TAU;
            }
            animator.target_rotations[idx][1] = current_y + delta;
            animator.target_rotations[idx][0] = -std::f32::consts::FRAC_PI_2 - pitch;
            // Spring's <270> is 270 ang-units/frame at 30 Hz ≈ 1500°/s —
            // effectively snap. Match that so the aimer locks on within
            // a frame of acquisition; bursts don't drag the firing point
            // behind successive shots.
            animator.turn_speeds[idx][0] = std::f32::consts::TAU * 4.0;
            animator.turn_speeds[idx][1] = std::f32::consts::TAU * 4.0;
        }
    }
}

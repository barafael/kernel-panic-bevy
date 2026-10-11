//! Deploy cycle + host-driven aim for armed units.
//!
//! Two concerns:
//! - **Deploy state** — [`Deployable`] / [`DeployState`] mirror the
//!   pack/unpack cycle of units that must unfold before firing (Pointer).
//!   The unit's animation driver owns the cycle (it runs the script's
//!   `Open()` / `Close()` threads off `StartMoving` / `StopMoving`);
//!   [`sync_deploy_state`] copies its state here for the fire gates.
//! - **Aim** — [`aim_weapons_system`] points each unit's weapon at its
//!   [`AimTarget`]. The path branches on whether the unit's COB script
//!   declares an `aimer` piece: see the function docs for the upstream
//!   conventions each branch reproduces.

use bevy::math::ops;
use bevy::prelude::*;

use super::{Dying, Stunned};
use crate::interaction::movement::{AttackMoveActive, MovePath, MoveTarget};
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

/// Attached to units with a deploy cycle: the driver's state, mirrored
/// every frame by [`sync_deploy_state`].
#[derive(Component)]
pub struct Deployable {
    pub state: DeployState,
}

/// Stamped by `combat_system` each frame an armed unit has picked a target
/// it wants to fire at. Read by `aim_weapons_system` to rotate the body /
/// tilt the gun before combat actually commits the shot. Removed in
/// frames where the unit has no viable target so aim systems don't keep
/// steering toward a stale position.
#[derive(Component, Clone, Copy, Debug)]
pub struct AimTarget {
    pub pos: Vec3,
    /// How the weapon's projectile leaves the muzzle, which is the
    /// direction the script is asked to aim at.
    pub launch: AimLaunch,
}

/// The launch direction a weapon's `AimWeapon` script receives —
/// `CWeapon::UpdateAim` hands the script the pitch of `wantedDir`, and
/// each weapon type has its own idea of that (`Weapon.cpp:417`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AimLaunch {
    /// Straight at the target (beams, lasers, melee).
    Direct,
    /// `CMissileLauncher::UpdateWantedDir`: the target direction lifted
    /// by `trajectoryHeight` and renormalised — the Pointer's Geometric
    /// (`trajectoryheight=1`) leaves at ~45° up, and so does its gun.
    Missile { trajectory_height: f32 },
    /// `CCannon::CalcWantedDir`: the ballistic solution
    /// ([`cannon_wanted_dir`]) for the shell's `speed` (elmos/frame)
    /// under the weapon's `gravity` (elmos/frame², [`weapon_gravity`]),
    /// low trajectory — the same solve the shell is launched with.
    Cannon { speed: f32, gravity: f32 },
}

impl AimLaunch {
    /// The launch of `weapon`'s projectile, per its Spring weapon type,
    /// on a map of `map_gravity` (elmos/frame², negative).
    pub fn of(weapon: Option<&spring_tdf::WeaponDef>, map_gravity: f32) -> Self {
        use spring_tdf::WeaponCategory as C;
        let Some(w) = weapon else {
            return Self::Direct;
        };
        match w.category() {
            C::MissileLauncher | C::StarburstLauncher if w.trajectory_height > 0.0 => {
                Self::Missile {
                    trajectory_height: w.trajectory_height,
                }
            }
            C::Cannon if w.weapon_velocity > 0.0 => Self::Cannon {
                speed: w.projectile_speed(),
                gravity: weapon_gravity(w, map_gravity),
            },
            _ => Self::Direct,
        }
    }

    /// Pitch (radians, up positive) of the launch toward a target
    /// `horizontal` elmos away and `up` elmos higher.
    fn pitch(self, horizontal: f32, up: f32) -> f32 {
        match self {
            Self::Direct => ops::atan2(up, horizontal.max(1e-6)),
            Self::Missile { trajectory_height } => {
                let dist = (horizontal * horizontal + up * up).sqrt().max(1e-6);
                let dir = Vec3::new(horizontal / dist, up / dist + trajectory_height, 0.0)
                    .normalize_or(Vec3::Y);
                ops::asin(dir.y.clamp(-1.0, 1.0))
            }
            Self::Cannon { speed, gravity } => {
                let dir = cannon_wanted_dir(Vec3::new(horizontal, up, 0.0), speed, gravity);
                ops::asin(dir.y.clamp(-1.0, 1.0))
            }
        }
    }
}

// ── Ballistics ────────────────────────────────────────────────────────
//
// One model for every ballistic shell, shared by the aim (the pitch the
// script is asked for), the fire side (the launch direction), the shell's
// flight (`weapon_fx::flight::CannonFlight`) and the range test — all in
// Spring's native units: elmos/frame speeds, elmos/frame² gravity.

/// Gravity a weapon's projectile falls under, in elmos/frame² (negative
/// is down): the map's (`map_gravity`), unless the TDF overrides it with
/// `myGravity` —
/// `mix(map.gravity, -myGravity, myGravity != 0)` (`CCannon::Init`,
/// `CWeapon::GetAccuratePredictedImpactTime`, `CMissileLauncher::
/// FireImpl`). `myGravity` is already per frame²: the BugCannon's 0.3 is
/// 270 elmos/s², over five times the map's 50.
pub fn weapon_gravity(weapon: &spring_tdf::WeaponDef, map_gravity: f32) -> f32 {
    if weapon.my_gravity != 0.0 {
        -weapon.my_gravity
    } else {
        map_gravity
    }
}

/// `CCannon::CalcWantedDir` (low trajectory — no Kernel Panic weapon sets
/// `highTrajectory`): the unit launch direction that puts a shell of
/// `speed` elmos/frame under `gravity` (elmos/frame², negative) onto a
/// point `target_vec` away. Out of ballistic reach the engine leaves the
/// direction level toward the target; a zero vector fires straight down.
pub fn cannon_wanted_dir(target_vec: Vec3, speed: f32, gravity: f32) -> Vec3 {
    let dsq = target_vec.length_squared();
    let dfsq = target_vec.xz().length_squared();
    let (g, v, dy) = (gravity, speed, target_vec.y);
    let dxz = dfsq.sqrt();

    let (mut vxz, mut vy) = (0.0f32, 0.0f32);
    if dsq == 0.0 {
        vy = -v;
    } else if dsq < 1e12 && dy.abs() < 1e6 {
        let vsq = v * v;
        let root1 = vsq * vsq + 2.0 * vsq * g * dy - g * g * dfsq;
        if root1 >= 0.0 {
            let root2 = 2.0 * dfsq * dsq * (vsq + g * dy + root1.sqrt());
            if root2 >= 0.0 {
                vxz = root2.sqrt() / (2.0 * dsq);
                vy = if dxz == 0.0 || vxz == 0.0 {
                    v
                } else {
                    vxz * dy / dxz - dxz * g / (2.0 * vxz)
                };
            }
        }
    }

    // `SafeNormalize` keeps a zero vector zero.
    let mut dir = Vec3::new(target_vec.x, 0.0, target_vec.z).normalize_or_zero();
    if vxz != 0.0 || vy != 0.0 {
        dir *= vxz;
        dir.y = vy;
        dir = dir.normalize_or_zero();
    }
    dir
}

/// `CCannon::FireImpl`'s launch direction before spray: the ballistic
/// solve, except for a target within 2 elmos of the muzzle (an `emit-sfx`
/// shot), which is fired straight at.
pub fn cannon_launch_dir(target_vec: Vec3, speed: f32, gravity: f32) -> Vec3 {
    if target_vec.length_squared() > 4.0 {
        cannon_wanted_dir(target_vec, speed, gravity)
    } else {
        target_vec
    }
}

/// `CCannon::GetStaticRange2D` after `CCannon::UpdateRange`: the 2D reach
/// of a shell of `speed` under `gravity` toward a target `height_diff`
/// elmos above (negative: below) the aim point, scaled so that level
/// ground reaches exactly the authored `range` (`rangeBoostFactor`), with
/// the engine's derived `heightBoostFactor` (TDF default -1) stretching
/// it downhill.
fn cannon_range_2d(range: f32, height_diff: f32, speed: f32, gravity: f32) -> f32 {
    if gravity >= 0.0 {
        return range;
    }
    // `CalcRange2D`: the 45° (max-range) launch, `speedFactor` √½, with
    // a 100-elmo smoothing band below the shooter.
    let calc = |mut height_diff: f32, range_boost: f32, height_boost: f32| -> f32 {
        const SMOOTH_HEIGHT: f32 = 100.0;
        let speed_2d = speed * std::f32::consts::FRAC_1_SQRT_2;
        let sq_speed_2d = speed_2d * speed_2d;
        if height_diff < -SMOOTH_HEIGHT {
            height_diff *= height_boost;
        } else if height_diff < 0.0 {
            height_diff *= 1.0 + (height_boost - 1.0) * -height_diff / SMOOTH_HEIGHT;
        }
        let root = sq_speed_2d + 2.0 * gravity * height_diff;
        if root < 0.0 {
            return 0.0;
        }
        range_boost * (sq_speed_2d + speed_2d * root.sqrt()) / -gravity
    };
    let range_boost = (range / calc(0.0, 1.0, -1.0)).clamp(0.0, 1.0);
    let height_boost = if range_boost > 0.0 {
        (2.0 - range_boost) / range_boost.sqrt()
    } else {
        -1.0
    };
    calc(height_diff, range_boost, height_boost)
}

/// `weaponDef->heightmod`: how much of the height difference counts
/// against the range. No Kernel Panic weapon authors `heightMod`, so this
/// is the engine's per-type default (`WeaponDef.cpp`).
fn height_mod(weapon: &spring_tdf::WeaponDef) -> f32 {
    use spring_tdf::WeaponCategory as C;
    match weapon.category() {
        C::Cannon => 0.8,
        C::BeamLaser | C::LightningCannon => 1.0,
        _ => 0.2,
    }
}

/// `CWeapon::GetRange2D(0, heightDiff · heightmod)` per weapon type: how
/// far (horizontally) `weapon` reaches toward a target `height_diff`
/// elmos above its aim point. `ground_target`: an attack-ground position,
/// which `TestRange` always checks in a sphere (`Target_Pos`).
pub fn weapon_range_2d(
    weapon: &spring_tdf::WeaponDef,
    height_diff: f32,
    ground_target: bool,
    map_gravity: f32,
) -> f32 {
    use spring_tdf::WeaponCategory as C;
    let range = weapon.effective_range();
    let ydiff = height_diff * height_mod(weapon);
    match weapon.category() {
        // Melee `cylinderTargeting` defaults to 1: a cylinder `range`
        // tall instead of a sphere.
        C::Melee if !ground_target => {
            if range > ydiff.abs() {
                range
            } else {
                0.0
            }
        }
        C::Cannon if weapon.weapon_velocity > 0.0 => cannon_range_2d(
            range,
            ydiff,
            weapon.projectile_speed(),
            weapon_gravity(weapon, map_gravity),
        ),
        // `CStarburstLauncher::GetRange2D`.
        C::StarburstLauncher => range + ydiff,
        _ => (range * range - ydiff * ydiff).max(0.0).sqrt(),
    }
}

/// `CWeapon::TestRange`: whether `target` is in reach of `weapon` aimed
/// from `aim_from` — the 2D distance against [`weapon_range_2d`] for the
/// height difference. The one range test every fire / chase decision
/// uses, so a unit that stops chasing is always able to shoot.
pub fn in_weapon_range(
    weapon: &spring_tdf::WeaponDef,
    aim_from: Vec3,
    target: Vec3,
    ground_target: bool,
    map_gravity: f32,
) -> bool {
    let reach = weapon_range_2d(weapon, target.y - aim_from.y, ground_target, map_gravity);
    aim_from.xz().distance_squared(target.xz()) <= reach * reach
}

/// XZ radius a target scan must cover to find every target
/// [`in_weapon_range`] could accept from `aim_from` — upstream
/// `GenerateWeaponTargets`' `scanRadius`, taking the lowest point of the
/// map as `max(0, minHeight)` = 0.
pub fn weapon_scan_radius(weapon: &spring_tdf::WeaponDef, aim_from: Vec3, map_gravity: f32) -> f32 {
    let lowest = weapon_range_2d(weapon, -aim_from.y.max(0.0), false, map_gravity);
    weapon.effective_range().max(lowest)
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
/// The pitch is that of the weapon's launch direction ([`AimLaunch`]),
/// measured in the unit frame.
pub fn local_aim_angles(unit_rot: Quat, to_target: Vec3, launch: AimLaunch) -> (f32, f32) {
    let local = unit_rot.inverse() * to_target;
    let (right, up, front) = (local.x, local.y, -local.z);
    let heading = ops::atan2(-right, front);
    let horizontal = (right * right + front * front).sqrt();
    (heading, launch.pitch(horizontal, up))
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
        ),
        // `CUnit::CanUpdateWeapons`: a paralyzed unit's weapons neither
        // aim nor fire.
        (Without<Dying>, Without<Stunned>),
    >,
) {
    for (mut aim, mut animator, gtf, target, move_target, move_path) in &mut query {
        let (rel_heading, pitch_rad) = local_aim_angles(
            gtf.rotation(),
            target.pos - gtf.translation(),
            target.launch,
        );

        let dh = (rel_heading - aim.last_heading_rad).abs();
        let dp = (pitch_rad - aim.last_pitch_rad).abs();
        let retarget = dh > AIM_SCRIPT_RETARGET_THRESHOLD || dp > AIM_SCRIPT_RETARGET_THRESHOLD;

        let ctx = AnimCtx {
            moving: move_target.is_some() || move_path.is_some(),
            aim_active: true,
            target_xz_dist: Some((target.pos - gtf.translation()).xz().length()),
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
    /// Freshly-spawned deployable units start stowed (`Closed`); the
    /// driver opens them from `Create()` (if built) or after their first
    /// `StopMoving`.
    pub fn initial() -> Self {
        Self {
            state: DeployState::Closed,
        }
    }
}

/// Mirror each deployable unit's driver state into [`Deployable`]: the
/// script's `isOpen` and its running `Open()`/`Close()` thread are the
/// single source of truth for whether the Pointer may fire.
pub fn sync_deploy_state(mut query: Query<(&mut Deployable, &UnitAnimator)>) {
    for (mut deployable, animator) in &mut query {
        let state = animator
            .driver
            .deploy_state()
            .unwrap_or(DeployState::Closed);
        if deployable.state != state {
            deployable.state = state;
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
///   etc.) turn the entire body to face the target via `look_to`. A
///   host-side stand-in for Spring's `set HEADING`: the .bos
///   `AimWeapon1` scripts aren't driven eagerly enough for those units'
///   aim loops to produce visible turret rotation in time.
/// - **Aimer-piece aim** — units with an [`AimerPiece`] (currently just
///   Byte's octahedron) leave the body alone and rotate only the aimer
///   piece. Mirrors `byte.bos`'s `AimWeapon1(h,p)`:
///   `turn aimer to y-axis h speed <270>` then
///   `turn aimer to x-axis (<-90>-p) speed <270>`, where `h` is the
///   **absolute world heading**. Upstream's byte never turns its body
///   for aim — only the aimer-rooted firing assembly does.
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
            &UnitStats,
            &AimTarget,
            Option<&crate::units::assets::animation::AimerPiece>,
            Option<&mut crate::interaction::ground_move::GroundMover>,
            Has<MoveTarget>,
            Has<AttackMoveActive>,
            Option<&super::TargetCache>,
            Option<&Deployable>,
        ),
        // A paralyzed unit doesn't turn to aim (`CanUpdateWeapons`).
        Without<Stunned>,
    >,
) {
    let dt = time.delta_secs();
    for (mut transform, stats, aim, aimer, mover, has_move, attack_move, engaged, deployable) in
        &mut query
    {
        // pointer.bos turns the body (`set HEADING`) inside `AimWeapon1`
        // only `if (isOpen)`: a stowed or unfolding Pointer sits still.
        if deployable.is_some_and(|d| d.state != DeployState::Open) {
            continue;
        }
        // HoverAttack aircraft (Flow) never turn their body for an
        // auto-acquired target: `HoverAirMoveType` owns the heading (it
        // only faces `circlingPos` under an explicit attack order), and
        // flow.bos swings the `base` piece onto the target instead.
        // Turning the body here also fought `hover_air_system`, which
        // rewrites the attitude every tick.
        if aimer.is_some() || stats.can_fly {
            continue;
        }
        // A plain move order owns the heading; a fight order that has
        // engaged is holding (see `moving_for_script`) and turns to
        // shoot, like pointer.bos's `set HEADING` loop in AimWeapon1. A
        // fight leg merely tracking an out-of-range T target marches on.
        let holding = attack_move && crate::interaction::movement::fight_engaged(true, engaged);
        if has_move && !holding {
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
        let max_turn = if stats.turn_rate > 0.0 {
            stats.turn_rate * dt
        } else {
            std::f32::consts::TAU
        };

        // Ground units: the mover writes the body rotation from its
        // heading every tick, so the script's `set HEADING` turns that
        // — a `look_to` on the transform alone was undone next tick.
        if let Some(mut mover) = mover {
            let wanted = crate::sim::Heading::from_vector(desired_forward.x, desired_forward.z);
            let delta = wanted.wrapping_sub(mover.heading) as f32;
            let max_units = max_turn / crate::sim::SHORT_ANGLE_TO_RAD;
            let step = delta.clamp(-max_units, max_units) as i32 as i16;
            if step != 0 {
                mover.heading = mover.heading.wrapping_add(step);
            }
            let up = transform.up().as_vec3();
            let rotation = crate::interaction::ground_move::attitude(mover.heading, up);
            if transform.rotation != rotation {
                transform.rotation = rotation;
            }
            continue;
        }

        let forward_vec = transform.forward().as_vec3();
        let current_xz = {
            let f = Vec3::new(forward_vec.x, 0.0, forward_vec.z);
            if f.length_squared() < 1e-6 {
                Vec3::Z
            } else {
                f.normalize()
            }
        };
        let new_forward =
            crate::interaction::movement::rotate_toward_xz(current_xz, desired_forward, max_turn);
        if new_forward.length_squared() > 1e-6 {
            transform.look_to(new_forward, Vec3::Y);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const G: f32 = crate::sim::MAP_GRAVITY_PER_FRAME2;

    /// A missile with `trajectoryheight=1` leaves at 45° toward a level
    /// target; a cannon shell's pitch is the low ballistic solution
    /// (`sin 2θ = g·d / v²` on flat ground); a beam aims straight.
    #[test]
    fn launch_pitches_follow_the_engine_weapon_types() {
        let level = Quat::IDENTITY;
        let target = Vec3::new(0.0, 0.0, -1000.0);
        let (_, p) = local_aim_angles(level, target, AimLaunch::Direct);
        assert!(p.abs() < 1e-6);
        let (_, p) = local_aim_angles(
            level,
            target,
            AimLaunch::Missile {
                trajectory_height: 1.0,
            },
        );
        assert!((p - std::f32::consts::FRAC_PI_4).abs() < 1e-4, "{p}");
        let v = 400.0 / crate::sim::GAME_SPEED;
        let gravity = crate::sim::MAP_GRAVITY_PER_FRAME2;
        let (_, p) = local_aim_angles(level, target, AimLaunch::Cannon { speed: v, gravity });
        let expected = 0.5 * ops::asin(-gravity * 1000.0 / (v * v));
        assert!((p - expected).abs() < 1e-3, "{p} vs {expected}");
    }

    /// corruptionweapons.tdf `[BugCannon]`: `weaponvelocity=600`,
    /// `myGravity=.3`, `range=1200` (`ballistic=1`, no weapon type →
    /// Cannon). `startvelocity` plays no part in a cannon shot.
    pub(crate) fn bug_cannon() -> spring_tdf::WeaponDef {
        spring_tdf::WeaponDef {
            ballistic: true,
            range: 1200.0,
            weapon_velocity: 600.0,
            start_velocity: 800.0,
            my_gravity: 0.3,
            ..Default::default()
        }
    }

    /// `myGravity` is elmos/frame² and replaces the map's gravity; the
    /// shell speed is `weaponvelocity / 30`. The aim the script receives
    /// is the textbook low-arc solution for those numbers: at 1000 elmos
    /// `sin 2θ = g·d / v² = 0.3·1000 / 20² = 0.75` → θ ≈ 24.3°.
    #[test]
    fn bug_cannon_aims_with_its_own_gravity_and_speed() {
        let w = bug_cannon();
        assert_eq!(weapon_gravity(&w, G), -0.3);
        let launch = AimLaunch::of(Some(&w), G);
        let AimLaunch::Cannon { speed, gravity } = launch else {
            panic!("{launch:?}");
        };
        assert!((speed - 20.0).abs() < 1e-4 && gravity == -0.3);
        let (_, p) = local_aim_angles(Quat::IDENTITY, Vec3::new(0.0, 0.0, -1000.0), launch);
        let expected = 0.5 * ops::asin(0.75f32);
        assert!((p - expected).abs() < 1e-4, "{p} vs {expected}");
        assert!(p.to_degrees() > 24.0 && p.to_degrees() < 24.6);
        // Map gravity (no myGravity) is the fallback.
        let plain = spring_tdf::WeaponDef {
            my_gravity: 0.0,
            ..w
        };
        assert_eq!(weapon_gravity(&plain, G), G);
    }

    /// `CCannon::CalcWantedDir`: uphill targets need a steeper launch,
    /// unreachable ones get the level fallback, a zero vector points down.
    #[test]
    fn cannon_wanted_dir_edge_cases() {
        let (v, g) = (20.0, -0.3);
        let level = cannon_wanted_dir(Vec3::new(800.0, 0.0, 0.0), v, g);
        let uphill = cannon_wanted_dir(Vec3::new(800.0, 200.0, 0.0), v, g);
        assert!(uphill.y > level.y);
        assert!((level.length() - 1.0).abs() < 1e-5);
        // Max reach on the level is v²/g ≈ 1333 elmos.
        let far = cannon_wanted_dir(Vec3::new(0.0, 0.0, 2000.0), v, g);
        assert_eq!(far, Vec3::Z);
        assert_eq!(cannon_wanted_dir(Vec3::ZERO, v, g), Vec3::NEG_Y);
    }

    /// `CCannon::UpdateRange` scales the ballistic reach so level ground
    /// reaches exactly the authored range; shooting downhill reaches
    /// farther, uphill less. Direct-fire weapons measure 2D with only a
    /// fifth of the height difference counting (`heightmod` 0.2).
    #[test]
    fn weapon_range_follows_test_range() {
        let w = bug_cannon();
        assert!((weapon_range_2d(&w, 0.0, false, G) - 1200.0).abs() < 0.5);
        assert!(weapon_range_2d(&w, -200.0, false, G) > 1250.0);
        assert!(weapon_range_2d(&w, 200.0, false, G) < 1150.0);
        assert!(weapon_scan_radius(&w, Vec3::new(0.0, 300.0, 0.0), G) > 1250.0);

        let beam = spring_tdf::WeaponDef {
            weapon_type: "LaserCannon".into(),
            weapon_velocity: 512.0,
            range: 256.0,
            ..Default::default()
        };
        let range = beam.effective_range();
        let from = Vec3::new(0.0, 10.0, 0.0);
        // 2D distance at the limit, target 100 elmos below: the 20-elmo
        // effective height shortens the reach to √(r² − 20²).
        let reach = (range * range - 20.0 * 20.0).sqrt();
        assert!(in_weapon_range(
            &beam,
            from,
            Vec3::new(reach - 0.5, -90.0, 0.0),
            false,
            G
        ));
        assert!(!in_weapon_range(
            &beam,
            from,
            Vec3::new(reach + 0.5, -90.0, 0.0),
            false,
            G
        ));
        // Melee: a cylinder, the height ignored up to `range`.
        let melee = spring_tdf::WeaponDef {
            weapon_type: "Melee".into(),
            range: 200.0,
            ..Default::default()
        };
        assert!(in_weapon_range(
            &melee,
            from,
            Vec3::new(199.0, 400.0, 0.0),
            false,
            G
        ));
        assert!(!in_weapon_range(
            &melee,
            from,
            Vec3::new(199.0, 1200.0, 0.0),
            false,
            G
        ));
    }

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
                let up = (Vec3::Y * ops::cos(bank) + right * ops::sin(bank)).normalize();
                let body = Transform::default().looking_to(front, up).rotation;
                for to_target in [
                    front * 100.0,
                    Vec3::new(80.0, -60.0, 10.0),
                    Vec3::new(-30.0, 25.0, -90.0),
                ] {
                    let (h, p) = local_aim_angles(body, to_target, AimLaunch::Direct);
                    let piece = Quat::from_euler(EulerRot::YXZ, h, -p, 0.0);
                    let gun = body * root * piece * Vec3::Z;
                    assert!(
                        gun.dot(to_target.normalize()) > 0.9999,
                        "heading {heading} bank {bank}: gun {gun} vs {to_target}"
                    );
                }
                // Dead ahead is h = 0.
                let (h, _) = local_aim_angles(body, front * 50.0, AimLaunch::Direct);
                assert!(h.abs() < 0.2, "ahead should be ~0, got {h}");
            }
        }
    }
}

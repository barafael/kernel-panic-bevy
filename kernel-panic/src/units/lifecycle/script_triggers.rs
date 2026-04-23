//! Bridges game events to COB animation script calls.
//!
//! Detects state changes (started moving, stopped moving, started building,
//! attacking) and fires the corresponding COB script functions.

use std::f32::consts::PI;

use bevy::prelude::*;

use super::production::Producer;
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::assets::animation::{AimerPiece, CobAnimator};
use crate::units::combat::Dying;

/// Marks a unit that fired `StartMoving` on its COB VM and has not yet
/// fired `StopMoving`. Presence means "previously observed moving"; the
/// trigger system toggles it against the live MoveTarget / MovePath state.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct WasMoving;

/// Marks a producer that fired `Activate` on its COB VM and has not yet
/// fired `Deactivate`. Toggled by `trigger_production_scripts`.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct WasActive;

/// Detect movement start/stop and fire StartMoving/StopMoving COB scripts.
#[allow(clippy::type_complexity)]
pub fn trigger_movement_scripts(
    mut query: Query<(
        Entity,
        &mut CobAnimator,
        Option<&MoveTarget>,
        Option<&MovePath>,
        Has<WasMoving>,
    )>,
    mut commands: Commands,
) {
    for (entity, mut animator, move_target, move_path, was_moving) in &mut query {
        let is_moving = move_target.is_some() || move_path.is_some();
        match (is_moving, was_moving) {
            (true, false) => {
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "StartMoving", &[]);
                commands.entity(entity).insert(WasMoving);
            }
            (false, true) => {
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "StopMoving", &[]);
                commands.entity(entity).remove::<WasMoving>();
            }
            _ => {}
        }
    }
}

/// Detect factory activation and fire Activate/Deactivate COB scripts.
pub fn trigger_production_scripts(
    mut query: Query<(Entity, &mut CobAnimator, &Producer, Has<WasActive>)>,
    mut commands: Commands,
) {
    for (entity, mut animator, producer, was_active) in &mut query {
        let is_active = producer.current_production().is_some();
        match (is_active, was_active) {
            (true, false) => {
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Activate", &[]);
                commands.entity(entity).insert(WasActive);
            }
            (false, true) => {
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Deactivate", &[]);
                commands.entity(entity).remove::<WasActive>();
            }
            _ => {}
        }
    }
}

/// Marker inserted by the combat system on the frame a unit fires.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct JustFired {
    /// World-space position of the target being fired at.
    pub target_pos: Vec3,
    /// Arc height of the projectile's flight. Non-zero for ballistic
    /// weapons (`trajectoryheight` > 0 in the .tdf); lets the gun aim at
    /// the launch angle rather than the direct line to target.
    pub arc_height: f32,
}

/// When a unit has JustFired, call AimWeapon1 and FireWeapon1 on the COB VM.
///
/// `AimWeapon1` is **skipped** for units with an [`AimerPiece`] — those
/// units (currently just byte) have their aimer rotation driven host-side
/// in `aim_weapons_system` so the aim is correct *before* the shot
/// leaves, not slewing afterwards. Running both produces a tug-of-war:
/// the host write happens this tick, the COB write follows on the next
/// VM tick with `(<-90>-p)` baked from a now-stale heading. `FireWeapon1`
/// still runs unconditionally — it's just emit-sfx for the muzzle puffs
/// and doesn't fight with host-side aim.
pub fn trigger_weapon_scripts(
    mut query: Query<
        (
            Entity,
            &mut CobAnimator,
            &GlobalTransform,
            &JustFired,
            Option<&AimerPiece>,
        ),
        Without<Dying>,
    >,
    mut commands: Commands,
) {
    for (entity, mut animator, attacker_gtf, just_fired, aimer) in &mut query {
        let cob = animator.cob.clone();

        if aimer.is_none() {
            let attacker_pos = attacker_gtf.translation();
            let to_target = just_fired.target_pos - attacker_pos;

            // Convert world-space direction to Spring heading (Y-axis rotation)
            // and pitch (X-axis elevation). COB uses angular units (65536 = 360°).
            let heading_rad = to_target.x.atan2(to_target.z);
            let horizontal_dist = (to_target.x * to_target.x + to_target.z * to_target.z).sqrt();
            // For a ballistic shot the initial launch angle is above the direct
            // line so the projectile arcs down onto the target. A symmetric arc
            // of height `h` over horizontal distance `d` has a peak at the
            // midpoint, giving an initial vertical rise of roughly 4h/d per
            // unit of horizontal travel — so the extra pitch is atan(4h/d).
            let direct_pitch = (-to_target.y).atan2(horizontal_dist);
            let arc_pitch = if just_fired.arc_height > 0.0 && horizontal_dist > 1.0 {
                (4.0 * just_fired.arc_height / horizontal_dist).atan()
            } else {
                0.0
            };
            let pitch_rad = direct_pitch + arc_pitch;

            let heading = (heading_rad / (2.0 * PI) * 65536.0) as i32;
            let pitch = (pitch_rad / (2.0 * PI) * 65536.0) as i32;

            // AimWeapon1(heading, pitch) — returns 1 if ready to fire.
            animator
                .vm
                .start_script(&cob, "AimWeapon1", &[heading, pitch]);
        }
        animator.vm.start_script(&cob, "FireWeapon1", &[]);

        commands.entity(entity).remove::<JustFired>();
    }
}

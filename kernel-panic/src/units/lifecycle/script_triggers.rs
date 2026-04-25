//! Bridges game events to COB animation script calls.
//!
//! Detects state changes (started moving, stopped moving, started building,
//! attacking) and fires the corresponding COB script functions.

use bevy::prelude::*;

use super::production::Producer;
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::assets::animation::CobAnimator;
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
/// Used solely as the trigger for `FireWeapon1` — aim (heading /
/// pitch / arc) is handled per-frame by [`drive_aim_script`], so this
/// is a bare marker rather than carrying the target pose.
///
/// [`drive_aim_script`]: crate::units::combat::drive_aim_script
#[derive(Component, Default)]
#[component(storage = "SparseSet")]
pub struct JustFired;

/// When a unit has JustFired, call FireWeapon1 on the COB VM.
///
/// `AimWeapon1` is **no longer** triggered here — [`drive_aim_script`]
/// runs it per-frame from [`AimTarget`] + entity transform so the
/// script's return value can gate firing *before* the shot. Calling
/// it again here would spawn a second aim thread, `signal SIG_AIM`
/// would kill the first one, and the [`AimScript`] ready flag would
/// race to false on the frame of the shot that *just* satisfied it.
///
/// `FireWeapon1` still runs per shot — it's the recoil / muzzle-flash
/// animation trigger and doesn't conflict with the aim path.
///
/// [`drive_aim_script`]: crate::units::combat::drive_aim_script
/// [`AimTarget`]: crate::units::combat::AimTarget
/// [`AimScript`]: crate::units::combat::AimScript
pub fn trigger_weapon_scripts(
    mut query: Query<(Entity, &mut CobAnimator, &JustFired), Without<Dying>>,
    mut commands: Commands,
) {
    for (entity, mut animator, _just_fired) in &mut query {
        let cob = animator.cob.clone();
        animator.vm.start_script(&cob, "FireWeapon1", &[]);
        commands.entity(entity).remove::<JustFired>();
    }
}

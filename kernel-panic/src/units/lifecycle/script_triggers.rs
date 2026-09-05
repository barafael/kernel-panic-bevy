//! Bridges game events to per-unit animation drivers.
//!
//! Detects state changes (started moving, stopped moving, started
//! producing, fired a weapon) and drives the unit's animation driver —
//! the Rust-native replacement for the old COB script entry points
//! (`StartMoving`, `Activate`, `FireWeapon1`, ...).

use bevy::prelude::*;

use super::production::Producer;
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::assets::animation::{AnimCtx, UnitAnimator};
use crate::units::combat::Dying;

/// Marks a unit that fired its start-moving animation and has not yet
/// fired stop-moving. Presence means "previously observed moving"; the
/// trigger system toggles it against the live MoveTarget / MovePath
/// state.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct WasMoving;

/// Marks a producer that fired its activate animation and has not yet
/// fired deactivate. Toggled by `trigger_production_scripts`.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct WasActive;

/// Detect movement start/stop and drive the driver's
/// `start_moving`/`stop_moving` hooks.
#[allow(clippy::type_complexity)]
pub fn trigger_movement_scripts(
    mut query: Query<(
        Entity,
        &mut UnitAnimator,
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
                let UnitAnimator { rig, driver, .. } = &mut *animator;
                driver.start_moving(rig, AnimCtx::minimal());
                commands.entity(entity).insert(WasMoving);
            }
            (false, true) => {
                let UnitAnimator { rig, driver, .. } = &mut *animator;
                driver.stop_moving(rig, AnimCtx::minimal());
                commands.entity(entity).remove::<WasMoving>();
            }
            _ => {}
        }
    }
}

/// Detect factory activation and drive the driver's
/// `activate`/`deactivate` hooks.
pub fn trigger_production_scripts(
    mut query: Query<(Entity, &mut UnitAnimator, &Producer, Has<WasActive>)>,
    mut commands: Commands,
) {
    for (entity, mut animator, producer, was_active) in &mut query {
        let is_active = producer.current_production().is_some();
        match (is_active, was_active) {
            (true, false) => {
                let UnitAnimator { rig, driver, .. } = &mut *animator;
                driver.activate(rig, AnimCtx::minimal());
                commands.entity(entity).insert(WasActive);
            }
            (false, true) => {
                let UnitAnimator { rig, driver, .. } = &mut *animator;
                driver.deactivate(rig, AnimCtx::minimal());
                commands.entity(entity).remove::<WasActive>();
            }
            _ => {}
        }
    }
}

/// Marker inserted by the combat system on the frame a unit fires.
/// Used solely as the trigger for the fire animation — aim (heading /
/// pitch / arc) is handled per-frame by `drive_aim_script`, so this is a
/// bare marker rather than carrying the target pose.
///
/// [`drive_aim_script`]: crate::units::combat::drive_aim_script
#[derive(Component, Default)]
#[component(storage = "SparseSet")]
pub struct JustFired;

/// When a unit has JustFired, drive its fire animation (muzzle flash /
/// recoil / barrel cycling).
///
/// `AimWeapon1` is **not** triggered here — [`drive_aim_script`]
/// runs it per-frame from [`AimTarget`] + entity transform so the
/// aim-ready gate applies *before* the shot.
///
/// [`drive_aim_script`]: crate::units::combat::drive_aim_script
/// [`AimTarget`]: crate::units::combat::AimTarget
pub fn trigger_weapon_scripts(
    mut query: Query<(Entity, &mut UnitAnimator, &JustFired), Without<Dying>>,
    mut commands: Commands,
) {
    for (entity, mut animator, _just_fired) in &mut query {
        let UnitAnimator { rig, driver, .. } = &mut *animator;
        driver.fire(rig, AnimCtx::minimal());
        commands.entity(entity).remove::<JustFired>();
    }
}

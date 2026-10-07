//! Bridges game events to per-unit animation drivers.
//!
//! Detects state changes (started moving, stopped moving, started
//! producing, fired a weapon) and drives the unit's animation driver —
//! the Rust-native replacement for the old COB script entry points
//! (`StartMoving`, `Activate`, `FireWeapon1`, ...).

use bevy::prelude::*;

use super::production::Producer;
use crate::interaction::ground_move::GroundMover;
use crate::interaction::movement::{AttackMoveActive, MovePath, MoveTarget, moving_for_script};
use crate::units::assets::animation::{AnimCtx, UnitAnimator};
use crate::units::combat::{AimTarget, Dying};

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
///
/// Ground units follow `CGroundMoveType::UpdateOwnerSpeed`: `StartMoving`
/// when the speed rises above 0.01, `StopMoving` when it falls back —
/// so a unit that halts mid-path (blocked, or holding to shoot on an
/// attack-move) gets its stop script, and a Pointer ordered off while
/// deployed starts folding the moment it rolls. Flyers, which have no
/// ground mover, keep the order-based rule.
#[allow(clippy::type_complexity)]
pub fn trigger_movement_scripts(
    mut query: Query<(
        Entity,
        &mut UnitAnimator,
        Option<&MoveTarget>,
        Option<&MovePath>,
        Has<AttackMoveActive>,
        Has<AimTarget>,
        Has<WasMoving>,
        Option<&GroundMover>,
    )>,
    mut commands: Commands,
) {
    for (entity, mut animator, move_target, move_path, attack_move, aiming, was_moving, mover) in
        &mut query
    {
        let is_moving = match mover {
            Some(m) => m.is_moving(),
            None => moving_for_script(
                move_target.is_some() || move_path.is_some(),
                attack_move,
                aiming,
            ),
        };
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

/// `CFactory::Update`: the yard closes only after this long without a
/// build step (`GAME_SPEED * (UNIT_SLOWUPDATE_RATE >> 1)` = 210 frames).
const DEACTIVATE_DELAY: f32 = 7.0;

/// Detect factory activation and drive the driver's
/// `activate`/`deactivate` hooks. Activation is immediate (the first
/// queued unit opens the yard); deactivation waits [`DEACTIVATE_DELAY`]
/// after the last build step, so a factory between two orders keeps its
/// arms out instead of folding and unfolding.
#[allow(clippy::type_complexity)]
pub fn trigger_production_scripts(
    mut query: Query<(
        Entity,
        &mut UnitAnimator,
        &Producer,
        Has<WasActive>,
        Has<crate::units::lifecycle::spawning::Emerging>,
    )>,
    mut commands: Commands,
) {
    for (entity, mut animator, producer, was_active, emerging) in &mut query {
        // `CFactory::Update` does nothing while `beingBuilt`: a spamming
        // minifac's queue opens its yard only once it stands finished.
        let is_active = !emerging
            && (producer.current_production().is_some()
                || (was_active && producer.idle_time < DEACTIVATE_DELAY));
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

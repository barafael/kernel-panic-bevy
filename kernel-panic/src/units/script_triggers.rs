//! Bridges game events to COB animation script calls.
//!
//! Detects state changes (started moving, stopped moving, started building,
//! attacking) and fires the corresponding COB script functions.

use std::f32::consts::PI;

use bevy::prelude::*;

use super::animation::CobAnimator;
use super::combat::{AttackCooldown, Dying};
use super::production::Producer;
use crate::interaction::movement::{MovePath, MoveTarget};

/// Tracks whether a unit was moving last frame, so we can detect transitions.
#[derive(Component, Default)]
pub struct MovementState {
    pub was_moving: bool,
}

/// Tracks whether a factory was actively building last frame.
#[derive(Component, Default)]
pub struct ProductionState {
    pub was_active: bool,
}

/// Detect movement start/stop and fire StartMoving/StopMoving COB scripts.
pub fn trigger_movement_scripts(
    mut query: Query<(
        Entity,
        &mut CobAnimator,
        Option<&MoveTarget>,
        Option<&MovePath>,
        Option<&mut MovementState>,
    )>,
    mut commands: Commands,
) {
    for (entity, mut animator, move_target, move_path, movement_state) in &mut query {
        let is_moving = move_target.is_some() || move_path.is_some();

        let was_moving = movement_state.as_ref().map_or(false, |s| s.was_moving);

        if is_moving && !was_moving {
            let cob = animator.cob.clone();
            animator.vm.start_script(&cob, "StartMoving", &[]);
        } else if !is_moving && was_moving {
            let cob = animator.cob.clone();
            animator.vm.start_script(&cob, "StopMoving", &[]);
        }

        match movement_state {
            Some(mut state) => state.was_moving = is_moving,
            None => {
                commands.entity(entity).insert(MovementState {
                    was_moving: is_moving,
                });
            }
        }
    }
}

/// Detect factory activation and fire Activate/Deactivate COB scripts.
pub fn trigger_production_scripts(
    mut query: Query<(
        Entity,
        &mut CobAnimator,
        &Producer,
        Option<&mut ProductionState>,
    )>,
    mut commands: Commands,
) {
    for (entity, mut animator, producer, production_state) in &mut query {
        // A factory is "active" if it's making progress (always true for now since
        // production is continuous, but will matter when build queues can be paused).
        let is_active = producer.progress_fraction() > 0.0;

        let was_active = production_state.as_ref().map_or(false, |s| s.was_active);

        if is_active && !was_active {
            let cob = animator.cob.clone();
            animator.vm.start_script(&cob, "Activate", &[]);
        } else if !is_active && was_active {
            let cob = animator.cob.clone();
            animator.vm.start_script(&cob, "Deactivate", &[]);
        }

        match production_state {
            Some(mut state) => state.was_active = is_active,
            None => {
                commands.entity(entity).insert(ProductionState {
                    was_active: is_active,
                });
            }
        }
    }
}

/// Marker inserted by the combat system on the frame a unit fires.
#[derive(Component)]
pub struct JustFired {
    /// World-space position of the target being fired at.
    pub target_pos: Vec3,
}

/// When a unit has JustFired, call AimWeapon1 and FireWeapon1 on the COB VM.
pub fn trigger_weapon_scripts(
    mut query: Query<(Entity, &mut CobAnimator, &GlobalTransform, &JustFired), Without<Dying>>,
    mut commands: Commands,
) {
    for (entity, mut animator, attacker_gtf, just_fired) in &mut query {
        let attacker_pos = attacker_gtf.translation();
        let to_target = just_fired.target_pos - attacker_pos;

        // Convert world-space direction to Spring heading (Y-axis rotation)
        // and pitch (X-axis elevation). COB uses angular units (65536 = 360°).
        let heading_rad = to_target.x.atan2(to_target.z);
        let horizontal_dist = (to_target.x * to_target.x + to_target.z * to_target.z).sqrt();
        let pitch_rad = (-to_target.y).atan2(horizontal_dist);

        let heading = (heading_rad / (2.0 * PI) * 65536.0) as i32;
        let pitch = (pitch_rad / (2.0 * PI) * 65536.0) as i32;

        let cob = animator.cob.clone();
        // AimWeapon1(heading, pitch) — returns 1 if ready to fire.
        animator
            .vm
            .start_script(&cob, "AimWeapon1", &[heading, pitch]);
        animator.vm.start_script(&cob, "FireWeapon1", &[]);

        commands.entity(entity).remove::<JustFired>();
    }
}

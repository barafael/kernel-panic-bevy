pub mod ability;
pub mod air_movement;
pub mod cursor;
pub mod debug_movement;
pub mod ground_move;
pub mod movement;
#[cfg(test)]
mod movement_harness;
pub(crate) mod selection;
pub mod structures;

use bevy::ecs::schedule::ScheduleConfigs;
use bevy::ecs::system::ScheduleSystem;
use bevy::gizmos::config::GizmoConfigStore;
use bevy::prelude::*;

use ability::AbilityHotkeyPlugin;
use cursor::CursorPlugin;
use movement::{
    CommandLineGizmos, draw_selected_command_lines, ground_clamp_system, ground_collision_system,
    guard_follow_system, movement_system, orient_stationary_to_terrain, update_path_heat,
};
use selection::SelectionPlugin;

/// Wipe every order component an explicit player command supersedes.
///
/// Used by the replace-style command paths (attack, attack-ground,
/// guard) before inserting their new order component. Queue-promotion
/// and path-completion sites in the movement system intentionally clear
/// narrower subsets — don't switch them over.
///
/// Centralized because the previous six hand-rolled chains had drifted:
/// attack-ground left a stale `AttackMoveActive`/`PendingBuild` behind
/// and guard left a stale `ForcedTarget`, while their sibling attack
/// path cleared all three.
pub(crate) fn clear_orders<'a, 'w>(ec: &'a mut EntityCommands<'w>) -> &'a mut EntityCommands<'w> {
    clear_active_order(ec).remove::<crate::units::combat::ForcedTarget>()
}

/// [`clear_orders`] minus the manual (T) `ForcedTarget` designation,
/// which survives a positional order (Spring's set-target keeps firing
/// while the unit moves).
fn clear_active_order<'a, 'w>(ec: &'a mut EntityCommands<'w>) -> &'a mut EntityCommands<'w> {
    ec.remove::<movement::MoveTarget>()
        .remove::<movement::MovePath>()
        .remove::<movement::CommandQueue>()
        .remove::<crate::units::combat::AttackGroundOrder>()
        .remove::<crate::units::combat::AttackTargetOrder>()
        .remove::<movement::AttackMoveActive>()
        .remove::<movement::GuardTarget>()
        .remove::<crate::units::lifecycle::construction::PendingBuild>()
        .remove::<crate::units::mechanics::command_fire::PendingCommandFire>()
}

/// Make `cmd` the unit's active order: the order-specific components a
/// queued command installs when it is promoted, or a fresh command
/// installs over a cleared unit ([`replace_order`]). Leaves the queue
/// and any other order state alone.
pub(crate) fn install_command(ec: &mut EntityCommands, cmd: movement::QueuedCommand) {
    use crate::units::lifecycle::construction::PendingBuild;
    use movement::{AttackMoveActive, MoveTarget, QueuedCommand};
    match cmd {
        QueuedCommand::Move(pos) | QueuedCommand::Patrol(pos) => {
            ec.insert(MoveTarget(pos)).remove::<PendingBuild>();
        }
        QueuedCommand::AttackMove(pos) => {
            ec.insert((MoveTarget(pos), AttackMoveActive))
                .remove::<PendingBuild>();
        }
        QueuedCommand::AttackUnit { target, .. } => {
            // Explicit attack supersedes a manual (T) designation, and
            // the attack system owns movement from here — no
            // `MoveTarget`, so a finished leg can't re-route.
            ec.remove::<PendingBuild>()
                .remove::<MoveTarget>()
                .remove::<crate::units::combat::ForcedTarget>()
                .insert(crate::units::combat::AttackTargetOrder { target });
        }
        QueuedCommand::BuildAt { kind, site } => {
            ec.insert((MoveTarget(site), PendingBuild { kind, site }));
        }
    }
}

/// Replace a unit's orders with `cmd`: drop the current order, its
/// computed path and queue (keeping only a manual `ForcedTarget`), then
/// [`install_command`] behind a fresh empty queue.
pub(crate) fn replace_order(ec: &mut EntityCommands, cmd: movement::QueuedCommand) {
    clear_active_order(ec).insert(movement::CommandQueue::default());
    install_command(ec, cmd);
}

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            SelectionPlugin,
            CursorPlugin,
            AbilityHotkeyPlugin,
            crate::interaction::debug_movement::DebugMovementPlugin,
        ))
        .init_resource::<ground_move::PathStats>()
        .init_gizmo_group::<CommandLineGizmos>()
        .add_systems(Startup, configure_command_line_gizmos)
        // Unit motion is simulation: it runs on the fixed 30 Hz
        // clock alongside the gameplay chain, so movement speed and
        // separation are frame-rate-independent. Input/selection/UI
        // systems stay on variable dt in `Update` — their commands
        // land in the next sim tick (Bevy runs FixedUpdate before
        // Update within a frame).
        .add_systems(FixedUpdate, unit_motion_systems())
        // Command-line gizmos draw from `Update` on variable dt —
        // pure visuals. No ordering edge against `movement_system`
        // (that lives in `FixedUpdate` now); worst case the overlay
        // trails the moved units by one frame.
        .add_systems(Update, draw_selected_command_lines);
    }
}

/// The fixed-tick unit-motion systems, in order. [`InteractionPlugin`]
/// runs them on `FixedUpdate`; the headless movement harness runs the
/// same list.
pub(crate) fn unit_motion_systems() -> ScheduleConfigs<ScheduleSystem> {
    (
        guard_follow_system,
        // Buildings stamped / cleared before anyone paths.
        structures::update_structure_layer.before(movement_system),
        update_path_heat.before(movement_system),
        movement_system,
        // `smoothGround.UpdateSmoothMesh()` precedes the unit
        // updates in the engine's frame.
        crate::terrain::smooth_ground::update_smooth_ground.before(air_movement::hover_air_system),
        air_movement::hover_air_system.after(movement_system),
        // `HandleObjectCollisions` for every ground unit reads
        // the positions all movers reached this frame.
        ground_collision_system.after(movement_system),
        // Runs last so any Y drift introduced by the two
        // preceding systems is corrected in the same frame.
        ground_clamp_system.after(ground_collision_system),
        // Tilt idle units and buildings after clamping so
        // the slope normal is sampled at the final Y.
        orient_stationary_to_terrain.after(ground_clamp_system),
    )
        .into_configs()
}

/// Thin out the command-line gizmo group so the dashed move-order overlay
/// reads as a delicate trail rather than the default 2-px gizmo weight.
fn configure_command_line_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<CommandLineGizmos>();
    config.line.width = 1.0;
}

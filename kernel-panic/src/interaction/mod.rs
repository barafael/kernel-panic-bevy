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
    CommandLineGizmos, advance_command_queue, draw_selected_command_lines, ground_clamp_system,
    ground_collision_system, guard_follow_system, movement_system, orient_stationary_to_terrain,
};
use selection::SelectionPlugin;

/// Every component of a unit's active order (the table on
/// [`movement::OrderState`]) and its per-order state, as one bundle to
/// remove.
type ActiveOrder = (
    movement::MoveTarget,
    movement::AttackMoveActive,
    crate::units::combat::AttackTargetOrder,
    crate::units::combat::AttackGroundOrder,
    movement::GuardTarget,
    crate::units::lifecycle::construction::PendingBuild,
    crate::units::lifecycle::construction::Constructing,
    // The build order's footprint wait: a re-issued build starts a
    // fresh one (`CBuilderCAI`'s retry count is per command).
    crate::units::lifecycle::construction::BuildSiteWait,
    crate::units::mechanics::command_fire::PendingCommandFire,
    crate::units::mechanics::command_fire::NxCast,
);

/// Wipe the unit's orders: the active order, its path, the queue and a
/// manual (T) `ForcedTarget` (Spring's `CMD_STOP`, and what any
/// non-queued command does first: `StopBuild`, queue cleared).
///
/// Used by Stop and the replace-style command paths that install an
/// order component directly (casts, the bot); the queued-command paths
/// go through [`replace_order`].
pub(crate) fn clear_orders<'a, 'w>(ec: &'a mut EntityCommands<'w>) -> &'a mut EntityCommands<'w> {
    clear_active_order(ec).remove::<crate::units::combat::ForcedTarget>()
}

/// [`clear_orders`] minus the manual (T) `ForcedTarget` designation,
/// which survives a positional order (Spring's set-target keeps firing
/// while the unit moves).
fn clear_active_order<'a, 'w>(ec: &'a mut EntityCommands<'w>) -> &'a mut EntityCommands<'w> {
    ec.remove::<ActiveOrder>()
        .remove::<(movement::MovePath, movement::CommandQueue)>()
}

/// End the active order but keep the queue: the next queued command is
/// promoted on the next sim tick ([`advance_command_queue`]). Spring's
/// `CMD_STOP` pushed in front of the queue when a queued command
/// cancels the running one (`CCommandAI::GiveAllowedCommand`).
pub(crate) fn finish_active_order<'a, 'w>(
    ec: &'a mut EntityCommands<'w>,
) -> &'a mut EntityCommands<'w> {
    ec.remove::<ActiveOrder>().remove::<movement::MovePath>()
}

/// Make `cmd` the unit's active order: whatever is left of the previous
/// order goes (one active order at a time: a fight marker must not leak
/// into a promoted move), then the components `cmd` runs on are
/// inserted. Used when a queued command is promoted and, behind a
/// cleared unit, by [`replace_order`]. Leaves the queue and the computed
/// path alone: the mover follows the old path until the new one is
/// searched.
pub(crate) fn install_command(ec: &mut EntityCommands, cmd: movement::QueuedCommand) {
    use crate::units::combat::{AttackGroundOrder, AttackTargetOrder, ForcedTarget};
    use crate::units::lifecycle::construction::PendingBuild;
    use movement::{AttackMoveActive, GuardTarget, MoveTarget, QueuedCommand};
    ec.remove::<ActiveOrder>();
    match cmd {
        QueuedCommand::Move(pos) => {
            ec.insert(MoveTarget(pos));
        }
        // A patrol leg is an internal fight order
        // (`CMobileCAI::ExecutePatrol` pushes a `CMD_FIGHT` to the point).
        QueuedCommand::AttackMove(pos) | QueuedCommand::Patrol(pos) => {
            ec.insert((MoveTarget(pos), AttackMoveActive));
        }
        // The orders below steer the unit themselves (their systems
        // issue their own `MoveTarget`s), and an explicit attack or
        // guard supersedes a manual (T) designation.
        QueuedCommand::AttackUnit { target, .. } => {
            ec.remove::<ForcedTarget>()
                .insert(AttackTargetOrder { target });
        }
        QueuedCommand::AttackGround(pos) => {
            ec.remove::<ForcedTarget>()
                .insert(AttackGroundOrder { pos });
        }
        QueuedCommand::Guard { target, .. } => {
            ec.remove::<ForcedTarget>().insert(GuardTarget(target));
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
        .init_resource::<ground_move::PathQueue>()
        .init_gizmo_group::<CommandLineGizmos>()
        .add_systems(Startup, configure_command_line_gizmos)
        // Unit motion is simulation: it runs on the fixed 30 Hz
        // clock alongside the gameplay chain, so movement speed and
        // separation are frame-rate-independent. Input/selection/UI
        // systems stay on variable dt in `Update` — their commands
        // land in the next sim tick (Bevy runs FixedUpdate before
        // Update within a frame).
        //
        // Before the gameplay chain, and not merely alongside it: the
        // movers queue plain `insert`s (orders, paths) on live units,
        // and `process_deploy` / `process_enter` in Produce despawn
        // live units without the `Dying` stage. Unordered, both
        // buffers could meet at one sync point with the despawn
        // applied first, and an insert on a missing entity panics.
        // Ordered first, the motion commands are flushed before
        // Produce runs.
        .add_systems(
            FixedUpdate,
            unit_motion_systems().before(crate::units::GameplaySet::Produce),
        )
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
        // Orders that ended since the last tick (attack target died,
        // build finished, ...) hand over to the next queued command
        // before anyone moves.
        advance_command_queue
            .before(guard_follow_system)
            .before(movement_system),
        // Before the movers it shares paths with: unordered, the
        // executor picks the order (`headless` ambiguity check).
        guard_follow_system.before(movement_system),
        // Buildings stamped / cleared before anyone paths.
        structures::update_structure_layer.before(movement_system),
        movement_system,
        // `smoothGround.UpdateSmoothMesh()` precedes the unit
        // updates in the engine's frame.
        crate::terrain::smooth_ground::update_smooth_ground.before(air_movement::hover_air_system),
        air_movement::hover_air_system.after(movement_system),
        // `HandleObjectCollisions` for every ground unit reads
        // the positions all movers reached this frame.
        ground_collision_system
            .after(movement_system)
            .after(air_movement::hover_air_system),
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

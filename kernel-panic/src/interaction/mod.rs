pub mod ability;
pub mod cursor;
pub mod debug_movement;
pub mod movement;
pub(crate) mod selection;

use bevy::gizmos::config::GizmoConfigStore;
use bevy::prelude::*;

// Kept for the UI rewrite: the deleted `ui/hud/placement.rs` and other
// HUD modules referenced this re-export.
#[allow(unused_imports)]
pub use selection::Selected;

use ability::AbilityHotkeyPlugin;
use cursor::CursorPlugin;
use movement::{
    CommandLineGizmos, draw_selected_command_lines, ground_clamp_system, guard_follow_system,
    movement_system, orient_stationary_to_terrain, unit_separation_system, update_path_heat,
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
pub(crate) fn clear_orders<'a>(ec: &'a mut EntityCommands<'a>) -> &'a mut EntityCommands<'a> {
    ec.remove::<movement::MoveTarget>()
        .remove::<movement::MovePath>()
        .remove::<movement::CommandQueue>()
        .remove::<crate::units::combat::AttackGroundOrder>()
        .remove::<crate::units::combat::AttackTargetOrder>()
        .remove::<movement::AttackMoveActive>()
        .remove::<crate::units::combat::ForcedTarget>()
        .remove::<movement::GuardTarget>()
        .remove::<crate::units::lifecycle::construction::PendingBuild>()
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
        .init_gizmo_group::<CommandLineGizmos>()
        .add_systems(Startup, configure_command_line_gizmos)
        // Unit motion is simulation: it runs on the fixed 30 Hz
        // clock alongside the gameplay chain, so movement speed and
        // separation are frame-rate-independent. Input/selection/UI
        // systems stay on variable dt in `Update` — their commands
        // land in the next sim tick (Bevy runs FixedUpdate before
        // Update within a frame).
        .add_systems(
            FixedUpdate,
            (
                guard_follow_system,
                update_path_heat.before(movement_system),
                movement_system,
                unit_separation_system.after(movement_system),
                // Runs last so any Y drift introduced by the two
                // preceding systems is corrected in the same frame.
                ground_clamp_system.after(unit_separation_system),
                // Tilt idle units and buildings after clamping so
                // the slope normal is sampled at the final Y.
                orient_stationary_to_terrain.after(ground_clamp_system),
            ),
        )
        // Command-line gizmos draw from `Update` on variable dt —
        // pure visuals. No ordering edge against `movement_system`
        // (that lives in `FixedUpdate` now); worst case the overlay
        // trails the moved units by one frame.
        .add_systems(Update, draw_selected_command_lines);
    }
}

/// Thin out the command-line gizmo group so the dashed move-order overlay
/// reads as a delicate trail rather than the default 2-px gizmo weight.
fn configure_command_line_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<CommandLineGizmos>();
    config.line.width = 1.0;
}

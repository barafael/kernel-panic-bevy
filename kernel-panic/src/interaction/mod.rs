pub mod ability;
pub mod cursor;
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
    CommandLineGizmos, draw_selected_command_lines, ground_clamp_system, movement_system,
    orient_stationary_to_terrain, unit_separation_system,
};
use selection::SelectionPlugin;

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((SelectionPlugin, CursorPlugin, AbilityHotkeyPlugin))
            .init_gizmo_group::<CommandLineGizmos>()
            .add_systems(Startup, configure_command_line_gizmos)
            .add_systems(
                Update,
                (
                    movement_system,
                    unit_separation_system.after(movement_system),
                    // Runs last so any Y drift introduced by the two
                    // preceding systems is corrected in the same frame.
                    ground_clamp_system.after(unit_separation_system),
                    // Tilt idle units and buildings after clamping so
                    // the slope normal is sampled at the final Y.
                    orient_stationary_to_terrain.after(ground_clamp_system),
                    draw_selected_command_lines.after(movement_system),
                ),
            );
    }
}

/// Thin out the command-line gizmo group so the dashed move-order overlay
/// reads as a delicate trail rather than the default 2-px gizmo weight.
fn configure_command_line_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<CommandLineGizmos>();
    config.line.width = 1.0;
}

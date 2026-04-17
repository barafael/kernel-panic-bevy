pub mod cursor;
pub mod movement;
pub(crate) mod selection;

use bevy::gizmos::config::GizmoConfigStore;
use bevy::prelude::*;

pub use selection::Selected;

use cursor::CursorPlugin;
use movement::{
    CommandLineGizmos, draw_selected_command_lines, movement_system, unit_separation_system,
};
use selection::SelectionPlugin;

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((SelectionPlugin, CursorPlugin))
            .init_gizmo_group::<CommandLineGizmos>()
            .add_systems(Startup, configure_command_line_gizmos)
            .add_systems(
                Update,
                (
                    movement_system,
                    unit_separation_system.after(movement_system),
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

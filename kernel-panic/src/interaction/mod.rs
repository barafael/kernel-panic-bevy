pub mod cursor;
pub mod movement;
pub(crate) mod selection;

use bevy::prelude::*;

pub use selection::Selected;

use cursor::CursorPlugin;
use movement::{draw_selected_command_lines, movement_system, unit_separation_system};
use selection::SelectionPlugin;

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((SelectionPlugin, CursorPlugin))
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

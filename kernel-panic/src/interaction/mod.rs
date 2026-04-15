mod movement;
mod selection;

use bevy::prelude::*;

use movement::movement_system;
use selection::{
    DragState, RightDragPath, handle_right_click, handle_selection, spawn_selection_rings,
    update_hover, update_hover_ring,
};

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DragState>()
            .init_resource::<RightDragPath>()
            .add_systems(
                Update,
                (
                    // Hover must run before selection so click can read the hovered entity.
                    update_hover,
                    handle_selection.after(update_hover),
                    handle_right_click,
                    spawn_selection_rings,
                    update_hover_ring,
                    movement_system,
                ),
            );
    }
}

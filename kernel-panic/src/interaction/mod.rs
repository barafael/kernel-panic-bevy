mod movement;
mod selection;

use bevy::prelude::*;

use movement::movement_system;
use selection::{
    DragState, RightDragPath, SpawnMoveIndicatorsEvent, decay_move_indicators, handle_right_click,
    handle_selection, spawn_move_indicator_visuals, spawn_selection_rings, update_hover,
    update_hover_ring,
};

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DragState>()
            .init_resource::<RightDragPath>()
            .add_message::<SpawnMoveIndicatorsEvent>()
            .add_systems(
                Update,
                (
                    update_hover,
                    handle_selection.after(update_hover),
                    handle_right_click,
                    // These access Assets<Mesh> mutably, so they must run after handle_right_click.
                    spawn_selection_rings.after(handle_right_click),
                    spawn_move_indicator_visuals.after(handle_right_click),
                    update_hover_ring.after(handle_right_click),
                    movement_system,
                    decay_move_indicators,
                ),
            );
    }
}

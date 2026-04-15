mod movement;
mod selection;

use bevy::prelude::*;

pub use selection::Selected;

use movement::movement_system;
use selection::{
    DragState, RightDragPath, billboard_health_bars, decay_move_indicators, despawn_health_bars,
    handle_right_click, handle_selection, spawn_health_bars, update_health_bars, update_hover,
    update_unit_highlight,
};

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DragState>()
            .init_resource::<RightDragPath>()
            .add_systems(
                Update,
                (
                    update_hover,
                    handle_selection.after(update_hover),
                    handle_right_click,
                    update_unit_highlight
                        .after(update_hover)
                        .after(handle_selection),
                    spawn_health_bars.after(handle_selection),
                    despawn_health_bars.after(handle_selection),
                    update_health_bars.after(spawn_health_bars),
                    billboard_health_bars.after(update_health_bars),
                    movement_system,
                    decay_move_indicators,
                ),
            );
    }
}

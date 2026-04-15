mod movement;
mod selection;

use bevy::prelude::*;

use movement::movement_system;
use selection::{handle_right_click, handle_selection};

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (handle_selection, handle_right_click, movement_system),
        );
    }
}

mod hud;
pub mod minimap;

use bevy::prelude::*;

use hud::HudPlugin;
use minimap::MinimapPlugin;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((MinimapPlugin, HudPlugin));
    }
}

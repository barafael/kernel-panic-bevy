//! Player-facing UI: in-game HUD, minimap, placement preview.
//!
//! Composed from sub-plugins under [`hud`] and [`minimap`]. World-space
//! overlays (health bars, selection rings, command-line gizmos) live in
//! `interaction::selection` — they're more game-state than UI and predate
//! this module's rebuild.

mod hud;
mod menu;
pub mod minimap;
pub mod theme;

use bevy::prelude::*;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((hud::HudPlugin, menu::MenuPlugin, minimap::MinimapPlugin));
    }
}

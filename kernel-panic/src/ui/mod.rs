//! Player-facing UI: in-game HUD, minimap, placement preview.
//!
//! Composed from sub-plugins under [`hud`] and [`minimap`]. World-space
//! overlays (health bars, selection rings, command-line gizmos) live in
//! `interaction::selection` — they're more game-state than UI and predate
//! this module's rebuild.

pub(crate) mod hud;
pub(crate) mod menu;
#[cfg(not(target_arch = "wasm32"))]
mod menu_shots;
pub mod minimap;
pub mod theme;

use bevy::prelude::*;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((hud::HudPlugin, menu::MenuPlugin, minimap::MinimapPlugin));
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins(menu_shots::MenuShotsPlugin);
    }
}

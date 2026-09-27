//! Player-facing UI: in-game HUD (command panel, build bar, tooltip,
//! placement preview), minimap, menus.
//!
//! Composed from sub-plugins under [`hud`] and [`minimap`]. World-space
//! overlays (health bars, selection rings, command-line gizmos) live in
//! `interaction::selection` — they're more game-state than UI and predate
//! this module's rebuild.

#[cfg(not(target_arch = "wasm32"))]
mod game_shots;
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
        app.add_plugins((menu_shots::MenuShotsPlugin, game_shots::GameShotsPlugin));
    }
}

/// Save a screenshot of the primary window to `path` (the dev
/// screenshot tools).
#[cfg(not(target_arch = "wasm32"))]
fn save_screenshot(commands: &mut Commands, path: std::path::PathBuf) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
}

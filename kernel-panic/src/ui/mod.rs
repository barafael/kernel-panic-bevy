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
#[cfg(not(target_arch = "wasm32"))]
mod record;
pub mod theme;

use bevy::prelude::*;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((hud::HudPlugin, menu::MenuPlugin, minimap::MinimapPlugin));
        #[cfg(not(target_arch = "wasm32"))]
        app.add_plugins((
            menu_shots::MenuShotsPlugin,
            game_shots::GameShotsPlugin,
            record::RecordPlugin,
        ))
        .add_systems(
            Update,
            periodic_shots.run_if(|d: Res<crate::game_setup::DevOptions>| {
                d.shot_every.is_some() && d.shot_dir.is_some()
            }),
        );
    }
}

/// `KP_SHOT_EVERY` / `KP_SHOT_DIR`: a screenshot every N frames.
#[cfg(not(target_arch = "wasm32"))]
fn periodic_shots(
    dev: Res<crate::game_setup::DevOptions>,
    mut frame: Local<u32>,
    mut commands: Commands,
) {
    *frame += 1;
    if let (Some(every), Some(dir)) = (dev.shot_every, &dev.shot_dir)
        && every > 0
        && *frame % every == 0
    {
        save_screenshot(&mut commands, dir.join(format!("shot_{:05}.png", *frame)));
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

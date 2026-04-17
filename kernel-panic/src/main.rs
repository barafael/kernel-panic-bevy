mod interaction;
mod map_loading;
mod rendering;
mod terrain;
mod ui;
mod units;

use bevy::prelude::*;

use interaction::InteractionPlugin;
use map_loading::MapLoadingPlugin;
use rendering::RenderingPlugin;
use terrain::TerrainPlugin;
use ui::UiPlugin;
use units::UnitsPlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Kernel Panic".to_string(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins((
            RenderingPlugin,
            InteractionPlugin,
            UiPlugin,
            UnitsPlugin,
            TerrainPlugin,
            MapLoadingPlugin,
        ))
        .run();
}

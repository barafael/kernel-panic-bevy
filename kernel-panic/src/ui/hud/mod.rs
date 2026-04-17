//! In-game HUD: info panel, build menu, order palette, and unit preview cache.
//!
//! Each sub-module is a self-contained `Plugin`; `HudPlugin` just wires them
//! together and owns the one shared resource (`UnitPreviews`).

mod build_menu;
mod info_panel;
mod order_palette;
mod placement;
mod previews;
mod style;

use bevy::prelude::*;

use build_menu::BuildMenuPlugin;
use info_panel::InfoPanelPlugin;
use order_palette::OrderPalettePlugin;
use placement::PlacementPlugin;
use previews::PreviewsPlugin;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            PreviewsPlugin,
            InfoPanelPlugin,
            BuildMenuPlugin,
            OrderPalettePlugin,
            PlacementPlugin,
        ));
    }
}

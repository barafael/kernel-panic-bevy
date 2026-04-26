//! In-game HUD panels: build menu, info panel, order palette, placement preview.

mod build_menu;
mod info_panel;
mod order_palette;
mod placement;
mod previews;

use bevy::prelude::*;

pub(super) struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            previews::PreviewsPlugin,
            build_menu::BuildMenuPlugin,
            info_panel::InfoPanelPlugin,
            order_palette::OrderPalettePlugin,
            placement::PlacementPlugin,
        ));
    }
}

//! In-game HUD: the Spring-style command panel, Kernel Panic's build bar,
//! the bottom-left tooltip box, building placement and the datavent
//! highlight shown while placing.

mod build_bar;
pub(crate) mod command_panel;
mod geo_highlight;
pub(crate) mod placement;
mod previews;
pub(crate) mod tooltip;

use bevy::prelude::*;

pub(super) struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            previews::PreviewsPlugin,
            command_panel::CommandPanelPlugin,
            build_bar::BuildBarPlugin,
            tooltip::TooltipPlugin,
            placement::PlacementPlugin,
            geo_highlight::GeoHighlightPlugin,
        ));
    }
}

//! In-game HUD: the Spring-style command panel, Kernel Panic's build bar,
//! the bottom-left tooltip box, building placement, the datavent
//! highlight shown while placing, the pause / speed line and the TAKE
//! button. [`widgets`] holds the scaffolding the
//! immediate-mode widgets share.

mod build_bar;
pub(crate) mod command_panel;
mod geo_highlight;
mod match_status;
pub(crate) mod placement;
mod previews;
mod take_button;
pub(crate) mod tooltip;
mod widgets;

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
            match_status::MatchStatusPlugin,
            take_button::TakeButtonPlugin,
        ));
    }
}

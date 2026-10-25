//! In-game HUD: the Spring-style command panel, Kernel Panic's build bar,
//! the bottom-left tooltip box, building placement, the datavent
//! highlight shown while placing, the Network's buffer line, the pause /
//! speed line, the TAKE
//! button, the automatic tip dispenser, in a networked match the player list and network banner,
//! and the replay viewer's bar. [`widgets`] holds the scaffolding the
//! immediate-mode widgets share.

mod buffer_line;
mod build_bar;
pub(crate) mod command_panel;
mod geo_highlight;
mod match_status;
mod net_overlay;
pub(crate) mod placement;
mod previews;
mod replay_bar;
mod take_button;
mod tips;
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
            buffer_line::BufferLinePlugin,
            tooltip::TooltipPlugin,
            placement::PlacementPlugin,
            geo_highlight::GeoHighlightPlugin,
            match_status::MatchStatusPlugin,
            take_button::TakeButtonPlugin,
            tips::TipsPlugin,
            net_overlay::NetOverlayPlugin,
            replay_bar::ReplayBarPlugin,
        ));
    }
}

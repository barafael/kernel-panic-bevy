//! Unit selection, highlighting, right-click movement orders, and health bars.
//!
//! Sub-modules are composed here; only `Selected` and `SelectionPlugin` leak out.

mod core;
mod groups;
mod health_bars;
mod highlight;
pub(crate) mod right_click;

use bevy::prelude::*;

pub(crate) use self::core::Hovered;
pub use self::core::Selected;
pub(crate) use self::core::SelectionSet;
pub(crate) use self::core::ground_hit;
pub(crate) use self::core::ground_hit_filtered;
pub(crate) use self::core::unit_hit;
pub(crate) use self::right_click::apply_ordered_command;
pub(crate) use self::right_click::{OrderMarker, PendingMoveIndicators};

use self::core::SelectionCorePlugin;
use self::groups::UnitGroupsPlugin;
use health_bars::HealthBarsPlugin;
use highlight::HighlightPlugin;
use right_click::RightClickPlugin;

pub struct SelectionPlugin;

impl Plugin for SelectionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            SelectionCorePlugin,
            UnitGroupsPlugin,
            RightClickPlugin,
            HighlightPlugin,
            HealthBarsPlugin,
        ));
    }
}

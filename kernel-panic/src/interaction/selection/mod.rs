//! Unit selection, highlighting, right-click movement orders, and health bars.
//!
//! Sub-modules are composed here; only `Selected` and `SelectionPlugin` leak out.

mod core;
mod groups;
mod health_bars;
mod highlight;
mod right_click;

use bevy::prelude::*;

pub(super) use self::core::Hovered;
pub use self::core::Selected;
// Kept for the UI rewrite: the deleted `ui/hud/placement.rs` referenced
// these and the new UI module is expected to need them again.
#[allow(unused_imports)]
pub(crate) use self::core::SelectionSet;
pub(crate) use self::core::ground_hit;
pub(crate) use self::right_click::apply_ordered_command;

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

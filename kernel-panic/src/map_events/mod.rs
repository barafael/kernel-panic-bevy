//! Per-map scripted events: Hex Farm's towers ([`hex_farm`]), the port's
//! shifting terrain ([`shifting`]) and the Circular Buffers' fast lanes
//! ([`lane_flow`]).
//!
//! Each is gated on a resource that [`map_loading`](crate::map_loading)
//! inserts only for matching maps; when it is absent, every system
//! short-circuits — no cost on other maps.

use bevy::prelude::*;

pub mod hex_farm;
pub mod lane_flow;
pub mod shifting;
pub use lane_flow::LaneFlow;
pub use shifting::ShiftingTerrain;

pub struct MapEventsPlugin;

impl Plugin for MapEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((hex_farm::HexFarmPlugin, shifting::ShiftingPlugin));
    }
}

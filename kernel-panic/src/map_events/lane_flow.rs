//! The Circular Buffer maps' directional fast lanes
//! ([`spring_map::ring`]): on the inner lane travel clockwise is fast and
//! counter-clockwise slow, on the outer lane the other way round, and
//! off the lanes nothing changes.
//!
//! Activation is gated on the [`LaneFlow`] resource being present (the
//! map loader inserts it for a `circular_buffer_<n>_v1` map).
//! [`crate::interaction::ground_move`] scales each ground unit's step by
//! [`LaneFlow::step_multiplier`]. Absent → no cost.

use bevy::prelude::*;
use spring_map::ring::Ring;

#[derive(Resource, Clone, Debug)]
pub struct LaneFlow(pub Ring);

impl LaneFlow {
    /// The flow of the map with this file stem, if any.
    pub fn for_map(map_name: &str) -> Option<Self> {
        Ring::for_map(map_name).map(Self)
    }

    /// Multiplier applied to a unit's step from `pos` heading `forward`.
    pub fn step_multiplier(&self, pos: Vec3, forward: Vec3) -> f32 {
        self.0
            .step_multiplier((pos.x, pos.z), (forward.x, forward.z))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A point on the north edge's inner or outer lane of the square ring.
    fn on_lane(ring: &Ring, lane: usize) -> Vec3 {
        let c = ring.centre();
        let d = (ring.lanes[lane].from + ring.lanes[lane].to) * 0.5;
        Vec3::new(c.0, 0.0, c.1 - d)
    }

    #[test]
    fn inner_lane_runs_clockwise_outer_counter() {
        let flow = LaneFlow::for_map("circular_buffer_4_v1").unwrap();
        let east = Vec3::X;
        // North edge: clockwise (seen on the minimap) is east.
        assert!((flow.step_multiplier(on_lane(&flow.0, 0), east) - 1.6).abs() < 1e-4);
        assert!((flow.step_multiplier(on_lane(&flow.0, 0), -east) - 0.4).abs() < 1e-4);
        assert!((flow.step_multiplier(on_lane(&flow.0, 1), east) - 0.4).abs() < 1e-4);
        assert!((flow.step_multiplier(on_lane(&flow.0, 1), -east) - 1.6).abs() < 1e-4);
        // Across a lane: unchanged.
        assert!((flow.step_multiplier(on_lane(&flow.0, 0), Vec3::Z) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn other_maps_have_none() {
        assert!(LaneFlow::for_map("Dual_Core").is_none());
        assert!(LaneFlow::for_map("Circular_Buffer").is_none());
    }
}

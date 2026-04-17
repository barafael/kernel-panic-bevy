//! Team-scoped small-building counts shared by Kernel Boost and Flow
//! speed. Both consumers tolerate ~second-scale staleness, so we
//! refresh on a fixed cadence rather than every frame.

use bevy::prelude::*;
use std::collections::HashMap;

use super::combat::Dying;
use super::components::{TeamId, UnitType};

/// Seconds between refreshes of `SmallBuildingCounts`. Consumers
/// (Kernel Boost production scaling, Flow speed boost) read the cached
/// value at higher cadences, so the production-rate and Flow-speed
/// bonuses lag a new building by up to this much — far below human
/// perception.
const COUNT_REFRESH_INTERVAL: f32 = 0.25;

#[derive(Resource, Default)]
pub struct SmallBuildingCounts {
    counts: HashMap<u8, u32>,
    accumulated: f32,
}

impl SmallBuildingCounts {
    pub fn get(&self, team: u8) -> u32 {
        self.counts.get(&team).copied().unwrap_or(0)
    }
}

pub fn count_small_buildings(
    time: Res<Time>,
    buildings: Query<(&UnitType, &TeamId), Without<Dying>>,
    mut state: ResMut<SmallBuildingCounts>,
) {
    state.accumulated += time.delta_secs();
    if state.accumulated < COUNT_REFRESH_INTERVAL {
        return;
    }
    state.accumulated = 0.0;
    state.counts.clear();
    for (unit, team) in &buildings {
        if unit.0.is_small_building() {
            *state.counts.entry(team.0).or_default() += 1;
        }
    }
}

//! Per-frame team-scoped counts shared across gameplay systems.
//!
//! Kernel Boost (production speed) and Flow-speed scaling both need
//! the same "how many small buildings does this team own" number. This
//! module computes it once per frame into [`SmallBuildingCounts`] so
//! the consumers don't each rescan every building.

use bevy::prelude::*;
use std::collections::HashMap;

use super::combat::Dying;
use super::components::{TeamId, UnitType};

/// Per-team count of units for which `UnitKind::is_small_building` is
/// true. Consumers read this; only `count_small_buildings` mutates it.
#[derive(Resource, Default)]
pub struct SmallBuildingCounts(HashMap<u8, u32>);

impl SmallBuildingCounts {
    pub fn get(&self, team: u8) -> u32 {
        self.0.get(&team).copied().unwrap_or(0)
    }
}

/// System: refresh `SmallBuildingCounts` once per frame. Runs before
/// any consumer so downstream reads see a coherent snapshot.
pub fn count_small_buildings(
    buildings: Query<(&UnitType, &TeamId), Without<Dying>>,
    mut counts: ResMut<SmallBuildingCounts>,
) {
    counts.0.clear();
    for (unit, team) in &buildings {
        if unit.0.is_small_building() {
            *counts.0.entry(team.0).or_default() += 1;
        }
    }
}

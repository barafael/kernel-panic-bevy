//! Team-scoped small-building counts shared by Kernel Boost and Flow
//! speed. Both consumers tolerate ~second-scale staleness, but the
//! count itself is event-driven: incremented when a `UnitType` is
//! added, decremented when `Dying` is inserted on the same entity.
//! There is no per-frame full scan.
//!
//! Assumption: a small building's `UnitType` is only removed via the
//! `Dying` death pipeline. If a future code path despawns small
//! buildings without going through `Dying` (e.g. an explicit map-cycle
//! teardown), wire it through `Dying` first or extend this module to
//! observe `RemovedComponents<UnitType>`.

use bevy::prelude::*;
use std::collections::HashMap;

use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};

#[derive(Resource, Default)]
pub struct SmallBuildingCounts {
    counts: HashMap<u8, u32>,
}

impl SmallBuildingCounts {
    pub fn get(&self, team: u8) -> u32 {
        self.counts.get(&team).copied().unwrap_or(0)
    }

    fn bump(&mut self, team: u8) {
        *self.counts.entry(team).or_default() += 1;
    }

    fn drop(&mut self, team: u8) {
        if let Some(slot) = self.counts.get_mut(&team) {
            *slot = slot.saturating_sub(1);
        }
    }
}

/// Bumps the per-team count whenever a small-building `UnitType` is
/// added. Newly-spawned buildings (including those still under
/// construction) match upstream `kernelboost.lua::UnitFinished` closely
/// enough at our 0.25s+ consumer cadence — the divergence window is
/// at most one build cycle.
pub fn track_added_buildings(
    added: Query<(&UnitType, &TeamId), Added<UnitType>>,
    mut counts: ResMut<SmallBuildingCounts>,
) {
    for (unit, team) in &added {
        if unit.0.is_small_building() {
            counts.bump(team.0);
        }
    }
}

/// Drops the per-team count when a small building enters its death
/// pipeline. Mirrors upstream `kernelboost.lua::UnitDestroyed`.
pub fn track_dying_buildings(
    dying: Query<(&UnitType, &TeamId), Added<Dying>>,
    mut counts: ResMut<SmallBuildingCounts>,
) {
    for (unit, team) in &dying {
        if unit.0.is_small_building() {
            counts.drop(team.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::content::definitions::UnitKind;

    fn run_added(world: &mut World) {
        let mut sys = IntoSystem::into_system(track_added_buildings);
        sys.initialize(world);
        sys.run((), world)
            .expect("track_added_buildings system run");
    }

    fn run_dying(world: &mut World) {
        let mut sys = IntoSystem::into_system(track_dying_buildings);
        sys.initialize(world);
        sys.run((), world)
            .expect("track_dying_buildings system run");
    }

    #[test]
    fn empty_world_yields_zero() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        run_added(&mut world);
        let counts = world.resource::<SmallBuildingCounts>();
        assert_eq!(counts.get(0), 0);
        assert_eq!(counts.get(7), 0);
    }

    #[test]
    fn small_building_spawn_bumps_team() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        world.spawn((UnitType(UnitKind::Socket), TeamId(0)));
        world.spawn((UnitType(UnitKind::Window), TeamId(0)));
        world.spawn((UnitType(UnitKind::Bit), TeamId(0))); // not a building
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 2);
    }

    #[test]
    fn homebase_does_not_count() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        world.spawn((UnitType(UnitKind::Kernel), TeamId(0)));
        world.spawn((UnitType(UnitKind::Hole), TeamId(1)));
        run_added(&mut world);
        let counts = world.resource::<SmallBuildingCounts>();
        assert_eq!(counts.get(0), 0);
        assert_eq!(counts.get(1), 0);
    }

    #[test]
    fn dying_drops_count() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        let entity = world.spawn((UnitType(UnitKind::Port), TeamId(2))).id();
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(2), 1);

        world.entity_mut(entity).insert(Dying { timer: 1.0 });
        run_dying(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(2), 0);
    }

    #[test]
    fn drop_saturates_at_zero() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        // Insert Dying directly without ever counting the spawn —
        // simulates an entity that bypassed our Added<UnitType> system
        // (which shouldn't happen, but the saturating drop guards it).
        world.spawn((
            UnitType(UnitKind::Firewall),
            TeamId(0),
            Dying { timer: 1.0 },
        ));
        run_dying(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 0);
    }

    #[test]
    fn multi_team_isolation() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        world.spawn((UnitType(UnitKind::Socket), TeamId(0)));
        world.spawn((UnitType(UnitKind::Socket), TeamId(0)));
        world.spawn((UnitType(UnitKind::Socket), TeamId(1)));
        run_added(&mut world);
        let counts = world.resource::<SmallBuildingCounts>();
        assert_eq!(counts.get(0), 2);
        assert_eq!(counts.get(1), 1);
    }
}

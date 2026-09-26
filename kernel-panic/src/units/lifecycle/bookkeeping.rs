//! Team-scoped small-building counts shared by Kernel Boost and Flow
//! speed. Both consumers tolerate ~second-scale staleness, but the
//! count itself is event-driven: a building is counted once it is
//! *finished* (upstream `kernelboost.lua` / `network_flowspeed.lua`
//! count in `UnitFinished`), i.e. when it spawns without — or later
//! loses — its construction [`Emerging`]; it is uncounted when `Dying`
//! is inserted on a counted entity. There is no per-frame full scan:
//! only buildings still under construction are revisited.
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
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::spawning::Emerging;

/// Per-team number of live `kind` units, nanoframes included — the
/// same census as upstream `Spring.GetTeamUnitsByDefs`. Used by the
/// `UnitRestricted` caps; a scan, so call it on demand, not per frame
/// per unit.
pub fn team_kind_counts(
    kind: UnitKind,
    units: &Query<(&UnitType, &TeamId), Without<Dying>>,
) -> HashMap<u8, u32> {
    let mut counts = HashMap::new();
    for (unit, team) in units {
        if unit.0 == kind {
            *counts.entry(team.0).or_default() += 1;
        }
    }
    counts
}

/// O(1) count of live units (everything carrying a `UnitType`), used by
/// the factory spawn cap. Replaces an `iter().count()` over the whole
/// unit query on every spawn-threshold frame — a scan that only gets
/// more expensive as the battle grows.
#[derive(Resource, Default, Debug)]
pub struct TotalUnitCount(pub u32);

/// Bumps [`TotalUnitCount`] for each newly-added `UnitType`. Pairs with
/// [`track_dying_units`]; both lean on Bevy change detection, so the
/// counter is exact as long as every unit despawn passes through
/// `Dying` (the same lifecycle assumption the small-building counts
/// document).
pub fn track_added_units(
    added: Query<(), Added<UnitType>>,
    mut count: ResMut<TotalUnitCount>,
) {
    count.0 += added.iter().count() as u32;
}

/// Drops [`TotalUnitCount`] for each unit entering the death pipeline.
pub fn track_dying_units(dying: Query<(), Added<Dying>>, mut count: ResMut<TotalUnitCount>) {
    count.0 = count.0.saturating_sub(dying.iter().count() as u32);
}

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

/// A small building that has been added to [`SmallBuildingCounts`]
/// (it finished construction). Only these are uncounted on death, so a
/// building destroyed mid-construction never decrements the tally.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct CountedSmallBuilding;

/// A small building still under construction, awaiting its count.
/// Keeps [`track_finished_buildings`] off the full unit table.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct UncountedSmallBuilding;

/// Classifies each newly-added small building: one spawned already
/// finished (map preplacement, test worlds) is counted at once, one
/// spawned as a construction nanoframe (`Emerging`, inserted in the same
/// command flush as its `UnitType`) waits for [`track_finished_buildings`].
pub fn track_added_buildings(
    added: Query<(Entity, &UnitType, &TeamId, Has<Emerging>), Added<UnitType>>,
    mut counts: ResMut<SmallBuildingCounts>,
    mut commands: Commands,
) {
    for (entity, unit, team, emerging) in &added {
        if !unit.0.is_small_building() {
            continue;
        }
        if emerging {
            commands.entity(entity).insert(UncountedSmallBuilding);
        } else {
            counts.bump(team.0);
            commands.entity(entity).insert(CountedSmallBuilding);
        }
    }
}

/// Counts a small building once its construction completes (`Emerging`
/// removed) — upstream `kernelboost.lua::UnitFinished`. A building that
/// died mid-construction is skipped (and never counted).
#[allow(clippy::type_complexity)]
pub fn track_finished_buildings(
    finished: Query<
        (Entity, &TeamId),
        (
            With<UncountedSmallBuilding>,
            Without<Emerging>,
            Without<Dying>,
        ),
    >,
    mut counts: ResMut<SmallBuildingCounts>,
    mut commands: Commands,
) {
    for (entity, team) in &finished {
        counts.bump(team.0);
        commands
            .entity(entity)
            .remove::<UncountedSmallBuilding>()
            .insert(CountedSmallBuilding);
    }
}

/// Drops the per-team count when a *counted* small building enters its
/// death pipeline. Mirrors upstream `kernelboost.lua::UnitDestroyed`,
/// which only subtracts buildings its `UnitFinished` had added.
pub fn track_dying_buildings(
    dying: Query<&TeamId, (Added<Dying>, With<CountedSmallBuilding>)>,
    mut counts: ResMut<SmallBuildingCounts>,
) {
    for team in &dying {
        counts.drop(team.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::content::definitions::UnitKind;

    use crate::units::lifecycle::spawning::EmergeStyle;

    fn run_added(world: &mut World) {
        let mut sys = IntoSystem::into_system(track_added_buildings);
        sys.initialize(world);
        sys.run((), world)
            .expect("track_added_buildings system run");
        sys.apply_deferred(world);
    }

    fn run_finished(world: &mut World) {
        let mut sys = IntoSystem::into_system(track_finished_buildings);
        sys.initialize(world);
        sys.run((), world)
            .expect("track_finished_buildings system run");
        sys.apply_deferred(world);
    }

    fn run_dying(world: &mut World) {
        let mut sys = IntoSystem::into_system(track_dying_buildings);
        sys.initialize(world);
        sys.run((), world)
            .expect("track_dying_buildings system run");
    }

    fn nanoframe() -> Emerging {
        Emerging {
            target_y: 0.0,
            remaining: 10.0,
            total: 10.0,
            rally_point: None,
            style: EmergeStyle::Rise,
        }
    }

    /// A building spawned as a construction nanoframe is not counted
    /// until it finishes (`kernelboost.lua::UnitFinished`).
    #[test]
    fn nanoframe_counts_only_when_finished() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        let socket = world
            .spawn((UnitType(UnitKind::Socket), TeamId(0), nanoframe()))
            .id();
        run_added(&mut world);
        run_finished(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 0);

        world.entity_mut(socket).remove::<Emerging>();
        run_finished(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 1);
        // Idempotent: a finished building is counted once.
        run_finished(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 1);
    }

    /// Destroying a building mid-construction must not decrement the
    /// finished buildings' count (`UnitDestroyed` skips `beingBuilt`).
    #[test]
    fn nanoframe_destroyed_does_not_decrement() {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        world.spawn((UnitType(UnitKind::Window), TeamId(1)));
        let frame = world
            .spawn((UnitType(UnitKind::Window), TeamId(1), nanoframe()))
            .id();
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(1), 1);

        world.entity_mut(frame).insert(Dying { timer: 1.0 });
        run_dying(&mut world);
        // Its Emerging later expiring on the corpse must not count it.
        world.entity_mut(frame).remove::<Emerging>();
        run_finished(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(1), 1);
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
            CountedSmallBuilding,
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

//! Team-scoped small-building counts shared by Kernel Boost and Flow
//! speed. Both consumers tolerate ~second-scale staleness, but the
//! count itself is event-driven: a building is counted once it is
//! *finished* (upstream `kernelboost.lua` / `network_flowspeed.lua`
//! count in `UnitFinished`), i.e. when it spawns without — or later
//! loses — its construction [`Emerging`]; it is uncounted when `Dying`
//! is inserted on a counted entity. There is no per-frame full scan:
//! only buildings still under construction are revisited.
//!
//! Note: [`TotalUnitCount`] (below) observes `RemovedComponents<UnitType>`
//! directly, so it has no such lifecycle assumption. The small-building
//! tally still counts through the `Dying` insertion, since only
//! *finished* buildings were ever counted.

use bevy::prelude::*;
use std::collections::HashMap;

use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::WeaponRegistry;
use crate::units::lifecycle::spawning::Emerging;

/// Kamikaze unit (FBI `Kamikaze=1`, the Logic Bomb): detonates when an
/// enemy comes within `trigger_radius` elmos (`KamikazeDistance`).
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct Kamikaze {
    pub trigger_radius: f32,
}

/// FBI `IdleAutoHeal > 0`: regenerates `rate` HP/s once idle for
/// `threshold` seconds (`IdleTime` sim frames / 30).
#[derive(Component, Clone, Copy, Debug)]
pub struct IdleAutoHeal {
    pub rate: f32,
    pub threshold: f32,
}

/// FBI `RadarDistance > 0`: reveals cloaked enemies within `radius`.
#[derive(Component, Clone, Copy, Debug)]
pub struct Detector {
    pub radius: f32,
}

/// The kind's primary weapon never auto-acquires: unarmed (no weapon,
/// BuildLaser only, unregistered TDF, `range=0`) or command-fire (NX
/// Flag, Infection, …). `combat_system` skips these instead of walking
/// every building and Packet just to `continue`.
#[derive(Component, Clone, Copy, Debug)]
pub struct NoAutoTarget;

/// Stamps the per-kind markers above on every newly-added unit, from
/// the registries — the values are constant per kind, so the per-tick
/// systems query `With<Marker>` instead of resolving every unit's FBI
/// entry each tick. Runs at the head of Simulate so a unit whose spawn
/// flushed this tick is tagged before the consumers run.
pub fn tag_unit_kinds(
    added: Query<(Entity, &UnitType), Added<UnitType>>,
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
) {
    for (entity, unit) in &added {
        let kind = unit.0;
        let mut entity = commands.entity(entity);
        let kamikaze = unit_registry.kamikaze_distance(kind);
        if kamikaze > 0.0 {
            entity.insert(Kamikaze {
                trigger_radius: kamikaze,
            });
        }
        let heal = unit_registry.idle_auto_heal(kind);
        if heal > 0.0 {
            entity.insert(IdleAutoHeal {
                rate: heal,
                threshold: crate::sim::frames_to_secs(unit_registry.idle_time(kind)),
            });
        }
        let radar = unit_registry.radar_distance(kind);
        if radar > 0.0 {
            entity.insert(Detector { radius: radar });
        }
        // Same resolution `combat_system` applies: `unit_registry.weapon`
        // already filters BuildLaser; a name missing from the TDFs
        // resolves to no weapon (range 0).
        let name = unit_registry.weapon(kind);
        let auto_targets = !name.is_empty()
            && weapon_registry
                .get(name)
                .is_some_and(|w| w.range != 0.0 && !w.command_fire);
        if !auto_targets {
            entity.insert(NoAutoTarget);
        }
    }
}

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

/// Live units of `kind` on `team` — the single-team form of
/// [`team_kind_counts`], shared by the command panel's cap indicator and
/// the Mine Launcher's cap check so both agree with the enforcement.
pub fn team_kind_count(
    kind: UnitKind,
    team: u8,
    units: &Query<(&UnitType, &TeamId), Without<Dying>>,
) -> u32 {
    units
        .iter()
        .filter(|(unit, t)| unit.0 == kind && t.0 == team)
        .count() as u32
}

/// O(1) count of live units (everything carrying a `UnitType`), used by
/// the factory spawn cap. Replaces an `iter().count()` over the whole
/// unit query on every spawn-threshold frame — a scan that only gets
/// more expensive as the battle grows.
#[derive(Resource, Default, Debug)]
pub struct TotalUnitCount(pub u32);

/// Bumps [`TotalUnitCount`] for each newly-added `UnitType`. Pairs with
/// [`track_removed_units`].
pub fn track_added_units(added: Query<(), Added<UnitType>>, mut count: ResMut<TotalUnitCount>) {
    count.0 += added.iter().count() as u32;
}

/// Drops [`TotalUnitCount`] for each entity whose `UnitType` went away.
///
/// Observed via `RemovedComponents<UnitType>` rather than
/// `Added<Dying>` so the invariant is structural: every despawn site
/// counts, including the two that historically skipped the `Dying`
/// pipeline (deploy pair-swap, packet absorption — their old
/// hand-decrements are gone). No despawn path needs to remember
/// anything.
pub fn track_removed_units(
    mut removed: RemovedComponents<UnitType>,
    mut count: ResMut<TotalUnitCount>,
) {
    count.0 = count.0.saturating_sub(removed.read().count() as u32);
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
            rally_then: None,
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

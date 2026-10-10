//! Per-kind unit markers, and the team-scoped tallies: live units per
//! team ([`TeamUnitCounts`], the factory cap) and finished small
//! buildings per team ([`SmallBuildingCounts`], Kernel Boost and Flow
//! speed).
//!
//! Each tally is kept by a marker component's hooks, not by a system
//! watching for removals: the marker's `on_add` counts, its `on_remove`
//! uncounts. `RemovedComponents` and `Added<Dying>` read from
//! `FixedUpdate` lose events — removal messages are cleared once per
//! *rendered* frame, which can pass several times between sim ticks,
//! and a building that gets `Dying` is usually despawned in the same
//! tick, before any `Added<Dying>` reader runs. Hooks fire at the
//! command flush itself, in any schedule, and a despawn runs
//! `on_remove` for every component on every path. Adding is still
//! driven by `Added<UnitType>`, which is change-tick based and reliable.
//!
//! A small building is counted once it is *finished* (upstream
//! `kernelboost.lua` / `network_flowspeed.lua` count in `UnitFinished`),
//! i.e. when it spawns without — or later loses — its construction
//! [`Emerging`]; `Dying` drops the marker at the kill
//! (`UnitDestroyed`). There is no per-frame full scan: only buildings
//! still under construction are revisited.

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
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

/// Upstream's per-team unit ceiling: KP's `MaxUnits` mod option
/// (`EngineOptions.lua`, default 1000 — "how many units and buildings
/// a player is allowed to own at the same time"), which the engine
/// clamps to `MAX_UNITS / numTeams` (`TeamHandler.cpp:45`, 32000 / n:
/// never lower for KP's seat counts).
pub const MAX_UNITS_PER_TEAM: u32 = 1000;

/// Per-team number of live units (everything carrying a `UnitType`,
/// nanoframes and corpses still playing their death included) —
/// upstream `CTeam::numUnits`, bumped by `CUnitHandler::AddUnit` and
/// dropped only by `DeleteUnit` once the death script has finished.
/// O(1) for the factory spawn cap; maintained by [`CountedUnit`]'s
/// hooks, so every despawn path counts.
#[derive(Resource, Default, Debug)]
pub struct TeamUnitCounts {
    counts: HashMap<u8, u32>,
}

impl TeamUnitCounts {
    pub fn get(&self, team: u8) -> u32 {
        self.counts.get(&team).copied().unwrap_or(0)
    }

    /// `CTeam::AtUnitLimit`: a factory keeps its order but builds
    /// nothing while this holds (`CFactory::QueueBuild` →
    /// `FACTORY_KEEP_BUILD_ORDER`).
    pub fn at_limit(&self, team: u8) -> bool {
        self.get(team) >= MAX_UNITS_PER_TEAM
    }
}

/// `+1` / `-1` on a per-team tally, saturating at zero.
fn tally(counts: &mut HashMap<u8, u32>, team: u8, add: bool) {
    let slot = counts.entry(team).or_default();
    *slot = if add {
        slot.saturating_add(1)
    } else {
        slot.saturating_sub(1)
    };
}

/// A unit counted in [`TeamUnitCounts`]. Its hooks are the whole
/// bookkeeping: `on_add` counts, `on_remove` uncounts — and a despawn
/// runs `on_remove` on every path (the death pipeline, a deploy
/// pair-swap, packet absorption, the map teardown), whatever schedule
/// it happens in. Not removed by `Dying`: upstream keeps counting a
/// unit until it is deleted.
#[derive(Component, Clone, Copy, Debug)]
#[component(on_add = CountedUnit::on_add, on_remove = CountedUnit::on_remove)]
pub struct CountedUnit {
    team: u8,
}

impl CountedUnit {
    fn on_add(world: DeferredWorld, ctx: HookContext) {
        Self::hook(world, ctx, true);
    }

    fn on_remove(world: DeferredWorld, ctx: HookContext) {
        Self::hook(world, ctx, false);
    }

    fn hook(mut world: DeferredWorld, ctx: HookContext, add: bool) {
        let Some(&Self { team }) = world.get::<Self>(ctx.entity) else {
            return;
        };
        if let Some(mut counts) = world.get_resource_mut::<TeamUnitCounts>() {
            tally(&mut counts.counts, team, add);
        }
    }
}

/// Tags each newly-added unit with [`CountedUnit`]. `Added<UnitType>`
/// is change-tick based, so no spawn is missed however many frames
/// pass between sim ticks; a unit despawned before this runs is simply
/// never counted (and so never uncounted).
#[allow(clippy::type_complexity)]
pub fn track_added_units(
    added: Query<(Entity, &TeamId), (Added<UnitType>, Without<CountedUnit>)>,
    mut commands: Commands,
) {
    for (entity, team) in &added {
        commands
            .entity(entity)
            .try_insert(CountedUnit { team: team.0 });
    }
}

#[derive(Resource, Default)]
pub struct SmallBuildingCounts {
    counts: HashMap<u8, u32>,
}

impl SmallBuildingCounts {
    pub fn get(&self, team: u8) -> u32 {
        self.counts.get(&team).copied().unwrap_or(0)
    }
}

/// A finished small building, counted in [`SmallBuildingCounts`]
/// (`kernelboost.lua::UnitFinished`). The hooks keep the tally: it is
/// uncounted when this marker goes — `Dying` drops it at the kill
/// (`UnitDestroyed`), a despawn without the death pipeline drops it
/// with the entity. A building destroyed mid-construction never
/// carried it, so it never decrements (`UnitDestroyed` skips
/// `beingBuilt`).
#[derive(Component, Clone, Copy, Debug)]
#[component(
    storage = "SparseSet",
    on_add = CountedSmallBuilding::on_add,
    on_remove = CountedSmallBuilding::on_remove
)]
pub struct CountedSmallBuilding {
    team: u8,
}

impl CountedSmallBuilding {
    fn on_add(world: DeferredWorld, ctx: HookContext) {
        Self::hook(world, ctx, true);
    }

    fn on_remove(world: DeferredWorld, ctx: HookContext) {
        Self::hook(world, ctx, false);
    }

    fn hook(mut world: DeferredWorld, ctx: HookContext, add: bool) {
        let Some(&Self { team }) = world.get::<Self>(ctx.entity) else {
            return;
        };
        if let Some(mut counts) = world.get_resource_mut::<SmallBuildingCounts>() {
            tally(&mut counts.counts, team, add);
        }
    }
}

/// A small building still under construction, awaiting its count.
/// Keeps [`track_finished_buildings`] off the full unit table; `Dying`
/// drops it, so a corpse whose `Emerging` lapses is never counted.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct UncountedSmallBuilding;

/// Classifies each newly-added small building: one spawned already
/// finished (map preplacement, test worlds) is counted at once, one
/// spawned as a construction nanoframe (`Emerging`, inserted in the same
/// command flush as its `UnitType`) waits for [`track_finished_buildings`].
pub fn track_added_buildings(
    added: Query<(Entity, &UnitType, &TeamId, Has<Emerging>), Added<UnitType>>,
    mut commands: Commands,
) {
    for (entity, unit, team, emerging) in &added {
        if !unit.0.is_small_building() {
            continue;
        }
        if emerging {
            commands.entity(entity).try_insert(UncountedSmallBuilding);
        } else {
            commands
                .entity(entity)
                .try_insert(CountedSmallBuilding { team: team.0 });
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
    mut commands: Commands,
) {
    for (entity, team) in &finished {
        commands
            .entity(entity)
            .try_remove::<UncountedSmallBuilding>()
            .try_insert(CountedSmallBuilding { team: team.0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::content::definitions::UnitKind;

    use crate::units::lifecycle::spawning::EmergeStyle;

    fn run<M>(world: &mut World, system: impl IntoSystem<(), (), M>) {
        let mut sys = IntoSystem::into_system(system);
        sys.initialize(world);
        sys.run((), world).expect("bookkeeping system run");
        sys.apply_deferred(world);
    }

    fn run_added(world: &mut World) {
        run(world, track_added_buildings);
    }

    fn run_finished(world: &mut World) {
        run(world, track_finished_buildings);
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

    fn buildings_world() -> World {
        let mut world = World::new();
        world.init_resource::<SmallBuildingCounts>();
        world
    }

    /// A building spawned as a construction nanoframe is not counted
    /// until it finishes (`kernelboost.lua::UnitFinished`).
    #[test]
    fn nanoframe_counts_only_when_finished() {
        let mut world = buildings_world();
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
        let mut world = buildings_world();
        world.spawn((UnitType(UnitKind::Window), TeamId(1)));
        let frame = world
            .spawn((UnitType(UnitKind::Window), TeamId(1), nanoframe()))
            .id();
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(1), 1);

        world.entity_mut(frame).insert(Dying { timer: 1.0 });
        // Its Emerging later expiring on the corpse must not count it.
        world.entity_mut(frame).remove::<Emerging>();
        run_finished(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(1), 1);
        world.entity_mut(frame).despawn();
        assert_eq!(world.resource::<SmallBuildingCounts>().get(1), 1);
    }

    #[test]
    fn empty_world_yields_zero() {
        let mut world = buildings_world();
        run_added(&mut world);
        let counts = world.resource::<SmallBuildingCounts>();
        assert_eq!(counts.get(0), 0);
        assert_eq!(counts.get(7), 0);
    }

    #[test]
    fn small_building_spawn_bumps_team() {
        let mut world = buildings_world();
        world.spawn((UnitType(UnitKind::Socket), TeamId(0)));
        world.spawn((UnitType(UnitKind::Window), TeamId(0)));
        world.spawn((UnitType(UnitKind::Bit), TeamId(0))); // not a building
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 2);
    }

    #[test]
    fn homebase_does_not_count() {
        let mut world = buildings_world();
        world.spawn((UnitType(UnitKind::Kernel), TeamId(0)));
        world.spawn((UnitType(UnitKind::Hole), TeamId(1)));
        run_added(&mut world);
        let counts = world.resource::<SmallBuildingCounts>();
        assert_eq!(counts.get(0), 0);
        assert_eq!(counts.get(1), 0);
    }

    #[test]
    fn dying_drops_count() {
        let mut world = buildings_world();
        let entity = world.spawn((UnitType(UnitKind::Port), TeamId(2))).id();
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(2), 1);

        world.entity_mut(entity).insert(Dying { timer: 1.0 });
        assert_eq!(world.resource::<SmallBuildingCounts>().get(2), 0);
    }

    /// Why: the old `track_dying_buildings` read `Added<Dying>` at the
    /// head of the *next* tick's Produce, but `cleanup_dying` despawns a
    /// building without death choreography in the same tick `Dying` is
    /// inserted — so it never saw one, and every counted building that
    /// died kept boosting the Kernel and Flow for the rest of the match.
    /// Inserting `Dying` and despawning in one go must uncount exactly
    /// once.
    #[test]
    fn dying_and_despawned_in_one_tick_uncounts_once() {
        let mut world = buildings_world();
        let a = world.spawn((UnitType(UnitKind::Socket), TeamId(0))).id();
        world.spawn((UnitType(UnitKind::Socket), TeamId(0)));
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 2);

        let mut commands = world.commands();
        commands.entity(a).insert(Dying { timer: 0.0 });
        commands.entity(a).despawn();
        world.flush();
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 1);
    }

    /// A counted building despawned without the death pipeline (the
    /// Exploit packing up, the map teardown) is uncounted too.
    #[test]
    fn despawn_without_dying_drops_count() {
        let mut world = buildings_world();
        let entity = world.spawn((UnitType(UnitKind::Firewall), TeamId(0))).id();
        run_added(&mut world);
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 1);
        world.entity_mut(entity).despawn();
        assert_eq!(world.resource::<SmallBuildingCounts>().get(0), 0);
    }

    #[test]
    fn multi_team_isolation() {
        let mut world = buildings_world();
        world.spawn((UnitType(UnitKind::Socket), TeamId(0)));
        world.spawn((UnitType(UnitKind::Socket), TeamId(0)));
        world.spawn((UnitType(UnitKind::Socket), TeamId(1)));
        run_added(&mut world);
        let counts = world.resource::<SmallBuildingCounts>();
        assert_eq!(counts.get(0), 2);
        assert_eq!(counts.get(1), 1);
    }

    /// Why: `TotalUnitCount` was decremented from
    /// `RemovedComponents<UnitType>` read in `FixedUpdate`, whose
    /// messages are cleared every rendered frame — above ~60 fps most
    /// despawns were never seen while every spawn was counted, and once
    /// the drifting count passed the cap every factory stopped for the
    /// rest of the match. Every despawn path must uncount, regardless of
    /// when (or whether) a system runs afterwards; `Dying` alone must
    /// not (upstream counts a unit until it is deleted).
    #[test]
    fn team_unit_counts_follow_spawn_and_despawn() {
        let mut world = World::new();
        world.init_resource::<TeamUnitCounts>();
        let bit = world.spawn((UnitType(UnitKind::Bit), TeamId(0))).id();
        let byte = world.spawn((UnitType(UnitKind::Byte), TeamId(0))).id();
        let other = world.spawn((UnitType(UnitKind::Bit), TeamId(3))).id();
        run(&mut world, track_added_units);
        // Idempotent.
        run(&mut world, track_added_units);
        let counts = world.resource::<TeamUnitCounts>();
        assert_eq!((counts.get(0), counts.get(3)), (2, 1));

        world.entity_mut(bit).insert(Dying { timer: 1.0 });
        assert_eq!(world.resource::<TeamUnitCounts>().get(0), 2);
        world.entity_mut(bit).despawn();
        // Absorbed / pair-swapped: no `Dying`, still uncounted.
        world.entity_mut(byte).despawn();
        world.entity_mut(other).despawn();
        let counts = world.resource::<TeamUnitCounts>();
        assert_eq!((counts.get(0), counts.get(3)), (0, 0));
    }

    /// `CTeam::AtUnitLimit` is per team: one team at the cap doesn't
    /// stop another's factories.
    #[test]
    fn unit_limit_is_per_team() {
        let mut world = World::new();
        world.init_resource::<TeamUnitCounts>();
        for _ in 0..MAX_UNITS_PER_TEAM {
            world.spawn((UnitType(UnitKind::Bit), TeamId(1)));
        }
        world.spawn((UnitType(UnitKind::Bit), TeamId(2)));
        run(&mut world, track_added_units);
        let counts = world.resource::<TeamUnitCounts>();
        assert!(counts.at_limit(1));
        assert!(!counts.at_limit(2));
        assert!(!counts.at_limit(0));
    }
}

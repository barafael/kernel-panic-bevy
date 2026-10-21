//! The match boundary: what belongs to one match, and the single place
//! where one match's state is replaced by the next one's.
//!
//! A load is requested long before it lands (the map is read and
//! prepared off the main thread, see the module above), and the old
//! world keeps running in between. Everything match-level therefore
//! switches at the *swap* ([`super::finish_map_load`]), never at the
//! request:
//!
//! - **Entities.** The teardown despawns match content only: scene
//!   entities (anything with a `Transform`), UI nodes, and data-only
//!   match entities tagged [`MatchEntity`] — minus the camera and
//!   [`PersistentEntity`](super::PersistentEntity) trees. Engine
//!   bookkeeping entities (windows and monitors, picking pointers,
//!   gamepads, in-flight screenshots, resource / observer / system
//!   entities) carry none of these and are never touched.
//! - **Per-match resources.** A plugin declares a resource match-scoped
//!   with [`MatchScopeAppExt::init_match_resource`] where it would call
//!   `init_resource`; [`begin_match`] re-initialises every such resource
//!   at the swap. No hand-kept reset list to forget an entry in.
//! - **Seats, seed and game state.** The match's [`ActiveMatch`] setup,
//!   its [`MatchSeed`], the [`LocalTeam`], [`Alliances`], [`AiSeats`],
//!   [`AiDifficulty`] and the result panel's [`GameState`] are set from
//!   the prepared map's setup in [`begin_match`] — the old world is
//!   judged against its own seats until it is gone.

use std::any::TypeId;

use bevy::ecs::entity::EntityHashSet;
use bevy::ecs::observer::Observer;
use bevy::ecs::resource::IsResource;
use bevy::ecs::system::SystemIdMarker;
use bevy::prelude::*;

use crate::game_setup::{ActiveMatch, AiDifficulty, GameSetup, SPECTATOR_TEAM};
use crate::net::driver::{NetSession, NextNetMatch};
use crate::net::replay_log::NextReplay;
use crate::sim::MatchSeed;
use crate::units::ai::AiSeats;
use crate::units::lifecycle::game_over::GameState;
use crate::units::player::LocalTeam;
use crate::units::team_colors::TeamColors;
use crate::units::teams::Alliances;

/// Marker for match content that has neither a `Transform` nor a UI
/// `Node` — the batched effect particles drawn from plain data (CEG
/// particles, beams, smoke puffs). Make it a required component of such
/// a type (`#[require(MatchEntity)]`) so the teardown clears it with the
/// match; everything with a `Transform` or `Node` is match content
/// already.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct MatchEntity;

/// Resets of every match-scoped resource, registered through
/// [`MatchScopeAppExt::init_match_resource`].
#[derive(Resource, Default)]
pub(crate) struct MatchResources(Vec<(TypeId, ResetFn)>);

/// Re-initialises one match-scoped resource.
type ResetFn = fn(&mut World);

/// Declares per-match resources.
pub trait MatchScopeAppExt {
    /// `init_resource::<R>()`, and re-initialise `R` (from its
    /// `Default` / `FromWorld`) at every match swap. For state that
    /// belongs to one match: per-team tallies, queues of pending
    /// spawns / hits, timers phased to the match start.
    fn init_match_resource<R: Resource + FromWorld>(&mut self) -> &mut Self;
}

impl MatchScopeAppExt for App {
    fn init_match_resource<R: Resource + FromWorld>(&mut self) -> &mut Self {
        self.init_resource::<R>();
        let mut registry = self.world_mut().get_resource_or_init::<MatchResources>();
        if !registry.0.iter().any(|(id, _)| *id == TypeId::of::<R>()) {
            registry.0.push((TypeId::of::<R>(), reset_resource::<R>));
        }
        self
    }
}

fn reset_resource<R: Resource + FromWorld>(world: &mut World) {
    let fresh = R::from_world(world);
    world.insert_resource(fresh);
}

/// Switch the match-level state to the match built from `setup`: every
/// match-scoped resource back to its initial value, the seats, and a
/// fresh `Playing` state with the game-over panel re-armed. Called at
/// the swap, after the old world's entities are gone (despawn hooks
/// and observers may still have written to the old tallies).
pub(crate) fn begin_match(world: &mut World, setup: &GameSetup) {
    // The match that ends keeps its replay (`net::replay_log`), from its
    // session's turn log, before the resets below drop it.
    crate::net::replay_log::finish_match(world);
    // The seed first: the resets below seed the match's random streams
    // (the AI's dice, `SimRng`) from it.
    world.insert_resource(MatchSeed(setup.seed));
    // A networked match the lobby started (or a replay the Replays page
    // opened) waits for its own swap: the attract demo's load, landing
    // first, must not take it (the session resets below would).
    let (waiting, replay) = if setup.demo {
        (
            world.remove_resource::<NextNetMatch>(),
            world.remove_resource::<NextReplay>(),
        )
    } else {
        (None, None)
    };
    let resets: Vec<ResetFn> = world
        .get_resource::<MatchResources>()
        .map(|r| r.0.iter().map(|(_, reset)| *reset).collect())
        .unwrap_or_default();
    for reset in resets {
        reset(world);
    }
    if let Some(next) = waiting {
        world.insert_resource(next);
    }
    if let Some(next) = replay {
        world.insert_resource(next);
    }

    // The human seat (its owner: seat `i` is owner `i`) drives input,
    // the result panel and the fog/cloak perspective; with no human
    // seat (the menu's attract-mode demo) the local player spectates.
    // In a networked match it is the seat the session's roster gives
    // this peer (a spectator has none). The difficulty the menu picked
    // drives the AI.
    // A replay's viewer watches.
    let local_team = world
        .get_resource::<NetSession>()
        .and_then(|n| crate::net::driver::local_team(&n.session))
        .unwrap_or_else(|| {
            setup
                .players
                .iter()
                .position(|p| !p.ai)
                .map_or(SPECTATOR_TEAM, |seat| seat as u8)
        });
    world.insert_resource(LocalTeam(local_team));
    // Who is allied, and which owners the AI plays, come from the
    // seats, never from who is watching: every peer runs the same
    // alliances and AI brains.
    world.insert_resource(Alliances::from_setup(setup));
    world.insert_resource(TeamColors::from_setup(setup));
    world.insert_resource(AiSeats::from_setup(setup));
    world.insert_resource(AiDifficulty(setup.difficulty));
    world.insert_resource(ActiveMatch(setup.clone()));
    // A decided previous match must not leave its result panel over
    // the new one. Applied by this frame's state transition.
    if let Some(mut next) = world.get_resource_mut::<NextState<GameState>>() {
        next.set(GameState::Playing);
    }
}

/// The match teardown: despawn every match entity (see the module
/// doc), keep the camera and persistent UI trees and everything the
/// engine owns.
pub(crate) fn despawn_match_entities(world: &mut World) {
    let keep = kept_entities(world);
    // One bulk pass (0.20): a single deferred flush and one batched
    // allocator free for the whole teardown, instead of a per-root
    // recursive `despawn` (each with its own flush) — the visible hitch
    // at the match swap. Recursive like the old loop, and it tolerates
    // roots already gone with a despawned parent the same way the old
    // existence check did.
    world.despawn_all_where::<Entity, Or<(With<Transform>, With<Node>, With<MatchEntity>)>>(|e| {
        !keep.contains(&e)
    });

    let strays = stray_game_entities(world, &keep);
    if !strays.is_empty() {
        warn!(
            "{} game entities without Transform / Node / MatchEntity survived the match \
             teardown (tag their type `#[require(MatchEntity)]`): {}",
            strays.len(),
            strays.join("; ")
        );
    }
}

/// The camera and persistent UI roots with all their descendants.
fn kept_entities(world: &mut World) -> EntityHashSet {
    use crate::rendering::camera::RtsCamera;

    let mut roots =
        world.query_filtered::<Entity, Or<(With<RtsCamera>, With<super::PersistentEntity>)>>();
    let mut keep: EntityHashSet = roots.iter(world).collect();
    // One pass builds a parent → children map, then a DFS from the
    // roots.
    let mut children_of: bevy::platform::collections::HashMap<Entity, Vec<Entity>> =
        Default::default();
    let mut relations = world.query::<(Entity, &ChildOf)>();
    for (e, child_of) in relations.iter(world) {
        children_of.entry(child_of.parent()).or_default().push(e);
    }
    let mut stack: Vec<Entity> = keep.iter().copied().collect();
    while let Some(parent) = stack.pop() {
        for &child in children_of.get(&parent).into_iter().flatten() {
            if keep.insert(child) {
                stack.push(child);
            }
        }
    }
    keep
}

/// Entities outside the kept trees that still carry one of this
/// crate's components after the teardown: match content that forgot
/// its [`MatchEntity`] tag. Engine entities and the ECS's own resource,
/// observer and system entities are skipped. Relies on component type
/// names, so it only reports in builds that keep them (debug names).
fn stray_game_entities(world: &World, keep: &EntityHashSet) -> Vec<String> {
    let crate_prefix = concat!(env!("CARGO_CRATE_NAME"), "::");
    let ecs_own = [
        world.component_id::<IsResource>(),
        world.component_id::<Observer>(),
        world.component_id::<SystemIdMarker>(),
    ];
    let mut out = Vec::new();
    for archetype in world.archetypes().iter() {
        if archetype.is_empty()
            || archetype
                .components()
                .iter()
                .any(|id| ecs_own.contains(&Some(*id)))
        {
            continue;
        }
        let ours: Vec<String> = archetype
            .components()
            .iter()
            .filter_map(|id| world.components().get_info(*id))
            .map(|info| info.name().to_string())
            .filter(|name| name.starts_with(crate_prefix))
            .collect();
        if ours.is_empty() {
            continue;
        }
        for e in archetype.entities() {
            if !keep.contains(&e.id()) {
                out.push(format!("{} ({})", e.id(), ours.join(", ")));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map_loading::PersistentEntity;
    use crate::rendering::camera::RtsCamera;
    use bevy::input::gamepad::Gamepad;
    use bevy::picking::pointer::{PointerId, PointerMap};
    use bevy::window::{Monitor, OnMonitor, PrimaryMonitor};

    fn monitor() -> Monitor {
        Monitor {
            name: Some("test".into()),
            physical_height: 1080,
            physical_width: 1920,
            physical_position: IVec2::ZERO,
            refresh_rate_millihertz: Some(60_000),
            scale_factor: 1.0,
            video_modes: Vec::new(),
        }
    }

    /// The teardown clears match content but nothing the engine owns:
    /// bevy_winit's monitor (whose `HasWindows` is a `linked_spawn`
    /// relationship target — despawning it would take the primary
    /// window, and `ExitCondition::OnAllClosed` the app, with it),
    /// the window, bevy_picking's mouse pointer and gamepads.
    #[test]
    fn teardown_spares_engine_entities() {
        let mut world = World::new();
        world.init_resource::<PointerMap>();
        let monitor = world.spawn((monitor(), PrimaryMonitor)).id();
        let window = world.spawn((Window::default(), OnMonitor(monitor))).id();
        let pointer = world.spawn(PointerId::Mouse).id();
        let gamepad = world.spawn(Gamepad::default()).id();

        let camera = world.spawn((RtsCamera, Transform::default())).id();
        let camera_child = world.spawn((Transform::default(), ChildOf(camera))).id();
        let menu = world.spawn((PersistentEntity, Node::default())).id();
        let menu_child = world.spawn((Node::default(), ChildOf(menu))).id();

        let unit = world.spawn(Transform::default()).id();
        let unit_piece = world.spawn((Transform::default(), ChildOf(unit))).id();
        let hud = world.spawn(Node::default()).id();
        let particle = world.spawn(MatchEntity).id();

        despawn_match_entities(&mut world);

        for (e, what) in [
            (monitor, "monitor"),
            (window, "window"),
            (pointer, "mouse pointer"),
            (gamepad, "gamepad"),
            (camera, "camera"),
            (camera_child, "camera child"),
            (menu, "menu root"),
            (menu_child, "menu child"),
        ] {
            assert!(world.get_entity(e).is_ok(), "{what} kept");
        }
        for (e, what) in [
            (unit, "unit"),
            (unit_piece, "unit piece"),
            (hud, "HUD node"),
            (particle, "data-only match entity"),
        ] {
            assert!(world.get_entity(e).is_err(), "{what} despawned");
        }
    }

    #[derive(Resource, Default)]
    struct Probe(u32);

    /// The ECS's own entities survive too: resources, global observers
    /// and registered systems keep working afterwards.
    #[test]
    fn teardown_spares_resources_observers_and_systems() {
        let mut world = World::new();
        world.init_resource::<Probe>();
        world.add_observer(|_: On<Add<PersistentEntity>>, mut probe: ResMut<Probe>| {
            probe.0 += 1;
        });
        let system = world.register_system(|mut probe: ResMut<Probe>| probe.0 += 10);
        let game = world.spawn(Transform::default()).id();
        let menu = world.spawn(PersistentEntity).id();
        world.flush();

        despawn_match_entities(&mut world);

        assert!(world.get_entity(game).is_err(), "match entity despawned");
        assert!(world.get_entity(menu).is_ok(), "persistent entity kept");
        assert_eq!(world.resource::<Probe>().0, 1);
        world.spawn(PersistentEntity);
        world.run_system(system).expect("registered system kept");
        assert_eq!(world.resource::<Probe>().0, 12, "observer kept");
    }

    #[derive(Component)]
    struct Untagged;

    /// Game data without a Transform / Node / tag is reported, not
    /// silently carried into the next match.
    #[test]
    fn untagged_game_entity_is_reported() {
        let mut world = World::new();
        world.init_resource::<Probe>();
        let stray = world.spawn(Untagged).id();
        world.spawn(MatchEntity);
        despawn_match_entities(&mut world);
        let strays = stray_game_entities(&world, &EntityHashSet::default());
        assert_eq!(strays.len(), 1, "{strays:?}");
        assert!(strays[0].starts_with(&stray.to_string()));
    }

    #[derive(Resource)]
    struct Tally(u32);

    impl Default for Tally {
        fn default() -> Self {
            Self(5)
        }
    }

    struct TestStream;
    impl crate::sim::RngStream for TestStream {
        const SALT: u64 = 7;
    }

    fn lifecycle_app() -> App {
        use bevy::state::app::StatesPlugin;
        let mut app = App::new();
        app.add_plugins(StatesPlugin)
            .init_state::<GameState>()
            .init_match_resource::<Tally>()
            .init_match_resource::<crate::sim::SimTick>()
            .init_match_resource::<crate::sim::SimRng<TestStream>>()
            .insert_resource(LocalTeam(0))
            .insert_resource(AiDifficulty(2));
        app
    }

    fn spectated_demo() -> GameSetup {
        let mut setup = GameSetup {
            demo: true,
            difficulty: 4,
            ..GameSetup::default()
        };
        for p in &mut setup.players {
            p.ai = true;
        }
        setup
    }

    /// The swap re-initialises every match-scoped resource, seats the
    /// new match and closes a decided match's result before the first
    /// tick of the new one.
    #[test]
    fn begin_match_switches_match_state() {
        let mut app = lifecycle_app();
        app.world_mut()
            .resource_mut::<NextState<GameState>>()
            .set(GameState::Defeat);
        app.update();
        app.world_mut().resource_mut::<Tally>().0 = 99;
        app.world_mut().resource_mut::<crate::sim::SimTick>().0 = 4000;
        let setup = GameSetup {
            seed: 0xfeed,
            ..spectated_demo()
        };

        begin_match(app.world_mut(), &setup);
        app.update();

        let world = app.world();
        assert_eq!(world.resource::<Tally>().0, 5, "match resource reset");
        // The match's clock restarts, its random streams roll from its
        // seed, and the AI plays every seat, whoever watches.
        assert_eq!(world.resource::<crate::sim::SimTick>().0, 0);
        assert_eq!(world.resource::<MatchSeed>().0, 0xfeed);
        assert_eq!(
            **world.resource::<crate::sim::SimRng<TestStream>>(),
            MatchSeed(0xfeed).stream(7)
        );
        assert_eq!(world.resource::<AiSeats>().0, vec![0, 1]);
        assert_eq!(world.resource::<LocalTeam>().0, SPECTATOR_TEAM);
        assert_eq!(
            *world.resource::<Alliances>(),
            Alliances::from_setup(&setup)
        );
        assert_eq!(world.resource::<AiDifficulty>().0, 4);
        assert!(world.resource::<ActiveMatch>().0.demo);
        assert_eq!(
            *world.resource::<State<GameState>>().get(),
            GameState::Playing
        );
    }

    /// Requesting the next match changes nothing match-level: the old
    /// world keeps its seats and its (here: decided) state until the
    /// swap.
    #[test]
    fn requesting_a_match_leaves_the_running_one_alone() {
        let mut app = lifecycle_app();
        app.world_mut()
            .resource_mut::<NextState<GameState>>()
            .set(GameState::Defeat);
        app.update();
        app.insert_resource(spectated_demo())
            .insert_resource(super::super::MapCatalog(Vec::new()))
            .init_resource::<super::super::ReadyMap>();
        app.world_mut().resource_mut::<Tally>().0 = 99;

        super::super::prepare_game_entry(app.world_mut());
        app.update();

        let world = app.world();
        assert_eq!(world.resource::<LocalTeam>().0, 0);
        assert_eq!(world.resource::<AiDifficulty>().0, 2);
        assert_eq!(world.resource::<Tally>().0, 99);
        assert!(world.get_resource::<ActiveMatch>().is_none());
        assert_eq!(
            *world.resource::<State<GameState>>().get(),
            GameState::Defeat
        );
    }
}

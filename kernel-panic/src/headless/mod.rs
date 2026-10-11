//! Headless simulation harness for the network experiments
//! (`net-experiments/x-sim.md`).
//!
//! The whole game — every plugin `main` adds, through [`crate::add_game`]
//! — runs in an `App` with Bevy's `DefaultPlugins` minus the window,
//! the log and the audio output (the audio asset type and loader stay
//! registered), and with the renderer *disabled* rather than
//! removed: `RenderPlugin` gets `WgpuSettings { backends: None }`, so no
//! GPU device and no render sub-app exist, but every asset type
//! (`Mesh`, `Image`, `StandardMaterial`, the custom materials, shaders)
//! is still registered and `Assets<T>` work as plain CPU-side stores.
//! Gameplay code that builds meshes and materials (unit models, weapon
//! FX batches, terrain chunks, the minimap image) runs unchanged; the
//! assets are simply never uploaded. That is the whole "stub": nothing
//! in the game had to be cfg'd out. The asset server reads from
//! `kernel-panic/assets` as in the game.
//!
//! The clock is driven by hand: `TimeUpdateStrategy::ManualDuration`
//! advances virtual time by exactly one 30 Hz step per `App::update`, so
//! every [`SimHarness::step`] runs exactly one `FixedUpdate` tick (or a
//! chosen number of them, to mimic uneven render frames). While the map
//! loads (off-thread, like the game) the clock stands still, so the
//! match always starts at fixed-clock time 0.
//!
//! The match is the normal skirmish path: a [`GameSetup`] with only AI
//! seats, entered through `AppState::InGame`, loaded by `map_loading`
//! from `assets/maps` (a baked `.kpmap` when present).

pub mod state;
#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

use bevy::app::PluginsState;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::WgpuSettings;
use bevy::time::TimeUpdateStrategy;
use bevy::window::ExitCondition;

use crate::game_setup::{
    ActiveMatch, AppState, DevOptions, GameOverDismissed, GameSetup, PlayerSpec,
};
use crate::units::components::{Faction, Homebase};

/// One sim tick.
pub const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Which executor runs `FixedUpdate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Executor {
    Single,
    Multi,
}

/// A seeded all-AI match.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub map: String,
    pub seats: Vec<PlayerSpec>,
    pub seed: u64,
    pub executor: Executor,
    pub difficulty: u8,
}

impl Scenario {
    /// `teams` ally teams of `per_team` AI seats each, factions cycling
    /// System → Hacker → Network across the seats.
    pub fn ai_match(map: &str, teams: u8, per_team: u8, seed: u64) -> Self {
        let mut seats = Vec::new();
        for t in 0..teams {
            for p in 0..per_team {
                let i = (t * per_team + p) as usize;
                seats.push(PlayerSpec {
                    faction: Faction::ALL[i % 3],
                    team: t,
                    ai: true,
                });
            }
        }
        Self {
            map: map.to_string(),
            seats,
            seed,
            executor: Executor::Multi,
            difficulty: 2,
        }
    }
}

/// Wall-clock cost of the last fixed tick(s), stamped from `FixedFirst`
/// to `FixedLast`.
#[derive(Resource, Default)]
pub struct TickTimer {
    started: Option<Instant>,
    /// Durations of the ticks run since the last [`SimHarness::step`].
    pub ticks: Vec<Duration>,
}

fn tick_timer_start(mut t: ResMut<TickTimer>) {
    t.started = Some(Instant::now());
}

fn tick_timer_end(mut t: ResMut<TickTimer>) {
    if let Some(s) = t.started.take() {
        t.ticks.push(s.elapsed());
    }
}

/// Per-tick state hashes, recorded at the end of every tick (inside
/// `FixedLast`, so frames that run several ticks log each one).
#[derive(Resource, Default)]
pub struct HashLog {
    pub enabled: bool,
    pub ticks: Vec<TickHash>,
    /// Ticks (1-based) at which to also dump a readable per-unit state.
    pub dump_at: Vec<usize>,
    pub dumps: Vec<(usize, Vec<String>)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TickHash {
    pub sim: u64,
    pub fx: u64,
    /// Positions, health / flags, orders.
    pub parts: [u64; 3],
    pub units: u32,
    pub projectiles: u32,
}

fn log_hash(world: &mut World) {
    if !world.resource::<HashLog>().enabled {
        return;
    }
    let c = state::capture(world);
    let tick = world.resource::<HashLog>().ticks.len() + 1;
    if world.resource::<HashLog>().dump_at.contains(&tick) {
        let lines = state::describe(world);
        world.resource_mut::<HashLog>().dumps.push((tick, lines));
    }
    world.resource_mut::<HashLog>().ticks.push(TickHash {
        sim: c.sim_hash,
        fx: c.fx_hash,
        parts: c.part_hashes,
        units: c.units as u32,
        projectiles: c.projectiles as u32,
    });
}

/// The game app without a window or renderer, plugins finished, nothing
/// run yet.
pub fn build_app(executor: Executor) -> App {
    let mut app = App::new();
    let assets_dir = crate::paths::from_project_root("kernel-panic/assets");
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::log::LogPlugin>()
            .disable::<bevy::audio::AudioPlugin>()
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            })
            .set(RenderPlugin {
                render_creation: WgpuSettings {
                    backends: None,
                    ..default()
                }
                .into(),
                ..default()
            })
            .set(AssetPlugin {
                file_path: assets_dir.to_string_lossy().into_owned(),
                ..default()
            }),
    )
    .insert_resource(DevOptions::default())
    .insert_resource(crate::rendering::settings::RenderSettings::default());
    // No audio output (and no music playing through the speakers during
    // a test), but the asset type and its loader the game's audio
    // module loads its sound bank and playlist through.
    app.init_asset::<bevy::audio::AudioSource>()
        .init_asset_loader::<bevy::audio::AudioLoader>();
    crate::add_game(&mut app);
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO))
        .init_resource::<TickTimer>()
        .init_resource::<HashLog>()
        .add_systems(FixedFirst, tick_timer_start)
        .add_systems(
            FixedLast,
            (tick_timer_end, log_hash)
                .chain()
                .after(crate::rendering::interpolation::record_sim_pose),
        );
    if executor == Executor::Single {
        app.edit_schedule(FixedUpdate, |s| {
            s.set_executor(bevy::ecs::schedule::SingleThreadedExecutor::new());
        });
    }
    while app.plugins_state() == PluginsState::Adding {
        bevy::tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();
    app
}

pub struct SimHarness {
    pub app: App,
    /// Fixed ticks run since the match started.
    pub tick: u64,
    pub scenario: Scenario,
    /// Wall time of the last `step`'s `App::update` (whole frame).
    pub last_frame: Duration,
}

impl SimHarness {
    /// Build the app, load the scenario's map and spawn the match. The
    /// first [`Self::step`] runs the match's first tick.
    pub fn new(scenario: Scenario) -> Self {
        crate::game_setup::test_seed::set(Some(scenario.seed));
        let mut app = build_app(scenario.executor);

        // Startup (map catalog, the menu's boot demo request) …
        app.update();
        // … then our match instead: the InGame entry supersedes the
        // demo's pending load.
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::InGame);
        enter_match(&mut app, &scenario, false);
        Self {
            app,
            tick: 0,
            scenario,
            last_frame: Duration::ZERO,
        }
    }

    /// One render frame that advances the clock by `ticks` sim ticks
    /// (0 = a frame with no tick).
    pub fn frame(&mut self, ticks: u32) -> u32 {
        self.app
            .world_mut()
            .resource_mut::<TickTimer>()
            .ticks
            .clear();
        self.app
            .insert_resource(TimeUpdateStrategy::ManualDuration(TICK * ticks));
        let t0 = Instant::now();
        self.app.update();
        self.last_frame = t0.elapsed();
        let ran = self.app.world().resource::<TickTimer>().ticks.len() as u32;
        self.tick += ran as u64;
        ran
    }

    /// One frame, one tick.
    pub fn step(&mut self) {
        let ran = self.frame(1);
        assert_eq!(ran, 1, "a 1/30 s frame must run exactly one tick");
    }

    /// Wall time of the tick(s) the last frame ran.
    pub fn take_tick_times(&mut self) -> Vec<Duration> {
        std::mem::take(&mut self.app.world_mut().resource_mut::<TickTimer>().ticks)
    }

    pub fn world_mut(&mut self) -> &mut World {
        self.app.world_mut()
    }
}

/// Load `scenario`'s map with the clock stopped (zero-length frames run
/// no tick) and wait for its world. `rematch`: the app is already in a
/// match; ask for a new one the way the in-game Restart does.
fn enter_match(app: &mut App, scenario: &Scenario, rematch: bool) {
    let old: Vec<Entity> = {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<Homebase>>()
            .iter(world)
            .collect()
    };
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
    app.world_mut().insert_resource(GameSetup {
        map: scenario.map.clone(),
        players: scenario.seats.clone(),
        difficulty: scenario.difficulty,
        demo: false,
        showcase: None,
        sandbox: false,
    });
    if rematch {
        app.world_mut()
            .resource_mut::<Messages<crate::game_setup::RunGame>>()
            .write(crate::game_setup::RunGame);
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        app.update();
        let world = app.world_mut();
        let bases: Vec<Entity> = world
            .query_filtered::<Entity, With<Homebase>>()
            .iter(world)
            .collect();
        let spawned = world
            .get_resource::<ActiveMatch>()
            .is_some_and(|m| m.0.map == scenario.map)
            && bases.len() == scenario.seats.len()
            && !bases.iter().any(|e| old.contains(e));
        if spawned {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "map {} never loaded",
            scenario.map
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    // All seats are AI: the local "player" spectates, and a spectator
    // owns no factory — keep the Defeat panel from freezing the sim
    // (eliminations still run).
    app.world_mut().insert_resource(GameOverDismissed(true));
    app.world_mut().resource_mut::<TickTimer>().ticks.clear();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(TICK));
}

impl SimHarness {
    /// Start a new match in the same app (the in-game Restart path):
    /// whatever the previous match left behind outside the match-scoped
    /// state carries over. `reset_fixed_clock`: also restart the fixed
    /// clock at 0, as a fresh app has it.
    pub fn rematch(&mut self, scenario: Scenario, reset_fixed_clock: bool) {
        crate::game_setup::test_seed::set(Some(scenario.seed));
        enter_match(&mut self.app, &scenario, true);
        if reset_fixed_clock {
            self.app
                .insert_resource(Time::<Fixed>::from_hz(crate::sim::SIMULATION_HZ));
        }
        self.tick = 0;
        self.scenario = scenario;
        self.world_mut().resource_mut::<HashLog>().ticks.clear();
    }
}

impl Drop for SimHarness {
    fn drop(&mut self) {
        crate::game_setup::test_seed::set(None);
    }
}

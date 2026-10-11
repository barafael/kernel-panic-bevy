//! Headless simulation harness for the network experiments
//! (`net-experiments/x-sim.md`) and the golden replays
//! (`net-experiments/m0-golden-replay.md`).
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
//! The match is the normal skirmish path: a [`GameSetup`] (all AI
//! seats, or a replay's seats), entered through `AppState::InGame`,
//! loaded by `map_loading` from `assets/maps` (a baked `.kpmap` when
//! present).
//!
//! Test builds always have the harness; the `replay` feature compiles it
//! into the game binary too, for the golden-replay matrix ([`replay`]):
//! `--replay` on native (`cli`), the `kp_replay` export on wasm32
//! (`web`). The same plugin set runs on both. wasm has no filesystem
//! and must never block: the assets come from an in-memory source
//! ([`AssetRoot::Memory`]), and every wait yields to the JS event loop
//! ([`pause`]), which is where Bevy runs its tasks on the web.

// A toolkit: the tests, the CLI and the wasm export each use their own
// part of it, so each build leaves some of it unused.
#![allow(dead_code)]

#[cfg(all(feature = "replay", not(target_arch = "wasm32")))]
pub mod cli;
#[cfg(not(target_arch = "wasm32"))]
pub mod golden;
pub mod replay;
pub mod state;
#[cfg(test)]
mod tests;
#[cfg(all(feature = "replay", target_arch = "wasm32"))]
mod web;

use std::path::PathBuf;
use std::time::Duration;

use bevy::app::PluginsState;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::WgpuSettings;
use bevy::time::TimeUpdateStrategy;
use bevy::window::ExitCondition;

use crate::game_setup::{
    ActiveMatch, AppState, DevOptions, GameOverDismissed, GameSetup, PlayerSpec,
};
use crate::map_loading::MapCatalog;
use crate::units::components::{Faction, Homebase};

/// One sim tick.
pub const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);

/// Which executor runs `FixedUpdate`. The game always uses the
/// single-threaded one (`UnitsPlugin`); `Multi` is the experiment's
/// comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Executor {
    Single,
    Multi,
}

/// A seeded match: all AI, or a replay's seats.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub map: String,
    pub seats: Vec<PlayerSpec>,
    pub seed: u64,
    pub executor: Executor,
    pub difficulty: u8,
    /// No win / lose check ([`GameSetup::sandbox`]).
    pub sandbox: bool,
    /// Pin the map catalog to exactly this entry (a `.kpmap` path in the
    /// catalog's form), so the match loads these bytes or nothing: never
    /// another archive of the same name, never the catalog's fallback
    /// first map. `None`: the normal catalog lookup.
    pub map_file: Option<PathBuf>,
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
            executor: Executor::Single,
            difficulty: 2,
            sandbox: false,
            map_file: None,
        }
    }
}

/// Where the harness's asset server reads from.
#[derive(Clone)]
pub enum AssetRoot {
    /// A directory on disk (native: `kernel-panic/assets`).
    Dir(String),
    /// An in-memory tree (wasm: the replay's `.kpmap`, handed in by JS).
    /// Whatever else the game asks the asset server for (sounds, fonts,
    /// cursor images) fails to load; only the frame loop reads those.
    Memory(bevy::asset::io::memory::Dir),
}

impl AssetRoot {
    /// `kernel-panic/assets` of this checkout.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn checkout() -> Self {
        let dir = crate::paths::from_project_root("kernel-panic/assets");
        AssetRoot::Dir(dir.to_string_lossy().into_owned())
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
    /// Positions, health / flags, orders, projectiles.
    pub parts: [u64; 4],
    pub units: u32,
    pub projectiles: u32,
    /// NaNs met while hashing (see [`state::Capture::nans`]).
    pub nans: u32,
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
        nans: c.nans as u32,
    });
}

/// The game app without a window or renderer, plugins finished, nothing
/// run yet, reading assets from this checkout.
#[cfg(not(target_arch = "wasm32"))]
pub fn build_app(executor: Executor) -> App {
    let mut app = add_plugins(executor, AssetRoot::checkout());
    while app.plugins_state() == PluginsState::Adding {
        bevy::tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();
    app
}

/// [`build_app`] for any [`AssetRoot`] on any target: waits for the
/// plugins by yielding ([`pause`]).
pub async fn build_app_async(executor: Executor, assets: AssetRoot) -> App {
    let mut app = add_plugins(executor, assets);
    while app.plugins_state() == PluginsState::Adding {
        #[cfg(not(target_arch = "wasm32"))]
        bevy::tasks::tick_global_task_pools_on_main_thread();
        pause().await;
    }
    app.finish();
    app.cleanup();
    app
}

/// Let background work run. Native: sleep briefly (the task pools have
/// their own threads). wasm: hand control back to the JS event loop for
/// one macrotask; Bevy's tasks (`wasm_bindgen_futures::spawn_local`,
/// e.g. asset loads) only make progress there.
pub async fn pause() {
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::sleep(Duration::from_millis(2));
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            // `setTimeout` is a global in browsers and in Node alike
            // (Node has no `window`).
            let global = js_sys::global();
            let set_timeout = js_sys::Reflect::get(&global, &"setTimeout".into())
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                .expect("no global setTimeout");
            let _ = set_timeout.call2(&global, &resolve, &0.into());
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

/// The harness's plugin set, the same on every target: the whole game
/// on `DefaultPlugins` minus window, log and audio output, renderer
/// disabled. Plugins added, not finished.
fn add_plugins(executor: Executor, assets: AssetRoot) -> App {
    let mut app = App::new();
    let file_path = match &assets {
        AssetRoot::Dir(dir) => dir.clone(),
        AssetRoot::Memory(_) => String::new(),
    };
    if let AssetRoot::Memory(dir) = assets {
        // Registered before `AssetPlugin` builds, so it becomes the
        // default source instead of the platform's (files / HTTP).
        use bevy::asset::AssetApp;
        use bevy::asset::io::{AssetSourceBuilder, AssetSourceId, memory::MemoryAssetReader};
        app.register_asset_source(
            AssetSourceId::Default,
            AssetSourceBuilder::new(move || Box::new(MemoryAssetReader { root: dir.clone() })),
        );
    }
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
                file_path,
                // No `.meta` lookups, as the web build.
                meta_check: bevy::asset::AssetMetaCheck::Never,
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
    if executor == Executor::Multi {
        app.edit_schedule(FixedUpdate, |s| {
            s.set_executor(bevy::ecs::schedule::MultiThreadedExecutor::new());
        });
    }
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
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(scenario: Scenario) -> Self {
        Self::new_with(scenario, AssetRoot::checkout())
    }

    /// [`Self::new`] reading assets from `assets`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new_with(scenario: Scenario, assets: AssetRoot) -> Self {
        bevy::tasks::block_on(Self::start(scenario, assets))
    }

    /// [`Self::new`] for any [`AssetRoot`], on any target.
    pub async fn start(scenario: Scenario, assets: AssetRoot) -> Self {
        let mut app = build_app_async(scenario.executor, assets).await;

        // Startup (map catalog, the menu's boot demo request) …
        app.update();
        // … then our match instead: the InGame entry supersedes the
        // demo's pending load.
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::InGame);
        enter_match(&mut app, &scenario, false).await;
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

/// Everything the match setup decides, applied before the match loads:
/// the [`GameSetup`] the map loader reads (seats, difficulty, the match
/// seed; `begin_match` derives the AI seats and the match-scoped seed
/// from it) and, for a replay, the one map file it may load.
pub fn apply_setup(world: &mut World, scenario: &Scenario) {
    world.insert_resource(GameSetup {
        map: scenario.map.clone(),
        players: scenario.seats.clone(),
        difficulty: scenario.difficulty,
        demo: false,
        showcase: None,
        sandbox: scenario.sandbox,
        seed: scenario.seed,
    });
    if let Some(file) = &scenario.map_file {
        world.insert_resource(MapCatalog(vec![file.clone()]));
    }
}

/// Load `scenario`'s map with the clock stopped (zero-length frames run
/// no tick) and wait for its world. `rematch`: the app is already in a
/// match; ask for a new one the way the in-game Restart does.
async fn enter_match(app: &mut App, scenario: &Scenario, rematch: bool) {
    let old: Vec<Entity> = {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<Homebase>>()
            .iter(world)
            .collect()
    };
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
    apply_setup(app.world_mut(), scenario);
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
        pause().await;
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
    #[cfg(test)]
    pub fn rematch(&mut self, scenario: Scenario, reset_fixed_clock: bool) {
        bevy::tasks::block_on(enter_match(&mut self.app, &scenario, true));
        if reset_fixed_clock {
            self.app
                .insert_resource(Time::<Fixed>::from_hz(crate::sim::SIMULATION_HZ));
        }
        self.tick = 0;
        self.scenario = scenario;
        self.world_mut().resource_mut::<HashLog>().ticks.clear();
    }
}

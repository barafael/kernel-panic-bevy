//! Map loading: pick a map archive from `assets/maps/` and spawn its
//! terrain, atmosphere, fog, nav grid, minimap, and homebases.
//!
//! Exposes [`MapLoadingPlugin`]. The map catalog is discovered at
//! Startup; the world itself is (re)built on every entry into
//! [`AppState::InGame`] and whenever the menu issues a [`RunGame`]
//! (restart / demo reload), so one code path serves fresh games,
//! restarts, and the menu's attract-mode reload.
//!
//! A load is two halves. [`prepare_map`] turns a decoded `SpringMap`
//! into a [`PreparedMap`] — the mipmapped ground texture, terrain chunk
//! meshes, heightmap, aircraft smooth mesh, nav grids and minimap layer,
//! all plain data — and on native runs in an `AsyncComputeTaskPool`
//! task so the frame keeps rendering (the attract demo behind the menu
//! reloads a random map every time a match ends; a quarter-gigabyte
//! texture used to freeze it for seconds). [`spawn_prepared_map`] then
//! does only the ECS work: asset inserts, entities, resources, the
//! unit spawn burst. Web (single-threaded wasm) runs the same
//! preparation synchronously once the fetched bytes land.
//!
//! Terrain-material construction (mipmap pyramid + fallback) lives in
//! [`mipmap`] so the orchestrator stays focused on sequencing.

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;

use bevy::prelude::*;

use crate::{
    game_setup::{GameSetup, RunGame},
    interaction,
    rendering::camera::{MapBounds, RtsCamera, RtsCameraState, compute_transform_from_state},
    terrain::{
        geovent::{GeoventAssets, spawn_geovent_smokers},
        heightmap::Heightmap,
        mesh::{TerrainChunk, generate_terrain_chunks},
        smooth_ground::SmoothGround,
    },
    ui,
    units::content::unit_registry::UnitRegistry,
    units::lifecycle::spawning::{spawn_demo_squads, spawn_homebases, spawn_showcase_homebase},
};
use spring_map::{
    hexfarm::HexFarm,
    map_types::{MapFeature, SmfHeader},
    smd_parser::MapInfo,
};

// HexFarm Lua-composited towers/bridges (native and web: the data
// round-trips through `.kpmap` v4).
pub(crate) mod lua_compositing;
mod mipmap;

// Web-only: maps arrive as fetched `.kpmap` bytes through the asset
// server, and the deploy's map list is embedded at build time.
#[cfg(target_arch = "wasm32")]
mod bytes_asset;
#[cfg(target_arch = "wasm32")]
use bytes_asset::BytesAsset;
#[cfg(target_arch = "wasm32")]
include!(concat!(env!("OUT_DIR"), "/web_map_catalog.rs"));

use mipmap::{build_terrain_image, dark_fallback_material, void_ground_material};

pub struct MapLoadingPlugin;

/// Set containing the game-entry request (`Update`) and the world
/// teardown+rebuild pair (`First`). The swap running in `First` means
/// no `Update` system's queued commands can straddle it; UI systems
/// that hold entity references across frames (command panel, build bar,
/// tooltip, placement ghost) still order `.after` this set so they see
/// a rerun request's state reset in the same frame.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GameWorldRebuild;

impl Plugin for MapLoadingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReadyMap>().add_systems(
            Startup,
            pick_map.after(crate::rendering::camera::spawn_camera),
        );

        #[cfg(not(target_arch = "wasm32"))]
        {
            app.init_resource::<PendingMapLoad>()
                .add_systems(
                    OnEnter(crate::game_setup::AppState::InGame),
                    prepare_game_entry.in_set(GameWorldRebuild),
                )
                // Restart / demo reload while in any state: the menu
                // writes `RunGame` (optionally with a fresh `GameSetup`)
                // and we tear down + rebuild the world in-place once the
                // prepared map lands. Ordered after the menu's writers so
                // their `GameSetup` inserts are applied before we read it
                // — otherwise the boot demo would run with the default
                // (non-demo) setup and spawn homebases.
                .add_systems(
                    Update,
                    prepare_game_entry
                        .run_if(rerun_requested)
                        .in_set(GameWorldRebuild)
                        .after(crate::ui::menu::boot_demo),
                )
                // The swap runs in `First`: every despawn command the old
                // world's Update systems queued has been applied by then.
                // In `Update` the exclusive teardown could land between
                // an unordered system queueing a despawn and that
                // command's end-of-schedule flush, which then hit an
                // already-despawned entity.
                .add_systems(
                    First,
                    (
                        poll_map_load.run_if(|pending: Res<PendingMapLoad>| pending.0.is_some()),
                        spawn_prepared_map,
                    )
                        .chain()
                        .in_set(GameWorldRebuild),
                );
        }

        // Web: no filesystem — the world is built from a fetched
        // `.kpmap`. `prepare_game_entry` requests the asset; the poll
        // system prepares + spawns once the bytes land.
        #[cfg(target_arch = "wasm32")]
        {
            use bevy::asset::AssetApp;
            app.init_asset::<BytesAsset>()
                .register_asset_loader(bytes_asset::BytesLoader)
                .init_resource::<PendingWebMapLoad>()
                .init_resource::<PrefetchedWebMaps>()
                .add_systems(
                    OnEnter(crate::game_setup::AppState::InGame),
                    prepare_game_entry.in_set(GameWorldRebuild),
                )
                .add_systems(
                    Update,
                    (
                        prepare_game_entry.run_if(rerun_requested),
                        prefetch_selected_map.run_if(resource_exists_and_changed::<GameSetup>),
                    )
                        .chain()
                        .in_set(GameWorldRebuild)
                        .after(crate::ui::menu::boot_demo),
                )
                // Swap in `First` — see the native branch.
                .add_systems(
                    First,
                    (
                        poll_map_load.run_if(|pending: Res<PendingWebMapLoad>| pending.0.is_some()),
                        spawn_prepared_map,
                    )
                        .chain()
                        .in_set(GameWorldRebuild),
                );
        }
    }
}

/// Run-condition helper for the in-game Restart path: true on any frame
/// where the menu issued a `RunGame`.
fn rerun_requested(mut reader: MessageReader<RunGame>) -> bool {
    reader.read().next().is_some()
}

/// Native: the map being read, decoded and prepared off the main thread.
/// Replacing it drops (cancels) the previous load — a skirmish started
/// while the demo's next map is still preparing simply wins.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Default)]
struct PendingMapLoad(Option<bevy::tasks::Task<Option<PreparedMap>>>);

/// A prepared map whose world has just been torn down; the frame's
/// [`spawn_prepared_map`] builds it.
#[derive(Resource, Default)]
struct ReadyMap(Option<PreparedMap>);

/// Web: in-flight `.kpmap` fetch, requested by [`prepare_game_entry`]
/// and consumed by [`poll_map_load`] when the payload lands.
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
struct PendingWebMapLoad(Option<Handle<BytesAsset>>);

/// Web: map paths already handed to the asset server (in flight or
/// loaded), so the prefetcher doesn't re-request every frame. Holds
/// the strong handles: Bevy frees an asset once its last strong handle
/// drops, which would throw the prefetched bytes away before the match
/// asks for them.
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
struct PrefetchedWebMaps(HashMap<String, Handle<BytesAsset>>);

/// Resolve the setup's map name against the catalog the same way
/// [`prepare_game_entry`] does: exact stem match, else the first entry.
fn resolve_catalog_path(setup_map: &str, catalog: &[PathBuf]) -> Option<PathBuf> {
    catalog
        .iter()
        .find(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy() == setup_map)
                .unwrap_or(false)
        })
        .or_else(|| catalog.first())
        .cloned()
}

/// Web: fetch the selected map's `.kpmap` while the player is still in
/// the menu, so the bytes are already local when Play is clicked. The
/// asset server dedupes loads of the same path, so `prepare_game_entry`
/// re-requesting it later is free. Runs only when the setup changes —
/// the selected map can't change otherwise.
#[cfg(target_arch = "wasm32")]
fn prefetch_selected_map(
    setup: Res<GameSetup>,
    catalog: Res<MapCatalog>,
    mut prefetched: ResMut<PrefetchedWebMaps>,
    server: Res<AssetServer>,
) {
    let Some(path) = resolve_catalog_path(&setup.map, &catalog.0) else {
        return;
    };
    let key = path.to_string_lossy().into_owned();
    if prefetched.0.contains_key(&key) {
        return;
    }
    let handle = server.load::<BytesAsset>(key.clone());
    prefetched.0.insert(key, handle);
    info!("Prefetching {}", setup.map);
}

/// Marker for entities that survive game-world teardown (menu UI):
/// the launch menu, Esc overlay, and game-over panel all carry it so
/// [`despawn_game_world`] spares them while clearing the match.
#[derive(Component)]
pub struct PersistentEntity;

/// Marker on terrain chunk meshes. The placement ghost's cursor ray
/// filters on this so it reads the ground plane only — never the ghost
/// mesh itself, unit meshes under the cursor, or order-palette gizmos.
#[derive(Component)]
pub struct TerrainChunkMarker;

/// Which chunk (in [`CHUNK_SIZE`](crate::terrain::mesh::CHUNK_SIZE)
/// squares) a terrain chunk entity is, so runtime height edits can
/// rebuild just the chunks they touch.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
pub struct TerrainChunkCoord(pub usize, pub usize);

/// All map archives available in `assets/maps/`, sorted. The menu's map
/// list and random-map resolution read this.
#[derive(Resource, Clone)]
pub struct MapCatalog(pub Vec<PathBuf>);

impl MapCatalog {
    /// Human-facing names (file stems) in catalog order.
    pub fn names(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|p| {
                p.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// Every transition into `InGame` (fresh game, Restart, re-run after
/// defeat) passes through here: reset the in-game state machine,
/// resolve the map path from the setup and start the load. The previous
/// game world keeps running until the new map is prepared;
/// [`finish_map_load`] tears it down right before the swap.
/// Exclusive system — it reads the registry and pools directly.
fn prepare_game_entry(world: &mut World) {
    use crate::game_setup::GameOverDismissed;
    use crate::units::lifecycle::game_over::GameState;

    // The human seat's ally team drives input, the game-over check and
    // the fog/cloak perspective; with no human seat (the menu's
    // attract-mode demo) the local player spectates. The difficulty the
    // menu picked drives the AI.
    let setup = world.resource::<GameSetup>().clone();
    let local_team = setup
        .players
        .iter()
        .find(|p| !p.ai)
        .map_or(crate::game_setup::SPECTATOR_TEAM, |p| p.team);
    world.resource_mut::<crate::units::player::LocalTeam>().0 = local_team;
    world.insert_resource(crate::game_setup::AiDifficulty(setup.difficulty));

    // Fresh in-game state: `Playing`, game-over panel re-armed.
    world
        .resource_mut::<NextState<GameState>>()
        .set(GameState::Playing);
    world.resource_mut::<GameOverDismissed>().0 = false;

    // Resolve the setup's map name against the catalog.
    let catalog = world.resource::<MapCatalog>().0.clone();
    let Some(path) = resolve_catalog_path(&setup.map, &catalog) else {
        error!("Map catalog is empty — cannot start a game");
        return;
    };
    info!("Preparing match on {} ({})", setup.map, path.display());
    // A load already in flight is superseded.
    world.resource_mut::<ReadyMap>().0 = None;

    #[cfg(not(target_arch = "wasm32"))]
    {
        let map_name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let inputs = PrepareInputs::gather(setup, world.resource::<UnitRegistry>(), map_name);
        let task = bevy::tasks::AsyncComputeTaskPool::get()
            .spawn(async move { load_and_prepare(&path, inputs) });
        // Dropping the previous task cancels it.
        world.resource_mut::<PendingMapLoad>().0 = Some(task);
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Catalog entries are already asset-relative paths
        // (`maps/<stem>.kpmap`) — request the fetch.
        let handle: Handle<BytesAsset> = world
            .resource::<AssetServer>()
            .load(path.to_string_lossy().into_owned());
        world.resource_mut::<PendingWebMapLoad>().0 = Some(handle);
    }
}

/// Native: read the selected archive (baked `.kpmap` preferred) and
/// prepare it. Runs on the compute pool; a failure is logged here and
/// leaves the current world in place.
#[cfg(not(target_arch = "wasm32"))]
fn load_and_prepare(map_path: &Path, inputs: PrepareInputs) -> Option<PreparedMap> {
    info!("Loading map: {}", inputs.map_name);
    let decode_start = bevy::platform::time::Instant::now();
    let spring_map = match load_map_dispatch(map_path) {
        Ok(m) => m,
        Err(error) => {
            error!("Failed to load {}: {error}", map_path.display());
            return None;
        }
    };
    info!(
        "  decoded in {:.0}ms",
        decode_start.elapsed().as_secs_f64() * 1000.0
    );
    Some(prepare_map(spring_map, inputs))
}

/// Native: once the compute task has the prepared map, swap worlds —
/// tear the old one down and hand the map to [`spawn_prepared_map`]
/// (same frame, right after this system, both in `First`).
#[cfg(not(target_arch = "wasm32"))]
fn poll_map_load(world: &mut World) {
    let prepared = {
        let mut pending = world.resource_mut::<PendingMapLoad>();
        let Some(task) = pending.0.as_mut() else {
            return;
        };
        let Some(result) = bevy::tasks::futures::check_ready(task) else {
            return;
        };
        pending.0 = None;
        result
    };
    if let Some(prepared) = prepared {
        finish_map_load(world, prepared);
    }
}

/// Web: poll the in-flight `.kpmap` fetch; once the bytes have landed,
/// decode and prepare the map on the spot (wasm is single-threaded),
/// then swap worlds. A failed fetch logs an error and clears the pending
/// state so the menu can retry.
#[cfg(target_arch = "wasm32")]
fn poll_map_load(world: &mut World) {
    use bevy::asset::LoadState;

    let Some(handle) = world.resource::<PendingWebMapLoad>().0.clone() else {
        return;
    };
    match world.resource::<AssetServer>().load_state(&handle) {
        LoadState::Loaded => {}
        LoadState::Failed(error) => {
            error!("Map fetch failed: {error}");
            world.resource_mut::<PendingWebMapLoad>().0 = None;
            return;
        }
        _ => return, // still fetching / deps loading
    }
    let Some(asset) = world.resource::<Assets<BytesAsset>>().get(&handle) else {
        return;
    };
    let bytes = asset.0.clone();
    world.resource_mut::<PendingWebMapLoad>().0 = None;

    let setup = world.resource::<GameSetup>().clone();
    let map_name = setup.map.clone();
    info!("Received {} ({} baked bytes)", map_name, bytes.len());
    let decode_start = bevy::platform::time::Instant::now();
    let spring_map = match spring_map::baked::read_baked_map(&bytes) {
        Ok(m) => m,
        Err(error) => {
            error!("Failed to decode baked map {map_name}: {error}");
            return;
        }
    };
    info!(
        "  decoded in {:.0}ms",
        decode_start.elapsed().as_secs_f64() * 1000.0
    );
    let inputs = PrepareInputs::gather(setup, world.resource::<UnitRegistry>(), map_name);
    let prepared = prepare_map(spring_map, inputs);
    finish_map_load(world, prepared);
}

/// The swap: the old world goes, and [`spawn_prepared_map`] builds the
/// new one in the same frame.
fn finish_map_load(world: &mut World, prepared: PreparedMap) {
    // The teardown below despawns units without the `Dying` pass the
    // event-driven tallies decrement on — start the new match from zero.
    world.insert_resource(crate::units::lifecycle::bookkeeping::SmallBuildingCounts::default());
    world.insert_resource(crate::units::lifecycle::bookkeeping::TotalUnitCount::default());

    // Tear down the previous game world (no-op on first entry). Kept
    // entities: windows, the RTS camera (and its children), and anything
    // tagged `PersistentEntity` (menu UI).
    despawn_game_world(world);
    world.resource_mut::<ReadyMap>().0 = Some(prepared);
}

fn despawn_game_world(world: &mut World) {
    use bevy::ecs::entity::EntityHashSet;
    use bevy::window::Window;

    // Roots we keep: windows, the RTS camera, persistent UI.
    let mut keep: EntityHashSet = EntityHashSet::default();
    let mut windows = world.query_filtered::<Entity, With<Window>>();
    for e in windows.iter(world) {
        keep.insert(e);
    }
    let mut cameras = world.query_filtered::<Entity, With<RtsCamera>>();
    for e in cameras.iter(world) {
        keep.insert(e);
    }
    let mut persistent = world.query_filtered::<Entity, With<PersistentEntity>>();
    for e in persistent.iter(world) {
        keep.insert(e);
    }
    // In-flight screenshots: the render world answers them a frame or
    // two later with a plain `insert(Captured)`, which panics on a
    // despawned entity (the dev shot tools and the recorder capture
    // right across demo restarts).
    let mut shots =
        world.query_filtered::<Entity, With<bevy::render::view::screenshot::Screenshot>>();
    for e in shots.iter(world) {
        keep.insert(e);
    }

    // Pull kept roots' descendants into the keep set (camera children,
    // UI trees). One pass builds a parent→children map, then a DFS from
    // the roots — the old fixed-point loop rescanned every `ChildOf` in
    // the world once per hierarchy level, O(E · depth).
    {
        let mut children_of: HashMap<Entity, Vec<Entity>> = HashMap::new();
        let mut relations = world.query::<(Entity, Option<&ChildOf>)>();
        for (e, child_of) in relations.iter(world) {
            if let Some(child_of) = child_of {
                children_of.entry(child_of.parent()).or_default().push(e);
            }
        }
        let mut stack: Vec<Entity> = keep.iter().copied().collect();
        while let Some(parent) = stack.pop() {
            if let Some(children) = children_of.get(&parent) {
                for &child in children {
                    if keep.insert(child) {
                        stack.push(child);
                    }
                }
            }
        }
    }

    // Everything not kept goes. `despawn` on the remaining roots takes
    // care of subtrees; existence checks tolerate overlaps.
    let mut all = world.query_filtered::<Entity, ()>();
    let doomed: Vec<Entity> = all.iter(world).filter(|e| !keep.contains(e)).collect();
    for e in doomed {
        if world.get_entity(e).is_ok() {
            world.entity_mut(e).despawn();
        }
    }
}

/// Discover the map archives in `assets/maps/` into the menu-facing
/// catalog. A CLI map argument (direct file or name stem) pre-seeds the
/// default `GameSetup` and auto-enters the game — preserving the old
/// "launch straight into a map" behaviour for headless testing.
///
/// Web: no filesystem. The deploy workflow bakes `.kpmap` files into the
/// artifact and [`WEB_MAP_CATALOG`] (see build.rs) lists them at compile
/// time, so the catalog is just the embedded list.
fn pick_map(mut commands: Commands) {
    #[cfg(target_arch = "wasm32")]
    {
        let paths: Vec<PathBuf> = WEB_MAP_CATALOG
            .iter()
            .map(|n| PathBuf::from(format!("maps/{n}.kpmap")))
            .collect();
        commands.insert_resource(MapCatalog(paths));
        commands.insert_resource(crate::game_setup::GameSetup::default());
        return;
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let maps_dir = crate::paths::from_project_root("kernel-panic/assets/maps");
        let mut maps: Vec<PathBuf> = std::fs::read_dir(&maps_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_map_ext(p.as_path()))
            .collect();
        dedupe_prefer_baked(&mut maps);
        maps.sort();

        if maps.is_empty() {
            panic!(
                "No map files found in {}. Place .sd7/.sdz files there or pass one as a CLI arg.",
                maps_dir.display()
            );
        }

        // CLI override: a direct usable map file, or a name stem matched
        // against the catalog.
        let cli_arg = std::env::args().nth(1);
        let direct = cli_arg
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.is_file() && is_map_ext(p.as_path()));
        let stem_pick = cli_arg.as_ref().and_then(|arg| {
            let stem = PathBuf::from(arg)
                .file_stem()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            maps.iter().position(|p| {
                p.file_stem()
                    .map(|s| s.to_ascii_lowercase() == stem)
                    .unwrap_or(false)
            })
        });

        let mut setup = crate::game_setup::GameSetup::default();
        let mut auto_enter = false;
        if let Some(path) = direct.or_else(|| stem_pick.map(|i| maps[i].clone())) {
            setup.map = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            auto_enter = true;
            info!("CLI map argument: {}", setup.map);
        }

        // Showcase override: `showcase:<System|Hacker|Network>` boots
        // straight into that faction's showcase, bypassing the menu.
        if let Some(faction) = cli_arg.as_ref().and_then(|arg| {
            arg.strip_prefix("showcase:")
                .and_then(|n| match n.to_ascii_lowercase().as_str() {
                    "system" => Some(crate::units::components::Faction::System),
                    "hacker" => Some(crate::units::components::Faction::Hacker),
                    "network" => Some(crate::units::components::Faction::Network),
                    _ => None,
                })
        }) {
            setup = crate::game_setup::showcase_setup(faction);
            auto_enter = true;
            info!("CLI showcase argument: {:?}", faction);
        }

        commands.insert_resource(MapCatalog(maps));
        commands.insert_resource(setup);
        if auto_enter {
            commands.insert_resource(NextState::Pending(crate::game_setup::AppState::InGame));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn is_map_ext(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("sd7") | Some("sdz") | Some("kpmap")
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn is_baked_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("kpmap"))
        .unwrap_or(false)
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, thiserror::Error)]
enum LoadMapError {
    #[error(transparent)]
    Source(#[from] spring_map::map_types::MapError),
    #[error(transparent)]
    Baked(#[from] spring_map::baked::BakedMapError),
    #[error("I/O error reading {path}: {error}")]
    Io {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
}

#[cfg(not(target_arch = "wasm32"))]
fn load_map_dispatch(path: &Path) -> Result<spring_map::SpringMap, LoadMapError> {
    if is_baked_ext(path) {
        let bytes = std::fs::read(path).map_err(|error| LoadMapError::Io {
            path: path.to_path_buf(),
            error,
        })?;
        Ok(spring_map::baked::read_baked_map(&bytes)?)
    } else {
        Ok(spring_map::load_map(path)?)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn dedupe_prefer_baked(maps: &mut Vec<PathBuf>) {
    use std::collections::HashSet;
    let baked_stems: HashSet<String> = maps
        .iter()
        .filter(|p| is_baked_ext(p))
        .filter_map(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
        })
        .collect();
    maps.retain(|p| {
        if is_baked_ext(p) {
            return true;
        }
        let Some(stem) = p
            .file_stem()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
        else {
            return true;
        };
        !baked_stems.contains(&stem)
    });
}

/// What [`prepare_map`] needs from the ECS side, snapshotted when the
/// load is requested so the preparation can run without the world.
struct PrepareInputs {
    setup: GameSetup,
    map_name: String,
    /// Distinct nav slope caps in Spring's encoding, ascending — one
    /// pathfinding grid each (see `cost.rs`; `compute_path` picks the
    /// tightest bucket whose cap ≥ the unit's).
    nav_caps: Vec<f32>,
    /// `(slope cap, footprint half-size, crushes)` of every mobile
    /// kind: the QTPFS layers to prebuild.
    mover_classes: Vec<(f32, i32, bool)>,
    /// `UnitRegistry::hexfarm_medians`: Hex Farm rolls its layout from
    /// the roster's median health / build time.
    hexfarm_medians: (f64, f64),
    match_seed: u64,
}

impl PrepareInputs {
    fn gather(setup: GameSetup, registry: &UnitRegistry, map_name: String) -> Self {
        use std::collections::BTreeSet;

        use crate::units::content::definitions::ALL_UNIT_KINDS;
        use crate::units::content::unit_registry::DEFAULT_MAX_SLOPE_DEGREES;

        // Exact caps, not rounded: a cap quantised *below* a unit's own
        // value made `NavGridSet::bucket` skip its grid and hand every
        // LIGHT/MEDIUM/HEAVY unit the MaxSlope-60 building grid, on
        // which nothing is impassable (units climbed sheer cliffs).
        // Positive finite floats order like their bit patterns.
        let mut distinct_caps = BTreeSet::<u32>::new();
        for &kind in ALL_UNIT_KINDS {
            distinct_caps.insert(registry.max_slope_ratio(kind).to_bits());
        }
        // Always keep the KP-default bucket (FBI MaxSlope=36 from
        // `MOVEINFO.TDF`'s LIGHT/MEDIUM/HEAVY) available for units
        // whose FBI omits `MaxSlope`.
        let default_cap = spring_pathfinding::max_slope_from_degrees(DEFAULT_MAX_SLOPE_DEGREES);
        distinct_caps.insert(default_cap.to_bits());

        let mut mover_classes: Vec<(u32, i32, bool)> = ALL_UNIT_KINDS
            .iter()
            .filter_map(|&kind| {
                let md = registry.move_def(kind)?;
                Some((
                    registry.max_slope_ratio(kind).to_bits(),
                    md.xsizeh,
                    crate::interaction::structures::crushes_features(md.crush_strength),
                ))
            })
            .collect();
        mover_classes.sort_unstable();
        mover_classes.dedup();

        Self {
            setup,
            map_name,
            // Ascending because BTreeSet iteration is sorted.
            nav_caps: distinct_caps.into_iter().map(f32::from_bits).collect(),
            mover_classes: mover_classes
                .into_iter()
                .map(|(cap, h, c)| (f32::from_bits(cap), h, c))
                .collect(),
            hexfarm_medians: registry.hexfarm_medians(),
            match_seed: crate::game_setup::match_seed(),
        }
    }
}

/// A map ready to spawn: everything [`spawn_prepared_map`] inserts,
/// computed by [`prepare_map`] from a decoded `SpringMap` without
/// touching the ECS.
struct PreparedMap {
    setup: GameSetup,
    map_name: String,
    header: SmfHeader,
    features: Vec<MapFeature>,
    map_info: Option<MapInfo>,
    /// A Lua-composited map (Hex Farm) is drawn entirely by its gadget
    /// over a hidden (`voidGround`) ground — see `lua_compositing`.
    void_ground: bool,
    /// The mipmapped ground texture; `None` on a void-ground map or one
    /// that shipped no `.smt`.
    terrain_image: Option<Image>,
    chunks: Vec<TerrainChunk>,
    heightmap: Heightmap,
    smooth_ground: SmoothGround,
    nav_set: interaction::movement::NavGridSet,
    /// Minimap terrain layer, already at minimap size.
    minimap: (Vec<u8>, u32, u32),
    /// Hex Farm: this match's layout and its skin atlas.
    hex_farm: Option<(HexFarm, Image)>,
}

/// Everything after "I have a `SpringMap` in hand" that is pure data:
/// the Hex Farm roll, texture mip chain, heightmap, aircraft smooth
/// mesh, terrain chunk meshes, nav grids and minimap layer. Shared by
/// the native compute task and the web arrival path.
fn prepare_map(spring_map: spring_map::SpringMap, inputs: PrepareInputs) -> PreparedMap {
    let t_prepare = bevy::platform::time::Instant::now();
    let spring_map::SpringMap {
        mut parsed,
        ground_texture,
        mut map_info,
        lua_compositing,
        ..
    } = spring_map;
    let void_ground = lua_compositing.is_some();
    // Hex Farm: roll this match's layout the way the gadget's
    // `Initialize()` does, and apply what it writes to the engine —
    // heightmap, datavents, start positions — before anything reads
    // the map.
    let hex_farm = lua_compositing.map(|compositing| {
        let (median_health, median_build_time) = inputs.hexfarm_medians;
        let farm = HexFarm::generate(
            inputs.match_seed,
            spring_map::hexfarm::HexFarmSetup {
                map_size_x: parsed.header.world_width() as f64,
                map_size_z: parsed.header.world_depth() as f64,
                teams: inputs.setup.players.len().max(1),
                median_health,
                median_build_time,
            },
        );
        farm.write_whole_heightmap(&mut parsed.heights);
        parsed
            .features
            .extend(farm.datavents().into_iter().map(|v| {
                MapFeature::new(
                    spring_map::map_types::FeatureType::GeoVent,
                    v[0] as f32,
                    v[1] as f32,
                    v[2] as f32,
                    0.0,
                    1.0,
                )
            }));
        if let Some(info) = &mut map_info {
            // The mapinfo's `teams` are dummies; `SetStartPos` decides.
            info.start_positions = farm
                .start_positions
                .iter()
                .enumerate()
                .map(|(team, p)| spring_map::smd_parser::StartPosition {
                    team: team as u32,
                    x: p[0] as f32,
                    z: p[1] as f32,
                })
                .collect();
        }
        info!(
            "  Hex Farm: {:?} boundary, {} towers, {} bridges, tower radius {}, {} vents",
            farm.boundary,
            farm.hexes.len(),
            farm.rects.len(),
            farm.tower_radius,
            farm.datavents().len(),
        );
        // Kernel Panic's team-coloured variant of the skin, rolled 1 in 5.
        let atlas = if farm.team_colored {
            lua_compositing::team_colored_atlas(compositing.atlas)
        } else {
            compositing.atlas
        };
        (farm, lua_compositing::atlas_image(atlas))
    });
    let spring_map::map_types::ParsedMap {
        header,
        heights,
        features,
        metalmap: _,
    } = parsed;

    info!(
        "  {}x{} (heightmap {}x{}), {} features",
        header.map_x,
        header.map_y,
        header.heightmap_width(),
        header.heightmap_height(),
        features.len(),
    );

    let t_texture = bevy::platform::time::Instant::now();
    let terrain_image = match ground_texture {
        _ if void_ground => None,
        Some(ground) => Some(build_terrain_image(ground)),
        None => {
            warn!("No ground texture — using fallback");
            None
        }
    };
    let texture_ms = t_texture.elapsed().as_secs_f64() * 1000.0;

    // Check actual height variance, not header values (gadgets may have modified the terrain).
    let min_actual = heights.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_actual = heights.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    if (max_actual - min_actual) < 1.0 {
        warn!(
            "  Terrain is effectively flat (height range: {:.1})",
            max_actual - min_actual
        );
    }

    let t_terrain = bevy::platform::time::Instant::now();
    let (hm_w, hm_h) = (header.heightmap_width(), header.heightmap_height());
    let heightmap = Heightmap::from_raw(heights, hm_w, hm_h);
    // The aircraft's smoothed ground (`smoothGround.Init` at
    // `PreLoadSimulation`); Hex Farm replaces it with its own flight
    // profile (`SetWholeSmoothMesh` in the gadget's `Initialize`).
    let mut smooth_ground = SmoothGround::from_heightmap(&heightmap);
    if let Some((farm, _)) = hex_farm.as_ref() {
        farm.set_whole_smooth_mesh(|x, z, h| {
            smooth_ground
                .mesh
                .set_smooth_mesh(x as f32, z as f32, h as f32, None);
        });
    }
    let chunks = generate_terrain_chunks(heightmap.heights(), hm_w, hm_h);
    let terrain_ms = t_terrain.elapsed().as_secs_f64() * 1000.0;

    // One pathfinding grid per distinct unit `MaxSlope`. A cell's slope
    // is the same for every bucket, so it is computed once and
    // thresholded per cap. Void squares are impassable to every move
    // class (see `mask_void`).
    let t_nav = bevy::platform::time::Instant::now();
    let mut nav_set = interaction::movement::NavGridSet::default();
    {
        use spring_pathfinding::{SpeedMap, slope_map, slope_mod_from_max_slope};

        let slopes = slope_map(heightmap.heights(), hm_w as u32, hm_h as u32);
        for &cap in &inputs.nav_caps {
            let slope_mod = slope_mod_from_max_slope(cap);
            let mut speed_map =
                SpeedMap::from_slopes(&slopes, hm_w as u32 - 1, hm_h as u32 - 1, cap, slope_mod);
            if let Some((farm, _)) = hex_farm.as_ref() {
                let all = [0, 0, speed_map.width - 1, speed_map.height - 1];
                crate::map_events::hex_farm::mask_void(
                    &farm.terrain,
                    farm.type_w,
                    &mut speed_map,
                    all,
                );
            }
            let blocked = speed_map.speeds.iter().filter(|&&s| s <= 0.0).count();
            info!(
                "  Nav bucket max_slope={:.3} (slope_mod={:.2}): {} blocked of {} cells ({}x{})",
                cap,
                slope_mod,
                blocked,
                speed_map.speeds.len(),
                speed_map.width,
                speed_map.height,
            );
            nav_set.buckets.push(interaction::movement::NavBucket {
                max_slope: cap,
                speed_map,
            });
        }
        // The QTPFS layer of every mover class over the bare terrain
        // (structures come later, as changes), tesselated here off the
        // main thread rather than in the first path request's tick.
        for &(cap, xsizeh, crushes) in &inputs.mover_classes {
            if let Some(bucket) = nav_set.bucket_index(cap) {
                let layer = spring_pathfinding::qtpfs::NodeLayer::new(
                    &nav_set.buckets[bucket].speed_map,
                    None,
                );
                nav_set
                    .terrain_layers
                    .insert((bucket, xsizeh, crushes), layer);
            }
        }
    }
    let nav_ms = t_nav.elapsed().as_secs_f64() * 1000.0;

    // Minimap terrain layer from the ground texture's base level (a
    // voidGround map has none: paint its towers and bridges instead).
    let (mm_w, mm_h) = ui::minimap::minimap_dims(header.world_width(), header.world_depth());
    let minimap_pixels = match (&hex_farm, &terrain_image) {
        (Some((farm, _)), _) => {
            const RES: usize = crate::map_events::hex_farm::MINIMAP_RES;
            let px = lua_compositing::minimap_pixels(farm, RES);
            ui::minimap::downsample_terrain(Some(&px), RES, RES, mm_w, mm_h)
        }
        (None, Some(image)) => {
            let (base, w, h) = mipmap::base_level(image);
            ui::minimap::downsample_terrain(Some(base), w, h, mm_w, mm_h)
        }
        (None, None) => ui::minimap::downsample_terrain(None, 0, 0, mm_w, mm_h),
    };

    info!(
        "  prepared in {:.0}ms: texture {texture_ms:.0}ms, terrain {terrain_ms:.0}ms, {} nav buckets {nav_ms:.0}ms",
        t_prepare.elapsed().as_secs_f64() * 1000.0,
        nav_set.buckets.len(),
    );

    PreparedMap {
        setup: inputs.setup,
        map_name: inputs.map_name,
        header,
        features,
        map_info,
        void_ground,
        terrain_image,
        chunks,
        heightmap,
        smooth_ground,
        nav_set,
        minimap: (minimap_pixels, mm_w, mm_h),
        hex_farm,
    }
}

/// The ECS half of a load, in the frame the old world was torn down:
/// terrain, atmosphere, fog, nav grids, minimap, homebases, per-map
/// events, heightmap resource.
#[allow(clippy::too_many_arguments)]
fn spawn_prepared_map(
    mut ready: ResMut<ReadyMap>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut fog_query: Query<&mut DistanceFog, With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
    mut geovent_assets: ResMut<GeoventAssets>,
    mut ctx: crate::units::lifecycle::spawning::SpawnContext,
) {
    let Some(prepared) = ready.0.take() else {
        return;
    };
    let t_spawn = bevy::platform::time::Instant::now();
    let PreparedMap {
        setup,
        map_name,
        header,
        features,
        map_info,
        void_ground,
        terrain_image,
        chunks,
        heightmap,
        smooth_ground,
        nav_set,
        minimap,
        hex_farm,
    } = prepared;

    let terrain_material = match terrain_image {
        _ if void_ground => void_ground_material(&mut ctx.materials),
        Some(image) => {
            let texture = ctx.images.add(image);
            crate::terrain::material::create_terrain_material(texture, &mut ctx.materials)
        }
        None => dark_fallback_material(&mut ctx.materials),
    };

    setup_camera(&header, &heightmap, &mut camera_query, &mut map_bounds);

    ctx.commands.insert_resource(smooth_ground);

    spawn_terrain(
        chunks,
        &features,
        &heightmap,
        terrain_material,
        &mut ctx.commands,
        &mut ctx.meshes,
        &mut ctx.materials,
        &mut ctx.images,
        &mut geovent_assets,
    );

    // Towers/bridges drawing + dynamic mode (or clear a previous match's).
    crate::map_events::hex_farm::install(
        hex_farm,
        &mut ctx.commands,
        &mut ctx.meshes,
        &mut ctx.materials,
        &mut ctx.images,
    );

    ctx.commands.insert_resource(nav_set);

    {
        let (pixels, mm_w, mm_h) = &minimap;
        ui::minimap::setup_minimap(
            &mut ctx.commands,
            &mut ctx.images,
            Some(pixels),
            *mm_w as usize,
            *mm_h as usize,
            header.world_width(),
            header.world_depth(),
        );
    }

    if let Some(map_info) = &map_info {
        apply_atmosphere(map_info, &mut ctx.commands);
        if void_ground {
            // The gadget turns sky and water off (`SetDrawSky(false)`,
            // `SetDrawWater(false)`): the void is black.
            ctx.commands.insert_resource(ClearColor(Color::BLACK));
        }
        apply_fog(map_info, &header, &mut fog_query);
        if setup.demo {
            // Attract-mode demo: an all-AI skirmish behind the menu.
            // Each seat gets a starting squad so there is action to
            // watch before the first production cycle completes; the
            // menu's demo director restarts the match once it's decided.
            let bases = spawn_homebases(&heightmap, map_info, &setup.players, &mut ctx);
            spawn_demo_squads(&heightmap, &bases, &mut ctx);
            ctx.commands
                .remove_resource::<crate::showcase::ShowcaseDirector>();
        } else if let Some(faction) = setup.showcase {
            spawn_showcase_homebase(&heightmap, map_info, faction, &mut ctx);
            ctx.commands
                .insert_resource(crate::showcase::ShowcaseDirector::new(faction));
            info!("  Showcase({:?}) — skipping full roster", faction);
        } else {
            spawn_homebases(&heightmap, map_info, &setup.players, &mut ctx);
            // Clear any leftover showcase director from a previous game.
            ctx.commands
                .remove_resource::<crate::showcase::ShowcaseDirector>();
        }
        configure_map_events(&map_name, map_info, &heightmap, &mut ctx.commands);
        let datavent_count = features
            .iter()
            .filter(|f| f.feature_type.is_geovent())
            .count();
        info!(
            "  {} start positions, {} datavents, gravity={}",
            map_info.start_positions.len(),
            datavent_count,
            map_info.gravity,
        );
    }

    ctx.commands.insert_resource(heightmap);
    info!(
        "  world spawned in {:.0}ms (main thread)",
        t_spawn.elapsed().as_secs_f64() * 1000.0
    );
}

fn setup_camera(
    header: &SmfHeader,
    heightmap: &Heightmap,
    camera_query: &mut Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    map_bounds: &mut MapBounds,
) {
    let world_w = header.world_width();
    let world_d = header.world_depth();
    let heightmap_w = header.heightmap_width();
    let heightmap_h = header.heightmap_height();
    let center_height = heightmap.heights()[(heightmap_h / 2) * heightmap_w + heightmap_w / 2];

    *map_bounds =
        MapBounds::from_map_extents(Vec3::new(0.0, 0.0, 0.0), Vec3::new(world_w, 0.0, world_d));

    let map_extent = world_w.max(world_d);
    if let Ok((mut cam_state, mut cam_transform)) = camera_query.single_mut() {
        let focus = Vec3::new(world_w / 2.0, center_height, world_d / 2.0);
        let distance = map_extent * 0.5;
        cam_state.snap_to(focus, distance);
        *cam_transform = compute_transform_from_state(&cam_state);
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_terrain(
    chunks: Vec<TerrainChunk>,
    features: &[MapFeature],
    heightmap: &Heightmap,
    terrain_material: Handle<StandardMaterial>,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    std_materials: &mut ResMut<Assets<StandardMaterial>>,
    images: &mut ResMut<Assets<Image>>,
    geovent_assets: &mut GeoventAssets,
) {
    info!("  Spawning {} terrain chunks", chunks.len());

    let (hm_w, _) = heightmap.grid_size();
    let chunks_x = (hm_w - 1).div_ceil(crate::terrain::mesh::CHUNK_SIZE);
    for (i, chunk) in chunks.into_iter().enumerate() {
        let mesh_handle = meshes.add(chunk.mesh);
        commands.spawn((
            TerrainChunkMarker,
            TerrainChunkCoord(i % chunks_x, i / chunks_x),
            Mesh3d(mesh_handle),
            MeshMaterial3d(terrain_material.clone()),
            Transform::from_translation(chunk.translation),
        ));
    }

    spawn_geovent_smokers(
        features,
        heightmap,
        commands,
        geovent_assets,
        std_materials,
        images,
    );
}

/// Insert any per-map [`map_events`](crate::map_events) resources whose
/// activation key matches the loaded map's filename stem.
fn configure_map_events(
    map_name: &str,
    map_info: &MapInfo,
    heightmap: &Heightmap,
    commands: &mut Commands,
) {
    // Per-map: the previous map's schedule must not carry over (an
    // eruption clock aimed at Stack_Overflow's starts, a swirl on a
    // map without one). The state resource (clock + rng) goes with the
    // config, and any spawns still queued for the old world are
    // dropped rather than drained into the new one.
    commands.remove_resource::<crate::map_events::EruptionConfig>();
    commands.remove_resource::<crate::map_events::EruptionState>();
    commands.remove_resource::<crate::map_events::EruptionSpawnQueue>();
    commands.init_resource::<crate::map_events::EruptionSpawnQueue>();
    commands.remove_resource::<crate::map_events::CircularFlow>();
    if map_name.eq_ignore_ascii_case("Stack_Overflow") {
        let starts: Vec<Vec3> = map_info
            .start_positions
            .iter()
            .map(|sp| heightmap.place(sp.x, sp.z))
            .collect();
        info!("  Stack_Overflow detected — installing eruption schedule");
        commands.insert_resource(crate::map_events::EruptionConfig::stack_overflow(starts));
        commands.insert_resource(crate::map_events::EruptionState::default());
    }
    if map_name.eq_ignore_ascii_case("Circular_Buffer") {
        let (w, d) = heightmap.world_size();
        let center = Vec2::new(w * 0.5, d * 0.5);
        info!("  Circular_Buffer detected — installing clockwise flow swirl");
        commands.insert_resource(crate::map_events::CircularFlow {
            center_xz: center,
            strength: 0.6,
            clockwise: true,
        });
    }
}

fn apply_atmosphere(map_info: &MapInfo, commands: &mut Commands) {
    let sky = map_info.atmosphere.sky_color;
    commands.insert_resource(ClearColor(Color::linear_rgb(sky[0], sky[1], sky[2])));

    let sun = map_info.lighting.ground_sun_color;
    let ambient = map_info.lighting.ground_ambient;
    let dir = map_info.lighting.sun_dir;

    let sun_dir =
        Vec3::new(dir[0], dir[1], dir[2]).normalize_or(Vec3::new(0.0, 1.0, 0.5).normalize());

    commands.spawn((
        DirectionalLight {
            color: Color::linear_rgb(sun[0], sun[1], sun[2]),
            illuminance: 8000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::default().looking_to(-sun_dir, Vec3::Y),
    ));

    commands.insert_resource(bevy::light::GlobalAmbientLight {
        color: Color::linear_rgb(ambient[0], ambient[1], ambient[2]),
        brightness: 200.0,
        ..default()
    });
}

/// Write the map's fog atmosphere onto the camera's `DistanceFog`.
///
/// Fog end scales with the map diagonal so large maps don't get walled
/// off by haze a few grid cells from the camera. Spring's `FogStart` is a
/// fraction of that end distance.
fn apply_fog(
    map_info: &MapInfo,
    header: &SmfHeader,
    fog_query: &mut Query<&mut DistanceFog, With<RtsCamera>>,
) {
    let Ok(mut fog) = fog_query.single_mut() else {
        return;
    };
    let color = map_info.atmosphere.fog_color;
    let fog_start_frac = map_info.atmosphere.fog_start;
    let world_w = header.world_width();
    let world_d = header.world_depth();
    let diagonal = (world_w * world_w + world_d * world_d).sqrt();
    // Cover the full map diagonal + a bit more so the far edge never fogs
    // completely. Floor at 4000 elmos for small maps.
    let max_view_distance = (diagonal * 1.1).max(4000.0);

    fog.color = Color::linear_rgb(color[0], color[1], color[2]);
    fog.falloff = FogFalloff::Linear {
        start: fog_start_frac * max_view_distance,
        end: max_view_distance,
    };
}

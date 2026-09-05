//! Map loading: pick a map archive from `assets/maps/` and spawn its
//! terrain, atmosphere, fog, nav grid, minimap, and homebases.
//!
//! Exposes [`MapLoadingPlugin`]. The map catalog is discovered at
//! Startup; the world itself is (re)built on every entry into
//! [`AppState::InGame`] and whenever the menu issues a [`RunGame`]
//! (restart / demo reload), so one code path serves fresh games,
//! restarts, and the menu's attract-mode reload.
//!
//! Terrain-material construction (mipmap pyramid + fallback) lives in
//! [`mipmap`] so the orchestrator stays focused on sequencing.

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
        mesh::generate_terrain_chunks,
    },
    ui,
    units::lifecycle::spawning::{spawn_homebases, spawn_showcase_homebase},
};
use spring_map::{map_types::ParsedMap, smd_parser::MapInfo};

// HexFarm Lua-composited decorations come out of the source-archive
// pipeline, which doesn't exist on wasm (plan §8.1).
#[cfg(not(target_arch = "wasm32"))]
mod lua_compositing;
mod mipmap;

// Web-only: maps arrive as fetched `.kpmap` bytes through the asset
// server, and the deploy's map list is embedded at build time.
#[cfg(target_arch = "wasm32")]
mod bytes_asset;
#[cfg(target_arch = "wasm32")]
use bytes_asset::BytesAsset;
#[cfg(target_arch = "wasm32")]
include!(concat!(env!("OUT_DIR"), "/web_map_catalog.rs"));

#[cfg(not(target_arch = "wasm32"))]
use lua_compositing::spawn_lua_compositing;
use mipmap::{build_terrain_material_from_texture, dark_fallback_material};

pub struct MapLoadingPlugin;

/// Set containing the world teardown+rebuild pair. UI systems that hold
/// entity references across frames (info panel, order palette, build
/// menu, placement ghost) must order themselves `.after(Self::*)` this —
/// otherwise a rebuild in the same frame can despawn entities their
/// queued commands still reference, which panics at command-apply time.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GameWorldRebuild;

impl Plugin for MapLoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, pick_map.after(crate::rendering::camera::spawn_camera));

        #[cfg(not(target_arch = "wasm32"))]
        {
            app.add_systems(
                OnEnter(crate::game_setup::AppState::InGame),
                (prepare_game_entry, load_map)
                    .chain()
                    .in_set(GameWorldRebuild),
            )
            // Restart / demo reload while in any state: the menu writes
            // `RunGame` (optionally with a fresh `GameSetup`) and we tear
            // down + rebuild the world in-place. Ordered after the menu's
            // writers so their `GameSetup` inserts are applied before we
            // read it — otherwise the boot demo would run with the
            // default (non-demo) setup and spawn homebases.
            .add_systems(
                Update,
                (prepare_game_entry, load_map)
                    .chain()
                    .in_set(GameWorldRebuild)
                    .run_if(rerun_requested)
                    .after(crate::ui::menu::boot_demo),
            );
        }

        // Web: no filesystem — the world is built from a fetched
        // `.kpmap`. `prepare_game_entry` requests the asset; the arrival
        // system polls every frame and spawns once the bytes land.
        #[cfg(target_arch = "wasm32")]
        {
            use bevy::asset::AssetApp;
            app.init_asset::<BytesAsset>()
                .register_asset_loader(bytes_asset::BytesLoader)
                .init_resource::<PendingWebMapLoad>()
                .add_systems(
                    OnEnter(crate::game_setup::AppState::InGame),
                    prepare_game_entry.in_set(GameWorldRebuild),
                )
                .add_systems(
                    Update,
                    (prepare_game_entry.run_if(rerun_requested), web_map_arrival)
                        .chain()
                        .in_set(GameWorldRebuild)
                        .after(crate::ui::menu::boot_demo),
                );
        }
    }
}

/// Run-condition helper for the in-game Restart path: true on any frame
/// where the menu issued a `RunGame`.
fn rerun_requested(mut reader: MessageReader<RunGame>) -> bool {
    reader.read().next().is_some()
}

/// Path to the map archive loaded for this session.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource)]
struct SelectedMap(PathBuf);

/// Web: in-flight `.kpmap` fetch, requested by [`prepare_game_entry`]
/// and consumed by [`web_map_arrival`] when the payload lands.
#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
struct PendingWebMapLoad(Option<Handle<BytesAsset>>);

/// Marker for entities that survive game-world teardown (menu UI):
/// the launch menu, Esc overlay, and game-over panel all carry it so
/// [`despawn_game_world`] spares them while clearing the match.
#[derive(Component)]
pub struct PersistentEntity;

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
/// defeat) passes through here: tear down any previous game world, reset
/// the in-game state machine, resolve the map path from the setup.
/// Exclusive system — it mutates the world directly for the teardown.
fn prepare_game_entry(world: &mut World) {
    use crate::game_setup::GameOverDismissed;
    use crate::units::lifecycle::game_over::GameState;

    // The local player is always seat 0 / team 0 in the setups the menu
    // builds.
    world.resource_mut::<crate::units::player::LocalTeam>().0 = 0;

    // Fresh in-game state: `Playing`, game-over panel re-armed.
    world.resource_mut::<NextState<GameState>>().set(GameState::Playing);
    world.resource_mut::<GameOverDismissed>().0 = false;

    // Resolve the setup's map name against the catalog.
    let setup = world.resource::<GameSetup>().clone();
    let catalog = world.resource::<MapCatalog>().0.clone();
    let path = catalog
        .iter()
        .find(|p| {
            p.file_stem()
                .map(|s| s.to_string_lossy() == setup.map)
                .unwrap_or(false)
        })
        .or_else(|| catalog.first())
        .cloned();
    match path {
        Some(p) => {
            info!("Preparing match on {} ({})", setup.map, p.display());
            #[cfg(not(target_arch = "wasm32"))]
            {
                world.resource_mut::<SelectedMap>().0 = p;
            }
            #[cfg(target_arch = "wasm32")]
            {
                // Catalog entries are already asset-relative paths
                // (`maps/<stem>.kpmap`) — request the fetch.
                let handle: Handle<BytesAsset> = world
                    .resource::<AssetServer>()
                    .load(p.to_string_lossy().into_owned());
                world.resource_mut::<PendingWebMapLoad>().0 = Some(handle);
            }
        }
        None => {
            error!("Map catalog is empty — cannot start a game");
            return;
        }
    }

    // Tear down the previous game world (no-op on first entry). Kept
    // entities: windows, the RTS camera (and its children), and anything
    // tagged `PersistentEntity` (menu UI).
    despawn_game_world(world);
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

    // Pull kept roots' descendants into the keep set (camera children,
    // UI trees).
    loop {
        let mut grew = false;
        let mut relations = world.query_filtered::<(Entity, &ChildOf), ()>();
        for (e, child_of) in relations.iter(world) {
            if keep.contains(&child_of.parent()) && keep.insert(e) {
                grew = true;
            }
        }
        if !grew {
            break;
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
                .map(|n| match n.to_ascii_lowercase().as_str() {
                    "system" => Some(crate::units::components::Faction::System),
                    "hacker" => Some(crate::units::components::Faction::Hacker),
                    "network" => Some(crate::units::components::Faction::Network),
                    _ => None,
                })
                .flatten()
        }) {
            setup = crate::game_setup::showcase_setup(faction);
            auto_enter = true;
            info!("CLI showcase argument: {:?}", faction);
        }

        commands.insert_resource(MapCatalog(maps.clone()));
        commands.insert_resource(SelectedMap(
            maps.first().cloned().expect("maps is non-empty"),
        ));
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
/// Native: read the selected archive (baked `.kpmap` preferred), then
/// build the world. Web uses [`web_map_arrival`] instead.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn load_map(
    selected: Res<SelectedMap>,
    setup: Res<GameSetup>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut fog_query: Query<&mut DistanceFog, With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
    mut geovent_assets: ResMut<GeoventAssets>,
    mut ctx: crate::units::lifecycle::spawning::SpawnContext,
) {
    let map_path = &selected.0;
    let map_name = map_path.file_stem().unwrap_or_default().to_string_lossy().into_owned();

    info!("Loading map: {map_name}");

    let spring_map = match load_map_dispatch(map_path) {
        Ok(m) => m,
        Err(error) => {
            error!("Failed to load {}: {error}", map_path.display());
            return;
        }
    };

    spawn_map_world(
        spring_map,
        &setup,
        &map_name,
        &mut camera_query,
        &mut fog_query,
        &mut map_bounds,
        &mut geovent_assets,
        &mut ctx,
    );
}

/// Web: poll the in-flight `.kpmap` fetch; once the bytes have landed,
/// decode and build the world. A failed fetch logs an error and clears
/// the pending state so the menu can retry.
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
fn web_map_arrival(
    mut pending: ResMut<PendingWebMapLoad>,
    assets: Res<Assets<BytesAsset>>,
    server: Res<AssetServer>,
    setup: Res<GameSetup>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut fog_query: Query<&mut DistanceFog, With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
    mut geovent_assets: ResMut<GeoventAssets>,
    mut ctx: crate::units::lifecycle::spawning::SpawnContext,
) {
    use bevy::asset::LoadState;

    let Some(handle) = pending.0.as_ref() else {
        return;
    };
    match server.load_state(handle) {
        LoadState::Loaded => {}
        LoadState::Failed(error) => {
            error!("Map fetch failed: {error}");
            pending.0 = None;
            return;
        }
        _ => return, // still fetching / deps loading
    }
    let Some(asset) = assets.get(handle) else {
        return;
    };

    let map_name = setup.map.clone();
    info!("Received {} ({} baked bytes)", map_name, asset.0.len());
    let spring_map = match spring_map::baked::read_baked_map(&asset.0) {
        Ok(m) => m,
        Err(error) => {
            error!("Failed to decode baked map {map_name}: {error}");
            pending.0 = None;
            return;
        }
    };

    spawn_map_world(
        spring_map,
        &setup,
        &map_name,
        &mut camera_query,
        &mut fog_query,
        &mut map_bounds,
        &mut geovent_assets,
        &mut ctx,
    );
    pending.0 = None;
}

/// Everything after "I have a `SpringMap` in hand": terrain, atmosphere,
/// fog, nav grids, minimap, homebases, per-map events, heightmap
/// resource. Shared by the native file path and the web baked-bytes
/// path.
#[allow(clippy::too_many_arguments)]
fn spawn_map_world(
    spring_map: spring_map::SpringMap,
    setup: &GameSetup,
    map_name: &str,
    camera_query: &mut Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    fog_query: &mut Query<&mut DistanceFog, With<RtsCamera>>,
    map_bounds: &mut ResMut<MapBounds>,
    geovent_assets: &mut GeoventAssets,
    ctx: &mut crate::units::lifecycle::spawning::SpawnContext,
) {
    let parsed = &spring_map.parsed;

    info!(
        "  {}x{} (heightmap {}x{}), {} features",
        parsed.header.map_x,
        parsed.header.map_y,
        parsed.header.heightmap_width(),
        parsed.header.heightmap_height(),
        parsed.features.len(),
    );

    let terrain_material = match &spring_map.ground_texture {
        Some(ground) => {
            build_terrain_material_from_texture(ground, &mut ctx.images, &mut ctx.materials)
        }
        None => {
            warn!("No ground texture — using fallback");
            dark_fallback_material(&mut *ctx.materials)
        }
    };

    setup_camera(parsed, camera_query, &mut **map_bounds);

    // Check actual height variance, not header values (gadgets may have modified the terrain).
    let min_actual = parsed.heights.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_actual = parsed
        .heights
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max);
    if (max_actual - min_actual) < 1.0 {
        warn!(
            "  Terrain is effectively flat (height range: {:.1})",
            max_actual - min_actual
        );
    }

    let heightmap = Heightmap::from_parsed(parsed);

    spawn_terrain(
        parsed,
        &heightmap,
        terrain_material,
        &mut ctx.commands,
        &mut ctx.meshes,
        &mut ctx.materials,
        &mut ctx.images,
        geovent_assets,
    );

    // HexFarm Lua-composited decorations: native-only (see module docs).
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(compositing) = &spring_map.lua_compositing {
        spawn_lua_compositing(
            compositing,
            &mut ctx.commands,
            &mut ctx.meshes,
            &mut ctx.materials,
            &mut ctx.images,
        );
    }

    // One pathfinding grid per distinct unit `MaxSlope`. Caps and
    // slope-mods are in Spring's encoding — see `cost.rs`.
    // `compute_path` picks the tightest bucket whose cap ≥ the unit's.
    {
        use spring_pathfinding::{NodeLayer, SpeedMap, slope_mod_from_max_slope};
        use std::collections::BTreeSet;

        use crate::units::content::definitions::ALL_UNIT_KINDS;
        use crate::units::content::unit_registry::DEFAULT_MAX_SLOPE_DEGREES;

        // Why: bin to 4 decimals so float jitter doesn't split
        // near-identical buckets.
        const BUCKET_QUANTUM: f32 = 10_000.0;

        let mut distinct_caps = BTreeSet::<u32>::new();
        for &kind in ALL_UNIT_KINDS {
            let cap = ctx.unit_registry.max_slope_ratio(kind);
            distinct_caps.insert((cap * BUCKET_QUANTUM).round() as u32);
        }
        // Always keep the KP-default bucket (FBI MaxSlope=36 from
        // `MOVEINFO.TDF`'s LIGHT/MEDIUM/HEAVY) available for units
        // whose FBI omits `MaxSlope`.
        let default_cap = spring_pathfinding::max_slope_from_degrees(DEFAULT_MAX_SLOPE_DEGREES);
        distinct_caps.insert((default_cap * BUCKET_QUANTUM).round() as u32);

        let mut nav_set = interaction::movement::NavGridSet::default();
        for cap_q in distinct_caps {
            let cap = cap_q as f32 / BUCKET_QUANTUM;
            let slope_mod = slope_mod_from_max_slope(cap);
            let speed_map = SpeedMap::from_heightmap(
                &parsed.heights,
                parsed.header.heightmap_width() as u32,
                parsed.header.heightmap_height() as u32,
                cap,
                slope_mod,
            );
            let layer = NodeLayer::new(&speed_map);
            info!(
                "  Nav bucket max_slope={:.3} (slope_mod={:.2}): {} leaf nodes from {}x{} speed map",
                cap,
                slope_mod,
                layer.leaf_count(),
                speed_map.width,
                speed_map.height,
            );
            nav_set.buckets.push(interaction::movement::NavBucket {
                max_slope: cap,
                layer,
            });
        }
        // Buckets already ascending because BTreeSet iteration is sorted.
        ctx.commands.insert_resource(nav_set);
    }

    // Setup minimap from ground texture.
    {
        let (gp, gw, gh) = match &spring_map.ground_texture {
            Some(g) => (Some(g.pixels.as_slice()), g.width, g.height),
            None => (None, 0, 0),
        };
        ui::minimap::setup_minimap(
            &mut ctx.commands,
            &mut *ctx.images,
            gp,
            gw,
            gh,
            parsed.header.world_width(),
            parsed.header.world_depth(),
        );
    }

    if let Some(map_info) = &spring_map.map_info {
        apply_atmosphere(map_info, &mut ctx.commands);
        apply_fog(map_info, parsed, fog_query);
        if setup.demo {
            // Attract-mode demo: no bases, no win/lose — the menu's
            // demo director (ui::menu) spawns the cast and replaces
            // losses.
            info!("  Demo match — skipping base spawn");
            // Clear any leftover showcase director from a previous game.
            ctx.commands
                .remove_resource::<crate::showcase::ShowcaseDirector>();
        } else if let Some(faction) = setup.showcase {
            spawn_showcase_homebase(&heightmap, map_info, faction, ctx);
            ctx.commands
                .insert_resource(crate::showcase::ShowcaseDirector::new(faction));
            info!("  Showcase({:?}) — skipping full roster", faction);
        } else {
            spawn_homebases(&heightmap, map_info, ctx);
            // Clear any leftover showcase director from a previous game.
            ctx.commands
                .remove_resource::<crate::showcase::ShowcaseDirector>();
        }
        configure_map_events(map_name, map_info, &heightmap, &mut ctx.commands);
        let datavent_count = parsed
            .features
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
}


fn setup_camera(
    parsed: &ParsedMap,
    camera_query: &mut Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    map_bounds: &mut MapBounds,
) {
    let world_w = parsed.header.world_width();
    let world_d = parsed.header.world_depth();
    let heightmap_w = parsed.header.heightmap_width();
    let heightmap_h = parsed.header.heightmap_height();
    let center_height = parsed.heights[(heightmap_h / 2) * heightmap_w + heightmap_w / 2];

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
    map: &ParsedMap,
    heightmap: &Heightmap,
    terrain_material: Handle<StandardMaterial>,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    std_materials: &mut ResMut<Assets<StandardMaterial>>,
    images: &mut ResMut<Assets<Image>>,
    geovent_assets: &mut GeoventAssets,
) {
    let chunks = generate_terrain_chunks(map);
    info!("  Spawning {} terrain chunks", chunks.len());

    for chunk in chunks {
        let mesh_handle = meshes.add(chunk.mesh);
        commands.spawn((
            Mesh3d(mesh_handle),
            MeshMaterial3d(terrain_material.clone()),
            Transform::from_translation(chunk.translation),
        ));
    }

    spawn_geovent_smokers(
        map,
        heightmap,
        commands,
        geovent_assets,
        meshes,
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
    if map_name.eq_ignore_ascii_case("Stack_Overflow") {
        let starts: Vec<Vec3> = map_info
            .start_positions
            .iter()
            .map(|sp| heightmap.place(sp.x, sp.z))
            .collect();
        info!("  Stack_Overflow detected — installing eruption schedule");
        commands.insert_resource(crate::map_events::EruptionConfig::stack_overflow(starts));
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
    parsed: &ParsedMap,
    fog_query: &mut Query<&mut DistanceFog, With<RtsCamera>>,
) {
    let Ok(mut fog) = fog_query.single_mut() else {
        return;
    };
    let color = map_info.atmosphere.fog_color;
    let fog_start_frac = map_info.atmosphere.fog_start;
    let world_w = parsed.header.world_width();
    let world_d = parsed.header.world_depth();
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

//! Map loading: pick a map archive from `assets/maps/` and spawn its
//! terrain, atmosphere, fog, nav grid, minimap, and homebases at startup.
//!
//! Exposes [`MapLoadingPlugin`], which loads the CLI-selected map at Startup.
//! Terrain-material construction (mipmap pyramid + fallback) lives in
//! [`mipmap`] so the orchestrator stays focused on sequencing.

use std::path::{Path, PathBuf};

use bevy::prelude::*;

use crate::{
    interaction,
    rendering::camera::{MapBounds, RtsCamera, RtsCameraState, compute_transform_from_state},
    terrain::{
        geovent::{GeoventAssets, spawn_geovent_smokers},
        heightmap::Heightmap,
        mesh::generate_terrain_chunks,
    },
    ui,
    units::{
        assets::{animation::CobFileCache, meshes::S3OModelCache},
        content::unit_registry::UnitRegistry,
        lifecycle::spawning::spawn_homebases,
    },
};
use spring_map::{map_types::ParsedMap, smd_parser::MapInfo};

mod mipmap;

use mipmap::{build_terrain_material_from_texture, dark_fallback_material};

pub struct MapLoadingPlugin;

impl Plugin for MapLoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Startup,
            (
                pick_map,
                load_map.after(crate::rendering::camera::spawn_camera),
            )
                .chain(),
        );
    }
}

/// Path to the map archive loaded for this session.
#[derive(Resource)]
struct SelectedMap(PathBuf);

/// Discover the map archives in `assets/maps/` and pick the one named
/// by the CLI arg (or the first alphabetically). If both a baked
/// `.kpmap` and a source `.sd7`/`.sdz` exist for the same stem, the
/// baked form wins — no archive extraction or Lua execution at
/// startup, and the WASM target (plan §8.1) can't run those anyway.
///
/// If the CLI arg is itself a usable map file (absolute or
/// cwd-relative), we take it verbatim — no directory search needed.
/// Otherwise we look up the maps directory relative to the project
/// root (see `paths::project_root`), so the binary works regardless
/// of where it was launched from.
fn pick_map(mut commands: Commands) {
    if let Some(direct) = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .filter(|p| p.is_file() && is_map_ext(p.as_path()))
    {
        info!("Loading map: {}", direct.display());
        commands.insert_resource(SelectedMap(direct));
        return;
    }

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

    // Match the CLI arg (if any) by filename stem — so a bare map name,
    // `name.sdz`, and a full path all pick the same entry.
    let initial = std::env::args()
        .nth(1)
        .and_then(|arg| {
            let stem = PathBuf::from(&arg)
                .file_stem()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            maps.iter().position(|p| {
                p.file_stem()
                    .map(|s| s.to_ascii_lowercase() == stem)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(0);

    let selected = maps.into_iter().nth(initial).unwrap();
    info!("Loading map: {}", selected.display());
    commands.insert_resource(SelectedMap(selected));
}

fn is_map_ext(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("sd7") | Some("sdz") | Some("kpmap")
    )
}

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

/// Dispatch on file extension: `.kpmap` reads the postcard payload via
/// the baked loader (no archive / Lua deps), anything else hits the
/// full `.sd7`/`.sdz` pipeline.
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

/// When a `.kpmap` and an `.sd7`/`.sdz` share the same file stem, drop
/// the source archive — the baked form has the same content and loads
/// without 7z / Lua. Side-effect-free if the baked form is absent.
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

#[allow(clippy::too_many_arguments)]
fn load_map(
    selected: Res<SelectedMap>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut fog_query: Query<&mut DistanceFog, With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    mut geovent_assets: ResMut<GeoventAssets>,
    unit_registry: Res<UnitRegistry>,
) {
    let map_path = &selected.0;
    let map_name = map_path.file_stem().unwrap_or_default().to_string_lossy();

    info!("Loading map: {map_name}");

    let spring_map = match load_map_dispatch(map_path) {
        Ok(m) => m,
        Err(error) => {
            error!("Failed to load {}: {error}", map_path.display());
            return;
        }
    };

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
            build_terrain_material_from_texture(ground, &mut images, &mut std_materials)
        }
        None => {
            warn!("No ground texture — using fallback");
            dark_fallback_material(&mut std_materials)
        }
    };

    setup_camera(parsed, &mut camera_query, &mut map_bounds);

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
        &mut commands,
        &mut meshes,
        &mut std_materials,
        &mut images,
        &mut geovent_assets,
    );

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
            let cap = unit_registry.max_slope_ratio(kind);
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
        commands.insert_resource(nav_set);
    }

    // Setup minimap from ground texture.
    {
        let (gp, gw, gh) = match &spring_map.ground_texture {
            Some(g) => (Some(g.pixels.as_slice()), g.width, g.height),
            None => (None, 0, 0),
        };
        ui::minimap::setup_minimap(
            &mut commands,
            &mut images,
            gp,
            gw,
            gh,
            parsed.header.world_width(),
            parsed.header.world_depth(),
        );
    }

    if let Some(map_info) = &spring_map.map_info {
        apply_atmosphere(map_info, &mut commands);
        apply_fog(map_info, parsed, &mut fog_query);
        spawn_homebases(
            &heightmap,
            map_info,
            &mut commands,
            &mut meshes,
            &mut std_materials,
            &mut images,
            &mut model_cache,
            &mut cob_cache,
            &unit_registry,
        );
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

    commands.insert_resource(heightmap);
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

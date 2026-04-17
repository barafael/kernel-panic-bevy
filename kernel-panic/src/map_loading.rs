//! Map loading: catalog discovery, hotkey cycling, and per-map spawn of
//! terrain, atmosphere, fog, nav grid, minimap, and homebases/showcase units.
//!
//! Exposes [`MapLoadingPlugin`], which:
//! - Discovers `.sd7` / `.sdz` archives in `assets/maps/` at Startup.
//! - Loads the initial map (honoring an optional CLI positional arg).
//! - Cycles through maps on `]` / `[` at runtime, despawning the previous
//!   map's entities first.
//! - Runs an `apply_pending_fog` Update system that copies the map's fog
//!   settings onto the camera once it exists.

use std::path::PathBuf;

use bevy::prelude::*;

use crate::interaction;
use crate::rendering::camera::{
    MapBounds, RtsCamera, RtsCameraState, compute_transform_from_state,
};
use crate::terrain::geovent::{GeoventAssets, spawn_geovent_smokers};
use crate::terrain::heightmap::Heightmap;
use crate::terrain::material::create_terrain_material;
use crate::terrain::mesh::generate_terrain_chunks;
use crate::ui;
use crate::units::animation::CobFileCache;
use crate::units::game_over::{GameOverUi, GameState};
use crate::units::meshes::S3OModelCache;
use crate::units::spawning::{spawn_homebases, spawn_showcase};
use crate::units::unit_registry::UnitRegistry;
use spring_map::map_types::{GroundTexture, MipmapData, ParsedMap};
use spring_map::smd_parser::MapInfo;

pub struct MapLoadingPlugin;

impl Plugin for MapLoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Startup,
            (
                discover_maps,
                load_current_map.after(crate::rendering::camera::spawn_camera),
            )
                .chain(),
        )
        .add_systems(Update, (cycle_map_on_keypress, apply_pending_fog));
    }
}

/// Marker component for all entities spawned by map loading.
/// Used to despawn everything when switching maps.
#[derive(Component)]
pub struct MapEntity;

/// Tracks available maps and the current selection.
#[derive(Resource)]
struct MapCatalog {
    maps: Vec<PathBuf>,
    current: usize,
}

/// Discover all .sd7/.sdz map files and pick the initial one.
fn discover_maps(mut commands: Commands) {
    let candidates = [
        PathBuf::from("kernel-panic/assets/maps"),
        PathBuf::from("assets/maps"),
    ];
    let maps_dir = candidates.iter().find(|p| p.is_dir());

    let mut maps: Vec<PathBuf> = Vec::new();

    if let Some(Ok(entries)) = maps_dir.map(std::fs::read_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "sd7" || ext == "sdz" {
                maps.push(path);
            }
        }
    }

    maps.sort();

    // If a CLI arg was given, find it in the list and set as current.
    // Match by canonicalised path when possible, falling back to file
    // name comparison so users can pass `Showcase`, `Showcase.sdz`, or
    // any of the equivalent relative/absolute path spellings.
    let initial = std::env::args()
        .nth(1)
        .and_then(|arg| {
            let arg_path = PathBuf::from(&arg);
            let arg_canonical = arg_path.canonicalize().ok();
            let arg_stem = arg_path
                .file_stem()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            maps.iter().position(|p| {
                if let Some(ref c) = arg_canonical
                    && let Ok(pc) = p.canonicalize()
                    && pc == *c
                {
                    return true;
                }
                p.file_stem()
                    .map(|s| s.to_ascii_lowercase() == arg_stem)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(0);

    if maps.is_empty() {
        error!("No map files found. Place .sd7/.sdz files in assets/maps/");
        std::process::exit(1);
    }

    info!(
        "Found {} maps, starting with: {}",
        maps.len(),
        maps[initial].display()
    );
    commands.insert_resource(MapCatalog {
        maps,
        current: initial,
    });
}

#[allow(clippy::too_many_arguments)]
fn cycle_map_on_keypress(
    keys: Res<ButtonInput<KeyCode>>,
    mut catalog: ResMut<MapCatalog>,
    map_entities: Query<Entity, With<MapEntity>>,
    game_over_ui: Query<Entity, With<GameOverUi>>,
    mut next_game_state: ResMut<NextState<GameState>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    mut geovent_assets: ResMut<GeoventAssets>,
    unit_registry: Res<UnitRegistry>,
) {
    let changed = if keys.just_pressed(KeyCode::BracketRight) {
        catalog.current = (catalog.current + 1) % catalog.maps.len();
        true
    } else if keys.just_pressed(KeyCode::BracketLeft) {
        catalog.current = (catalog.current + catalog.maps.len() - 1) % catalog.maps.len();
        true
    } else {
        false
    };

    if !changed {
        return;
    }

    // Despawn all existing map entities.
    for entity in &map_entities {
        commands.entity(entity).despawn();
    }

    // Reset game state so the new map starts fresh.
    next_game_state.set(GameState::Playing);
    for entity in &game_over_ui {
        commands.entity(entity).despawn();
    }

    load_map_at_index(
        &catalog,
        &mut commands,
        &mut meshes,
        &mut std_materials,
        &mut images,
        &mut camera_query,
        &mut map_bounds,
        &mut model_cache,
        &mut cob_cache,
        &mut geovent_assets,
        &unit_registry,
    );
}

/// Initial map load at startup.
#[allow(clippy::too_many_arguments)]
fn load_current_map(
    catalog: Res<MapCatalog>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    mut geovent_assets: ResMut<GeoventAssets>,
    unit_registry: Res<UnitRegistry>,
) {
    load_map_at_index(
        &catalog,
        &mut commands,
        &mut meshes,
        &mut std_materials,
        &mut images,
        &mut camera_query,
        &mut map_bounds,
        &mut model_cache,
        &mut cob_cache,
        &mut geovent_assets,
        &unit_registry,
    );
}

#[allow(clippy::too_many_arguments)]
fn load_map_at_index(
    catalog: &MapCatalog,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    std_materials: &mut ResMut<Assets<StandardMaterial>>,
    images: &mut ResMut<Assets<Image>>,
    camera_query: &mut Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    map_bounds: &mut ResMut<MapBounds>,
    model_cache: &mut ResMut<S3OModelCache>,
    cob_cache: &mut ResMut<CobFileCache>,
    geovent_assets: &mut GeoventAssets,
    unit_registry: &UnitRegistry,
) {
    let map_path = &catalog.maps[catalog.current];
    let map_name = map_path.file_stem().unwrap_or_default().to_string_lossy();

    info!(
        "Loading map [{}/{}]: {map_name}",
        catalog.current + 1,
        catalog.maps.len()
    );

    let spring_map = match spring_map::load_map(map_path) {
        Ok(m) => m,
        Err(err) => {
            error!("Failed to load {}: {err}", map_path.display());
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
        Some(ground) => build_terrain_material_from_texture(ground, images, std_materials),
        None => {
            warn!("No ground texture — using fallback");
            dark_fallback_material(std_materials)
        }
    };

    setup_camera(parsed, camera_query, map_bounds);

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
        commands,
        meshes,
        std_materials,
        images,
        geovent_assets,
    );

    // Build pathfinding grid from heightmap.
    {
        let speed_map = spring_pathfinding::SpeedMap::from_heightmap(
            &parsed.heights,
            parsed.header.heightmap_width() as u32,
            parsed.header.heightmap_height() as u32,
            // max_slope = rise/run above which terrain is impassable. 3.0
            // (~72°) only blocks near-vertical cliffs, leaving all climbable
            // hills open. Keeps `0.8` (the previous value) from detouring
            // around every modest rise.
            3.0,
            // slope_mod = how much slope slows travel. Spring's default is
            // 40, which leaves cliff-edge shortcuts cheaper than long flat
            // detours. Bumping heavily so the pathfinder prefers gentle
            // routes when they exist but still crosses a cliff if that's
            // the only way.
            400.0,
        );
        let node_layer = spring_pathfinding::NodeLayer::new(&speed_map);
        info!(
            "  Nav grid: {} leaf nodes from {}x{} speed map",
            node_layer.leaf_count(),
            speed_map.width,
            speed_map.height,
        );
        commands.insert_resource(interaction::movement::NavGrid(node_layer));
    }

    // Setup minimap from ground texture.
    {
        let (gp, gw, gh) = match &spring_map.ground_texture {
            Some(g) => (Some(g.pixels.as_slice()), g.width, g.height),
            None => (None, 0, 0),
        };
        ui::minimap::setup_minimap(
            commands,
            images,
            gp,
            gw,
            gh,
            parsed.header.world_width(),
            parsed.header.world_depth(),
        );
    }

    if let Some(map_info) = &spring_map.map_info {
        apply_atmosphere(map_info, commands);
        apply_fog(map_info, parsed, commands);
        if map_name.eq_ignore_ascii_case("Showcase") {
            spawn_showcase(
                &heightmap,
                map_info,
                commands,
                meshes,
                std_materials,
                images,
                model_cache,
                cob_cache,
                unit_registry,
            );
        } else {
            spawn_homebases(
                &heightmap,
                map_info,
                commands,
                meshes,
                std_materials,
                images,
                model_cache,
                cob_cache,
                unit_registry,
            );
        }
        info!(
            "  {} start positions, gravity={}",
            map_info.start_positions.len(),
            map_info.gravity,
        );
    }

    commands.insert_resource(heightmap);
}

fn setup_camera(
    parsed: &ParsedMap,
    camera_query: &mut Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    map_bounds: &mut ResMut<MapBounds>,
) {
    let world_w = parsed.header.world_width();
    let world_d = parsed.header.world_depth();
    let heightmap_w = parsed.header.heightmap_width();
    let heightmap_h = parsed.header.heightmap_height();
    let center_height = parsed.heights[(heightmap_h / 2) * heightmap_w + heightmap_w / 2];

    **map_bounds =
        MapBounds::from_map_extents(Vec3::new(0.0, 0.0, 0.0), Vec3::new(world_w, 0.0, world_d));

    let map_extent = world_w.max(world_d);
    if let Ok((mut cam_state, mut cam_transform)) = camera_query.single_mut() {
        let focus = Vec3::new(world_w / 2.0, center_height, world_d / 2.0);
        let distance = map_extent * 0.5;
        cam_state.snap_to(focus, distance);
        *cam_transform = compute_transform_from_state(&cam_state);
    }
}

fn dark_fallback_material(materials: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::srgb(0.02, 0.02, 0.02),
        unlit: true,
        ..default()
    })
}

fn build_terrain_material_from_texture(
    ground: &GroundTexture,
    images: &mut ResMut<Assets<Image>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) -> Handle<StandardMaterial> {
    let MipmapData {
        pixels: mipmap_pixels,
        level_count: mip_levels,
    } = generate_mipmaps(&ground.pixels, ground.width, ground.height);

    let size = bevy::render::render_resource::Extent3d {
        width: ground.width as u32,
        height: ground.height as u32,
        depth_or_array_layers: 1,
    };
    let format = bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb;
    let usage =
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD;

    let mut image = Image::new_uninit(
        size,
        bevy::render::render_resource::TextureDimension::D2,
        format,
        usage,
    );
    image.data = Some(mipmap_pixels);
    image.texture_descriptor.mip_level_count = mip_levels;
    image.sampler = bevy::image::ImageSampler::Descriptor(bevy::image::ImageSamplerDescriptor {
        min_filter: bevy::image::ImageFilterMode::Linear,
        mag_filter: bevy::image::ImageFilterMode::Linear,
        mipmap_filter: bevy::image::ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });

    info!(
        "  Texture: {}x{}, {mip_levels} mip levels",
        ground.width, ground.height,
    );

    let texture_handle = images.add(image);
    create_terrain_material(texture_handle, materials)
}

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
            MapEntity,
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
        MapEntity,
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

/// Fog settings from the map, stored as a resource. A system applies it to the camera.
#[derive(Resource)]
struct MapFogSettings {
    color: Color,
    start: f32,
    end: f32,
}

/// Apply pending fog settings to the camera's DistanceFog component.
fn apply_pending_fog(
    settings: Option<Res<MapFogSettings>>,
    mut camera_q: Query<&mut DistanceFog, With<RtsCamera>>,
    mut commands: Commands,
) {
    let Some(settings) = settings else { return };
    let Ok(mut fog) = camera_q.single_mut() else {
        return;
    };

    fog.color = settings.color;
    fog.falloff = FogFalloff::Linear {
        start: settings.start,
        end: settings.end,
    };
    commands.remove_resource::<MapFogSettings>();
}

/// Queue fog update from map atmosphere.
///
/// Fog end scales with the map diagonal so large maps (like the showcase
/// plain) don't get walled off by haze a few grid cells from the camera.
/// Spring's `FogStart` is a fraction of that end distance.
fn apply_fog(map_info: &MapInfo, parsed: &ParsedMap, commands: &mut Commands) {
    let fog = map_info.atmosphere.fog_color;
    let fog_start_frac = map_info.atmosphere.fog_start;
    let world_w = parsed.header.world_width();
    let world_d = parsed.header.world_depth();
    let diagonal = (world_w * world_w + world_d * world_d).sqrt();
    // Cover the full map diagonal + a bit more so the far edge never fogs
    // completely. Floor at 4000 elmos for small maps.
    let max_view_distance = (diagonal * 1.1).max(4000.0);

    commands.insert_resource(MapFogSettings {
        color: Color::linear_rgb(fog[0], fog[1], fog[2]),
        start: fog_start_frac * max_view_distance,
        end: max_view_distance,
    });
}

fn generate_mipmaps(pixels: &[u8], width: usize, height: usize) -> MipmapData {
    let mut all_data = Vec::with_capacity(pixels.len() * 4 / 3);
    all_data.extend_from_slice(pixels);
    let mut levels = 1u32;

    let mut current_w = width;
    let mut current_h = height;
    let mut src = pixels.to_vec();

    while current_w > 1 || current_h > 1 {
        let next_w = (current_w / 2).max(1);
        let next_h = (current_h / 2).max(1);
        let mut dst = vec![0u8; next_w * next_h * 4];

        for y in 0..next_h {
            for x in 0..next_w {
                let src_x = (x * 2).min(current_w - 1);
                let src_y = (y * 2).min(current_h - 1);
                let src_x1 = (src_x + 1).min(current_w - 1);
                let src_y1 = (src_y + 1).min(current_h - 1);

                let i00 = (src_y * current_w + src_x) * 4;
                let i10 = (src_y * current_w + src_x1) * 4;
                let i01 = (src_y1 * current_w + src_x) * 4;
                let i11 = (src_y1 * current_w + src_x1) * 4;

                for channel in 0..4 {
                    let avg = (src[i00 + channel] as u16
                        + src[i10 + channel] as u16
                        + src[i01 + channel] as u16
                        + src[i11 + channel] as u16)
                        / 4;
                    dst[(y * next_w + x) * 4 + channel] = avg as u8;
                }
            }
        }

        all_data.extend_from_slice(&dst);
        levels += 1;
        src = dst;
        current_w = next_w;
        current_h = next_h;
    }

    MipmapData {
        pixels: all_data,
        level_count: levels,
    }
}

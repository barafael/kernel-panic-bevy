mod rendering;
mod terrain;

use std::path::PathBuf;

use bevy::prelude::*;

use rendering::RenderingPlugin;
use rendering::camera::{MapBounds, RtsCamera, RtsCameraState};
use spring_map::map_types::{GroundTexture, ParsedMap, SQUARE_SIZE};
use spring_map::smd_parser::MapInfo;
use terrain::material::{create_datavent_material, create_terrain_material};
use terrain::mesh::generate_terrain_chunks;

/// Marker component for all entities spawned by map loading.
/// Used to despawn everything when switching maps.
#[derive(Component)]
struct MapEntity;

/// Tracks available maps and the current selection.
#[derive(Resource)]
struct MapCatalog {
    maps: Vec<PathBuf>,
    current: usize,
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Kernel Panic".to_string(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(RenderingPlugin)
        .add_systems(
            Startup,
            (
                discover_maps,
                load_current_map.after(rendering::camera::spawn_camera),
            )
                .chain(),
        )
        .add_systems(Update, cycle_map_on_keypress)
        .run();
}

/// Discover all .sd7/.sdz map files and pick the initial one.
fn discover_maps(mut commands: Commands) {
    let candidates = [
        PathBuf::from("kernel-panic/assets/maps"),
        PathBuf::from("assets/maps"),
    ];
    let maps_dir = candidates.iter().find(|p| p.is_dir());

    let mut maps: Vec<PathBuf> = Vec::new();

    if let Some(dir) = maps_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
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
    }

    maps.sort();

    // If a CLI arg was given, find it in the list and set as current.
    let initial = std::env::args()
        .nth(1)
        .and_then(|arg| {
            let arg_path = PathBuf::from(&arg);
            maps.iter().position(|p| p == &arg_path)
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

/// Watch for `/` (next map) and `\` (previous map) keypresses.
fn cycle_map_on_keypress(
    keys: Res<ButtonInput<KeyCode>>,
    mut catalog: ResMut<MapCatalog>,
    map_entities: Query<Entity, With<MapEntity>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
) {
    let changed = if keys.just_pressed(KeyCode::Slash) {
        catalog.current = (catalog.current + 1) % catalog.maps.len();
        true
    } else if keys.just_pressed(KeyCode::Backslash) {
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

    load_map_at_index(
        &catalog,
        &mut commands,
        &mut meshes,
        &mut std_materials,
        &mut images,
        &mut camera_query,
        &mut map_bounds,
    );
}

/// Initial map load at startup.
fn load_current_map(
    catalog: Res<MapCatalog>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
) {
    load_map_at_index(
        &catalog,
        &mut commands,
        &mut meshes,
        &mut std_materials,
        &mut images,
        &mut camera_query,
        &mut map_bounds,
    );
}

fn load_map_at_index(
    catalog: &MapCatalog,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    std_materials: &mut ResMut<Assets<StandardMaterial>>,
    images: &mut ResMut<Assets<Image>>,
    camera_query: &mut Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    map_bounds: &mut ResMut<MapBounds>,
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

    if parsed.header.min_height == parsed.header.max_height {
        warn!(
            "  Flat terrain (min=max={}). Lua heightmap gadget not supported yet.",
            parsed.header.min_height
        );
    }

    spawn_terrain(parsed, terrain_material, commands, meshes, std_materials);

    if let Some(map_info) = &spring_map.map_info {
        apply_atmosphere(map_info, commands);
        spawn_start_position_markers(parsed, map_info, commands, meshes, std_materials);
        info!(
            "  {} start positions, gravity={}",
            map_info.start_positions.len(),
            map_info.gravity,
        );
    }

    // Update window title.
    commands.insert_resource(WindowTitle(map_name.into_owned()));
}

/// Resource to defer window title update (applied by a system).
#[derive(Resource)]
struct WindowTitle(String);

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
        *cam_transform = rendering::camera::compute_transform_from_state(&cam_state);
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
    let (all_pixels, mip_levels) = generate_mipmaps(&ground.pixels, ground.width, ground.height);

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
    image.data = Some(all_pixels);
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
    terrain_material: Handle<StandardMaterial>,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    std_materials: &mut ResMut<Assets<StandardMaterial>>,
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

    spawn_datavent_markers(map, commands, meshes, std_materials);
}

fn spawn_datavent_markers(
    map: &ParsedMap,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    let marker_material = create_datavent_material(materials);
    let marker_mesh = meshes.add(Cuboid::new(16.0, 2.0, 16.0));

    let mut datavent_count = 0u32;

    for feature in &map.features {
        if feature.feature_type.is_geovent() {
            let heightmap_w = map.header.heightmap_width();
            let square_size = SQUARE_SIZE as f32;
            let heightmap_x =
                (feature.x / square_size).clamp(0.0, (heightmap_w - 1) as f32) as usize;
            let heightmap_z = (feature.z / square_size)
                .clamp(0.0, (map.header.heightmap_height() - 1) as f32)
                as usize;
            let height = map.heights[heightmap_z * heightmap_w + heightmap_x];

            commands.spawn((
                MapEntity,
                Mesh3d(marker_mesh.clone()),
                MeshMaterial3d(marker_material.clone()),
                Transform::from_xyz(feature.x, height + 2.0, feature.z),
            ));

            datavent_count += 1;
        }
    }

    if datavent_count > 0 {
        info!("  {datavent_count} datavents");
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

fn spawn_start_position_markers(
    parsed: &ParsedMap,
    map_info: &MapInfo,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    let marker_mesh = meshes.add(Cylinder::new(12.0, 4.0));

    let team_colors = [
        Color::linear_rgb(0.0, 0.8, 0.0),
        Color::linear_rgb(0.8, 0.0, 0.0),
        Color::linear_rgb(0.0, 0.4, 1.0),
        Color::linear_rgb(1.0, 1.0, 0.0),
        Color::linear_rgb(1.0, 0.0, 1.0),
        Color::linear_rgb(0.0, 1.0, 1.0),
        Color::linear_rgb(1.0, 0.5, 0.0),
        Color::linear_rgb(0.5, 0.0, 1.0),
    ];

    let square_size = SQUARE_SIZE as f32;

    for start_pos in &map_info.start_positions {
        let color = team_colors[start_pos.team as usize % team_colors.len()];
        let marker_material = materials.add(StandardMaterial {
            base_color: color,
            emissive: LinearRgba::from(color) * 2.0,
            unlit: false,
            ..default()
        });

        let heightmap_w = parsed.header.heightmap_width();
        let heightmap_x = (start_pos.x / square_size).clamp(0.0, (heightmap_w - 1) as f32) as usize;
        let heightmap_z = (start_pos.z / square_size)
            .clamp(0.0, (parsed.header.heightmap_height() - 1) as f32)
            as usize;
        let height = parsed.heights[heightmap_z * heightmap_w + heightmap_x];

        commands.spawn((
            MapEntity,
            Mesh3d(marker_mesh.clone()),
            MeshMaterial3d(marker_material),
            Transform::from_xyz(start_pos.x, height + 3.0, start_pos.z),
        ));
    }

    info!("  {} start positions", map_info.start_positions.len());
}

fn generate_mipmaps(pixels: &[u8], width: usize, height: usize) -> (Vec<u8>, u32) {
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

    (all_data, levels)
}

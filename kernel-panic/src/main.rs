mod rendering;
mod terrain;

use std::path::PathBuf;

use bevy::prelude::*;

use rendering::RenderingPlugin;
use rendering::camera::{MapBounds, RtsCamera, RtsCameraState};
use spring_map::SpringMap;
use spring_map::map_types::{GroundTexture, ParsedMap, SQUARE_SIZE, SmfHeader};
use terrain::material::{create_datavent_material, create_terrain_material};
use terrain::mesh::generate_terrain_chunks;

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
            load_and_spawn_terrain.after(rendering::camera::spawn_camera),
        )
        .run();
}

fn load_and_spawn_terrain(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut std_materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut camera_query: Query<(&mut RtsCameraState, &mut Transform), With<RtsCamera>>,
    mut map_bounds: ResMut<MapBounds>,
) {
    let map_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let workspace_path = PathBuf::from("kernel-panic/assets/maps/Marble_Madness_Map.sd7");
            if workspace_path.exists() {
                workspace_path
            } else {
                PathBuf::from("assets/maps/Marble_Madness_Map.sd7")
            }
        });

    let spring_map = match spring_map::load_map(&map_path) {
        Ok(m) => m,
        Err(err) => {
            error!("Failed to load map from {}: {err}", map_path.display());
            info!("Usage: kernel-panic <path-to-map.sd7>");
            spawn_test_terrain(&mut commands, &mut meshes, &mut std_materials);
            return;
        }
    };

    let parsed = &spring_map.parsed;

    info!(
        "Loaded map: {}x{} (heightmap {}x{}), {} features",
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
            warn!("No ground texture available — using fallback material");
            dark_fallback_material(&mut std_materials)
        }
    };

    setup_camera(parsed, &mut camera_query, &mut map_bounds);

    if parsed.header.min_height == parsed.header.max_height {
        warn!(
            "Map has min_height == max_height ({}) — terrain will be flat. \
             This map likely uses a Lua gadget to deform the heightmap at runtime.",
            parsed.header.min_height
        );
    }

    spawn_terrain(
        parsed,
        terrain_material,
        &mut commands,
        &mut meshes,
        &mut std_materials,
    );
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
        info!("Camera → focus={focus}, distance={distance:.0}");
        cam_state.snap_to(focus, distance);
        *cam_transform = rendering::camera::compute_transform_from_state(&cam_state);
    } else {
        warn!("Camera entity not found — could not center on map");
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
    info!(
        "Ground texture: {}x{} ({:.1} MB)",
        ground.width,
        ground.height,
        ground.pixels.len() as f64 / 1_048_576.0,
    );

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
        "Texture: {}x{}, {mip_levels} mip levels, 16x aniso",
        ground.width, ground.height
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
    info!("Spawning {} terrain chunks", chunks.len());

    for chunk in chunks {
        let mesh_handle = meshes.add(chunk.mesh);
        commands.spawn((
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
        if feature.type_name.eq_ignore_ascii_case("GeoVent") {
            let heightmap_w = map.header.heightmap_width();
            let square_size = SQUARE_SIZE as f32;
            let heightmap_x =
                (feature.x / square_size).clamp(0.0, (heightmap_w - 1) as f32) as usize;
            let heightmap_z = (feature.z / square_size)
                .clamp(0.0, (map.header.heightmap_height() - 1) as f32)
                as usize;
            let height = map.heights[heightmap_z * heightmap_w + heightmap_x];

            commands.spawn((
                Mesh3d(marker_mesh.clone()),
                MeshMaterial3d(marker_material.clone()),
                Transform::from_xyz(feature.x, height + 2.0, feature.z),
            ));

            datavent_count += 1;
        }
    }

    if datavent_count > 0 {
        info!("Placed {datavent_count} datavents (GeoVent features)");
    } else {
        warn!("No GeoVent features found in map");
    }
}

fn spawn_test_terrain(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut Assets<StandardMaterial>,
) {
    info!("Generating test terrain");

    let terrain_material = dark_fallback_material(materials);
    let map = build_test_map();
    let chunks = generate_terrain_chunks(&map);

    for chunk in chunks {
        let mesh_handle = meshes.add(chunk.mesh);
        commands.spawn((
            Mesh3d(mesh_handle),
            MeshMaterial3d(terrain_material.clone()),
            Transform::from_translation(chunk.translation),
        ));
    }

    commands.spawn(DirectionalLight {
        illuminance: 500.0,
        shadows_enabled: false,
        ..default()
    });
}

fn build_test_map() -> ParsedMap {
    let header = SmfHeader::new_flat(256, 256, -20.0, 80.0);

    let heightmap_w = header.heightmap_width();
    let heightmap_h = header.heightmap_height();

    let mut heights = Vec::with_capacity(heightmap_w * heightmap_h);
    for grid_z in 0..heightmap_h {
        for grid_x in 0..heightmap_w {
            let frac_x = grid_x as f32 / heightmap_w as f32;
            let frac_z = grid_z as f32 / heightmap_h as f32;
            let height = 10.0 * (frac_x * std::f32::consts::TAU * 2.0).sin()
                + 8.0 * (frac_z * std::f32::consts::TAU * 3.0).sin()
                + 5.0 * ((frac_x + frac_z) * std::f32::consts::TAU * 1.5).cos()
                + 15.0;
            heights.push(height);
        }
    }

    ParsedMap {
        header,
        heights,
        features: vec![],
        metalmap: vec![0; 256 / 2 * 256 / 2],
    }
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

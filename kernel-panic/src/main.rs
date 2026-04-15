mod rendering;
mod terrain;

use std::path::PathBuf;

use bevy::prelude::*;

use rendering::RenderingPlugin;
use rendering::camera::{MapBounds, RtsCamera, RtsCameraState};
use spring_map::map_types::ParsedMap;
use spring_map::sd7_archive::{ExtractedMap, load_map_archive};
use spring_map::smf_parser::parse_smf;
use spring_map::smt_parser::{assemble_ground_texture, parse_smt_tiles, parse_tilemap};
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

/// Startup system: load a map from the first command-line argument
/// (or a default path) and spawn the terrain mesh chunks.
///
/// Accepts .sd7, .sdz, or raw .smf files.
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
            let ws = PathBuf::from("kernel-panic/assets/maps/Marble_Madness_Map.sd7");
            if ws.exists() {
                ws
            } else {
                PathBuf::from("assets/maps/Marble_Madness_Map.sd7")
            }
        });

    let extracted = match load_map_archive(&map_path) {
        Ok(e) => e,
        Err(err) => {
            error!("Failed to load map from {}: {err}", map_path.display());
            info!("Usage: kernel-panic <path-to-map.sd7>");
            info!("Spawning a flat test terrain instead.");
            spawn_test_terrain(&mut commands, &mut meshes, &mut std_materials);
            return;
        }
    };

    info!(
        "Extracted '{}' from {}",
        extracted.smf_name,
        map_path.display()
    );

    let parsed = match parse_smf(&extracted.smf_data) {
        Ok(m) => m,
        Err(err) => {
            error!("Failed to parse SMF: {err}");
            spawn_test_terrain(&mut commands, &mut meshes, &mut std_materials);
            return;
        }
    };

    info!(
        "Loaded map: {}x{} (heightmap {}x{}), {} features",
        parsed.header.map_x,
        parsed.header.map_y,
        parsed.header.heightmap_width(),
        parsed.header.heightmap_height(),
        parsed.features.len(),
    );

    let terrain_material =
        build_terrain_material(&extracted, &parsed, &mut images, &mut std_materials);

    // Set map bounds for camera clamping.
    let world_w = parsed.header.world_width();
    let world_d = parsed.header.world_depth();
    let hm_w = parsed.header.heightmap_width();
    let hm_h = parsed.header.heightmap_height();
    let center_height = parsed.heights[(hm_h / 2) * hm_w + hm_w / 2];

    *map_bounds =
        MapBounds::from_map_extents(Vec3::new(0.0, 0.0, 0.0), Vec3::new(world_w, 0.0, world_d));

    // Center the camera on the map.
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

    if parsed.header.min_height == parsed.header.max_height {
        warn!(
            "Map has min_height == max_height ({}) — terrain will be flat. \
             This map likely uses a Lua gadget to deform the heightmap at runtime.",
            parsed.header.min_height
        );
    }

    spawn_terrain_from_map(
        &parsed,
        terrain_material,
        &mut commands,
        &mut meshes,
        &mut std_materials,
    );
}

fn dark_fallback_material(materials: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::srgb(0.02, 0.02, 0.02),
        unlit: true,
        ..default()
    })
}

fn build_terrain_material(
    extracted: &ExtractedMap,
    parsed: &ParsedMap,
    images: &mut ResMut<Assets<Image>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) -> Handle<StandardMaterial> {
    let Some(smt_data) = &extracted.smt_data else {
        warn!("No SMT tile data found — using fallback material");
        return dark_fallback_material(materials);
    };

    let (tm_w, tm_h, tilemap) = match parse_tilemap(&extracted.smf_data, &parsed.header) {
        Ok(t) => t,
        Err(err) => {
            warn!("Failed to parse tilemap: {err}");
            return dark_fallback_material(materials);
        }
    };

    info!("Tilemap: {tm_w}x{tm_h} ({} entries)", tilemap.len());

    let tiles = match parse_smt_tiles(smt_data) {
        Ok(t) => t,
        Err(err) => {
            warn!("Failed to parse SMT tiles: {err}");
            return dark_fallback_material(materials);
        }
    };

    info!("Parsed {} tiles from SMT", tiles.len());

    // Assemble the full ground texture.
    let (tex_w, tex_h, pixels) = assemble_ground_texture(&tiles, &tilemap, tm_w, tm_h);

    // Debug: sample a few pixels to verify the texture has actual content.
    let center = ((tex_h / 2) * tex_w + tex_w / 2) * 4;
    let quarter = ((tex_h / 4) * tex_w + tex_w / 4) * 4;
    info!(
        "Assembled {tex_w}x{tex_h} ground texture ({:.1} MB). \
         Sample pixels — center: rgba({},{},{},{}), quarter: rgba({},{},{},{})",
        pixels.len() as f64 / 1_048_576.0,
        pixels[center],
        pixels[center + 1],
        pixels[center + 2],
        pixels[center + 3],
        pixels[quarter],
        pixels[quarter + 1],
        pixels[quarter + 2],
        pixels[quarter + 3],
    );

    // Create a Bevy Image with mipmaps and anisotropic filtering to reduce moiré
    // from the fine circuit-board grid lines at oblique angles.
    let (all_pixels, mip_levels) = generate_mipmaps(&pixels, tex_w, tex_h);

    let size = bevy::render::render_resource::Extent3d {
        width: tex_w as u32,
        height: tex_h as u32,
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

    info!("Texture: {tex_w}x{tex_h}, {mip_levels} mip levels, 16x aniso");

    let texture_handle = images.add(image);
    create_terrain_material(texture_handle, materials)
}

fn spawn_terrain_from_map(
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

/// Extract datavent positions from GeoVent features in the map.
fn spawn_datavent_markers(
    map: &ParsedMap,
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    let marker_material = create_datavent_material(materials);
    let marker_mesh = meshes.add(Cuboid::new(16.0, 2.0, 16.0));

    let geovent_type_indices: Vec<usize> = map
        .feature_type_names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.eq_ignore_ascii_case("GeoVent"))
        .map(|(i, _)| i)
        .collect();

    let mut datavent_count = 0u32;

    for feature in &map.features {
        if geovent_type_indices.contains(&(feature.feature_type as usize)) {
            let hm_w = map.header.heightmap_width();
            let sq = map.header.square_size as f32;
            let hx = (feature.x / sq).clamp(0.0, (hm_w - 1) as f32) as usize;
            let hz =
                (feature.z / sq).clamp(0.0, (map.header.heightmap_height() - 1) as f32) as usize;
            let height = map.heights[hz * hm_w + hx];

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
        warn!("No GeoVent features found in map — datavents will be missing");
    }
}

/// Spawn a small flat test terrain when no map file is available.
fn spawn_test_terrain(
    commands: &mut Commands,
    meshes: &mut ResMut<Assets<Mesh>>,
    materials: &mut ResMut<Assets<StandardMaterial>>,
) {
    info!("Generating test terrain (256x256 map-squares = 2048x2048 elmos)");

    let terrain_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.02, 0.05, 0.02),
        unlit: true,
        ..default()
    });

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

/// Build a synthetic map with rolling hills for testing without a real SMF file.
fn build_test_map() -> ParsedMap {
    use spring_map::map_types::SmfHeader;

    let map_x = 256;
    let map_y = 256;

    let header = SmfHeader {
        map_id: 0,
        map_x,
        map_y,
        square_size: 8,
        min_height: -20.0,
        max_height: 80.0,
        heightmap_ptr: 0,
        type_map_ptr: 0,
        tiles_ptr: 0,
        minimap_ptr: 0,
        metalmap_ptr: 0,
        feature_ptr: 0,
        num_extra_headers: 0,
    };

    let hm_w = header.heightmap_width();
    let hm_h = header.heightmap_height();

    let mut heights = Vec::with_capacity(hm_w * hm_h);
    for gz in 0..hm_h {
        for gx in 0..hm_w {
            let fx = gx as f32 / hm_w as f32;
            let fz = gz as f32 / hm_h as f32;
            let h = 10.0 * (fx * std::f32::consts::TAU * 2.0).sin()
                + 8.0 * (fz * std::f32::consts::TAU * 3.0).sin()
                + 5.0 * ((fx + fz) * std::f32::consts::TAU * 1.5).cos()
                + 15.0;
            heights.push(h);
        }
    }

    ParsedMap {
        header,
        heights,
        feature_type_names: vec![],
        features: vec![],
        metalmap: vec![0; (map_x / 2 * map_y / 2) as usize],
    }
}

/// Generate a full mipmap chain by box-filtering each level to half resolution.
/// Returns `(contiguous_pixel_data, mip_level_count)`.
fn generate_mipmaps(pixels: &[u8], width: usize, height: usize) -> (Vec<u8>, u32) {
    let mut all_data = Vec::with_capacity(pixels.len() * 4 / 3);
    all_data.extend_from_slice(pixels);
    let mut levels = 1u32;

    let mut w = width;
    let mut h = height;
    let mut src = pixels.to_vec();

    while w > 1 || h > 1 {
        let new_w = (w / 2).max(1);
        let new_h = (h / 2).max(1);
        let mut dst = vec![0u8; new_w * new_h * 4];

        for y in 0..new_h {
            for x in 0..new_w {
                let sx = (x * 2).min(w - 1);
                let sy = (y * 2).min(h - 1);
                let sx1 = (sx + 1).min(w - 1);
                let sy1 = (sy + 1).min(h - 1);

                let i00 = (sy * w + sx) * 4;
                let i10 = (sy * w + sx1) * 4;
                let i01 = (sy1 * w + sx) * 4;
                let i11 = (sy1 * w + sx1) * 4;

                for c in 0..4 {
                    let avg = (src[i00 + c] as u16
                        + src[i10 + c] as u16
                        + src[i01 + c] as u16
                        + src[i11 + c] as u16)
                        / 4;
                    dst[(y * new_w + x) * 4 + c] = avg as u8;
                }
            }
        }

        all_data.extend_from_slice(&dst);
        levels += 1;
        src = dst;
        w = new_w;
        h = new_h;
    }

    (all_data, levels)
}

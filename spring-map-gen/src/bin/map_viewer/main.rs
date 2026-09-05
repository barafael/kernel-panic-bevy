//! Standalone map viewer for Spring RTS engine map files.
//!
//! Loads any Spring map archive through spring-map, renders terrain with
//! ground textures, spawns Kernel Panic unit types at start positions
//! with S3O models, COB animations, and periodic weapon fire visuals.
//!
//! Controls:
//!   Arrow keys — pan camera
//!   Scroll wheel — zoom
//!   Middle mouse drag — orbit
//!   Q/E — rotate
//!   Space — trigger weapon fire demo

mod units;

use std::path::PathBuf;

use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::render::view::Hdr;
use clap::Parser;
use thiserror::Error;

use spring_map::map_types::{ParsedMap, SQUARE_SIZE};
use spring_tdf::{UnitDefs, WeaponDefs};

/// Renders a Spring RTS map archive (`.sd7`, `.sdz`) or a raw `.smf` file in
/// an interactive Bevy window with start-position markers and a weapon-fire
/// demo.
#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    /// Path to a map archive (`.sd7` / `.sdz`) or a raw `.smf` file.
    map: PathBuf,
}

#[derive(Debug, Error)]
enum MapViewerError {
    #[error("map file not found: {0}")]
    MapNotFound(PathBuf),
}

// ── Resources ──────────────────────────────────────────────────────────

#[derive(Resource)]
struct MapPath(PathBuf);

#[derive(Resource)]
struct MapInfoDisplay {
    name: String,
    map_x: i32,
    map_y: i32,
    num_features: usize,
}

#[derive(Resource)]
struct UnitDefs_(UnitDefs);

#[derive(Resource)]
struct WeaponDefs_(WeaponDefs);

/// Marker for all entities belonging to the current map.
#[derive(Component)]
struct MapEntity;

// ── Camera ─────────────────────────────────────────────────────────────

#[derive(Component)]
struct RtsCamera;

#[derive(Component)]
struct CamState {
    focus: Vec3,
    distance: f32,
    yaw: f32,
    pitch: f32,
}

impl Default for CamState {
    fn default() -> Self {
        Self {
            focus: Vec3::new(512.0, 0.0, 512.0),
            distance: 800.0,
            yaw: 0.0,
            pitch: std::f32::consts::FRAC_PI_4,
        }
    }
}

fn cam_transform(s: &CamState) -> Transform {
    let h = s.distance * s.pitch.cos();
    let v = s.distance * s.pitch.sin();
    let offset = Vec3::new(h * s.yaw.sin(), v, h * s.yaw.cos());
    Transform::from_translation(s.focus + offset).looking_at(s.focus, Vec3::Y)
}

// ── HUD ────────────────────────────────────────────────────────────────

#[derive(Component)]
struct HudText;

// ── Units ──────────────────────────────────────────────────────────────

/// Marker for demo units spawned at the start positions. Units render as
/// static piece trees — the viewer carries no animation (the game's
/// per-unit Rust drivers live in kernel-panic; the spring-cob VM this
/// viewer used to run has been retired).
#[derive(Component)]
struct ViewerUnit;

#[derive(Component)]
struct PieceIndex(#[allow(dead_code)] usize);

// ── Weapon FX ──────────────────────────────────────────────────────────

/// Timer that periodically triggers weapon fire demo.
#[derive(Resource)]
struct WeaponFireTimer(Timer);

#[derive(Component)]
struct BeamVisual {
    lifetime: f32,
    max_lifetime: f32,
}

#[derive(Component)]
struct ProjectileVisual {
    origin: Vec3,
    target: Vec3,
    speed: f32,
    progress: f32,
    arc_height: f32,
}

// ── Main ───────────────────────────────────────────────────────────────

fn main() -> Result<(), MapViewerError> {
    let args = Args::parse();
    if !args.map.exists() {
        return Err(MapViewerError::MapNotFound(args.map));
    }

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!(
                    "Map Viewer — {}",
                    args.map.file_name().unwrap_or_default().to_string_lossy()
                ),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(MapPath(args.map))
        .add_systems(Startup, (setup_resources, setup_camera, setup_hud).chain())
        .add_systems(Startup, load_map_system.after(setup_camera))
        .add_systems(
            Update,
            (
                camera_control,
                weapon_fire_demo,
                tick_weapon_fx,
                update_hud,
            ),
        )
        .run();
    Ok(())
}

// ── Setup ──────────────────────────────────────────────────────────────

fn setup_resources(mut commands: Commands) {
    let unit_defs = load_unit_defs();
    let weapon_defs = load_weapon_defs();
    commands.insert_resource(UnitDefs_(unit_defs));
    commands.insert_resource(WeaponDefs_(weapon_defs));
    commands.insert_resource(WeaponFireTimer(Timer::from_seconds(
        3.0,
        TimerMode::Repeating,
    )));
}

fn setup_camera(mut commands: Commands) {
    let state = CamState::default();
    let transform = cam_transform(&state);

    commands.spawn((
        RtsCamera,
        state,
        Camera3d::default(),
        transform,
        Hdr,
        bevy::post_process::bloom::Bloom {
            intensity: 0.15,
            ..default()
        },
        DistanceFog {
            color: Color::BLACK,
            falloff: FogFalloff::Linear {
                start: 3600.0,
                end: 4000.0,
            },
            ..default()
        },
    ));
}

fn setup_hud(mut commands: Commands) {
    commands.spawn((
        HudText,
        Text::new("Loading..."),
        TextFont {
            font_size: 18.0,
            ..default()
        },
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            left: Val::Px(10.0),
            ..default()
        },
    ));
}

// ── Map loading ────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn load_map_system(
    map_path: Res<MapPath>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    unit_defs: Res<UnitDefs_>,
    weapon_defs: Res<WeaponDefs_>,
    mut camera_q: Query<(&mut CamState, &mut Transform), With<RtsCamera>>,
) {
    let path = &map_path.0;
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let spring_map = match spring_map::load_map(path) {
        Ok(m) => m,
        Err(error) => {
            error!("Failed to load map '{}': {error}", path.display());
            commands.insert_resource(MapInfoDisplay {
                name: format!("<load failed: {error}>"),
                map_x: 0,
                map_y: 0,
                num_features: 0,
            });
            return;
        }
    };

    let parsed = &spring_map.parsed;
    let map_info = spring_map.map_info.as_ref();

    // Camera.
    let world_w = parsed.header.world_width();
    let world_d = parsed.header.world_depth();
    let hm_w = parsed.header.heightmap_width();
    let hm_h = parsed.header.heightmap_height();
    let center_h = parsed.heights[(hm_h / 2) * hm_w + hm_w / 2];

    if let Ok((mut cam, mut tf)) = camera_q.single_mut() {
        cam.focus = Vec3::new(world_w / 2.0, center_h, world_d / 2.0);
        cam.distance = world_w.max(world_d) * 0.5;
        *tf = cam_transform(&cam);
    }

    // Terrain material.
    let terrain_mat = match &spring_map.ground_texture {
        Some(gt) => {
            let size = bevy::render::render_resource::Extent3d {
                width: gt.width as u32,
                height: gt.height as u32,
                depth_or_array_layers: 1,
            };
            let usage = bevy::asset::RenderAssetUsages::RENDER_WORLD
                | bevy::asset::RenderAssetUsages::MAIN_WORLD;
            let image = Image::new(
                size,
                bevy::render::render_resource::TextureDimension::D2,
                gt.pixels.clone(),
                bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                usage,
            );
            let tex = images.add(image);
            materials.add(StandardMaterial {
                base_color_texture: Some(tex),
                unlit: true,
                ..default()
            })
        }
        None => materials.add(StandardMaterial {
            base_color: Color::srgb(0.02, 0.02, 0.02),
            unlit: true,
            ..default()
        }),
    };

    // Terrain chunks.
    let chunks = generate_terrain_chunks(parsed);
    for chunk in chunks {
        let handle = meshes.add(chunk.mesh);
        commands.spawn((
            MapEntity,
            Mesh3d(handle),
            MeshMaterial3d(terrain_mat.clone()),
            Transform::from_translation(chunk.translation),
        ));
    }

    // Datavent markers.
    let dv_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.4, 0.0),
        emissive: LinearRgba::new(2.0, 0.8, 0.0, 1.0),
        unlit: false,
        ..default()
    });
    let dv_mesh = meshes.add(Cuboid::new(16.0, 2.0, 16.0));
    for feature in &parsed.features {
        if feature.feature_type.is_geovent() {
            let sq = SQUARE_SIZE as f32;
            let hx = (feature.x / sq).clamp(0.0, (hm_w - 1) as f32) as usize;
            let hz = (feature.z / sq).clamp(0.0, (hm_h - 1) as f32) as usize;
            let h = parsed.heights[hz * hm_w + hx];
            commands.spawn((
                MapEntity,
                Mesh3d(dv_mesh.clone()),
                MeshMaterial3d(dv_mat.clone()),
                Transform::from_xyz(feature.x, h + 2.0, feature.z),
            ));
        }
    }

    // Tree markers.
    let tree_mat = materials.add(StandardMaterial {
        base_color: Color::srgb(0.0, 0.5, 0.0),
        emissive: LinearRgba::new(0.0, 0.5, 0.0, 1.0),
        unlit: false,
        ..default()
    });
    let tree_mesh = meshes.add(Cylinder::new(3.0, 12.0));
    for feature in &parsed.features {
        if feature.feature_type.is_tree() {
            let sq = SQUARE_SIZE as f32;
            let hx = (feature.x / sq).clamp(0.0, (hm_w - 1) as f32) as usize;
            let hz = (feature.z / sq).clamp(0.0, (hm_h - 1) as f32) as usize;
            let h = parsed.heights[hz * hm_w + hx];
            commands.spawn((
                MapEntity,
                Mesh3d(tree_mesh.clone()),
                MeshMaterial3d(tree_mat.clone()),
                Transform::from_xyz(feature.x, h + 6.0, feature.z),
            ));
        }
    }

    // Atmosphere.
    if let Some(info) = map_info {
        let sky = info.atmosphere.sky_color;
        commands.insert_resource(ClearColor(Color::linear_rgb(sky[0], sky[1], sky[2])));

        let sun = info.lighting.ground_sun_color;
        let dir = info.lighting.sun_dir;
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

        let ambient = info.lighting.ground_ambient;
        commands.insert_resource(bevy::light::GlobalAmbientLight {
            color: Color::linear_rgb(ambient[0], ambient[1], ambient[2]),
            brightness: 200.0,
            ..default()
        });
    }

    // Spawn units at start positions.
    if let Some(info) = map_info {
        units::spawn_all_units(
            parsed,
            info,
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &unit_defs.0,
            &weapon_defs.0,
        );
    }

    info!(
        "Loaded map '{}': {}x{}, {} features",
        name,
        parsed.header.map_x,
        parsed.header.map_y,
        parsed.features.len(),
    );

    commands.insert_resource(MapInfoDisplay {
        name,
        map_x: parsed.header.map_x,
        map_y: parsed.header.map_y,
        num_features: parsed.features.len(),
    });
}

// ── Camera control ─────────────────────────────────────────────────────

fn camera_control(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut scroll: MessageReader<MouseWheel>,
    mut motion: MessageReader<bevy::input::mouse::MouseMotion>,
    mut q: Query<(&mut CamState, &mut Transform), With<RtsCamera>>,
) {
    let Ok((mut s, mut tf)) = q.single_mut() else {
        return;
    };
    let dt = time.delta_secs();

    // Pan.
    let fwd = Vec3::new(-s.yaw.sin(), 0.0, -s.yaw.cos());
    let right = Vec3::new(fwd.z, 0.0, -fwd.x);
    let mut pan = Vec3::ZERO;
    if keys.pressed(KeyCode::ArrowUp) {
        pan += fwd;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        pan -= fwd;
    }
    if keys.pressed(KeyCode::ArrowLeft) {
        pan += right;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        pan -= right;
    }
    if pan != Vec3::ZERO {
        let speed = 800.0 * (s.distance / 500.0).max(0.3);
        s.focus += pan.normalize() * speed * dt;
    }

    // Zoom.
    let mut zt: f32 = 0.0;
    for e in scroll.read() {
        zt += e.y;
    }
    if zt != 0.0 {
        let factor = 1.0 - zt.clamp(-5.0, 5.0) * 0.06;
        s.distance = (s.distance * factor).clamp(100.0, 4000.0);
    }

    // Orbit.
    if mouse.pressed(MouseButton::Middle) {
        for m in motion.read() {
            s.yaw -= m.delta.x * 0.003;
            s.pitch = (s.pitch + m.delta.y * 0.003).clamp(0.15, std::f32::consts::FRAC_PI_2 - 0.05);
        }
    } else {
        motion.clear();
    }
    if keys.pressed(KeyCode::KeyQ) {
        s.yaw += 0.8 * dt;
    }
    if keys.pressed(KeyCode::KeyE) {
        s.yaw -= 0.8 * dt;
    }

    *tf = cam_transform(&s);
}


// ── Weapon fire demo ───────────────────────────────────────────────────

fn weapon_fire_demo(
    time: Res<Time>,
    mut timer: ResMut<WeaponFireTimer>,
    keys: Res<ButtonInput<KeyCode>>,
    units: Query<(&GlobalTransform, Entity), With<ViewerUnit>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let fire = keys.just_pressed(KeyCode::Space) || timer.0.tick(time.delta()).just_finished();
    if !fire {
        return;
    }

    // Collect unit positions first.
    let positions: Vec<Vec3> = units.iter().map(|(gtf, _)| gtf.translation()).collect();
    if positions.len() < 2 {
        return;
    }

    // Each unit fires at the next one in the list.
    for (idx, (gtf, _)) in units.iter().enumerate() {
        let target = positions[(idx + 1) % positions.len()];
        let my_pos = gtf.translation();

        // Spawn beam visual.
        let dir = target - my_pos;
        let length = dir.length();
        if length > 0.1 {
            let colors = [
                LinearRgba::new(0.0, 1.0, 0.3, 1.0), // green
                LinearRgba::new(1.0, 0.0, 0.2, 1.0), // red
                LinearRgba::new(0.2, 0.5, 1.0, 1.0), // blue
                LinearRgba::new(1.0, 1.0, 0.0, 1.0), // yellow
            ];
            let color = colors[idx % colors.len()];
            let midpoint = (my_pos + target) / 2.0;
            let rotation = Quat::from_rotation_arc(Vec3::Z, dir.normalize());

            let mat = materials.add(StandardMaterial {
                base_color: Color::LinearRgba(color),
                emissive: color * 8.0,
                unlit: true,
                alpha_mode: AlphaMode::Add,
                ..default()
            });
            let mesh = meshes.add(Cuboid::new(1.5, 1.5, length));
            commands.spawn((
                MapEntity,
                BeamVisual {
                    lifetime: 0.2,
                    max_lifetime: 0.2,
                },
                Mesh3d(mesh),
                MeshMaterial3d(mat),
                Transform::from_translation(midpoint).with_rotation(rotation),
            ));

            // Also spawn a projectile for variety.
            if idx % 3 == 0 {
                let proj_mat = materials.add(StandardMaterial {
                    base_color: Color::LinearRgba(color),
                    emissive: color * 6.0,
                    unlit: true,
                    ..default()
                });
                let proj_mesh = meshes.add(Sphere::new(3.0));
                commands.spawn((
                    MapEntity,
                    ProjectileVisual {
                        origin: my_pos,
                        target,
                        speed: 300.0,
                        progress: 0.0,
                        arc_height: 0.3,
                    },
                    Mesh3d(proj_mesh),
                    MeshMaterial3d(proj_mat),
                    Transform::from_translation(my_pos),
                ));
            }
        }
    }
}

// ── Weapon FX tick ─────────────────────────────────────────────────────

#[allow(clippy::type_complexity)]
fn tick_weapon_fx(
    time: Res<Time>,
    mut beams: Query<(Entity, &mut BeamVisual, &mut Transform), Without<ProjectileVisual>>,
    mut projectiles: Query<(Entity, &mut ProjectileVisual, &mut Transform)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();

    for (entity, mut beam, mut transform) in &mut beams {
        beam.lifetime -= dt;
        if beam.lifetime <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let fade = (beam.lifetime / beam.max_lifetime).sqrt();
        let s = transform.scale;
        transform.scale = Vec3::new(s.x.min(1.0) * fade, s.y.min(1.0) * fade, s.z);
    }

    for (entity, mut proj, mut transform) in &mut projectiles {
        let total = proj.origin.distance(proj.target);
        if total < 0.1 {
            commands.entity(entity).despawn();
            continue;
        }
        proj.progress += (proj.speed * dt) / total;
        if proj.progress >= 1.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let t = proj.progress;
        let mut pos = proj.origin.lerp(proj.target, t);
        if proj.arc_height > 0.0 {
            pos.y += proj.arc_height * total * 4.0 * t * (1.0 - t);
        }
        transform.translation = pos;
    }
}

// ── HUD update ─────────────────────────────────────────────────────────

fn update_hud(
    info: Option<Res<MapInfoDisplay>>,
    mut text_q: Query<&mut Text, With<HudText>>,
    units: Query<&ViewerUnit>,
) {
    let Ok(mut text) = text_q.single_mut() else {
        return;
    };
    let Some(info) = info else { return };
    let unit_count = units.iter().count();
    **text = format!(
        "{}\n{}x{} | {} features | {} units\n\nArrows pan | Scroll zoom | Middle-drag orbit | Q/E rotate | Space fire weapons",
        info.name, info.map_x, info.map_y, info.num_features, unit_count,
    );
}

// ── Terrain chunk generation ───────────────────────────────────────────

struct TerrainChunk {
    mesh: Mesh,
    translation: Vec3,
}

fn generate_terrain_chunks(map: &ParsedMap) -> Vec<TerrainChunk> {
    const CHUNK: usize = 32;
    let hm_w = map.header.heightmap_width();
    let hm_h = map.header.heightmap_height();
    let sq = SQUARE_SIZE as f32;

    let chunks_x = (hm_w - 1).div_ceil(CHUNK);
    let chunks_z = (hm_h - 1).div_ceil(CHUNK);
    let mut chunks = Vec::with_capacity(chunks_x * chunks_z);

    for cz in 0..chunks_z {
        for cx in 0..chunks_x {
            let vx_start = cx * CHUNK;
            let vz_start = cz * CHUNK;
            let vx_end = (vx_start + CHUNK + 1).min(hm_w);
            let vz_end = (vz_start + CHUNK + 1).min(hm_h);
            let lw = vx_end - vx_start;
            let lh = vz_end - vz_start;

            let mut positions = Vec::with_capacity(lw * lh);
            let mut normals = vec![[0.0f32; 3]; lw * lh];
            let mut uvs = Vec::with_capacity(lw * lh);

            for lz in 0..lh {
                for lx in 0..lw {
                    let gx = vx_start + lx;
                    let gz = vz_start + lz;
                    let h = map.heights[gz * hm_w + gx];
                    positions.push([lx as f32 * sq, h, lz as f32 * sq]);
                    uvs.push([gx as f32 / (hm_w - 1) as f32, gz as f32 / (hm_h - 1) as f32]);
                }
            }

            let qw = lw.saturating_sub(1);
            let qh = lh.saturating_sub(1);
            let mut indices = Vec::with_capacity(qw * qh * 6);
            for qz in 0..qh {
                for qx in 0..qw {
                    let tl = (qz * lw + qx) as u32;
                    let tr = tl + 1;
                    let bl = ((qz + 1) * lw + qx) as u32;
                    let br = bl + 1;
                    indices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);

                    let p = |i: u32| Vec3::from(positions[i as usize]);
                    let n1 = (p(bl) - p(tl)).cross(p(tr) - p(tl));
                    let n2 = (p(bl) - p(tr)).cross(p(br) - p(tr));
                    for &i in &[tl, bl, tr] {
                        let n = &mut normals[i as usize];
                        n[0] += n1.x;
                        n[1] += n1.y;
                        n[2] += n1.z;
                    }
                    for &i in &[tr, bl, br] {
                        let n = &mut normals[i as usize];
                        n[0] += n2.x;
                        n[1] += n2.y;
                        n[2] += n2.z;
                    }
                }
            }
            for n in &mut normals {
                *n = Vec3::from(*n).normalize_or(Vec3::Y).to_array();
            }

            let mut mesh = Mesh::new(
                bevy::mesh::PrimitiveTopology::TriangleList,
                bevy::asset::RenderAssetUsages::RENDER_WORLD
                    | bevy::asset::RenderAssetUsages::MAIN_WORLD,
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            mesh.insert_indices(bevy::mesh::Indices::U32(indices));

            chunks.push(TerrainChunk {
                mesh,
                translation: Vec3::new(vx_start as f32 * sq, 0.0, vz_start as f32 * sq),
            });
        }
    }
    chunks
}

// ── TDF loading helpers ────────────────────────────────────────────────

fn find_upstream_dir(leaf: &str) -> Option<PathBuf> {
    [
        "upstream/Kernel-Panic",
        "kernel-panic/upstream/Kernel-Panic",
    ]
    .iter()
    .map(|base| PathBuf::from(format!("{base}/{leaf}")))
    .find(|p| p.is_dir())
}

fn load_unit_defs() -> UnitDefs {
    let Some(dir) = find_upstream_dir("units") else {
        return UnitDefs::default();
    };
    let mut merged = UnitDefs::default();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "fbi")
                && let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(tdf) = spring_tdf::Tdf::parse(&text)
            {
                let defs = UnitDefs::from_tdf(&tdf);
                merged.units.extend(defs.units);
            }
        }
    }
    merged
}

fn load_weapon_defs() -> WeaponDefs {
    let Some(dir) = find_upstream_dir("weapons") else {
        return WeaponDefs::default();
    };
    let mut merged = WeaponDefs::default();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "tdf")
                && let Ok(text) = std::fs::read_to_string(&path)
                && let Ok(tdf) = spring_tdf::Tdf::parse(&text)
            {
                let defs = WeaponDefs::from_tdf(&tdf);
                merged.weapons.extend(defs.weapons);
            }
        }
    }
    merged
}

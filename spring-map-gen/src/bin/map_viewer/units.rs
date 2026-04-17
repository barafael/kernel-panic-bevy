//! Unit spawning for the map viewer — places all 19 Kernel Panic unit types.

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use spring_cob::{CobFile, CobVm, parse_cob};
use spring_map::map_types::{ParsedMap, SQUARE_SIZE};
use spring_map::smd_parser::MapInfo;
use spring_tdf::{UnitDefs, WeaponDefs};
use spring_unit_mesh::{S3OModel, S3OPiece, TgaImage, parse_tga};

use super::{CobAnimator, MapEntity, PieceIndex};

// ── Unit kinds ─────────────────────────────────────────────────────────

const UNIT_NAMES: &[(&str, Faction, f32)] = &[
    // (unitname, faction, mesh_scale)
    // System
    ("kernel", Faction::System, 3.0),
    ("assembler", Faction::System, 1.2),
    ("bit", Faction::System, 0.5),
    ("byte", Faction::System, 2.0),
    ("pointer", Faction::System, 1.5),
    ("socket", Faction::System, 2.0),
    ("firewall", Faction::Network, 1.5),
    // Hacker
    ("hole", Faction::Hacker, 3.0),
    ("bug", Faction::Hacker, 0.5),
    ("exploit", Faction::Hacker, 1.5),
    ("worm", Faction::Hacker, 1.5),
    ("virus", Faction::Hacker, 0.6),
    ("dos", Faction::Hacker, 1.3),
    ("window", Faction::Hacker, 2.0),
    ("logic_bomb", Faction::Hacker, 0.8),
    // Network
    ("connection", Faction::Network, 3.0),
    ("port", Faction::Network, 2.0),
    ("packet", Faction::Network, 0.6),
    ("signal", Faction::Network, 0.4),
];

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Faction {
    System,
    Hacker,
    Network,
}

impl Faction {
    fn color(self) -> LinearRgba {
        match self {
            Faction::System => LinearRgba::new(0.0, 1.0, 0.3, 1.0),
            Faction::Hacker => LinearRgba::new(1.0, 0.0, 0.2, 1.0),
            Faction::Network => LinearRgba::new(0.2, 0.5, 1.0, 1.0),
        }
    }
}

// ── Asset loading ──────────────────────────────────────────────────────

const ASSET_DIRS: &[&str] = &[
    "upstream/Kernel-Panic/objects3d",
    "upstream/Kernel-Panic/unittextures",
    "upstream/Kernel-Panic/scripts",
    "kernel-panic/upstream/Kernel-Panic/objects3d",
    "kernel-panic/upstream/Kernel-Panic/unittextures",
    "kernel-panic/upstream/Kernel-Panic/scripts",
];

fn load_asset<T, E: fmt::Display>(
    filename: &str,
    parse: impl Fn(&[u8]) -> Result<T, E>,
) -> Option<T> {
    for dir in ASSET_DIRS {
        let path = PathBuf::from(format!("{dir}/{filename}"));
        if let Ok(data) = std::fs::read(&path)
            && let Ok(result) = parse(&data)
        {
            return Some(result);
        }
    }
    None
}

// ── Main spawn function ────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn spawn_all_units(
    parsed: &ParsedMap,
    map_info: &MapInfo,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    unit_defs: &UnitDefs,
    _weapon_defs: &WeaponDefs,
) {
    let hm_w = parsed.header.heightmap_width();
    let sq = SQUARE_SIZE as f32;

    // We'll distribute all 19 units across the start positions, placing
    // a cluster of units around each start position.
    let starts = &map_info.start_positions;
    if starts.is_empty() {
        return;
    }

    let mut model_cache: HashMap<String, Option<S3OModel>> = HashMap::new();
    let mut tga_cache: HashMap<String, Option<TgaImage>> = HashMap::new();
    let mut cob_cache: HashMap<String, Option<Arc<CobFile>>> = HashMap::new();

    for (unit_idx, &(unitname, faction, mesh_scale)) in UNIT_NAMES.iter().enumerate() {
        // Pick a start position for this unit (round-robin).
        let sp = &starts[unit_idx % starts.len()];

        // Position with offset so units don't stack. When multiple units share
        // a start position they sit on a ring around it; the ring is wide
        // enough (~800 elmos) that unit meshes don't visually overlap even
        // at the largest mesh_scale values.
        let ring_idx = unit_idx / starts.len();
        let angle = unit_idx as f32 * 0.73; // golden-angle-ish spread
        let radius = 400.0 + ring_idx as f32 * 250.0;
        let offset_x = angle.cos() * radius;
        let offset_z = angle.sin() * radius;

        let wx = sp.x + offset_x;
        let wz = sp.z + offset_z;

        // Sample heightmap.
        let hx = (wx / sq).clamp(0.0, (hm_w - 1) as f32) as usize;
        let hz = (wz / sq).clamp(0.0, (parsed.header.heightmap_height() - 1) as f32) as usize;
        let height = parsed.heights[hz * hm_w + hx];
        let position = Vec3::new(wx, height, wz);

        // Resolve model name from FBI.
        let model_name = unit_defs
            .get(unitname)
            .map(|d| d.object_name.as_str())
            .unwrap_or("");

        // Load or get cached S3O.
        if !model_cache.contains_key(model_name) {
            let model = if model_name.is_empty() {
                None
            } else {
                load_asset(model_name, spring_unit_mesh::parse_s3o)
            };
            model_cache.insert(model_name.to_string(), model);
        }
        let s3o = model_cache.get(model_name).and_then(|m| m.as_ref());

        // Build material (faction-colored).
        let material = if let Some(model) = s3o {
            let tex_name = &model.texture1;
            if !tga_cache.contains_key(tex_name) {
                let tga = load_asset(tex_name, parse_tga);
                tga_cache.insert(tex_name.clone(), tga);
            }
            if let Some(tga) = tga_cache.get(tex_name).and_then(|t| t.as_ref()) {
                let pixels = colorize(tga, faction);
                let size = bevy::render::render_resource::Extent3d {
                    width: tga.width,
                    height: tga.height,
                    depth_or_array_layers: 1,
                };
                let usage = bevy::asset::RenderAssetUsages::RENDER_WORLD
                    | bevy::asset::RenderAssetUsages::MAIN_WORLD;
                let image = Image::new(
                    size,
                    bevy::render::render_resource::TextureDimension::D2,
                    pixels,
                    bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                    usage,
                );
                let tex = images.add(image);
                materials.add(StandardMaterial {
                    base_color_texture: Some(tex),
                    unlit: true,
                    ..default()
                })
            } else {
                emissive_mat(faction, materials)
            }
        } else {
            emissive_mat(faction, materials)
        };

        // Spawn root entity.
        let root = commands
            .spawn((
                MapEntity,
                Transform::from_translation(position),
                Visibility::default(),
            ))
            .id();

        // Spawn per-piece children or fallback mesh.
        if let Some(model) = s3o {
            let mut piece_entities = Vec::new();
            let mut piece_parents: Vec<Option<usize>> = Vec::new();
            let mut piece_offsets: Vec<[f32; 3]> = Vec::new();
            flatten_pieces(
                &model.root_piece,
                None,
                &mut piece_parents,
                &mut piece_offsets,
            );

            for (idx, parent_idx) in piece_parents.iter().enumerate() {
                let piece = get_piece(&model.root_piece, idx);
                let has_geo = piece.is_some_and(|p| !p.vertices.is_empty());
                let offset = piece_offsets[idx];

                let pe = if has_geo {
                    let mesh = piece_mesh(piece.unwrap());
                    let mh = meshes.add(mesh);
                    commands.spawn((
                        PieceIndex(idx),
                        Mesh3d(mh),
                        MeshMaterial3d(material.clone()),
                        Transform::from_xyz(offset[0], offset[1], offset[2]),
                        Visibility::default(),
                    ))
                } else {
                    commands.spawn((
                        PieceIndex(idx),
                        Transform::from_xyz(offset[0], offset[1], offset[2]),
                        Visibility::default(),
                    ))
                };
                let pe_id = pe.id();
                piece_entities.push(pe_id);

                let parent_entity = match parent_idx {
                    Some(pi) => piece_entities[*pi],
                    None => root,
                };
                commands.entity(parent_entity).add_child(pe_id);
            }

            // Attach COB animator.
            let script = format!("{unitname}.cob");
            if !cob_cache.contains_key(&script) {
                let cob = load_asset(&script, parse_cob).map(Arc::new);
                cob_cache.insert(script.clone(), cob);
            }
            if let Some(cob) = cob_cache.get(&script).and_then(|c| c.clone()) {
                let n = piece_entities.len();
                let mut vm = CobVm::new(&cob);
                vm.start_script(&cob, "Create", &[]);

                commands.entity(root).insert(CobAnimator {
                    vm,
                    cob,
                    piece_entities: piece_entities.clone(),
                    piece_base_offsets: piece_offsets,
                    piece_rotations: vec![[0.0; 3]; n],
                    piece_translations: vec![[0.0; 3]; n],
                    target_rotations: vec![[0.0; 3]; n],
                    turn_speeds: vec![[0.0; 3]; n],
                    target_translations: vec![[0.0; 3]; n],
                    move_speeds: vec![[0.0; 3]; n],
                    spin_speeds: vec![[0.0; 3]; n],
                });
            }
        } else {
            // Fallback cylinder.
            let mesh = meshes.add(Cylinder::new(5.0 * mesh_scale, 8.0 * mesh_scale));
            let child = commands
                .spawn((
                    Mesh3d(mesh),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                    Visibility::default(),
                ))
                .id();
            commands.entity(root).add_child(child);
        }
    }

    info!("Spawned {} unit types", UNIT_NAMES.len());
}

fn emissive_mat(
    faction: Faction,
    materials: &mut Assets<StandardMaterial>,
) -> Handle<StandardMaterial> {
    let c = faction.color();
    materials.add(StandardMaterial {
        base_color: Color::LinearRgba(c),
        emissive: c * 4.0,
        unlit: true,
        ..default()
    })
}

fn colorize(tga: &TgaImage, faction: Faction) -> Vec<u8> {
    let fc = faction.color();
    let fr = (fc.red.clamp(0.0, 1.0) * 255.0) as u8;
    let fg = (fc.green.clamp(0.0, 1.0) * 255.0) as u8;
    let fb = (fc.blue.clamp(0.0, 1.0) * 255.0) as u8;

    let mut out = Vec::with_capacity(tga.pixels.len());
    for chunk in tga.pixels.chunks_exact(4) {
        let (sr, sg, sb) = (chunk[0], chunk[1], chunk[2]);
        let alpha = chunk[3] as u16;
        let brightness = sr.max(sg).max(sb);
        let (br, bg, bb) = if brightness == 0 {
            (fr, fg, fb)
        } else {
            let t = brightness as u16;
            (
                ((fr as u16 * (255 - t) + sr as u16 * t) / 255) as u8,
                ((fg as u16 * (255 - t) + sg as u16 * t) / 255) as u8,
                ((fb as u16 * (255 - t) + sb as u16 * t) / 255) as u8,
            )
        };
        let scale = 25 + (alpha * 230 / 255);
        out.extend_from_slice(&[
            (br as u16 * scale / 255) as u8,
            (bg as u16 * scale / 255) as u8,
            (bb as u16 * scale / 255) as u8,
            255,
        ]);
    }
    out
}

fn flatten_pieces(
    piece: &S3OPiece,
    parent: Option<usize>,
    parents: &mut Vec<Option<usize>>,
    offsets: &mut Vec<[f32; 3]>,
) {
    let my = parents.len();
    parents.push(parent);
    offsets.push(piece.offset);
    for child in &piece.children {
        flatten_pieces(child, Some(my), parents, offsets);
    }
}

fn get_piece(root: &S3OPiece, target: usize) -> Option<&S3OPiece> {
    let mut counter = 0;
    get_piece_r(root, target, &mut counter)
}

fn get_piece_r<'a>(
    piece: &'a S3OPiece,
    target: usize,
    counter: &mut usize,
) -> Option<&'a S3OPiece> {
    if *counter == target {
        return Some(piece);
    }
    *counter += 1;
    for child in &piece.children {
        if let Some(found) = get_piece_r(child, target, counter) {
            return Some(found);
        }
    }
    None
}

fn piece_mesh(piece: &S3OPiece) -> Mesh {
    let positions: Vec<[f32; 3]> = piece.vertices.iter().map(|v| v.position).collect();
    let normals: Vec<[f32; 3]> = piece.vertices.iter().map(|v| v.normal).collect();
    let uvs: Vec<[f32; 2]> = piece.vertices.iter().map(|v| v.texcoord).collect();

    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_indices(Indices::U32(piece.indices.clone()));
    mesh
}

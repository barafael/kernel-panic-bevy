use std::sync::Arc;

use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use spring_cob::CobVm;
use spring_unit_mesh::S3OPiece;

use spring_map::map_types::{ParsedMap, SQUARE_SIZE};
use spring_map::smd_parser::MapInfo;

use super::animation::{CobAnimator, CobFileCache, PieceIndex, load_cob_cached};
use super::components::{Faction, Health, Homebase, SelectionVolume, TeamId, UnitType};
use super::definitions::{UnitKind, stats};
use super::meshes::{S3OModelCache, unit_material, unit_radius};
use super::production::default_production;
use crate::MapEntity;

const FACTION_ORDER: [Faction; 3] = [Faction::System, Faction::Hacker, Faction::Network];

#[derive(Resource, Clone)]
pub struct SelectionVolumeMaterial(pub Handle<StandardMaterial>);

#[allow(clippy::too_many_arguments)]
pub fn spawn_homebases(
    parsed: &ParsedMap,
    map_info: &MapInfo,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cob_cache: &mut CobFileCache,
) {
    let invisible_mat = get_or_create_invisible_material(commands, materials);
    let square_size = SQUARE_SIZE as f32;

    for start_pos in &map_info.start_positions {
        let faction = FACTION_ORDER[start_pos.team as usize % FACTION_ORDER.len()];
        let kind = faction.homebase();

        let heightmap_w = parsed.header.heightmap_width();
        let heightmap_x = (start_pos.x / square_size).clamp(0.0, (heightmap_w - 1) as f32) as usize;
        let heightmap_z = (start_pos.z / square_size)
            .clamp(0.0, (parsed.header.heightmap_height() - 1) as f32)
            as usize;
        let height = parsed.heights[heightmap_z * heightmap_w + heightmap_x];
        let position = Vec3::new(start_pos.x, height, start_pos.z);

        spawn_unit(
            kind,
            faction,
            start_pos.team as u8,
            position,
            commands,
            meshes,
            materials,
            images,
            model_cache,
            cob_cache,
            &invisible_mat,
        );
    }

    info!("Spawned {} homebases", map_info.start_positions.len());
}

/// Spawn a single unit with per-piece children and COB animation.
#[allow(clippy::too_many_arguments)]
pub fn spawn_unit(
    kind: UnitKind,
    faction: Faction,
    team: u8,
    position: Vec3,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cob_cache: &mut CobFileCache,
    invisible_mat: &SelectionVolumeMaterial,
) {
    let unit_stats = stats(kind);
    let material = unit_material(kind, faction, materials, images, model_cache);
    let radius = unit_radius(kind, model_cache);
    let selection_sphere = meshes.add(Sphere::new(radius).mesh().ico(3).unwrap());

    // Spawn the root unit entity.
    let unit_entity = commands
        .spawn((
            MapEntity,
            UnitType(kind),
            faction,
            TeamId(team),
            Health::full(unit_stats.max_health),
            Transform::from_translation(position),
            Visibility::default(),
        ))
        .id();

    if let Some(producer) = default_production(kind) {
        commands.entity(unit_entity).insert(producer);
    }
    if matches!(
        kind,
        UnitKind::Kernel | UnitKind::Hole | UnitKind::Connection
    ) {
        commands.entity(unit_entity).insert(Homebase);
    }

    // Selection volume child.
    let sel_child = commands
        .spawn((
            SelectionVolume,
            Mesh3d(selection_sphere),
            MeshMaterial3d(invisible_mat.0.clone()),
            Transform::from_xyz(0.0, radius * 0.5, 0.0),
        ))
        .id();
    commands.entity(unit_entity).add_child(sel_child);

    // Try to load the s3o model and build per-piece children.
    let s3o_model = super::meshes::load_s3o_model(unit_stats.model, model_cache);

    if let Some(model) = &s3o_model {
        // Flatten the piece tree into a list, spawning each as a child entity.
        let mut piece_entities = Vec::new();
        let mut piece_parents: Vec<Option<usize>> = Vec::new();
        flatten_pieces(&model.root_piece, None, &mut piece_parents);

        // Spawn piece entities.
        for (idx, parent_idx) in piece_parents.iter().enumerate() {
            let piece = get_piece_by_index(&model.root_piece, idx);
            let has_geometry = piece.map_or(false, |p| !p.vertices.is_empty());

            let mut piece_cmd = if has_geometry {
                let piece = piece.unwrap();
                let mesh = piece_to_mesh(piece);
                let mesh_handle = meshes.add(mesh);
                commands.spawn((
                    PieceIndex(idx),
                    Mesh3d(mesh_handle),
                    MeshMaterial3d(material.clone()),
                    Transform::from_xyz(piece.offset[0], piece.offset[1], piece.offset[2]),
                    Visibility::default(),
                ))
            } else {
                let offset = piece.map_or(Vec3::ZERO, |p| {
                    Vec3::new(p.offset[0], p.offset[1], p.offset[2])
                });
                commands.spawn((
                    PieceIndex(idx),
                    Transform::from_translation(offset),
                    Visibility::default(),
                ))
            };
            let piece_entity = piece_cmd.id();
            piece_entities.push(piece_entity);

            // Parent to the unit or to the parent piece.
            let bevy_parent = match parent_idx {
                Some(pi) => piece_entities[*pi],
                None => unit_entity,
            };
            commands.entity(bevy_parent).add_child(piece_entity);
        }

        // Attach CobAnimator.
        if let Some(cob) = load_cob_cached(unit_stats.script, cob_cache) {
            let num_pieces = piece_entities.len();
            let mut vm = CobVm::new(&cob);
            vm.start_script(&cob, "Create", &[]);

            commands.entity(unit_entity).insert(CobAnimator {
                vm,
                cob,
                piece_entities: piece_entities.clone(),
                piece_rotations: vec![[0.0; 3]; num_pieces],
                piece_translations: vec![[0.0; 3]; num_pieces],
                target_rotations: vec![[0.0; 3]; num_pieces],
                turn_speeds: vec![[0.0; 3]; num_pieces],
                target_translations: vec![[0.0; 3]; num_pieces],
                move_speeds: vec![[0.0; 3]; num_pieces],
                spin_speeds: vec![[0.0; 3]; num_pieces],
            });
        }
    } else {
        // Fallback: single flattened mesh, no animation.
        let mesh = super::meshes::unit_mesh(kind, meshes, model_cache);
        commands
            .entity(unit_entity)
            .insert((Mesh3d(mesh), MeshMaterial3d(material)));
    }
}

/// Flatten the piece tree depth-first, recording each piece's parent index.
fn flatten_pieces(piece: &S3OPiece, parent_idx: Option<usize>, result: &mut Vec<Option<usize>>) {
    let my_idx = result.len();
    result.push(parent_idx);
    for child in &piece.children {
        flatten_pieces(child, Some(my_idx), result);
    }
}

/// Get a piece by its flattened index (depth-first order).
fn get_piece_by_index(root: &S3OPiece, target: usize) -> Option<&S3OPiece> {
    let mut counter = 0;
    get_piece_recursive(root, target, &mut counter)
}

fn get_piece_recursive<'a>(
    piece: &'a S3OPiece,
    target: usize,
    counter: &mut usize,
) -> Option<&'a S3OPiece> {
    if *counter == target {
        return Some(piece);
    }
    *counter += 1;
    for child in &piece.children {
        if let Some(found) = get_piece_recursive(child, target, counter) {
            return Some(found);
        }
    }
    None
}

fn piece_to_mesh(piece: &S3OPiece) -> Mesh {
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

fn get_or_create_invisible_material(
    commands: &mut Commands,
    materials: &mut Assets<StandardMaterial>,
) -> SelectionVolumeMaterial {
    let mat = SelectionVolumeMaterial(materials.add(StandardMaterial {
        base_color: Color::srgba(0.0, 0.0, 0.0, 0.0),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    }));
    commands.insert_resource(mat.clone());
    mat
}

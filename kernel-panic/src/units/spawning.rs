use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use spring_cob::CobVm;
use spring_unit_mesh::S3OPiece;

use spring_map::map_types::{ParsedMap, SQUARE_SIZE};
use spring_map::smd_parser::MapInfo;

use super::animation::{CobAnimator, CobFileCache, PieceIndex, load_cob_cached};
use super::combat::Deployable;
use super::components::{Faction, Health, Homebase, SelectionVolume, TeamId, UnitType};
use super::definitions::UnitKind;
use super::meshes::{S3OModelCache, unit_material, unit_radius};
use super::production::default_production;
use super::unit_registry::UnitRegistry;
use crate::MapEntity;

const FACTION_ORDER: [Faction; 3] = [Faction::System, Faction::Hacker, Faction::Network];

#[derive(Resource, Clone)]
pub struct SelectionVolumeMaterial(pub Handle<StandardMaterial>);

/// Marks a freshly-built unit that's rising up through its construction hole.
/// The `emerge_system` lerps the entity's Y coordinate from below ground up
/// to `target_y` over `total` seconds, then strips the component (and gives
/// the unit its post-emergence rally order, if any).
#[derive(Component)]
pub struct Emerging {
    /// Final Y coordinate the unit should reach when fully emerged.
    pub target_y: f32,
    /// Seconds remaining in the emerge animation.
    pub remaining: f32,
    /// Total duration of the emerge animation (used to compute lerp t).
    pub total: f32,
    /// World point the unit should walk to once it has emerged. `None` for
    /// stationary units that don't need to clear the factory.
    pub rally_point: Option<Vec3>,
}

/// Cached piece-index lookup for animated factories. Set once when a
/// producer spawns; lets the production system pull the live world
/// position of script-driven pieces (the `nanoemitter` that orbits the
/// build hole, and the `pad` that defines the emergence point) without
/// rescanning the model every frame.
///
/// Falls back to `None` for either field if the model doesn't have the
/// expected piece — the production system then uses the factory's root
/// transform instead.
#[derive(Component, Default)]
pub struct FactoryPieces {
    pub nanoemitter: Option<usize>,
    pub pad: Option<usize>,
}

/// Lifts `Emerging` units smoothly upward through the build hole. When the
/// animation completes, the component is removed and the unit is given its
/// rally-walk command (if any).
pub fn emerge_system(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut Emerging)>,
) {
    let dt = time.delta_secs();
    for (entity, mut transform, mut emerging) in &mut q {
        emerging.remaining = (emerging.remaining - dt).max(0.0);
        // t goes 0 → 1 over the duration; ease-out so the unit decelerates
        // as it reaches the surface (more "machine settling into place").
        let raw = 1.0 - (emerging.remaining / emerging.total).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - raw).powi(2);
        let start_y = emerging.target_y - EMERGE_DEPTH;
        transform.translation.y = start_y + (emerging.target_y - start_y) * eased;

        if emerging.remaining <= 0.0 {
            transform.translation.y = emerging.target_y;
            let rally = emerging.rally_point;
            commands.entity(entity).remove::<Emerging>();
            if let Some(target) = rally {
                commands
                    .entity(entity)
                    .insert(crate::interaction::movement::MoveTarget(target))
                    .remove::<crate::interaction::movement::MovePath>();
            }
        }
    }
}

/// Distance below ground that a freshly-built unit starts at. The
/// `emerge_system` lifts it back up by this much. Roughly the height of a
/// typical unit so the model is fully hidden underground at t=0.
pub const EMERGE_DEPTH: f32 = 40.0;
/// Seconds the emerge animation takes from below-ground to fully out.
pub const EMERGE_DURATION: f32 = 0.6;

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
    unit_registry: &UnitRegistry,
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
            unit_registry,
        );
    }

    info!("Spawned {} homebases", map_info.start_positions.len());
}

/// Mobile unit kinds, in display order — one per slot on the showcase map.
/// Excludes stationary units (LogicBomb, Exploit) and units that only spawn
/// dynamically (Virus, which is created from Worm kills).
const SHOWCASE_KINDS: &[UnitKind] = &[
    UnitKind::Assembler,
    UnitKind::Bit,
    UnitKind::Byte,
    UnitKind::Pointer,
    UnitKind::Bug,
    UnitKind::Worm,
    UnitKind::Dos,
    UnitKind::Virus,
    UnitKind::Packet,
    UnitKind::Signal,
];

/// Spawn one of each mobile unit at the map's start positions, instead of
/// the usual three-faction homebases. Used by the `Showcase` map for visual
/// inspection of unit models / animations / pathing in isolation.
#[allow(clippy::too_many_arguments)]
pub fn spawn_showcase(
    parsed: &ParsedMap,
    map_info: &MapInfo,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cob_cache: &mut CobFileCache,
    unit_registry: &UnitRegistry,
) {
    let invisible_mat = get_or_create_invisible_material(commands, materials);
    let square_size = SQUARE_SIZE as f32;
    let heightmap_w = parsed.header.heightmap_width();
    let heightmap_h = parsed.header.heightmap_height();

    for (slot, start_pos) in map_info.start_positions.iter().enumerate() {
        let Some(&kind) = SHOWCASE_KINDS.get(slot) else {
            break;
        };
        let hx = (start_pos.x / square_size).clamp(0.0, (heightmap_w - 1) as f32) as usize;
        let hz = (start_pos.z / square_size).clamp(0.0, (heightmap_h - 1) as f32) as usize;
        let height = parsed.heights[hz * heightmap_w + hx];
        let position = Vec3::new(start_pos.x, height, start_pos.z);

        spawn_unit(
            kind,
            kind.faction(),
            slot as u8,
            position,
            commands,
            meshes,
            materials,
            images,
            model_cache,
            cob_cache,
            &invisible_mat,
            unit_registry,
        );
    }

    info!(
        "Showcase: spawned {} mobile units",
        SHOWCASE_KINDS.len().min(map_info.start_positions.len())
    );
}

/// Spawn a single unit with per-piece children and COB animation.
/// Returns the root entity of the spawned unit.
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
    unit_registry: &UnitRegistry,
) -> Entity {
    let model_name = unit_registry.model(kind);
    let material = unit_material(kind, faction, materials, images, model_cache, model_name);
    let radius = unit_radius(kind, model_cache, unit_registry);
    let selection_sphere = meshes.add(Sphere::new(radius).mesh().ico(3).unwrap());

    // Spawn the root unit entity.
    let unit_entity = commands
        .spawn((
            MapEntity,
            UnitType(kind),
            faction,
            TeamId(team),
            Health::full(unit_registry.max_health(kind)),
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
    if matches!(kind, UnitKind::Pointer) {
        commands.entity(unit_entity).insert(Deployable::initial());
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
    let s3o_model = super::meshes::load_s3o_model(model_name, model_cache);

    if let Some(model) = &s3o_model {
        // Flatten the piece tree into a list, spawning each as a child entity.
        let mut piece_entities = Vec::new();
        let mut piece_parents: Vec<Option<usize>> = Vec::new();
        let mut piece_offsets: Vec<[f32; 3]> = Vec::new();
        flatten_pieces(
            &model.root_piece,
            None,
            &mut piece_parents,
            &mut piece_offsets,
        );

        // Spawn piece entities.
        for (idx, parent_idx) in piece_parents.iter().enumerate() {
            let piece = get_piece_by_index(&model.root_piece, idx);
            let has_geometry = piece.is_some_and(|p| !p.vertices.is_empty());
            let offset = piece_offsets[idx];

            let piece_cmd = if has_geometry {
                let piece = piece.unwrap();
                let mesh = piece_to_mesh(piece);
                let mesh_handle = meshes.add(mesh);
                commands.spawn((
                    PieceIndex(idx),
                    Mesh3d(mesh_handle),
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
            let piece_entity = piece_cmd.id();
            piece_entities.push(piece_entity);

            let bevy_parent = match parent_idx {
                Some(pi) => piece_entities[*pi],
                None => unit_entity,
            };
            commands.entity(bevy_parent).add_child(piece_entity);
        }

        // Attach CobAnimator with base offsets.
        if let Some(cob) = load_cob_cached(&kind.script(), cob_cache) {
            let num_pieces = piece_entities.len();
            let mut vm = CobVm::new(&cob);
            vm.start_script(&cob, "Create", &[]);

            commands.entity(unit_entity).insert(CobAnimator {
                vm,
                cob,
                piece_entities: piece_entities.clone(),
                piece_base_offsets: piece_offsets,
                piece_rotations: vec![[0.0; 3]; num_pieces],
                piece_translations: vec![[0.0; 3]; num_pieces],
                target_rotations: vec![[0.0; 3]; num_pieces],
                turn_speeds: vec![[0.0; 3]; num_pieces],
                target_translations: vec![[0.0; 3]; num_pieces],
                move_speeds: vec![[0.0; 3]; num_pieces],
                spin_speeds: vec![[0.0; 3]; num_pieces],
            });
        }

        // For factories, cache the piece indices we need for build FX so
        // the production system can read their world transforms each frame
        // without rescanning the model. Both lookups are best-effort: any
        // missing piece falls back to the factory root.
        if default_production(kind).is_some() {
            commands.entity(unit_entity).insert(FactoryPieces {
                nanoemitter: find_piece_index_by_name(&model.root_piece, "nanoemitter"),
                pad: find_piece_index_by_name(&model.root_piece, "pad"),
            });
        }
    } else {
        // Fallback: single flattened mesh, no animation.
        let mesh = super::meshes::unit_mesh(kind, meshes, model_cache, unit_registry);
        commands
            .entity(unit_entity)
            .insert((Mesh3d(mesh), MeshMaterial3d(material)));
    }

    unit_entity
}

/// Flatten the piece tree depth-first, recording each piece's parent index.
fn flatten_pieces(
    piece: &S3OPiece,
    parent_idx: Option<usize>,
    result: &mut Vec<Option<usize>>,
    offsets: &mut Vec<[f32; 3]>,
) {
    let my_idx = result.len();
    result.push(parent_idx);
    offsets.push(piece.offset);
    for child in &piece.children {
        flatten_pieces(child, Some(my_idx), result, offsets);
    }
}

/// Get a piece by its flattened index (depth-first order).
fn get_piece_by_index(root: &S3OPiece, target: usize) -> Option<&S3OPiece> {
    let mut counter = 0;
    get_piece_recursive(root, target, &mut counter)
}

/// Find the flattened (depth-first) index of the first piece whose name
/// matches `target` case-insensitively. `None` if the model has no such
/// piece. Used by factories to cache their `nanoemitter` / `pad` indices.
fn find_piece_index_by_name(root: &S3OPiece, target: &str) -> Option<usize> {
    let mut counter = 0;
    find_by_name_recursive(root, target, &mut counter)
}

fn find_by_name_recursive(piece: &S3OPiece, target: &str, counter: &mut usize) -> Option<usize> {
    if piece.name.eq_ignore_ascii_case(target) {
        return Some(*counter);
    }
    *counter += 1;
    for child in &piece.children {
        if let Some(found) = find_by_name_recursive(child, target, counter) {
            return Some(found);
        }
    }
    None
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

/// System: drain the `VirusSpawnQueue` and spawn Virus units at the queued
/// positions. Runs after the death system so kills in a given frame produce
/// Viruses on the next.
#[allow(clippy::too_many_arguments)]
pub fn spawn_queued_viruses(
    mut virus_spawns: ResMut<super::combat::VirusSpawnQueue>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    sel_mat: Option<Res<SelectionVolumeMaterial>>,
    unit_registry: Res<UnitRegistry>,
) {
    if virus_spawns.0.is_empty() {
        return;
    }

    let invisible_mat = match sel_mat {
        Some(m) => m.clone(),
        None => get_or_create_invisible_material(&mut commands, &mut materials),
    };

    for (pos, faction, team) in virus_spawns.0.drain(..) {
        spawn_unit(
            UnitKind::Virus,
            faction,
            team,
            pos,
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut model_cache,
            &mut cob_cache,
            &invisible_mat,
            &unit_registry,
        );
    }
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

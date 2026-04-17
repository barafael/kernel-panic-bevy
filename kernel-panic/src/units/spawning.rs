use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use spring_cob::CobVm;
use spring_unit_mesh::S3OPiece;

use spring_map::smd_parser::MapInfo;

use super::animation::{CobAnimator, CobFileCache, PieceIndex, load_cob_cached};
use super::combat::Deployable;
use super::components::{Faction, Health, Homebase, SelectionVolume, TeamId, UnitType};
use super::definitions::UnitKind;
use super::meshes::{S3OModelCache, unit_material, unit_radius};
use super::production::default_production;
use super::unit_registry::UnitRegistry;
use crate::map_loading::MapEntity;
use crate::terrain::heightmap::Heightmap;

const FACTION_ORDER: [Faction; 3] = [Faction::System, Faction::Hacker, Faction::Network];

#[derive(Resource, Clone)]
pub struct SelectionVolumeMaterial(pub Handle<StandardMaterial>);

/// Marks a freshly-built unit that hasn't finished emerging from its
/// construction site. `emerge_system` ticks `remaining` toward 0 over
/// `total` seconds; how the visible model arrives depends on `style`.
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
    /// How the model becomes visible during the rise window.
    pub style: EmergeStyle,
}

/// Per-faction emergence visual.
///
/// - `Rise` — System units (Kernel-built). Spawn underground at
///   `target_y - EMERGE_DEPTH` and lerp Y up to surface, with their own
///   COB `Create()` script also moving the `base` piece up via
///   `BUILD_PERCENT_LEFT`.
/// - `Fade` — Hacker / Network units (Hole, Connection, Window, Port).
///   Spawn at surface but materialize via an alpha ramp on a per-unit
///   cloned material. Mirrors upstream's `lua_SetAlphaThreshold(255 → 0)`
///   pattern in bug.bos / packet.bos / connection.bos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmergeStyle {
    Rise,
    Fade,
}

/// Per-piece original-material handles, restored when an entity finishes
/// fading in. Spawned alongside `Emerging { Fade }` so the per-unit
/// alpha ramp doesn't bleed into the shared faction-colored material.
#[derive(Component)]
pub struct FadeMaterials {
    /// (piece_entity, faded_clone, original) tuples.
    pub overrides: Vec<(Entity, Handle<StandardMaterial>, Handle<StandardMaterial>)>,
}

/// Cached piece-index lookup for animated factories. Set once when a
/// producer spawns; lets the production system pull the live world
/// position of script-driven pieces without rescanning the model every
/// frame.
///
/// `emitters` is the list of pieces that draw a build laser to `pad`
/// while the factory is constructing something. The set is faction-
/// specific:
/// - **kernel** has `tip0..tip3` on its 4 pillars (4 rays).
/// - **socket** has `blaser0`/`blaser1` (2 rays).
/// - **hole**/**window**/**connection**/**port** have a single
///   `nanoemitter` (or fall back to a root offset).
/// - **carrier** has `mover` (the lifting hatch — emits from its raised
///   position).
///
/// `pad` is the build target / emergence point. Both fields fall back
/// to `None` when the model doesn't have the expected pieces, in which
/// case the production system uses the factory's root transform.
#[derive(Component, Default)]
pub struct FactoryPieces {
    pub emitters: Vec<usize>,
    pub pad: Option<usize>,
}

/// Tick `Emerging` units forward — either lerping Y upward (Rise style)
/// or ramping per-piece alpha (Fade style). When the timer expires the
/// component is removed, faded materials are restored to the shared
/// originals, and the unit gets its rally-walk command if any.
pub fn emerge_system(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &mut Transform,
        &mut Emerging,
        Option<&FadeMaterials>,
    )>,
    piece_mats: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();
    for (entity, mut transform, mut emerging, fade) in &mut q {
        emerging.remaining = (emerging.remaining - dt).max(0.0);
        // t goes 0 → 1 over the duration.
        let t = (1.0 - emerging.remaining / emerging.total).clamp(0.0, 1.0);

        match emerging.style {
            EmergeStyle::Rise => {
                // Ease-out so the unit decelerates as it reaches the surface
                // (reads as "machine settling into place").
                let eased = 1.0 - (1.0 - t).powi(2);
                let start_y = emerging.target_y - EMERGE_DEPTH;
                transform.translation.y = start_y + (emerging.target_y - start_y) * eased;
            }
            EmergeStyle::Fade => {
                // Linear alpha ramp; pieces stay at surface y throughout.
                if let Some(fade) = fade {
                    for (_, faded_handle, _) in &fade.overrides {
                        if let Some(mat) = materials.get_mut(faded_handle) {
                            mat.base_color = mat.base_color.with_alpha(t);
                        }
                    }
                }
            }
        }

        if emerging.remaining <= 0.0 {
            if matches!(emerging.style, EmergeStyle::Rise) {
                transform.translation.y = emerging.target_y;
            }
            // Restore the shared faction material on every piece we
            // overrode, so future asset swaps / faction recolors take
            // effect on this unit too. The cloned faded handle leaks
            // into the assets pool until despawn — fine, it's small.
            if let Some(fade) = fade {
                for (piece_entity, _, original) in &fade.overrides {
                    if piece_mats.get(*piece_entity).is_ok() {
                        commands
                            .entity(*piece_entity)
                            .insert(MeshMaterial3d(original.clone()));
                    }
                }
                commands.entity(entity).remove::<FadeMaterials>();
            }
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
/// How long before the build cycle completes the unit appears underground
/// and starts rising. Picked so the rise feels like part of the build
/// rather than an after-effect — the player sees ~1.5s of "the laser
/// drew this thing into being". Clamped against `build_time` so very
/// short cycles still finish naturally.
pub const EMERGE_LEAD_TIME: f32 = 1.5;

#[allow(clippy::too_many_arguments)]
pub fn spawn_homebases(
    heightmap: &Heightmap,
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

    for start_pos in &map_info.start_positions {
        let faction = FACTION_ORDER[start_pos.team as usize % FACTION_ORDER.len()];
        let kind = faction.homebase();

        let position = heightmap.place(start_pos.x, start_pos.z);

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
    UnitKind::Trojan,
    UnitKind::Virus,
    UnitKind::Packet,
    UnitKind::Signal,
    UnitKind::Gateway,
    UnitKind::Flow,
];

/// Spawn one of each mobile unit at the map's start positions, instead of
/// the usual three-faction homebases. Used by the `Showcase` map for visual
/// inspection of unit models / animations / pathing in isolation.
#[allow(clippy::too_many_arguments)]
pub fn spawn_showcase(
    heightmap: &Heightmap,
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

    let mut showcase_positions: Vec<(UnitKind, Vec3)> = Vec::new();

    for (slot, start_pos) in map_info.start_positions.iter().enumerate() {
        let Some(&kind) = SHOWCASE_KINDS.get(slot) else {
            break;
        };
        let position = heightmap.place(start_pos.x, start_pos.z);
        showcase_positions.push((kind, position));

        // All showcase units share team 0 so they never engage each other
        // — the goal is visual inspection, not gameplay.
        spawn_unit(
            kind,
            kind.faction(),
            0,
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

    // Plant one team-1 target ~120 elmos from each showcase unit so
    // every armed slot has something nearby to engage — many KP weapons
    // have ranges in the 200-450 elmo bracket, well below the spread of
    // the 4×3 showcase grid, so a single centroid target was unreachable
    // for most units. Using Bit (System) as the target works for the
    // Hacker/Network attackers; we additionally spawn a Bug (Hacker) for
    // System attackers so every unit has at least one cross-faction
    // enemy in range. The target factions are chosen to be different
    // from the attacker's faction (combat skips faction-mates).
    let mut targets_spawned = 0usize;
    for (kind, position) in &showcase_positions {
        let attacker_faction = kind.faction();
        let target_kind = match attacker_faction {
            Faction::System => UnitKind::Bug,
            Faction::Hacker | Faction::Network => UnitKind::Bit,
        };
        let offset = Vec3::new(220.0, 0.0, 0.0);
        let target_xz = *position + offset;
        let target_pos = heightmap.place(target_xz.x, target_xz.z);
        spawn_unit(
            target_kind,
            target_kind.faction(),
            1,
            target_pos,
            commands,
            meshes,
            materials,
            images,
            model_cache,
            cob_cache,
            &invisible_mat,
            unit_registry,
        );
        targets_spawned += 1;
    }

    info!(
        "Showcase: spawned {} mobile units + {} sacrificial targets",
        showcase_positions.len(),
        targets_spawned,
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

    // Some s3o models are authored with their root at the mesh CENTER
    // rather than at the bottom (octaeder.s3o, used by Byte, has blade
    // vertices spanning y∈[-48,48]). If we plant the root at the
    // heightmap, half the model sinks below ground. Lift the spawn point
    // by however much the lowest vertex extends below piece-tree origin.
    let ground_lift = super::meshes::load_s3o_model(model_name, model_cache)
        .map(|m| compute_ground_lift(&m.root_piece, [0.0, 0.0, 0.0]))
        .unwrap_or(0.0);
    let lifted_position = position + Vec3::new(0.0, ground_lift, 0.0);

    // Spawn the root unit entity.
    let unit_entity = commands
        .spawn((
            MapEntity,
            UnitType(kind),
            faction,
            TeamId(team),
            Health::full(unit_registry.max_health(kind)),
            Transform::from_translation(lifted_position),
            Visibility::default(),
        ))
        .id();

    commands.entity(unit_entity).insert((
        super::combat::IdleTimer(0.0),
        super::combat::StunCharge(0.0),
    ));

    if super::cloak::spawns_cloaked(kind) {
        commands.entity(unit_entity).insert(super::cloak::Cloaked);
    }

    if kind == UnitKind::Port {
        commands
            .entity(unit_entity)
            .insert(super::network_buffer::PortTimer::default());
    }

    if kind == UnitKind::Flow {
        commands
            .entity(unit_entity)
            .insert(super::network_buffer::SpeedBoost::default());
    }

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

        // Attach CobAnimator with base offsets. The COB script references
        // pieces by index into its own `piece_names` table, which is the
        // declaration order in the .bos source — *not* the s3o depth-first
        // flatten order. Build a lookup that lets us address pieces by COB
        // index everywhere downstream by reordering `piece_entities` and
        // `piece_offsets` to match the COB's order. Pieces named in the
        // .bos that don't exist in the s3o stay as a stub entity at the
        // unit root (zero offset) so animations targeting them are no-ops
        // instead of indexing into the wrong piece.
        if let Some(cob) = load_cob_cached(&kind.script(), cob_cache) {
            let cob_piece_count = cob.piece_names.len();
            let mut cob_entities = Vec::with_capacity(cob_piece_count);
            let mut cob_offsets = Vec::with_capacity(cob_piece_count);
            for cob_name in &cob.piece_names {
                match find_piece_index_by_name(&model.root_piece, cob_name) {
                    Some(s3o_idx) => {
                        cob_entities.push(piece_entities[s3o_idx]);
                        cob_offsets.push(piece_offsets[s3o_idx]);
                    }
                    None => {
                        // Stub entity so VM operations on this slot don't
                        // accidentally hit a real piece.
                        let stub = commands
                            .spawn((Transform::default(), Visibility::default()))
                            .id();
                        commands.entity(unit_entity).add_child(stub);
                        cob_entities.push(stub);
                        cob_offsets.push([0.0; 3]);
                    }
                }
            }

            let mut vm = CobVm::new(&cob);
            vm.start_script(&cob, "Create", &[]);

            commands.entity(unit_entity).insert(CobAnimator {
                vm,
                cob,
                piece_entities: cob_entities,
                piece_base_offsets: cob_offsets,
                piece_rotations: vec![[0.0; 3]; cob_piece_count],
                piece_translations: vec![[0.0; 3]; cob_piece_count],
                target_rotations: vec![[0.0; 3]; cob_piece_count],
                turn_speeds: vec![[0.0; 3]; cob_piece_count],
                target_translations: vec![[0.0; 3]; cob_piece_count],
                move_speeds: vec![[0.0; 3]; cob_piece_count],
                spin_speeds: vec![[0.0; 3]; cob_piece_count],
                linear_constant: kind.cob_linear_constant(),
            });
        }

        // For factories, cache the piece indices we need for build FX so
        // the production system can read their world transforms each frame
        // without rescanning the model. Indices are into the COB piece
        // table (which is what `CobAnimator::piece_entities` is keyed on
        // post-remapping above) — `None` if the model has no such piece,
        // in which case the production system falls back to the factory
        // root.
        if default_production(kind).is_some() {
            let cob = load_cob_cached(&kind.script(), cob_cache);
            let cob_index = |name: &str| -> Option<usize> {
                cob.as_ref().and_then(|c| {
                    c.piece_names
                        .iter()
                        .position(|p| p.eq_ignore_ascii_case(name))
                })
            };
            // Faction-specific emitter piece names, in the order they
            // appear in the upstream .bos for each factory. Pieces that
            // don't exist on this model resolve to None and are filtered
            // out, so unknown factories naturally fall through to the
            // single-nanoemitter case (or to the synthetic-offset
            // fallback in production_system if even that's missing).
            //
            // Note: upstream's Network homebase is Carrier (with `mover`
            // hatch); we use Connection instead, which has no production
            // pieces in its .bos at all. Connection therefore falls
            // through to the nanoemitter case → synthetic offset.
            let emitter_names: &[&str] = match kind {
                UnitKind::Kernel => &["tip0", "tip1", "tip2", "tip3"],
                UnitKind::Socket => &["blaser0", "blaser1"],
                _ => &["nanoemitter"],
            };
            let emitters: Vec<usize> = emitter_names.iter().filter_map(|n| cob_index(n)).collect();
            commands.entity(unit_entity).insert(FactoryPieces {
                emitters,
                pad: cob_index("pad"),
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

/// Walk the piece tree and return how far below the root origin the
/// lowest vertex sits, in elmos. Pieces inherit their parents' offsets
/// so their world-space y is parent_world_y + piece.offset.y +
/// vertex.y. Returns 0.0 if every vertex is at or above y=0 (the
/// common case — most models are authored with the root at ground).
fn compute_ground_lift(piece: &S3OPiece, parent_origin: [f32; 3]) -> f32 {
    let min_y = walk_min_y(piece, parent_origin);
    if min_y >= 0.0 { 0.0 } else { -min_y }
}

fn walk_min_y(piece: &S3OPiece, parent_origin: [f32; 3]) -> f32 {
    let origin = [
        parent_origin[0] + piece.offset[0],
        parent_origin[1] + piece.offset[1],
        parent_origin[2] + piece.offset[2],
    ];
    let mut min_y = f32::INFINITY;
    for v in &piece.vertices {
        min_y = min_y.min(origin[1] + v.position[1]);
    }
    for child in &piece.children {
        min_y = min_y.min(walk_min_y(child, origin));
    }
    min_y
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

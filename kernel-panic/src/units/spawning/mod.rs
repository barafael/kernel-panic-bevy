//! Unit spawning orchestration.
//!
//! The core [`spawn_unit`] mounts a unit's s3o model, attaches every
//! gameplay component (combat timers, faction-specific markers, COB
//! animator with resolved muzzle/gunbase/hatch piece indices), and
//! registers faction-specific build-FX pieces for factories. Helpers on
//! top of it — [`spawn_homebases`], [`spawn_queued_viruses`],
//! [`spawn_queued_mines`] — wire in the assets, registry, and invisible
//! selection-volume material once at the system boundary.
//!
//! Split into:
//! - [`emerge`] — [`Emerging`] / [`FadeMaterials`] lifecycle.
//! - [`s3o_mount`] — piece-tree walking helpers used by `spawn_unit`.
//! - [`mod`](self) — [`spawn_unit`] + the top-level spawners.

use bevy::prelude::*;

use spring_cob::CobVm;

use spring_map::smd_parser::MapInfo;

use super::animation::{CobAnimator, CobFileCache, PieceIndex, load_cob_cached};
use super::combat::Deployable;
use super::components::{Faction, Health, Homebase, SelectionVolume, TeamId, UnitStats, UnitType};
use super::definitions::UnitKind;
use super::meshes::{S3OModelCache, unit_material, unit_radius};
use super::production::default_production;
use super::unit_registry::UnitRegistry;
use crate::terrain::heightmap::Heightmap;

mod emerge;
mod s3o_mount;

pub use emerge::{
    EMERGE_DEPTH, EMERGE_LEAD_TIME, EmergeStyle, Emerging, FadeMaterials, emerge_system,
};

use s3o_mount::{
    compute_ground_lift, find_piece_index_by_name, flatten_pieces, get_piece_by_index,
    piece_to_mesh,
};

/// Shared handle to the fully-transparent material applied to every
/// unit's `SelectionVolume` child. Lazily created on first spawn so
/// maps with no units never mint a material asset.
#[derive(Resource, Clone)]
pub struct SelectionVolumeMaterial(pub Handle<StandardMaterial>);

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
        let faction = Faction::from_team_id(start_pos.team as u8);
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

    let unit_entity = commands
        .spawn((
            UnitType(kind),
            faction,
            TeamId(team),
            Health::full(unit_registry.max_health(kind)),
            UnitStats {
                radius: unit_registry.collision_radius(kind),
                hit_radius: radius,
                speed: unit_registry.speed(kind),
                turn_rate: unit_registry.turn_rate(kind),
                can_fly: unit_registry.can_fly(kind),
                cruise_alt: unit_registry.cruise_alt(kind),
                no_chase_vtol: unit_registry.no_chase_vtol(kind),
            },
            Transform::from_translation(lifted_position),
            Visibility::default(),
        ))
        .id();

    commands.entity(unit_entity).insert((
        super::combat::IdleTimer(0.0),
        super::combat::StunCharge(0.0),
    ));

    if kind.spawns_cloaked() {
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
    // While there is no real "enemy" team — every faction is AI-driven
    // but human-controllable — spot every unit at spawn so the fog-of-war
    // pass leaves them all visible. Switch this back to a per-team check
    // when a proper player/AI distinction lands.
    commands.entity(unit_entity).insert(super::cloak::Spotted);
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
                    PieceIndex,
                    Mesh3d(mesh_handle),
                    MeshMaterial3d(material.clone()),
                    Transform::from_xyz(offset[0], offset[1], offset[2]),
                    Visibility::default(),
                ))
            } else {
                commands.spawn((
                    PieceIndex,
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

            // Resolve muzzle piece before moving `cob` into CobAnimator.
            // Ordered by upstream KP convention: `gunpoint` covers
            // Bit/Pointer/DOS/Exploit*, `bp0` the Byte's first barrel,
            // `flare`/`barrel`/`muzzle` as safety nets for any third-
            // party unit that uses the generic names.
            const MUZZLE_NAMES: &[&str] = &["gunpoint", "bp0", "flare", "barrel", "muzzle"];
            let piece_index = |name: &str| -> Option<usize> {
                cob.piece_names
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(name))
            };
            let muzzle_idx = MUZZLE_NAMES.iter().find_map(|n| piece_index(n));
            let gunbase_idx = piece_index("gunbase");
            let hatch_idx = piece_index("body");

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

            if let Some(idx) = muzzle_idx {
                commands
                    .entity(unit_entity)
                    .insert(super::animation::MuzzlePiece(idx));
            }
            if let Some(idx) = gunbase_idx {
                commands
                    .entity(unit_entity)
                    .insert(super::animation::GunbasePiece(idx));
            }
            if kind == UnitKind::Connection
                && let Some(idx) = hatch_idx
            {
                commands
                    .entity(unit_entity)
                    .insert(super::animation::HatchPiece(idx));
            }
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

/// Drain the `VirusSpawnQueue` and spawn Virus units at the queued
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
    let invisible_mat = match sel_mat {
        Some(m) => m.clone(),
        None => get_or_create_invisible_material(&mut commands, &mut materials),
    };

    for spawn in virus_spawns.drain() {
        spawn_unit(
            UnitKind::Virus,
            spawn.faction,
            spawn.team,
            spawn.position,
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

/// Drain the `MineSpawnQueue` and spawn Logic Bombs at the queued
/// positions. Sibling of `spawn_queued_viruses`; runs in the same
/// `Resolve` set so a Byte's `LaunchMines` cast in frame N produces
/// mines visible in frame N+1's Simulate pass. Logic Bombs auto-pick
/// up `Cloaked` via `UnitKind::spawns_cloaked`, so they behave like
/// factory-built mines the moment they appear.
#[allow(clippy::too_many_arguments)]
pub fn spawn_queued_mines(
    mut mine_spawns: ResMut<super::command_fire::MineSpawnQueue>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    sel_mat: Option<Res<SelectionVolumeMaterial>>,
    unit_registry: Res<UnitRegistry>,
) {
    let invisible_mat = match sel_mat {
        Some(m) => m.clone(),
        None => get_or_create_invisible_material(&mut commands, &mut materials),
    };

    for spawn in mine_spawns.drain() {
        spawn_unit(
            UnitKind::LogicBomb,
            spawn.faction,
            spawn.team,
            spawn.position,
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

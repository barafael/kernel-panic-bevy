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

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::units::assets::animation::PieceIndex;
use spring_map::smd_parser::MapInfo;

use super::production::default_production;
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::meshes::{S3OModelCache, unit_material, unit_radius};
use crate::units::combat::Deployable;
use crate::units::components::{
    Faction, Health, Homebase, SelectionVolume, TeamId, UnitStats, UnitType,
};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

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

/// Bundles the asset / cache / registry resources `spawn_unit` needs.
///
/// Every system that calls `spawn_unit` previously had to declare 8
/// separate `Commands` / `ResMut<Assets<…>>` / `ResMut<…Cache>` params
/// and forward them through; bundling them as one `SystemParam` shrinks
/// each call site to a single argument and lets helper functions reborrow
/// `&mut SpawnContext` without re-listing the same eight types.
///
/// `sel_mat` is `Option` because the resource is lazily created on the
/// first spawn — a fresh app boot has no `SelectionVolumeMaterial` until
/// `ensure_invisible_material` mints it.
#[derive(SystemParam)]
pub struct SpawnContext<'w, 's> {
    pub commands: Commands<'w, 's>,
    pub meshes: ResMut<'w, Assets<Mesh>>,
    pub materials: ResMut<'w, Assets<StandardMaterial>>,
    pub images: ResMut<'w, Assets<Image>>,
    pub model_cache: ResMut<'w, S3OModelCache>,
    pub sel_mat: Option<Res<'w, SelectionVolumeMaterial>>,
    pub unit_registry: Res<'w, UnitRegistry>,
    pub weapon_registry: Res<'w, crate::units::content::weapons::WeaponRegistry>,
}

impl SpawnContext<'_, '_> {
    /// Get the shared invisible-selection material, lazy-initialising the
    /// resource on first call. Mirrors the previous standalone helper so
    /// the first spawn on a fresh app boot still works without requiring
    /// a startup system to plant the resource.
    fn ensure_invisible_material(&mut self) -> SelectionVolumeMaterial {
        if let Some(m) = &self.sel_mat {
            return SelectionVolumeMaterial(m.0.clone());
        }
        let mat = SelectionVolumeMaterial(self.materials.add(StandardMaterial {
            base_color: Color::srgba(0.0, 0.0, 0.0, 0.0),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        }));
        self.commands.insert_resource(mat.clone());
        mat
    }
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

/// Per-faction starting entry: the team slot, faction tag, and the
/// homebase kind to plant at the map's declared start position.
struct FactionRoster {
    faction: Faction,
    homebase: UnitKind,
}

/// Margin from the map edge when clamping a homebase's start position.
/// Keeps a Kernel/Hole/Carrier footprint clear of the world boundary
/// even when the map's declared start position sits right on it.
const HOMEBASE_EDGE_MARGIN: f32 = 100.0;

/// Sandbox mode roster: only the three homebases, each on its own team
/// (and faction), all human-controllable. Different teams + factions
/// means [`is_friendly`](super::super::components::is_friendly) returns
/// false across pairs, so units produced by these bases will engage on
/// sight as enemies.
const ROSTERS: [FactionRoster; 3] = [
    FactionRoster {
        faction: Faction::System,
        homebase: UnitKind::Kernel,
    },
    FactionRoster {
        faction: Faction::Hacker,
        homebase: UnitKind::Hole,
    },
    FactionRoster {
        faction: Faction::Network,
        homebase: UnitKind::Carrier,
    },
];

/// Spawn one homebase per faction at the map's declared start position.
/// No datavent buildings, no mobile-unit clusters — anything past the
/// three bases comes from in-game production.
pub fn spawn_homebases(heightmap: &Heightmap, map_info: &MapInfo, ctx: &mut SpawnContext) {
    let (world_w, world_d) = heightmap.world_size();
    let cx = world_w * 0.5;
    let cz = world_d * 0.5;
    let radius = world_w.min(world_d) * 0.30;
    let fallback_positions = [
        (cx + radius, cz),
        (cx - radius * 0.5, cz - radius * 0.866),
        (cx - radius * 0.5, cz + radius * 0.866),
    ];

    for (i, roster) in ROSTERS.iter().enumerate() {
        let team = i as u8;
        let (fx, fz) = map_info
            .start_positions
            .get(i)
            .map(|sp| (sp.x, sp.z))
            .unwrap_or(fallback_positions[i]);
        let fx = fx.clamp(HOMEBASE_EDGE_MARGIN, world_w - HOMEBASE_EDGE_MARGIN);
        let fz = fz.clamp(HOMEBASE_EDGE_MARGIN, world_d - HOMEBASE_EDGE_MARGIN);
        let home_pos = heightmap.place(fx, fz);

        spawn_unit(roster.homebase, roster.faction, team, home_pos, ctx);
    }

    info!(
        "Spawned starter roster: {} homebases across {} factions",
        ROSTERS.len(),
        ROSTERS.len(),
    );
}

/// Showcase mode: spawn exactly one homebase for `faction` on team 0 at
/// the map's first start position (or the map centre as fallback).
pub fn spawn_showcase_homebase(
    heightmap: &Heightmap,
    map_info: &MapInfo,
    faction: Faction,
    ctx: &mut SpawnContext,
) {
    let (world_w, world_d) = heightmap.world_size();
    let (fx, fz) = map_info
        .start_positions
        .first()
        .map(|sp| (sp.x, sp.z))
        .unwrap_or((world_w * 0.5, world_d * 0.5));
    let fx = fx.clamp(HOMEBASE_EDGE_MARGIN, world_w - HOMEBASE_EDGE_MARGIN);
    let fz = fz.clamp(HOMEBASE_EDGE_MARGIN, world_d - HOMEBASE_EDGE_MARGIN);
    let home_pos = heightmap.place(fx, fz);

    spawn_unit(faction.homebase(), faction, 0, home_pos, ctx);
    info!(
        "Showcase({:?}): spawned {:?} homebase at ({:.0}, {:.0})",
        faction,
        faction.homebase(),
        fx,
        fz,
    );
}

/// Spawn a single unit with per-piece children and COB animation.
/// Returns the root entity of the spawned unit.
pub fn spawn_unit(
    kind: UnitKind,
    faction: Faction,
    team: u8,
    position: Vec3,
    ctx: &mut SpawnContext,
) -> Entity {
    let invisible_mat = ctx.ensure_invisible_material();
    // Reborrow each `SpawnContext` field as a plain `&mut` to its inner
    // value so the existing body — which threads `&mut Assets<…>` /
    // `&mut S3OModelCache` / `&UnitRegistry` to helper functions — works
    // unchanged. Disjoint-field borrow rules let us hold these
    // simultaneously.
    let commands = &mut ctx.commands;
    let meshes = &mut *ctx.meshes;
    let materials = &mut *ctx.materials;
    let images = &mut *ctx.images;
    let model_cache = &mut *ctx.model_cache;
    let unit_registry = &*ctx.unit_registry;
    let invisible_mat = &invisible_mat;
    let model_name = unit_registry.model(kind);
    let material = unit_material(kind, faction, materials, images, model_cache, model_name);
    let radius = unit_radius(kind, model_cache, unit_registry);
    let selection_sphere = meshes.add(Sphere::new(radius).mesh().ico(3).unwrap());

    // Some s3o models are authored with their root at the mesh CENTER
    // rather than at the bottom (octaeder.s3o, used by Byte, has blade
    // vertices spanning y∈[-48,48]). If we plant the root at the
    // heightmap, half the model sinks below ground. Lift the spawn point
    // by however much the lowest vertex extends below piece-tree origin.
    let s3o_model = crate::units::assets::meshes::load_s3o_model(model_name, model_cache);
    let ground_lift = s3o_model.as_ref().map(compute_ground_lift).unwrap_or(0.0);
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
        crate::units::combat::IdleTimer(0.0),
        crate::units::combat::StunCharge(0.0),
        // §1.8 first slice: cache a typed collision volume so
        // projectile / shield / per-shot-miss systems can do
        // volume-aware tests without re-deriving from the S3O on
        // every check. Today every unit spawns a Sphere matching
        // the existing `hit_radius`; future per-unit overrides
        // (Cylinder for tall thin units, AABB for boxes) only need
        // to update this classifier.
        crate::units::combat::CollisionVolume::from_s3o_radius(radius),
    ));

    // Cache the weapon-id binding once so the per-frame combat hot
    // path can read it directly without hashing strings against the
    // weapon registry every tick. Units with no primary weapon (or
    // whose only weapon is BuildLaser, filtered by `unit_registry.weapon`)
    // get no binding — combat skips them via `Option<&WeaponBinding>`.
    let weapon_name = unit_registry.weapon(kind);
    if !weapon_name.is_empty() {
        if let Some(weapon_id) = ctx.weapon_registry.intern(weapon_name) {
            commands
                .entity(unit_entity)
                .insert(crate::units::combat::WeaponBinding(weapon_id));
        }
    }

    if kind.spawns_cloaked() {
        commands
            .entity(unit_entity)
            .insert(crate::units::mechanics::cloak::Cloaked);
    }

    if kind == UnitKind::Port {
        commands
            .entity(unit_entity)
            .insert(crate::units::mechanics::network_buffer::PortTimer::default());
    }

    if kind == UnitKind::Flow {
        commands
            .entity(unit_entity)
            .insert(crate::units::mechanics::network_buffer::SpeedBoost::default());
    }

    if let Some(producer) = default_production(kind) {
        commands.entity(unit_entity).insert(producer);
    }
    if matches!(kind, UnitKind::Kernel | UnitKind::Hole | UnitKind::Carrier) {
        commands.entity(unit_entity).insert(Homebase);
    }
    // Why: visibility is now driven by `update_fog_visibility` from
    // the [`PlayerTeam`] perspective. Friendlies get `Spotted` on the
    // first fog tick (≤100 ms later); enemies stay un-spotted until
    // a friendly observer enters sight. There's a sub-100 ms flash of
    // a fresh enemy spawn before the next fog tick hides it — at the
    // throttle cadence we use, indistinguishable from the spawn fade.
    // Pointer is the only upstream unit whose Deployable cycle is
    // movement-gated ("drive closed, sit open"). Byte *does* have
    // `Open()` / `Close()` COB routines, but upstream's `byte.bos`
    // only calls `Open` from `AimWeapon1` (first aim → unfold →
    // `isOpen=1`) and `Close` on a 3-second idle timeout — byte
    // does NOT pack just because it's moving. I briefly put Byte
    // into the generic Deployable list; that made it close on every
    // move order and never re-open in time to fire.
    //
    // Instead, kick the byte's `Open` script once right after
    // `Create`: the COB physically fans the blades out, the visual
    // reads as "deployed" from spawn, and combat stays ungated so
    // firing happens when combat decides, not when a deploy state
    // machine decides.
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

        // S3O models author their visual front along local +Z (Spring's
        // `frontdir` convention from `SolidObject::ComposeMatrix`), but
        // Bevy's `Transform::look_to` aligns local -Z with the requested
        // forward, so without compensation the body — and its gun — face
        // 180° away. Parent every top-level piece under a `model_root`
        // carrying a constant 180° Y rotation so the visual +Z ends up at
        // world -Z, matching Bevy's forward and the host-side `look_to`
        // contract.
        let model_root = commands
            .spawn((
                Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI)),
                Visibility::default(),
            ))
            .id();
        commands.entity(unit_entity).add_child(model_root);

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
                None => model_root,
            };
            commands.entity(bevy_parent).add_child(piece_entity);
        }

        // Attach the animation rig. The rig is keyed on the unit's
        // static piece table (declaration order in the original script —
        // *not* the s3o depth-first flatten order), so remap piece
        // entities/offsets to table order here. Pieces named in the
        // table that don't exist in the s3o stay as a stub entity at
        // the unit root (zero offset) so animations targeting them are
        // no-ops instead of indexing into the wrong piece.
        {
            let table = crate::units::assets::animation::piece_names(kind);
            let mut table_entities = Vec::with_capacity(table.len());
            let mut table_offsets = Vec::with_capacity(table.len());
            for table_name in table {
                match find_piece_index_by_name(&model.root_piece, table_name) {
                    Some(s3o_idx) => {
                        table_entities.push(piece_entities[s3o_idx]);
                        table_offsets.push(piece_offsets[s3o_idx]);
                    }
                    None => {
                        // Stub entity so animation ops on this slot don't
                        // accidentally hit a real piece.
                        let stub = commands
                            .spawn((Transform::default(), Visibility::default()))
                            .id();
                        commands.entity(unit_entity).add_child(stub);
                        table_entities.push(stub);
                        table_offsets.push([0.0; 3]);
                    }
                }
            }

            // Resolve cached piece components: muzzle names are per-kind
            // (Byte/Flow cycle theirs at fire time); gunbase/body are
            // one-off lookups for the Pointer aim pivot and the
            // Connection hatch respectively.
            let table_index = |name: &str| -> Option<usize> {
                table
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(name))
            };
            let muzzle_idx = crate::units::assets::animation::muzzle_piece_names(kind)
                .and_then(|names| {
                    names.first().and_then(|n| {
                        table.iter().position(|p| p.eq_ignore_ascii_case(n))
                    })
                })
                .or_else(|| {
                    crate::units::assets::animation::MUZZLE_CANDIDATE_NAMES.iter().find_map(|n| {
                        table.iter().position(|p| p.eq_ignore_ascii_case(n))
                    })
                });
            let gunbase_idx = table_index("gunbase");
            let aimer_idx = table_index("aimer");
            let hatch_idx = table_index("body");
            // Aim-before-fire gate is only meaningful for units whose
            // script declares `AimWeapon1`.
            let has_aim = crate::units::assets::animation::has_aim_weapon(kind);

            let piece_count = table.len();
            let piece_rotations = vec![[0.0; 3]; piece_count];
            let target_rotations = vec![[0.0; 3]; piece_count];
            commands.entity(unit_entity).insert(
                crate::units::assets::animation::UnitAnimator {
                    created: false,
                    driver: crate::units::assets::animation::driver_for(kind),
                    rig: crate::units::assets::animation::AnimRig {
                        piece_names: table.iter().map(|s| s.to_string()).collect(),
                        piece_entities: table_entities,
                        piece_base_offsets: table_offsets,
                        piece_rotations,
                        piece_translations: vec![[0.0; 3]; piece_count],
                        target_rotations,
                        turn_speeds: vec![[0.0; 3]; piece_count],
                        target_translations: vec![[0.0; 3]; piece_count],
                        move_speeds: vec![[0.0; 3]; piece_count],
                        spin_speeds: vec![[0.0; 3]; piece_count],
                        muzzle: muzzle_idx.unwrap_or(0),
                        move_gate: 1.0,
                        outbox: Vec::new(),
                    },
                },
            );

            if let Some(idx) = muzzle_idx {
                commands
                    .entity(unit_entity)
                    .insert(crate::units::assets::animation::MuzzlePiece(idx));
            }
            if let Some(idx) = gunbase_idx {
                commands
                    .entity(unit_entity)
                    .insert(crate::units::assets::animation::GunbasePiece(idx));
            }
            if let Some(idx) = aimer_idx {
                commands
                    .entity(unit_entity)
                    .insert(crate::units::assets::animation::AimerPiece(idx));
            }
            if kind == UnitKind::Connection
                && let Some(idx) = hatch_idx
            {
                commands
                    .entity(unit_entity)
                    .insert(crate::units::assets::animation::HatchPiece(idx));
            }
            if has_aim {
                commands
                    .entity(unit_entity)
                    .insert(crate::units::combat::AimScript::default());
            }
        }

        // For factories, cache the piece indices we need for build FX so
        // the production system can read their world transforms each frame
        // without rescanning the model. Indices are into the static piece
        // table (which is what `AnimRig::piece_entities` is keyed on
        // above) — `None` if the model has no such piece, in which case
        // the production system falls back to the factory root.
        if default_production(kind).is_some() {
            let table = crate::units::assets::animation::piece_names(kind);
            let table_index = |name: &str| -> Option<usize> {
                table
                    .iter()
                    .position(|p| p.eq_ignore_ascii_case(name))
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
            let emitters: Vec<usize> = emitter_names.iter().filter_map(|n| table_index(n)).collect();
            commands.entity(unit_entity).insert(FactoryPieces {
                emitters,
                pad: table_index("pad"),
            });
        }
    } else {
        // Fallback: single flattened mesh, no animation.
        let mesh =
            crate::units::assets::meshes::unit_mesh(kind, meshes, model_cache, unit_registry);
        commands
            .entity(unit_entity)
            .insert((Mesh3d(mesh), MeshMaterial3d(material)));
    }

    unit_entity
}

/// Drain the `VirusSpawnQueue` and spawn Virus units at the queued
/// positions. Runs after the death system so kills in a given frame produce
/// Viruses on the next.
pub fn spawn_queued_viruses(
    mut virus_spawns: ResMut<crate::units::combat::VirusSpawnQueue>,
    mut ctx: SpawnContext,
) {
    for spawn in virus_spawns.drain() {
        spawn_unit(
            UnitKind::Virus,
            spawn.faction,
            spawn.team,
            spawn.position,
            &mut ctx,
        );
    }
}

/// Drain the `MineSpawnQueue` and spawn Logic Bombs at the queued
/// positions. Sibling of `spawn_queued_viruses`; runs in the same
/// `Resolve` set so a Byte's `LaunchMines` cast in frame N produces
/// mines visible in frame N+1's Simulate pass. Logic Bombs auto-pick
/// up `Cloaked` via `UnitKind::spawns_cloaked`, so they behave like
/// factory-built mines the moment they appear.
pub fn spawn_queued_mines(
    mut mine_spawns: ResMut<crate::units::mechanics::command_fire::MineSpawnQueue>,
    mut ctx: SpawnContext,
) {
    for spawn in mine_spawns.drain() {
        spawn_unit(
            UnitKind::LogicBomb,
            spawn.faction,
            spawn.team,
            spawn.position,
            &mut ctx,
        );
    }
}

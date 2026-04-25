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

use super::production::default_production;
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::animation::{CobAnimator, CobFileCache, PieceIndex, load_cob_cached};
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

/// Ordered roster the startup spawner lays out for each faction.
struct FactionRoster {
    faction: Faction,
    /// Homebase kind (Kernel / Hole / Carrier). Spawned at the map's
    /// declared start position for this team, not on a datavent.
    homebase: UnitKind,
    /// Small buildings that upstream only allows on a datavent (Socket
    /// / Terminal for System, Window / Obelisk for Hacker, Port /
    /// Firewall for Network). The spawner assigns each one to the
    /// nearest unclaimed datavent, falling back to skipping the kind
    /// once no free vents remain.
    datavent_buildings: &'static [UnitKind],
    /// Mobile units cluster next to the homebase. Shared kinds (Debug,
    /// BadBlock, LogicBomb) are left out — any constructor can build
    /// them on demand.
    units: &'static [UnitKind],
}

/// Gap between successive units placed in the mobile-unit row, on top
/// of each unit's own collision radius. Keeps bodies visibly apart
/// without triggering the spatial-hash push-out on spawn.
const UNIT_ROW_GAP: f32 = 12.0;
/// Max mobile units per row before wrapping to a new row further
/// south. Keeps the cluster visually compact.
const UNITS_PER_ROW: usize = 4;
/// Distance between adjacent rows in the mobile-unit cluster (Byte
/// sits on row 0, Assembler wraps to row 1, etc.).
const UNIT_ROW_SPACING: f32 = 40.0;
/// Gap between the homebase and the first row of mobile units — big
/// enough to clear a Kernel's 64-elmo footprint plus the largest
/// unit's radius.
const CLUSTER_STANDOFF: f32 = 100.0;
/// Keep-out radius around each homebase's centre when assigning
/// datavent-buildings. Maps can place a geovent on or adjacent to a
/// start position (upstream KP's `Valley` does exactly that); without
/// this guard the first Socket/Window/Port gets placed inside the
/// Kernel's 8×8 footprint. 120 elmos = homebase half-diagonal (≈46)
/// plus a small-building half-width (≈16) plus breathing room.
const HOMEBASE_DATAVENT_CLEARANCE: f32 = 120.0;

/// Every faction's starting roster. The homebase is anchored at the
/// map's declared start position; datavent-only buildings are scattered
/// onto unclaimed vents; mobile units cluster just south of the
/// homebase.
const ROSTERS: [FactionRoster; 3] = [
    FactionRoster {
        faction: Faction::System,
        homebase: UnitKind::Kernel,
        datavent_buildings: &[UnitKind::Socket, UnitKind::Terminal],
        units: &[
            UnitKind::Bit,
            UnitKind::Byte,
            UnitKind::Pointer,
            UnitKind::Assembler,
        ],
    },
    FactionRoster {
        faction: Faction::Hacker,
        homebase: UnitKind::Hole,
        datavent_buildings: &[UnitKind::Window, UnitKind::Obelisk],
        units: &[
            UnitKind::Bug,
            UnitKind::Exploit,
            UnitKind::Worm,
            UnitKind::Dos,
            UnitKind::Trojan,
        ],
    },
    FactionRoster {
        faction: Faction::Network,
        homebase: UnitKind::Carrier,
        datavent_buildings: &[UnitKind::Port, UnitKind::Firewall],
        // Signal intentionally omitted — upstream's `signal.fbi` is the
        // SIGTERM air-strike bomber (Side=CPU), spawned by Terminal's
        // ability rather than by the Carrier. Putting it in the Network
        // cluster misrepresented the unit roster.
        units: &[
            UnitKind::Packet,
            UnitKind::Flow,
            UnitKind::Gateway,
            UnitKind::Connection,
        ],
    },
];

/// Spawn every faction's starting roster. See [`FactionRoster`] for the
/// per-faction layout rules.
///
/// `datavent_positions` is the list of geovent world positions on the
/// current map — computed from `parsed.features` in map_loading before
/// the geovent-smoker entities exist. Each datavent is claimed at most
/// once across all factions, in first-come order (closest-to-faction-
/// homebase wins the claim).
#[allow(clippy::too_many_arguments)]
pub fn spawn_homebases(
    heightmap: &Heightmap,
    map_info: &MapInfo,
    datavent_positions: &[Vec3],
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    cob_cache: &mut CobFileCache,
    unit_registry: &UnitRegistry,
) {
    let invisible_mat = get_or_create_invisible_material(commands, materials);

    let (world_w, world_d) = heightmap.world_size();
    let cx = world_w * 0.5;
    let cz = world_d * 0.5;
    let radius = world_w.min(world_d) * 0.30;
    let fallback_positions = [
        (cx + radius, cz),
        (cx - radius * 0.5, cz - radius * 0.866),
        (cx - radius * 0.5, cz + radius * 0.866),
    ];

    // Resolve every faction's homebase position up front so we can
    // block datavents that sit inside any homebase's footprint before
    // the allocation loop.
    let home_positions: [Vec3; 3] = {
        let mut out = [Vec3::ZERO; 3];
        for (i, _) in ROSTERS.iter().enumerate() {
            let (fx, fz) = map_info
                .start_positions
                .get(i)
                .map(|sp| (sp.x, sp.z))
                .unwrap_or(fallback_positions[i]);
            let fx = fx.clamp(CLUSTER_STANDOFF, world_w - CLUSTER_STANDOFF);
            let fz = fz.clamp(CLUSTER_STANDOFF, world_d - CLUSTER_STANDOFF);
            out[i] = heightmap.place(fx, fz);
        }
        out
    };

    // Datavent bookkeeping: a parallel `bool` mask so we can mark a
    // claim without mutating the input slice. Pre-seed the mask by
    // claiming every vent that falls inside a homebase's keep-out
    // radius — otherwise a map whose start position sits on a geovent
    // (Valley, etc.) plants a Socket inside the Kernel's 8×8 footprint.
    let mut claimed = vec![false; datavent_positions.len()];
    let clearance_sq = HOMEBASE_DATAVENT_CLEARANCE * HOMEBASE_DATAVENT_CLEARANCE;
    for (vi, vpos) in datavent_positions.iter().enumerate() {
        if home_positions
            .iter()
            .any(|hp| hp.distance_squared(*vpos) < clearance_sq)
        {
            claimed[vi] = true;
        }
    }
    let mut total_buildings = 0usize;
    let mut total_units = 0usize;
    let mut skipped_for_no_vent = 0usize;

    for (i, roster) in ROSTERS.iter().enumerate() {
        let team = i as u8;
        let home_pos = home_positions[i];

        // 1. Homebase at the exact start position.
        spawn_unit(
            roster.homebase,
            roster.faction,
            team,
            home_pos,
            commands,
            meshes,
            materials,
            images,
            model_cache,
            cob_cache,
            &invisible_mat,
            unit_registry,
        );
        total_buildings += 1;

        // 2. Datavent-only buildings. Each one claims the nearest
        // unclaimed vent to the homebase; if no vents remain we skip
        // the kind (matches "until you run out of spots").
        for &kind in roster.datavent_buildings {
            let Some(vent_idx) = claim_nearest_vent(datavent_positions, &claimed, home_pos) else {
                skipped_for_no_vent += 1;
                continue;
            };
            claimed[vent_idx] = true;
            let vent_pos = datavent_positions[vent_idx];
            spawn_unit(
                kind,
                roster.faction,
                team,
                vent_pos,
                commands,
                meshes,
                materials,
                images,
                model_cache,
                cob_cache,
                &invisible_mat,
                unit_registry,
            );
            total_buildings += 1;
        }

        // 3. Mobile units: grid laid out in the direction pointing from
        // the homebase toward the centroid of all homebases. A fixed
        // "+Z south" cluster drops units off the map whenever the
        // start position sits on a north edge (quadcore's Hacker slot
        // does exactly this); pointing toward the centroid instead
        // keeps the cluster on-map for every map shape without any
        // per-map special-casing. Perpendicular `right` axis spreads
        // the row across the unit radii sum.
        let centroid = (home_positions[0] + home_positions[1] + home_positions[2]) / 3.0;
        let raw_forward = Vec3::new(centroid.x - home_pos.x, 0.0, centroid.z - home_pos.z);
        let forward = if raw_forward.length_squared() > 1e-3 {
            raw_forward.normalize()
        } else {
            Vec3::Z
        };
        let right = Vec3::new(forward.z, 0.0, -forward.x);

        let row_widths = row_total_widths(roster.units, unit_registry);
        let mut row_idx = 0usize;
        let mut col_idx = 0usize;
        let mut cursor = 0.0f32; // running sum along `right`
        let mut row_start = -row_widths[0] * 0.5;

        for (j, &kind) in roster.units.iter().enumerate() {
            let r = unit_registry.collision_radius(kind);
            // Step past half the previous unit plus the gap before
            // planting the centre of this unit.
            let step = if col_idx == 0 {
                r
            } else {
                cursor + UNIT_ROW_GAP + r
            };
            let across = row_start + step;
            let depth = CLUSTER_STANDOFF + row_idx as f32 * UNIT_ROW_SPACING;

            let ux = home_pos.x + forward.x * depth + right.x * across;
            let uz = home_pos.z + forward.z * depth + right.z * across;
            let pos = heightmap.place(ux, uz);
            spawn_unit(
                kind,
                roster.faction,
                team,
                pos,
                commands,
                meshes,
                materials,
                images,
                model_cache,
                cob_cache,
                &invisible_mat,
                unit_registry,
            );
            total_units += 1;

            cursor = step + r;
            col_idx += 1;
            if col_idx >= UNITS_PER_ROW && j + 1 < roster.units.len() {
                row_idx += 1;
                col_idx = 0;
                cursor = 0.0;
                let next_row_width = row_widths.get(row_idx).copied().unwrap_or(row_widths[0]);
                row_start = -next_row_width * 0.5;
            }
        }
    }

    // Only mention skipped datavent-buildings when there actually
    // were any — the old unconditional "(0 datavent-buildings skipped:
    // no vents left)" tail was misleading: it read as if the map was
    // out of vents whether or not anything had been skipped. In the
    // common case (every Socket / Terminal / Window / Port / Obelisk
    // / Firewall found a vent), the tail is simply omitted.
    if skipped_for_no_vent > 0 {
        info!(
            "Spawned starter roster: {} buildings, {} mobile units across {} factions ({} datavent-buildings skipped: no free vents)",
            total_buildings,
            total_units,
            ROSTERS.len(),
            skipped_for_no_vent,
        );
    } else {
        info!(
            "Spawned starter roster: {} buildings, {} mobile units across {} factions",
            total_buildings,
            total_units,
            ROSTERS.len(),
        );
    }
}

/// Find the unclaimed vent closest to `origin`, or `None` if every
/// vent in the slice is already taken.
fn claim_nearest_vent(positions: &[Vec3], claimed: &[bool], origin: Vec3) -> Option<usize> {
    positions
        .iter()
        .enumerate()
        .filter_map(|(i, p)| (!claimed[i]).then_some((i, p.distance_squared(origin))))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
}

/// For each row in the unit-cluster grid, sum up `2*radius + gap` for
/// every unit that lands on that row. Returns the resulting row widths
/// so the caller can centre each row on the homebase X.
fn row_total_widths(units: &[UnitKind], unit_registry: &UnitRegistry) -> Vec<f32> {
    let row_count = (units.len() + UNITS_PER_ROW - 1) / UNITS_PER_ROW;
    let mut widths = vec![0.0; row_count.max(1)];
    for (j, &kind) in units.iter().enumerate() {
        let row = j / UNITS_PER_ROW;
        let col = j % UNITS_PER_ROW;
        let r = unit_registry.collision_radius(kind);
        widths[row] += 2.0 * r;
        if col > 0 {
            widths[row] += UNIT_ROW_GAP;
        }
    }
    widths
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
    let s3o_model = crate::units::assets::meshes::load_s3o_model(model_name, model_cache);
    let ground_lift = s3o_model
        .as_ref()
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
        crate::units::combat::IdleTimer(0.0),
        crate::units::combat::StunCharge(0.0),
    ));

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
    // While there is no real "enemy" team — every faction is AI-driven
    // but human-controllable — spot every unit at spawn so the fog-of-war
    // pass leaves them all visible. Switch this back to a per-team check
    // when a proper player/AI distinction lands.
    commands
        .entity(unit_entity)
        .insert(crate::units::mechanics::cloak::Spotted);
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

            // Resolve cached piece components before moving `cob` into
            // CobAnimator. MuzzlePiece::resolve owns the per-unit piece-
            // name convention for weapon emit points; gunbase/body are
            // one-off lookups for the Pointer aim pivot and the
            // Connection hatch respectively.
            let muzzle_idx = crate::units::assets::animation::MuzzlePiece::resolve(&cob);
            let piece_index = |name: &str| -> Option<usize> {
                cob.piece_names
                    .iter()
                    .position(|n| n.eq_ignore_ascii_case(name))
            };
            let gunbase_idx = piece_index("gunbase");
            let aimer_idx = piece_index("aimer");
            let hatch_idx = piece_index("body");
            // Aim-before-fire gate is only meaningful for units whose
            // `.cob` actually declares `AimWeapon1`.
            let has_aim_weapon = cob.function_id("AimWeapon1").is_some();

            // Pieces start at their authored S3O offsets.
            let piece_rotations = vec![[0.0; 3]; cob_piece_count];
            let target_rotations = vec![[0.0; 3]; cob_piece_count];
            commands.entity(unit_entity).insert(CobAnimator {
                vm,
                cob,
                piece_entities: cob_entities,
                piece_base_offsets: cob_offsets,
                piece_rotations,
                piece_translations: vec![[0.0; 3]; cob_piece_count],
                target_rotations,
                turn_speeds: vec![[0.0; 3]; cob_piece_count],
                target_translations: vec![[0.0; 3]; cob_piece_count],
                move_speeds: vec![[0.0; 3]; cob_piece_count],
                spin_speeds: vec![[0.0; 3]; cob_piece_count],
            });

            if let Some(muzzle) = muzzle_idx {
                commands.entity(unit_entity).insert(muzzle);
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
            if has_aim_weapon {
                commands
                    .entity(unit_entity)
                    .insert(crate::units::combat::AimScript::default());
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
#[allow(clippy::too_many_arguments)]
pub fn spawn_queued_viruses(
    mut virus_spawns: ResMut<crate::units::combat::VirusSpawnQueue>,
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
    mut mine_spawns: ResMut<crate::units::mechanics::command_fire::MineSpawnQueue>,
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

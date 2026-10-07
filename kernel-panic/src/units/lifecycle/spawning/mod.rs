//! Unit spawning orchestration.
//!
//! The core [`spawn_unit`] mounts a unit's s3o model, attaches every
//! gameplay component (combat timers, faction-specific markers, COB
//! animator with resolved muzzle/gunbase/hatch piece indices), and
//! registers faction-specific build-FX pieces for factories. Helpers on
//! top of it — [`spawn_homebases`], [`spawn_queued_viruses`],
//! [`spawn_queued_mines`] — wire in the assets and registry once at the
//! system boundary.
//!
//! Split into:
//! - [`emerge`] — [`Emerging`] / [`FadeMaterials`] lifecycle.
//! - [`s3o_mount`] — the per-model [`PieceLayout`] `spawn_unit` mounts.
//! - [`mod`](self) — [`spawn_unit`] + the top-level spawners.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::units::assets::animation::PieceIndex;
use spring_map::smd_parser::MapInfo;

use super::production::default_production;
use crate::sim::Heading;
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::meshes::{
    S3OModelCache, piece_layout, selection_sphere, unit_material, unit_mid_y, unit_radius,
};
use crate::units::combat::Deployable;
use crate::units::components::{
    Faction, Health, Homebase, SelectionVolume, TeamId, UnitStats, UnitType,
};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

mod emerge;
mod s3o_mount;

pub use emerge::{EmergeStyle, Emerging, FadeMaterials, emerge_system};
pub use s3o_mount::PieceLayout;

/// Bundles the asset / cache / registry resources `spawn_unit` needs.
///
/// Every system that calls `spawn_unit` previously had to declare 7
/// separate `Commands` / `ResMut<Assets<…>>` / `ResMut<…Cache>` params
/// and forward them through; bundling them as one `SystemParam` shrinks
/// each call site to a single argument and lets helper functions reborrow
/// `&mut SpawnContext` without re-listing the same seven types.
#[derive(SystemParam)]
pub struct SpawnContext<'w, 's> {
    pub commands: Commands<'w, 's>,
    pub meshes: ResMut<'w, Assets<Mesh>>,
    pub materials: ResMut<'w, Assets<StandardMaterial>>,
    pub images: ResMut<'w, Assets<Image>>,
    pub model_cache: ResMut<'w, S3OModelCache>,
    pub unit_registry: Res<'w, UnitRegistry>,
    pub weapon_registry: Res<'w, crate::units::content::weapons::WeaponRegistry>,
}

/// Cached piece-index lookup for animated factories. Set once when a
/// producer spawns; lets the production system pull the live world
/// position of script-driven pieces without rescanning the model every
/// frame.
///
/// A factory's `QueryBuildInfo` piece: where the buildee stands
/// (`pad` on Kernel/Hole/Carrier; `None` on Socket/Window, which build
/// at their origin like the engine's `GetPiecePos(-1)`). Build-laser
/// effects are the scripts' own (`emit-sfx 2048` from their pieces).
#[derive(Component, Default)]
pub struct FactoryPieces {
    pub pad: Option<usize>,
}

/// Margin from the map edge when clamping a homebase's start position.
/// Keeps a Kernel/Hole/Carrier footprint clear of the world boundary
/// even when the map's declared start position sits right on it.
const HOMEBASE_EDGE_MARGIN: f32 = 100.0;

/// Spawn one homebase per seat of the match, like upstream
/// `game_spawn.lua`: seat `i` takes the map's `i`-th start position and
/// gets its side's start unit (System → Kernel, Hacker → Hole,
/// Network → Carrier) on the seat's ally team. Seats beyond the map's
/// declared start positions fall back to a ring around the map centre.
/// Returns each seat's faction, team and homebase position.
pub fn spawn_homebases(
    heightmap: &Heightmap,
    map_info: &MapInfo,
    nav: Option<&crate::interaction::movement::NavGridSet>,
    players: &[crate::game_setup::PlayerSpec],
    ctx: &mut SpawnContext,
) -> Vec<(Faction, u8, Vec3)> {
    let (world_w, world_d) = heightmap.world_size();
    let cx = world_w * 0.5;
    let cz = world_d * 0.5;
    let radius = world_w.min(world_d) * 0.30;
    let seats = players.len().max(1) as f32;
    let mut bases = Vec::with_capacity(players.len());

    // Resolve every seat's position first: kernel.bos/hole.bos/carrier.bos
    // `TurnTowardBarycenter` turns each base's front toward the
    // barycenter of all units (the bases, two ticks after Create) in 90°
    // steps, so factories exit toward the enemy — provided that side has
    // room and walkable ground; see `exit_heading`.
    let mut positions = Vec::with_capacity(players.len());
    for i in 0..players.len() {
        let (fx, fz) = map_info
            .start_positions
            .get(i)
            .map(|sp| (sp.x, sp.z))
            .unwrap_or_else(|| {
                let theta = std::f32::consts::TAU * i as f32 / seats;
                (cx + radius * theta.cos(), cz + radius * theta.sin())
            });
        let fx = fx.clamp(HOMEBASE_EDGE_MARGIN, world_w - HOMEBASE_EDGE_MARGIN);
        let fz = fz.clamp(HOMEBASE_EDGE_MARGIN, world_d - HOMEBASE_EDGE_MARGIN);
        positions.push(heightmap.place(fx, fz));
    }
    let barycenter = positions.iter().sum::<Vec3>() / positions.len().max(1) as f32;

    for (seat, &home_pos) in players.iter().zip(&positions) {
        let heading = exit_heading(
            home_pos,
            barycenter,
            heightmap,
            nav,
            &ExitProducts::of(seat.faction, ctx),
        );
        spawn_unit_facing(
            seat.faction.homebase(),
            seat.faction,
            seat.team,
            home_pos,
            heading,
            ctx,
        );
        bases.push((seat.faction, seat.team, home_pos));
    }

    info!("Spawned {} homebases", players.len());
    bases
}

/// What a homebase's exit has to accommodate: the base's own radius and
/// its swarm product's radius and slope cap, for the `SendToEmptySpot`
/// arc (`4R + 4r`) its units will walk to.
pub struct ExitProducts {
    pub factory_radius: f32,
    pub unit_radius: f32,
    pub slope_cap: f32,
}

impl ExitProducts {
    pub fn of(faction: Faction, ctx: &mut SpawnContext) -> Self {
        let product = match faction {
            Faction::System => UnitKind::Bit,
            Faction::Hacker => UnitKind::Bug,
            Faction::Network => UnitKind::Packet,
        };
        let registry = &*ctx.unit_registry;
        let cache = &mut *ctx.model_cache;
        Self {
            factory_radius: unit_radius(faction.homebase(), cache, registry),
            unit_radius: unit_radius(product, cache, registry),
            slope_cap: registry.max_slope_ratio(product),
        }
    }
}

/// The 90°-snapped heading a homebase should face so that its products
/// walk cleanly into the map.
///
/// The script's `TurnTowardBarycenter` picks the quarter turn toward the
/// barycenter of the bases; on a two-sided map that is toward the enemy.
/// But a base in a map corner, or alone (sandbox, showcase, one seat per
/// edge), can end up facing the map boundary or a cliff, and then every
/// product falls back to a random exit spot and jams at the edge. So each
/// of the four headings is scored by how much of the `SendToEmptySpot`
/// exit arc (`4R + 4r` ahead, front half) lies inside the map on ground
/// the product can stand on; a heading whose exit waypoint itself is off
/// the map or unwalkable scores nothing. Headings scoring at least three
/// quarters of the best are candidates. The barycenter's heading is kept
/// while it is a candidate and no other candidate has a quarter more
/// room to the map edge ahead of it (a corner base turns along the
/// longer side); otherwise the candidate with the most room wins, ties
/// going to the one nearest the map centre.
pub fn exit_heading(
    from: Vec3,
    barycenter: Vec3,
    heightmap: &Heightmap,
    nav: Option<&crate::interaction::movement::NavGridSet>,
    products: &ExitProducts,
) -> Heading {
    const QUARTER: i32 = crate::sim::SPRING_CIRCLE_DIVS / 4;
    /// Fraction of the best score a heading must reach to be a candidate.
    const KEEP_FRACTION: f32 = 0.75;
    /// Another candidate needs this much more room ahead to override the
    /// barycenter heading.
    const ROOM_SWITCH: f32 = 1.25;
    let (world_w, world_d) = heightmap.world_size();
    let centre = Vec3::new(world_w * 0.5, 0.0, world_d * 0.5);
    let preferred = barycenter_heading(from, barycenter);
    let toward_centre = barycenter_heading(from, centre);
    let arc = 4.0 * products.factory_radius + 4.0 * products.unit_radius;
    let exit = products.factory_radius + products.unit_radius;

    let walkable = |x: f32, z: f32| {
        x >= 0.0
            && z >= 0.0
            && x < world_w
            && z < world_d
            && nav.is_none_or(|n| n.passable(products.slope_cap, x, z))
    };
    let score = |heading: Heading| -> usize {
        let f = heading.to_vector();
        let (fx, fz) = (f.x, f.y);
        // `rightdir = frontdir × updir`: facing +Z, right is −X.
        let (rx, rz) = (-fz, fx);
        // Units leave through the exit waypoint first: no exit, no use.
        if !walkable(from.x + fx * exit, from.z + fz * exit) {
            return 0;
        }
        let mut n = 1;
        // The half circle `SendToEmptySpot` scans, one sample per step.
        const STEPS: usize = 100;
        for i in 0..STEPS {
            let a = i as f32 * std::f32::consts::PI / (STEPS as f32 * 0.5);
            let (c, s) = (a.cos(), a.sin());
            if c < 0.0 {
                continue;
            }
            let x = from.x + fx * (arc * c) + rx * (arc * s);
            let z = from.z + fz * (arc * c) + rz * (arc * s);
            if walkable(x, z) {
                n += 1;
            }
        }
        n
    };

    // Distance to the map edge straight ahead.
    let room = |heading: Heading| -> f32 {
        let f = heading.to_vector();
        let along_x = if f.x > 0.5 {
            world_w - from.x
        } else if f.x < -0.5 {
            from.x
        } else {
            f32::INFINITY
        };
        let along_z = if f.y > 0.5 {
            world_d - from.z
        } else if f.y < -0.5 {
            from.z
        } else {
            f32::INFINITY
        };
        along_x.min(along_z).max(0.0)
    };

    let scored: Vec<(Heading, usize)> = (0..4)
        .map(|q| Heading((q * QUARTER) as i16))
        .map(|h| (h, score(h)))
        .collect();
    let best = scored.iter().map(|(_, n)| *n).max().unwrap_or(0);
    if best == 0 {
        return preferred;
    }
    let band: Vec<Heading> = scored
        .iter()
        .filter(|(_, n)| *n as f32 >= best as f32 * KEEP_FRACTION)
        .map(|(h, _)| *h)
        .collect();
    let turn_from_centre = |h: Heading| (h.wrapping_sub(toward_centre) as i32).abs();
    let roomiest = band
        .iter()
        .copied()
        .max_by(|a, b| {
            room(*a)
                .total_cmp(&room(*b))
                .then_with(|| turn_from_centre(*b).cmp(&turn_from_centre(*a)))
        })
        .unwrap_or(preferred);
    if band.contains(&preferred) && room(roomiest) < room(preferred) * ROOM_SWITCH {
        preferred
    } else {
        roomiest
    }
}

/// `TurnTowardBarycenter`: the heading from `from` toward `barycenter`
/// snapped to the nearest 90° (`snap90(XZ_ATAN)`), south when the two
/// coincide.
pub fn barycenter_heading(from: Vec3, barycenter: Vec3) -> Heading {
    let (dx, dz) = (barycenter.x - from.x, barycenter.z - from.z);
    if dx * dx + dz * dz < 1.0 {
        return Heading::default();
    }
    let raw = Heading::from_vector(dx, dz).0 as i32;
    let quarter = crate::sim::SPRING_CIRCLE_DIVS / 4;
    let snapped = (raw + quarter / 2).div_euclid(quarter) * quarter;
    Heading(snapped as i16)
}

/// Starting squad per seat of the menu's attract-mode demo: the AI's
/// spam / artillery / heavy picks for the faction, fanned out on the
/// side of the base facing the map centre, so fighting starts within
/// a minute instead of after the first production cycles.
pub fn spawn_demo_squads(
    heightmap: &Heightmap,
    bases: &[(Faction, u8, Vec3)],
    ctx: &mut SpawnContext,
) {
    const SPAM: usize = 8;
    const RING: f32 = 220.0;
    /// Height difference from the base that still counts as "the same
    /// plateau" (Hex Farm's void sits ≥96 elmos below a tower top).
    const LEVEL_TOLERANCE: f32 = 40.0;
    let (w, d) = heightmap.world_size();
    let centre = Vec3::new(w * 0.5, 0.0, d * 0.5);
    for &(faction, team, base) in bases {
        let Some(roster) = crate::units::ai::build_orders::homebase_roster(faction.homebase())
        else {
            continue;
        };
        let squad =
            std::iter::repeat_n(roster.spam, SPAM).chain([roster.arty, roster.arty, roster.heavy]);
        let to_centre = (centre - base).with_y(0.0).normalize_or(Vec3::X);
        let heading = to_centre.z.atan2(to_centre.x);
        let count = SPAM + 3;
        for (i, kind) in squad.enumerate() {
            // Spread over a 120° arc facing the centre, pulled inward
            // until the spot is level with the base: on Hex Farm the
            // smallest towers (r=192) end inside the ring and the void
            // around them kills anything spawned there; on hilly maps it
            // keeps squads off cliffs.
            let a = heading + (i as f32 / (count - 1) as f32 - 0.5) * 2.1;
            let spot = |r: f32| {
                let x =
                    (base.x + r * a.cos()).clamp(HOMEBASE_EDGE_MARGIN, w - HOMEBASE_EDGE_MARGIN);
                let z =
                    (base.z + r * a.sin()).clamp(HOMEBASE_EDGE_MARGIN, d - HOMEBASE_EDGE_MARGIN);
                heightmap.place(x, z)
            };
            let Some(mut pos) = [RING, RING * 0.65, RING * 0.4]
                .into_iter()
                .map(spot)
                .find(|p| (p.y - base.y).abs() < LEVEL_TOLERANCE)
            else {
                continue;
            };
            if ctx.unit_registry.can_fly(kind) {
                pos.y += ctx.unit_registry.cruise_alt(kind);
            }
            spawn_unit(kind, faction, team, pos, ctx);
        }
    }
}

/// Showcase mode: spawn exactly one homebase for `faction` on team 0 at
/// the map's first start position (or the map centre as fallback).
pub fn spawn_showcase_homebase(
    heightmap: &Heightmap,
    map_info: &MapInfo,
    nav: Option<&crate::interaction::movement::NavGridSet>,
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

    let heading = exit_heading(
        home_pos,
        home_pos,
        heightmap,
        nav,
        &ExitProducts::of(faction, ctx),
    );
    spawn_unit_facing(faction.homebase(), faction, 0, home_pos, heading, ctx);
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
///
/// Every GPU asset comes from `S3OModelCache` (shared material, shared
/// piece meshes, shared picking sphere), so a spawn is entity work only.
/// Components known up front go in as few bundles as possible — each
/// separate `insert` on a live entity is an archetype move — and every
/// child spawns with its `ChildOf` in the bundle instead of a follow-up
/// `add_child` command.
pub fn spawn_unit(
    kind: UnitKind,
    faction: Faction,
    team: u8,
    position: Vec3,
    ctx: &mut SpawnContext,
) -> Entity {
    spawn_unit_facing(kind, faction, team, position, Heading::default(), ctx)
}

/// [`spawn_unit`] with an initial heading (`CUnitLoader::LoadUnit`'s
/// `facing`): factory products face their pad, homebases their enemy.
pub fn spawn_unit_facing(
    kind: UnitKind,
    faction: Faction,
    team: u8,
    position: Vec3,
    heading: Heading,
    ctx: &mut SpawnContext,
) -> Entity {
    // Reborrow each `SpawnContext` field as a plain `&mut` to its inner
    // value so the body — which threads `&mut Assets<…>` /
    // `&mut S3OModelCache` / `&UnitRegistry` to helper functions — can
    // hold them simultaneously (disjoint-field borrow rules).
    let commands = &mut ctx.commands;
    let meshes = &mut *ctx.meshes;
    let materials = &mut *ctx.materials;
    let images = &mut *ctx.images;
    let model_cache = &mut *ctx.model_cache;
    let unit_registry = &*ctx.unit_registry;
    let model_name = unit_registry.model(kind);
    let material = unit_material(kind, faction, materials, images, model_cache, model_name);
    let radius = unit_radius(kind, model_cache, unit_registry);
    let mid_y = unit_mid_y(kind, model_cache, unit_registry);
    let selection_sphere = selection_sphere(radius, meshes, model_cache);
    let layout = piece_layout(model_name, meshes, model_cache);

    // Some s3o models are authored with their root at the mesh CENTER
    // rather than at the bottom (octaeder.s3o, used by Byte, has blade
    // vertices spanning y∈[-48,48]). If we plant the root at the
    // heightmap, half the model sinks below ground. Lift the spawn point
    // by however much the lowest vertex extends below piece-tree origin.
    let ground_lift = match kind {
        // network_base.s3o (Network homebase) is authored as a flat pad
        // hanging entirely below its origin (mover spans y −22..+2), so
        // the "bottom rests on the heightmap" lift parked the whole pad
        // 22 elmos in the air. Plant it at origin — Spring parity, where
        // the pad reads as sitting flush on the terrain.
        UnitKind::Carrier => 0.0,
        _ => layout.as_ref().map(|l| l.ground_lift).unwrap_or(0.0),
    };
    let lifted_position = position + Vec3::new(0.0, ground_lift, 0.0);
    if kind.is_homebase() {
        info!(
            "spawn {kind:?}: ground y={:.1}, lift={ground_lift:.1}, root y={:.1}",
            position.y, lifted_position.y
        );
    }

    let transform = Transform::from_translation(lifted_position)
        .with_rotation(crate::interaction::ground_move::attitude(heading, Vec3::Y));
    let unit_entity = commands
        .spawn((
            UnitType(kind),
            faction,
            TeamId(team),
            Health::full(unit_registry.max_health(kind)),
            UnitStats::from_registry(kind, unit_registry, radius, mid_y),
            transform,
            Visibility::default(),
        ))
        .id();

    commands.entity(unit_entity).insert((
        crate::units::combat::IdleTimer(0.0),
        crate::units::combat::StunCharge(0.0),
        crate::interaction::movement::ground_mover_components(
            kind,
            unit_registry,
            unit_entity,
            &transform,
        ),
        crate::interaction::movement::GroundLift(ground_lift),
        // §1.8 first slice: cache a typed collision volume so
        // projectile / shield / per-shot-miss systems can do
        // volume-aware tests without re-deriving from the S3O on
        // every check. Today every unit spawns a Sphere matching
        // the existing `hit_radius`; future per-unit overrides
        // (Cylinder for tall thin units, AABB for boxes) only need
        // to update this classifier.
        crate::units::combat::CollisionVolume::from_s3o(radius, mid_y),
    ));

    // Cache the weapon-id binding once so the per-frame combat hot
    // path can read it directly without hashing strings against the
    // weapon registry every tick. Units with no primary weapon (or
    // whose only weapon is BuildLaser, filtered by `unit_registry.weapon`)
    // get no binding — combat skips them via `Option<&WeaponBinding>`.
    let weapon_name = unit_registry.weapon(kind);
    if !weapon_name.is_empty()
        && let Some(weapon_id) = ctx.weapon_registry.intern(weapon_name)
    {
        commands
            .entity(unit_entity)
            .insert(crate::units::combat::WeaponBinding(weapon_id));
    }

    // Worm bites detonate Weapon2 (worm.bos `emit-sfx 4097`); cache its
    // id like the primary binding so the fire path never hashes names.
    if kind.has_autohold()
        && let Some(splash) = ctx.weapon_registry.intern(unit_registry.weapon2(kind))
    {
        commands
            .entity(unit_entity)
            .insert(crate::units::mechanics::worm::WormSplash(splash));
    }

    // Logic Bombs spawn cloaked per their FBI; the Worm starts its
    // surface-to-bite cycle submerged (worm.bos Create), so it does too.
    if kind == UnitKind::Worm || unit_registry.init_cloaked(kind) {
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

    // Terminal / Firewall start recharging at creation (upstream
    // `UnitCreated` in airstrike.lua / network_reflectorshield.lua).
    if let Some(cooldown) = crate::units::mechanics::command_fire::initial_cooldown(kind) {
        commands.entity(unit_entity).insert(cooldown);
    }

    if let Some(producer) = default_production(kind) {
        commands.entity(unit_entity).insert(producer);
    }
    if kind.is_homebase() {
        commands.entity(unit_entity).insert(Homebase);
    }
    // Why: visibility is now driven by `update_fog_visibility` from
    // the [`crate::units::player::LocalTeam`] perspective. Friendlies get `Spotted` on the
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

    // Selection volume child: a mesh with no material. It is never
    // drawn — Bevy only queues an entity for a render phase through its
    // material — but `Mesh3d` still gets it an `Aabb`, a
    // `VisibilityClass` and a `ViewVisibility`, which is all
    // `MeshRayCast` needs to pick it (hover / click in
    // `selection::core`). `Visibility` stays inherited so a cloaked unit
    // (root `Visibility::Hidden`) is unpickable along with its model.
    commands.spawn((
        SelectionVolume,
        Mesh3d(selection_sphere),
        Transform::from_xyz(0.0, radius * 0.5, 0.0),
        ChildOf(unit_entity),
    ));

    if let Some(layout) = &layout {
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
                ChildOf(unit_entity),
            ))
            .id();

        // Spawn piece entities in layout (depth-first) order, so every
        // parent precedes its children.
        let mut piece_entities = Vec::with_capacity(layout.pieces.len());
        for spec in &layout.pieces {
            let parent = spec.parent.map_or(model_root, |pi| piece_entities[pi]);
            let transform = Transform::from_xyz(spec.offset[0], spec.offset[1], spec.offset[2]);
            let piece_entity = match &spec.mesh {
                Some(mesh) => commands
                    .spawn((
                        PieceIndex,
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material.clone()),
                        transform,
                        Visibility::default(),
                        spec.emit,
                        ChildOf(parent),
                    ))
                    .id(),
                None => commands
                    .spawn((
                        PieceIndex,
                        transform,
                        Visibility::default(),
                        spec.emit,
                        ChildOf(parent),
                    ))
                    .id(),
            };
            piece_entities.push(piece_entity);
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
                match layout.index_by_name(table_name) {
                    Some(s3o_idx) => {
                        table_entities.push(piece_entities[s3o_idx]);
                        table_offsets.push(layout.pieces[s3o_idx].offset);
                    }
                    None => {
                        // Stub entity so animation ops on this slot don't
                        // accidentally hit a real piece.
                        let stub = commands
                            .spawn((
                                Transform::default(),
                                Visibility::default(),
                                ChildOf(unit_entity),
                            ))
                            .id();
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
                table.iter().position(|n| n.eq_ignore_ascii_case(name))
            };
            let muzzle_idx = crate::units::assets::animation::muzzle_piece_names(kind)
                .and_then(|names| {
                    names
                        .first()
                        .and_then(|n| table.iter().position(|p| p.eq_ignore_ascii_case(n)))
                })
                .or_else(|| {
                    crate::units::assets::animation::MUZZLE_CANDIDATE_NAMES
                        .iter()
                        .find_map(|n| table.iter().position(|p| p.eq_ignore_ascii_case(n)))
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
            let rig = crate::units::assets::animation::AnimRig {
                piece_names: table,
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
                // First apply must push the (possibly driver-snapped)
                // poses even though nothing has ticked yet.
                dirty: true,
            };
            let mut driver = crate::units::assets::animation::driver_for(kind);
            // Bind piece indices once, before any entry point can run —
            // even a unit killed on its spawn frame then addresses pieces
            // through bound indices.
            driver.bind(&rig);
            commands
                .entity(unit_entity)
                .insert(crate::units::assets::animation::UnitAnimator {
                    created: false,
                    driver,
                    rig,
                });

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
            if kind == UnitKind::Byte {
                commands
                    .entity(unit_entity)
                    .insert(crate::units::combat::Byte);
            }
        }

        if default_production(kind).is_some() {
            let table = crate::units::assets::animation::piece_names(kind);
            let table_index = |name: &str| -> Option<usize> {
                table.iter().position(|p| p.eq_ignore_ascii_case(name))
            };
            commands.entity(unit_entity).insert(FactoryPieces {
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
/// up `Cloaked` from their FBI `Init_Cloaked`, so they behave like
/// factory-built mines the moment they appear.
///
/// Each mine is dropped when its team already fields the Logic Bomb
/// `UnitRestricted` cap — upstream `Launcher.lua` only calls
/// `CreateUnit` while `#GetTeamUnitsByDefs(team, logic_bomb) <
/// maxThisUnit`.
pub fn spawn_queued_mines(
    mut mine_spawns: ResMut<crate::units::mechanics::command_fire::MineSpawnQueue>,
    live_units: Query<(&UnitType, &TeamId), Without<crate::units::combat::Dying>>,
    mut ctx: SpawnContext,
) {
    let limit = ctx
        .unit_registry
        .team_limit(UnitKind::LogicBomb)
        .unwrap_or(u32::MAX);
    let mut counts =
        crate::units::lifecycle::bookkeeping::team_kind_counts(UnitKind::LogicBomb, &live_units);
    for spawn in mine_spawns.drain() {
        let count = counts.entry(spawn.team).or_default();
        if *count >= limit {
            continue;
        }
        *count += 1;
        spawn_unit(
            UnitKind::LogicBomb,
            spawn.faction,
            spawn.team,
            spawn.position,
            &mut ctx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::movement::{NavBucket, NavGridSet};

    const W: usize = 129;
    const D: usize = 97;
    const PRODUCTS: ExitProducts = ExitProducts {
        factory_radius: 64.0,
        unit_radius: 16.0,
        slope_cap: 0.412_215,
    };

    /// 1024 × 768 elmos, flat.
    fn flat() -> Heightmap {
        Heightmap::from_raw(vec![100.0; W * D], W, D)
    }

    /// A base at the west edge facing the enemy to the east keeps that
    /// heading; alone in the north-west corner, where the script's
    /// south-facing default would walk its products along the edge, it
    /// turns east into the map instead.
    #[test]
    fn exit_heading_turns_away_from_the_map_edge() {
        let hm = flat();
        let centre = Vec3::new(512.0, 100.0, 384.0);
        let west = Vec3::new(120.0, 100.0, 384.0);
        assert_eq!(
            exit_heading(west, centre, &hm, None, &PRODUCTS),
            Heading(16384)
        );
        let corner = Vec3::new(120.0, 100.0, 120.0);
        assert_eq!(barycenter_heading(corner, corner), Heading(0));
        assert_eq!(
            exit_heading(corner, corner, &hm, None, &PRODUCTS),
            Heading(16384)
        );
    }

    /// A cliff across the exit makes the base face a side where its
    /// units can actually walk out.
    #[test]
    fn exit_heading_avoids_an_unwalkable_exit() {
        let mut heights = vec![100.0; W * D];
        // Three raised vertex rows at z = 192..208 elmos: the squares on
        // either side are far too steep for a Bit.
        for row in [24, 26] {
            for x in 0..W {
                heights[row * W + x] = 600.0;
            }
        }
        let hm = Heightmap::from_raw(heights.clone(), W, D);
        let slope_mod = spring_pathfinding::slope_mod_from_max_slope(PRODUCTS.slope_cap);
        let speed_map = spring_pathfinding::SpeedMap::from_heightmap(
            &heights,
            W as u32,
            D as u32,
            PRODUCTS.slope_cap,
            slope_mod,
        );
        let mut nav = NavGridSet::default();
        nav.buckets.push(NavBucket {
            max_slope: PRODUCTS.slope_cap,
            speed_map,
        });
        let base = Vec3::new(512.0, 100.0, 120.0);
        let centre = Vec3::new(512.0, 100.0, 384.0);
        assert_eq!(barycenter_heading(base, centre), Heading(0));
        assert!(!nav.passable(PRODUCTS.slope_cap, 512.0, 200.0));
        // Flat ground: the barycenter heading stands. With the cliff
        // across it, the base turns to one of the open sides instead.
        assert_eq!(
            exit_heading(base, centre, &flat(), None, &PRODUCTS),
            Heading(0)
        );
        let turned = exit_heading(base, centre, &hm, Some(&nav), &PRODUCTS);
        assert!(
            matches!(turned, Heading(16384) | Heading(-16384)),
            "{turned:?}"
        );
    }

    /// `TurnTowardBarycenter`: the heading toward the barycenter snapped
    /// to 90° steps (0 = +Z, 16384 = +X), wrapping cleanly at the back.
    #[test]
    fn barycenter_heading_snaps_to_quarter_turns() {
        let from = Vec3::new(500.0, 0.0, 500.0);
        let toward = |dx: f32, dz: f32| barycenter_heading(from, from + Vec3::new(dx, 0.0, dz));
        assert_eq!(toward(0.0, 100.0), Heading(0));
        assert_eq!(toward(100.0, 0.0), Heading(16384));
        assert_eq!(toward(-100.0, 20.0), Heading(-16384));
        assert_eq!(toward(5.0, -100.0), Heading(i16::MIN));
        // A base sitting on the barycenter faces south.
        assert_eq!(barycenter_heading(from, from), Heading(0));
        // Slightly past 45° rounds to the nearer axis.
        assert_eq!(toward(100.0, 120.0), Heading(0));
        assert_eq!(toward(120.0, 100.0), Heading(16384));
    }
}

//! Building placement for mobile constructors — Spring's
//! `CMDTYPE_ICON_BUILDING` flow from `CGuiHandler`.
//!
//! A click on a build button (or its hotkey) *arms* the build command
//! ([`PlacementMode::kind`]); nothing is placed yet. While armed:
//!
//! 1. A translucent ghost of the building follows the cursor — green on
//!    a valid site, red otherwise. Kernel Panic's small buildings (the
//!    minifacs and specials, `kpunittypes.lua` `SmallBuilding`, whose
//!    yardmaps need a geothermal vent) snap to the nearest free datavent;
//!    Bad Blocks, Logic Bombs and Debuggers sit on Spring's 16-elmo build
//!    grid anywhere the slope allows (`CGameHelper::Pos2BuildPos`).
//! 2. The *second* click places it: the left press anchors the order and
//!    the release commits it (`MouseRelease` → `GetCommand`). A plain
//!    click places one building and disarms (`FinishCommand`); with
//!    Shift the order is queued and the command stays armed for the next
//!    placement. Shift+drag lays a row of buildings from the press point
//!    to the release point (`GetBuildPositions`: Ctrl keeps the row
//!    axis-aligned, Alt fills the rectangle, Alt+Ctrl its outline).
//! 3. Once Shift is let go, the next click only disarms (`needShift`).
//!
//! Right-click / Escape cancel through the command panel's input (as
//! `CGuiHandler::MousePress` does). The press and release of a placing
//! click are consumed so they never reach click-to-select — placing a
//! building keeps the constructor selected.

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use crate::interaction::movement::{CommandQueue, MoveTarget, QueuedCommand};
use crate::interaction::selection::{Selected, apply_ordered_command, ground_hit_filtered};
use crate::map_loading::TerrainChunkMarker;
use crate::rendering::camera::RtsCamera;
use crate::sim::SQUARE_SIZE;
use crate::terrain::geovent::{GeoventSmoker, VentClaim};
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::meshes::{S3OModelCache, unit_material, unit_mesh};
use crate::units::combat::Dying;
use crate::units::components::{Faction, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::construction::buildings_for;

pub(super) struct PlacementPlugin;

impl Plugin for PlacementPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlacementMode>()
            .init_resource::<PlacementState>()
            .add_systems(
                Update,
                (
                    disarm_without_builders,
                    plan_sites,
                    sync_ghosts,
                    place_on_release,
                )
                    .chain()
                    // Before selection so a placing click never reaches
                    // click-to-select; after the game-world rebuild so
                    // stale ghosts are observed post-teardown.
                    .before(crate::interaction::selection::SelectionSet::Select)
                    .after(crate::map_loading::GameWorldRebuild),
            );
    }
}

/// The armed build command: the next map click places `kind` for every
/// selected constructor that can build it. Cleared on a plain placement,
/// right-click / Escape, or when no such constructor stays selected.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub(crate) struct PlacementMode {
    pub kind: Option<UnitKind>,
}

/// Max XZ distance from the cursor to a datavent for the ghost to snap.
const SNAP_RADIUS: f32 = 64.0;
/// Spring's `BUILD_SQUARE_SIZE` (the 16-elmo build grid).
const BUILD_SQUARE: f32 = 16.0;

const GHOST_VALID_COLOR: Color = Color::srgba(0.30, 1.00, 0.40, 0.55);
const GHOST_INVALID_COLOR: Color = Color::srgba(1.00, 0.30, 0.30, 0.55);

#[derive(Component)]
struct GhostMarker;

/// One planned building position of the current preview.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlannedSite {
    pos: Vec3,
    valid: bool,
}

#[derive(Resource, Default)]
struct PlacementState {
    /// Ghost entities, one per planned site (pooled).
    ghosts: Vec<Entity>,
    ghost_kind: Option<UnitKind>,
    /// The sites the release would place right now.
    sites: Vec<PlannedSite>,
    /// Ground point under the placing press (the row's anchor).
    anchor: Option<Vec3>,
    /// The current left press belongs to placement (consume its release).
    press_owned: bool,
    /// Spring `needShift`: placed with Shift, so the next click without
    /// Shift disarms instead of placing.
    need_shift: bool,
}

/// Modifier state relevant to placement.
#[derive(Clone, Copy, Debug, Default)]
struct Mods {
    shift: bool,
    ctrl: bool,
    alt: bool,
}

fn mods(keys: &ButtonInput<KeyCode>) -> Mods {
    Mods {
        shift: keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]),
        ctrl: keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight]),
        alt: keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]),
    }
}

/// Spring footprint in heightmap squares (`UnitDef::xsize` =
/// `FootprintX × 2`).
fn footprint_squares(footprint_elmos: Vec2) -> IVec2 {
    (footprint_elmos / SQUARE_SIZE)
        .round()
        .as_ivec2()
        .max(IVec2::ONE)
}

/// `CGameHelper::Pos2BuildPos`: snap to the 16-elmo build grid — to a
/// grid line for footprints of a multiple of four squares, between grid
/// lines otherwise.
fn build_pos(p: Vec2, size: IVec2) -> Vec2 {
    let snap = |v: f32, s: i32| {
        if s & 2 != 0 {
            (v / BUILD_SQUARE).floor() * BUILD_SQUARE + SQUARE_SIZE
        } else {
            ((v + SQUARE_SIZE) / BUILD_SQUARE).floor() * BUILD_SQUARE
        }
    };
    Vec2::new(snap(p.x, size.x), snap(p.y, size.y))
}

/// `FillRowOfBuildPos`: `n` grid-snapped positions stepping from `p`.
fn fill_row(out: &mut Vec<Vec2>, p: Vec2, step: Vec2, n: i32, size: IVec2) {
    for i in 0..n.max(0) {
        out.push(build_pos(p + step * i as f32, size));
    }
}

/// `CGuiHandler::GetBuildPositions` (line / rectangle branch) between
/// two ground points for a footprint of `size` squares.
fn row_positions(start: Vec2, end: Vec2, size: IVec2, m: Mods) -> Vec<Vec2> {
    let start = build_pos(start, size);
    let end = build_pos(end, size);
    let delta = end - start;
    let xsize = SQUARE_SIZE * size.x as f32;
    let zsize = SQUARE_SIZE * size.y as f32;
    let xnum = ((delta.x.abs() + xsize * 1.4) / xsize) as i32;
    let znum = ((delta.y.abs() + zsize * 1.4) / zsize) as i32;
    let mut xstep = if delta.x > 0.0 { xsize } else { -xsize };
    let mut zstep = if delta.y > 0.0 { zsize } else { -zsize };
    let mut out = Vec::new();
    if m.alt {
        if m.ctrl {
            // Hollow rectangle.
            if xnum > 1 && znum > 1 {
                let (x0, z0) = (start.x, start.y);
                fill_row(
                    &mut out,
                    Vec2::new(x0, z0 + zstep),
                    Vec2::new(0.0, zstep),
                    znum - 1,
                    size,
                );
                fill_row(
                    &mut out,
                    Vec2::new(x0 + xstep, z0 + (znum - 1) as f32 * zstep),
                    Vec2::new(xstep, 0.0),
                    xnum - 1,
                    size,
                );
                fill_row(
                    &mut out,
                    Vec2::new(
                        x0 + (xnum - 1) as f32 * xstep,
                        z0 + (znum - 2) as f32 * zstep,
                    ),
                    Vec2::new(0.0, -zstep),
                    znum - 1,
                    size,
                );
                fill_row(
                    &mut out,
                    Vec2::new(x0 + (xnum - 2) as f32 * xstep, z0),
                    Vec2::new(-xstep, 0.0),
                    xnum - 1,
                    size,
                );
            } else if xnum == 1 {
                fill_row(&mut out, start, Vec2::new(0.0, zstep), znum, size);
            } else {
                fill_row(&mut out, start, Vec2::new(xstep, 0.0), xnum, size);
            }
        } else {
            // Filled rectangle, boustrophedon.
            for zn in 0..znum {
                let z = start.y + zn as f32 * zstep;
                if zn & 1 == 1 {
                    fill_row(
                        &mut out,
                        Vec2::new(start.x + (xnum - 1) as f32 * xstep, z),
                        Vec2::new(-xstep, 0.0),
                        xnum,
                        size,
                    );
                } else {
                    fill_row(
                        &mut out,
                        Vec2::new(start.x, z),
                        Vec2::new(xstep, 0.0),
                        xnum,
                        size,
                    );
                }
            }
        }
    } else {
        // A line; Ctrl keeps it axis-aligned.
        let x_dominates = delta.x.abs() > delta.y.abs();
        if x_dominates {
            zstep = if m.ctrl {
                0.0
            } else {
                xstep * delta.y / if delta.x != 0.0 { delta.x } else { 1.0 }
            };
        } else {
            xstep = if m.ctrl {
                0.0
            } else {
                zstep * delta.x / if delta.y != 0.0 { delta.y } else { 1.0 }
            };
        }
        fill_row(
            &mut out,
            start,
            Vec2::new(xstep, zstep),
            if x_dominates { xnum } else { znum },
            size,
        );
    }
    out
}

/// Existing structures a new building must not overlap (centre, half
/// extents in XZ).
type Obstacles = Vec<(Vec2, Vec2)>;

/// Validate / snap one candidate position for `kind`.
fn resolve_site(
    kind: UnitKind,
    raw: Vec2,
    vents: &[Vec3],
    taken_vents: &mut Vec<Vec3>,
    obstacles: &Obstacles,
    heightmap: Option<&Heightmap>,
    registry: &UnitRegistry,
) -> PlannedSite {
    let footprint = registry.footprint_elmos(kind);
    let ground = |p: Vec2| heightmap.map_or(Vec3::new(p.x, 0.0, p.y), |h| h.place(p.x, p.y));
    let slope_ok = |pos: Vec3| {
        heightmap.is_none_or(|h| {
            h.max_slope_in_footprint(pos, footprint) <= registry.max_slope_ratio(kind)
        })
    };
    if kind.is_small_building() {
        // Nearest free vent in XZ within the snap radius.
        let best = vents
            .iter()
            .filter(|v| !taken_vents.iter().any(|t| t.distance_squared(**v) < 1.0))
            .map(|v| (*v, v.xz().distance(raw)))
            .filter(|(_, d)| *d <= SNAP_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        return match best {
            Some((vent, _)) => {
                taken_vents.push(vent);
                PlannedSite {
                    pos: vent,
                    valid: slope_ok(vent),
                }
            }
            None => PlannedSite {
                pos: ground(raw),
                valid: false,
            },
        };
    }
    let pos2 = build_pos(raw, footprint_squares(footprint));
    let pos = ground(pos2);
    let half = footprint * 0.5;
    let blocked = obstacles.iter().any(|(c, h)| {
        (c.x - pos2.x).abs() < h.x + half.x - 0.01 && (c.y - pos2.y).abs() < h.y + half.y - 0.01
    });
    PlannedSite {
        pos,
        valid: !blocked && slope_ok(pos),
    }
}

/// Plan the sites a release at `end` would place (a row from `anchor`
/// with Shift held, a single site otherwise).
#[allow(clippy::too_many_arguments)]
fn plan(
    kind: UnitKind,
    anchor: Option<Vec3>,
    end: Vec3,
    m: Mods,
    vents: &[Vec3],
    obstacles: &Obstacles,
    heightmap: Option<&Heightmap>,
    registry: &UnitRegistry,
) -> Vec<PlannedSite> {
    let size = footprint_squares(registry.footprint_elmos(kind));
    // Vent buildings sample the row the same way; every sample then
    // snaps to a vent of its own in `resolve_site`.
    let raws: Vec<Vec2> = match anchor {
        Some(a) if m.shift => row_positions(a.xz(), end.xz(), size, m),
        _ => vec![end.xz()],
    };
    let single = raws.len() == 1;
    let mut taken = Vec::new();
    let mut out: Vec<PlannedSite> = Vec::new();
    for raw in raws {
        let site = resolve_site(kind, raw, vents, &mut taken, obstacles, heightmap, registry);
        // A row of vent buildings only keeps the samples that found a
        // vent of their own.
        if !single && kind.is_small_building() && !site.valid {
            continue;
        }
        if !out
            .iter()
            .any(|o| o.pos.xz().distance_squared(site.pos.xz()) < 1.0)
        {
            out.push(site);
        }
    }
    out
}

#[allow(clippy::type_complexity)]
fn disarm_without_builders(
    mut placement: ResMut<PlacementMode>,
    builders: Query<&UnitType, With<Selected>>,
) {
    if let Some(kind) = placement.kind
        && !builders.iter().any(|u| buildings_for(u.0).contains(&kind))
    {
        placement.kind = None;
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn plan_sites(
    placement: Res<PlacementMode>,
    mut state: ResMut<PlacementState>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    terrain: Query<(), With<TerrainChunkMarker>>,
    vents: Query<&GeoventSmoker, Without<VentClaim>>,
    structures: Query<(&UnitType, &Transform), Without<Dying>>,
    heightmap: Option<Res<Heightmap>>,
    registry: Res<UnitRegistry>,
) {
    let Some(kind) = placement.kind else {
        state.sites.clear();
        state.anchor = None;
        state.press_owned = false;
        state.need_shift = false;
        return;
    };
    // Terrain-only cast: the ghosts hover on the cursor ray and an
    // unfiltered cast would hit them and freeze the preview.
    let terrain_only = |e: Entity| terrain.contains(e);
    let Some(cursor) = ground_hit_filtered(&windows, &camera_q, &mut ray_cast, &terrain_only)
    else {
        state.sites.clear();
        return;
    };
    let vent_pos: Vec<Vec3> = vents.iter().map(|v| v.pos).collect();
    let obstacles: Obstacles = structures
        .iter()
        .filter(|(u, _)| registry.is_building(u.0) || u.0.is_building())
        .map(|(u, tf)| (tf.translation.xz(), registry.footprint_elmos(u.0) * 0.5))
        .collect();
    let anchor = if state.press_owned && mouse.pressed(MouseButton::Left) {
        state.anchor
    } else {
        None
    };
    state.sites = plan(
        kind,
        anchor,
        cursor,
        mods(&keys),
        &vent_pos,
        &obstacles,
        heightmap.as_deref(),
        &registry,
    );
}

#[allow(clippy::too_many_arguments)]
fn sync_ghosts(
    mut commands: Commands,
    placement: Res<PlacementMode>,
    mut state: ResMut<PlacementState>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    registry: Res<UnitRegistry>,
    selected_q: Query<&Faction, With<Selected>>,
    mut ghosts: Query<
        (
            &mut Transform,
            &mut Visibility,
            &MeshMaterial3d<StandardMaterial>,
        ),
        With<GhostMarker>,
    >,
) {
    // Drop ghosts that went with a game-world teardown, and every ghost
    // when the kind changed or placement disarmed.
    state.ghosts.retain(|e| ghosts.contains(*e));
    if placement.kind != state.ghost_kind {
        for e in state.ghosts.drain(..) {
            commands.entity(e).try_despawn();
        }
        state.ghost_kind = placement.kind;
    }
    let Some(kind) = placement.kind else {
        return;
    };
    while state.ghosts.len() < state.sites.len() {
        let faction = selected_q.iter().next().copied().unwrap_or(Faction::System);
        let mesh = unit_mesh(kind, &mut meshes, &mut model_cache, &registry);
        let model_name = registry.model(kind).to_string();
        let base = unit_material(
            kind,
            faction,
            &mut materials,
            &mut images,
            &mut model_cache,
            &model_name,
        );
        let texture = materials
            .get(&base)
            .and_then(|m| m.base_color_texture.clone());
        let mat = materials.add(StandardMaterial {
            base_color: GHOST_VALID_COLOR,
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            base_color_texture: texture,
            ..default()
        });
        let e = commands
            .spawn((
                GhostMarker,
                Mesh3d(mesh),
                MeshMaterial3d(mat),
                Transform::default(),
                Visibility::Hidden,
            ))
            .id();
        state.ghosts.push(e);
    }
    let sites = state.sites.clone();
    for (i, e) in state.ghosts.iter().enumerate() {
        let Ok((mut tf, mut vis, mat)) = ghosts.get_mut(*e) else {
            continue;
        };
        match sites.get(i) {
            Some(site) => {
                // Lift slightly so the mesh doesn't z-fight the ground.
                tf.translation = site.pos + Vec3::Y * 0.5;
                *vis = Visibility::Inherited;
                let want = if site.valid {
                    GHOST_VALID_COLOR
                } else {
                    GHOST_INVALID_COLOR
                };
                // `get_mut` marks the material modified (a GPU
                // re-upload): only take it on an actual change.
                if materials.get(&mat.0).is_some_and(|m| m.base_color != want)
                    && let Some(m) = materials.get_mut(&mat.0)
                {
                    m.base_color = want;
                }
            }
            None => *vis = Visibility::Hidden,
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn place_on_release(
    mut commands: Commands,
    mut placement: ResMut<PlacementMode>,
    mut state: ResMut<PlacementState>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    builders: Query<(Entity, &UnitType), With<Selected>>,
    move_target_q: Query<(), With<MoveTarget>>,
    vents: Query<(Entity, &GeoventSmoker), Without<VentClaim>>,
) {
    let Some(kind) = placement.kind else {
        return;
    };
    if mouse.just_pressed(MouseButton::Left) {
        mouse.clear_just_pressed(MouseButton::Left);
        state.press_owned = true;
        state.anchor = state.sites.first().map(|s| s.pos);
        return;
    }
    if !(state.press_owned && mouse.just_released(MouseButton::Left)) {
        return;
    }
    mouse.clear_just_released(MouseButton::Left);
    state.press_owned = false;
    state.anchor = None;

    let shift = mods(&keys).shift;
    if state.need_shift && !shift {
        // Placed with Shift before and Shift is up now: this click only
        // ends the command (Spring's `needShift`).
        placement.kind = None;
        state.need_shift = false;
        return;
    }
    let sites = std::mem::take(&mut state.sites);
    if sites.len() == 1 && !sites[0].valid {
        // CMD_FAILED: nothing placed, the command stays armed.
        return;
    }
    let valid: Vec<Vec3> = sites.iter().filter(|s| s.valid).map(|s| s.pos).collect();
    if valid.is_empty() {
        return;
    }

    for (entity, ut) in &builders {
        if !buildings_for(ut.0).contains(&kind) {
            continue;
        }
        let orders = valid
            .iter()
            .map(|&site| QueuedCommand::BuildAt { kind, site });
        if shift && move_target_q.contains(entity) {
            for cmd in orders {
                apply_ordered_command(entity, cmd, true, &move_target_q, &mut commands);
            }
        } else {
            let mut orders = orders;
            if let Some(first) = orders.next() {
                apply_ordered_command(entity, first, false, &move_target_q, &mut commands);
            }
            let rest: Vec<QueuedCommand> = orders.collect();
            if !rest.is_empty() {
                commands
                    .entity(entity)
                    .insert(CommandQueue { commands: rest });
            }
        }
    }
    // Claim the vents so no second constructor can stack on them.
    for (vent_entity, vent) in &vents {
        if valid.iter().any(|s| vent.pos.distance_squared(*s) < 1.0) {
            commands.entity(vent_entity).insert(VentClaim);
        }
    }
    // FinishCommand.
    if shift {
        state.need_shift = true;
    } else {
        placement.kind = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::lifecycle::construction::PendingBuild;
    use bevy::ecs::system::RunSystemOnce;

    fn world() -> World {
        let mut world = World::new();
        world.init_resource::<PlacementMode>();
        world.init_resource::<PlacementState>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();
        world
    }

    fn click(world: &mut World) {
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        world.run_system_once(place_on_release).unwrap();
        let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
        mouse.clear();
        mouse.release(MouseButton::Left);
        world.run_system_once(place_on_release).unwrap();
        world.resource_mut::<ButtonInput<MouseButton>>().clear();
    }

    fn set_site(world: &mut World, pos: Vec3, valid: bool) {
        world.resource_mut::<PlacementState>().sites = vec![PlannedSite { pos, valid }];
    }

    /// Spring's grid snap: 4×4-footprint (8 squares) buildings on the
    /// 16-elmo lines, 1×1 (2 squares) ones between them.
    #[test]
    fn build_pos_snaps_like_pos2buildpos() {
        assert_eq!(
            build_pos(Vec2::new(100.0, 7.0), IVec2::splat(8)),
            Vec2::new(96.0, 0.0)
        );
        assert_eq!(
            build_pos(Vec2::new(100.0, 7.0), IVec2::splat(2)),
            Vec2::new(104.0, 8.0)
        );
    }

    /// A drag along X lays footprint-spaced buildings from the anchor;
    /// Ctrl flattens a diagonal drag onto the dominant axis.
    #[test]
    fn row_positions_follow_getbuildpositions() {
        let size = IVec2::splat(4); // Bad Block: 32 elmos
        let row = row_positions(
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 0.0),
            size,
            Mods::default(),
        );
        assert_eq!(
            row,
            vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(32.0, 0.0),
                Vec2::new(64.0, 0.0),
                Vec2::new(96.0, 0.0)
            ]
        );
        let straight = row_positions(
            Vec2::new(0.0, 0.0),
            Vec2::new(100.0, 40.0),
            size,
            Mods {
                ctrl: true,
                ..default()
            },
        );
        assert!(straight.iter().all(|p| p.y == 0.0));
        let filled = row_positions(
            Vec2::new(0.0, 0.0),
            Vec2::new(32.0, 32.0),
            size,
            Mods {
                alt: true,
                ..default()
            },
        );
        assert_eq!(filled.len(), 4);
    }

    /// A plain release places one building for every selected
    /// constructor, claims the vent, disarms, and keeps the builder
    /// selected (the press and release are both consumed).
    #[test]
    fn click_places_and_disarms() {
        let mut world = world();
        let site = Vec3::new(10.0, 2.0, -4.0);
        let vent = world
            .spawn(GeoventSmoker {
                pos: site,
                emit_timer: 0.0,
                rng: 0,
            })
            .id();
        let builder = world
            .spawn((
                UnitType(UnitKind::Assembler),
                Selected,
                Transform::default(),
            ))
            .id();
        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Socket);
        set_site(&mut world, site, true);

        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        world.run_system_once(place_on_release).unwrap();
        assert!(
            !world
                .resource::<ButtonInput<MouseButton>>()
                .just_pressed(MouseButton::Left),
            "the placing press is consumed"
        );
        assert!(
            world.get::<PendingBuild>(builder).is_none(),
            "press alone places nothing"
        );
        {
            let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
            mouse.clear();
            mouse.release(MouseButton::Left);
        }
        set_site(&mut world, site, true);
        world.run_system_once(place_on_release).unwrap();
        assert!(
            !world
                .resource::<ButtonInput<MouseButton>>()
                .just_released(MouseButton::Left),
            "the release is consumed: click-to-select never deselects the builder"
        );
        let pending = world.get::<PendingBuild>(builder).expect("BuildAt issued");
        assert_eq!(pending.kind, UnitKind::Socket);
        assert_eq!(pending.site, site);
        assert!(world.get::<VentClaim>(vent).is_some());
        assert_eq!(world.resource::<PlacementMode>().kind, None);
        assert!(world.get::<Selected>(builder).is_some());
    }

    /// Shift keeps the command armed; after Shift is released the next
    /// click only disarms (`needShift`).
    #[test]
    fn shift_keeps_armed_until_a_plain_click() {
        let mut world = world();
        let builder = world
            .spawn((UnitType(UnitKind::Trojan), Selected, Transform::default()))
            .id();
        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::BadBlock);
        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ShiftLeft);
        set_site(&mut world, Vec3::new(40.0, 1.0, 40.0), true);
        click(&mut world);
        assert!(world.get::<PendingBuild>(builder).is_some());
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            Some(UnitKind::BadBlock)
        );

        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .release(KeyCode::ShiftLeft);
        world.entity_mut(builder).remove::<PendingBuild>();
        set_site(&mut world, Vec3::new(80.0, 1.0, 40.0), true);
        click(&mut world);
        assert!(
            world.get::<PendingBuild>(builder).is_none(),
            "no second building"
        );
        assert_eq!(world.resource::<PlacementMode>().kind, None);
    }

    /// A click on an invalid site places nothing and stays armed.
    #[test]
    fn invalid_site_keeps_armed() {
        let mut world = world();
        let builder = world
            .spawn((
                UnitType(UnitKind::Assembler),
                Selected,
                Transform::default(),
            ))
            .id();
        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Socket);
        set_site(&mut world, Vec3::ZERO, false);
        click(&mut world);
        assert!(world.get::<PendingBuild>(builder).is_none());
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            Some(UnitKind::Socket)
        );
    }

    /// A row of sites becomes one active order plus a queue.
    #[test]
    fn row_release_queues_every_site() {
        let mut world = world();
        let builder = world
            .spawn((UnitType(UnitKind::Gateway), Selected, Transform::default()))
            .id();
        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::LogicBomb);
        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ShiftLeft);
        world.resource_mut::<PlacementState>().sites = (0..3)
            .map(|i| PlannedSite {
                pos: Vec3::new(i as f32 * 16.0, 0.0, 0.0),
                valid: true,
            })
            .collect();
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        world.run_system_once(place_on_release).unwrap();
        {
            let mut mouse = world.resource_mut::<ButtonInput<MouseButton>>();
            mouse.clear();
            mouse.release(MouseButton::Left);
        }
        world.resource_mut::<PlacementState>().sites = (0..3)
            .map(|i| PlannedSite {
                pos: Vec3::new(i as f32 * 16.0, 0.0, 0.0),
                valid: true,
            })
            .collect();
        world.run_system_once(place_on_release).unwrap();
        assert_eq!(world.get::<PendingBuild>(builder).unwrap().site, Vec3::ZERO);
        assert_eq!(
            world.get::<CommandQueue>(builder).unwrap().commands.len(),
            2
        );
    }

    /// Only constructors that list the building receive the order.
    #[test]
    fn disarms_without_a_capable_builder() {
        let mut world = world();
        world.spawn((UnitType(UnitKind::Assembler), Selected));
        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Window);
        world.run_system_once(disarm_without_builders).unwrap();
        assert_eq!(world.resource::<PlacementMode>().kind, None);
    }
    /// Regression: the cursor ray must hit terrain only. The ghosts hover
    /// on the cursor ray, so an unfiltered cast hits a ghost first — the
    /// preview froze at its spawn point instead of following the cursor,
    /// which made every placement land (or fail) at a stale spot.
    #[test]
    fn sites_track_terrain_not_ghosts() {
        use bevy::camera::primitives::Aabb;
        use bevy::camera::visibility::SetViewVisibility;
        use bevy::camera::{ComputedCameraValues, RenderTargetInfo, Viewport};
        use bevy::math::{DVec2, Vec3A};
        // `cast_ray` culls candidates with `par_iter`.
        bevy::tasks::ComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut world = world();
        world.init_resource::<Assets<Mesh>>();
        world.insert_resource(UnitRegistry::empty());

        // 100×100 viewport, cursor dead centre, camera looking down at
        // 45° → the ray reaches the ground at the origin.
        let mut window = Window::default();
        window.set_physical_cursor_position(Some(DVec2::new(50.0, 50.0)));
        world.spawn(window);
        world.spawn((
            RtsCamera,
            Camera {
                viewport: Some(Viewport {
                    physical_position: UVec2::ZERO,
                    physical_size: UVec2::splat(100),
                    ..default()
                }),
                // Headless stand-in for what `camera_system` derives from
                // the render target.
                computed: ComputedCameraValues {
                    target_info: Some(RenderTargetInfo {
                        physical_size: UVec2::splat(100),
                        scale_factor: 1.0,
                    }),
                    ..default()
                },
                ..default()
            },
            GlobalTransform::from(
                Transform::from_xyz(0.0, 100.0, 100.0).looking_at(Vec3::ZERO, Vec3::Y),
            ),
        ));
        let solid = |world: &mut World, size: Vec3, at: Vec3, terrain: bool| {
            let mesh = world
                .resource_mut::<Assets<Mesh>>()
                .add(Mesh::from(Cuboid::from_size(size)));
            let mut e = world.spawn((
                Mesh3d(mesh),
                Transform::from_translation(at),
                GlobalTransform::from_translation(at),
                InheritedVisibility::VISIBLE,
                ViewVisibility::default(),
                Aabb {
                    center: Vec3A::ZERO,
                    half_extents: Vec3A::from(size / 2.0),
                },
            ));
            if terrain {
                e.insert(TerrainChunkMarker);
            } else {
                e.insert(GhostMarker);
            }
            let id = e.id();
            world.get_mut::<ViewVisibility>(id).unwrap().set_visible();
        };
        solid(&mut world, Vec3::new(400.0, 1.0, 400.0), Vec3::ZERO, true);
        // A ghost parked on the cursor ray halfway to the ground.
        solid(
            &mut world,
            Vec3::splat(4.0),
            Vec3::new(0.0, 50.0, 50.0),
            false,
        );

        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::BadBlock);
        world.run_system_once(plan_sites).unwrap();
        let sites = &world.resource::<PlacementState>().sites;
        assert_eq!(sites.len(), 1);
        assert!(
            sites[0].pos.xz().length() < 1.0,
            "the site sits on the terrain under the cursor, not under the ghost: {sites:?}",
        );
        assert!(sites[0].valid, "free flat ground is a valid Bad Block site");
    }
}

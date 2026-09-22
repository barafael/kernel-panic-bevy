//! Datavent-placement mode for mobile constructors.
//!
//! Armed by the build menu writing [`PlacementMode::kind`] when the
//! player clicks a constructor's building icon. While armed:
//!
//! 1. A translucent ghost of the target building's S3O mesh hovers at
//!    the cursor, snapping to the nearest unclaimed datavent within
//!    [`SNAP_RADIUS`]. Tinted green on a valid snap, red otherwise.
//! 2. Left-click commits a `BuildAt` order to every selected
//!    constructor (Shift queues; plain click replaces). The mouse press
//!    is cleared so the underlying selection system doesn't drop the
//!    selection.
//! 3. Right-click or Escape cancels.
//!
//! Runs `before(SelectionSet::Select)` so a consumed click never reaches
//! the click-to-select / drag-box logic.

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use crate::interaction::movement::{MoveTarget, QueuedCommand};
use crate::interaction::selection::{Selected, apply_ordered_command, ground_hit_filtered};
use crate::map_loading::TerrainChunkMarker;
use crate::rendering::camera::RtsCamera;
use crate::terrain::geovent::{GeoventSmoker, VentClaim};
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::meshes::{S3OModelCache, unit_material, unit_mesh};
use crate::units::components::{Faction, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

use super::build_menu::PlacementMode;

pub(super) struct PlacementPlugin;

impl Plugin for PlacementPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlacementGhost>().add_systems(
            Update,
            (manage_ghost_lifecycle, update_ghost, commit_or_cancel)
                .chain()
                // Run before selection so a committed/cancelled click
                // never reaches click-to-select. After the game-world
                // rebuild so stale ghost state is observed post-teardown
                // (the try_despawn calls tolerate the ghost's absence).
                .before(crate::interaction::selection::SelectionSet::Select)
                .after(crate::map_loading::GameWorldRebuild),
        );
    }
}

/// Max XZ distance from the cursor to a datavent for the ghost to snap.
const SNAP_RADIUS: f32 = 64.0;

const GHOST_VALID_COLOR: Color = Color::srgba(0.30, 1.00, 0.40, 0.55);
const GHOST_INVALID_COLOR: Color = Color::srgba(1.00, 0.30, 0.30, 0.55);

/// Marker for the ghost entity rendered in the world.
#[derive(Component)]
struct GhostMarker;

/// Sync state between the [`PlacementMode`] resource (driven by the
/// build menu) and the live ghost entity. Holds the entity handle so
/// `manage_ghost_lifecycle` can despawn it when placement is disarmed,
/// and the most recent snap result so `commit_or_cancel` knows whether
/// the click is on a valid site without re-running the raycast.
#[derive(Resource, Default)]
struct PlacementGhost {
    entity: Option<Entity>,
    /// `kind` the ghost was spawned for, so a kind change rebuilds the
    /// mesh.
    spawned_kind: Option<UnitKind>,
    /// Snapped vent position, set by [`update_ghost`] each frame.
    snapped: Option<Vec3>,
}

#[allow(clippy::too_many_arguments)]
fn manage_ghost_lifecycle(
    mut commands: Commands,
    mut state: ResMut<PlacementGhost>,
    mut placement: ResMut<PlacementMode>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    unit_registry: Res<UnitRegistry>,
    selected_q: Query<&Faction, With<Selected>>,
    constructors_q: Query<(), (With<UnitType>, With<Selected>)>,
) {
    // Auto-cancel if no constructor is selected — armed placement with
    // an empty selection has nothing to dispatch the order to.
    if placement.kind.is_some() && constructors_q.is_empty() {
        placement.kind = None;
    }

    match (placement.kind, state.entity, state.spawned_kind) {
        (Some(kind), Some(entity), Some(prev)) if prev != kind => {
            // Kind changed mid-placement (rare: user clicked a
            // different building icon). Despawn and respawn so the
            // ghost mesh updates. `try_despawn` tolerates the ghost
            // having gone with a game-world teardown.
            commands.entity(entity).try_despawn();
            state.entity = None;
            state.spawned_kind = None;
            spawn_ghost(
                kind,
                &selected_q,
                &mut state,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &unit_registry,
            );
        }
        (Some(kind), None, _) => {
            spawn_ghost(
                kind,
                &selected_q,
                &mut state,
                &mut commands,
                &mut meshes,
                &mut materials,
                &mut images,
                &mut model_cache,
                &unit_registry,
            );
        }
        (None, Some(entity), _) => {
            // `try_despawn` tolerates the ghost having gone with a
            // game-world teardown (menu reload / restart).
            commands.entity(entity).try_despawn();
            state.entity = None;
            state.spawned_kind = None;
            state.snapped = None;
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_ghost(
    kind: UnitKind,
    selected_q: &Query<&Faction, With<Selected>>,
    state: &mut PlacementGhost,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    model_cache: &mut S3OModelCache,
    unit_registry: &UnitRegistry,
) {
    // Tint with the constructor's faction so the preview reads as
    // "this will be mine".
    let faction = selected_q.iter().next().copied().unwrap_or(Faction::System);

    let mesh = unit_mesh(kind, meshes, model_cache, unit_registry);
    let model_name = unit_registry.model(kind).to_string();
    let base_mat = unit_material(kind, faction, materials, images, model_cache, &model_name);

    // Clone the base material into a translucent tinted variant so
    // tweaking valid/invalid color doesn't leak into the real building.
    let ghost_mat = {
        let source = materials.get(&base_mat).cloned().unwrap_or_default();
        materials.add(StandardMaterial {
            base_color: GHOST_VALID_COLOR,
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            base_color_texture: source.base_color_texture.clone(),
            ..default()
        })
    };

    let entity = commands
        .spawn((
            GhostMarker,
            Mesh3d(mesh),
            MeshMaterial3d(ghost_mat),
            Transform::default(),
            Visibility::Hidden,
        ))
        .id();

    state.entity = Some(entity);
    state.spawned_kind = Some(kind);
    state.snapped = None;
}

#[allow(clippy::too_many_arguments)]
fn update_ghost(
    mut state: ResMut<PlacementGhost>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    mut transforms: Query<(&mut Transform, &mut Visibility), With<GhostMarker>>,
    ghost_mats: Query<&MeshMaterial3d<StandardMaterial>, With<GhostMarker>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    geovents: Query<&GeoventSmoker, Without<VentClaim>>,
    terrain: Query<(), With<TerrainChunkMarker>>,
    placement: Res<PlacementMode>,
    heightmap: Option<Res<Heightmap>>,
    unit_registry: Res<UnitRegistry>,
) {
    let Some(ghost) = state.entity else {
        return;
    };

    // Terrain-only cast. The ghost itself hovers exactly on the cursor
    // ray, so an unfiltered cast would hit its own translucent mesh
    // first and freeze the preview at its spawn point instead of
    // tracking the cursor.
    let terrain_only = |e: Entity| terrain.contains(e);
    let Some(cursor_pt) = ground_hit_filtered(&windows, &camera_q, &mut ray_cast, &terrain_only)
    else {
        // Cursor off-screen or off-terrain: hide the ghost this frame.
        if let Ok((_, mut vis)) = transforms.get_mut(ghost) {
            *vis = Visibility::Hidden;
        }
        state.snapped = None;
        return;
    };

    // Snap to the nearest unclaimed vent in XZ.
    let mut best: Option<(Vec3, f32)> = None;
    for vent in &geovents {
        let dx = vent.pos.x - cursor_pt.x;
        let dz = vent.pos.z - cursor_pt.z;
        let d = (dx * dx + dz * dz).sqrt();
        if d <= SNAP_RADIUS && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((vent.pos, d));
        }
    }

    let (pos, mut valid) = match best {
        Some((p, _)) => (p, true),
        None => (cursor_pt, false),
    };

    // Slope gate: even on a snapped vent, reject if the building's
    // footprint would straddle terrain steeper than its FBI MaxSlope.
    // Mirrors upstream's `CGameHelper::TestUnitBuildSquare` slope check.
    if valid && let (Some(kind), Some(hm)) = (placement.kind, heightmap.as_deref()) {
        let footprint = unit_registry.footprint_elmos(kind);
        let cap = unit_registry.max_slope_ratio(kind);
        if hm.max_slope_in_footprint(pos, footprint) > cap {
            valid = false;
        }
    }

    state.snapped = if valid { Some(pos) } else { None };

    if let Ok((mut tf, mut vis)) = transforms.get_mut(ghost) {
        // Lift slightly so the mesh doesn't z-fight with the ground.
        tf.translation = pos + Vec3::Y * 0.5;
        *vis = Visibility::Inherited;
    }
    if let Ok(mat_handle) = ghost_mats.get(ghost)
        && let Some(mat) = materials.get_mut(&mat_handle.0)
    {
        mat.base_color = if valid {
            GHOST_VALID_COLOR
        } else {
            GHOST_INVALID_COLOR
        };
    }
}

#[allow(clippy::too_many_arguments)]
fn commit_or_cancel(
    mut commands: Commands,
    mut placement: ResMut<PlacementMode>,
    state: Res<PlacementGhost>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    builders: Query<(Entity, &UnitType), With<Selected>>,
    move_target_q: Query<(), With<MoveTarget>>,
    vents: Query<(Entity, &GeoventSmoker), Without<VentClaim>>,
    ui_interactions: Query<&Interaction>,
) {
    if placement.kind.is_none() {
        return;
    }

    // A cursor over any live UI node (build icons, order palette, HUD)
    // means the click belongs to the UI. The ghost's snapped position
    // is stale from wherever the cursor last touched terrain —
    // committing under the panel would build at a spot the player
    // isn't even pointing at. Placement stays armed.
    if ui_interactions
        .iter()
        .any(|i| matches!(i, Interaction::Pressed | Interaction::Hovered))
    {
        return;
    }

    if mouse.just_pressed(MouseButton::Right) {
        mouse.clear_just_pressed(MouseButton::Right);
        placement.kind = None;
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        keys.clear_just_pressed(KeyCode::Escape);
        placement.kind = None;
        return;
    }

    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(kind) = placement.kind else {
        return;
    };
    let Some(site) = state.snapped else {
        // Click on invalid site: still consume so it doesn't bleed into
        // selection (would deselect the constructor mid-placement).
        mouse.clear_just_pressed(MouseButton::Left);
        return;
    };

    let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

    let mut any_dispatched = false;
    for (entity, ut) in &builders {
        if !ut.0.is_constructor() {
            continue;
        }
        apply_ordered_command(
            entity,
            QueuedCommand::BuildAt { kind, site },
            shift,
            &move_target_q,
            &mut commands,
        );
        any_dispatched = true;
    }

    if any_dispatched {
        // Stamp the claim so a second constructor can't queue onto the
        // same vent during this frame.
        for (vent_entity, vent) in &vents {
            if vent.pos.distance_squared(site) < 1.0 {
                commands.entity(vent_entity).insert(VentClaim);
                break;
            }
        }
    }

    if !shift {
        placement.kind = None;
    }
    mouse.clear_just_pressed(MouseButton::Left);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::lifecycle::construction::PendingBuild;
    use bevy::camera::Viewport;
    use bevy::camera::primitives::Aabb;
    use bevy::camera::visibility::SetViewVisibility;
    use bevy::camera::{ComputedCameraValues, RenderTargetInfo};
    use bevy::ecs::system::RunSystemOnce;
    use bevy::math::primitives::Cuboid;
    use bevy::math::{DVec2, Vec3A};
    use bevy::mesh::Mesh;

    /// Commit dispatches `BuildAt` (as `PendingBuild` + `MoveTarget`) to
    /// every selected constructor and stamps the vent so a second
    /// constructor can't queue onto it. Plain click disarms placement.
    #[test]
    fn commit_dispatches_buildat_claims_vent_and_disarms() {
        let mut world = World::new();
        world.init_resource::<PlacementMode>();
        world.init_resource::<PlacementGhost>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();

        let site = Vec3::new(10.0, 2.0, -4.0);
        let vent = world
            .spawn((GeoventSmoker {
                pos: site,
                emit_timer: 0.0,
                rng: 0,
            },))
            .id();
        let builder = world
            .spawn((
                UnitType(UnitKind::Assembler),
                Selected,
                Transform::default(),
            ))
            .id();

        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Socket);
        world.resource_mut::<PlacementGhost>().snapped = Some(site);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);

        world.run_system_once(commit_or_cancel).unwrap();

        let b = world.entity(builder);
        let pending = b.get::<PendingBuild>().expect("PendingBuild inserted");
        assert_eq!(pending.kind, UnitKind::Socket);
        assert_eq!(pending.site, site);
        let target = b.get::<MoveTarget>().expect("MoveTarget inserted");
        assert_eq!(target.0, site);
        assert!(world.get::<VentClaim>(vent).is_some(), "vent claimed");
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            None,
            "plain click must disarm placement",
        );
    }

    /// Shift-click queues: the order is dispatched (with a fresh queue —
    /// the builder had no active order) and placement stays armed so the
    /// next click places another one.
    #[test]
    fn shift_keeps_placement_armed_for_queueing() {
        let mut world = World::new();
        world.init_resource::<PlacementMode>();
        world.init_resource::<PlacementGhost>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();

        let site = Vec3::new(40.0, 1.0, 40.0);
        world.spawn((GeoventSmoker {
            pos: site,
            emit_timer: 0.0,
            rng: 1,
        },));
        let builder = world
            .spawn((UnitType(UnitKind::Trojan), Selected, Transform::default()))
            .id();

        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Firewall);
        world.resource_mut::<PlacementGhost>().snapped = Some(site);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        world
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ShiftLeft);

        world.run_system_once(commit_or_cancel).unwrap();

        assert!(world.entity(builder).get::<PendingBuild>().is_some());
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            Some(UnitKind::Firewall),
            "shift must keep placement armed",
        );
    }

    /// Right-click cancels placement and consumes the press so the
    /// right-click move-order system never sees it.
    #[test]
    fn right_click_cancels_and_consumes_the_press() {
        let mut world = World::new();
        world.init_resource::<PlacementMode>();
        world.init_resource::<PlacementGhost>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();

        world.spawn((UnitType(UnitKind::Gateway), Selected, Transform::default()));

        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Debug);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Right);

        world.run_system_once(commit_or_cancel).unwrap();

        assert_eq!(world.resource::<PlacementMode>().kind, None);
        let mouse = world.resource::<ButtonInput<MouseButton>>();
        assert!(
            !mouse.just_pressed(MouseButton::Right),
            "cancel must eat the click so it can't double as a move order",
        );
    }

    /// A left-click on an invalid site (no snapped vent) places nothing,
    /// is swallowed so it can't fall through into click-to-select, and
    /// keeps placement armed for a proper click elsewhere.
    #[test]
    fn invalid_site_swallows_click_and_stays_armed() {
        let mut world = World::new();
        world.init_resource::<PlacementMode>();
        world.init_resource::<PlacementGhost>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();

        let builder = world
            .spawn((
                UnitType(UnitKind::Assembler),
                Selected,
                Transform::default(),
            ))
            .id();

        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Socket);
        world.resource_mut::<PlacementGhost>().snapped = None;
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);

        world.run_system_once(commit_or_cancel).unwrap();

        assert!(world.entity(builder).get::<PendingBuild>().is_none());
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            Some(UnitKind::Socket)
        );
        assert!(
            !world
                .resource::<ButtonInput<MouseButton>>()
                .just_pressed(MouseButton::Left),
            "invalid-site click must not leak into selection",
        );
    }

    /// A click over a UI node (e.g. a build icon) never commits a
    /// placement at the ghost's stale snapped position, and placement
    /// stays armed afterwards.
    #[test]
    fn ui_click_never_commits_placement() {
        let mut world = World::new();
        world.init_resource::<PlacementMode>();
        world.init_resource::<PlacementGhost>();
        world.init_resource::<ButtonInput<MouseButton>>();
        world.init_resource::<ButtonInput<KeyCode>>();

        let builder = world
            .spawn((
                UnitType(UnitKind::Assembler),
                Selected,
                Transform::default(),
            ))
            .id();
        world.spawn((Button, Interaction::Hovered, Transform::default()));

        let stale = Vec3::new(-30.0, 0.0, 12.0);
        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Socket);
        world.resource_mut::<PlacementGhost>().snapped = Some(stale);
        world
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);

        world.run_system_once(commit_or_cancel).unwrap();

        assert!(world.entity(builder).get::<PendingBuild>().is_none());
        assert_eq!(
            world.resource::<PlacementMode>().kind,
            Some(UnitKind::Socket)
        );
    }

    /// Regression: the ghost's cursor ray must hit terrain only. The
    /// ghost hovers exactly on the cursor ray, so an unfiltered cast
    /// hits its own translucent mesh first — the preview froze at its
    /// spawn point instead of following the cursor, which made every
    /// placement land (or fail) at a stale spot.
    fn ghost_tracks_terrain_and_ignores_itself_setup() {
        // `cast_ray` culls candidates with `par_iter`.
        bevy::tasks::ComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
    }

    #[test]
    fn ghost_tracks_terrain_and_ignores_itself() {
        ghost_tracks_terrain_and_ignores_itself_setup();
        let mut world = World::new();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<PlacementMode>();
        world.init_resource::<PlacementGhost>();
        world.insert_resource(UnitRegistry::empty());

        // 100x100 viewport with the cursor dead-centre; camera 100 units
        // above the origin looking straight down → ray hits (0, 0, 0).
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
                // the render target: scale factor 1.0 so the logical
                // cursor conversion works without a render world.
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
                Transform::from_xyz(0.0, 100.0, 0.0).looking_at(Vec3::ZERO, Vec3::Z),
            ),
        ));

        // Terrain plane: a wide flat box at ground level.
        let terrain_mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::from(Cuboid::new(200.0, 1.0, 200.0)));
        let terrain = world
            .spawn((
                TerrainChunkMarker,
                Mesh3d(terrain_mesh),
                Transform::default(),
                GlobalTransform::default(),
                InheritedVisibility::VISIBLE,
                ViewVisibility::default(),
                Aabb {
                    center: Vec3A::ZERO,
                    half_extents: Vec3A::new(100.0, 0.5, 100.0),
                },
            ))
            .id();
        // Headless stand-in for render-world visibility propagation.
        {
            let mut vv = world.get_mut::<ViewVisibility>(terrain).unwrap();
            vv.set_visible();
        }

        // The ghost itself, parked exactly on the cursor ray above the
        // ground point — a decoy that an unfiltered cast would hit.
        let ghost_mesh = world
            .resource_mut::<Assets<Mesh>>()
            .add(Mesh::from(Cuboid::new(4.0, 4.0, 4.0)));
        let ghost_mat = world
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial {
                base_color: GHOST_VALID_COLOR,
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                ..default()
            });
        let ghost = world
            .spawn((
                GhostMarker,
                Mesh3d(ghost_mesh),
                MeshMaterial3d(ghost_mat),
                Transform::from_xyz(0.0, 50.0, 0.0),
                GlobalTransform::from_xyz(0.0, 50.0, 0.0),
                Visibility::Inherited,
                InheritedVisibility::VISIBLE,
                ViewVisibility::default(),
                Aabb {
                    center: Vec3A::ZERO,
                    half_extents: Vec3A::splat(2.0),
                },
            ))
            .id();
        // Mark the decoy view-visible (same call the render world uses).
        {
            let mut vv = world.get_mut::<ViewVisibility>(ghost).unwrap();
            vv.set_visible();
        }

        world.resource_mut::<PlacementMode>().kind = Some(UnitKind::Socket);
        // The lifecycle system normally installs these; point the ghost
        // state straight at the decoy.
        world.resource_mut::<PlacementGhost>().entity = Some(ghost);
        world.resource_mut::<PlacementGhost>().spawned_kind = Some(UnitKind::Socket);

        world.run_system_once(update_ghost).unwrap();

        let tf = world.get::<Transform>(ghost).unwrap();
        assert!(
            (tf.translation - Vec3::new(0.0, 1.0, 0.0)).length() < 0.5,
            "ghost must sit on the terrain under the cursor (+0.5 lift over \
             the box top at y=0.5), not on its own mesh: got {tf:?}",
        );
        assert_eq!(
            world.resource::<PlacementGhost>().snapped,
            None,
            "no vents around: nothing valid to snap to",
        );
    }
}

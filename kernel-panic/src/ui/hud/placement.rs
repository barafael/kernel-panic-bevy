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
use crate::interaction::selection::{Selected, apply_ordered_command, ground_hit};
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
                // never reaches click-to-select.
                .before(crate::interaction::selection::SelectionSet::Select),
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
            // ghost mesh updates.
            commands.entity(entity).despawn();
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
            commands.entity(entity).despawn();
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
    placement: Res<PlacementMode>,
    heightmap: Option<Res<Heightmap>>,
    unit_registry: Res<UnitRegistry>,
) {
    let Some(ghost) = state.entity else {
        return;
    };

    let Some(cursor_pt) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
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
) {
    if placement.kind.is_none() {
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

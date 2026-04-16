use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use crate::rendering::camera::RtsCamera;
use crate::units::components::{Faction, Health, SelectionVolume, UnitType, health_color};

use super::movement::{MovePath, MoveTarget};

/// Marker for temporary move-target indicators on the ground.
#[derive(Component)]
pub struct MoveIndicator {
    pub lifetime: Timer,
}

/// Marker for ephemeral formation preview dots shown during right-drag.
#[derive(Component)]
pub struct FormationPreview;

/// Despawn move indicators after their lifetime expires.
pub fn decay_move_indicators(
    time: Res<Time>,
    mut query: Query<(Entity, &mut MoveIndicator)>,
    mut commands: Commands,
) {
    for (entity, mut indicator) in &mut query {
        indicator.lifetime.tick(time.delta());
        if indicator.lifetime.is_finished() {
            commands.entity(entity).despawn();
        }
    }
}

/// Marks a unit as selected by the player.
#[derive(Component)]
pub struct Selected;

/// Marks the unit currently under the cursor.
#[derive(Component)]
pub struct Hovered;

/// Stores the unit's original (un-brightened) material handle so we can
/// restore it when the unit is no longer hovered or selected.
#[derive(Component)]
pub struct OriginalMaterial(pub Handle<StandardMaterial>);

/// Health bar background child entity.
#[derive(Component)]
pub struct HealthBarBg;

/// Health bar foreground (colored) child entity.
#[derive(Component)]
pub struct HealthBarFg;

/// Shared mesh and material assets for health bars.
#[derive(Resource, Clone)]
pub(crate) struct HealthBarAssets {
    bar_mesh: Handle<Mesh>,
    bg_material: Handle<StandardMaterial>,
}

/// Emissive boost multiplier for hovered units.
const HOVER_BRIGHTNESS: f32 = 1.5;
/// Emissive boost multiplier for selected units.
const SELECTED_BRIGHTNESS: f32 = 2.5;

/// Health bar dimensions (world-space units).
const HEALTH_BAR_WIDTH: f32 = 20.0;
const HEALTH_BAR_HEIGHT: f32 = 2.0;
/// Vertical offset above the unit's origin.
const HEALTH_BAR_Y_OFFSET: f32 = 30.0;

/// Update `Hovered` component each frame based on cursor position.
pub fn update_hover(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    unit_q: Query<Entity, With<UnitType>>,
    volume_q: Query<&ChildOf, With<SelectionVolume>>,
    hovered_q: Query<Entity, With<Hovered>>,
    mut commands: Commands,
) {
    // Clear previous hover.
    for entity in &hovered_q {
        commands.entity(entity).remove::<Hovered>();
    }

    let Some(ray) = cursor_ray(&windows, &camera_q) else {
        return;
    };

    let hits = ray_cast.cast_ray(ray, &default());
    if let Some(entity) = resolve_unit_hit(hits, &unit_q, &volume_q) {
        commands.entity(entity).insert(Hovered);
    }
}

/// Minimum drag distance in pixels before it counts as a box-select.
const DRAG_THRESHOLD: f32 = 8.0;

/// Tracks drag state for box selection.
#[derive(Resource, Default)]
pub struct DragState {
    /// Screen position where left mouse was pressed.
    start: Option<Vec2>,
    /// Whether we're actively dragging (past threshold).
    dragging: bool,
}

/// Visual overlay for the selection box.
#[derive(Component)]
pub struct SelectionBoxNode;

/// Handle left-click and drag-box selection.
///
/// Modifier behaviour (matches original Kernel Panic):
/// - **Plain click/drag** -- replace the current selection.
/// - **Shift+click/drag** -- add to the current selection.
/// - **Ctrl+click** -- toggle the clicked unit in/out of the selection.
#[allow(clippy::too_many_arguments)]
pub fn handle_selection(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    hovered_q: Query<Entity, With<Hovered>>,
    selected_q: Query<Entity, With<Selected>>,
    unit_q: Query<(Entity, &GlobalTransform), With<UnitType>>,
    box_nodes: Query<Entity, With<SelectionBoxNode>>,
    mut drag_state: ResMut<DragState>,
    mut commands: Commands,
) {
    let cursor_pos = windows.single().ok().and_then(|w| w.cursor_position());

    // --- Left press: start tracking ---
    if mouse.just_pressed(MouseButton::Left) {
        drag_state.start = cursor_pos;
        drag_state.dragging = false;
    }

    // --- While held: update box if past threshold ---
    if mouse.pressed(MouseButton::Left)
        && let (Some(start), Some(current)) = (drag_state.start, cursor_pos)
    {
        let distance = (current - start).length();
        if distance > DRAG_THRESHOLD {
            drag_state.dragging = true;

            let min_x = start.x.min(current.x);
            let min_y = start.y.min(current.y);
            let width = (current.x - start.x).abs();
            let height = (current.y - start.y).abs();

            for entity in &box_nodes {
                commands.entity(entity).despawn();
            }

            commands.spawn((
                SelectionBoxNode,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(min_x),
                    top: Val::Px(min_y),
                    width: Val::Px(width),
                    height: Val::Px(height),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BorderColor::all(Color::WHITE),
                BackgroundColor(Color::NONE),
            ));
        }
    }

    // --- Left release ---
    if mouse.just_released(MouseButton::Left) {
        for entity in &box_nodes {
            commands.entity(entity).despawn();
        }

        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
        let additive = shift || ctrl;

        if !additive {
            for entity in &selected_q {
                commands.entity(entity).remove::<Selected>();
            }
        }

        if drag_state.dragging {
            if let (Some(start), Some(end)) = (drag_state.start, cursor_pos) {
                let min_screen = Vec2::new(start.x.min(end.x), start.y.min(end.y));
                let max_screen = Vec2::new(start.x.max(end.x), start.y.max(end.y));

                if let Ok((camera, camera_transform)) = camera_q.single() {
                    for (entity, global_transform) in &unit_q {
                        let Ok(screen_pos) = camera
                            .world_to_viewport(camera_transform, global_transform.translation())
                        else {
                            continue;
                        };
                        if screen_pos.x >= min_screen.x
                            && screen_pos.x <= max_screen.x
                            && screen_pos.y >= min_screen.y
                            && screen_pos.y <= max_screen.y
                        {
                            commands.entity(entity).insert(Selected);
                        }
                    }
                }
            }
        } else if ctrl {
            if let Some(entity) = hovered_q.iter().next() {
                if selected_q.contains(entity) {
                    commands.entity(entity).remove::<Selected>();
                } else {
                    commands.entity(entity).insert(Selected);
                }
            }
        } else {
            if let Some(entity) = hovered_q.iter().next() {
                commands.entity(entity).insert(Selected);
            }
        }

        drag_state.start = None;
        drag_state.dragging = false;
    }
}

/// Minimum distance between sampled path points (world units).
const PATH_SAMPLE_MIN_DISTANCE: f32 = 20.0;

/// Tracks right-click drag path for move commands.
#[derive(Resource, Default)]
pub struct RightDragPath {
    /// World-space points sampled along the drag path.
    points: Vec<Vec3>,
    /// Whether we're actively dragging.
    active: bool,
}

/// Buffered indicator targets written by `handle_right_click`, consumed by
/// `spawn_move_indicator_visuals`. Separated into two systems because
/// `MeshRayCast` holds `Res<Assets<Mesh>>` which conflicts with `ResMut`.
#[derive(Resource, Default)]
pub struct PendingMoveIndicators {
    pub targets: Vec<Vec3>,
}

/// Right-click: single click moves all selected to one point.
/// Right-drag: sample a path, distribute selected units along it on release.
#[allow(clippy::too_many_arguments)]
pub fn handle_right_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    selected_q: Query<Entity, With<Selected>>,
    mut commands: Commands,
    mut drag_path: ResMut<RightDragPath>,
    mut pending: ResMut<PendingMoveIndicators>,
) {
    if mouse.just_pressed(MouseButton::Right) {
        drag_path.points.clear();
        drag_path.active = true;

        if let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast) {
            drag_path.points.push(point);
        }
    }

    if mouse.pressed(MouseButton::Right)
        && drag_path.active
        && let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast)
    {
        let dominated = drag_path
            .points
            .last()
            .is_some_and(|last| last.distance(point) < PATH_SAMPLE_MIN_DISTANCE);
        if !dominated {
            drag_path.points.push(point);
        }
    }

    if mouse.just_released(MouseButton::Right) && drag_path.active {
        drag_path.active = false;

        let units: Vec<Entity> = selected_q.iter().collect();
        if units.is_empty() || drag_path.points.is_empty() {
            return;
        }

        if drag_path.points.len() == 1 {
            let target = drag_path.points[0];
            for entity in &units {
                commands
                    .entity(*entity)
                    .insert(MoveTarget(target))
                    .remove::<MovePath>();
            }
            pending.targets.push(target);
        } else {
            let targets = sample_path_evenly(&drag_path.points, units.len());
            for (entity, target) in units.iter().zip(targets.iter()) {
                commands
                    .entity(*entity)
                    .insert(MoveTarget(*target))
                    .remove::<MovePath>();
            }
            pending.targets.extend(targets);
        }

        drag_path.points.clear();
    }
}

/// Drains `PendingMoveIndicators` and creates torus visuals.
///
/// Separated from `handle_right_click` because `MeshRayCast` holds an
/// immutable `Res<Assets<Mesh>>` that conflicts with `ResMut<Assets<Mesh>>`.
pub fn spawn_move_indicator_visuals(
    mut pending: ResMut<PendingMoveIndicators>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if pending.targets.is_empty() {
        return;
    }

    let mesh = meshes.add(Torus::new(2.0, 4.0));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.0, 1.0, 0.3, 0.6),
        emissive: LinearRgba::new(0.0, 1.0, 0.3, 1.0) * 2.0,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });

    for target in pending.targets.drain(..) {
        commands.spawn((
            MoveIndicator {
                lifetime: Timer::from_seconds(1.5, TimerMode::Once),
            },
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(target + Vec3::Y * 1.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
    }
}

/// Shared assets for formation preview indicators.
#[derive(Resource, Clone)]
pub(crate) struct FormationPreviewAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

/// Show/update/remove preview dots during a right-drag formation draw.
pub fn update_formation_preview(
    drag_path: Res<RightDragPath>,
    selected_q: Query<Entity, With<Selected>>,
    preview_q: Query<Entity, With<FormationPreview>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    assets: Option<Res<FormationPreviewAssets>>,
) {
    // If drag is not active or path has fewer than 2 points, despawn any existing previews.
    let unit_count = selected_q.iter().count();
    if !drag_path.active || drag_path.points.len() < 2 || unit_count == 0 {
        for entity in &preview_q {
            commands.entity(entity).despawn();
        }
        return;
    }

    // Lazy-init shared assets.
    let preview_assets = if let Some(a) = assets {
        a.clone()
    } else {
        let a = FormationPreviewAssets {
            mesh: meshes.add(Torus::new(1.5, 3.0)),
            material: materials.add(StandardMaterial {
                base_color: Color::srgba(1.0, 1.0, 0.3, 0.4),
                emissive: LinearRgba::new(1.0, 1.0, 0.3, 1.0) * 1.5,
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            }),
        };
        commands.insert_resource(a.clone());
        a
    };

    let targets = sample_path_evenly(&drag_path.points, unit_count);

    // Despawn old previews and spawn fresh ones.
    for entity in &preview_q {
        commands.entity(entity).despawn();
    }
    for target in &targets {
        commands.spawn((
            FormationPreview,
            Mesh3d(preview_assets.mesh.clone()),
            MeshMaterial3d(preview_assets.material.clone()),
            Transform::from_translation(*target + Vec3::Y * 1.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
    }
}

fn ground_hit(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut MeshRayCast,
) -> Option<Vec3> {
    let ray = cursor_ray(windows, camera_q)?;
    let hits = ray_cast.cast_ray(ray, &default());
    hits.first().map(|(_, hit)| hit.point)
}

fn sample_path_evenly(path: &[Vec3], count: usize) -> Vec<Vec3> {
    if count == 0 || path.is_empty() {
        return vec![];
    }
    if count == 1 {
        return vec![*path.last().unwrap()];
    }

    let mut cumulative = vec![0.0f32];
    for i in 1..path.len() {
        let prev = cumulative[i - 1];
        cumulative.push(prev + path[i - 1].distance(path[i]));
    }
    let total_length = *cumulative.last().unwrap();

    if total_length < 0.01 {
        return vec![path[0]; count];
    }

    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let target_dist = (i as f32 / (count - 1) as f32) * total_length;

        let seg = cumulative
            .windows(2)
            .position(|w| w[0] <= target_dist && target_dist <= w[1])
            .unwrap_or(path.len() - 2);

        let seg_start_dist = cumulative[seg];
        let seg_length = cumulative[seg + 1] - seg_start_dist;
        let t = if seg_length > 0.0 {
            (target_dist - seg_start_dist) / seg_length
        } else {
            0.0
        };

        result.push(path[seg].lerp(path[seg + 1], t));
    }

    result
}

// ---------------------------------------------------------------------------
// Material brightening for hover / selection
// ---------------------------------------------------------------------------

/// Brighten a unit's materials when it becomes hovered or selected.
/// Works on both the root entity and its piece children (S3O models).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update_unit_highlight(
    hovered_q: Query<(Entity, &Faction), (With<Hovered>, Without<Selected>, With<UnitType>)>,
    selected_q: Query<(Entity, &Faction), (With<Selected>, With<UnitType>)>,
    unhighlighted_q: Query<
        Entity,
        (
            Without<Hovered>,
            Without<Selected>,
            With<UnitType>,
            With<Highlighted>,
        ),
    >,
    children_q: Query<&Children>,
    mesh_mat_q: Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    original_q: Query<&OriginalMaterial>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    // Restore materials on units that are no longer hovered or selected.
    for unit_entity in &unhighlighted_q {
        restore_unit_materials(unit_entity, &children_q, &original_q, &mut commands);
        commands.entity(unit_entity).try_remove::<Highlighted>();
    }

    // Apply hover brightness (only if not also selected).
    for (unit_entity, faction) in &hovered_q {
        brighten_unit(
            unit_entity,
            faction,
            HOVER_BRIGHTNESS,
            &children_q,
            &mesh_mat_q,
            &original_q,
            &mut materials,
            &mut commands,
        );
        commands.entity(unit_entity).insert(Highlighted);
    }

    // Apply selection brightness (takes priority over hover).
    for (unit_entity, faction) in &selected_q {
        brighten_unit(
            unit_entity,
            faction,
            SELECTED_BRIGHTNESS,
            &children_q,
            &mesh_mat_q,
            &original_q,
            &mut materials,
            &mut commands,
        );
        commands.entity(unit_entity).insert(Highlighted);
    }
}

/// Marker to track that a unit currently has brightened materials.
#[derive(Component)]
pub(crate) struct Highlighted;

/// Brighten all mesh materials on a unit entity and its children.
#[allow(clippy::too_many_arguments)]
fn brighten_unit(
    unit_entity: Entity,
    faction: &Faction,
    factor: f32,
    children_q: &Query<&Children>,
    mesh_mat_q: &Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    commands: &mut Commands,
) {
    // Collect all entities to brighten: the unit itself + all descendants with meshes.
    let mut targets = Vec::new();
    if mesh_mat_q.contains(unit_entity) {
        targets.push(unit_entity);
    }
    collect_mesh_descendants(unit_entity, children_q, mesh_mat_q, &mut targets);

    for entity in targets {
        let Ok((_, current_mat)) = mesh_mat_q.get(entity) else {
            continue;
        };
        apply_brightness(
            entity,
            &current_mat.0,
            faction,
            factor,
            original_q,
            materials,
            commands,
        );
    }
}

/// Restore original materials on a unit and all its descendants.
fn restore_unit_materials(
    unit_entity: Entity,
    children_q: &Query<&Children>,
    original_q: &Query<&OriginalMaterial>,
    commands: &mut Commands,
) {
    let mut targets = Vec::new();
    if original_q.contains(unit_entity) {
        targets.push(unit_entity);
    }
    collect_original_descendants(unit_entity, children_q, original_q, &mut targets);

    for entity in targets {
        if let Ok(original) = original_q.get(entity) {
            commands
                .entity(entity)
                .try_insert(MeshMaterial3d(original.0.clone()))
                .try_remove::<OriginalMaterial>();
        }
    }
}

fn collect_mesh_descendants(
    entity: Entity,
    children_q: &Query<&Children>,
    mesh_mat_q: &Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    targets: &mut Vec<Entity>,
) {
    if let Ok(children) = children_q.get(entity) {
        for child in children.iter() {
            if mesh_mat_q.contains(child) {
                targets.push(child);
            }
            collect_mesh_descendants(child, children_q, mesh_mat_q, targets);
        }
    }
}

fn collect_original_descendants(
    entity: Entity,
    children_q: &Query<&Children>,
    original_q: &Query<&OriginalMaterial>,
    targets: &mut Vec<Entity>,
) {
    if let Ok(children) = children_q.get(entity) {
        for child in children.iter() {
            if original_q.contains(child) {
                targets.push(child);
            }
            collect_original_descendants(child, children_q, original_q, targets);
        }
    }
}

fn apply_brightness(
    entity: Entity,
    current_handle: &Handle<StandardMaterial>,
    faction: &Faction,
    factor: f32,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    commands: &mut Commands,
) {
    let source_handle = if let Ok(orig) = original_q.get(entity) {
        orig.0.clone()
    } else {
        let h = current_handle.clone();
        commands
            .entity(entity)
            .try_insert(OriginalMaterial(h.clone()));
        h
    };

    let Some(source) = materials.get(&source_handle) else {
        return;
    };

    let mut bright = source.clone();
    let color = LinearRgba::from(faction.color());
    bright.emissive = color * factor;
    let handle = materials.add(bright);
    commands.entity(entity).try_insert(MeshMaterial3d(handle));
}

// ---------------------------------------------------------------------------
// Health bars for selected units
// ---------------------------------------------------------------------------

/// Spawn health bar child entities on newly-selected units.
pub fn spawn_health_bars(
    new_selections: Query<Entity, Added<Selected>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    bar_assets: Option<Res<HealthBarAssets>>,
) {
    if new_selections.is_empty() {
        return;
    }

    let assets = get_or_init_bar_assets(bar_assets, &mut commands, &mut meshes, &mut materials);

    for entity in &new_selections {
        // Background bar (dark).
        // The Plane3d mesh is 1x1 in XY with Z normal. Scale X=width, Y=height.
        commands.entity(entity).with_child((
            HealthBarBg,
            Mesh3d(assets.bar_mesh.clone()),
            MeshMaterial3d(assets.bg_material.clone()),
            Transform::from_xyz(0.0, HEALTH_BAR_Y_OFFSET, 0.0).with_scale(Vec3::new(
                HEALTH_BAR_WIDTH,
                HEALTH_BAR_HEIGHT,
                1.0,
            )),
        ));

        let fg_material = materials.add(StandardMaterial {
            base_color: Color::linear_rgb(0.0, 1.0, 0.0),
            emissive: LinearRgba::new(0.0, 1.0, 0.0, 1.0) * 2.0,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        });

        // Foreground bar (colored, sits slightly in front of background).
        commands.entity(entity).with_child((
            HealthBarFg,
            Mesh3d(assets.bar_mesh.clone()),
            MeshMaterial3d(fg_material),
            Transform::from_xyz(0.0, HEALTH_BAR_Y_OFFSET, 0.0).with_scale(Vec3::new(
                HEALTH_BAR_WIDTH,
                HEALTH_BAR_HEIGHT,
                1.0,
            )),
        ));
    }
}

/// Remove health bar children from units that are no longer selected.
pub fn despawn_health_bars(
    mut removed_selections: RemovedComponents<Selected>,
    bg_bars: Query<(Entity, &ChildOf), With<HealthBarBg>>,
    fg_bars: Query<(Entity, &ChildOf), With<HealthBarFg>>,
    mut commands: Commands,
) {
    for unit in removed_selections.read() {
        for (bar_entity, child_of) in bg_bars.iter().chain(fg_bars.iter()) {
            if child_of.parent() == unit {
                commands.entity(bar_entity).despawn();
            }
        }
    }
}

/// Update health bar scale and color each frame for selected units.
pub fn update_health_bars(
    selected_units: Query<&Health, With<Selected>>,
    mut fg_bars: Query<
        (&ChildOf, &mut Transform, &MeshMaterial3d<StandardMaterial>),
        With<HealthBarFg>,
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (child_of, mut transform, mat_handle) in &mut fg_bars {
        let Ok(health) = selected_units.get(child_of.parent()) else {
            continue;
        };

        let frac = health.fraction().clamp(0.0, 1.0);

        transform.scale.x = HEALTH_BAR_WIDTH * frac;
        transform.translation.x = -HEALTH_BAR_WIDTH * (1.0 - frac) * 0.5;

        let color = health_color(frac);
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            mat.base_color = color;
            mat.emissive = LinearRgba::from(color) * 2.0;
        }
    }
}

/// Make health bars always face the camera (billboard).
///
/// The bar mesh is a Plane3d in XY with normal along +Z. We rotate so that
/// +Z points toward the camera, keeping the bar upright (Y stays world-up).
#[allow(clippy::type_complexity)]
pub fn billboard_health_bars(
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    parents: Query<&GlobalTransform, With<UnitType>>,
    mut bars: Query<
        (&ChildOf, &mut Transform, Option<&HealthBarFg>),
        Or<(With<HealthBarBg>, With<HealthBarFg>)>,
    >,
) {
    let Ok(cam_gt) = camera_q.single() else {
        return;
    };

    for (child_of, mut transform, is_fg) in &mut bars {
        let Ok(parent_gt) = parents.get(child_of.parent()) else {
            continue;
        };

        let bar_world_pos = parent_gt.translation() + Vec3::Y * HEALTH_BAR_Y_OFFSET;
        let to_camera = (cam_gt.translation() - bar_world_pos).normalize_or(Vec3::Z);

        // Compute a world-space rotation that faces +Z toward the camera,
        // keeping Y as the up direction.
        let world_rot = Quat::from_rotation_arc(Vec3::Z, to_camera);

        // Convert to parent-local rotation.
        let parent_rot_inv = parent_gt.to_scale_rotation_translation().1.inverse();
        transform.rotation = parent_rot_inv * world_rot;

        // Push foreground bar slightly toward the camera to avoid z-fighting.
        if is_fg.is_some() {
            transform.translation =
                Vec3::new(0.0, HEALTH_BAR_Y_OFFSET, 0.0) + (parent_rot_inv * to_camera) * 0.2;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_unit_hit(
    hits: &[(Entity, bevy::picking::mesh_picking::ray_cast::RayMeshHit)],
    unit_q: &Query<Entity, With<UnitType>>,
    volume_q: &Query<&ChildOf, With<SelectionVolume>>,
) -> Option<Entity> {
    hits.iter().find_map(|(entity, _)| {
        if unit_q.contains(*entity) {
            return Some(*entity);
        }
        if let Ok(child_of) = volume_q.get(*entity) {
            let parent = child_of.parent();
            if unit_q.contains(parent) {
                return Some(parent);
            }
        }
        None
    })
}

fn cursor_ray(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
) -> Option<Ray3d> {
    let window = windows.single().ok()?;
    let cursor_pos = window.cursor_position()?;
    let (camera, camera_transform) = camera_q.single().ok()?;
    camera.viewport_to_world(camera_transform, cursor_pos).ok()
}

fn get_or_init_bar_assets(
    existing: Option<Res<HealthBarAssets>>,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> HealthBarAssets {
    if let Some(res) = existing {
        return res.into_inner().clone();
    }

    let assets = HealthBarAssets {
        bar_mesh: meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.5, 0.5))),
        bg_material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.0, 0.0, 0.0, 0.6),
            emissive: LinearRgba::NONE,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
    };
    let cloned = assets.clone();
    commands.insert_resource(assets);
    cloned
}

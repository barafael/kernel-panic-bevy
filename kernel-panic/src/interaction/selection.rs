use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use crate::rendering::camera::RtsCamera;
use crate::units::components::{SelectionVolume, UnitType};

use super::movement::MoveTarget;

/// Marker for temporary move-target indicators on the ground.
#[derive(Component)]
pub struct MoveIndicator {
    pub lifetime: Timer,
}

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

/// Visual ring shown under selected units.
#[derive(Component)]
pub struct SelectionRing;

/// Visual ring shown under the hovered unit.
#[derive(Component)]
pub struct HoverRing;

/// Shared mesh and material assets for selection/hover rings.
#[derive(Resource, Clone)]
pub(crate) struct RingAssets {
    mesh: Handle<Mesh>,
    selection_material: Handle<StandardMaterial>,
    hover_material: Handle<StandardMaterial>,
}

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
/// - **Plain click/drag** — replace the current selection.
/// - **Shift+click/drag** — add to the current selection.
/// - **Ctrl+click** — toggle the clicked unit in/out of the selection.
#[allow(clippy::too_many_arguments)]
pub fn handle_selection(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    hovered_q: Query<Entity, With<Hovered>>,
    selected_q: Query<Entity, With<Selected>>,
    ring_q: Query<(Entity, &ChildOf), With<SelectionRing>>,
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
    if mouse.pressed(MouseButton::Left) {
        if let (Some(start), Some(current)) = (drag_state.start, cursor_pos) {
            let distance = (current - start).length();
            if distance > DRAG_THRESHOLD {
                drag_state.dragging = true;

                // Update or spawn the selection box UI node.
                let min_x = start.x.min(current.x);
                let min_y = start.y.min(current.y);
                let width = (current.x - start.x).abs();
                let height = (current.y - start.y).abs();

                // Remove old box node.
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
    }

    // --- Left release ---
    if mouse.just_released(MouseButton::Left) {
        // Remove selection box visual.
        for entity in &box_nodes {
            commands.entity(entity).despawn();
        }

        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        let ctrl = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
        let additive = shift || ctrl;

        // Unless Shift/Ctrl is held, clear previous selection first.
        if !additive {
            for entity in &selected_q {
                commands.entity(entity).remove::<Selected>();
            }
            for (entity, _) in &ring_q {
                commands.entity(entity).despawn();
            }
        }

        if drag_state.dragging {
            // Box select: find all units whose screen position is inside the box.
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
            // Ctrl+click: toggle the hovered unit.
            if let Some(entity) = hovered_q.iter().next() {
                if selected_q.contains(entity) {
                    commands.entity(entity).remove::<Selected>();
                    // Despawn the selection ring that is a child of this entity.
                    despawn_ring_for(&ring_q, entity, &mut commands);
                } else {
                    commands.entity(entity).insert(Selected);
                }
            }
        } else {
            // Plain click or Shift+click: select the hovered unit (additive keeps existing).
            if let Some(entity) = hovered_q.iter().next() {
                commands.entity(entity).insert(Selected);
            }
        }

        drag_state.start = None;
        drag_state.dragging = false;
    }
}

/// Despawn any `SelectionRing` that is a child of `unit`.
fn despawn_ring_for(
    ring_q: &Query<(Entity, &ChildOf), With<SelectionRing>>,
    unit: Entity,
    commands: &mut Commands,
) {
    for (ring, child_of) in ring_q.iter() {
        if child_of.parent() == unit {
            commands.entity(ring).despawn();
        }
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

/// Event requesting move indicator visuals at specific world positions.
#[derive(Message)]
pub struct SpawnMoveIndicatorsEvent(pub Vec<Vec3>);

/// Spawn move indicator meshes from events (separate system to avoid resource conflicts).
pub fn spawn_move_indicator_visuals(
    mut events: MessageReader<SpawnMoveIndicatorsEvent>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for event in events.read() {
        spawn_move_indicators(&event.0, &mut commands, &mut meshes, &mut materials);
    }
}

/// Right-click: single click moves all selected to one point.
/// Right-drag: sample a path, distribute selected units along it on release.
pub fn handle_right_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    selected_q: Query<Entity, With<Selected>>,
    mut commands: Commands,
    mut indicator_events: MessageWriter<SpawnMoveIndicatorsEvent>,
    mut drag_path: ResMut<RightDragPath>,
) {
    // --- Right press: start path ---
    if mouse.just_pressed(MouseButton::Right) {
        drag_path.points.clear();
        drag_path.active = true;

        if let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast) {
            drag_path.points.push(point);
        }
    }

    // --- While held: sample path points ---
    if mouse.pressed(MouseButton::Right) && drag_path.active {
        if let Some(point) = ground_hit(&windows, &camera_q, &mut ray_cast) {
            let dominated = drag_path
                .points
                .last()
                .is_some_and(|last| last.distance(point) < PATH_SAMPLE_MIN_DISTANCE);
            if !dominated {
                drag_path.points.push(point);
            }
        }
    }

    // --- Right release: issue commands ---
    if mouse.just_released(MouseButton::Right) && drag_path.active {
        drag_path.active = false;

        let units: Vec<Entity> = selected_q.iter().collect();
        if units.is_empty() || drag_path.points.is_empty() {
            return;
        }

        let assigned_targets = if drag_path.points.len() == 1 {
            let target = drag_path.points[0];
            for entity in &units {
                commands.entity(*entity).insert(MoveTarget(target));
            }
            vec![target]
        } else {
            let targets = sample_path_evenly(&drag_path.points, units.len());
            for (entity, target) in units.iter().zip(targets.iter()) {
                commands.entity(*entity).insert(MoveTarget(*target));
            }
            targets
        };

        // Emit event for visual indicator spawning (handled by separate system).
        indicator_events.write(SpawnMoveIndicatorsEvent(assigned_targets));

        drag_path.points.clear();
    }
}

/// Spawn small glowing markers at each target point that fade after 1.5 seconds.
fn spawn_move_indicators(
    targets: &[Vec3],
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let mesh = meshes.add(Torus::new(2.0, 4.0));
    let material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.0, 1.0, 0.3, 0.6),
        emissive: LinearRgba::new(0.0, 1.0, 0.3, 1.0) * 2.0,
        unlit: true,
        alpha_mode: AlphaMode::Blend,
        ..default()
    });

    for target in targets {
        commands.spawn((
            MoveIndicator {
                lifetime: Timer::from_seconds(1.5, TimerMode::Once),
            },
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(*target + Vec3::Y * 1.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
    }
}

/// Cast a ray from the cursor to the ground and return the hit point.
fn ground_hit(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut MeshRayCast,
) -> Option<Vec3> {
    let ray = cursor_ray(windows, camera_q)?;
    let hits = ray_cast.cast_ray(ray, &default());
    hits.first().map(|(_, hit)| hit.point)
}

/// Sample `count` points evenly distributed along a polyline path.
fn sample_path_evenly(path: &[Vec3], count: usize) -> Vec<Vec3> {
    if count == 0 || path.is_empty() {
        return vec![];
    }
    if count == 1 {
        // Single unit: place at the end of the path.
        return vec![*path.last().unwrap()];
    }

    // Compute cumulative distances along the path.
    let mut cumulative = vec![0.0f32];
    for i in 1..path.len() {
        let prev = cumulative[i - 1];
        cumulative.push(prev + path[i - 1].distance(path[i]));
    }
    let total_length = *cumulative.last().unwrap();

    if total_length < 0.01 {
        return vec![path[0]; count];
    }

    // Sample `count` equidistant points along the path.
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let target_dist = (i as f32 / (count - 1) as f32) * total_length;

        // Find the segment containing this distance.
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

/// Spawn a visual ring under newly-selected units.
pub fn spawn_selection_rings(
    new_selections: Query<Entity, Added<Selected>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    ring_assets: Option<Res<RingAssets>>,
) {
    if new_selections.is_empty() {
        return;
    }

    let assets = get_or_init_ring_assets(ring_assets, &mut commands, &mut meshes, &mut materials);

    for entity in &new_selections {
        commands.entity(entity).with_child((
            SelectionRing,
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.selection_material.clone()),
            Transform::from_xyz(0.0, -1.0, 0.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
    }
}

/// Spawn a dim ring under the newly-hovered unit, remove when unhovered.
pub fn update_hover_ring(
    new_hovers: Query<Entity, Added<Hovered>>,
    selected_q: Query<&Selected>,
    hover_rings: Query<Entity, With<HoverRing>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    ring_assets: Option<Res<RingAssets>>,
) {
    // Remove old hover rings.
    for entity in &hover_rings {
        commands.entity(entity).despawn();
    }

    // Only proceed if there is a non-selected hovered unit.
    let needs_ring = new_hovers.iter().any(|e| !selected_q.contains(e));
    if !needs_ring {
        return;
    }

    let assets = get_or_init_ring_assets(ring_assets, &mut commands, &mut meshes, &mut materials);

    for entity in &new_hovers {
        if selected_q.contains(entity) {
            continue;
        }

        commands.entity(entity).with_child((
            HoverRing,
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.hover_material.clone()),
            Transform::from_xyz(0.0, -1.0, 0.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
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

fn get_or_init_ring_assets(
    existing: Option<Res<RingAssets>>,
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) -> RingAssets {
    if let Some(res) = existing {
        return res.into_inner().clone();
    }

    let assets = RingAssets {
        mesh: meshes.add(Torus::new(18.0, 22.0)),
        selection_material: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 1.0, 1.0, 0.5),
            emissive: LinearRgba::new(1.0, 1.0, 1.0, 1.0) * 3.0,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        hover_material: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 1.0, 1.0, 0.2),
            emissive: LinearRgba::new(1.0, 1.0, 1.0, 1.0),
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
    };
    let cloned = assets.clone();
    commands.insert_resource(assets);
    cloned
}

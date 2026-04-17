//! Right-click movement orders: single-click moves the selection to a point,
//! right-drag samples a path and distributes units along it. Shows a
//! formation preview during the drag and a move-target torus on release.

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use super::core::{Selected, SelectionSet, cursor_ray};
use crate::interaction::movement::{CommandQueue, MovePath, MoveTarget, QueuedCommand};
use crate::rendering::camera::RtsCamera;

pub(super) struct RightClickPlugin;

impl Plugin for RightClickPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RightDragPath>()
            .init_resource::<PendingMoveIndicators>()
            .add_systems(
                Update,
                (
                    handle_right_click,
                    spawn_move_indicator_visuals.after(handle_right_click),
                    update_formation_preview.after(handle_right_click),
                    decay_move_indicators,
                )
                    .in_set(SelectionSet::RightClick),
            );
    }
}

/// Marker for temporary move-target indicators on the ground.
#[derive(Component)]
pub struct MoveIndicator {
    pub lifetime: Timer,
}

/// Marker for ephemeral formation preview dots shown during right-drag.
#[derive(Component)]
pub struct FormationPreview;

/// Despawn move indicators after their lifetime expires.
fn decay_move_indicators(
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
fn handle_right_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    selected_q: Query<(Entity, &Transform), With<Selected>>,
    move_target_q: Query<(), With<MoveTarget>>,
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

        let mut units: Vec<(Entity, Vec3)> = selected_q
            .iter()
            .map(|(e, tf)| (e, tf.translation))
            .collect();
        if units.is_empty() || drag_path.points.is_empty() {
            return;
        }

        let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);

        if drag_path.points.len() == 1 {
            // Single-point move: every unit goes to the same point — no
            // ordering question to answer.
            let target = drag_path.points[0];
            for (entity, _) in &units {
                apply_ordered_command(
                    *entity,
                    QueuedCommand::Move(target),
                    shift,
                    &move_target_q,
                    &mut commands,
                );
            }
            pending.targets.push(target);
        } else {
            // Path-based formation: sort units by projection onto the drag's
            // principal axis (start → end) so the nearest-to-start unit gets
            // the first target. This keeps movement lines roughly parallel
            // instead of letting arbitrary ECS ordering cause paths to cross.
            let path_start = *drag_path.points.first().unwrap();
            let path_end = *drag_path.points.last().unwrap();
            let axis = Vec3::new(path_end.x - path_start.x, 0.0, path_end.z - path_start.z);
            let axis_len_sq = axis.length_squared();

            if axis_len_sq > 0.01 {
                units.sort_by(|(_, a), (_, b)| {
                    let pa = (Vec3::new(a.x - path_start.x, 0.0, a.z - path_start.z)).dot(axis);
                    let pb = (Vec3::new(b.x - path_start.x, 0.0, b.z - path_start.z)).dot(axis);
                    pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                });
            }

            let targets = sample_path_evenly(&drag_path.points, units.len());
            for ((entity, _), target) in units.iter().zip(targets.iter()) {
                apply_ordered_command(
                    *entity,
                    QueuedCommand::Move(*target),
                    shift,
                    &move_target_q,
                    &mut commands,
                );
            }
            pending.targets.extend(targets);
        }

        drag_path.points.clear();
    }
}

/// Apply a positional command to a unit, either replacing its current order
/// or enqueuing it behind the currently running order (and any already queued).
pub(crate) fn apply_ordered_command(
    entity: Entity,
    cmd: QueuedCommand,
    enqueue: bool,
    move_target_q: &Query<(), With<MoveTarget>>,
    commands: &mut Commands,
) {
    if enqueue && move_target_q.contains(entity) {
        // Unit has an active order — append to (or create) the queue.
        commands
            .entity(entity)
            .entry::<CommandQueue>()
            .or_default()
            .and_modify(move |mut queue: Mut<CommandQueue>| {
                queue.push(cmd);
            });
    } else {
        // Replace (or enqueue with no active order): install as active order,
        // reset the queue, and invalidate any computed path.
        commands
            .entity(entity)
            .insert(MoveTarget(cmd.position()))
            .insert(CommandQueue::default())
            .remove::<MovePath>();
    }
}

/// Drains `PendingMoveIndicators` and creates torus visuals.
///
/// Separated from `handle_right_click` because `MeshRayCast` holds an
/// immutable `Res<Assets<Mesh>>` that conflicts with `ResMut<Assets<Mesh>>`.
fn spawn_move_indicator_visuals(
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
fn update_formation_preview(
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

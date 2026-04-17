//! Core selection state: hover detection, left-click + drag-box selection,
//! and the resolve-unit-under-cursor logic shared across the sub-module.

use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, RayMeshHit};
use bevy::prelude::*;

use crate::rendering::camera::RtsCamera;
use crate::units::components::{SelectionVolume, UnitType};

pub(super) struct SelectionCorePlugin;

impl Plugin for SelectionCorePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DragState>()
            .configure_sets(
                Update,
                (
                    SelectionSet::Hover,
                    SelectionSet::Select,
                    SelectionSet::RightClick,
                    SelectionSet::Visuals,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    update_hover.in_set(SelectionSet::Hover),
                    handle_selection.in_set(SelectionSet::Select),
                ),
            );
    }
}

/// Ordered phases of per-frame selection work. Each phase runs in order; other
/// modules reference these sets so their systems can run at the right time
/// without naming internal functions.
#[derive(SystemSet, Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum SelectionSet {
    /// Resolve the unit under the cursor.
    Hover,
    /// Process left-click + drag-box selection.
    Select,
    /// Process right-click movement orders (runs in `right_click` module).
    RightClick,
    /// Apply visual highlights / bars (runs in `highlight` and `health_bars`).
    Visuals,
}

/// Marks a unit as selected by the player.
#[derive(Component)]
pub struct Selected;

/// Marks the unit currently under the cursor.
#[derive(Component)]
pub struct Hovered;

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

/// Update `Hovered` component each frame based on cursor position.
fn update_hover(
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

/// Handle left-click and drag-box selection.
///
/// Modifier behaviour (matches original Kernel Panic):
/// - **Plain click/drag** -- replace the current selection.
/// - **Shift+click/drag** -- add to the current selection.
/// - **Ctrl+click** -- toggle the clicked unit in/out of the selection.
#[allow(clippy::too_many_arguments)]
fn handle_selection(
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
        } else if let Some(entity) = hovered_q.iter().next() {
            commands.entity(entity).insert(Selected);
        }

        drag_state.start = None;
        drag_state.dragging = false;
    }
}

pub(super) fn resolve_unit_hit(
    hits: &[(Entity, RayMeshHit)],
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

pub(crate) fn cursor_ray(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
) -> Option<Ray3d> {
    let window = windows.single().ok()?;
    let cursor_pos = window.cursor_position()?;
    let (camera, camera_transform) = camera_q.single().ok()?;
    camera.viewport_to_world(camera_transform, cursor_pos).ok()
}

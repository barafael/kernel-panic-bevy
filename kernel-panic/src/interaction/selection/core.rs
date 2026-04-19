//! Core selection state: hover detection, left-click + drag-box selection,
//! and the resolve-unit-under-cursor logic shared across the sub-module.

use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, RayMeshHit};
use bevy::prelude::*;

use crate::rendering::camera::RtsCamera;
use crate::units::components::UnitType;

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
    /// True when the press that opened the current mouse-down happened
    /// over a UI button. The release then skips world-space selection so
    /// clicking a build icon doesn't also deselect the constructor the
    /// click was targeting. Cleared on the matching release.
    started_on_ui: bool,
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
    parent_q: Query<&ChildOf>,
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
    if let Some(entity) = resolve_unit_hit(hits, &unit_q, &parent_q) {
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
    ui_interactions: Query<&Interaction>,
    attack_mode: Res<crate::interaction::ability::AttackGroundMode>,
    mut drag_state: ResMut<DragState>,
    mut commands: Commands,
) {
    let cursor_pos = windows.single().ok().and_then(|w| w.cursor_position());

    // Ground-attack mode: while the `A`-toggle is active, the click
    // belongs to the attack-ground hotkey (see
    // `ability::trigger_attack_ground_click`) and must not flow into
    // selection — otherwise the release would clear the selection
    // before the attack-ground command could dispatch.
    let attack_mode_active = attack_mode.active;

    // --- Left press: start tracking ---
    if mouse.just_pressed(MouseButton::Left) {
        // If the press landed on a UI button (build icon, order palette,
        // etc.), swallow the whole click cycle — otherwise the matching
        // release later drops `Selected` off every unit and the click on
        // the build icon also deselects the constructor it was meant
        // to command. `Interaction::Pressed` is set on the UI node for
        // exactly the frame the button is pressed-and-held, which makes
        // this a cheap O(button-count) scan.
        let on_ui = ui_interactions
            .iter()
            .any(|i| *i == Interaction::Pressed || *i == Interaction::Hovered);
        drag_state.started_on_ui = on_ui || attack_mode_active;
        if on_ui || attack_mode_active {
            return;
        }
        drag_state.start = cursor_pos;
        drag_state.dragging = false;
    }

    if drag_state.started_on_ui {
        if mouse.just_released(MouseButton::Left) {
            drag_state.started_on_ui = false;
        }
        return;
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
    parent_q: &Query<&ChildOf>,
) -> Option<Entity> {
    // A ray can land on the unit root, its invisible selection-volume
    // sphere, or any visible S3O piece — which can be nested several
    // levels deep via COB piece parenting. Walk up the hierarchy from
    // whatever we hit until we find an ancestor with `UnitType`.
    hits.iter().find_map(|(entity, _)| {
        let mut cur = *entity;
        loop {
            if unit_q.contains(cur) {
                return Some(cur);
            }
            match parent_q.get(cur) {
                Ok(child_of) => cur = child_of.parent(),
                Err(_) => return None,
            }
        }
    })
}

pub(super) fn cursor_ray(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
) -> Option<Ray3d> {
    let window = windows.single().ok()?;
    let cursor_pos = window.cursor_position()?;
    let (camera, camera_transform) = camera_q.single().ok()?;
    camera.viewport_to_world(camera_transform, cursor_pos).ok()
}

/// Cast a ray from the cursor into the world and return the first
/// mesh hit. Used by right-click orders and ability targeting.
pub(crate) fn ground_hit(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut MeshRayCast,
) -> Option<Vec3> {
    let ray = cursor_ray(windows, camera_q)?;
    let hits = ray_cast.cast_ray(ray, &default());
    hits.first().map(|(_, hit)| hit.point)
}

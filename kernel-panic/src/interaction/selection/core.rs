//! Core selection state: hover detection, left-click + drag-box selection,
//! and the resolve-unit-under-cursor logic shared across the sub-module.

use bevy::ecs::system::SystemParam;
use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayMeshHit};
use bevy::prelude::*;

use crate::map_loading::TerrainChunkMarker;
use crate::rendering::camera::RtsCamera;
use crate::units::assets::animation::PieceIndex;
use crate::units::components::{SelectionVolume, TeamId, UnitType};

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
            // No picking or ordering in the menu: clicks there belong to
            // the menu buttons, and the demo behind it is all-AI.
            .configure_sets(
                Update,
                (SelectionSet::Hover, SelectionSet::Select, SelectionSet::RightClick)
                    .run_if(in_state(crate::game_setup::AppState::InGame)),
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

/// Window for double-click recognition (seconds). Standard OS default
/// is ~500 ms; we run tighter so a slow second click doesn't surprise
/// the user with a select-all.
const DOUBLE_CLICK_INTERVAL: f32 = 0.30;

/// Tracks drag state for box selection.
#[derive(Resource, Default)]
pub struct DragState {
    /// Screen position where left mouse was pressed.
    start: Option<Vec2>,
    /// Whether we're actively dragging (past threshold).
    dragging: bool,
    /// True when the press that opened the current mouse-down belonged to
    /// an armed order cursor mode. The release then skips world-space
    /// selection so the order click doesn't also clear the selection it
    /// was issued to. Cleared on the matching release.
    swallowed: bool,
    /// (timestamp, entity) of the last click that landed on a unit. A
    /// second click on the same entity within
    /// [`DOUBLE_CLICK_INTERVAL`] expands the selection to all visible
    /// units of the same `(UnitType, TeamId)` on screen.
    last_click: Option<(f32, Entity)>,
}

/// Visual overlay for the selection box.
#[derive(Component)]
pub struct SelectionBoxNode;

/// Cursor ray cast restricted to what selection actually targets: unit
/// picking spheres, unit model pieces and terrain chunks. Everything
/// else with a mesh — health bars, move rings, shield shells, CEG
/// particles, hex-farm overlays, formation previews — is skipped before
/// its triangles are tested. Pieces stay in because several models poke
/// out of their sphere (a Bit's ball reaches y=33 over an r=16 sphere
/// centred at y=8; the Obelisk is 86 tall on r=35), and ground points
/// come from the heightmap chunks, which Hex Farm rebuilds in step with
/// its tower overlays.
#[derive(SystemParam)]
pub(crate) struct PickRayCast<'w, 's> {
    ray_cast: MeshRayCast<'w, 's>,
    pickable: Query<
        'w,
        's,
        (),
        Or<(
            With<SelectionVolume>,
            With<PieceIndex>,
            With<TerrainChunkMarker>,
        )>,
    >,
}

impl PickRayCast<'_, '_> {
    /// Nearest-first hits of `ray` against units and terrain.
    pub(crate) fn cast(&mut self, ray: Ray3d) -> &[(Entity, RayMeshHit)] {
        let Self { ray_cast, pickable } = self;
        let filter = |e: Entity| pickable.contains(e);
        ray_cast.cast_ray(ray, &MeshRayCastSettings::default().with_filter(&filter))
    }
}

/// What the last hover cast saw, so a still cursor over a still camera
/// doesn't re-cast every frame.
#[derive(Default)]
struct HoverCache {
    cursor: Option<Vec2>,
    camera: Option<GlobalTransform>,
    /// Frames since the last cast; units moving under a still cursor are
    /// picked up when this reaches [`HOVER_RECAST_FRAMES`].
    frames_since_cast: u32,
}

/// Re-cast at least this often even when nothing on our side moved.
const HOVER_RECAST_FRAMES: u32 = 4;

/// Update the `Hovered` component from the cursor position.
///
/// Casts only when the cursor or camera moved (or every
/// [`HOVER_RECAST_FRAMES`] frames), and only touches `Hovered` when the
/// unit under the cursor changes — a steady hover is free of archetype
/// moves.
fn update_hover(
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: PickRayCast,
    unit_q: Query<Entity, With<UnitType>>,
    parent_q: Query<&ChildOf>,
    hovered_q: Query<Entity, With<Hovered>>,
    mut cache: Local<HoverCache>,
    mut commands: Commands,
) {
    let cursor = windows.single().ok().and_then(|w| w.cursor_position());
    let camera = camera_q.single().ok().map(|(_, gt)| *gt);
    let unchanged = cursor == cache.cursor && camera == cache.camera;
    if unchanged && cursor.is_some() && cache.frames_since_cast < HOVER_RECAST_FRAMES {
        cache.frames_since_cast += 1;
        return;
    }
    cache.cursor = cursor;
    cache.camera = camera;
    cache.frames_since_cast = 0;

    let target = cursor_ray(&windows, &camera_q)
        .and_then(|ray| resolve_unit_hit(ray_cast.cast(ray), &unit_q, &parent_q));

    let mut already = false;
    for entity in &hovered_q {
        if Some(entity) == target {
            already = true;
        } else {
            commands.entity(entity).remove::<Hovered>();
        }
    }
    if let Some(entity) = target
        && !already
    {
        commands.entity(entity).insert(Hovered);
    }
}

/// Handle left-click and drag-box selection.
///
/// Modifier behaviour (matches original Kernel Panic):
/// - **Plain click/drag** -- replace the current selection.
/// - **Shift+click/drag** -- add to the current selection.
/// - **Ctrl+click** -- toggle the clicked unit in/out of the selection.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_selection(
    time: Res<Time>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    hovered_q: Query<Entity, With<Hovered>>,
    selected_q: Query<Entity, With<Selected>>,
    unit_q: Query<(Entity, &GlobalTransform, &TeamId), With<UnitType>>,
    kind_team_q: Query<(&UnitType, &TeamId)>,
    local: Res<crate::units::player::LocalTeam>,
    same_kind_q: Query<(Entity, &UnitType, &TeamId, &GlobalTransform, &Visibility)>,
    mut box_nodes: Query<&mut Node, With<SelectionBoxNode>>,
    modes: Res<crate::interaction::ability::OrderCursorModes>,
    mut drag_state: ResMut<DragState>,
    mut commands: Commands,
) {
    let cursor_pos = windows.single().ok().and_then(|w| w.cursor_position());

    // While any order cursor mode is armed (attack-ground, attack-move,
    // or patrol), the left-click belongs to that mode's click handler and
    // must not flow into selection — otherwise the release would clear the
    // selection before the order could dispatch.
    let cursor_mode_active = modes.any_active();

    // --- Left press: start tracking ---
    if mouse.just_pressed(MouseButton::Left) {
        drag_state.swallowed = cursor_mode_active;
        if cursor_mode_active {
            return;
        }
        drag_state.start = cursor_pos;
        drag_state.dragging = false;
    }

    if drag_state.swallowed {
        if mouse.just_released(MouseButton::Left) {
            drag_state.swallowed = false;
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

            // One overlay node, spawned on the first drag and reshaped
            // in place afterwards (`Display::None` between drags) — a UI
            // node respawned every frame relayouts the whole tree.
            let layout = Node {
                position_type: PositionType::Absolute,
                left: Val::Px(min_x),
                top: Val::Px(min_y),
                width: Val::Px(width),
                height: Val::Px(height),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            };
            match box_nodes.single_mut() {
                Ok(mut node) => {
                    node.left = layout.left;
                    node.top = layout.top;
                    node.width = layout.width;
                    node.height = layout.height;
                    node.display = Display::DEFAULT;
                }
                Err(_) => {
                    commands.spawn((
                        SelectionBoxNode,
                        layout,
                        BorderColor::all(Color::WHITE),
                        BackgroundColor(Color::NONE),
                    ));
                }
            }
        }
    }

    // Only the local player's own units can be selected (and so
    // ordered) — clicking or boxing another team's units just clears
    // the selection, as in Spring. Co-op control would widen this.
    let own = |e: Entity| kind_team_q.get(e).is_ok_and(|(_, t)| t.0 == local.0);

    // --- Left release ---
    if mouse.just_released(MouseButton::Left) {
        for mut node in &mut box_nodes {
            if node.display != Display::None {
                node.display = Display::None;
            }
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
                    for (entity, global_transform, team) in &unit_q {
                        if team.0 != local.0 {
                            continue;
                        }
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
            if let Some(entity) = hovered_q.iter().next().filter(|e| own(*e)) {
                if selected_q.contains(entity) {
                    commands.entity(entity).remove::<Selected>();
                } else {
                    commands.entity(entity).insert(Selected);
                }
            }
        } else if let Some(entity) = hovered_q.iter().next().filter(|e| own(*e)) {
            commands.entity(entity).insert(Selected);

            // Double-click: expand selection to every visible unit of the
            // same `(UnitType, TeamId)` currently on screen. The same-team
            // filter avoids the surprising "click your bug, get every
            // bug on the map (including AI's)" outcome.
            let now = time.elapsed_secs();
            let prior_was_double = drag_state
                .last_click
                .is_some_and(|(t, e)| e == entity && (now - t) <= DOUBLE_CLICK_INTERVAL);
            if prior_was_double
                && let Ok((kind, team)) = kind_team_q.get(entity)
                && let Ok((camera, camera_transform)) = camera_q.single()
                && let Some(window) = windows.single().ok()
            {
                let win_size = Vec2::new(window.width(), window.height());
                for (other, other_kind, other_team, gtf, vis) in &same_kind_q {
                    if other_kind.0 != kind.0 || other_team.0 != team.0 {
                        continue;
                    }
                    if matches!(*vis, Visibility::Hidden) {
                        continue;
                    }
                    let Ok(screen_pos) =
                        camera.world_to_viewport(camera_transform, gtf.translation())
                    else {
                        continue;
                    };
                    if screen_pos.x >= 0.0
                        && screen_pos.x <= win_size.x
                        && screen_pos.y >= 0.0
                        && screen_pos.y <= win_size.y
                    {
                        commands.entity(other).insert(Selected);
                    }
                }
                // Reset so a third click in quick succession doesn't
                // re-trigger the expansion (already at maximum scope).
                drag_state.last_click = None;
            } else {
                drag_state.last_click = Some((now, entity));
            }
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
    // A ray lands on a unit's selection-volume sphere (a child of the
    // unit root; the flat-mesh fallback carries the mesh on the root
    // itself). Walk up the hierarchy from whatever we hit until we find
    // an ancestor with `UnitType`.
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
/// pickable hit (unit sphere or terrain). Used by right-click orders
/// and ability targeting.
pub(crate) fn ground_hit(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut PickRayCast,
) -> Option<Vec3> {
    let ray = cursor_ray(windows, camera_q)?;
    ray_cast.cast(ray).first().map(|(_, hit)| hit.point)
}

/// Like [`ground_hit`], but only meshes passing `filter` are considered.
/// Used by the placement ghost, which must hit terrain only — an
/// unfiltered cast would hit the ghost's own (translucent) mesh sitting
/// exactly on the cursor ray, freezing the preview at its spawn point
/// instead of following the cursor.
pub(crate) fn ground_hit_filtered(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut MeshRayCast,
    filter: impl Fn(Entity) -> bool,
) -> Option<Vec3> {
    let ray = cursor_ray(windows, camera_q)?;
    let settings = MeshRayCastSettings::default().with_filter(&filter);
    ray_cast
        .cast_ray(ray, &settings)
        .first()
        .map(|(_, hit)| hit.point)
}

/// Cast a ray from the cursor and resolve the first *unit* it lands on
/// (walking up S3O piece hierarchy to the unit root). Returns `None` when
/// the ray hits only terrain / nothing at all. Used by right-click attack
/// orders and guard targeting.
#[allow(clippy::type_complexity)]
pub(crate) fn unit_hit(
    windows: &Query<&Window>,
    camera_q: &Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    ray_cast: &mut PickRayCast,
    unit_q: &Query<Entity, With<UnitType>>,
    parent_q: &Query<&ChildOf>,
) -> Option<Entity> {
    let ray = cursor_ray(windows, camera_q)?;
    resolve_unit_hit(ray_cast.cast(ray), unit_q, parent_q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::camera::CameraPlugin;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::transform::TransformPlugin;
    use bevy::MinimalPlugins;

    /// The selection volume is a `Mesh3d` with no material: Bevy's
    /// visibility pass must still give it an `Aabb` and mark it
    /// `ViewVisibility`, and the filtered cast must hit it through a
    /// hidden-root check (a cloaked unit's sphere stays unpickable).
    #[test]
    fn materialless_selection_volume_is_picked() {
        // `cast_ray` culls candidates with `par_iter`.
        bevy::tasks::ComputeTaskPool::get_or_init(bevy::tasks::TaskPool::new);
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, TransformPlugin, CameraPlugin))
            .insert_resource(Assets::<Mesh>::default());

        let sphere = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Sphere::new(10.0).mesh().ico(3).unwrap());
        app.world_mut().spawn((
            Camera3d::default(),
            Transform::from_xyz(0.0, 200.0, 0.0).looking_at(Vec3::ZERO, Vec3::Z),
        ));
        let spawn_unit = |world: &mut World, x: f32, vis: Visibility| {
            let unit = world
                .spawn((UnitType(crate::units::content::definitions::UnitKind::Bit), Transform::from_xyz(x, 0.0, 0.0), vis))
                .id();
            world.spawn((
                SelectionVolume,
                Mesh3d(sphere.clone()),
                Transform::from_xyz(0.0, 5.0, 0.0),
                ChildOf(unit),
            ));
            unit
        };
        let visible = spawn_unit(app.world_mut(), 0.0, Visibility::Inherited);
        let cloaked = spawn_unit(app.world_mut(), 100.0, Visibility::Hidden);
        // One frame: transform propagation, bounds, frusta, visibility.
        app.update();

        let hit = |world: &mut World, x: f32| {
            world
                .run_system_once(
                    move |mut ray_cast: PickRayCast,
                          unit_q: Query<Entity, With<UnitType>>,
                          parent_q: Query<&ChildOf>| {
                        let ray = Ray3d::new(Vec3::new(x, 100.0, 0.0), Dir3::NEG_Y);
                        resolve_unit_hit(ray_cast.cast(ray), &unit_q, &parent_q)
                    },
                )
                .unwrap()
        };
        assert_eq!(hit(app.world_mut(), 0.0), Some(visible));
        assert_eq!(hit(app.world_mut(), 100.0), None, "hidden root: {cloaked:?} unpickable");
        assert_eq!(hit(app.world_mut(), 50.0), None);
    }
}

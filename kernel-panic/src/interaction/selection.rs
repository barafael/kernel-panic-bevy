use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use crate::rendering::camera::RtsCamera;
use crate::units::components::{SelectionVolume, UnitType};

use super::movement::MoveTarget;

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

/// Left-click: select a unit (or deselect by clicking terrain).
pub fn handle_selection(
    mouse: Res<ButtonInput<MouseButton>>,
    hovered_q: Query<Entity, With<Hovered>>,
    selected_q: Query<Entity, With<Selected>>,
    ring_q: Query<Entity, With<SelectionRing>>,
    mut commands: Commands,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }

    // Clear previous selection.
    for entity in &selected_q {
        commands.entity(entity).remove::<Selected>();
    }
    for entity in &ring_q {
        commands.entity(entity).despawn();
    }

    // Select whatever is currently hovered.
    if let Some(entity) = hovered_q.iter().next() {
        commands.entity(entity).insert(Selected);
    }
}

/// Right-click: issue a move command to selected units.
pub fn handle_right_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    selected_q: Query<Entity, With<Selected>>,
    mut commands: Commands,
) {
    if !mouse.just_pressed(MouseButton::Right) {
        return;
    }

    let Some(ray) = cursor_ray(&windows, &camera_q) else {
        return;
    };

    let hits = ray_cast.cast_ray(ray, &default());

    let Some((_, hit)) = hits.first() else {
        return;
    };

    for entity in &selected_q {
        commands.entity(entity).insert(MoveTarget(hit.point));
    }
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

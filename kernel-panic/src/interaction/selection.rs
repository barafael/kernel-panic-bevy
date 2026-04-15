use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use crate::rendering::camera::RtsCamera;
use crate::units::components::UnitType;

use super::movement::MoveTarget;

/// Marks a unit as selected by the player.
#[derive(Component)]
pub struct Selected;

/// Visual ring shown under selected units.
#[derive(Component)]
pub struct SelectionRing;

/// Shared mesh and material for selection rings so we don't reallocate per click.
#[derive(Resource, Clone)]
pub(crate) struct SelectionRingAssets {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

impl SelectionRingAssets {
    fn init(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> Self {
        Self {
            mesh: meshes.add(Torus::new(18.0, 22.0)),
            material: materials.add(StandardMaterial {
                base_color: Color::srgba(1.0, 1.0, 1.0, 0.5),
                emissive: LinearRgba::new(1.0, 1.0, 1.0, 1.0) * 3.0,
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            }),
        }
    }
}

pub fn handle_selection(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    unit_q: Query<Entity, With<UnitType>>,
    selected_q: Query<Entity, With<Selected>>,
    ring_q: Query<Entity, With<SelectionRing>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    ring_assets: Option<Res<SelectionRingAssets>>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }

    let Some(ray) = cursor_ray(&windows, &camera_q) else {
        return;
    };

    let hits = ray_cast.cast_ray(ray, &default());

    let clicked_unit = hits.iter().find(|(entity, _)| unit_q.contains(*entity));

    // Clear previous selection.
    for entity in &selected_q {
        commands.entity(entity).remove::<Selected>();
    }
    for entity in &ring_q {
        commands.entity(entity).despawn();
    }

    if let Some(&(entity, _)) = clicked_unit {
        commands.entity(entity).insert(Selected);

        // Lazily initialize shared ring assets on first selection.
        let assets = match ring_assets {
            Some(res) => res.into_inner().clone(),
            None => {
                let a = SelectionRingAssets::init(&mut meshes, &mut materials);
                let cloned = a.clone();
                commands.insert_resource(a);
                cloned
            }
        };

        commands.entity(entity).with_child((
            SelectionRing,
            Mesh3d(assets.mesh),
            MeshMaterial3d(assets.material),
            Transform::from_xyz(0.0, -1.0, 0.0)
                .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        ));
    }
}

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
    let target = hit.point;

    for entity in &selected_q {
        commands.entity(entity).insert(MoveTarget(target));
    }
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

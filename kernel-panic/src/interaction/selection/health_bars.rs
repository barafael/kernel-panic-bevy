//! World-space health bars that appear above selected units and billboard to
//! face the camera. Bars are child entities of the unit so they follow it
//! automatically; we only need to update scale/color each frame.

use bevy::prelude::*;

use super::core::{Selected, SelectionSet};
use crate::rendering::camera::RtsCamera;
use crate::ui::hud::style::UI_OVERLAY_BLACK;
use crate::units::components::{Health, UnitType, health_color};

pub(super) struct HealthBarsPlugin;

impl Plugin for HealthBarsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                spawn_health_bars,
                despawn_health_bars,
                update_health_bars.after(spawn_health_bars),
                billboard_health_bars.after(update_health_bars),
            )
                .in_set(SelectionSet::Visuals),
        );
    }
}

/// Health bar background child entity.
#[derive(Component)]
struct HealthBarBg;

/// Health bar foreground (colored) child entity.
#[derive(Component)]
struct HealthBarFg;

/// Shared mesh and material assets for health bars.
#[derive(Resource, Clone)]
struct HealthBarAssets {
    bar_mesh: Handle<Mesh>,
    bg_material: Handle<StandardMaterial>,
}

/// Health bar dimensions (world-space units).
const HEALTH_BAR_WIDTH: f32 = 20.0;
const HEALTH_BAR_HEIGHT: f32 = 2.0;
/// Vertical offset above the unit's origin.
const HEALTH_BAR_Y_OFFSET: f32 = 30.0;

/// Spawn health bar child entities on newly-selected units.
fn spawn_health_bars(
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
fn despawn_health_bars(
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
fn update_health_bars(
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
fn billboard_health_bars(
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
            base_color: UI_OVERLAY_BLACK,
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

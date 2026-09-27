//! World-space health bars that appear above selected units and billboard to
//! face the camera. Bars are child entities of the unit so they follow it
//! automatically; we only need to update scale/color each frame.

use bevy::prelude::*;

use super::core::{Selected, SelectionSet};
use crate::rendering::camera::RtsCamera;
use crate::units::components::{Health, UnitType, health_color};

/// Translucent black backing color for the health bar's background
/// quad (formerly `crate::ui::hud::style::UI_OVERLAY_BLACK` — inlined
/// here so the world-space health-bar visuals don't depend on the now-
/// removed `ui` module).
const UI_OVERLAY_BLACK: Color = Color::srgba(0.0, 0.0, 0.0, 0.6);

pub(super) struct HealthBarsPlugin;

impl Plugin for HealthBarsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                spawn_health_bars,
                despawn_health_bars,
                update_health_bars.after(spawn_health_bars),
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
    /// Foreground colours quantised over [`FG_PALETTE_STEPS`] health
    /// fractions, so every selected unit shares one of a fixed set of
    /// materials instead of owning a material rewritten every frame.
    fg_materials: Vec<Handle<StandardMaterial>>,
}

/// Health-fraction buckets in the foreground palette. 32 steps of the
/// green→yellow→red ramp are indistinguishable on a 2-elmo-tall bar.
const FG_PALETTE_STEPS: usize = 32;

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

        // Foreground bar (colored, sits slightly in front of background).
        // Starts at full health; `update_health_bars` picks the real
        // bucket on its first pass.
        commands.entity(entity).with_child((
            HealthBarFg,
            Mesh3d(assets.bar_mesh.clone()),
            MeshMaterial3d(assets.fg_materials[FG_PALETTE_STEPS - 1].clone()),
            Transform::from_xyz(0.0, HEALTH_BAR_Y_OFFSET, 0.0).with_scale(Vec3::new(
                HEALTH_BAR_WIDTH,
                HEALTH_BAR_HEIGHT,
                1.0,
            )),
        ));
    }
}

/// Remove health bar children from units that are no longer selected.
///
/// Walks each deselected unit's `Children` directly (O(children-of-unit))
/// rather than scanning every health bar in the world per deselection
/// (the previous O(removed × total-bars) form). At realistic selection
/// sizes the difference is two orders of magnitude.
fn despawn_health_bars(
    mut removed_selections: RemovedComponents<Selected>,
    children_q: Query<&Children>,
    bar_q: Query<(), Or<(With<HealthBarBg>, With<HealthBarFg>)>>,
    mut commands: Commands,
) {
    for unit in removed_selections.read() {
        let Ok(children) = children_q.get(unit) else {
            continue;
        };
        for child in children.iter() {
            if bar_q.contains(child) {
                commands.entity(child).despawn();
            }
        }
    }
}

/// Per selected unit: size and colour the foreground bar from its
/// health, and billboard both bars toward the camera.
///
/// The bar mesh is a Plane3d in XY with normal along +Z. We rotate so that
/// +Z points toward the camera, keeping the bar upright (Y stays world-up).
/// Transforms and material handles are only written when their value
/// changes, so a still camera over a full-health selection leaves the
/// transform-propagation and render-extract passes nothing to do.
#[allow(clippy::type_complexity)]
fn update_health_bars(
    camera_q: Query<&GlobalTransform, With<RtsCamera>>,
    selected_units: Query<(&Health, &GlobalTransform, &Children), (With<Selected>, With<UnitType>)>,
    mut bars: Query<
        (
            &mut Transform,
            Option<&mut MeshMaterial3d<StandardMaterial>>,
            Has<HealthBarFg>,
        ),
        Or<(With<HealthBarBg>, With<HealthBarFg>)>,
    >,
    bar_assets: Option<Res<HealthBarAssets>>,
) {
    let (Ok(cam_gt), Some(assets)) = (camera_q.single(), bar_assets) else {
        return;
    };

    for (health, parent_gt, children) in &selected_units {
        let frac = health.fraction().clamp(0.0, 1.0);
        let fg_material = &assets.fg_materials[palette_index(frac)];

        let bar_world_pos = parent_gt.translation() + Vec3::Y * HEALTH_BAR_Y_OFFSET;
        let to_camera = (cam_gt.translation() - bar_world_pos).normalize_or(Vec3::Z);
        // World-space rotation that faces +Z toward the camera, keeping Y
        // as the up direction, converted to parent-local once per unit.
        let world_rot = Quat::from_rotation_arc(Vec3::Z, to_camera);
        let parent_rot_inv = parent_gt.to_scale_rotation_translation().1.inverse();
        let rotation = parent_rot_inv * world_rot;
        // Push the foreground bar slightly toward the camera to avoid
        // z-fighting.
        let fg_translation =
            Vec3::new(0.0, HEALTH_BAR_Y_OFFSET, 0.0) + (parent_rot_inv * to_camera) * 0.2;

        for child in children.iter() {
            let Ok((mut transform, material, is_fg)) = bars.get_mut(child) else {
                continue;
            };
            let mut next = *transform;
            next.rotation = rotation;
            if is_fg {
                next.scale.x = HEALTH_BAR_WIDTH * frac;
                next.translation = fg_translation;
                if let Some(mut material) = material
                    && material.0 != *fg_material
                {
                    material.0 = fg_material.clone();
                }
            }
            transform.set_if_neq(next);
        }
    }
}

/// Palette bucket for a health fraction in `[0, 1]`.
fn palette_index(frac: f32) -> usize {
    ((frac * (FG_PALETTE_STEPS - 1) as f32).round() as usize).min(FG_PALETTE_STEPS - 1)
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

    let fg_materials = (0..FG_PALETTE_STEPS)
        .map(|i| {
            let color = health_color(i as f32 / (FG_PALETTE_STEPS - 1) as f32);
            materials.add(StandardMaterial {
                base_color: color,
                emissive: LinearRgba::from(color) * 2.0,
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                ..default()
            })
        })
        .collect();
    let assets = HealthBarAssets {
        bar_mesh: meshes.add(Plane3d::new(Vec3::Z, Vec2::new(0.5, 0.5))),
        bg_material: materials.add(StandardMaterial {
            base_color: UI_OVERLAY_BLACK,
            emissive: LinearRgba::NONE,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        fg_materials,
    };
    let cloned = assets.clone();
    commands.insert_resource(assets);
    cloned
}

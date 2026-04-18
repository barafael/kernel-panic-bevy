//! Datavent-placement mode for mobile constructors.
//!
//! Activated by a `BeginPlacementEvent` from the build menu (issued when
//! the player clicks a building icon while a constructor is selected).
//! While active, a translucent ghost of the target building follows the
//! cursor, snapping to the nearest datavent within range. The ghost tints
//! green on a valid datavent and red otherwise.
//!
//! **Commit:** left-click while the ghost is green → issue a `BuildAt`
//! command to the builder (shift-queued if Shift is held). Otherwise the
//! left-click is swallowed so the underlying selection system doesn't fire.
//!
//! **Cancel:** right-click, Escape, or selecting a different unit — all
//! exit placement mode without issuing a command.

use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;

use crate::interaction::Selected;
use crate::interaction::movement::{MoveTarget, QueuedCommand};
use crate::interaction::selection::{SelectionSet, apply_ordered_command, ground_hit};
use crate::rendering::camera::RtsCamera;
use crate::terrain::geovent::{GeoventSmoker, VentClaim};
use crate::units::components::{Faction, UnitType};
use crate::units::definitions::UnitKind;
use crate::units::meshes::{S3OModelCache, unit_material, unit_mesh};
use crate::units::unit_registry::UnitRegistry;

use super::build_menu::BeginPlacementEvent;

pub(super) struct PlacementPlugin;

impl Plugin for PlacementPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlacementMode>().add_systems(
            Update,
            (
                begin_placement,
                update_placement_ghost,
                commit_or_cancel_placement,
            )
                .chain()
                // Run before selection so consumed mouse clicks don't
                // bleed through into click-to-select.
                .before(SelectionSet::Select),
        );
    }
}

/// Active placement mode. `None` field means no placement in progress.
#[derive(Resource, Default)]
pub struct PlacementMode {
    pub active: Option<ActivePlacement>,
}

pub struct ActivePlacement {
    pub builder: Entity,
    pub kind: UnitKind,
    /// Ghost entity rendered at the snapped datavent (or at the raw cursor
    /// position when no datavent is in range).
    pub ghost: Entity,
    /// Snapped datavent world position, if any. `None` when the cursor is
    /// too far from every datavent (the ghost is shown red at the raw
    /// cursor and left-click is rejected).
    pub snapped: Option<Vec3>,
}

/// Max XZ distance from the cursor to a datavent for the ghost to snap
/// onto it. Generous enough that clicking "near" a vent still registers.
const SNAP_RADIUS: f32 = 48.0;

/// Begin placement when the build menu fires an event. If a previous
/// placement was already active we despawn its ghost and replace it.
fn begin_placement(
    mut ev: MessageReader<BeginPlacementEvent>,
    mut mode: ResMut<PlacementMode>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    unit_registry: Res<UnitRegistry>,
    faction_q: Query<&Faction>,
) {
    for event in ev.read() {
        if let Some(existing) = mode.active.take() {
            commands.entity(existing.ghost).despawn();
        }

        // Use the builder's faction for the ghost tint so the preview
        // reads visually as "this will be mine".
        let faction = faction_q
            .get(event.builder)
            .copied()
            .unwrap_or(Faction::System);
        let mesh = unit_mesh(event.kind, &mut meshes, &mut model_cache, &unit_registry);
        let model_name = unit_registry.model(event.kind).to_string();
        let base_mat = unit_material(
            event.kind,
            faction,
            &mut materials,
            &mut images,
            &mut model_cache,
            &model_name,
        );
        // Clone the base material and make it translucent green. We spawn
        // a fresh handle so that flipping between valid/invalid tint
        // doesn't mutate the real unit material.
        let ghost_mat = {
            let source = materials
                .get(&base_mat)
                .cloned()
                .unwrap_or(StandardMaterial::default());
            materials.add(StandardMaterial {
                base_color: GHOST_VALID_COLOR,
                alpha_mode: AlphaMode::Blend,
                unlit: true,
                base_color_texture: source.base_color_texture.clone(),
                ..default()
            })
        };

        let ghost = commands
            .spawn((
                PlacementGhost,
                Mesh3d(mesh),
                MeshMaterial3d(ghost_mat),
                Transform::default(),
                Visibility::Hidden,
            ))
            .id();

        mode.active = Some(ActivePlacement {
            builder: event.builder,
            kind: event.kind,
            ghost,
            snapped: None,
        });
    }
}

#[derive(Component)]
struct PlacementGhost;

const GHOST_VALID_COLOR: Color = Color::srgba(0.3, 1.0, 0.4, 0.55);
const GHOST_INVALID_COLOR: Color = Color::srgba(1.0, 0.3, 0.3, 0.55);

/// Each frame: ray-cast the cursor onto the ground, find the nearest
/// datavent within SNAP_RADIUS, update the ghost's position + tint.
/// Despawns the ghost if the builder was deselected or destroyed.
#[allow(clippy::too_many_arguments)]
fn update_placement_ghost(
    mut mode: ResMut<PlacementMode>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform), With<RtsCamera>>,
    mut ray_cast: MeshRayCast,
    mut transforms: Query<(&mut Transform, &mut Visibility), With<PlacementGhost>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    ghost_mats: Query<&MeshMaterial3d<StandardMaterial>, With<PlacementGhost>>,
    selected_q: Query<Entity, With<Selected>>,
    builder_q: Query<(), With<UnitType>>,
    geovents: Query<&GeoventSmoker, Without<VentClaim>>,
    mut commands: Commands,
) {
    let Some(active) = mode.active.as_mut() else {
        return;
    };

    // Auto-cancel when the builder was destroyed or deselected.
    let builder_alive = builder_q.contains(active.builder);
    let builder_selected = selected_q.contains(active.builder);
    if !builder_alive || !builder_selected {
        commands.entity(active.ghost).despawn();
        mode.active = None;
        return;
    }

    let Some(cursor_pt) = ground_hit(&windows, &camera_q, &mut ray_cast) else {
        // Cursor off-screen / off-terrain: hide the ghost this frame.
        if let Ok((_, mut vis)) = transforms.get_mut(active.ghost) {
            *vis = Visibility::Hidden;
        }
        active.snapped = None;
        return;
    };

    // Snap to nearest *unclaimed* datavent within SNAP_RADIUS — the
    // `Without<VentClaim>` filter on the query means claimed vents don't
    // appear here at all, so a second constructor can't stack another
    // building onto a vent that's already being built on.
    let mut best: Option<(Vec3, f32)> = None;
    for vent in &geovents {
        let d = (vent.pos - cursor_pt).length();
        if d <= SNAP_RADIUS && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((vent.pos, d));
        }
    }

    let (ghost_pos, valid) = match best {
        Some((pos, _)) => (pos, true),
        None => (cursor_pt, false),
    };
    active.snapped = if valid { Some(ghost_pos) } else { None };

    if let Ok((mut tf, mut vis)) = transforms.get_mut(active.ghost) {
        tf.translation = ghost_pos + Vec3::Y * 0.1;
        *vis = Visibility::Inherited;
    }
    if let Ok(mat_handle) = ghost_mats.get(active.ghost)
        && let Some(mat) = materials.get_mut(&mat_handle.0)
    {
        mat.base_color = if valid {
            GHOST_VALID_COLOR
        } else {
            GHOST_INVALID_COLOR
        };
    }
}

/// Commit on left-click (if snapped), cancel on right-click or Escape.
/// Consumed clicks are cleared from `ButtonInput` so the selection system
/// doesn't also see them as a selection / deselect action.
///
/// On commit we also stamp `VentClaim` onto the target datavent so a
/// concurrent constructor placing during the same frame can't end up
/// building a second structure on the same spot. The claim is released
/// by `release_stale_vent_claims` once neither the builder nor a
/// finished building occupies the site.
#[allow(clippy::too_many_arguments)]
fn commit_or_cancel_placement(
    mut mode: ResMut<PlacementMode>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    move_target_q: Query<(), With<MoveTarget>>,
    vents: Query<(Entity, &GeoventSmoker), Without<VentClaim>>,
    mut commands: Commands,
) {
    let Some(active) = mode.active.as_ref() else {
        return;
    };

    if mouse.just_pressed(MouseButton::Right) {
        mouse.clear_just_pressed(MouseButton::Right);
        commands.entity(active.ghost).despawn();
        mode.active = None;
        return;
    }
    if keys.just_pressed(KeyCode::Escape) {
        keys.clear_just_pressed(KeyCode::Escape);
        commands.entity(active.ghost).despawn();
        mode.active = None;
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        if let Some(site) = active.snapped {
            let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            apply_ordered_command(
                active.builder,
                QueuedCommand::BuildAt {
                    kind: active.kind,
                    site,
                },
                shift,
                &move_target_q,
                &mut commands,
            );
            // Stamp the claim onto the specific vent at this site. Uses
            // a tiny exact-ish equality (1 elmo) because `site` came from
            // the snap step, which just copied `vent.pos`.
            for (vent_entity, vent) in &vents {
                if vent.pos.distance_squared(site) < 1.0 {
                    commands.entity(vent_entity).insert(VentClaim);
                    break;
                }
            }
            commands.entity(active.ghost).despawn();
            mode.active = None;
        }
        // Either we committed or the click was on invalid ground; either
        // way we consume it so the selection system doesn't reinterpret
        // it as a click-to-select on a unit under the cursor.
        mouse.clear_just_pressed(MouseButton::Left);
    }
}

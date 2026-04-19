//! Material brightening for hovered/selected units. When a unit enters a
//! highlighted state we clone its material and crank the emissive boost up,
//! stashing the original handle in `OriginalMaterial` so we can restore it
//! when the highlight ends.

use bevy::prelude::*;

use super::core::{Hovered, Selected, SelectionSet};
use crate::units::components::{Faction, SelectionVolume, UnitType};

pub(super) struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, update_unit_highlight.in_set(SelectionSet::Visuals));
    }
}

/// Emissive boost multiplier for hovered units.
const HOVER_BRIGHTNESS: f32 = 1.5;
/// Emissive boost multiplier for selected units.
const SELECTED_BRIGHTNESS: f32 = 2.5;

/// Stores the unit's original (un-brightened) material handle so we can
/// restore it when the unit is no longer hovered or selected.
#[derive(Component)]
struct OriginalMaterial(Handle<StandardMaterial>);

/// Tracks the brightness factor currently baked into a unit's materials.
/// The system skips re-applying when this matches the desired factor, so a
/// steady selection doesn't mint a fresh `StandardMaterial` every frame.
#[derive(Component)]
struct Highlighted(f32);

/// Two epsilon for the `f32` factor comparison so we treat `HOVER_BRIGHTNESS`
/// vs `SELECTED_BRIGHTNESS` as unambiguously different without triggering on
/// bit-identical values.
const FACTOR_EPS: f32 = 0.01;

/// Brighten a unit's materials when it becomes hovered or selected.
/// Works on both the root entity and its piece children (S3O models).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_unit_highlight(
    hovered_q: Query<
        (Entity, &Faction, Option<&Highlighted>),
        (With<Hovered>, Without<Selected>, With<UnitType>),
    >,
    selected_q: Query<(Entity, &Faction, Option<&Highlighted>), (With<Selected>, With<UnitType>)>,
    unhighlighted_q: Query<
        Entity,
        (
            Without<Hovered>,
            Without<Selected>,
            With<UnitType>,
            With<Highlighted>,
        ),
    >,
    children_q: Query<&Children>,
    mesh_mat_q: Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    volume_q: Query<(), With<SelectionVolume>>,
    original_q: Query<&OriginalMaterial>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for unit_entity in &unhighlighted_q {
        restore_unit_materials(unit_entity, &children_q, &original_q, &mut commands);
        commands.entity(unit_entity).try_remove::<Highlighted>();
    }

    // Apply hover brightness only when the state actually changed. Skipping
    // otherwise is the whole point: `apply_brightness` clones+inserts a new
    // `StandardMaterial` per piece, and doing that 60×/s leaks asset handles.
    for (unit_entity, faction, current) in &hovered_q {
        if !needs_rebrighten(current, HOVER_BRIGHTNESS) {
            continue;
        }
        brighten_unit(
            unit_entity,
            faction,
            HOVER_BRIGHTNESS,
            &children_q,
            &mesh_mat_q,
            &volume_q,
            &original_q,
            &mut materials,
            &mut commands,
        );
        commands
            .entity(unit_entity)
            .insert(Highlighted(HOVER_BRIGHTNESS));
    }

    for (unit_entity, faction, current) in &selected_q {
        if !needs_rebrighten(current, SELECTED_BRIGHTNESS) {
            continue;
        }
        brighten_unit(
            unit_entity,
            faction,
            SELECTED_BRIGHTNESS,
            &children_q,
            &mesh_mat_q,
            &volume_q,
            &original_q,
            &mut materials,
            &mut commands,
        );
        commands
            .entity(unit_entity)
            .insert(Highlighted(SELECTED_BRIGHTNESS));
    }
}

fn needs_rebrighten(current: Option<&Highlighted>, desired: f32) -> bool {
    current.map_or(true, |h| (h.0 - desired).abs() > FACTOR_EPS)
}

/// Brighten all mesh materials on a unit entity and its children.
#[allow(clippy::too_many_arguments)]
fn brighten_unit(
    unit_entity: Entity,
    faction: &Faction,
    factor: f32,
    children_q: &Query<&Children>,
    mesh_mat_q: &Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    volume_q: &Query<(), With<SelectionVolume>>,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    commands: &mut Commands,
) {
    // Collect all entities to brighten: the unit itself + all descendants with meshes.
    // Skip the invisible selection-volume sphere — its material has low
    // alpha, and tinting base_color would turn it into a solid coloured blob
    // over the unit.
    let mut targets = Vec::new();
    if mesh_mat_q.contains(unit_entity) && !volume_q.contains(unit_entity) {
        targets.push(unit_entity);
    }
    collect_mesh_descendants(unit_entity, children_q, mesh_mat_q, volume_q, &mut targets);

    for entity in targets {
        let Ok((_, current_mat)) = mesh_mat_q.get(entity) else {
            continue;
        };
        apply_brightness(
            entity,
            &current_mat.0,
            faction,
            factor,
            original_q,
            materials,
            commands,
        );
    }
}

/// Restore original materials on a unit and all its descendants.
fn restore_unit_materials(
    unit_entity: Entity,
    children_q: &Query<&Children>,
    original_q: &Query<&OriginalMaterial>,
    commands: &mut Commands,
) {
    let mut targets = Vec::new();
    if original_q.contains(unit_entity) {
        targets.push(unit_entity);
    }
    collect_original_descendants(unit_entity, children_q, original_q, &mut targets);

    for entity in targets {
        if let Ok(original) = original_q.get(entity) {
            commands
                .entity(entity)
                .try_insert(MeshMaterial3d(original.0.clone()))
                .try_remove::<OriginalMaterial>();
        }
    }
}

fn collect_mesh_descendants(
    entity: Entity,
    children_q: &Query<&Children>,
    mesh_mat_q: &Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    volume_q: &Query<(), With<SelectionVolume>>,
    targets: &mut Vec<Entity>,
) {
    if let Ok(children) = children_q.get(entity) {
        for child in children.iter() {
            if mesh_mat_q.contains(child) && !volume_q.contains(child) {
                targets.push(child);
            }
            collect_mesh_descendants(child, children_q, mesh_mat_q, volume_q, targets);
        }
    }
}

fn collect_original_descendants(
    entity: Entity,
    children_q: &Query<&Children>,
    original_q: &Query<&OriginalMaterial>,
    targets: &mut Vec<Entity>,
) {
    if let Ok(children) = children_q.get(entity) {
        for child in children.iter() {
            if original_q.contains(child) {
                targets.push(child);
            }
            collect_original_descendants(child, children_q, original_q, targets);
        }
    }
}

fn apply_brightness(
    entity: Entity,
    current_handle: &Handle<StandardMaterial>,
    faction: &Faction,
    factor: f32,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    commands: &mut Commands,
) {
    let source_handle = if let Ok(orig) = original_q.get(entity) {
        orig.0.clone()
    } else {
        let h = current_handle.clone();
        commands
            .entity(entity)
            .try_insert(OriginalMaterial(h.clone()));
        h
    };

    let Some(source) = materials.get(&source_handle) else {
        return;
    };

    // Unit materials are `unlit: true`, so the fragment shader only scales
    // `base_color_texture * base_color`. Blend the source `base_color`
    // toward the faction tint so the brightening is faction-coloured
    // without saturating away the texture's own hues, and scale overall
    // brightness by `factor`. Preserve alpha from the source so semi-
    // transparent materials (e.g. the invisible selection volume) don't
    // turn into opaque coloured blobs.
    let mut bright = source.clone();
    let src = LinearRgba::from(source.base_color);
    let tint = LinearRgba::from(faction.color());
    const TINT_MIX: f32 = 0.4;
    let mixed = LinearRgba {
        red: (src.red * (1.0 - TINT_MIX) + tint.red * TINT_MIX) * factor,
        green: (src.green * (1.0 - TINT_MIX) + tint.green * TINT_MIX) * factor,
        blue: (src.blue * (1.0 - TINT_MIX) + tint.blue * TINT_MIX) * factor,
        alpha: src.alpha,
    };
    bright.base_color = Color::LinearRgba(mixed);
    bright.emissive = mixed;
    let handle = materials.add(bright);
    commands.entity(entity).try_insert(MeshMaterial3d(handle));
}

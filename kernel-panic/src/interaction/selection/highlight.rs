//! Material brightening for hovered/selected units. When a unit enters a
//! highlighted state we clone its material and crank the emissive boost up,
//! stashing the original handle in `OriginalMaterial` so we can restore it
//! when the highlight ends.

use bevy::prelude::*;

use super::core::{Hovered, Selected, SelectionSet};
use crate::units::components::{Faction, UnitType};

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

/// Marker to track that a unit currently has brightened materials.
#[derive(Component)]
struct Highlighted;

/// Brighten a unit's materials when it becomes hovered or selected.
/// Works on both the root entity and its piece children (S3O models).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_unit_highlight(
    hovered_q: Query<(Entity, &Faction), (With<Hovered>, Without<Selected>, With<UnitType>)>,
    selected_q: Query<(Entity, &Faction), (With<Selected>, With<UnitType>)>,
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
    original_q: Query<&OriginalMaterial>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    // Restore materials on units that are no longer hovered or selected.
    for unit_entity in &unhighlighted_q {
        restore_unit_materials(unit_entity, &children_q, &original_q, &mut commands);
        commands.entity(unit_entity).try_remove::<Highlighted>();
    }

    // Apply hover brightness (only if not also selected).
    for (unit_entity, faction) in &hovered_q {
        brighten_unit(
            unit_entity,
            faction,
            HOVER_BRIGHTNESS,
            &children_q,
            &mesh_mat_q,
            &original_q,
            &mut materials,
            &mut commands,
        );
        commands.entity(unit_entity).insert(Highlighted);
    }

    // Apply selection brightness (takes priority over hover).
    for (unit_entity, faction) in &selected_q {
        brighten_unit(
            unit_entity,
            faction,
            SELECTED_BRIGHTNESS,
            &children_q,
            &mesh_mat_q,
            &original_q,
            &mut materials,
            &mut commands,
        );
        commands.entity(unit_entity).insert(Highlighted);
    }
}

/// Brighten all mesh materials on a unit entity and its children.
#[allow(clippy::too_many_arguments)]
fn brighten_unit(
    unit_entity: Entity,
    faction: &Faction,
    factor: f32,
    children_q: &Query<&Children>,
    mesh_mat_q: &Query<(Entity, &MeshMaterial3d<StandardMaterial>)>,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    commands: &mut Commands,
) {
    // Collect all entities to brighten: the unit itself + all descendants with meshes.
    let mut targets = Vec::new();
    if mesh_mat_q.contains(unit_entity) {
        targets.push(unit_entity);
    }
    collect_mesh_descendants(unit_entity, children_q, mesh_mat_q, &mut targets);

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
    targets: &mut Vec<Entity>,
) {
    if let Ok(children) = children_q.get(entity) {
        for child in children.iter() {
            if mesh_mat_q.contains(child) {
                targets.push(child);
            }
            collect_mesh_descendants(child, children_q, mesh_mat_q, targets);
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

    let mut bright = source.clone();
    let color = LinearRgba::from(faction.color());
    bright.emissive = color * factor;
    let handle = materials.add(bright);
    commands.entity(entity).try_insert(MeshMaterial3d(handle));
}

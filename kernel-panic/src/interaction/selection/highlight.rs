//! Material brightening for hovered/selected units. When a unit enters a
//! highlighted state we swap each piece's material for a brightened
//! variant, stashing the original handle in `OriginalMaterial` so we can
//! restore it when the highlight ends.
//!
//! Brightened variants are minted once per (source material, faction,
//! factor) and reused: all pieces of a unit share one material and all
//! units of a faction share a handful, so hovering across an army costs
//! a few asset adds in total instead of one per piece per state change.

use std::collections::HashMap;

use bevy::prelude::*;

use super::core::{Hovered, Selected, SelectionSet};
use crate::units::components::{Faction, UnitType};

pub(super) struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BrightMaterials>()
            .add_systems(Update, update_unit_highlight.in_set(SelectionSet::Visuals));
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
/// steady selection doesn't touch materials every frame.
#[derive(Component)]
struct Highlighted(f32);

/// Brightened variants keyed by (source material, faction tint, factor
/// bits). Sources are the shared faction materials, plus the per-unit
/// emerge-fade / cloak clones of units highlighted mid-fade; entries
/// whose source asset is gone are pruned whenever a new one is added.
#[derive(Resource, Default)]
struct BrightMaterials(HashMap<(AssetId<StandardMaterial>, Faction, u32), Handle<StandardMaterial>>);

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
    mesh_mat_q: Query<&MeshMaterial3d<StandardMaterial>>,
    original_q: Query<&OriginalMaterial>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut bright: ResMut<BrightMaterials>,
    mut commands: Commands,
) {
    for unit_entity in &unhighlighted_q {
        restore_unit_materials(unit_entity, &children_q, &original_q, &mut commands);
        commands.entity(unit_entity).try_remove::<Highlighted>();
    }

    // Apply brightness only when the state actually changed — a steady
    // hover/selection must not re-insert materials every frame.
    let pending: Vec<(Entity, Faction, f32)> = hovered_q
        .iter()
        .map(|(e, f, h)| (e, f, h, HOVER_BRIGHTNESS))
        .chain(selected_q.iter().map(|(e, f, h)| (e, f, h, SELECTED_BRIGHTNESS)))
        .filter(|(_, _, current, factor)| needs_rebrighten(*current, *factor))
        .map(|(e, f, _, factor)| (e, *f, factor))
        .collect();
    for (unit_entity, faction, factor) in pending {
        brighten_unit(
            unit_entity,
            &faction,
            factor,
            &children_q,
            &mesh_mat_q,
            &original_q,
            &mut materials,
            &mut bright,
            &mut commands,
        );
        commands.entity(unit_entity).insert(Highlighted(factor));
    }
}

fn needs_rebrighten(current: Option<&Highlighted>, desired: f32) -> bool {
    current.is_none_or(|h| (h.0 - desired).abs() > FACTOR_EPS)
}

/// Brighten all mesh materials on a unit entity and its children.
#[allow(clippy::too_many_arguments)]
fn brighten_unit(
    unit_entity: Entity,
    faction: &Faction,
    factor: f32,
    children_q: &Query<&Children>,
    mesh_mat_q: &Query<&MeshMaterial3d<StandardMaterial>>,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    bright: &mut BrightMaterials,
    commands: &mut Commands,
) {
    // The unit itself (flat-mesh fallback) + every descendant with a
    // material. The selection volume has none, so it is skipped
    // naturally.
    let mut targets = Vec::new();
    if mesh_mat_q.contains(unit_entity) {
        targets.push(unit_entity);
    }
    collect_mesh_descendants(unit_entity, children_q, mesh_mat_q, &mut targets);

    for entity in targets {
        let Ok(current_mat) = mesh_mat_q.get(entity) else {
            continue;
        };
        apply_brightness(
            entity,
            &current_mat.0,
            faction,
            factor,
            original_q,
            materials,
            bright,
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
    mesh_mat_q: &Query<&MeshMaterial3d<StandardMaterial>>,
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

#[allow(clippy::too_many_arguments)]
fn apply_brightness(
    entity: Entity,
    current_handle: &Handle<StandardMaterial>,
    faction: &Faction,
    factor: f32,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    bright: &mut BrightMaterials,
    commands: &mut Commands,
) {
    // The source is whatever the piece wore before its first highlight —
    // the shared faction material, or a per-unit fade/cloak clone if
    // the unit was highlighted mid-fade — and is what gets restored.
    let source_handle = if let Ok(orig) = original_q.get(entity) {
        orig.0.clone()
    } else {
        let h = current_handle.clone();
        commands
            .entity(entity)
            .try_insert(OriginalMaterial(h.clone()));
        h
    };

    let key = (source_handle.id(), *faction, factor.to_bits());
    let handle = if let Some(handle) = bright.0.get(&key) {
        handle.clone()
    } else {
        let Some(variant) = brightened(materials.get(&source_handle), faction, factor) else {
            return;
        };
        let handle = materials.add(variant);
        bright.0.retain(|(source, _, _), _| materials.contains(*source));
        bright.0.insert(key, handle.clone());
        handle
    };
    commands.entity(entity).try_insert(MeshMaterial3d(handle));
}

/// The brightened variant of `source`, or `None` if the asset is gone.
fn brightened(
    source: Option<&StandardMaterial>,
    faction: &Faction,
    factor: f32,
) -> Option<StandardMaterial> {
    let source = source?;
    // Unit materials are `unlit: true`, so the fragment shader only scales
    // `base_color_texture * base_color`. Blend the source `base_color`
    // toward the faction tint so the brightening is faction-coloured
    // without saturating away the texture's own hues, and scale overall
    // brightness by `factor`. Preserve alpha from the source so semi-
    // transparent materials (fade / cloak clones) don't turn into opaque
    // coloured blobs.
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
    Some(bright)
}

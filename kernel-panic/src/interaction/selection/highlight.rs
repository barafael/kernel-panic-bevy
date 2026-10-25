//! How hovered and selected units stand out.
//!
//! Selected units glow as in KP ([`update_selection_glow`]). A hovered
//! unit (a remake addition: KP marks nothing under the cursor) gets
//! brightened materials: we swap each piece's material for a brightened
//! variant, stashing the original handle in `OriginalMaterial` so we can
//! restore it when the highlight ends.
//!
//! Brightened variants are minted once per (source material, owner colour,
//! factor) and reused: all pieces of a unit share one material and all
//! units of an owner share a handful, so hovering across an army costs
//! a few asset adds in total instead of one per piece per state change.

use std::collections::HashMap;

use bevy::prelude::*;

use super::core::{Hovered, Selected, SelectionSet};
use crate::sim::DetFloat;
use crate::units::assets::animation::PieceIndex;
use crate::units::components::{Health, UnitType};
use crate::units::mechanics::cloak::Cloaked;
use crate::units::team_colors::UnitColor;

pub(super) struct HighlightPlugin;

impl Plugin for HighlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BrightMaterials>()
            .init_resource::<GlowMaterials>()
            .add_systems(
                Update,
                (update_unit_highlight, update_selection_glow).in_set(SelectionSet::Visuals),
            );
    }
}

/// Emissive boost multiplier for hovered units.
const HOVER_BRIGHTNESS: f32 = 1.5;

/// Stores the unit's original (un-brightened) material handle so we can
/// restore it when the unit is no longer hovered or selected.
#[derive(Component)]
pub(crate) struct OriginalMaterial(pub(crate) Handle<StandardMaterial>);

/// Tracks the brightness factor currently baked into a unit's materials.
/// The system skips re-applying when this matches the desired factor, so a
/// steady selection doesn't touch materials every frame.
#[derive(Component)]
pub(crate) struct Highlighted(f32);

/// Brightened variants keyed by (source material, owner tint, factor
/// bits). Sources are the shared owner-coloured materials, plus the per-unit
/// emerge-fade / cloak clones of units highlighted mid-fade; entries
/// whose source asset is gone are pruned whenever a new one is added.
#[derive(Resource, Default)]
struct BrightMaterials(
    HashMap<(AssetId<StandardMaterial>, UnitColor, u32), Handle<StandardMaterial>>,
);

/// Epsilon for the `f32` factor comparison, so a factor that has not
/// changed is never re-applied.
const FACTOR_EPS: f32 = 0.01;

/// Brighten a unit's materials when it becomes hovered or selected.
/// Works on both the root entity and its piece children (S3O models).
/// The brightening of the unit under the cursor: on while it is hovered
/// and not selected (a selected unit glows instead).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_unit_highlight(
    hovered_q: Query<
        (Entity, &UnitColor, Option<&Highlighted>),
        (With<Hovered>, Without<Selected>, With<UnitType>),
    >,
    unhighlighted_q: Query<
        Entity,
        (
            Or<(Without<Hovered>, With<Selected>)>,
            With<UnitType>,
            With<Highlighted>,
        ),
    >,
    children_q: Query<&Children>,
    mesh_mat_q: Query<&MeshMaterial3d<StandardMaterial>, Without<SelectionGlow>>,
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
    // hover must not re-insert materials every frame.
    let pending: Vec<(Entity, UnitColor, f32)> = hovered_q
        .iter()
        .map(|(e, f, h)| (e, f, h, HOVER_BRIGHTNESS))
        .filter(|(_, _, current, factor)| needs_rebrighten(*current, *factor))
        .map(|(e, f, _, factor)| (e, *f, factor))
        .collect();
    for (unit_entity, owner, factor) in pending {
        brighten_unit(
            unit_entity,
            &owner,
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
    owner: &UnitColor,
    factor: f32,
    children_q: &Query<&Children>,
    mesh_mat_q: &Query<&MeshMaterial3d<StandardMaterial>, Without<SelectionGlow>>,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    bright: &mut BrightMaterials,
    commands: &mut Commands,
) {
    // The unit itself (flat-mesh fallback) + every descendant with a
    // material but the glow. The selection volume has none, so it is
    // skipped naturally.
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
            owner,
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
    mesh_mat_q: &Query<&MeshMaterial3d<StandardMaterial>, Without<SelectionGlow>>,
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
    owner: &UnitColor,
    factor: f32,
    original_q: &Query<&OriginalMaterial>,
    materials: &mut Assets<StandardMaterial>,
    bright: &mut BrightMaterials,
    commands: &mut Commands,
) {
    // The source is whatever the piece wore before its first highlight —
    // the shared owner-coloured material, or a per-unit fade/cloak clone if
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

    let key = (source_handle.id(), *owner, factor.to_bits());
    let handle = if let Some(handle) = bright.0.get(&key) {
        handle.clone()
    } else {
        let Some(variant) = brightened(materials.get(&source_handle), owner, factor) else {
            return;
        };
        let handle = materials.add(variant);
        bright
            .0
            .retain(|(source, _, _), _| materials.contains(*source));
        bright.0.insert(key, handle.clone());
        handle
    };
    commands.entity(entity).try_insert(MeshMaterial3d(handle));
}

/// The brightened variant of `source`, or `None` if the asset is gone.
fn brightened(
    source: Option<&StandardMaterial>,
    owner: &UnitColor,
    factor: f32,
) -> Option<StandardMaterial> {
    let source = source?;
    // Unit materials are `unlit: true`, so the fragment shader only scales
    // `base_color_texture * base_color`. Blend the source `base_color`
    // toward the owner tint so the brightening is owner-coloured
    // without saturating away the texture's own hues, and scale overall
    // brightness by `factor`. Preserve alpha from the source so semi-
    // transparent materials (fade / cloak clones) don't turn into opaque
    // coloured blobs.
    let mut bright = source.clone();
    let src = LinearRgba::from(source.base_color);
    let tint = LinearRgba::from(owner.0.color());
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

/// One overlay of KP's selection glow (`gui_hilight_selectedunits.lua`,
/// a widget KP keeps on): each model piece of a selected unit is drawn a
/// second time with this child, additively, in the unit's health colour.
/// The widget's outline (the triangles once more as 3 px lines and
/// points) has no counterpart: wgpu draws no wide lines.
#[derive(Component)]
pub(crate) struct SelectionGlow;

/// A selected unit's glow: the [`glow_shade`] it is drawn in and its
/// [`SelectionGlow`] overlays.
#[derive(Component)]
struct Glow {
    shade: (u8, bool),
    overlays: Vec<Entity>,
}

/// Health steps of the glow colour.
const GLOW_STEPS: u8 = 32;

/// `glPolygonOffset(-2, -2)`: the glow wins the depth test against the
/// surface it covers.
const GLOW_DEPTH_BIAS: f32 = 2.0;

/// The glow materials by [`glow_shade`].
#[derive(Resource, Default)]
struct GlowMaterials(HashMap<(u8, bool), Handle<StandardMaterial>>);

/// The unit's health in [`GLOW_STEPS`] steps, and whether it is cloaked.
fn glow_shade(health: Option<&Health>, cloaked: bool) -> (u8, bool) {
    let fraction = health.map_or(1.0, |h| {
        if h.max > 0.0 {
            (h.current / h.max).det_clamp(0.0, 1.0)
        } else {
            1.0
        }
    });
    ((fraction * f32::from(GLOW_STEPS)).round() as u8, cloaked)
}

/// `SetUnitColor` and `HilightUnit`: green at full health, yellow at
/// half, red at none, at a quarter alpha (an eighth cloaked), blended
/// `GL_SRC_ALPHA, GL_ONE`. That adds colour × alpha to KP's sRGB
/// framebuffer, so the colour here is that sum, in sRGB.
fn glow_material((step, cloaked): (u8, bool)) -> StandardMaterial {
    let fraction = f32::from(step) / f32::from(GLOW_STEPS);
    let (red, green) = if fraction > 0.5 {
        (2.0 - 2.0 * fraction, 1.0)
    } else {
        (1.0, 2.0 * fraction)
    };
    let alpha = if cloaked { 0.125 } else { 0.25 };
    StandardMaterial {
        base_color: Color::srgb(red * alpha, green * alpha, 0.0),
        unlit: true,
        alpha_mode: AlphaMode::Add,
        depth_bias: GLOW_DEPTH_BIAS,
        ..default()
    }
}

/// KP's selection highlight: every selected unit glows in its health
/// colour (`HilightUnit` for each of `spGetSelectedUnits`). The glow
/// follows the health and the cloak, and goes with the selection.
#[allow(clippy::type_complexity)]
fn update_selection_glow(
    selected: Query<
        (Entity, Option<&Health>, Has<Cloaked>, Option<&Glow>),
        (With<Selected>, With<UnitType>),
    >,
    deselected: Query<(Entity, &Glow), Without<Selected>>,
    children_q: Query<&Children>,
    pieces: Query<&Mesh3d, With<PieceIndex>>,
    mut glows: ResMut<GlowMaterials>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for (unit, glow) in &deselected {
        for &overlay in &glow.overlays {
            commands.entity(overlay).try_despawn();
        }
        commands.entity(unit).try_remove::<Glow>();
    }
    for (unit, health, cloaked, glow) in &selected {
        let shade = glow_shade(health, cloaked);
        if glow.is_some_and(|g| g.shade == shade) {
            continue;
        }
        let material = glows
            .0
            .entry(shade)
            .or_insert_with(|| materials.add(glow_material(shade)))
            .clone();
        let overlays = match glow {
            Some(glow) => {
                for &overlay in &glow.overlays {
                    commands
                        .entity(overlay)
                        .try_insert(MeshMaterial3d(material.clone()));
                }
                glow.overlays.clone()
            }
            None => {
                let mut pieces_found = Vec::new();
                collect_pieces(unit, &children_q, &pieces, &mut pieces_found);
                pieces_found
                    .into_iter()
                    .map(|(piece, mesh)| {
                        commands
                            .spawn((
                                SelectionGlow,
                                Mesh3d(mesh),
                                MeshMaterial3d(material.clone()),
                                ChildOf(piece),
                            ))
                            .id()
                    })
                    .collect()
            }
        };
        commands.entity(unit).try_insert(Glow { shade, overlays });
    }
}

/// Every model piece under `entity` that has a mesh, with that mesh.
fn collect_pieces(
    entity: Entity,
    children_q: &Query<&Children>,
    pieces: &Query<&Mesh3d, With<PieceIndex>>,
    out: &mut Vec<(Entity, Handle<Mesh>)>,
) {
    if let Ok(children) = children_q.get(entity) {
        for child in children.iter() {
            if let Ok(mesh) = pieces.get(child) {
                out.push((child, mesh.0.clone()));
            }
            collect_pieces(child, children_q, pieces, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<GlowMaterials>()
            .add_systems(Update, update_selection_glow);
        app
    }

    /// A unit with a model root and two pieces, one of them meshless.
    fn spawn_unit(app: &mut App, hp: f32) -> (Entity, Entity) {
        let mesh = app
            .world_mut()
            .resource_mut::<Assets<Mesh>>()
            .add(Cuboid::default());
        let world = app.world_mut();
        let unit = world
            .spawn((
                UnitType(crate::units::content::definitions::UnitKind::Port),
                Health {
                    current: hp,
                    max: 20000.0,
                },
            ))
            .id();
        let root = world.spawn(ChildOf(unit)).id();
        let piece = world.spawn((PieceIndex, Mesh3d(mesh), ChildOf(root))).id();
        world.spawn((PieceIndex, ChildOf(piece)));
        (unit, piece)
    }

    fn glow_color(app: &App, overlay: Entity) -> LinearRgba {
        let handle = &app
            .world()
            .get::<MeshMaterial3d<StandardMaterial>>(overlay)
            .unwrap()
            .0;
        let material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(handle)
            .unwrap();
        assert_eq!(material.alpha_mode, AlphaMode::Add);
        LinearRgba::from(material.base_color)
    }

    /// A selected unit's meshed pieces each get an additive overlay with
    /// the piece's mesh: green at full health, red when nearly dead. It
    /// goes with the selection.
    #[test]
    fn selected_units_glow_in_their_health_colour() {
        let mut app = app();
        let (unit, piece) = spawn_unit(&mut app, 20000.0);
        app.update();
        assert!(app.world().get::<Glow>(unit).is_none(), "not selected");

        app.world_mut().entity_mut(unit).insert(Selected);
        app.update();
        let overlays = app.world().get::<Glow>(unit).unwrap().overlays.clone();
        assert_eq!(overlays.len(), 1, "the meshless piece has none");
        let overlay = overlays[0];
        assert_eq!(app.world().get::<ChildOf>(overlay).unwrap().parent(), piece);
        assert_eq!(
            app.world().get::<Mesh3d>(overlay).unwrap().0,
            app.world().get::<Mesh3d>(piece).unwrap().0
        );
        let full = glow_color(&app, overlay);
        assert_eq!((full.red, full.blue), (0.0, 0.0));
        assert!(full.green > 0.0);

        app.world_mut().get_mut::<Health>(unit).unwrap().current = 1000.0;
        app.update();
        let hurt = glow_color(&app, overlay);
        assert!(hurt.red > 0.0 && hurt.green < full.green, "{hurt:?}");

        app.world_mut().entity_mut(unit).remove::<Selected>();
        app.update();
        assert!(app.world().get::<Glow>(unit).is_none());
        assert!(app.world().get_entity(overlay).is_err());
    }

    /// `SetUnitColor`'s ramp and `HilightUnit`'s alphas.
    #[test]
    fn glow_shades_follow_kp() {
        let at = |current: f32, cloaked: bool| {
            let h = Health {
                current,
                max: 100.0,
            };
            glow_material(glow_shade(Some(&h), cloaked))
                .base_color
                .to_srgba()
        };
        let full = at(100.0, false);
        assert_eq!((full.red, full.green, full.blue), (0.0, 0.25, 0.0));
        let half = at(50.0, false);
        assert_eq!((half.red, half.green), (0.25, 0.25), "yellow at half");
        let dead = at(0.0, false);
        assert_eq!((dead.red, dead.green), (0.25, 0.0));
        assert_eq!(at(100.0, true).green, 0.125, "an eighth cloaked");
    }
}

//! "Emerging" lifecycle: fresh-built units rise out of the factory or
//! fade into view before becoming playable. `production_system` attaches
//! [`Emerging`] with a style matching the unit's faction; `emerge_system`
//! ticks it forward and removes the component when the animation
//! completes.

use bevy::prelude::*;

/// Marks a freshly-built unit that hasn't finished emerging from its
/// construction site. `emerge_system` ticks `remaining` toward 0 over
/// `total` seconds; how the visible model arrives depends on `style`.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct Emerging {
    /// Final Y coordinate the unit should reach when fully emerged.
    pub target_y: f32,
    /// Seconds remaining in the emerge animation.
    pub remaining: f32,
    /// Total duration of the emerge animation (used to compute lerp t).
    pub total: f32,
    /// World point the unit should walk to once it has emerged. `None` for
    /// stationary units that don't need to clear the factory.
    pub rally_point: Option<Vec3>,
    /// `CFactory::SendToEmptySpot`'s second waypoint: the free spot on
    /// the factory's exit arc, queued behind `rally_point` (the point
    /// just outside the factory).
    pub rally_then: Option<Vec3>,
    /// How the model becomes visible during the rise window.
    pub style: EmergeStyle,
}

/// Per-faction emergence visual.
///
/// - `Rise` — System units (Kernel-built). Spawn underground at
///   `target_y - EMERGE_DEPTH` and lerp Y up to surface, with their own
///   COB `Create()` script also moving the `base` piece up via
///   `BUILD_PERCENT_LEFT`.
/// - `Fade` — Hacker / Network units (Hole, Connection, Window, Port).
///   Spawn at surface but materialize via an alpha ramp on a per-unit
///   cloned material. Mirrors upstream's `lua_SetAlphaThreshold(255 → 0)`
///   pattern in bug.bos / packet.bos / connection.bos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmergeStyle {
    Rise,
    Fade,
}

/// Per-unit faded material clones plus the per-piece originals to put
/// back when the entity finishes fading in. Spawned alongside
/// `Emerging { Fade }` so the per-unit alpha ramp doesn't bleed into the
/// shared faction-colored material.
#[derive(Component)]
pub struct FadeMaterials {
    /// The faded clones `emerge_system` ramps — one per *distinct*
    /// source material the unit's pieces used, shared by every piece
    /// that used it. A unit's pieces all carry the same (model,
    /// faction) material, so this is one handle: one clone and one
    /// `get_mut` (re-upload) per tick per unit, not one per piece.
    pub faded: Vec<Handle<StandardMaterial>>,
    /// (piece_entity, original) — the shared material each piece gets
    /// back once the fade completes.
    pub overrides: Vec<(Entity, Handle<StandardMaterial>)>,
}

/// Tick `Emerging` units forward. A `Rise`-style unit stands at its
/// final height from the first frame: Kernel Panic sets
/// `ShowNanoFrame=0` on every unit, so the engine draws the complete
/// model in place and all construction motion comes from the unit's own
/// script (`BUILD_PERCENT_LEFT` piece moves). `Fade` ramps per-piece
/// alpha. When the timer expires the component is removed, faded
/// materials are restored to the shared originals, and the unit gets its
/// rally-walk command if any.
pub fn emerge_system(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &mut Transform,
        &mut Emerging,
        Option<&FadeMaterials>,
    )>,
    piece_mats: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let dt = time.delta_secs();
    for (entity, mut transform, mut emerging, fade) in &mut q {
        emerging.remaining = (emerging.remaining - dt).max(0.0);
        // t goes 0 → 1 over the duration.
        let t = (1.0 - emerging.remaining / emerging.total).clamp(0.0, 1.0);

        match emerging.style {
            EmergeStyle::Rise => {
                if transform.translation.y != emerging.target_y {
                    transform.translation.y = emerging.target_y;
                }
            }
            EmergeStyle::Fade => {
                // Linear alpha ramp; pieces stay at surface y throughout.
                if let Some(fade) = fade {
                    for faded_handle in &fade.faded {
                        if let Some(mat) = materials.get_mut(faded_handle) {
                            mat.base_color = mat.base_color.with_alpha(t);
                        }
                    }
                }
            }
        }

        if emerging.remaining <= 0.0 {
            if matches!(emerging.style, EmergeStyle::Rise) {
                transform.translation.y = emerging.target_y;
            }
            // Restore the shared faction material on every piece we
            // overrode, so future asset swaps / faction recolors take
            // effect on this unit too. With the pieces back on their
            // originals and `FadeMaterials` gone, the faded clone's
            // last strong handle drops and the asset is freed.
            if let Some(fade) = fade {
                for (piece_entity, original) in &fade.overrides {
                    if piece_mats.get(*piece_entity).is_ok() {
                        commands
                            .entity(*piece_entity)
                            .insert(MeshMaterial3d(original.clone()));
                    }
                }
                commands.entity(entity).remove::<FadeMaterials>();
            }
            let (rally, then) = (emerging.rally_point, emerging.rally_then);
            commands.entity(entity).remove::<Emerging>();
            if let Some(target) = rally {
                let mut unit = commands.entity(entity);
                unit.insert(crate::interaction::movement::MoveTarget(target))
                    .remove::<crate::interaction::movement::MovePath>();
                if let Some(then) = then {
                    unit.insert(crate::interaction::movement::CommandQueue {
                        commands: vec![crate::interaction::movement::QueuedCommand::Move(then)],
                    });
                }
            }
        }
    }
}

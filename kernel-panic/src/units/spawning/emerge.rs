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
    /// How the model becomes visible during the rise window.
    pub style: EmergeStyle,
    /// Last `BUILD_PERCENT_LEFT` value pushed into the unit's CobVm. The
    /// publisher system pushes a new value only when the integer crosses
    /// a new unit so the VM doesn't re-receive the same reading 60× /s.
    pub last_build_percent: i32,
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

/// Per-piece original-material handles, restored when an entity finishes
/// fading in. Spawned alongside `Emerging { Fade }` so the per-unit
/// alpha ramp doesn't bleed into the shared faction-colored material.
#[derive(Component)]
pub struct FadeMaterials {
    /// (piece_entity, faded_clone, original) tuples.
    pub overrides: Vec<(Entity, Handle<StandardMaterial>, Handle<StandardMaterial>)>,
}

/// Distance below ground that a freshly-built unit starts at. The
/// `emerge_system` lifts it back up by this much. Roughly the height of a
/// typical unit so the model is fully hidden underground at t=0.
pub const EMERGE_DEPTH: f32 = 40.0;

/// How long before the build cycle completes the unit appears underground
/// and starts rising. Picked so the rise feels like part of the build
/// rather than an after-effect — the player sees ~1.5s of "the laser
/// drew this thing into being". Clamped against `build_time` so very
/// short cycles still finish naturally.
pub const EMERGE_LEAD_TIME: f32 = 1.5;

/// Tick `Emerging` units forward — either lerping Y upward (Rise style)
/// or ramping per-piece alpha (Fade style). When the timer expires the
/// component is removed, faded materials are restored to the shared
/// originals, and the unit gets its rally-walk command if any.
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
                // Ease-out so the unit decelerates as it reaches the surface
                // (reads as "machine settling into place").
                let eased = 1.0 - (1.0 - t).powi(2);
                let start_y = emerging.target_y - EMERGE_DEPTH;
                transform.translation.y = start_y + (emerging.target_y - start_y) * eased;
            }
            EmergeStyle::Fade => {
                // Linear alpha ramp; pieces stay at surface y throughout.
                if let Some(fade) = fade {
                    for (_, faded_handle, _) in &fade.overrides {
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
            // effect on this unit too. The cloned faded handle leaks
            // into the assets pool until despawn — fine, it's small.
            if let Some(fade) = fade {
                for (piece_entity, _, original) in &fade.overrides {
                    if piece_mats.get(*piece_entity).is_ok() {
                        commands
                            .entity(*piece_entity)
                            .insert(MeshMaterial3d(original.clone()));
                    }
                }
                commands.entity(entity).remove::<FadeMaterials>();
            }
            let rally = emerging.rally_point;
            commands.entity(entity).remove::<Emerging>();
            if let Some(target) = rally {
                commands
                    .entity(entity)
                    .insert(crate::interaction::movement::MoveTarget(target))
                    .remove::<crate::interaction::movement::MovePath>();
            }
        }
    }
}

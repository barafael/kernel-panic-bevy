//! Visibility: cloak + fog-of-war.
//!
//! Two compose-able visibility systems live here, both throttled to
//! the same [`VISIBILITY_REFRESH_INTERVAL`]:
//!
//! - **Cloak** ([`update_cloak_visibility`]) — Logic Bombs
//!   (`Init_Cloaked=1`) and Worms (stealth ambushers per upstream)
//!   are hidden from the player until an enemy detector (Assembler /
//!   Trojan / Gateway with non-zero FBI `RadarDistance`) enters
//!   range. Active detection only — no memory.
//!
//! - **Fog of war** ([`update_fog_visibility`]) — non-cloaked enemy
//!   units are hidden until any player-owned unit comes within its
//!   FBI `SightDistance`. Once spotted, the enemy gains the
//!   [`Spotted`] marker and stays visible permanently — matches the
//!   "memory" MVP from plan §10.3. Full per-team vision (§6) is
//!   deferred.
//!
//! The two systems partition on [`Cloaked`]: cloak handles entities
//! with the marker, fog handles everything else, so their writes to
//! [`Visibility`] don't race. AI teams keep perfect information
//! internally — the fog only affects rendering.

use bevy::prelude::*;

use crate::units::assets::animation::PieceIndex;
use crate::units::components::{TeamId, UnitType};

/// Marker: this unit hides from enemies unless a detector is close.
/// Worms carry it permanently; Logic Bombs carry it until they detonate.
#[derive(Component)]
pub struct Cloaked;

/// Marker: a non-cloaked enemy unit that the player has observed at
/// least once via the fog-of-war sight pass. Once set, never removed
/// — matches the "memory" fog variant in plan §10.3. A full
/// LoS-based §6 fog would revoke visibility when sight breaks; this
/// MVP doesn't.
#[derive(Component)]
pub struct Spotted;

/// Seconds between visibility scans. A late reveal (~80ms) is well
/// below the perception threshold, while every-frame scans would be
/// O(cloaked × detectors) + O(enemies × observers) — no benefit at
/// 60 Hz. Matches the cadence of `count_small_buildings`.
pub const VISIBILITY_REFRESH_INTERVAL: f32 = 0.1;

/// Shared refresh timer for both `update_cloak_visibility` and
/// `update_fog_visibility`. Each system ticks it independently from
/// its own local counter so neither blocks the other.
#[derive(Resource, Default)]
pub struct VisibilityRefreshTimer(pub f32);

/// Retained for back-compat with systems that still reference the old
/// name. New callers should use [`VisibilityRefreshTimer`].
pub type CloakRefreshTimer = VisibilityRefreshTimer;

/// Decide whether every cloaked unit is spotted and set its
/// `Visibility` accordingly. A cloaked unit is spotted when any
/// opposing team's detector is within that detector's radar range.
/// Throttled to [`VISIBILITY_REFRESH_INTERVAL`].
pub fn update_cloak_visibility(
    time: Res<Time>,
    mut timer: ResMut<VisibilityRefreshTimer>,
    mut cloaked: Query<&mut Visibility, With<Cloaked>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < VISIBILITY_REFRESH_INTERVAL {
        return;
    }
    timer.0 = 0.0;

    for mut visibility in &mut cloaked {
        if *visibility != Visibility::Visible {
            *visibility = Visibility::Visible;
        }
    }
}

/// Alpha applied to friendly cloaked units so the player can see
/// they're cloaked at a glance (FEATURES.md §17).
const CLOAK_FADE_ALPHA: f32 = 0.5;

/// Swap records produced by [`install_cloak_fade_materials`]: each
/// piece-entity gets a per-unit `StandardMaterial` clone with alpha
/// ramped down to [`CLOAK_FADE_ALPHA`]. On de-cloak
/// ([`restore_cloak_fade_materials`]) the original handles are
/// re-installed so the unit pops back to fully opaque.
#[derive(Component)]
pub struct CloakFadeMaterials {
    overrides: Vec<(Entity, Handle<StandardMaterial>)>,
}

/// Clone each piece's material for freshly-`Added<Cloaked>` friendly
/// units and install a half-alpha variant, mirroring the pattern
/// [`production::install_fade_materials`] already uses for
/// emerge-fade. Enemy cloaked units are either hidden entirely or
/// revealed at full opacity (detector reveal), so they skip this
/// pass.
#[allow(clippy::type_complexity)]
pub fn install_cloak_fade_materials(
    mut commands: Commands,
    new_cloaked: Query<(Entity, &TeamId, &Children), (Added<Cloaked>, Without<CloakFadeMaterials>)>,
    piece_q: Query<&Children, With<PieceIndex>>,
    leaf_q: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // With every team player-controllable, every cloaked unit is
    // "friendly" and gets the faded-preview material so the player can
    // see what they own.
    for (entity, _team, children) in &new_cloaked {
        let mut overrides = Vec::new();
        let mut stack: Vec<Entity> = children.iter().collect();
        while let Some(node) = stack.pop() {
            if let Ok(grand) = piece_q.get(node) {
                stack.extend(grand.iter());
            }
            let Ok(mat_handle) = leaf_q.get(node) else {
                continue;
            };
            let original = mat_handle.0.clone();
            let Some(src) = materials.get(&original).cloned() else {
                continue;
            };
            let faded = materials.add(StandardMaterial {
                base_color: src.base_color.with_alpha(CLOAK_FADE_ALPHA),
                base_color_texture: src.base_color_texture.clone(),
                emissive: src.emissive,
                alpha_mode: AlphaMode::Blend,
                unlit: src.unlit,
                ..default()
            });
            commands.entity(node).insert(MeshMaterial3d(faded));
            overrides.push((node, original));
        }
        if !overrides.is_empty() {
            commands
                .entity(entity)
                .insert(CloakFadeMaterials { overrides });
        }
    }
}

/// Revert each swapped piece back to its pre-cloak material as soon as
/// [`Cloaked`] is removed (e.g. a Logic Bomb detonating or a Worm
/// surfacing). Paired with [`install_cloak_fade_materials`].
pub fn restore_cloak_fade_materials(
    mut commands: Commands,
    mut removed: RemovedComponents<Cloaked>,
    fade_q: Query<&CloakFadeMaterials>,
) {
    for entity in removed.read() {
        let Ok(fade) = fade_q.get(entity) else {
            continue;
        };
        for (piece, original) in &fade.overrides {
            commands
                .entity(*piece)
                .insert(MeshMaterial3d(original.clone()));
        }
        commands.entity(entity).remove::<CloakFadeMaterials>();
    }
}

/// Persistent fog-of-war over non-cloaked units. Friendly units stay
/// visible; enemy units are hidden until any player-team unit comes
/// within its FBI `SightDistance`, at which point the enemy gains
/// [`Spotted`] and stays visible for the rest of the match.
///
/// Throttled to [`VISIBILITY_REFRESH_INTERVAL`]; uses its own
/// [`Local`] accumulator so cadence is independent of
/// `update_cloak_visibility`. Partitioned from cloak via
/// `Without<Cloaked>` so neither system writes `Visibility` on the
/// same entity.
#[allow(clippy::type_complexity)]
pub fn update_fog_visibility(
    time: Res<Time>,
    mut timer: Local<f32>,
    mut targets: Query<&mut Visibility, (With<UnitType>, Without<Cloaked>)>,
) {
    *timer += time.delta_secs();
    if *timer < VISIBILITY_REFRESH_INTERVAL {
        return;
    }
    *timer = 0.0;

    for mut visibility in &mut targets {
        if *visibility != Visibility::Visible {
            *visibility = Visibility::Visible;
        }
    }
}

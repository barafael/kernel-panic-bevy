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
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::unit_registry::UnitRegistry;

/// Which team's perspective the fog-of-war pass applies from. In
/// sandbox mode (every faction human-controllable) this picks the
/// "player" — defaults to team 0. A future MP build would set this
/// per client.
#[derive(Resource, Debug, Clone, Copy, Default)]
pub struct PlayerTeam(pub u8);

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

/// Visibility for cloaked units (Worms / Logic Bombs) from the
/// [`PlayerTeam`]'s perspective.
///
/// - Friendly cloaked: always visible (`install_cloak_fade_materials`
///   handles the half-alpha rendering).
/// - Enemy cloaked: hidden unless a player-team detector
///   (Assembler / Trojan / Gateway with FBI `RadarDistance > 0`)
///   is within range.
///
/// Throttled to [`VISIBILITY_REFRESH_INTERVAL`].
pub fn update_cloak_visibility(
    time: Res<Time>,
    mut timer: ResMut<VisibilityRefreshTimer>,
    player: Res<PlayerTeam>,
    unit_registry: Res<UnitRegistry>,
    detectors_q: Query<(&TeamId, &UnitType, &GlobalTransform), Without<Dying>>,
    mut cloaked_q: Query<(&TeamId, &GlobalTransform, &mut Visibility), With<Cloaked>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < VISIBILITY_REFRESH_INTERVAL {
        return;
    }
    timer.0 = 0.0;

    let detectors: Vec<(Vec3, f32)> = detectors_q
        .iter()
        .filter(|(team, _, _)| team.0 == player.0)
        .filter_map(|(_, ut, gtf)| {
            let radar = unit_registry.radar_distance(ut.0);
            (radar > 0.0).then(|| (gtf.translation(), radar * radar))
        })
        .collect();

    for (team, gtf, mut vis) in &mut cloaked_q {
        if team.0 == player.0 {
            if *vis != Visibility::Visible {
                *vis = Visibility::Visible;
            }
            continue;
        }
        let pos = gtf.translation();
        let detected = detectors
            .iter()
            .any(|(dp, radar_sq)| pos.distance_squared(*dp) <= *radar_sq);
        let target = if detected {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *vis != target {
            *vis = target;
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

/// Active fog-of-war over non-cloaked units, applied from the
/// [`PlayerTeam`]'s perspective. Friendly units (team == player)
/// stay visible. Enemy units are visible iff *currently* within
/// any friendly unit's FBI `SightDistance`; the [`Spotted`] marker
/// is added/removed in lockstep with that visibility so downstream
/// systems (minimap dot filter) can read it cheaply. Sight is
/// revoked when the friendly observer dies or moves away — full
/// LoS, not the memory variant.
///
/// AI-controlled teams ignore this entirely; their decision systems
/// query the world directly.
///
/// Throttled to [`VISIBILITY_REFRESH_INTERVAL`]; partitioned from
/// `update_cloak_visibility` via `Without<Cloaked>` so neither
/// system writes `Visibility` on the same entity.
#[allow(clippy::type_complexity)]
pub fn update_fog_visibility(
    time: Res<Time>,
    mut timer: Local<f32>,
    player: Res<PlayerTeam>,
    unit_registry: Res<UnitRegistry>,
    viewers_q: Query<(&TeamId, &UnitType, &GlobalTransform), Without<Dying>>,
    mut targets_q: Query<
        (
            Entity,
            &TeamId,
            &GlobalTransform,
            &mut Visibility,
            Has<Spotted>,
        ),
        (With<UnitType>, Without<Cloaked>),
    >,
    mut commands: Commands,
) {
    *timer += time.delta_secs();
    if *timer < VISIBILITY_REFRESH_INTERVAL {
        return;
    }
    *timer = 0.0;

    // Player-team viewers and their sight-radius squares. Empty if
    // the player has no units alive (then everything is hidden).
    let viewers: Vec<(Vec3, f32)> = viewers_q
        .iter()
        .filter(|(team, _, _)| team.0 == player.0)
        .map(|(_, ut, gtf)| {
            let sight = unit_registry.sight_distance(ut.0);
            (gtf.translation(), sight * sight)
        })
        .collect();

    for (entity, team, gtf, mut vis, was_spotted) in &mut targets_q {
        // Friendly units always visible to the player; their Spotted
        // marker stays in sync so the minimap shows them.
        if team.0 == player.0 {
            if *vis != Visibility::Visible {
                *vis = Visibility::Visible;
            }
            if !was_spotted {
                commands.entity(entity).insert(Spotted);
            }
            continue;
        }

        let pos = gtf.translation();
        let in_sight = viewers
            .iter()
            .any(|(vp, sight_sq)| pos.distance_squared(*vp) <= *sight_sq);

        if in_sight {
            if !was_spotted {
                commands.entity(entity).insert(Spotted);
            }
            if *vis != Visibility::Visible {
                *vis = Visibility::Visible;
            }
        } else {
            if was_spotted {
                commands.entity(entity).remove::<Spotted>();
            }
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        }
    }
}

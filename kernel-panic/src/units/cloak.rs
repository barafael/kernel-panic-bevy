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

use super::components::{TeamId, UnitType};
use super::game_over::PlayerTeam;
use super::unit_registry::UnitRegistry;

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
    player_team: Res<PlayerTeam>,
    unit_registry: Res<UnitRegistry>,
    detectors: Query<(&UnitType, &TeamId, &GlobalTransform)>,
    mut cloaked: Query<(&TeamId, &GlobalTransform, &mut Visibility), With<Cloaked>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < VISIBILITY_REFRESH_INTERVAL {
        return;
    }
    timer.0 = 0.0;

    // Snapshot every unit that could act as a detector. Done once per
    // frame so the nested loop is O(cloaked * detectors) with no
    // duplicate component lookups.
    let scanners: Vec<(u8, f32, Vec3)> = detectors
        .iter()
        .filter_map(|(ut, team, gtf)| {
            let range = unit_registry.detector_range(ut.0);
            if range > 0.0 {
                Some((team.0, range * range, gtf.translation()))
            } else {
                None
            }
        })
        .collect();

    for (team, gtf, mut visibility) in &mut cloaked {
        let pos = gtf.translation();
        let detected = scanners
            .iter()
            .any(|(scanner_team, range_sq, scanner_pos)| {
                *scanner_team != team.0 && scanner_pos.distance_squared(pos) <= *range_sq
            });

        // Friendly cloaked units stay visible to the player so you can
        // manage your own Worms / Logic Bombs.
        let friendly = team.0 == player_team.0;
        let new_vis = if friendly || detected {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != new_vis {
            *visibility = new_vis;
        }
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
    player_team: Res<PlayerTeam>,
    unit_registry: Res<UnitRegistry>,
    observers: Query<(&UnitType, &TeamId, &GlobalTransform)>,
    mut targets: Query<
        (
            Entity,
            &TeamId,
            &GlobalTransform,
            &mut Visibility,
            Option<&Spotted>,
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

    // Snapshot player-team observers (squared ranges for cheap tests).
    // AI teams see the map perfectly — fog only affects what the human
    // player renders, matching the module-level doc.
    let scouts: Vec<(f32, Vec3)> = observers
        .iter()
        .filter_map(|(ut, team, gtf)| {
            if team.0 != player_team.0 {
                return None;
            }
            let range = unit_registry.sight_distance(ut.0);
            if range > 0.0 {
                Some((range * range, gtf.translation()))
            } else {
                None
            }
        })
        .collect();

    for (entity, team, gtf, mut visibility, spotted) in &mut targets {
        if team.0 == player_team.0 {
            if *visibility != Visibility::Visible {
                *visibility = Visibility::Visible;
            }
            continue;
        }

        let already = spotted.is_some();
        let in_sight = !already
            && scouts.iter().any(|(range_sq, scout_pos)| {
                scout_pos.distance_squared(gtf.translation()) <= *range_sq
            });
        if in_sight {
            commands.entity(entity).insert(Spotted);
        }

        let visible = already || in_sight;
        let new_vis = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != new_vis {
            *visibility = new_vis;
        }
    }
}

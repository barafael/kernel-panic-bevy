//! Cloak / detector visibility.
//!
//! Logic Bombs (`Init_Cloaked=1` in the FBI) and Worms (stealth ambusher
//! per upstream design) are hidden from the player until an enemy
//! detector — Assembler / Trojan / Gateway with non-zero FBI
//! `RadarDistance` — comes within range.
//!
//! Without fog of war (§6) we can't fully simulate per-team vision, so
//! this system only affects rendering: cloaked enemy units are faded
//! out when no player-owned detector is nearby. AI teams have perfect
//! information internally.

use bevy::prelude::*;

use super::components::{Faction, TeamId, UnitType};
use super::game_over::PlayerTeam;
use super::unit_registry::UnitRegistry;

/// Marker: this unit hides from enemies unless a detector is close.
/// Worms carry it permanently; Logic Bombs carry it until they detonate.
#[derive(Component)]
pub struct Cloaked;

/// Seconds between cloak-visibility scans. A cloaked unit being revealed
/// ~80ms late (one detector step) is well below the perception threshold,
/// while every-frame scanning is an O(cloaked × detectors) pass the game
/// doesn't need at 60 Hz. Matches the cadence of `count_small_buildings`.
const CLOAK_REFRESH_INTERVAL: f32 = 0.1;

/// Accumulated time since the last cloak scan. Kept as a resource so
/// the system can early-exit without doing any ECS iteration when the
/// timer hasn't elapsed.
#[derive(Resource, Default)]
pub struct CloakRefreshTimer(pub f32);

/// Decide whether every cloaked unit is spotted and set its `Visibility`
/// accordingly. A unit is spotted when any opposing team's detector is
/// within that detector's radar range. Throttled to
/// [`CLOAK_REFRESH_INTERVAL`] so this never contributes more than ~10
/// passes per second regardless of frame rate.
pub fn update_cloak_visibility(
    time: Res<Time>,
    mut timer: ResMut<CloakRefreshTimer>,
    player_team: Res<PlayerTeam>,
    unit_registry: Res<UnitRegistry>,
    detectors: Query<(&UnitType, &TeamId, &GlobalTransform)>,
    mut cloaked: Query<(&TeamId, &Faction, &GlobalTransform, &mut Visibility), With<Cloaked>>,
) {
    timer.0 += time.delta_secs();
    if timer.0 < CLOAK_REFRESH_INTERVAL {
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

    for (team, _faction, gtf, mut visibility) in &mut cloaked {
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

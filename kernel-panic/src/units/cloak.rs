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
use super::definitions::UnitKind;
use super::game_over::PlayerTeam;
use super::unit_registry::UnitRegistry;

/// Marker: this unit hides from enemies unless a detector is close.
/// Worms carry it permanently; Logic Bombs carry it until they detonate.
#[derive(Component)]
pub struct Cloaked;

/// Each frame, decide whether every cloaked unit is spotted
/// and set its `Visibility` accordingly. A unit is spotted when any
/// opposing team's detector is within that detector's radar range.
pub fn update_cloak_visibility(
    player_team: Res<PlayerTeam>,
    unit_registry: Res<UnitRegistry>,
    detectors: Query<(&UnitType, &TeamId, &GlobalTransform)>,
    mut cloaked: Query<(&TeamId, &Faction, &GlobalTransform, &mut Visibility), With<Cloaked>>,
) {
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

//! Minimal enemy AI.
//!
//! Runs a tick every `AI_TICK_INTERVAL` seconds per non-player team that
//! owns a homebase. The state machine is deliberately shallow — just
//! enough to make single-player a real game:
//!
//! 1. **Build**: when the homebase's production queue is short, enqueue
//!    a combat unit from the faction's basic roster.
//! 2. **Attack**: once the team has `ARMY_THRESHOLD` idle combat units,
//!    send every idle unit toward the nearest enemy homebase.
//!
//! Expand (constructors → datavents → secondary factories) and Defend
//! (recall on incursion) are deferred; they'll layer on once the basic
//! build+attack loop is proven in-game.

use bevy::prelude::*;

use super::components::{Faction, Homebase, TeamId, UnitType};
use super::definitions::UnitKind;
use super::game_over::PlayerTeam;
use super::production::Producer;
use crate::interaction::movement::{MovePath, MoveTarget};

/// Seconds between AI decisions. Once per second keeps per-frame cost
/// negligible and feels reactive enough for the slow KP pacing.
const AI_TICK_INTERVAL: f32 = 1.0;

/// Minimum idle combat units a team must have before it starts pushing.
/// Small enough that pressure builds early, large enough that the AI
/// doesn't trickle units into the meat grinder one at a time.
const ARMY_THRESHOLD: usize = 8;

/// Keep production queues short; re-queue as they drain. Prevents an
/// absurd backlog that locks the AI into building one unit type
/// forever.
const MAX_QUEUE_DEPTH: usize = 3;

/// Per-team AI tick accumulator so each team's brain runs at the same
/// cadence independent of frame rate.
#[derive(Resource, Default)]
pub struct AiTicker {
    accumulated: f32,
}

/// Main AI brain. Runs once per `AI_TICK_INTERVAL` regardless of frame
/// rate so decisions don't scale with render speed.
pub fn ai_brain(
    time: Res<Time>,
    mut ticker: ResMut<AiTicker>,
    player_team: Res<PlayerTeam>,
    mut homebases: Query<(&TeamId, &Faction, &GlobalTransform, &mut Producer), With<Homebase>>,
    combat_units: Query<
        (
            Entity,
            &TeamId,
            &UnitType,
            &GlobalTransform,
            Option<&MoveTarget>,
            Option<&MovePath>,
        ),
        Without<Homebase>,
    >,
    mut commands: Commands,
) {
    ticker.accumulated += time.delta_secs();
    if ticker.accumulated < AI_TICK_INTERVAL {
        return;
    }
    ticker.accumulated = 0.0;

    // Snapshot homebase positions per team so we can find the nearest
    // enemy homebase without aliasing the mutable producer query.
    let homebase_positions: Vec<(u8, Vec3)> = homebases
        .iter()
        .map(|(team, _, gtf, _)| (team.0, gtf.translation()))
        .collect();

    for (team, faction, homebase_gtf, mut producer) in &mut homebases {
        if team.0 == player_team.0 {
            continue;
        }

        // Count and collect this AI's idle combat units.
        let idle: Vec<(Entity, Vec3)> = combat_units
            .iter()
            .filter(|(_, t, ut, _, mt, mp)| {
                t.0 == team.0 && is_combat_unit(ut.0) && mt.is_none() && mp.is_none()
            })
            .map(|(e, _, _, gtf, _, _)| (e, gtf.translation()))
            .collect();

        // Keep the production queue topped up. When we're below the
        // army threshold we build aggressively; at or above, we slow
        // down so the pushed army isn't immediately thinned by the
        // homebase pulling units back home.
        if producer.queue().len() < MAX_QUEUE_DEPTH {
            producer.enqueue(basic_combat_unit(*faction));
        }

        if idle.len() < ARMY_THRESHOLD {
            continue;
        }

        // Attack phase: send idle units toward the nearest enemy
        // homebase. If no enemy homebase exists (unusual — game-over
        // would normally have triggered) we just hold position.
        let self_pos = homebase_gtf.translation();
        let Some(target) = nearest_enemy_homebase(team.0, &homebase_positions, self_pos) else {
            continue;
        };

        for (entity, _) in idle {
            commands.entity(entity).insert(MoveTarget(target));
        }
    }
}

/// Basic combat unit each faction can cheaply mass-produce. Matches the
/// upstream Kernel/Hole/Connection `canbuild1` entry in SIDEDATA.TDF.
fn basic_combat_unit(faction: Faction) -> UnitKind {
    match faction {
        Faction::System => UnitKind::Bit,
        Faction::Hacker => UnitKind::Bug,
        Faction::Network => UnitKind::Packet,
    }
}

/// Coarse combat-unit classifier. Excludes constructors, viruses (they
/// spawn from deaths and manage themselves), LogicBombs (suicide timing
/// is a command ability, not a rush unit), and support types.
fn is_combat_unit(kind: UnitKind) -> bool {
    matches!(
        kind,
        UnitKind::Bit
            | UnitKind::Byte
            | UnitKind::Pointer
            | UnitKind::Bug
            | UnitKind::Exploit
            | UnitKind::Worm
            | UnitKind::Dos
            | UnitKind::Packet
            | UnitKind::Signal
            | UnitKind::Flow
    )
}

fn nearest_enemy_homebase(own_team: u8, homebases: &[(u8, Vec3)], from: Vec3) -> Option<Vec3> {
    homebases
        .iter()
        .filter(|(t, _)| *t != own_team)
        .min_by(|(_, a), (_, b)| {
            from.distance_squared(*a)
                .partial_cmp(&from.distance_squared(*b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, pos)| *pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_enemy_skips_own_team() {
        let bases = [
            (0, Vec3::new(0.0, 0.0, 0.0)),
            (1, Vec3::new(100.0, 0.0, 0.0)),
            (2, Vec3::new(50.0, 0.0, 0.0)),
        ];
        // From team 0's origin, team 2 is closer than team 1.
        let target = nearest_enemy_homebase(0, &bases, Vec3::ZERO).unwrap();
        assert_eq!(target, Vec3::new(50.0, 0.0, 0.0));
    }

    #[test]
    fn nearest_enemy_none_when_alone() {
        let bases = [(0, Vec3::ZERO)];
        assert!(nearest_enemy_homebase(0, &bases, Vec3::ZERO).is_none());
    }

    #[test]
    fn combat_classifier_includes_basic_units() {
        assert!(is_combat_unit(UnitKind::Bit));
        assert!(is_combat_unit(UnitKind::Bug));
        assert!(is_combat_unit(UnitKind::Packet));
        assert!(is_combat_unit(UnitKind::Byte));
        assert!(!is_combat_unit(UnitKind::Assembler));
        assert!(!is_combat_unit(UnitKind::Virus));
        assert!(!is_combat_unit(UnitKind::LogicBomb));
        assert!(!is_combat_unit(UnitKind::Kernel));
    }

    #[test]
    fn basic_combat_unit_per_faction() {
        assert_eq!(basic_combat_unit(Faction::System), UnitKind::Bit);
        assert_eq!(basic_combat_unit(Faction::Hacker), UnitKind::Bug);
        assert_eq!(basic_combat_unit(Faction::Network), UnitKind::Packet);
    }
}

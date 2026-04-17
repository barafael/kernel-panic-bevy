//! Enemy AI.
//!
//! Runs a tick every `AI_TICK_INTERVAL` per non-player team that owns
//! a homebase. Each tick runs three phases in order:
//!
//! 1. **Build**: keep the homebase production queue topped up with
//!    basic combat units, occasionally inserting a constructor.
//! 2. **Expand**: route any idle constructor to the nearest unclaimed
//!    datavent and queue the appropriate secondary factory.
//! 3. **Defend / Attack**: if an enemy unit is within
//!    `DEFEND_RADIUS` of any friendly homebase, recall idle combat
//!    units to the threatened base. Otherwise, once ARMY_THRESHOLD
//!    idle units exist, send them at the nearest enemy homebase.

use bevy::prelude::*;

use super::components::{Faction, Homebase, TeamId, UnitType};
use super::construction::{Constructing, PendingBuild, is_constructor};
use super::definitions::UnitKind;
use super::game_over::PlayerTeam;
use super::production::Producer;
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::terrain::geovent::GeoventSmoker;

/// Seconds between AI decisions.
const AI_TICK_INTERVAL: f32 = 1.0;

/// Minimum idle combat units a team must have before it starts pushing.
const ARMY_THRESHOLD: usize = 8;

/// Keep production queues short; re-queue as they drain.
const MAX_QUEUE_DEPTH: usize = 3;

/// If any non-friendly unit is inside this distance of a friendly
/// homebase, idle combat units recall home instead of pushing out.
const DEFEND_RADIUS: f32 = 700.0;

/// Max distance a datavent can be from an existing friendly building
/// before we consider it "unclaimed". Also the radius the AI checks
/// when deciding whether to send a constructor.
const DATAVENT_CLAIM_RADIUS: f32 = 120.0;

/// Ratio of combat-unit orders to constructor orders when refilling the
/// queue. One constructor per four combat units keeps the build curve
/// aggressive without starving expansion.
const CONSTRUCTOR_EVERY: u32 = 4;

/// Per-team AI tick accumulator.
#[derive(Resource, Default)]
pub struct AiTicker {
    accumulated: f32,
    /// Per-team counter of how many times we've queued a combat unit
    /// since the last constructor — used to sequence builds.
    combat_units_since_constructor: std::collections::HashMap<u8, u32>,
}

/// Main AI brain. Splits into helpers so each phase reads top-to-bottom.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
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
    constructors: Query<
        (
            Entity,
            &TeamId,
            &UnitType,
            &GlobalTransform,
            Option<&MoveTarget>,
            Option<&PendingBuild>,
            Option<&Constructing>,
        ),
        Without<Homebase>,
    >,
    buildings: Query<(&TeamId, &UnitType, &GlobalTransform)>,
    datavents: Query<&GeoventSmoker>,
    mut commands: Commands,
) {
    ticker.accumulated += time.delta_secs();
    if ticker.accumulated < AI_TICK_INTERVAL {
        return;
    }
    ticker.accumulated = 0.0;

    let homebase_positions: Vec<(u8, Vec3)> = homebases
        .iter()
        .map(|(team, _, gtf, _)| (team.0, gtf.translation()))
        .collect();

    for (team, faction, homebase_gtf, mut producer) in &mut homebases {
        if team.0 == player_team.0 {
            continue;
        }

        queue_builds(team.0, *faction, &mut producer, &mut ticker);

        dispatch_constructor(
            team.0,
            *faction,
            &constructors,
            &buildings,
            &datavents,
            &mut commands,
        );

        let idle: Vec<(Entity, Vec3)> = combat_units
            .iter()
            .filter(|(_, t, ut, _, mt, mp)| {
                t.0 == team.0 && is_combat_unit(ut.0) && mt.is_none() && mp.is_none()
            })
            .map(|(e, _, _, gtf, _, _)| (e, gtf.translation()))
            .collect();

        if let Some(threat) = homebase_under_threat(team.0, &homebase_positions, &combat_units) {
            for (entity, _) in idle {
                commands.entity(entity).insert(MoveTarget(threat));
            }
            continue;
        }

        if idle.len() < ARMY_THRESHOLD {
            continue;
        }

        let self_pos = homebase_gtf.translation();
        let Some(target) = nearest_enemy_homebase(team.0, &homebase_positions, self_pos) else {
            continue;
        };
        for (entity, _) in idle {
            commands.entity(entity).insert(MoveTarget(target));
        }
    }
}

fn queue_builds(team: u8, faction: Faction, producer: &mut Producer, ticker: &mut AiTicker) {
    if producer.queue().len() >= MAX_QUEUE_DEPTH {
        return;
    }
    let counter = ticker
        .combat_units_since_constructor
        .entry(team)
        .or_default();
    if *counter >= CONSTRUCTOR_EVERY {
        producer.enqueue(constructor_unit(faction));
        *counter = 0;
    } else {
        producer.enqueue(basic_combat_unit(faction));
        *counter += 1;
    }
}

#[allow(clippy::type_complexity)]
fn dispatch_constructor(
    team: u8,
    faction: Faction,
    constructors: &Query<
        (
            Entity,
            &TeamId,
            &UnitType,
            &GlobalTransform,
            Option<&MoveTarget>,
            Option<&PendingBuild>,
            Option<&Constructing>,
        ),
        Without<Homebase>,
    >,
    buildings: &Query<(&TeamId, &UnitType, &GlobalTransform)>,
    datavents: &Query<&GeoventSmoker>,
    commands: &mut Commands,
) {
    let Some((entity, ctor_pos)) = constructors.iter().find_map(|(e, t, ut, gtf, mt, pb, c)| {
        if t.0 == team && is_constructor(ut.0) && mt.is_none() && pb.is_none() && c.is_none() {
            Some((e, gtf.translation()))
        } else {
            None
        }
    }) else {
        return;
    };

    let Some(site) = nearest_unclaimed_datavent(ctor_pos, buildings, datavents) else {
        return;
    };

    let kind = secondary_factory(faction);
    commands
        .entity(entity)
        .insert(MoveTarget(site))
        .insert(PendingBuild { kind, site });
}

fn nearest_unclaimed_datavent(
    from: Vec3,
    buildings: &Query<(&TeamId, &UnitType, &GlobalTransform)>,
    datavents: &Query<&GeoventSmoker>,
) -> Option<Vec3> {
    let claim_sq = DATAVENT_CLAIM_RADIUS * DATAVENT_CLAIM_RADIUS;
    let building_positions: Vec<Vec3> = buildings
        .iter()
        .filter(|(_, ut, _)| is_building(ut.0))
        .map(|(_, _, gtf)| gtf.translation())
        .collect();
    datavents
        .iter()
        .map(|vent| vent.pos)
        .filter(|vent_pos| {
            building_positions
                .iter()
                .all(|b| b.distance_squared(*vent_pos) > claim_sq)
        })
        .min_by(|a, b| {
            from.distance_squared(*a)
                .partial_cmp(&from.distance_squared(*b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

#[allow(clippy::type_complexity)]
fn homebase_under_threat(
    team: u8,
    homebases: &[(u8, Vec3)],
    combat_units: &Query<
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
) -> Option<Vec3> {
    let radius_sq = DEFEND_RADIUS * DEFEND_RADIUS;
    homebases
        .iter()
        .find(|(t, _)| *t == team)
        .and_then(|(_, base_pos)| {
            let under_fire = combat_units.iter().any(|(_, t, _, gtf, _, _)| {
                t.0 != team && gtf.translation().distance_squared(*base_pos) < radius_sq
            });
            under_fire.then_some(*base_pos)
        })
}

fn basic_combat_unit(faction: Faction) -> UnitKind {
    match faction {
        Faction::System => UnitKind::Bit,
        Faction::Hacker => UnitKind::Bug,
        Faction::Network => UnitKind::Packet,
    }
}

fn constructor_unit(faction: Faction) -> UnitKind {
    match faction {
        Faction::System => UnitKind::Assembler,
        Faction::Hacker => UnitKind::Trojan,
        Faction::Network => UnitKind::Gateway,
    }
}

fn secondary_factory(faction: Faction) -> UnitKind {
    match faction {
        Faction::System => UnitKind::Socket,
        Faction::Hacker => UnitKind::Window,
        Faction::Network => UnitKind::Port,
    }
}

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

fn is_building(kind: UnitKind) -> bool {
    matches!(
        kind,
        UnitKind::Kernel
            | UnitKind::Hole
            | UnitKind::Connection
            | UnitKind::Socket
            | UnitKind::Window
            | UnitKind::Port
            | UnitKind::Firewall
            | UnitKind::Terminal
            | UnitKind::Obelisk
            | UnitKind::BadBlock
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

    #[test]
    fn constructor_per_faction() {
        assert_eq!(constructor_unit(Faction::System), UnitKind::Assembler);
        assert_eq!(constructor_unit(Faction::Hacker), UnitKind::Trojan);
        assert_eq!(constructor_unit(Faction::Network), UnitKind::Gateway);
    }

    #[test]
    fn secondary_factory_per_faction() {
        assert_eq!(secondary_factory(Faction::System), UnitKind::Socket);
        assert_eq!(secondary_factory(Faction::Hacker), UnitKind::Window);
        assert_eq!(secondary_factory(Faction::Network), UnitKind::Port);
    }
}

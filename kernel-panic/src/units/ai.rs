//! Enemy AI — a Fair-KPAI-inspired loop running one tick per second per
//! non-local team that owns a homebase.
//!
//! Each tick runs four phases in order:
//!
//! 1. **Build**: keep the homebase production queue topped up with basic
//!    combat units, occasionally inserting a constructor. Fairness cap:
//!    once this team's army outnumbers the largest enemy army by
//!    [`FAIRNESS_SLACK`], combat production pauses (constructors keep
//!    flowing — expansion is never "unfair").
//! 2. **Expand**: route any idle constructor to the nearest unclaimed
//!    datavent and queue the faction's secondary factory (minifacs
//!    auto-spam via `production::autospam_minifacs`).
//! 3. **Defend**: if any enemy unit is within `DEFEND_RADIUS` of the
//!    homebase, recall idle combat units to it.
//! 4. **Attack**: otherwise, once `ARMY_THRESHOLD` idle combat units
//!    exist, send them at the nearest enemy homebase.

use std::collections::HashMap;

use bevy::prelude::*;

use super::{
    components::{Faction, Homebase, TeamId, UnitType},
    construction::{Constructing, PendingBuild},
    player::LocalTeam,
    production::Producer,
};
use crate::{
    game_setup::AiDifficulty,
    interaction::movement::{MovePath, MoveTarget},
    terrain::geovent::{GeoventSmoker, VentClaim},
};

/// Seconds between AI decisions.
const AI_TICK_INTERVAL: f32 = 1.0;

/// Minimum idle combat units a team must have before it starts pushing.
const ARMY_THRESHOLD: usize = 8;

/// Keep production queues short; re-queue as they drain.
const MAX_QUEUE_DEPTH: usize = 3;

/// If any enemy unit is inside this distance of the homebase, idle
/// combat units recall home instead of pushing out.
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
    combat_units_since_constructor: HashMap<u8, u32>,
}

/// One combat unit's state captured at the top of an AI tick. Named so
/// downstream loops can read `.pos` / `.idle` instead of tuple indices.
struct CombatSnapshot {
    entity: Entity,
    team: u8,
    is_combat: bool,
    pos: Vec3,
    idle: bool,
}

/// Scratch buffers reused across AI ticks so the 1 Hz snapshot rebuild
/// doesn't re-allocate.
#[derive(Default)]
pub struct AiScratch {
    homebase_positions: Vec<(u8, Vec3)>,
    datavent_positions: Vec<Vec3>,
    building_positions: Vec<Vec3>,
    combat_snapshot: Vec<CombatSnapshot>,
    army_counts: HashMap<u8, usize>,
    idle: Vec<(Entity, Vec3)>,
}

/// Main AI brain. Splits into helpers so each phase reads top-to-bottom.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn ai_brain(
    time: Res<Time>,
    mut ticker: ResMut<AiTicker>,
    mut scratch: Local<AiScratch>,
    local: Res<LocalTeam>,
    difficulty: Res<AiDifficulty>,
    mut homebases: Query<(&TeamId, &Faction, &GlobalTransform, &mut Producer), With<Homebase>>,
    mobiles: Query<
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
    datavents: Query<&GeoventSmoker, Without<VentClaim>>,
    mut commands: Commands,
) {
    ticker.accumulated += time.delta_secs();
    if ticker.accumulated < AI_TICK_INTERVAL {
        return;
    }
    ticker.accumulated = 0.0;

    let scratch: &mut AiScratch = &mut scratch;
    scratch.homebase_positions.clear();
    scratch
        .homebase_positions
        .extend(homebases.iter().map(|(t, _, g, _)| (t.0, g.translation())));

    scratch.datavent_positions.clear();
    scratch
        .datavent_positions
        .extend(datavents.iter().map(|v| v.pos));

    scratch.building_positions.clear();
    scratch.building_positions.extend(
        buildings
            .iter()
            .filter(|(_, ut, _)| ut.0.is_building())
            .map(|(_, _, g)| g.translation()),
    );

    scratch.combat_snapshot.clear();
    scratch.army_counts.clear();
    scratch.combat_snapshot.extend(mobiles.iter().map(
        |(e, t, ut, g, mt, mp)| {
            let is_combat = ut.0.is_combat_unit();
            if is_combat {
                *scratch.army_counts.entry(t.0).or_default() += 1;
            }
            CombatSnapshot {
                entity: e,
                team: t.0,
                is_combat,
                pos: g.translation(),
                idle: mt.is_none() && mp.is_none(),
            }
        },
    ));

    for (team, faction, homebase_gtf, mut producer) in &mut homebases {
        if team.0 == local.0 {
            continue;
        }

        let strongest_enemy_army = scratch
            .army_counts
            .iter()
            .filter(|(t, _)| **t != team.0)
            .map(|(_, c)| *c)
            .max()
            .unwrap_or(0);
        // Fairness (upstream "Fair KPAI"): stop out-producing the
        // strongest enemy army by more than the difficulty's slack.
        // Expansion and defense still run — the cap only limits
        // snowballing.
        let allow_combat = scratch.army_counts.get(&team.0).copied().unwrap_or(0)
            < strongest_enemy_army + difficulty.fairness_slack();

        queue_builds(
            team.0,
            *faction,
            &mut producer,
            &mut ticker,
            allow_combat,
        );

        dispatch_constructor(
            team.0,
            *faction,
            &constructors,
            &scratch.building_positions,
            &scratch.datavent_positions,
            &mut commands,
        );

        scratch.idle.clear();
        scratch.idle.extend(
            scratch
                .combat_snapshot
                .iter()
                .filter(|c| c.team == team.0 && c.is_combat && c.idle)
                .map(|c| (c.entity, c.pos)),
        );

        if let Some(threat) = homebase_under_threat(
            team.0,
            &scratch.homebase_positions,
            &scratch.combat_snapshot,
        ) {
            assign_targets(&scratch.idle, threat, &mut commands);
            continue;
        }

        if scratch.idle.len() < ARMY_THRESHOLD {
            continue;
        }

        let self_pos = homebase_gtf.translation();
        let Some(target) = nearest_enemy_homebase(team.0, &scratch.homebase_positions, self_pos)
        else {
            continue;
        };
        assign_targets(&scratch.idle, target, &mut commands);
    }
}

/// Insert `MoveTarget(target)` on each entity. Extracted so the two
/// defend/attack branches share one inner loop.
fn assign_targets(idle: &[(Entity, Vec3)], target: Vec3, commands: &mut Commands) {
    for (entity, _) in idle {
        commands.entity(*entity).insert(MoveTarget(target));
    }
}

fn queue_builds(
    team: u8,
    faction: Faction,
    producer: &mut Producer,
    ticker: &mut AiTicker,
    allow_combat: bool,
) {
    if producer.queue().len() >= MAX_QUEUE_DEPTH {
        return;
    }
    let counter = ticker.combat_units_since_constructor.entry(team).or_default();
    if *counter >= CONSTRUCTOR_EVERY {
        producer.enqueue(faction.constructor());
        *counter = 0;
    } else if allow_combat {
        producer.enqueue(faction.basic_combat_unit());
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
    building_positions: &[Vec3],
    datavent_positions: &[Vec3],
    commands: &mut Commands,
) {
    let Some((entity, ctor_pos)) = constructors.iter().find_map(|(e, t, ut, gtf, mt, pb, c)| {
        if t.0 == team && ut.0.is_constructor() && mt.is_none() && pb.is_none() && c.is_none() {
            Some((e, gtf.translation()))
        } else {
            None
        }
    }) else {
        return;
    };

    let Some(site) = nearest_unclaimed_datavent(ctor_pos, building_positions, datavent_positions)
    else {
        return;
    };

    let kind = faction.secondary_factory();
    commands
        .entity(entity)
        .insert(MoveTarget(site))
        .insert(PendingBuild { kind, site });
}

fn nearest_unclaimed_datavent(
    from: Vec3,
    building_positions: &[Vec3],
    datavent_positions: &[Vec3],
) -> Option<Vec3> {
    let claim_sq = DATAVENT_CLAIM_RADIUS * DATAVENT_CLAIM_RADIUS;
    datavent_positions
        .iter()
        .copied()
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

fn homebase_under_threat(
    team: u8,
    homebases: &[(u8, Vec3)],
    combat_snapshot: &[CombatSnapshot],
) -> Option<Vec3> {
    let radius_sq = DEFEND_RADIUS * DEFEND_RADIUS;
    homebases
        .iter()
        .find(|(t, _)| *t == team)
        .and_then(|(_, base_pos)| {
            let under_fire = combat_snapshot
                .iter()
                .any(|c| c.team != team && c.pos.distance_squared(*base_pos) < radius_sq);
            under_fire.then_some(*base_pos)
        })
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
    fn fairness_cap_blocks_combat_but_not_expansion() {
        let mut ticker = AiTicker::default();
        let mut producer = Producer::new();
        let faction = Faction::System;

        // 4 combat units queued → the 5th build is a constructor.
        // (Drain as production would: MAX_QUEUE_DEPTH only limits the
        // backlog, not the cadence counter.)
        for _ in 0..4 {
            queue_builds(1, faction, &mut producer, &mut ticker, true);
            assert_eq!(producer.dequeue_front(), Some(faction.basic_combat_unit()));
        }
        queue_builds(1, faction, &mut producer, &mut ticker, true);
        assert_eq!(producer.queue().front().copied(), Some(faction.constructor()));

        // Unfair army: combat production pauses mid-cadence, but the
        // constructor that comes due still flows (expansion continues).
        let mut unfair = Producer::new();
        let mut ticker = AiTicker::default();
        for _ in 0..4 {
            queue_builds(1, faction, &mut unfair, &mut ticker, true);
            assert_eq!(unfair.dequeue_front(), Some(faction.basic_combat_unit()));
        }
        queue_builds(1, faction, &mut unfair, &mut ticker, false);
        assert_eq!(unfair.queue().front().copied(), Some(faction.constructor()));

        // And while the cap holds with nothing due, nothing is queued.
        queue_builds(1, faction, &mut unfair, &mut ticker, false);
        assert_eq!(unfair.queue().len(), 1);
    }
}

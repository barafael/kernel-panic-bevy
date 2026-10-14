//! Leaving play and taking over, in full headless matches (the user's
//! rule, 2026-10-15; `units::lifecycle::game_over`, `net::apply`):
//! resigning alone blows the side up within 4 s and ends the match; with
//! an ally in play the units wait for a TAKE; an AI ally takes them
//! itself; the last ally's resignation blows up everything left.
//!
//! Marble Madness, four seats at most. The first human seat is the
//! local player (issues through the bus); other human seats send their
//! commands as remote frames.

use std::collections::BTreeSet;

use bevy::prelude::*;

use super::apply::Refusals;
use super::{CommandBus, CommandFrame, SimCommand};
use crate::game_setup::PlayerSpec;
use crate::headless::replay::{MapInput, shuffle_storage, trail_text};
use crate::headless::{HashLog, Scenario, SimHarness};
use crate::units::combat::Dying;
use crate::units::components::{Faction, TeamId, UnitType};
use crate::units::lifecycle::bookkeeping::{MAX_UNITS_PER_TEAM, TeamUnitCounts};
use crate::units::lifecycle::game_over::{FUSE_FRAMES, LeaveFuse};
use crate::units::teams::TeamFates;

const MAP: &str = "Marble_Madness_Map";

/// Seats `(ally team, ai)` in seat order.
fn scenario(seats: &[(u8, bool)], seed: u64) -> Option<Scenario> {
    let map = MapInput::checkout(MAP).ok()?;
    let mut s = Scenario::ai_match(MAP, seats.len() as u8, 1, seed);
    s.seats = seats
        .iter()
        .enumerate()
        .map(|(i, &(ally_team, ai))| PlayerSpec {
            faction: Faction::ALL[i % 3],
            ally_team,
            ai,
        })
        .collect();
    s.map_file = Some(map.file);
    Some(s)
}

fn start(seats: &[(u8, bool)], seed: u64) -> Option<SimHarness> {
    let Some(s) = scenario(seats, seed) else {
        eprintln!("{MAP}.kpmap not baked: skipped");
        return None;
    };
    let mut h = SimHarness::new(s);
    h.world_mut().resource_mut::<HashLog>().enabled = true;
    Some(h)
}

/// `cmd` from `player`, applied on the next tick (a remote frame).
fn send(h: &mut SimHarness, player: u8, cmd: SimCommand) {
    let mut bus = h.world_mut().resource_mut::<CommandBus>();
    let tick = bus.tick;
    bus.deliver(CommandFrame {
        tick,
        player,
        commands: vec![cmd],
    });
}

/// `owner`'s live units (not dying).
fn units_of(h: &mut SimHarness, owner: u8) -> Vec<Entity> {
    let w = h.world_mut();
    let mut q = w.query_filtered::<(Entity, &TeamId), (With<UnitType>, Without<Dying>)>();
    q.iter(w)
        .filter(|(_, t)| t.0 == owner)
        .map(|(e, _)| e)
        .collect()
}

fn fuse(h: &mut SimHarness, e: Entity) -> Option<u64> {
    h.world_mut().get::<LeaveFuse>(e).map(|f| f.at)
}

fn refusals(h: &mut SimHarness) -> u64 {
    h.world_mut().resource::<Refusals>().count
}

/// `owner`'s homebase starts on four of its first roster unit (a
/// remote frame), so there is an army to blow up or take.
fn build_some(h: &mut SimHarness, owner: u8) {
    let w = h.world_mut();
    let mut q = w.query::<(&crate::net::NetId, &TeamId, &UnitType)>();
    let home = q
        .iter(w)
        .find(|(_, t, u)| t.0 == owner && u.0.is_homebase())
        .map(|(id, _, u)| (*id, u.0));
    if let Some((id, kind)) = home {
        let roster = crate::units::lifecycle::production::factory_roster(kind);
        send(
            h,
            owner,
            SimCommand::Factory {
                factories: super::Units::of([id]),
                kind: roster[0],
                op: super::FactoryOp::Add(4),
            },
        );
    }
}

fn steps(h: &mut SimHarness, n: u32) {
    for _ in 0..n {
        h.step();
    }
}

/// 1v1: the human resigns. Every one of its units blows up within
/// 120 ticks, not all on the same tick, and the match ends a census
/// after the last one, the AI winning.
#[test]
fn resign_alone_blows_up_and_ends() {
    let Some(mut h) = start(&[(0, false), (1, true)], 0x1e1) else {
        return;
    };
    h.step();
    build_some(&mut h, 0);
    steps(&mut h, 400);
    let mine = units_of(&mut h, 0);
    assert!(mine.len() >= 2, "a homebase and its first units");
    h.world_mut()
        .resource_mut::<CommandBus>()
        .issue(SimCommand::Resign);
    h.step();
    let at = h.world_mut().resource::<crate::sim::SimTick>().frame();
    let mut died: BTreeSet<u64> = BTreeSet::new();
    let mut alive = mine.clone();
    for _ in 0..=FUSE_FRAMES + 2 {
        h.step();
        let now = h.world_mut().resource::<crate::sim::SimTick>().frame();
        let still = units_of(&mut h, 0);
        if still.len() < alive.len() {
            died.insert(now);
        }
        alive = still;
    }
    assert!(alive.is_empty(), "everything blew up within 4 s");
    assert!(died.len() > 1, "at different ticks: {died:?}");
    assert!(died.iter().all(|t| *t <= at + u64::from(FUSE_FRAMES) + 2));
    steps(&mut h, 90);
    assert_eq!(
        h.world_mut().resource::<TeamFates>().winners(),
        Some(&[1][..])
    );
}

/// 2v2, both humans on one side: owner 1 resigns. Its units stay,
/// unfused; an enemy's Take is refused; owner 0 takes them all; a
/// second Take finds nothing; caps don't move. The same match with the
/// ECS storage reshuffled every tick gives the same trail.
#[test]
fn take_from_a_resigned_ally() {
    let run = |shuffle: bool| {
        let mut h = start(&[(0, false), (0, false), (1, true), (1, true)], 0x7a1)?;
        let mut rng = 0x5eed_u32;
        let mut step = |h: &mut SimHarness, n: u32| {
            for _ in 0..n {
                if shuffle {
                    shuffle_storage(h.world_mut(), &mut rng);
                }
                h.step();
            }
        };
        h.step();
        build_some(&mut h, 1);
        step(&mut h, 400);
        let theirs = units_of(&mut h, 1);
        assert!(theirs.len() > 2, "an army to take");
        assert!(!theirs.is_empty());
        let counted_before = {
            let c = h.world_mut().resource::<TeamUnitCounts>();
            (c.get(0), c.get(1))
        };
        send(&mut h, 1, SimCommand::Resign);
        step(&mut h, 40);
        assert!(h.world_mut().resource::<TeamFates>().is_dead(1));
        for e in &theirs {
            assert_eq!(fuse(&mut h, *e), None, "left for the ally");
        }
        assert_eq!(units_of(&mut h, 1).len(), theirs.len(), "nothing died");

        let r0 = refusals(&mut h);
        send(&mut h, 2, SimCommand::Take { from: 1 });
        step(&mut h, 1);
        assert_eq!(refusals(&mut h), r0 + 1, "an enemy can't take");
        assert_eq!(units_of(&mut h, 1).len(), theirs.len());

        h.world_mut()
            .resource_mut::<CommandBus>()
            .issue(SimCommand::Take { from: 1 });
        step(&mut h, 1);
        assert!(units_of(&mut h, 1).is_empty(), "all taken");
        for e in &theirs {
            if let Some(t) = h.world_mut().get::<TeamId>(*e) {
                assert_eq!(t.0, 0);
            }
        }
        let c = h.world_mut().resource::<TeamUnitCounts>();
        assert_eq!(c.get(1), 0, "the tally moved with the units");
        assert!(c.get(0) > counted_before.0);
        assert_eq!(c.limit(0), MAX_UNITS_PER_TEAM, "caps don't move");

        let r1 = refusals(&mut h);
        h.world_mut()
            .resource_mut::<CommandBus>()
            .issue(SimCommand::Take { from: 1 });
        step(&mut h, 1);
        assert_eq!(refusals(&mut h), r1 + 1, "nothing left to take");
        step(&mut h, 120);
        let ticks = h.world_mut().resource::<HashLog>().ticks.clone();
        Some(trail_text(&ticks))
    };
    let Some(plain) = run(false) else {
        return;
    };
    let shuffled = run(true).unwrap();
    assert_eq!(
        crate::headless::replay::first_difference(&plain, &shuffled),
        None,
        "the query order doesn't matter"
    );
}

/// Team Game: the human resigns, its AI ally takes its units at its
/// next brain tick (no human ally left to press TAKE).
#[test]
fn ai_ally_takes() {
    let Some(mut h) = start(&[(0, false), (0, true), (1, true)], 0xa1) else {
        return;
    };
    h.step();
    build_some(&mut h, 0);
    steps(&mut h, 400);
    let mine = units_of(&mut h, 0);
    h.world_mut()
        .resource_mut::<CommandBus>()
        .issue(SimCommand::Resign);
    steps(&mut h, 70);
    assert!(units_of(&mut h, 0).is_empty(), "taken");
    let taken = mine
        .iter()
        .filter(|e| h.world_mut().get::<TeamId>(**e).is_some_and(|t| t.0 == 1))
        .count();
    assert!(taken > 0, "the AI ally owns them");
    for e in mine {
        assert_eq!(fuse(&mut h, e), None);
    }
}

/// The last active owner of a side resigns after its ally: everything
/// left of the side — the first leaver's untaken units too — blows up.
#[test]
fn last_ally_resigning_blows_up_everything() {
    let Some(mut h) = start(&[(0, false), (0, false), (1, true)], 0x1a5) else {
        return;
    };
    steps(&mut h, 300);
    send(&mut h, 1, SimCommand::Resign);
    steps(&mut h, 10);
    let first = units_of(&mut h, 1);
    assert!(!first.is_empty());
    assert!(
        first
            .iter()
            .all(|e| h.world_mut().get::<LeaveFuse>(*e).is_none())
    );
    h.world_mut()
        .resource_mut::<CommandBus>()
        .issue(SimCommand::Resign);
    steps(&mut h, 2);
    for owner in [0, 1] {
        for e in units_of(&mut h, owner) {
            assert!(fuse(&mut h, e).is_some(), "owner {owner}'s units fused");
        }
    }
    steps(&mut h, FUSE_FRAMES + 2);
    assert!(units_of(&mut h, 0).is_empty() && units_of(&mut h, 1).is_empty());
}

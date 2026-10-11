//! The golden replays of the determinism matrix
//! (`kernel-panic/tests/golden/*.kpreplay`, `.github/workflows/determinism.yml`)
//! and how they are recorded.
//!
//! Two long all-AI matches (a 4v4 and an 8-player FFA, 18 000 ticks =
//! 10 game minutes each): the AI runs inside the sim, so these logs are
//! setup and seed only. And one short match with three human seats, one
//! per faction, whose scripted commands go through every [`SimCommand`]
//! kind (orders, per-unit and path moves, patrol, guard, builds, the
//! factory queue, repeat, auto-hold, manual targets, deploy, enter,
//! casts, self-destruct) against an AI opponent. The first human seat
//! is the local player and issues through [`CommandBus::issue`] (the
//! input systems' path); the other two are delivered as remote frames.
//!
//! Recording also replays the fresh log and requires the replay's trail
//! to equal the recording's: the bus's log really captures the match.
//! Re-record after a change that alters the sim:
//!
//! ```text
//! cargo run --release -p kernel-panic --features replay -- --record-golden
//! ```

use bevy::prelude::*;

use super::replay::{self, MapInput, MatchSetup, PlayOptions, Replay, ReplayError, Seat};
use super::{HashLog, SimHarness};
use crate::net::{
    CommandBus, CommandFrame, FactoryOp, NetId, NetPos, Order, PointOrder, PosList, SimCommand,
    Units,
};
use crate::terrain::geovent::GeoventSmoker;
use crate::units::combat::Dying;
use crate::units::components::{Homebase, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::construction::buildings_for;
use crate::units::lifecycle::production::factory_roster;

/// One golden log.
pub struct Golden {
    /// File stem in `kernel-panic/tests/golden/`.
    pub name: &'static str,
    pub setup: MatchSetup,
    pub ticks: u32,
    /// Drive the human seats with [`script`].
    pub scripted: bool,
}

/// Every golden log.
pub fn all() -> Vec<Golden> {
    let ai = |faction, team| Seat {
        faction,
        team,
        ai: true,
    };
    vec![
        // X-sim's reference scenario: 2 ally teams × 4 AI seats.
        Golden {
            name: "4v4_speed_balls",
            setup: MatchSetup {
                map: "Speed_Balls_16_Way".into(),
                seats: (0..8).map(|i| ai(i % 3, i / 4)).collect(),
                difficulty: 2,
                seed: 0x5eed,
                sandbox: false,
            },
            ticks: 18_000,
            scripted: false,
        },
        // Eight independent AI players at the hardest difficulty.
        Golden {
            name: "ffa8_quad_core",
            setup: MatchSetup {
                map: "Quad_Core".into(),
                seats: (0..8).map(|i| ai(i % 3, i)).collect(),
                difficulty: 4,
                seed: 0x0ff_a8,
                sandbox: false,
            },
            ticks: 18_000,
            scripted: false,
        },
        // Three scripted human seats (System, Hacker, Network) and an
        // AI System.
        Golden {
            name: "commands_marble",
            setup: MatchSetup {
                map: "Marble_Madness_Map".into(),
                seats: vec![
                    Seat {
                        faction: 0,
                        team: 0,
                        ai: false,
                    },
                    Seat {
                        faction: 1,
                        team: 1,
                        ai: false,
                    },
                    Seat {
                        faction: 2,
                        team: 2,
                        ai: false,
                    },
                    ai(0, 3),
                ],
                difficulty: 2,
                seed: 0xc0_33a2d,
                sandbox: false,
            },
            ticks: 3_000,
            scripted: true,
        },
    ]
}

/// Record `golden` from a live match on this checkout's map, replay the
/// result and check the two trails agree. Returns the replay and its
/// trail.
pub fn record(golden: &Golden) -> Result<(Replay, String), ReplayError> {
    let map = MapInput::checkout(&golden.setup.map)?;
    let map_bytes = map.bytes.clone();
    let mut h = SimHarness::new_with(golden.setup.scenario(Some(map.file)), map.assets);
    h.world_mut().resource_mut::<HashLog>().enabled = true;
    h.world_mut().resource_mut::<CommandBus>().log = Some(Vec::new());
    let local = golden.setup.seats.iter().find(|s| !s.ai).map(|s| s.team);
    while h.tick < golden.ticks as u64 {
        if golden.scripted {
            let world = h.world_mut();
            let tick = world.resource::<CommandBus>().tick;
            let commands = script(world, tick);
            let mut bus = world.resource_mut::<CommandBus>();
            for (player, cmds) in commands {
                if Some(player) == local {
                    for cmd in cmds {
                        bus.issue(cmd);
                    }
                } else if !cmds.is_empty() {
                    bus.deliver(CommandFrame {
                        tick,
                        player,
                        commands: cmds,
                    });
                }
            }
        }
        h.step();
    }
    let applied = h
        .world_mut()
        .resource_mut::<CommandBus>()
        .log
        .take()
        .unwrap_or_default();
    let recorded = replay::trail_text(&h.world_mut().resource::<HashLog>().ticks);
    drop(h);

    let replay = Replay::record(golden.setup.clone(), &map_bytes, golden.ticks, applied);
    // Through the file format, as the matrix will read it.
    let replay = Replay::decode(&replay.encode())?;
    let played = bevy::tasks::block_on(replay::play(
        &replay,
        MapInput::checkout(&golden.setup.map)?,
        &PlayOptions::default(),
    ))?;
    let trail = replay::trail_text(&played.hashes);
    if let Some(line) = replay::first_difference(&recorded, &trail) {
        return Err(ReplayError::Check(format!(
            "{}: the replay diverges from its recording at line {line}:\n  recorded {}\n  replayed {}",
            golden.name,
            recorded.lines().nth(line - 1).unwrap_or("<end>"),
            trail.lines().nth(line - 1).unwrap_or("<end>"),
        )));
    }
    Ok((replay, trail))
}

/// A live unit, as the script sees it.
#[derive(Clone, Copy)]
struct Unit {
    id: NetId,
    team: u8,
    kind: UnitKind,
    pos: Vec3,
    homebase: bool,
}

fn units(world: &mut World) -> Vec<Unit> {
    let mut q = world
        .query_filtered::<(&NetId, &TeamId, &UnitType, &Transform, Has<Homebase>), Without<Dying>>(
        );
    let mut out: Vec<Unit> = q
        .iter(world)
        .map(|(id, team, ut, tf, homebase)| Unit {
            id: *id,
            team: team.0,
            kind: ut.0,
            pos: tf.translation,
            homebase,
        })
        .collect();
    out.sort_by_key(|u| u.id);
    out
}

/// The scripted human players' commands for bus tick `tick`, per team.
/// Everything is chosen from the live state (unit ids, positions), so
/// the script adapts to whatever the sim did; the log records the
/// result.
fn script(world: &mut World, tick: u32) -> Vec<(u8, Vec<SimCommand>)> {
    const HUMANS: [u8; 3] = [0, 1, 2];
    const ENEMY: u8 = 3;
    // The script acts on these ticks only (cheap elsewhere).
    const ACTS: [u32; 20] = [
        1, 450, 500, 510, 700, 760, 820, 880, 940, 1000, 1100, 1150, 1200, 1250, 1300, 1400, 1500,
        1600, 2200, 2400,
    ];
    if !ACTS.contains(&tick) {
        return Vec::new();
    }
    let all = units(world);
    let mut vents: Vec<Vec3> = {
        let mut q = world.query::<&GeoventSmoker>();
        q.iter(world).map(|g| g.pos).collect()
    };
    vents.sort_by(|a, b| (a.x, a.z).partial_cmp(&(b.x, b.z)).unwrap());
    let bases: Vec<Unit> = all.iter().copied().filter(|u| u.homebase).collect();
    let center = bases.iter().map(|u| u.pos).sum::<Vec3>() / bases.len().max(1) as f32;
    let enemy_base = bases.iter().find(|u| u.team == ENEMY).copied();

    let mut out = Vec::new();
    for team in HUMANS {
        let Some(home) = bases.iter().find(|u| u.team == team).copied() else {
            continue;
        };
        let roster = factory_roster(home.kind);
        let builder_kind = roster
            .iter()
            .copied()
            .find(|k| !buildings_for(*k).is_empty());
        let own: Vec<Unit> = all.iter().copied().filter(|u| u.team == team).collect();
        let builders: Vec<NetId> = own
            .iter()
            .filter(|u| Some(u.kind) == builder_kind)
            .map(|u| u.id)
            .collect();
        let mobiles: Vec<Unit> = own
            .iter()
            .copied()
            .filter(|u| !u.homebase && roster.contains(&u.kind) && Some(u.kind) != builder_kind)
            .collect();
        let ids = |v: &[Unit]| Units::of(v.iter().map(|u| u.id));
        let to_center = (center - home.pos).normalize_or_zero();
        let rally = home.pos + to_center * 220.0;
        let side = Vec3::new(-to_center.z, 0.0, to_center.x);
        let p = NetPos::quantize;
        let mut cmds = Vec::new();
        match tick {
            1 => {
                let factory = || Units::of([home.id]);
                if let Some(b) = builder_kind {
                    cmds.push(SimCommand::Factory {
                        factories: factory(),
                        kind: b,
                        op: FactoryOp::Add(1),
                    });
                }
                cmds.push(SimCommand::Factory {
                    factories: factory(),
                    kind: roster[0],
                    op: FactoryOp::Add(6),
                });
                cmds.push(SimCommand::Factory {
                    factories: factory(),
                    kind: roster[1],
                    op: FactoryOp::AddFront(2),
                });
                cmds.push(SimCommand::Factory {
                    factories: factory(),
                    kind: roster[0],
                    op: FactoryOp::Remove {
                        count: 1,
                        oldest: false,
                    },
                });
                cmds.push(SimCommand::SetRepeat {
                    units: factory(),
                    on: true,
                });
            }
            450 => cmds.push(SimCommand::SetRepeat {
                units: Units::of([home.id]),
                on: false,
            }),
            500 => {
                // The builder's first building (a factory, on the free
                // vent nearest home), then a queued row of the second.
                if let Some(b) = builder_kind {
                    let options = buildings_for(b);
                    if let Some(vent) = vents.iter().copied().min_by(|a, c| {
                        a.distance_squared(home.pos)
                            .total_cmp(&c.distance_squared(home.pos))
                    }) {
                        cmds.push(SimCommand::Build {
                            builders: Units::of(builders.iter().copied()),
                            kind: options[0],
                            sites: PosList::quantize([vent]),
                            queue: false,
                        });
                    }
                    if let Some(&second) = options.get(1) {
                        let a = home.pos + side * 120.0;
                        cmds.push(SimCommand::Build {
                            builders: Units::of(builders.iter().copied()),
                            kind: second,
                            sites: PosList::quantize([a, a + side * 32.0]),
                            queue: true,
                        });
                    }
                }
            }
            510 => cmds.push(SimCommand::Factory {
                factories: Units::of([home.id]),
                kind: roster[0],
                op: FactoryOp::Remove {
                    count: 2,
                    oldest: true,
                },
            }),
            700 => {
                cmds.push(SimCommand::Order {
                    units: ids(&mobiles),
                    order: Order::Move(p(rally)),
                    queue: false,
                });
                // The same group again: the bus sends `Units::Prev`.
                cmds.push(SimCommand::Order {
                    units: ids(&mobiles),
                    order: Order::AttackMove(p(rally + to_center * 80.0)),
                    queue: true,
                });
            }
            760 => cmds.push(SimCommand::per_unit(
                PointOrder::AttackMove,
                mobiles
                    .iter()
                    .enumerate()
                    .map(|(i, u)| (u.id, rally + side * (i as f32 * 24.0 - 60.0)))
                    .collect(),
                false,
            )),
            820 => {
                if let Some(u) = mobiles.first() {
                    cmds.push(SimCommand::Path {
                        unit: u.id,
                        waypoints: PosList::quantize([
                            rally,
                            rally + side * 100.0,
                            rally + side * 100.0 + to_center * 100.0,
                        ]),
                        queue: false,
                    });
                }
            }
            880 => {
                let half = &mobiles[..mobiles.len() / 2];
                cmds.push(SimCommand::Order {
                    units: ids(half),
                    order: Order::Patrol(p(rally + side * 150.0)),
                    queue: false,
                });
            }
            940 => {
                if let Some(u) = mobiles.last() {
                    cmds.push(SimCommand::Order {
                        units: Units::of([u.id]),
                        order: Order::Guard(home.id),
                        queue: false,
                    });
                }
                if let Some(u) = mobiles.get(1) {
                    cmds.push(SimCommand::Stop {
                        units: Units::of([u.id]),
                    });
                }
                cmds.push(SimCommand::SetAutoHold {
                    units: ids(&mobiles),
                    on: true,
                });
            }
            1000 => {
                cmds.push(SimCommand::SetAutoHold {
                    units: ids(&mobiles),
                    on: false,
                });
                // Bugs ↔ Exploits (Hacker); a no-op for the others.
                cmds.push(SimCommand::Deploy {
                    units: ids(&mobiles),
                });
            }
            1100 => {
                if let Some(e) = enemy_base {
                    cmds.push(SimCommand::SetTarget {
                        units: ids(&mobiles),
                        target: e.id,
                    });
                }
            }
            1150 => cmds.push(SimCommand::UnsetTarget {
                units: ids(&mobiles),
            }),
            1200 => {
                if let Some(e) = enemy_base {
                    cmds.push(SimCommand::Order {
                        units: ids(&mobiles),
                        order: Order::AttackUnit(e.id),
                        queue: false,
                    });
                }
            }
            1250 => {
                if let (Some(e), Some(u)) = (enemy_base, mobiles.first()) {
                    cmds.push(SimCommand::Order {
                        units: Units::of([u.id]),
                        order: Order::AttackGround(p(e.pos + side * 60.0)),
                        queue: true,
                    });
                }
            }
            1300 => cmds.push(SimCommand::Deploy {
                units: ids(&mobiles),
            }),
            1400 => cmds.push(SimCommand::Enter {
                units: ids(&mobiles),
            }),
            1500 => {
                // Aimed abilities from everything this player owns
                // (command fire, Dispatch); units without one ignore it.
                if let Some(e) = enemy_base {
                    cmds.push(SimCommand::Cast {
                        units: ids(&own),
                        target: p(e.pos),
                        auto: true,
                    });
                }
            }
            1600 => {
                if let Some(e) = enemy_base {
                    cmds.push(SimCommand::Order {
                        units: ids(&mobiles),
                        order: Order::AttackMove(p(e.pos)),
                        queue: false,
                    });
                }
                cmds.push(SimCommand::Factory {
                    factories: Units::of([home.id]),
                    kind: roster[2],
                    op: FactoryOp::Add(3),
                });
            }
            2200 => {
                if let Some(u) = mobiles.first() {
                    cmds.push(SimCommand::SelfDestruct {
                        units: Units::of([u.id]),
                    });
                }
            }
            2400 => {
                if let Some(e) = enemy_base {
                    cmds.push(SimCommand::Order {
                        units: ids(&mobiles),
                        order: Order::AttackMove(p(e.pos)),
                        queue: false,
                    });
                }
            }
            _ => {}
        }
        out.push((team, cmds));
    }
    out
}

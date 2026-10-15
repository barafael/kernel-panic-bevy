//! Command fuzz: random command frames from every seat (and from
//! players without one) into a headless match, through the same
//! `deliver` → `apply_sim_commands` path a lockstep peer's frames take.
//!
//! Two seeded streams, generated from the live state each tick:
//!
//! - **S**, the chaos stream: every [`SimCommand`] variant with random
//!   units (own, other teams', dead, never-existing ids), points (on and
//!   a little off the map), kinds and counts, `Units::Prev`, pause and
//!   speed, a rare resignation (seat 3's), and frames whose encoding was mutated
//!   at random and still decoded. Validation drops what is invalid,
//!   the rest plays.
//! - **M**, the malformed stream: commands validation must refuse
//!   *whole*, leaving no trace — frames from unseated players, unit
//!   lists not sorted and unique, per-unit lists of the wrong length,
//!   points off the map, factory counts and speeds out of range,
//!   oversized build rows, cheats (the match has two humans), unit 0,
//!   and orders naming only other teams' or unknown units.
//!
//! [`check`] plays S twice, S with the ECS storage reshuffled every
//! tick (the query-order fuzz), and S with M mixed in. All four trails
//! must be identical, and M must have been refused command for command
//! ([`super::apply::Refusals`]). Any panic fails it too.
//!
//! The match: two human seats (delivered like remote players) and two
//! AI seats on Marble Madness. `cargo test -p kernel-panic net::fuzz`
//! runs it when the map is baked (skips otherwise); CI runs it through
//! the replay binary (`kernel-panic --command-fuzz`,
//! `determinism.yml`), where the maps are baked.

use bevy::prelude::*;

use crate::sim::DetFloat;
use strum::VariantArray;

use super::apply::{MAX_BUILD_SITES, MAX_FACTORY_COUNT, Refusals};
use super::control::{MAX_SPEED_PERCENT, MIN_SPEED_PERCENT};
use super::speed::SERVER_PLAYER;
use super::{
    CommandBus, CommandFrame, DevCommand, FactoryOp, IdList, MatchControl, NetId, NetPos, Order,
    PointOrder, PosList, SimCommand, Units,
};
use crate::headless::replay::{MapInput, MatchSetup, Seat, shuffle_storage, trail_text};
use crate::headless::{HashLog, SimHarness};
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::construction::buildings_for;
use crate::units::lifecycle::production::factory_roster;

/// The fuzz match's map (small, loads fast; the scripted golden's).
pub const MAP: &str = "Marble_Madness_Map";
/// Seats: humans 0 and 1, AI 2 and 3 ([`setup`]).
const SEATED: [u8; 4] = [0, 1, 2, 3];
const HUMANS: [u8; 2] = [0, 1];
/// Senders without a seat (255 is the spectator's team, and the
/// server's, which may send nothing but `InternalSpeed`).
const UNSEATED: [u8; 4] = [4, 9, 200, 255];

/// The fuzz match's setup.
pub fn setup(seed: u64) -> MatchSetup {
    let seat = |faction, ally_team, ai| Seat {
        faction,
        ally_team,
        ai,
    };
    MatchSetup {
        map: MAP.into(),
        seats: vec![
            seat(0, 0, false),
            seat(1, 1, false),
            seat(2, 2, true),
            seat(0, 3, true),
        ],
        difficulty: 2,
        seed,
        sandbox: false,
        rules: Default::default(),
    }
}

/// SplitMix64: the generators' dice (never the sim's).
struct Dice(u64);

impl Dice {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn pick<T: Copy>(&mut self, from: &[T]) -> Option<T> {
        (!from.is_empty()).then(|| from[self.below(from.len() as u64) as usize])
    }
}

/// One live unit as the generators see it.
#[derive(Clone, Copy)]
struct Unit {
    id: NetId,
    team: u8,
    kind: UnitKind,
    pos: Vec3,
}

/// What the generators read each tick (in `NetId` order).
struct View {
    units: Vec<Unit>,
    /// Map size in elmos.
    size: Vec2,
    /// The highest id ever handed out (ids below it that aren't live
    /// are dead).
    max_id: u32,
    resigned: Vec<u8>,
}

fn view(world: &mut World) -> View {
    let mut q = world.query_filtered::<(&NetId, &TeamId, &UnitType, &Transform), Without<Dying>>();
    let mut units: Vec<Unit> = q
        .iter(world)
        .map(|(id, t, ut, tf)| Unit {
            id: *id,
            team: t.0,
            kind: ut.0,
            pos: tf.translation,
        })
        .collect();
    units.sort_by_key(|u| u.id);
    let size = world
        .get_resource::<Heightmap>()
        .map_or(Vec2::splat(2048.0), |h| h.world_size().into());
    let max_id = units.last().map_or(0, |u| u.id.0).max(1);
    let resigned = world
        .resource::<MatchControl>()
        .resigned
        .iter()
        .copied()
        .collect();
    View {
        units,
        size,
        max_id,
        resigned,
    }
}

impl View {
    fn own(&self, team: u8) -> Vec<Unit> {
        self.units
            .iter()
            .copied()
            .filter(|u| u.team == team)
            .collect()
    }
    fn foreign(&self, team: u8) -> Vec<Unit> {
        self.units
            .iter()
            .copied()
            .filter(|u| u.team != team)
            .collect()
    }
    /// A point on the map, or (`off`) just beyond an edge.
    fn point(&self, d: &mut Dice, off: bool) -> NetPos {
        let (w, h) = (self.size.x, self.size.y);
        let mut p = Vec3::new(
            d.below(w as u64) as f32 + d.below(8) as f32 / 8.0,
            d.below(200) as f32,
            d.below(h as u64) as f32,
        );
        if off {
            match d.below(4) {
                0 => p.x = -1.0 - d.below(500) as f32,
                1 => p.x = w + 1.0 + d.below(500) as f32,
                2 => p.z = -1.0 - d.below(500) as f32,
                _ => p.z = h + 1.0 + d.below(500) as f32,
            }
        }
        NetPos::quantize(p)
    }
    /// A point near a unit (the usual order target).
    fn near(&self, d: &mut Dice, u: Unit) -> NetPos {
        let r = |d: &mut Dice| d.below(400) as f32 - 200.0;
        let p = u.pos + Vec3::new(r(d), 0.0, r(d));
        let p = Vec3::new(
            p.x.det_clamp(0.0, self.size.x),
            p.y,
            p.z.det_clamp(0.0, self.size.y),
        );
        NetPos::quantize(p)
    }
    /// An id that is dead or never existed.
    fn stale(&self, d: &mut Dice) -> NetId {
        let live = |id: u32| {
            self.units
                .binary_search_by_key(&NetId(id), |u| u.id)
                .is_ok()
        };
        for _ in 0..8 {
            let id = 1 + d.below(self.max_id as u64) as u32;
            if !live(id) {
                return NetId(id);
            }
        }
        NetId(self.max_id + 1 + d.below(1000) as u32)
    }
}

/// Some of `team`'s units, sometimes with others' and stale ids mixed in.
fn some_units(d: &mut Dice, v: &View, team: u8) -> Vec<NetId> {
    let own = v.own(team);
    let mut ids: Vec<NetId> = own.iter().filter(|_| d.chance(40)).map(|u| u.id).collect();
    if ids.is_empty()
        && let Some(u) = d.pick(&own)
    {
        ids.push(u.id);
    }
    if d.chance(20) {
        ids.extend(d.pick(&v.foreign(team)).map(|u| u.id));
    }
    if d.chance(15) {
        ids.push(v.stale(d));
    }
    ids
}

fn any_kind(d: &mut Dice) -> UnitKind {
    d.pick(UnitKind::VARIANTS).unwrap()
}

/// One command of the chaos stream for `team`; `prev`: an explicit list
/// was sent earlier in this frame, so `Units::Prev` may follow.
fn chaos_command(d: &mut Dice, v: &View, team: u8, tick: u32, prev: bool) -> SimCommand {
    let ids = some_units(d, v, team);
    let units = if prev && d.chance(30) {
        Units::Prev
    } else {
        Units::of(ids.iter().copied())
    };
    let own = v.own(team);
    let anchor = d.pick(&own).or_else(|| d.pick(&v.units));
    let point = |d: &mut Dice| match anchor {
        Some(u) if d.chance(80) => v.near(d, u),
        _ => {
            let off = d.chance(10);
            v.point(d, off)
        }
    };
    let target = |d: &mut Dice| match d.pick(&v.units) {
        Some(u) if d.chance(85) => u.id,
        _ => v.stale(d),
    };
    let kind_for = |d: &mut Dice, ids: &[NetId], builds: fn(UnitKind) -> &'static [UnitKind]| {
        let options: Vec<UnitKind> = ids
            .iter()
            .filter_map(|id| v.units.iter().find(|u| u.id == *id))
            .flat_map(|u| builds(u.kind).iter().copied())
            .collect();
        match d.pick(&options) {
            Some(k) if d.chance(85) => k,
            _ => any_kind(d),
        }
    };
    match d.below(23) {
        0..=3 => SimCommand::Order {
            units,
            order: match d.below(6) {
                0 => Order::Move(point(d)),
                1 => Order::AttackMove(point(d)),
                2 => Order::Patrol(point(d)),
                3 => Order::AttackGround(point(d)),
                4 => Order::AttackUnit(target(d)),
                _ => Order::Guard(target(d)),
            },
            queue: d.chance(40),
        },
        4 => {
            let pairs: Vec<(NetId, Vec3)> =
                ids.iter().map(|id| (*id, point(d).to_vec3())).collect();
            let order = if d.chance(50) {
                PointOrder::Move
            } else {
                PointOrder::AttackMove
            };
            SimCommand::per_unit(order, pairs, d.chance(30))
        }
        5 => SimCommand::Path {
            unit: ids.first().copied().unwrap_or_else(|| v.stale(d)),
            waypoints: PosList((0..1 + d.below(12)).map(|_| point(d)).collect()),
            queue: d.chance(30),
        },
        6 => SimCommand::Stop { units },
        7 | 8 => SimCommand::Build {
            kind: kind_for(d, &ids, buildings_for),
            builders: units,
            sites: PosList((0..1 + d.below(4)).map(|_| point(d)).collect()),
            queue: d.chance(40),
        },
        9 | 10 => SimCommand::Factory {
            kind: kind_for(d, &ids, factory_roster),
            factories: units,
            op: match d.below(3) {
                0 => FactoryOp::Add(d.below(8) as u16),
                1 => FactoryOp::AddFront(d.below(8) as u16),
                _ => FactoryOp::Remove {
                    count: d.below(8) as u16,
                    oldest: d.chance(50),
                },
            },
        },
        11 => SimCommand::SetRepeat {
            units,
            on: d.chance(50),
        },
        12 => SimCommand::SetAutoHold {
            units,
            on: d.chance(50),
        },
        13 => SimCommand::SetTarget {
            units,
            target: target(d),
        },
        14 => SimCommand::UnsetTarget { units },
        // Rarely, one unit at a time and never a factory, or the match
        // runs out of units (a team without factories is eliminated).
        15 if d.chance(20) => SimCommand::SelfDestruct {
            units: Units::of(
                ids.iter()
                    .copied()
                    .filter(|id| v.units.iter().any(|u| u.id == *id && !u.kind.is_factory()))
                    .take(1),
            ),
        },
        16 => SimCommand::Deploy { units },
        17 => SimCommand::Enter { units },
        18 => SimCommand::Cast {
            units,
            target: point(d),
            auto: d.chance(30),
        },
        19 => SimCommand::SetSpeed {
            percent: MIN_SPEED_PERCENT + d.below(200) as u16,
        },
        20 => SimCommand::Pause { on: d.chance(50) },
        // Every seat is its own side here: always refused, never a
        // no-op that could move units.
        21 => SimCommand::Take {
            from: d.pick(&SEATED).unwrap(),
        },
        // A resignation, rarely, late and only by the last seat (the
        // others keep playing); otherwise another order.
        _ if team == 3 && tick > 400 && d.chance(10) => SimCommand::Resign,
        _ => SimCommand::Stop { units },
    }
}

/// The chaos stream's frames for `tick`.
fn chaos(d: &mut Dice, v: &View, tick: u32) -> Vec<CommandFrame> {
    let mut frames = Vec::new();
    for team in SEATED {
        if !d.chance(35) {
            continue;
        }
        let n = 1 + d.below(3);
        let mut commands = Vec::new();
        // `Units::Prev` only behind an explicit list of this frame: what
        // it names then never depends on the malformed stream.
        let mut listed = false;
        for _ in 0..n {
            let mut cmd = chaos_command(d, v, team, tick, listed);
            listed |= cmd.units_mut().is_some_and(|u| u.list().is_some());
            commands.push(cmd);
        }
        frames.push(CommandFrame {
            tick,
            player: team,
            commands,
        });
    }
    // The host's speed control, now and then (`net::speed`).
    if d.chance(2) {
        frames.push(CommandFrame {
            tick,
            player: SERVER_PLAYER,
            commands: vec![SimCommand::InternalSpeed {
                percent: MIN_SPEED_PERCENT + d.below(91) as u16,
            }],
        });
    }
    // A frame whose bytes were damaged in transit (or by a cheater) and
    // still decode: delivered as decoded, whatever tick and player it
    // now names.
    if d.chance(20) {
        let team = d.pick(&SEATED).unwrap();
        let frame = CommandFrame {
            tick,
            player: team,
            commands: vec![chaos_command(d, v, team, tick, false)],
        };
        let mut bytes = frame.encode();
        for _ in 0..1 + d.below(3) {
            let i = d.below(bytes.len() as u64) as usize;
            bytes[i] ^= 1 << d.below(8);
        }
        // A `Units::Prev` the damage made up would name whatever list
        // came last, the malformed stream's included: left out.
        let names_prev = |f: &CommandFrame| {
            f.commands.iter().any(|c| {
                c.clone()
                    .units_mut()
                    .is_some_and(|u| matches!(u, Units::Prev))
            })
        };
        if let Ok(mut f) = CommandFrame::decode(&bytes)
            && !names_prev(&f)
        {
            // Keep it on this tick (an old tick is dropped by `deliver`,
            // a far one would sit in the queue past the run).
            f.tick = tick;
            frames.push(f);
        }
    }
    frames
}

/// A non-canonical list: a duplicate, a swap or a zero.
fn broken_list(d: &mut Dice, ids: &[NetId]) -> Units {
    let mut v: Vec<NetId> = ids.to_vec();
    if v.is_empty() {
        v.push(NetId(1));
    }
    match d.below(3) {
        0 => v.push(*v.last().unwrap()),
        1 => v.insert(0, NetId(v[0].0 + 1)),
        _ => v.insert(0, NetId(0)),
    }
    // Bypass `IdList::new`'s canonicalisation.
    Units::List(IdList::raw(v))
}

/// The malformed stream's frames for `tick`, and how many of their
/// commands must show up as refusals.
fn malformed(
    d: &mut Dice,
    v: &View,
    tick: u32,
    chaos: &[CommandFrame],
) -> (Vec<CommandFrame>, u64) {
    let mut frames = Vec::new();
    let mut counted = 0;
    if !d.chance(30) {
        return (frames, 0);
    }
    // From a sender without a seat: anything at all.
    if d.chance(30) {
        let player = d.pick(&UNSEATED).unwrap();
        let team = d.pick(&SEATED).unwrap();
        let mut commands = vec![chaos_command(d, v, team, tick, false)];
        if d.chance(20) {
            commands.push(SimCommand::Resign);
            commands.push(SimCommand::Dropped);
        }
        counted += commands.len() as u64;
        frames.push(CommandFrame {
            tick,
            player,
            commands,
        });
    }
    // From a seated human (a resigned one's is refused too). Not from
    // an AI seat: the AI's own sealed commands use `Units::Prev` against
    // what its seat sent last, and in a match nobody else sends as an
    // AI seat (the host stamps the sender), so lists delivered in its
    // name would change what its next `Prev` names.
    let player = d.pick(&HUMANS).unwrap();
    let own: Vec<NetId> = v.own(player).iter().map(|u| u.id).collect();
    let mut commands = Vec::new();
    for _ in 0..1 + d.below(3) {
        let (cmd, counts) = match d.below(14) {
            // Only the server sets the speed control's speed.
            13 => (SimCommand::InternalSpeed { percent: 50 }, true),
            // Every other seat is an enemy here, and oneself is no ally
            // to take from.
            12 => (
                SimCommand::Take {
                    from: d.pick(&SEATED).unwrap(),
                },
                true,
            ),
            0 => (
                SimCommand::Order {
                    units: Units::of(own.iter().copied()),
                    order: match d.below(4) {
                        0 => Order::Move(v.point(d, true)),
                        1 => Order::AttackMove(v.point(d, true)),
                        2 => Order::Patrol(v.point(d, true)),
                        _ => Order::AttackGround(v.point(d, true)),
                    },
                    queue: d.chance(50),
                },
                true,
            ),
            1 => (
                SimCommand::Cast {
                    units: Units::of(own.iter().copied()),
                    target: v.point(d, true),
                    auto: d.chance(50),
                },
                true,
            ),
            2 => (
                SimCommand::SelfDestruct {
                    units: broken_list(d, &own),
                },
                true,
            ),
            3 => {
                let mut ids = own.clone();
                ids.push(NetId(v.max_id + 1));
                let targets = PosList(
                    (0..ids.len() + 1 + d.below(2) as usize)
                        .map(|_| v.point(d, false))
                        .collect(),
                );
                (
                    SimCommand::PerUnit {
                        order: PointOrder::Move,
                        units: IdList::new(ids),
                        targets,
                        queue: false,
                    },
                    true,
                )
            }
            4 => (
                SimCommand::Factory {
                    factories: Units::of(own.iter().copied()),
                    kind: any_kind(d),
                    op: FactoryOp::Add(MAX_FACTORY_COUNT + 1 + d.below(1000) as u16),
                },
                true,
            ),
            5 => (
                SimCommand::SetSpeed {
                    percent: if d.chance(50) {
                        d.below(MIN_SPEED_PERCENT as u64) as u16
                    } else {
                        MAX_SPEED_PERCENT + 1 + d.below(1000) as u16
                    },
                },
                true,
            ),
            6 => (
                SimCommand::Build {
                    builders: Units::of(own.iter().copied()),
                    kind: any_kind(d),
                    sites: PosList(vec![v.point(d, false); MAX_BUILD_SITES + 1]),
                    queue: false,
                },
                true,
            ),
            7 => (
                SimCommand::Dev(match d.below(4) {
                    0 => DevCommand::Spawn {
                        kind: any_kind(d),
                        team: player,
                        at: v.point(d, false),
                        count: 1,
                        spacing: 24,
                    },
                    1 => DevCommand::Kill {
                        units: IdList::new(v.units.iter().map(|u| u.id)),
                    },
                    2 => DevCommand::SetHealth {
                        units: IdList::new(v.units.iter().map(|u| u.id)),
                        hp: 0,
                    },
                    _ => DevCommand::GodMode { on: true },
                }),
                true,
            ),
            8 => (
                SimCommand::Path {
                    unit: NetId(0),
                    waypoints: PosList(vec![v.point(d, false)]),
                    queue: false,
                },
                true,
            ),
            // Valid in form, but for units the sender doesn't control
            // or that don't exist: nothing happens, nothing is counted.
            9 => (
                SimCommand::Order {
                    units: Units::of(v.foreign(player).iter().map(|u| u.id)),
                    order: Order::Move(v.point(d, false)),
                    queue: false,
                },
                false,
            ),
            10 => (
                SimCommand::SelfDestruct {
                    units: Units::of(v.foreign(player).iter().map(|u| u.id)),
                },
                false,
            ),
            _ => (
                SimCommand::Stop {
                    units: Units::of((0..3).map(|_| v.stale(d))),
                },
                false,
            ),
        };
        // A resigned player's commands are all refused (resigned before
        // or by the chaos stream's commands of this tick, which apply
        // first).
        let resigned = v.resigned.contains(&player)
            || chaos
                .iter()
                .any(|f| f.player == player && f.commands.contains(&SimCommand::Resign));
        if counts || resigned {
            counted += 1;
        }
        commands.push(cmd);
    }
    frames.push(CommandFrame {
        tick,
        player,
        commands,
    });
    (frames, counted)
}

/// One fuzz run.
pub struct Run {
    pub trail: String,
    /// Frames and commands delivered (both streams).
    pub frames: u64,
    pub commands: u64,
    /// Malformed commands that must have been refused, and refusals.
    pub expected_refusals: u64,
    pub refusals: u64,
}

/// Play `ticks` ticks of the chaos stream (seeded `seed`), with the
/// malformed stream mixed in or not, the storage reshuffled every
/// `shuffle_every` ticks (0: never).
pub fn run(ticks: u32, seed: u64, with_malformed: bool, shuffle_every: u32) -> Result<Run, String> {
    let setup = setup(seed);
    let map = MapInput::checkout(MAP).map_err(|e| e.to_string())?;
    let mut h = SimHarness::new_with(setup.scenario(Some(map.file)), map.assets);
    h.world_mut().resource_mut::<HashLog>().enabled = true;
    let (mut chaos_dice, mut bad_dice) = (Dice(seed), Dice(seed ^ 0xbad));
    let mut shuffle_rng = 0x5eed_u32;
    let mut out = Run {
        trail: String::new(),
        frames: 0,
        commands: 0,
        expected_refusals: 0,
        refusals: 0,
    };
    let refusals_at_start = h.world_mut().resource::<Refusals>().count;
    while h.tick < ticks as u64 {
        let world = h.world_mut();
        let tick = world.resource::<CommandBus>().tick;
        let v = view(world);
        let mut frames = chaos(&mut chaos_dice, &v, tick);
        if with_malformed {
            let (bad, counted) = malformed(&mut bad_dice, &v, tick, &frames);
            out.expected_refusals += counted;
            frames.extend(bad);
        }
        let mut bus = world.resource_mut::<CommandBus>();
        for f in frames {
            out.frames += 1;
            out.commands += f.commands.len() as u64;
            bus.deliver(f);
        }
        // Paused (someone's chaos `Pause`): the local player (seat 0,
        // which never resigns here) unpauses through the tick driver at
        // once, as a player would after a while; the solo host cuts the
        // resume turn on the next frame.
        let control = world.resource::<MatchControl>().clone();
        if control.paused {
            world
                .resource_mut::<CommandBus>()
                .issue(SimCommand::Pause { on: false });
        }
        if shuffle_every > 0 && h.tick.is_multiple_of(shuffle_every as u64) {
            shuffle_storage(h.world_mut(), &mut shuffle_rng);
        }
        // One tick of game time at the current speed (the driver scales
        // the clock by it); a frame that falls short by rounding runs
        // none, the next one does.
        let dt = crate::headless::TICK.div_f64(f64::from(control.speed()));
        let mut frames = 0;
        while h.frame_dt(dt) == 0 {
            frames += 1;
            if frames > 8 {
                return Err(format!("no tick runs at tick {tick} ({control:?})"));
            }
        }
    }
    let world = h.world_mut();
    out.refusals = world.resource::<Refusals>().count - refusals_at_start;
    out.trail = trail_text(&world.resource::<HashLog>().ticks);
    Ok(out)
}

/// The whole check: S twice, S shuffled, S with M. `Err` describes the
/// first failure.
pub fn check(ticks: u32, seed: u64) -> Result<String, String> {
    let first_diff = |a: &str, b: &str, what: &str| -> Result<(), String> {
        match crate::headless::replay::first_difference(a, b) {
            None => Ok(()),
            Some(line) => Err(format!(
                "{what}: trails differ from tick {line}:\n  {}\n  {}",
                a.lines().nth(line - 1).unwrap_or("<end>"),
                b.lines().nth(line - 1).unwrap_or("<end>")
            )),
        }
    };
    let plain = run(ticks, seed, false, 0)?;
    let again = run(ticks, seed, false, 0)?;
    first_diff(&plain.trail, &again.trail, "the same stream twice")?;
    let shuffled = run(ticks, seed, false, 1)?;
    first_diff(
        &plain.trail,
        &shuffled.trail,
        "the stream with the storage shuffled",
    )?;
    let bad = run(ticks, seed, true, 0)?;
    first_diff(
        &plain.trail,
        &bad.trail,
        "malformed commands changed the match",
    )?;
    let refused = bad.refusals - plain.refusals;
    if refused != bad.expected_refusals {
        return Err(format!(
            "{} malformed commands should have been refused, {refused} were",
            bad.expected_refusals
        ));
    }
    let last = plain.trail.lines().last().unwrap_or("");
    Ok(format!(
        "command fuzz, seed {seed:#x}: {ticks} ticks, {} frames / {} commands \
         ({} refused in part or whole), + {} frames / {} malformed commands all refused; \
         same trail twice, shuffled and with the malformed ones; last tick: {last}",
        plain.frames,
        plain.commands,
        plain.refusals,
        bad.frames - plain.frames,
        bad.commands - plain.commands,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command fuzz ([`check`]), when the map is baked. Knobs:
    /// `KP_FUZZ_TICKS` (default 600), `KP_FUZZ_SEED`.
    #[test]
    fn command_fuzz() {
        if MapInput::checkout(MAP).is_err() {
            eprintln!("command_fuzz: {MAP}.kpmap not baked, skipped (CI: determinism.yml)");
            return;
        }
        let env = |k: &str, default: u64| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let report = check(
            env("KP_FUZZ_TICKS", 600) as u32,
            env("KP_FUZZ_SEED", 0xf022),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        eprintln!("{report}");
    }

    /// Random bytes and damaged encodings never panic the decoder, and
    /// whatever decodes applies without panicking in a bare world.
    #[test]
    fn decoding_garbage_never_panics() {
        let mut d = Dice(7);
        let mut world = World::new();
        super::super::init_test_world(&mut world);
        let unit = world
            .spawn((UnitType(UnitKind::Bit), TeamId(0), Transform::default()))
            .id();
        let id = *world.get::<NetId>(unit).unwrap();
        let sample = CommandFrame {
            tick: 0,
            player: 0,
            commands: vec![
                SimCommand::Order {
                    units: Units::of([id]),
                    order: Order::Move(NetPos::default()),
                    queue: false,
                },
                SimCommand::Dev(DevCommand::Kill {
                    units: IdList::new([id]),
                }),
                SimCommand::SetSpeed { percent: 150 },
            ],
        }
        .encode();
        let mut decoded = 0;
        for i in 0..20_000 {
            let bytes: Vec<u8> = if i % 2 == 0 {
                (0..d.below(40)).map(|_| d.next() as u8).collect()
            } else {
                let mut b = sample.clone();
                for _ in 0..1 + d.below(4) {
                    let k = d.below(b.len() as u64) as usize;
                    b[k] ^= 1 << d.below(8);
                }
                b
            };
            if let Ok(mut frame) = CommandFrame::decode(&bytes) {
                decoded += 1;
                let mut bus = world.resource_mut::<CommandBus>();
                frame.tick = bus.tick;
                bus.deliver(frame);
                super::super::apply_now(&mut world);
            }
        }
        assert!(decoded > 100, "only {decoded} decoded");
    }
}

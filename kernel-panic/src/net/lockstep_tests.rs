//! In-process lockstep: several independent sim worlds (the movement
//! harness — fixed-tick motion chain, real FBI registry, flat map), one
//! per "machine", each issuing only its own player's commands through
//! its [`CommandBus`]. A simulated network carries the encoded frames
//! between them with a configurable latency; a peer stalls while a
//! remote frame for its next tick is missing. Every peer must reach the
//! same state hash on every tick.
//!
//! The machines also differ in what they spawn outside the sim (dummy
//! entities standing in for render / UI entities), so their `Entity`
//! ids disagree while their `NetId`s match.
//!
//! `cargo test -p kernel-panic net::lockstep -- --nocapture` prints the
//! numbers (stalls, frame sizes, bandwidth).

use bevy::math::ops;
use bevy::prelude::*;

use super::*;
use crate::interaction::movement::{AttackMoveActive, CommandQueue, MovePath, MoveTarget};
use crate::interaction::movement_harness::Harness;
use crate::units::components::{TeamId, UnitType};
use crate::units::lifecycle::production::Producer;
use crate::units::player::LocalTeam;

const SPECTATOR: u8 = crate::game_setup::SPECTATOR_TEAM;

/// One machine.
struct Peer {
    h: Harness,
    local: u8,
    /// State hash after each applied tick.
    hashes: Vec<u64>,
    stalls: u32,
    /// Per remote player: the tick after the last frame delivered.
    received: std::collections::HashMap<u8, u32>,
}

impl Peer {
    /// The lockstep gate: every `remote` player's frame for the next
    /// tick has arrived (each sends one per tick, empty or not). The
    /// first `input_delay` ticks need nobody: no command can be
    /// scheduled that early.
    fn ready(&self, remote: &[u8]) -> bool {
        let bus = self.h.world.resource::<CommandBus>();
        bus.tick < bus.input_delay
            || remote
                .iter()
                .all(|p| self.received.get(p).is_some_and(|&next| next > bus.tick))
    }

    /// A frame arrived (a remote player's, or our own straight back).
    fn deliver(&mut self, frame: CommandFrame) {
        let next = self.received.entry(frame.player).or_default();
        *next = (*next).max(frame.tick + 1);
        self.h.world.resource_mut::<CommandBus>().deliver(frame);
    }
}

fn fnv(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= *b as u64;
        *hash = hash.wrapping_mul(0x100_0000_01b3);
    }
}

fn fnv_vec3(hash: &mut u64, v: Vec3) {
    for f in v.to_array() {
        fnv(hash, &f.to_bits().to_le_bytes());
    }
}

/// Hash of the sim state that the commands drive: every unit by
/// `NetId` with its exact pose, active move order, fight marker, path
/// and queue, and factory queues.
fn state_hash(world: &mut World) -> u64 {
    let mut q = world.query::<(
        &NetId,
        &Transform,
        Option<&MoveTarget>,
        Option<&CommandQueue>,
        Option<&MovePath>,
        Has<AttackMoveActive>,
        Option<&Producer>,
    )>();
    let mut rows: Vec<_> = q.iter(world).collect();
    rows.sort_by_key(|r| *r.0);
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for (id, tf, mt, queue, path, fight, producer) in rows {
        fnv(&mut h, &id.0.to_le_bytes());
        fnv_vec3(&mut h, tf.translation);
        for f in tf.rotation.to_array() {
            fnv(&mut h, &f.to_bits().to_le_bytes());
        }
        if let Some(mt) = mt {
            fnv(&mut h, b"M");
            fnv_vec3(&mut h, mt.0);
        }
        for c in queue.iter().flat_map(|q| &q.commands) {
            fnv(
                &mut h,
                format!("{c:?}").split('(').next().unwrap().as_bytes(),
            );
            fnv_vec3(&mut h, c.position());
        }
        if let Some(p) = path {
            fnv(&mut h, &(p.current as u32).to_le_bytes());
            for w in &p.waypoints {
                fnv_vec3(&mut h, *w);
            }
        }
        fnv(&mut h, &[fight as u8]);
        if let Some(p) = producer {
            fnv(&mut h, &[p.repeat() as u8]);
            for k in p.queue() {
                fnv(&mut h, <&str>::from(*k).as_bytes());
            }
        }
    }
    h
}

/// Team 0: a block of Bits; team 1: Bytes and Pointers. Both teams own
/// a homebase (team 2, the AI, too when `ai`).
fn spawn_units(h: &mut Harness, ai: bool) {
    let block = |cx: f32, cz: f32, n: usize, i: usize| {
        let side = (n as f32).sqrt().ceil() as usize;
        Vec3::new(
            cx + (i % side) as f32 * 24.0,
            0.0,
            cz + (i / side) as f32 * 24.0,
        )
    };
    h.spawn(UnitKind::Kernel, 0, Vec3::new(200.0, 0.0, 200.0));
    for i in 0..24 {
        h.spawn(UnitKind::Bit, 0, block(400.0, 400.0, 24, i));
    }
    h.spawn(UnitKind::Kernel, 1, Vec3::new(1800.0, 0.0, 1800.0));
    for i in 0..12 {
        h.spawn(UnitKind::Byte, 1, block(1500.0, 1500.0, 12, i));
    }
    for i in 0..6 {
        h.spawn(UnitKind::Pointer, 1, block(1400.0, 1650.0, 6, i));
    }
    if ai {
        let base = h.spawn(UnitKind::Kernel, 2, Vec3::new(1800.0, 0.0, 300.0));
        h.world.entity_mut(base).insert(Producer::new());
        for i in 0..2 {
            h.spawn(UnitKind::Assembler, 2, block(1700.0, 450.0, 2, i));
        }
        for i in 0..10 {
            h.spawn(UnitKind::Bit, 2, block(1550.0, 350.0, 10, i));
        }
        for (i, pos) in [(900.0, 300.0), (1200.0, 700.0), (600.0, 1200.0)]
            .into_iter()
            .enumerate()
        {
            h.world.spawn(crate::terrain::geovent::GeoventSmoker {
                facing: 0,
                pos: Vec3::new(pos.0, 0.0, pos.1),
                emit_timer: 0.0,
                rng: i as u32 + 1,
            });
        }
    }
}

fn peer(local: u8, delay: u32, entity_offset: usize, ai: bool) -> Peer {
    let mut h = Harness::flat();
    // Render / UI entities only this machine spawned.
    for _ in 0..entity_offset {
        h.world.spawn_empty();
    }
    init_test_world(&mut h.world);
    h.world.insert_resource(LocalTeam(local));
    {
        let mut bus = h.world.resource_mut::<CommandBus>();
        bus.input_delay = delay;
        // Our frames go out through the outbox, and come back to us
        // like everyone else's.
        bus.routed = true;
        bus.log = Some(Vec::new());
    }
    h.pre_schedule.add_systems(apply::apply_sim_commands);
    if ai {
        use crate::units::ai;
        h.world.insert_resource(ai::AiTicker::with_seed(0x5eed));
        h.world.insert_resource(ai::AiSeats(vec![2]));
        h.world.insert_resource(crate::game_setup::AiDifficulty(1));
        h.world.init_resource::<ai::TeamDamageLog>();
        h.world
            .init_resource::<crate::units::mechanics::network_buffer::PacketBuffer>();
        h.world
            .init_resource::<crate::units::spatial::SpatialIndex>();
        h.world.init_resource::<crate::units::teams::Alliances>();
        h.pre_schedule
            .add_systems(ai::ai_brain.after(apply::apply_sim_commands));
    }
    spawn_units(&mut h, ai);
    Peer {
        h,
        local,
        hashes: Vec::new(),
        stalls: 0,
        received: Default::default(),
    }
}

/// The units of `team`, by id.
fn team_ids(world: &mut World, team: u8, kind: Option<UnitKind>) -> Vec<NetId> {
    let mut q = world.query::<(&NetId, &TeamId, &UnitType)>();
    let mut ids: Vec<NetId> = q
        .iter(world)
        .filter(|(_, t, k)| t.0 == team && kind.is_none_or(|kind| k.0 == kind))
        .map(|(id, ..)| *id)
        .collect();
    ids.sort();
    ids
}

fn p(x: f32, z: f32) -> NetPos {
    NetPos::quantize(Vec3::new(x, 0.0, z))
}

/// A scripted match: (local tick the player issues on, player, command).
fn script(world: &mut World) -> Vec<(u32, u8, SimCommand)> {
    let bits = team_ids(world, 0, Some(UnitKind::Bit));
    let bytes = team_ids(world, 1, Some(UnitKind::Byte));
    let pointers = team_ids(world, 1, Some(UnitKind::Pointer));
    let kernel1 = team_ids(world, 1, Some(UnitKind::Kernel))[0];
    let mut s = vec![
        (
            0,
            0,
            SimCommand::Order {
                units: Units::of(bits.clone()),
                order: Order::Move(p(1100.0, 1000.0)),
                queue: false,
            },
        ),
        (
            2,
            1,
            SimCommand::Order {
                units: Units::of(bytes.clone()),
                order: Order::AttackMove(p(500.0, 600.0)),
                queue: false,
            },
        ),
        (
            3,
            1,
            SimCommand::Factory {
                factories: Units::of([kernel1]),
                kind: UnitKind::Bit,
                op: FactoryOp::Add(5),
            },
        ),
    ];
    // Shift-queue ten waypoints for the Bits, one click every 6 ticks.
    for i in 0..10 {
        let a = i as f32 * 0.6;
        s.push((
            15 + i * 6,
            0,
            SimCommand::Order {
                units: Units::of(bits.clone()),
                order: Order::Move(p(
                    1000.0 + 300.0 * ops::cos(a),
                    1000.0 + 300.0 * ops::sin(a),
                )),
                queue: true,
            },
        ));
    }
    // A drawn formation for the Bytes.
    s.push((
        40,
        1,
        SimCommand::per_unit(
            PointOrder::Move,
            bytes
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, Vec3::new(700.0 + i as f32 * 30.0, 0.0, 900.0)))
                .collect(),
            false,
        ),
    ));
    s.push((
        50,
        0,
        SimCommand::Stop {
            units: Units::of(bits[..6].iter().copied()),
        },
    ));
    s.push((
        51,
        0,
        SimCommand::Order {
            units: Units::of(bits[6..12].iter().copied()),
            order: Order::Patrol(p(300.0, 900.0)),
            queue: false,
        },
    ));
    s.push((
        60,
        1,
        SimCommand::Path {
            unit: pointers[0],
            waypoints: PosList::quantize(
                (0..5).map(|i| Vec3::new(1300.0 - i as f32 * 80.0, 0.0, 1500.0 - i as f32 * 60.0)),
            ),
            queue: false,
        },
    ));
    s.push((
        75,
        0,
        SimCommand::Order {
            units: Units::of(bits[12..16].iter().copied()),
            order: Order::Guard(bits[0]),
            queue: false,
        },
    ));
    s.push((
        90,
        1,
        SimCommand::Order {
            units: Units::of(pointers[1..].iter().copied()),
            order: Order::AttackMove(p(450.0, 450.0)),
            queue: true,
        },
    ));
    // An area attack over the Bits, queued behind the fight above.
    s.push((
        95,
        1,
        SimCommand::Order {
            units: Units::of(pointers[1..].iter().copied()),
            order: Order::AttackArea {
                center: p(900.0, 900.0),
                radius: NetLen::quantize(400.0),
            },
            queue: true,
        },
    ));
    s.push((
        120,
        0,
        SimCommand::Order {
            units: Units::of(bits[16..].iter().copied()),
            order: Order::Move(p(1500.0, 500.0)),
            queue: false,
        },
    ));
    // Orders for units the player doesn't own are ignored everywhere.
    s.push((
        130,
        1,
        SimCommand::Stop {
            units: Units::of(bits.clone()),
        },
    ));
    s
}

struct Outcome {
    hashes: Vec<Vec<u64>>,
    stalls: Vec<u32>,
    wire_bytes: Vec<usize>,
    wire_frames: usize,
    logs: Vec<Vec<CommandFrame>>,
}

/// Run `ticks` ticks on peers for players `locals` (+ a spectator),
/// frames delayed by `latency` steps on the wire.
fn run(delay: u32, latency: usize, ticks: u32, ai: bool) -> Outcome {
    let locals = [0u8, 1, SPECTATOR];
    let mut peers: Vec<Peer> = locals
        .iter()
        .enumerate()
        .map(|(i, &l)| peer(l, delay, i * entity_offset(), ai))
        .collect();
    let script = script(&mut peers[0].h.world);
    // The machines disagree on `Entity`, agree on `NetId`.
    let first_bit = |w: &mut World| {
        let mut q = w.query::<(Entity, &NetId, &UnitType)>();
        q.iter(w)
            .filter(|(_, _, k)| k.0 == UnitKind::Bit)
            .min_by_key(|(_, id, _)| **id)
            .map(|(e, id, _)| (e, *id))
            .unwrap()
    };
    let (e0, id0) = first_bit(&mut peers[0].h.world);
    let (e1, id1) = first_bit(&mut peers[1].h.world);
    if entity_offset() > 0 {
        assert_ne!(e0, e1);
    }
    assert_eq!(id0, id1);

    let players = [0u8, 1];
    // (deliver at step, recipient, encoded frame)
    let mut wire: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    let mut wire_bytes = vec![0usize; locals.len()];
    let mut wire_frames = 0;
    let mut step = 0usize;
    while peers.iter().any(|p| p.h.tick < ticks) {
        let mut progressed = false;
        for i in 0..peers.len() {
            let peer = &mut peers[i];
            if peer.h.tick >= ticks {
                continue;
            }
            let remote: Vec<u8> = players
                .iter()
                .copied()
                .filter(|&p| p != peer.local)
                .collect();
            if !peer.ready(&remote) {
                peer.stalls += 1;
                continue;
            }
            for (_, _, cmd) in script
                .iter()
                .filter(|(t, pl, _)| *t == peer.h.tick && *pl == peer.local)
            {
                peer.h.world.resource_mut::<CommandBus>().issue(cmd.clone());
            }
            peer.h.step();
            progressed = true;
            let hash = state_hash(&mut peer.h.world);
            peer.hashes.push(hash);
            let outbox = std::mem::take(&mut peer.h.world.resource_mut::<CommandBus>().outbox);
            for frame in outbox {
                let bytes = frame.encode();
                peer.deliver(frame);
                wire_bytes[i] += bytes.len();
                wire_frames += 1;
                for j in 0..locals.len() {
                    if j != i {
                        wire.push((step + latency, j, bytes.clone()));
                    }
                }
            }
        }
        let (due, later): (Vec<_>, Vec<_>) = wire.drain(..).partition(|(at, ..)| *at <= step);
        let delivered = !due.is_empty();
        wire = later;
        for (_, to, bytes) in due {
            peers[to].deliver(CommandFrame::decode(&bytes).unwrap());
        }
        assert!(
            progressed || delivered || !wire.is_empty(),
            "lockstep deadlock at step {step}"
        );
        step += 1;
    }
    Outcome {
        stalls: peers.iter().map(|p| p.stalls).collect(),
        logs: peers
            .iter_mut()
            .map(|p| {
                p.h.world
                    .resource_mut::<CommandBus>()
                    .log
                    .take()
                    .unwrap_or_default()
            })
            .collect(),
        hashes: peers.into_iter().map(|p| p.hashes).collect(),
        wire_bytes,
        wire_frames,
    }
}

fn entity_offset() -> usize {
    std::env::var("KP_TEST_ENTITY_OFFSET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(37)
}

fn first_divergence(a: &[u64], b: &[u64]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// Every peer reaches the same state on every tick, for several input
/// delays and wire latencies; the outcome depends on the delay only
/// (latency just stalls), and the commands really changed the sim.
#[test]
fn lockstep_peers_agree() {
    const TICKS: u32 = 240;
    let mut by_delay: Vec<(u32, u64)> = Vec::new();
    for delay in [1u32, 3, 6] {
        for latency in [0usize, 2, 5] {
            let out = run(delay, latency, TICKS, false);
            for (i, h) in out.hashes.iter().enumerate().skip(1) {
                assert_eq!(
                    first_divergence(&out.hashes[0], h),
                    None,
                    "peer {i} diverged (delay {delay}, latency {latency})"
                );
                assert_eq!(h.len(), TICKS as usize);
            }
            println!(
                "delay {delay} latency {latency}: final {:016x}, stalls {:?}, wire bytes/peer {:?} in {} frames",
                out.hashes[0].last().unwrap(),
                out.stalls,
                out.wire_bytes,
                out.wire_frames,
            );
            by_delay.push((delay, *out.hashes[0].last().unwrap()));
        }
    }
    for w in by_delay.windows(2) {
        if w[0].0 == w[1].0 {
            assert_eq!(w[0].1, w[1].1, "latency changed the outcome");
        }
    }
    // The commands mattered: different delays end in different states.
    assert_ne!(by_delay[0].1, by_delay.last().unwrap().1);
}

/// The AI (team 2) runs inside every peer's sim and issues through the
/// same bus; its frames are never sent, yet every peer agrees.
#[test]
fn lockstep_with_in_sim_ai() {
    const TICKS: u32 = 300;
    let out = run(2, 3, TICKS, true);
    for (i, h) in out.hashes.iter().enumerate().skip(1) {
        assert_eq!(
            first_divergence(&out.hashes[0], h),
            None,
            "peer {i} diverged"
        );
    }
    let ai_frames: Vec<&CommandFrame> = out.logs[0].iter().filter(|f| f.player == 2).collect();
    assert!(!ai_frames.is_empty(), "the AI issued nothing");
    let bytes: usize = ai_frames.iter().map(|f| f.encode().len()).sum();
    let cmds: usize = ai_frames.iter().map(|f| f.commands.len()).sum();
    println!(
        "AI over {:.0} s: {} frames, {cmds} commands, {bytes} bytes ({:.1} B/s if broadcast, payload only)",
        TICKS as f32 / 30.0,
        ai_frames.len(),
        bytes as f32 / (TICKS as f32 / 30.0),
    );
    for f in &ai_frames {
        let kinds: Vec<&str> = f
            .commands
            .iter()
            .map(|c| match c {
                SimCommand::Order { .. } => "Order",
                SimCommand::PerUnit { .. } => "PerUnit",
                SimCommand::Build { .. } => "Build",
                SimCommand::Factory { .. } => "Factory",
                SimCommand::SetRepeat { .. } => "SetRepeat",
                SimCommand::Cast { .. } => "Cast",
                SimCommand::Deploy { .. } => "Deploy",
                _ => "other",
            })
            .collect();
        println!("  tick {:>4}: {:>3} B  {kinds:?}", f.tick, f.encode().len());
    }
}

/// A command must never be applied twice nor dropped: the apply log of
/// every peer is the same command stream.
#[test]
fn every_peer_applies_the_same_stream() {
    let out = run(3, 4, 200, false);
    for (i, (a, b)) in out.logs[0].iter().zip(&out.logs[1]).enumerate() {
        if a != b {
            println!("first difference at {i}:\n{a:?}\n{b:?}");
            break;
        }
    }
    println!("lens {} {}", out.logs[0].len(), out.logs[1].len());
    assert_eq!(out.logs[0], out.logs[1]);
    assert_eq!(out.logs[0], out.logs[2]);
    assert!(out.logs[0].iter().map(|f| f.commands.len()).sum::<usize>() >= 20);
}

// --- wire size -----------------------------------------------------------

/// Realistic ids: units of one player spawned over a long game,
/// interleaved with everybody else's spawns.
fn spread_ids(n: usize, first: u32, gap: u32) -> Vec<NetId> {
    let mut rng = 0x1234_5678u32;
    let mut id = first;
    (0..n)
        .map(|_| {
            id += 1 + crate::rng::xorshift32(&mut rng) % (2 * gap);
            NetId(id)
        })
        .collect()
}

fn frame(tick: u32, commands: Vec<SimCommand>) -> CommandFrame {
    CommandFrame {
        tick,
        player: 3,
        commands,
    }
}

/// The same command with fixed-width fields (u32 ids, f32 positions),
/// as a naive `bincode`/`#[repr(C)]` encoding would send it.
fn naive_size(units: usize, positions: usize) -> usize {
    // tick + player + count + tag + flags; ids; positions.
    4 + 1 + 2 + 1 + 1 + 2 + units * 4 + positions * 12
}

#[test]
fn frame_sizes() {
    // Ten minutes in: tick 18000, ids around 3000.
    let tick = 18_000;
    let pos = NetPos::quantize(Vec3::new(3121.4, 87.2, 2650.9));
    let one = spread_ids(1, 3000, 20);
    let fifty = spread_ids(50, 2000, 20);
    let fifty_wide = spread_ids(50, 100, 300);

    let empty = frame(tick, vec![]);
    let single = frame(
        tick,
        vec![SimCommand::Order {
            units: Units::of(one.clone()),
            order: Order::Move(pos),
            queue: false,
        }],
    );
    let box50 = frame(
        tick,
        vec![SimCommand::Order {
            units: Units::of(fifty.clone()),
            order: Order::Move(pos),
            queue: false,
        }],
    );
    let box50_wide = frame(
        tick,
        vec![SimCommand::Order {
            units: Units::of(fifty_wide.clone()),
            order: Order::Move(pos),
            queue: false,
        }],
    );
    // Shift-queue: the first click names the group, the next nine
    // reuse it (`Units::Prev`, substituted by the bus when sealing).
    let mut bus = CommandBus::default();
    let mut queued = Vec::new();
    for i in 0..10 {
        bus.issue(SimCommand::Order {
            units: Units::of(fifty.clone()),
            order: Order::Move(NetPos::quantize(Vec3::new(
                3000.0 + i as f32 * 97.0,
                80.0,
                2600.0 - i as f32 * 41.0,
            ))),
            queue: true,
        });
        bus.routed = true;
        bus.seal(Some(3));
        bus.tick += 6;
        queued.append(&mut bus.outbox);
    }
    let queued_sizes: Vec<usize> = queued.iter().map(|f| f.encode().len()).collect();
    let queued_total: usize = queued_sizes.iter().sum();
    let formation = frame(
        tick,
        vec![SimCommand::per_unit(
            PointOrder::Move,
            fifty
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    (
                        *id,
                        Vec3::new(
                            2800.0 + i as f32 * 11.3,
                            90.0 + (i % 3) as f32,
                            2400.0 + i as f32 * 4.1,
                        ),
                    )
                })
                .collect(),
            false,
        )],
    );
    let build_row = frame(
        tick,
        vec![SimCommand::Build {
            builders: Units::of(spread_ids(3, 2500, 50)),
            kind: UnitKind::BadBlock,
            sites: PosList::quantize(
                (0..8).map(|i| Vec3::new(1600.0 + i as f32 * 32.0, 64.0, 900.0)),
            ),
            queue: BuildQueue::Back,
        }],
    );
    let factory = frame(
        tick,
        vec![SimCommand::Factory {
            factories: Units::of(one.clone()),
            kind: UnitKind::Byte,
            op: FactoryOp::Add(5),
        }],
    );
    let path = frame(
        tick,
        vec![SimCommand::Path {
            unit: one[0],
            waypoints: PosList::quantize(
                (0..12).map(|i| Vec3::new(2000.0 + i as f32 * 50.0, 70.0, 1000.0 + (i * i) as f32)),
            ),
            queue: false,
        }],
    );

    let rows = [
        ("empty frame", empty.encode().len(), 7),
        ("1 unit move", single.encode().len(), naive_size(1, 1)),
        ("50-unit box move", box50.encode().len(), naive_size(50, 1)),
        (
            "50-unit box move, ids spread over 15k",
            box50_wide.encode().len(),
            naive_size(50, 1),
        ),
        (
            "50-unit formation (per-unit targets)",
            formation.encode().len(),
            naive_size(50, 50),
        ),
        (
            "shift-queue 10 waypoints, 50 units (10 frames)",
            queued_total,
            10 * naive_size(50, 1),
        ),
        (
            "build row: 3 builders, 8 sites",
            build_row.encode().len(),
            naive_size(3, 8),
        ),
        ("factory +5", factory.encode().len(), naive_size(1, 0)),
        (
            "1 unit drawn path, 12 waypoints",
            path.encode().len(),
            naive_size(1, 12),
        ),
    ];
    println!("{:<48} {:>7} {:>7}", "frame", "postcard", "naive");
    for (name, size, naive) in rows {
        println!("{name:<48} {size:>7} {naive:>7}");
    }
    println!("shift-queue per click: {queued_sizes:?}");

    // Round trip.
    for f in [
        &empty, &single, &box50, &formation, &build_row, &factory, &path,
    ] {
        assert_eq!(&CommandFrame::decode(&f.encode()).unwrap(), f);
    }
    assert!(matches!(
        queued[1].commands[0],
        SimCommand::Order {
            units: Units::Prev,
            ..
        }
    ));
    assert!(empty.encode().len() <= 6);
    assert!(single.encode().len() <= 20);
    assert!(box50.encode().len() <= 80);
}

/// Quantization: what a peer applies is exactly what the issuer applies.
#[test]
fn positions_round_trip_exactly() {
    for v in [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(8191.937, -12.5, 0.06),
        Vec3::new(1234.567, 89.01, 2345.678),
    ] {
        let q = NetPos::quantize(v);
        assert!((q.to_vec3() - v).abs().max_element() <= 0.5 / codec::POS_SCALE);
        assert_eq!(NetPos::quantize(q.to_vec3()), q);
    }
}

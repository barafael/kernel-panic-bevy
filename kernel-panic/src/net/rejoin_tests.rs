//! Joining a running match (M3, [`super::rejoin`]): a player who dropped
//! comes back to its seat, a newcomer watches, and a networked match's
//! turn log plays back as a replay.
//!
//! Protocol level (a stand-in sim, milliseconds): the newcomer goes
//! through the real lobby ([`Lobby`]: `Hello` with its key, the host's
//! `Running`, the map check) and the driver's late-join path
//! ([`NetSession::from_next`]). Full sim: headless game apps, the same
//! way ([`rejoin_full_sim`]), then the host's recording through the
//! replay player.
//!
//! ```text
//! cargo test -p kernel-panic net::rejoin_tests -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use super::compat::{Build, Compat};
use super::driver::{CatchUp, GameSpeed, NetSession, NextNetMatch, Pacer, TickRunner, drive_frame};
use super::lobby::{Lobby, LobbyEvent, MapHave, StartMsg};
use super::rejoin::{Door, PlayerKey};
use super::session::{Desync, Session, SessionConfig, Timeouts};
use super::sim_net::{LinkParams, SimNetwork};
use super::transport::PeerId;
use super::wire::Turn;
use super::{CommandFrame, NetId, SimCommand, Units};

const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);
const FRAME: Duration = Duration::from_micros(16_667);
const MAP: &str = "stand-in";
const MAP_HASH: u64 = 0x5eed;

/// A stand-in sim (as `session_tests`): its state folds in every turn;
/// it records the leaves and returns it applies.
#[derive(Default)]
struct Fake {
    player: Option<u8>,
    tick: u32,
    state: u64,
    trail: Vec<u64>,
    staged: Vec<SimCommand>,
    /// command id → tick applied
    applied: BTreeMap<u32, u32>,
    /// (tick, player, returned) of every `Dropped` / `Returned`.
    leaves: Vec<(u32, u8, bool)>,
}

impl Fake {
    fn mix(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl TickRunner for Fake {
    fn speed(&self) -> GameSpeed {
        GameSpeed::default()
    }

    fn seal(&mut self, delay: u32) -> Vec<CommandFrame> {
        match self.player {
            Some(player) if !self.staged.is_empty() => vec![CommandFrame {
                tick: self.tick + delay,
                player,
                commands: std::mem::take(&mut self.staged),
            }],
            _ => {
                self.staged.clear();
                Vec::new()
            }
        }
    }

    fn run(&mut self, turn: &Turn) {
        assert_eq!(turn.tick, self.tick, "turns in order");
        self.mix(&turn.tick.to_le_bytes());
        self.mix(&postcard::to_allocvec(&turn.frames).unwrap());
        for (player, cmds) in &turn.frames {
            for c in cmds {
                match c {
                    SimCommand::Stop {
                        units: Units::List(l),
                    } => {
                        let id = l.ids()[0].0;
                        assert!(self.applied.insert(id, turn.tick).is_none(), "{id} twice");
                    }
                    SimCommand::Dropped => self.leaves.push((turn.tick, *player, false)),
                    SimCommand::Returned => self.leaves.push((turn.tick, *player, true)),
                    _ => {}
                }
            }
        }
        self.tick += 1;
        self.trail.push(self.state);
    }

    fn hash(&mut self) -> u64 {
        self.state
    }

    fn dump(&mut self, _: &Session, _: &Desync) -> String {
        panic!("no desync expected");
    }
}

struct Peer {
    id: PeerId,
    net: NetSession,
    sim: Fake,
    present: bool,
    issued: u32,
}

impl Peer {
    fn frame(&mut self, now: Duration) {
        let n = &mut self.net;
        drive_frame(
            &mut n.session,
            &mut n.pacer,
            &mut self.sim,
            FRAME,
            now,
            TICK,
        );
    }
}

struct Room {
    net: SimNetwork,
    peers: Vec<Peer>,
    keys: BTreeMap<PeerId, PlayerKey>,
}

fn start_msg(roster: &BTreeMap<PeerId, Option<u8>>) -> StartMsg {
    StartMsg {
        match_id: 77,
        setup: super::lobby::encode_setup(&crate::game_setup::GameSetup {
            map: MAP.into(),
            ..Default::default()
        }),
        roster: roster.iter().map(|(p, s)| (*p, *s)).collect(),
        compat: Compat::of(Build::current(), MAP_HASH),
    }
}

/// Three seated peers (ids 1–3, seats 0–2), 30 ms links, the host's
/// rejoin grace `grace`.
fn room(grace: Duration) -> Room {
    let net = SimNetwork::new(0x7e70);
    net.set_default_link(LinkParams::latency_ms(30));
    let roster: BTreeMap<PeerId, Option<u8>> =
        (0..3).map(|i| (PeerId(i + 1), Some(i as u8))).collect();
    let keys: BTreeMap<PeerId, PlayerKey> = roster
        .keys()
        .map(|p| (*p, PlayerKey(0x100 + p.0 as u64)))
        .collect();
    let peers = roster
        .iter()
        .map(|(&id, &seat)| {
            let cfg = SessionConfig {
                match_id: 77,
                me: id,
                roster: roster.clone(),
                delay: Default::default(),
            };
            let mut net_session = NetSession::from_next(NextNetMatch {
                config: cfg,
                transport: Box::new(net.join(id)),
                catch_up: CatchUp {
                    budget: None,
                    ..CatchUp::default()
                },
                join: None,
                door: (id == PeerId(1)).then(|| Door {
                    start: start_msg(&roster),
                    keys: keys.clone(),
                    names: roster.keys().map(|p| (*p, format!("p{}", p.0))).collect(),
                    in_room: Default::default(),
                }),
            });
            net_session.session.set_timeouts(Timeouts {
                rejoin: grace,
                ..Timeouts::default()
            });
            Peer {
                id,
                net: net_session,
                sim: Fake {
                    player: seat,
                    ..Fake::default()
                },
                present: true,
                issued: 0,
            }
        })
        .collect();
    Room { net, peers, keys }
}

impl Room {
    /// `secs` of 60 Hz frames; every seated present peer issues an
    /// order every 9 of its ticks once it is live.
    fn run(&mut self, secs: f64) {
        for _ in 0..(secs * 60.0) as u32 {
            self.net.advance(FRAME);
            let now = self.net.now();
            for p in self.peers.iter_mut().filter(|p| p.present) {
                let live = !p.net.pacer.fast_forward;
                if let Some(seat) = p.sim.player
                    && live
                    && p.sim.tick > 0
                    && p.sim.tick.is_multiple_of(9)
                    && p.sim.staged.is_empty()
                {
                    p.issued += 1;
                    let id = (seat as u32 + 1) * 1_000_000 + p.id.0 as u32 * 10_000 + p.issued;
                    p.sim.staged.push(SimCommand::Stop {
                        units: Units::of([NetId(id)]),
                    });
                }
                p.frame(now);
            }
        }
    }

    /// A newcomer with `key` opens the room as `id`, meets it through
    /// its lobby and joins the running match the host describes.
    fn knock(&mut self, id: PeerId, key: PlayerKey) -> usize {
        let mut lobby = Lobby::new(Box::new(self.net.join(id)), "newcomer", MAP).with_key(key);
        lobby.provide_map(MAP, MapHave::Hash(MAP_HASH));
        for _ in 0..600 {
            self.net.advance(FRAME);
            let now = self.net.now();
            lobby.poll(now);
            for p in self.peers.iter_mut().filter(|p| p.present) {
                p.frame(now);
            }
            if let Some(ev) = lobby.take_events().pop() {
                match ev {
                    LobbyEvent::Start(start) => {
                        let start = *start;
                        assert!(start.join.is_some(), "a late join");
                        let seat = start.session.roster[&id];
                        let net = NetSession::from_next(NextNetMatch {
                            config: start.session,
                            transport: lobby.into_transport(),
                            catch_up: CatchUp {
                                budget: None,
                                ..CatchUp::default()
                            },
                            join: start.join,
                            door: start.door,
                        });
                        assert!(net.pacer.fast_forward);
                        self.peers.push(Peer {
                            id,
                            net,
                            sim: Fake {
                                player: seat,
                                ..Fake::default()
                            },
                            present: true,
                            issued: 0,
                        });
                        return self.peers.len() - 1;
                    }
                    LobbyEvent::Closed(why) => panic!("refused: {why}"),
                }
            }
        }
        panic!("{id} never got into the match");
    }

    /// Every present peer's trail equals the host's over the common
    /// prefix and has at least `min` ticks.
    fn assert_agree(&self, min: usize) {
        let host = &self.peers[0].sim.trail;
        for p in self.peers.iter().filter(|p| p.present) {
            let t = &p.sim.trail;
            let n = t.len().min(host.len());
            if let Some(k) = (0..n).find(|&k| t[k] != host[k]) {
                panic!("{} diverges from the host at tick {k}", p.id);
            }
            assert!(t.len() >= min, "{}: only {} ticks", p.id, t.len());
        }
    }
}

/// A player whose connection breaks comes back within the grace: it gets
/// its seat back under a new connection id (lower than the host's, so
/// the host must be named, not elected), replays the match from tick 0
/// and plays on; nothing was sequenced for it, its orders apply again,
/// every trail is the host's.
#[test]
fn a_player_back_within_the_grace_keeps_its_seat() {
    let mut r = room(Duration::from_secs(5));
    r.run(4.0);
    r.net.leave(PeerId(3));
    r.peers[2].present = false;
    let key = r.keys[&PeerId(3)];
    r.run(2.0);
    let i = r.knock(PeerId(0), key);
    r.run(6.0);
    let back = &r.peers[i];
    assert!(!back.net.pacer.fast_forward, "caught up");
    assert_eq!(back.net.session.player(), Some(2), "its seat");
    r.assert_agree(300);
    for p in r.peers.iter().filter(|p| p.present) {
        assert!(p.sim.leaves.is_empty(), "{}: nothing sequenced", p.id);
    }
    // Its orders after the return applied everywhere.
    let host = &r.peers[0].sim.applied;
    let mine = host.keys().filter(|id| *id / 10_000 == 300).count();
    assert!(mine > 5, "{mine} of the returning player's orders applied");
    // The roster: the new connection on seat 2, the old one gone,
    // everywhere.
    for p in r.peers.iter().filter(|p| p.present) {
        let peers = p.net.session.peers();
        let seat2: Vec<PeerId> = peers
            .iter()
            .filter(|q| q.player == Some(2))
            .map(|q| q.peer)
            .collect();
        assert_eq!(seat2, vec![PeerId(0)], "{}", p.id);
        assert!(peers.iter().all(|q| q.peer != PeerId(3)), "{}", p.id);
        assert_eq!(
            peers.iter().find(|q| q.host).map(|q| q.peer),
            Some(PeerId(1)),
            "{}: the host is the host",
            p.id
        );
    }
    eprintln!(
        "back within the grace: {} ticks replayed and in step; {mine} orders after the return",
        r.peers[i].sim.trail.len()
    );
}

/// Without a grace the seat is dropped at once; the player's return is
/// sequenced after it (`Returned`, the same tick on every peer); then
/// a newcomer with an unknown key watches: no seat, no commands, the
/// same trail.
#[test]
fn a_dropped_player_returns_and_a_newcomer_watches() {
    let mut r = room(Duration::ZERO);
    r.run(3.0);
    r.net.leave(PeerId(2));
    r.peers[1].present = false;
    r.run(2.0);
    let key = r.keys[&PeerId(2)];
    let back = r.knock(PeerId(40), key);
    r.run(3.0);
    let watcher = r.knock(PeerId(41), PlayerKey(0xbeef));
    r.run(5.0);
    r.assert_agree(350);
    let leaves = r.peers[0].sim.leaves.clone();
    assert_eq!(leaves.len(), 2, "{leaves:?}");
    assert_eq!((leaves[0].1, leaves[0].2), (1, false), "dropped");
    assert_eq!((leaves[1].1, leaves[1].2), (1, true), "returned");
    assert!(leaves[1].0 > leaves[0].0);
    for p in r.peers.iter().filter(|p| p.present) {
        assert_eq!(p.sim.leaves, leaves, "{}: on the same ticks", p.id);
    }
    assert_eq!(r.peers[back].net.session.player(), Some(1));
    let w = &r.peers[watcher];
    assert_eq!(w.net.session.player(), None, "a newcomer watches");
    assert!(!w.net.pacer.fast_forward);
    assert!(w.issued == 0);
    // The returning player's orders count again.
    let host = &r.peers[0].sim.applied;
    assert!(host.keys().any(|id| *id / 10_000 == 200 + 40));
    eprintln!(
        "dropped at tick {}, returned at tick {}; the watcher replayed {} ticks",
        leaves[0].0,
        leaves[1].0,
        r.peers[watcher].sim.trail.len()
    );
}

/// The match's turn log, as every peer keeps it, is the same everywhere
/// and plays back as a replay session to the same trail.
#[test]
fn the_turn_log_plays_back() {
    let mut r = room(Duration::from_secs(5));
    r.run(5.0);
    let host = &r.peers[0];
    let ticks = host.net.session.applied();
    let log = host.net.session.log().to_vec();
    assert!(log.len() > 20, "{} frames", log.len());
    for p in &r.peers[1..] {
        let n = p.net.session.applied().min(ticks);
        let theirs: Vec<&CommandFrame> =
            p.net.session.log().iter().filter(|f| f.tick < n).collect();
        let ours: Vec<&CommandFrame> = log.iter().filter(|f| f.tick < n).collect();
        assert_eq!(theirs, ours, "{}: the same log", p.id);
    }
    let mut play = NetSession::new(Session::replay(&log, ticks));
    play.pacer = Pacer {
        catch_up: CatchUp::replay(),
        ..Pacer::default()
    };
    let mut sim = Fake::default();
    let mut now = Duration::ZERO;
    while !play.session.halted() {
        now += FRAME;
        let rep = drive_frame(
            &mut play.session,
            &mut play.pacer,
            &mut sim,
            FRAME,
            now,
            TICK,
        );
        assert!(rep.paced <= 1 && rep.extra == 0, "one tick a frame at most");
    }
    assert_eq!(sim.trail, r.peers[0].sim.trail[..ticks as usize]);
    assert!(play.session.log().is_empty(), "a replay records nothing");
}

/// "Back to room" (agent I) is not a rejoin: a player that leaves the
/// match for its room keeps its connection and says hello to the room
/// while the host still plays. The host's session ignores that hello
/// (no `Running`: the player stays in the room), drops the seat at once
/// (no grace, no `Returned`), and ignores a reload under the same key
/// too; a newcomer with another key still gets in to watch.
#[test]
fn back_in_the_room_is_not_a_knock() {
    let mut r = room(Duration::from_secs(30));
    r.run(3.0);
    let key = r.keys[&PeerId(3)];
    let (transport, connected) = r.peers[2].net.session.hand_back().expect("a transport");
    r.peers[2].present = false;
    let mut back = Lobby::resume(transport, &connected, "p3", MAP, None, key);
    back.provide_map(MAP, MapHave::Hash(MAP_HASH));
    // The same player's page reloads in the room: a new connection, the
    // same key.
    let mut reload = Lobby::new(Box::new(r.net.join(PeerId(50))), "p3", MAP).with_key(key);
    reload.provide_map(MAP, MapHave::Hash(MAP_HASH));
    for _ in 0..(4 * 60) {
        r.net.advance(FRAME);
        let now = r.net.now();
        for l in [&mut back, &mut reload] {
            l.poll(now);
            assert!(l.take_events().is_empty(), "nothing asks it into the match");
            assert!(!l.joining());
        }
        for p in r.peers.iter_mut().filter(|p| p.present) {
            p.frame(now);
        }
    }
    assert!(back.away(PeerId(1)), "the host is still in the match");
    let leaves = r.peers[0].sim.leaves.clone();
    assert_eq!(leaves.len(), 1, "dropped once, never returned: {leaves:?}");
    assert_eq!((leaves[0].1, leaves[0].2), (2, false));
    let dropped_after = leaves[0].0 - r.peers[2].sim.tick;
    assert!(
        dropped_after < 30,
        "at once, not after the grace: {dropped_after} ticks"
    );
    assert!(
        r.peers[0]
            .net
            .session
            .peers()
            .iter()
            .all(|q| q.peer != PeerId(50)),
        "the reload is not in the match"
    );
    let watcher = r.knock(PeerId(51), PlayerKey(0xbeef));
    r.run(3.0);
    assert_eq!(r.peers[watcher].net.session.player(), None);
    r.assert_agree(150);
}

// --- the whole game ------------------------------------------------------

mod full {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use crate::game_setup::{ActiveMatch, GameSetup, PlayerSpec};
    use crate::headless::replay::{self, MapInput, PlayOptions};
    use crate::headless::{Executor, HashLog, Scenario, SimHarness, golden};
    use crate::net::compat::{Build, Compat};
    use crate::net::driver::{CatchUp, DumpDir, NetSession, NetStatus, NextNetMatch};
    use crate::net::lobby::{Lobby, LobbyEvent, MapHave, StartMsg, encode_setup};
    use crate::net::rejoin::{Door, PlayerKey};
    use crate::net::session::{SessionConfig, Timeouts};
    use crate::net::sim_net::{LinkParams, SimNetwork};
    use crate::net::transport::PeerId;
    use crate::net::{CommandBus, MatchControl, SimCommand};
    use crate::units::components::Faction;
    use crate::units::teams::TeamFates;

    const MAP: &str = "Marble_Madness_Map";
    const DT: std::time::Duration = Duration::from_micros(33_333);

    struct FullPeer {
        id: PeerId,
        team: Option<u8>,
        h: SimHarness,
        scripted: u32,
        present: bool,
    }

    struct Room {
        net: SimNetwork,
        peers: Vec<FullPeer>,
        keys: BTreeMap<PeerId, PlayerKey>,
        map_hash: u64,
        dumps: std::path::PathBuf,
    }

    fn net_session(h: &mut SimHarness, next: NextNetMatch, dumps: &std::path::Path) {
        let w = h.world_mut();
        let mut net = NetSession::from_next(next);
        // A fast machine whatever this one is: the host's speed control
        // (`net::speed`) leaves the game at full speed (as
        // `headless::netplay`).
        net.pacer.cost = Some(Duration::from_millis(1));
        w.insert_resource(net);
        w.insert_resource(DumpDir(dumps.to_path_buf()));
        w.resource_mut::<HashLog>().enabled = true;
    }

    fn scenario_of(setup: &GameSetup) -> Scenario {
        Scenario {
            map: setup.map.clone(),
            seats: setup.players.clone(),
            seed: setup.seed,
            executor: Executor::Single,
            difficulty: setup.difficulty,
            sandbox: setup.sandbox,
            rules: setup.rules,
            map_file: None,
        }
    }

    impl Room {
        /// Seats 0 and 1 (humans) against seat 2 (human) and an AI;
        /// peers 1–3 play seats 0–2; the host drops a disconnected seat
        /// at once (no grace), so the return is a sequenced `Returned`.
        fn start(seed: u64) -> Self {
            let setup = GameSetup {
                map: MAP.into(),
                players: (0..4)
                    .map(|i| PlayerSpec {
                        faction: Faction::ALL[i % 3],
                        ally_team: u8::from(i >= 2),
                        ai: i == 3,
                    })
                    .collect(),
                seed,
                ..GameSetup::default()
            };
            let net = SimNetwork::new(seed);
            net.set_default_link(LinkParams::latency_ms(35));
            let roster: BTreeMap<PeerId, Option<u8>> =
                (0..3u8).map(|i| (PeerId(i as u128 + 1), Some(i))).collect();
            let keys: BTreeMap<PeerId, PlayerKey> = roster
                .keys()
                .map(|p| (*p, PlayerKey(0x7000 + p.0 as u64)))
                .collect();
            let dumps = crate::paths::project_root()
                .join("target")
                .join(format!("rejoin-dumps-{seed:x}"));
            let _ = std::fs::remove_dir_all(&dumps);
            let mut map_hash = 0;
            let mut peers = Vec::new();
            for (&id, &team) in &roster {
                let mut h = SimHarness::new(scenario_of(&setup));
                map_hash = h
                    .world_mut()
                    .resource::<crate::net::replay_log::ActiveMapHash>()
                    .0;
                let door = (id == PeerId(1)).then(|| Door {
                    start: StartMsg {
                        match_id: seed,
                        setup: encode_setup(&setup),
                        roster: roster.iter().map(|(p, s)| (*p, *s)).collect(),
                        compat: Compat::of(Build::current(), map_hash),
                    },
                    keys: keys.clone(),
                    names: roster
                        .keys()
                        .map(|p| (*p, format!("player{}", p.0)))
                        .collect(),
                    in_room: Default::default(),
                });
                let next = NextNetMatch {
                    config: SessionConfig {
                        match_id: seed,
                        me: id,
                        roster: roster.clone(),
                        delay: Default::default(),
                    },
                    transport: Box::new(net.join(id)),
                    catch_up: CatchUp {
                        budget: None,
                        ..CatchUp::default()
                    },
                    join: None,
                    door,
                };
                net_session(&mut h, next, &dumps);
                h.world_mut()
                    .resource_mut::<NetSession>()
                    .session
                    .set_timeouts(Timeouts {
                        rejoin: Duration::ZERO,
                        ..Timeouts::default()
                    });
                peers.push(FullPeer {
                    id,
                    team,
                    h,
                    scripted: 0,
                    present: true,
                });
            }
            Self {
                net,
                peers,
                keys,
                map_hash,
                dumps,
            }
        }

        /// One frame of every present peer, each playing its seat with
        /// the golden script once it is live (a late joiner skips the
        /// ticks it replays).
        fn frame(&mut self) {
            for peer in self.peers.iter_mut().filter(|p| p.present) {
                let world = peer.h.world_mut();
                let tick = world.resource::<CommandBus>().tick;
                let live = !world.resource::<NetSession>().pacer.fast_forward;
                if let (Some(team), true) = (peer.team, live) {
                    for t in peer.scripted + 1..=tick {
                        let cmds =
                            golden::script_for(world, t, &[team], |me| if me < 2 { 2 } else { 0 });
                        let mut bus = world.resource_mut::<CommandBus>();
                        for (_, list) in cmds {
                            for cmd in list {
                                bus.issue(cmd);
                            }
                        }
                    }
                }
                peer.scripted = peer.scripted.max(tick);
                peer.h.frame_dt(DT);
            }
        }

        fn run(&mut self, frames: u32) {
            for _ in 0..frames {
                self.net.advance(DT);
                self.frame();
            }
        }

        /// A newcomer with `key` opens the room as `id`; once its lobby
        /// has the host's `Running`, it loads the match the way the game
        /// does and joins late.
        fn knock(&mut self, id: PeerId, key: PlayerKey) -> usize {
            let mut lobby = Lobby::new(Box::new(self.net.join(id)), "late", MAP).with_key(key);
            lobby.provide_map(MAP, MapHave::Hash(self.map_hash));
            for _ in 0..300 {
                self.net.advance(DT);
                lobby.poll(self.net.now());
                self.frame();
                let Some(ev) = lobby.take_events().pop() else {
                    continue;
                };
                let LobbyEvent::Start(start) = ev else {
                    panic!("refused: {ev:?}");
                };
                let start = *start;
                let team = start.session.roster[&id];
                // The network clock stands still while it loads.
                let mut h = SimHarness::new(scenario_of(&start.setup));
                let next = NextNetMatch {
                    config: start.session,
                    transport: lobby.into_transport(),
                    catch_up: CatchUp {
                        budget: None,
                        ..CatchUp::default()
                    },
                    join: start.join,
                    door: start.door,
                };
                net_session(&mut h, next, &self.dumps);
                self.peers.push(FullPeer {
                    id,
                    team,
                    h,
                    scripted: 0,
                    present: true,
                });
                return self.peers.len() - 1;
            }
            panic!("{id} never got into the match");
        }

        fn trail(&mut self, i: usize) -> Vec<u64> {
            self.peers[i]
                .h
                .world_mut()
                .resource::<HashLog>()
                .ticks
                .iter()
                .map(|t| t.sim)
                .collect()
        }

        fn tick(&mut self, i: usize) -> u32 {
            self.peers[i].h.world_mut().resource::<CommandBus>().tick
        }
    }

    /// The whole game: seat 1's connection breaks at tick ~150 and the
    /// host drops it (its ally, seat 0, plays on: its units wait idle);
    /// two seconds later the player opens the room again under a new
    /// connection with its key, replays the match from tick 0 in a
    /// frame or two, and `Returned` puts seat 1 back in play on every
    /// peer; its script's orders apply again. Then a newcomer watches.
    /// Every trail — the returning player's and the watcher's from tick
    /// 1 — is the host's, with no desync; the host's recording of the
    /// match (`replay_log::replay_of`) plays back through the replay
    /// player to the host's trail.
    #[test]
    fn rejoin_full_sim() {
        let _serial = super::super::multipeer_tests::SERIAL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut r = Room::start(0x4e70);
        r.run(150);
        r.net.leave(PeerId(2));
        r.peers[1].present = false;
        r.run(60);
        let dropped_seen = r.peers[0]
            .h
            .world_mut()
            .resource::<MatchControl>()
            .dropped
            .contains(&1);
        assert!(dropped_seen, "seat 1 was dropped");
        let key = r.keys[&PeerId(2)];
        let back = r.knock(PeerId(20), key);
        r.run(60);
        let watcher = r.knock(PeerId(21), PlayerKey(0xfeed));
        while r.tick(0) < 560 {
            r.run(30);
        }
        r.run(10);

        // In step, from tick 1.
        let host = r.trail(0);
        for i in [2, back, watcher] {
            let t = r.trail(i);
            assert!(
                t.len() + 10 >= host.len(),
                "peer {i}: {} of {} ticks",
                t.len(),
                host.len()
            );
            let n = t.len().min(host.len());
            if let Some(k) = (0..n).find(|&k| t[k] != host[k]) {
                panic!("{} diverges from the host at tick {}", r.peers[i].id, k + 1);
            }
            let st = r.peers[i].h.world_mut().resource::<NetStatus>().clone();
            assert!(st.desync.is_none() && st.ended.is_none());
            assert!(st.catching_up.is_none(), "peer {i} is live");
        }
        // Seat 1 is back in play everywhere, the watcher watches.
        for i in [0, 2, back, watcher] {
            let w = r.peers[i].h.world_mut();
            let c = w.resource::<MatchControl>();
            assert!(
                !c.resigned.contains(&1) && !c.dropped.contains(&1),
                "peer {i}"
            );
            assert!(!w.resource::<TeamFates>().is_dead(1), "peer {i}");
        }
        assert_eq!(r.peers[back].team, Some(1));
        assert_eq!(r.peers[watcher].team, None);
        let local = r.peers[watcher]
            .h
            .world_mut()
            .resource::<crate::units::player::LocalTeam>()
            .0;
        assert_eq!(local, crate::game_setup::SPECTATOR_TEAM);
        // The host's log: the drop, the return, and seat 1's orders after.
        let log = r.peers[0]
            .h
            .world_mut()
            .resource::<NetSession>()
            .session
            .log()
            .to_vec();
        let at = |cmd: SimCommand| {
            log.iter()
                .find(|f| f.player == 1 && f.commands.contains(&cmd))
                .map(|f| f.tick)
        };
        let (dropped, returned) = (at(SimCommand::Dropped), at(SimCommand::Returned));
        let returned = returned.expect("a Returned in the log");
        assert!(dropped.expect("a Dropped in the log") < returned);
        let after = log
            .iter()
            .filter(|f| f.player == 1 && f.tick > returned)
            .count();
        assert!(after > 0, "seat 1 ordered again after its return");
        eprintln!(
            "rejoin full sim: seat 1 dropped at tick {dropped:?}, back at tick {returned}, \
             {after} frames of orders after; {} ticks in step on 4 peers (the returning \
             player's and the watcher's from tick 1)",
            host.len()
        );

        // The host's recording plays back to its trail.
        let host_world = r.peers[0].h.world_mut();
        let recorded = crate::net::replay_log::replay_of(host_world).expect("a replay");
        assert!(recorded.meta.networked);
        assert_eq!(
            recorded.setup,
            crate::net::replay_log::MatchSetup::of(&host_world.resource::<ActiveMatch>().0)
        );
        let full_trail = replay::trail_text(&host_world.resource::<HashLog>().ticks);
        let recorded = replay::Replay::decode(&recorded.encode()).unwrap();
        let Ok(map) = MapInput::checkout(MAP) else {
            eprintln!("no baked {MAP}: the replay round trip is skipped");
            return;
        };
        drop(r);
        let played =
            bevy::tasks::block_on(replay::play(&recorded, map, &PlayOptions::default())).unwrap();
        let trail = replay::trail_text(&played.hashes);
        let expect: String = full_trail
            .lines()
            .take(recorded.ticks as usize)
            .map(|l| format!("{l}\n"))
            .collect();
        if let Some(line) = replay::first_difference(&expect, &trail) {
            panic!(
                "the replay diverges from the match at tick {line}:\n  match  {}\n  replay {}",
                expect.lines().nth(line - 1).unwrap_or("<end>"),
                trail.lines().nth(line - 1).unwrap_or("<end>")
            );
        }
        eprintln!(
            "the host's replay ({} frames, {} commands) plays back to the match's {} ticks",
            recorded.frames.len(),
            recorded.commands(),
            recorded.ticks
        );
    }
}

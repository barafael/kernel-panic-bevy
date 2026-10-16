//! Lockstep with the whole game: several headless apps in one process
//! (the M0 harness, [`SimHarness`]), one per peer, each running the real
//! tick driver over one [`SimNetwork`] ([`NetPeer`]). Every peer plays
//! its own seat with the golden-replay command script and
//! the AI seats run in every sim; every peer's per-tick state hash trail
//! must equal the host's.
//!
//! The CI subset (2 and 8 peers, a forced desync, a dead link, peers
//! leaving) keeps to short matches; the `#[ignore]`d `long_match` takes
//! its size from the environment:
//!
//! ```text
//! KP_MP_PEERS=8 KP_MP_TICKS=6000 KP_MP_LATENCY=80 KP_MP_LOSS=0.03 \
//!   cargo test --release -p kernel-panic net::multipeer_tests::long_match -- --ignored --nocapture
//! ```
//!
//! Without baked `.kpmap`s the maps load from their `.sd7` archives
//! (slower, same content).

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use bevy::prelude::*;

use super::driver::{DumpDir, NetSession, NetStatus};
use super::session::{Ended, Session};
use super::sim_net::{LinkParams, SimNetwork};
use super::test_support::{self, env};
use super::transport::PeerId;
use super::{CommandBus, NetId};
use crate::headless::netplay::{NetPeer, scenario};
use crate::headless::{HashLog, Scenario, SimHarness};

/// One full app at a time per test, and one test at a time: eight apps
/// are a few GB.
pub(super) static SERIAL: Mutex<()> = Mutex::new(());

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// The peers of one [`SimNetwork`], each a [`NetPeer`] (the host
/// first).
pub(super) struct Room {
    pub(super) net: SimNetwork,
    pub(super) peers: Vec<NetPeer>,
    dumps: std::path::PathBuf,
}

/// One peer per human seat of `scn` (seat `i` played by `PeerId(i + 1)`,
/// the host peer 1), on links `link`.
pub(super) fn start(scn: &Scenario, seed: u64, link: LinkParams) -> Room {
    let net = SimNetwork::new(seed);
    net.set_default_link(link);
    let humans = scn.seats.iter().take_while(|s| !s.ai).count() as u8;
    assert!(
        scn.seats[humans as usize..].iter().all(|s| s.ai),
        "the human seats first"
    );
    let ids: Vec<PeerId> = (1..=humans).map(|i| PeerId(i as u128)).collect();
    let dumps = crate::paths::project_root()
        .join("target")
        .join(format!("mp-dumps-{seed:x}"));
    let _ = std::fs::remove_dir_all(&dumps);
    let peers = ids
        .iter()
        .map(|&id| {
            let h = SimHarness::new(scn.clone());
            let transport = Box::new(net.join(id));
            let mut peer = NetPeer::join(h, id, &ids, humans, seed, transport);
            peer.h.world_mut().insert_resource(DumpDir(dumps.clone()));
            peer
        })
        .collect();
    Room { net, peers, dumps }
}

impl Room {
    /// Run `frames` frames of `dt` on every present peer (in order),
    /// each playing its seat's script; `each` sees the room first.
    pub(super) fn run(&mut self, frames: u32, dt: Duration, mut each: impl FnMut(&mut Room, u32)) {
        for f in 0..frames {
            self.net.advance(dt);
            each(self, f);
            for peer in self.peers.iter_mut().filter(|p| p.present) {
                peer.frame(dt);
            }
        }
    }

    pub(super) fn trail(&mut self, i: usize) -> Vec<u64> {
        self.peers[i].trail()
    }

    /// Every present peer's trail equals the host's over the common
    /// prefix, and is at least `min` ticks long.
    pub(super) fn assert_agree(&mut self, min: usize) -> usize {
        let (host, guests) = self.peers.split_first_mut().expect("a host");
        let host = (host.me, host.trail());
        let guests = guests.iter_mut().filter(|p| p.present);
        test_support::assert_agree(
            std::iter::once(host).chain(guests.map(|p| (p.me, p.trail()))),
            min,
        )
    }

    pub(super) fn status(&mut self, i: usize) -> NetStatus {
        self.peers[i].h.world_mut().resource::<NetStatus>().clone()
    }

    pub(super) fn session<R>(&mut self, i: usize, f: impl FnOnce(&Session) -> R) -> R {
        self.peers[i].session(f)
    }

    /// Delay, lateness, round trips and traffic, for the write-up.
    pub(super) fn report(&mut self, label: &str) {
        let delay = self.session(0, |s| s.delay());
        let late = self.session(0, |s| s.stats.late_batches);
        let rtts: Vec<String> = (1..self.peers.len())
            .map(|i| {
                let id = self.peers[i].me;
                let r = self.session(0, |s| s.rtt(id)).unwrap_or(0.0);
                format!("{r:.0}")
            })
            .collect();
        let ticks = self.trail(0).len();
        eprintln!(
            "{label}: {ticks} ticks, delay {delay}, late batches {late}, rtt [{}] ms",
            rtts.join(" ")
        );
        // Payload bytes per second of network time, both channels.
        let secs = self.net.now().as_secs_f64();
        for (i, peer) in self.peers.iter().enumerate().filter(|(i, _)| *i < 2) {
            let t = self.net.traffic(peer.me);
            let up: u64 = t.iter().map(|c| c.sent_bytes).sum();
            let msgs: u64 = t.iter().map(|c| c.sent_msgs).sum();
            let down: u64 = t.iter().map(|c| c.recv_bytes).sum();
            eprintln!(
                "  {}: up {:.0} B/s in {:.0} msg/s, down {:.0} B/s",
                if i == 0 { "host " } else { "guest" },
                up as f64 / secs,
                msgs as f64 / secs,
                down as f64 / secs,
            );
        }
    }
}

/// Two peers and an AI on Marble Madness, 60 ms ± 20 one way, 3 % loss,
/// duplicates and reordering, 60 Hz frames: identical every tick.
#[test]
fn two_peers_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scn = scenario("Marble_Madness_Map", 2, 1, 0x2b);
    let link = LinkParams {
        latency: ms(60),
        jitter: ms(20),
        loss: 0.03,
        duplicate: 0.01,
        reorder: 0.03,
        down: false,
    };
    let mut r = start(&scn, 0x2b, link);
    r.run(60 * 22, Duration::from_micros(16_667), |_, _| {});
    let n = r.assert_agree(550);
    r.report("2 peers full sim");
    assert!(r.session(0, |s| s.desync().is_none()));
    // The script's orders went through: both seats applied commands.
    let applied: usize = r.peers[1].h.world_mut().resource::<CommandBus>().tick as usize;
    assert!(applied >= n);
}

/// Eight peers on Quad Core, links from 0 to 200 ms one way with
/// jitter, 1–5 % loss and reordering.
#[test]
fn eight_peers_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scn = scenario("Quad_Core", 8, 0, 0x8b);
    let mut r = start(&scn, 0x8b, LinkParams::default());
    for i in 1..8u64 {
        let link = LinkParams {
            latency: ms(i * 28),
            jitter: ms(5 + i * 3),
            loss: 0.01 + 0.04 * (i as f64 / 7.0),
            duplicate: 0.01,
            reorder: 0.03,
            down: false,
        };
        r.net.set_link_both(PeerId(1), PeerId(i as u128 + 1), link);
    }
    r.run(30 * 16, Duration::from_micros(33_333), |_, _| {});
    r.assert_agree(300);
    r.report("8 peers full sim");
    assert!(r.session(0, |s| s.desync().is_none()));
}

/// A unit nudged on one guest: the host finds it at the next hash frame,
/// every peer stops at the same tick and writes a dump.
#[test]
fn forced_desync_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scn = scenario("Marble_Madness_Map", 3, 1, 0xd5);
    let mut r = start(&scn, 0xd5, LinkParams::latency_ms(30));
    let mut nudged_after = None;
    r.run(30 * 14, Duration::from_micros(33_333), |r, _| {
        let p = &mut r.peers[2];
        let ticks = p.h.world_mut().resource::<HashLog>().ticks.len();
        if nudged_after.is_none() && ticks >= 200 {
            let w = p.h.world_mut();
            let mut q = w.query::<(&NetId, &mut Transform)>();
            let (_, mut tf) = q.iter_mut(w).min_by_key(|(id, _)| **id).expect("units");
            tf.translation.x += 1.0;
            nudged_after = Some(ticks as u32);
        }
    });
    let nudged = nudged_after.expect("nudged");
    let d = r.session(0, |s| s.desync().cloned()).expect("detected");
    eprintln!(
        "desync: nudged after tick {nudged}, found at frame {}, everyone stops after {}",
        d.frame, d.stop_at
    );
    assert_eq!(d.peers, vec![PeerId(3)]);
    assert!(
        d.frame > nudged && d.frame <= nudged + 30,
        "within 30 ticks"
    );
    for i in 0..r.peers.len() {
        let st = r.status(i);
        assert_eq!(st.desync.as_ref().map(|x| x.stop_at), Some(d.stop_at));
        let ticks = r.trail(i).len() as u32;
        assert_eq!(ticks, d.stop_at, "peer {i} paused at the stop");
        let dump = st.desync.and_then(|d| d.dump).expect("dumped");
        let text = std::fs::read_to_string(&dump).expect("dump file");
        assert!(text.contains(&format!("frame {}", d.frame)));
        assert!(text.lines().count() > 20, "a per-unit dump");
    }
    let _ = std::fs::remove_dir_all(&r.dumps);
}

/// A guest's link is dead for 4 s and comes back: the guest waits, the
/// others play on, it catches up, and everyone agrees.
#[test]
fn dead_link_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scn = scenario("Marble_Madness_Map", 3, 0, 0x5a);
    let link = LinkParams {
        latency: ms(40),
        jitter: ms(10),
        loss: 0.01,
        ..Default::default()
    };
    let mut r = start(&scn, 0x5a, link);
    let mut waited = false;
    r.run(60 * 13, Duration::from_micros(16_667), |r, f| {
        if f == 60 * 4 {
            r.net
                .set_link_both(PeerId(1), PeerId(3), LinkParams { down: true, ..link });
        }
        if f == 60 * 6 {
            let st = r.status(2);
            waited |= st.waiting.is_some_and(|(who, _)| who == vec![PeerId(1)]);
        }
        if f == 60 * 8 {
            r.net.set_link_both(PeerId(1), PeerId(3), link);
        }
    });
    assert!(
        waited,
        "the cut-off guest showed it was waiting for the host"
    );
    let n = r.assert_agree(320);
    let host = r.trail(0).len();
    assert!(host - n < 10, "caught up: host {host}, guest {n}");
    r.report("dead link 4 s");
}

/// A guest leaves: its units stay idle, the others play on. Then the
/// host leaves: the match is over.
#[test]
fn peers_leaving_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scn = scenario("Marble_Madness_Map", 3, 1, 0x1e);
    let mut r = start(&scn, 0x1e, LinkParams::latency_ms(25));
    r.run(30 * 6, Duration::from_micros(33_333), |r, f| {
        if f == 30 * 3 {
            r.net.leave(PeerId(3));
            r.peers[2].present = false;
        }
    });
    r.assert_agree(150);
    assert_eq!(r.session(0, |s| s.guests()), vec![PeerId(2)]);
    r.net.leave(PeerId(1));
    r.peers[0].present = false;
    r.run(30, Duration::from_micros(33_333), |_, _| {});
    assert_eq!(r.status(1).ended, Some(Ended::HostLeft));
    let stopped = r.trail(1).len();
    r.run(30, Duration::from_micros(33_333), |_, _| {});
    assert_eq!(r.trail(1).len(), stopped, "no host, no clock");
}

/// Match control across peers: a guest pauses and later resumes, another
/// doubles the game speed. Every peer stops at the same tick and stays
/// there, resumes together, runs faster from the same tick, and the
/// trails agree.
#[test]
fn pause_and_speed_full_sim() {
    use super::{MatchControl, SimCommand};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let scn = scenario("Marble_Madness_Map", 3, 1, 0x9a);
    let mut r = start(&scn, 0x9a, LinkParams::latency_ms(40));
    let issue = |r: &mut Room, peer: usize, cmd: SimCommand| {
        r.peers[peer]
            .h
            .world_mut()
            .resource_mut::<CommandBus>()
            .issue(cmd);
    };
    let control = |r: &mut Room, peer: usize| {
        r.peers[peer]
            .h
            .world_mut()
            .resource::<MatchControl>()
            .clone()
    };
    // Ticks of the host at frames 300 and 330 (paused), 420 and 480
    // (1×), 600 and 660 (2×).
    let mut at = BTreeMap::new();
    r.run(30 * 24, Duration::from_micros(33_333), |r, f| {
        match f {
            200 => issue(r, 1, SimCommand::Pause { on: true }),
            360 => issue(r, 1, SimCommand::Pause { on: false }),
            500 => issue(r, 2, SimCommand::SetSpeed { percent: 200 }),
            _ => {}
        }
        if [300, 330, 420, 480, 600, 660].contains(&f) {
            at.insert(f, r.trail(0).len());
        }
        if f == 330 {
            for i in 0..r.peers.len() {
                let c = control(r, i);
                assert!(c.paused, "peer {i} paused");
                assert_eq!(c.paused_by, Some(1));
            }
        }
        if f == 600 {
            for i in 0..r.peers.len() {
                assert!(!control(r, i).paused);
                assert_eq!(control(r, i).speed_percent, 200, "peer {i} at 2×");
            }
        }
    });
    assert_eq!(at[&300], at[&330], "no tick while paused");
    let normal = at[&480] - at[&420];
    let fast = at[&660] - at[&600];
    assert!((55..=65).contains(&normal), "1×: {normal} ticks in 2 s");
    assert!((115..=125).contains(&fast), "2×: {fast} ticks in 2 s");
    r.assert_agree(at[&660] - 10);
    assert!(r.session(0, |s| s.desync().is_none()));
}

/// Two peers press TAKE on the same frame for their resigned ally's
/// units: one gets all of them, the other's `Take` finds nothing, and
/// every peer agrees on which (the host's turn order decides).
#[test]
fn take_race_full_sim() {
    use super::SimCommand;
    use crate::units::combat::Dying;
    use crate::units::components::{TeamId, UnitType};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // Three human allies against an AI.
    let mut scn = scenario("Marble_Madness_Map", 3, 1, 0x7ace);
    for (i, seat) in scn.seats.iter_mut().enumerate() {
        seat.ally_team = u8::from(i == 3);
    }
    let mut r = start(&scn, 0x7ace, LinkParams::latency_ms(30));
    let issue = |r: &mut Room, peer: usize, cmd: SimCommand| {
        r.peers[peer]
            .h
            .world_mut()
            .resource_mut::<CommandBus>()
            .issue(cmd);
    };
    r.run(30 * 16, Duration::from_micros(33_333), |r, f| match f {
        150 => issue(r, 2, SimCommand::Resign),
        250 => {
            issue(r, 0, SimCommand::Take { from: 2 });
            issue(r, 1, SimCommand::Take { from: 2 });
        }
        _ => {}
    });
    r.assert_agree(400);
    let owners = |r: &mut Room, peer: usize| {
        let w = r.peers[peer].h.world_mut();
        let mut q = w.query_filtered::<&TeamId, (With<UnitType>, Without<Dying>)>();
        let mut n = [0u32; 4];
        for t in q.iter(w) {
            n[t.0 as usize] += 1;
        }
        n
    };
    let host = owners(&mut r, 0);
    assert_eq!(host[2], 0, "all of owner 2's units were taken");
    for peer in 1..3 {
        assert_eq!(owners(&mut r, peer), host, "peer {peer} agrees");
    }
}

/// Long and configurable.
#[test]
#[ignore = "long full-sim match; --ignored, env knobs in the module doc"]
fn long_match() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let peers: u8 = env("KP_MP_PEERS", 4);
    let ticks: u32 = env("KP_MP_TICKS", 3000);
    let link = LinkParams {
        latency: ms(env("KP_MP_LATENCY", 80)),
        jitter: ms(env("KP_MP_JITTER", 20)),
        loss: env("KP_MP_LOSS", 0.02),
        duplicate: 0.01,
        reorder: 0.02,
        down: false,
    };
    let map = std::env::var("KP_MP_MAP").unwrap_or_else(|_| "Quad_Core".into());
    let scn = scenario(&map, peers, 0, 0x10);
    let mut r = start(&scn, 0x10, link);
    let t0 = std::time::Instant::now();
    r.run(ticks * 2, Duration::from_micros(16_667), |_, _| {});
    r.assert_agree(ticks as usize * 9 / 10);
    r.report(&format!("long match, {peers} peers on {map}"));
    eprintln!("wall time {:.1} s", t0.elapsed().as_secs_f64());
    assert!(r.session(0, |s| s.desync().is_none()));
}

/// What a desync check costs: the state hash (every 30 ticks) along a
/// 4v4 AI match, sampled every 1 500 ticks for `KP_HASH_TICKS` ticks;
/// `KP_HASH_MAP` / `KP_HASH_FFA=1` pick the 8-player FFA instead.
#[test]
#[ignore = "runs a long AI match; --ignored --nocapture, --release for real numbers"]
fn hash_cost() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let ticks: u64 = std::env::var("KP_HASH_TICKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9000);
    let scn = if std::env::var("KP_HASH_FFA").is_ok_and(|v| v == "1") {
        Scenario::ai_match("Quad_Core", 8, 1, 0xffa8)
    } else {
        Scenario::ai_match("Speed_Balls_16_Way", 2, 4, 0x5eed)
    };
    let mut h = SimHarness::new(scn);
    let mut tick_time = Duration::ZERO;
    eprintln!("  tick  units  proj  entities  hash/call  tick(avg)  hash share");
    for t in 1..=ticks {
        h.step();
        tick_time += h.take_tick_times().iter().sum::<Duration>();
        if t % 1500 != 0 {
            continue;
        }
        let w = h.world_mut();
        let n = 50;
        let t0 = std::time::Instant::now();
        let mut c = super::state::capture(w);
        for _ in 1..n {
            c = super::state::capture(w);
        }
        let per = t0.elapsed() / n;
        let tick = tick_time / 1500;
        tick_time = Duration::ZERO;
        eprintln!(
            "{t:>6} {:>6} {:>5} {:>9} {:>10.0?} {:>10.2?} {:>9.2} %",
            c.units,
            c.projectiles,
            c.total_entities,
            per,
            tick,
            100.0 * per.as_secs_f64() / 30.0 / tick.as_secs_f64(),
        );
    }
    let t1 = std::time::Instant::now();
    let lines = super::state::describe(h.world_mut()).len();
    eprintln!("desync dump: {:?} for {lines} lines", t1.elapsed());
}

/// M2 drops with the whole game: four humans and an AI (seats 0, 1 on
/// one side, 2, 3 and the AI on the other). Peer 4 leaves the way the
/// menu does it (resign, then the driver closes its transport); peer
/// 3's connection just vanishes (a crash). The host sequences a
/// `Dropped` for each seat; on both remaining peers both seats leave
/// play on the same tick, only the crashed one counts as dropped (the
/// other had resigned), and the trails agree.
#[test]
fn drops_full_sim() {
    use super::SimCommand;
    use crate::units::teams::TeamFates;
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut scn = scenario("Marble_Madness_Map", 4, 1, 0xd209);
    for (i, seat) in scn.seats.iter_mut().enumerate() {
        seat.ally_team = u8::from(i >= 2);
    }
    let mut r = start(&scn, 0xd209, LinkParams::latency_ms(35));
    // The crash is dropped at once: no grace for a return (M3).
    r.peers[0]
        .h
        .world_mut()
        .resource_mut::<NetSession>()
        .session
        .set_timeouts(super::session::Timeouts {
            rejoin: Duration::ZERO,
            ..Default::default()
        });
    let mut closed_at = 0;
    r.run(30 * 9, Duration::from_micros(33_333), |r, f| {
        if f == 60 {
            let w = r.peers[3].h.world_mut();
            w.resource_mut::<CommandBus>().issue(SimCommand::Resign);
            w.resource_mut::<NetSession>().request_leave();
        }
        if r.peers[3].present && r.status(3).closed {
            r.peers[3].present = false;
            closed_at = f;
        }
        if f == 120 {
            r.net.leave(PeerId(3));
            r.peers[2].present = false;
        }
    });
    let leaver = r.status(3);
    assert_eq!(leaver.ended, Some(Ended::Left));
    assert!(!r.peers[3].present, "closed within the run");
    r.assert_agree(200);
    let fates = |r: &mut Room, i: usize| {
        let w = r.peers[i].h.world_mut();
        let f = w.resource::<TeamFates>();
        (f.left_at(2), f.left_at(3))
    };
    let host = fates(&mut r, 0);
    assert!(host.0.is_some() && host.1.is_some(), "both left: {host:?}");
    eprintln!(
        "drops: Leave match asked at frame 60, closed at frame {closed_at}; \
         seat 3 left play at tick {:?}, seat 2 (gone at frame 120) at tick {:?}",
        host.1, host.0
    );
    assert_eq!(fates(&mut r, 1), host, "on the same ticks");
    for peer in &mut r.peers[..2] {
        let w = peer.h.world_mut();
        let c = w.resource::<super::MatchControl>();
        assert_eq!(c.dropped.iter().copied().collect::<Vec<_>>(), vec![2]);
        assert_eq!(c.resigned.iter().copied().collect::<Vec<_>>(), vec![2, 3]);
    }
    let st = r.status(1);
    let gone: Vec<PeerId> = st.peers.iter().filter(|p| p.gone).map(|p| p.peer).collect();
    assert_eq!(gone, vec![PeerId(3), PeerId(4)], "the player list knows");
    r.report("drops");
}

/// The host's match rules across peers: pausing off (only the host may),
/// speeds 50–200 %. The host's seat goes into the rules from the roster
/// on every peer; a guest's pause and an out-of-range speed are refused
/// by the host before they are sequenced; the host's pause holds
/// everyone.
#[test]
fn host_rules_full_sim() {
    use super::{MatchControl, MatchRules, SimCommand};
    use crate::game_setup::ActiveMatch;
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut scn = scenario("Marble_Madness_Map", 2, 1, 0x7e5);
    scn.rules = MatchRules {
        pausable: false,
        min_speed: 50,
        max_speed: 200,
        ..MatchRules::default()
    };
    let mut r = start(&scn, 0x7e5, LinkParams::latency_ms(40));
    let issue = |r: &mut Room, peer: usize, cmd: SimCommand| {
        r.peers[peer]
            .h
            .world_mut()
            .resource_mut::<CommandBus>()
            .issue(cmd);
    };
    let control = |r: &mut Room, peer: usize| {
        r.peers[peer]
            .h
            .world_mut()
            .resource::<MatchControl>()
            .clone()
    };
    r.run(30 * 8, Duration::from_micros(33_333), |r, f| match f {
        40 => {
            issue(r, 1, SimCommand::Pause { on: true });
            issue(r, 1, SimCommand::SetSpeed { percent: 400 });
            issue(r, 1, SimCommand::SetSpeed { percent: 150 });
        }
        100 => {
            for i in 0..2 {
                let c = control(r, i);
                assert!(!c.paused, "peer {i}: a guest may not pause");
                assert_eq!(c.speed_percent, 150, "peer {i}");
            }
            issue(r, 0, SimCommand::Pause { on: true });
        }
        160 => {
            for i in 0..2 {
                assert!(control(r, i).paused, "peer {i}: the host may");
            }
            issue(r, 0, SimCommand::Pause { on: false });
        }
        _ => {}
    });
    for peer in &mut r.peers[..2] {
        let w = peer.h.world_mut();
        assert_eq!(w.resource::<ActiveMatch>().0.rules.host, Some(0));
        assert!(!w.resource::<MatchControl>().paused);
    }
    assert_eq!(r.session(0, |s| s.stats.refused), 2);
    r.assert_agree(150);
}

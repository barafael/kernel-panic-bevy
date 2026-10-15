//! The lockstep runtime without the game: [`drive_frame`] and the
//! [`Session`] protocol over the simulated network, with a stand-in sim
//! whose state is a hash of every turn it applied. Fast enough to sweep
//! network conditions (latency, jitter, loss, duplication, reordering,
//! dead links, peers leaving) at 2 and 8 peers in milliseconds; the
//! full-sim versions are `multipeer_tests`.
//!
//! The measurements of `net-experiments/m1-runtime.md` §5 print with
//! `--nocapture`:
//!
//! ```text
//! cargo test -p kernel-panic net::session_tests -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use super::driver::{CatchUp, FrameReport, GameSpeed, Pacer, TickRunner, drive_frame};
use super::session::{DelayBounds, Desync, Ended, Session, SessionConfig, Timeouts};
use super::sim_net::{LinkParams, SimNetwork};
use super::transport::PeerId;
use super::wire::Turn;
use super::{CommandFrame, NetId, SimCommand, Units};

const TICK: Duration = Duration::from_nanos(1_000_000_000 / 30);

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A stand-in sim: its state folds in every applied turn, so any
/// difference in the turn stream shows in the trail. `SetRepeat` doubles
/// as pause (`on`) and resume.
#[derive(Default)]
struct Fake {
    player: Option<u8>,
    tick: u32,
    state: u64,
    trail: Vec<u64>,
    staged: Vec<SimCommand>,
    speed: GameSpeed,
    /// (local tick at issue, command id)
    issued: Vec<(u32, u32)>,
    /// command id → tick applied
    applied: BTreeMap<u32, u32>,
    /// Flip a state bit after this tick (a forced desync).
    perturb_after: Option<u32>,
    dumps: u32,
    /// (tick, player) of every `Dropped` applied.
    dropped: Vec<(u32, u8)>,
}

fn order(id: u32) -> SimCommand {
    SimCommand::Stop {
        units: Units::of([NetId(id)]),
    }
}

fn id_of(cmd: &SimCommand) -> Option<u32> {
    match cmd {
        SimCommand::Stop {
            units: Units::List(l),
        } => l.ids().first().map(|n| n.0),
        _ => None,
    }
}

impl Fake {
    fn issue(&mut self, id: u32) {
        self.issued.push((self.tick, id));
        self.staged.push(order(id));
    }

    fn mix(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.state ^= b as u64;
            self.state = self.state.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl TickRunner for Fake {
    fn speed(&self) -> GameSpeed {
        self.speed
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
                if let Some(id) = id_of(c) {
                    assert!(
                        self.applied.insert(id, turn.tick).is_none(),
                        "command {id} applied twice"
                    );
                }
                if let SimCommand::Pause { on } = c {
                    self.speed.paused = *on;
                }
                if matches!(c, SimCommand::Dropped) {
                    self.dropped.push((turn.tick, *player));
                }
            }
        }
        self.tick += 1;
        if self.perturb_after == Some(self.tick) {
            self.state ^= 1;
        }
        self.trail.push(self.state);
    }

    fn hash(&mut self) -> u64 {
        self.state
    }

    fn dump(&mut self, _session: &Session, _desync: &Desync) -> String {
        self.dumps += 1;
        "fake".into()
    }
}

struct Peer {
    id: PeerId,
    s: Session,
    p: Pacer,
    sim: Fake,
    /// Frame period and the next frame's time.
    period: Duration,
    next_frame: Duration,
    last_frame: Duration,
    frames: Vec<FrameReport>,
    /// Issue a command every this many of its own ticks (0: never).
    every: u32,
    issued_n: u32,
}

struct Room {
    net: SimNetwork,
    peers: Vec<Peer>,
}

fn room(n: usize, seed: u64, link: LinkParams) -> Room {
    let net = SimNetwork::new(seed);
    net.set_default_link(link);
    let roster: BTreeMap<PeerId, Option<u8>> = (0..n)
        .map(|i| (PeerId(i as u128 + 1), Some(i as u8)))
        .collect();
    let peers = (0..n)
        .map(|i| {
            let id = PeerId(i as u128 + 1);
            let ep = net.join(id);
            let cfg = SessionConfig {
                match_id: 77,
                me: id,
                roster: roster.clone(),
                delay: DelayBounds::default(),
            };
            Peer {
                id,
                s: Session::networked(cfg, Box::new(ep)),
                p: Pacer {
                    catch_up: CatchUp {
                        budget: None,
                        ..CatchUp::default()
                    },
                    ..Pacer::default()
                },
                sim: Fake {
                    player: Some(i as u8),
                    ..Fake::default()
                },
                // 60 Hz frames, peers out of phase.
                period: Duration::from_micros(16_667),
                next_frame: ms(i as u64 * 3),
                last_frame: Duration::ZERO,
                frames: Vec::new(),
                every: 5 + i as u32,
                issued_n: 0,
            }
        })
        .collect();
    Room { net, peers }
}

impl Room {
    /// Run `secs` of wall time in 1 ms steps; each peer runs a frame
    /// when its own frame clock says so. `each_ms` sees the room before
    /// every step.
    fn run(&mut self, secs: f64, mut each_ms: impl FnMut(&mut Room)) {
        let steps = (secs * 1000.0) as u64;
        for _ in 0..steps {
            self.net.advance(ms(1));
            each_ms(self);
            let now = self.net.now();
            for peer in &mut self.peers {
                if now < peer.next_frame {
                    continue;
                }
                if peer.every > 0
                    && peer.sim.tick > 0
                    && peer.sim.tick.is_multiple_of(peer.every)
                    && peer
                        .sim
                        .issued
                        .last()
                        .is_none_or(|(t, _)| *t != peer.sim.tick)
                    && peer.s.ended().is_none()
                {
                    peer.issued_n += 1;
                    let id = peer.sim.player.unwrap_or(0) as u32 * 1_000_000 + peer.issued_n;
                    peer.sim.issue(id);
                }
                let dt = now - peer.last_frame;
                peer.last_frame = now;
                peer.next_frame = now + peer.period;
                let rep = drive_frame(&mut peer.s, &mut peer.p, &mut peer.sim, dt, now, TICK);
                peer.frames.push(rep);
            }
        }
    }

    /// Every peer's trail agrees with the host's over the common prefix,
    /// and each ran at least `min_ticks`.
    fn assert_agree(&self, min_ticks: usize) {
        let host = &self.peers[0].sim.trail;
        for peer in &self.peers {
            let t = &peer.sim.trail;
            assert!(
                t.len() >= min_ticks,
                "{} ran only {} ticks (host {})",
                peer.id,
                t.len(),
                host.len()
            );
            let n = t.len().min(host.len());
            if let Some(i) = (0..n).find(|&i| t[i] != host[i]) {
                panic!("{} diverges from the host at tick {i}", peer.id);
            }
        }
    }

    /// Every command issued by a peer still present was applied, once,
    /// at the same tick everywhere; returns its latencies in ticks.
    fn latencies(&self, settle_ticks: u32) -> Vec<(PeerId, Vec<u32>)> {
        let applied = &self.peers[0].sim.applied;
        let horizon = self.peers.iter().map(|p| p.sim.tick).min().unwrap();
        for peer in &self.peers {
            for (id, t) in &peer.sim.applied {
                if *t < horizon {
                    assert_eq!(
                        applied.get(id),
                        Some(t),
                        "{} applied {id} elsewhere",
                        peer.id
                    );
                }
            }
        }
        self.peers
            .iter()
            .map(|peer| {
                let lat = peer
                    .sim
                    .issued
                    .iter()
                    .filter(|(t, _)| t + settle_ticks < horizon)
                    .map(|(t, id)| {
                        let at = applied
                            .get(id)
                            .unwrap_or_else(|| panic!("{}'s command {id} never applied", peer.id));
                        at - t
                    })
                    .collect();
                (peer.id, lat)
            })
            .collect()
    }
}

fn mean(v: &[u32]) -> f64 {
    v.iter().map(|&x| x as f64).sum::<f64>() / v.len().max(1) as f64
}

/// Single player: a solo host applies a command on the tick after it
/// was issued, as before lockstep.
#[test]
fn solo_applies_on_the_next_tick() {
    let mut s = Session::solo();
    let mut p = Pacer::default();
    let mut sim = Fake {
        player: Some(0),
        ..Fake::default()
    };
    let mut now = Duration::ZERO;
    for f in 0..200u32 {
        if f % 7 == 3 {
            sim.issue(f);
        }
        now += Duration::from_micros(16_667);
        let rep = drive_frame(
            &mut s,
            &mut p,
            &mut sim,
            Duration::from_micros(16_667),
            now,
            TICK,
        );
        assert!(rep.paced <= 1 && rep.extra == 0 && !rep.starved);
    }
    assert!(sim.tick >= 99, "30 ticks a second: {}", sim.tick);
    for (t, id) in &sim.issued {
        assert_eq!(sim.applied[id], *t, "sealed and applied on the next tick");
    }
    assert_eq!(s.delay(), 0);
}

/// 2 peers, from a LAN to a bad Wi-Fi link: identical trails, every
/// command applied once, latency = the delay.
#[test]
fn two_peers_agree_under_loss_and_jitter() {
    let cases = [
        LinkParams::default(),
        LinkParams::latency_ms(50),
        LinkParams {
            latency: ms(60),
            jitter: ms(30),
            loss: 0.05,
            duplicate: 0.02,
            reorder: 0.05,
            down: false,
        },
        LinkParams {
            latency: ms(100),
            jitter: ms(20),
            loss: 0.01,
            duplicate: 0.0,
            reorder: 0.02,
            down: false,
        },
    ];
    for (i, link) in cases.into_iter().enumerate() {
        let mut r = room(2, 100 + i as u64, link);
        r.run(20.0, |_| {});
        r.assert_agree(500);
        let lat = r.latencies(30);
        let s = &r.peers[1].s;
        eprintln!(
            "2 peers, one-way {:?} ±{:?} loss {}: delay {} (rtt {:.0} ms), latency host {:.1} guest {:.1} ticks, late {} fills {}+{}",
            link.latency,
            link.jitter,
            link.loss,
            r.peers[0].s.delay(),
            r.peers[0].s.rtt(PeerId(2)).unwrap_or(0.0),
            mean(&lat[0].1),
            mean(&lat[1].1),
            r.peers[0].s.stats.late_batches,
            s.stats.gap_fills,
            r.peers[0].s.stats.gap_fills,
        );
        // The host has no advantage: its commands wait the same delay.
        assert!(
            (mean(&lat[0].1) - mean(&lat[1].1)).abs() < 1.5,
            "host {:?} vs guest {:?}",
            mean(&lat[0].1),
            mean(&lat[1].1)
        );
    }
}

/// 8 peers, links from 0 to 200 ms with jitter, 1–5 % loss and
/// reordering: identical trails.
#[test]
fn eight_peers_agree_on_mixed_links() {
    let mut r = room(8, 8, LinkParams::default());
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
    r.run(20.0, |_| {});
    r.assert_agree(450);
    let lat = r.latencies(40);
    let host = &r.peers[0].s;
    eprintln!(
        "8 peers: delay {} late {} host fills {}",
        host.delay(),
        host.stats.late_batches,
        host.stats.gap_fills
    );
    for (peer, l) in &lat {
        eprintln!(
            "  {peer}: rtt {:>5.0} ms, latency {:.1} ticks over {} commands",
            host.rtt(*peer).unwrap_or(0.0),
            mean(l),
            l.len()
        );
    }
}

/// The adaptive delay against the round trip (`m1-runtime.md` §5).
#[test]
fn delay_follows_the_round_trip() {
    let mut last = 0;
    eprintln!("one-way   jitter  rtt(ms)  delay  latency(ticks)");
    for one_way in [0u64, 25, 50, 100, 150, 200] {
        let link = LinkParams {
            latency: ms(one_way),
            jitter: ms(one_way / 5),
            ..Default::default()
        };
        let mut r = room(2, one_way, link);
        r.run(12.0, |_| {});
        r.assert_agree(300);
        let d = r.peers[0].s.delay();
        let lat = r.latencies(30);
        let rtt = r.peers[0].s.rtt(PeerId(2)).unwrap_or(0.0);
        eprintln!(
            "{one_way:>5} ms {:>5} ms {rtt:>8.0} {d:>6} {:>8.1}",
            one_way / 5,
            mean(&lat[1].1)
        );
        assert!(d >= last, "the delay grows with the round trip");
        // Enough to cover it: the round trip in ticks, plus the guest's
        // backlog and a margin, and not much more.
        let rtt_ticks = (rtt / 33.3).ceil() as u32;
        assert!(
            d >= rtt_ticks && d <= rtt_ticks + 5,
            "rtt {rtt} ms, delay {d}"
        );
        last = d;
    }
}

/// A guest's link is dead for 3 s: it stalls (the others don't), then
/// gets the gap filled and catches up, capped per frame.
#[test]
fn a_dead_link_stalls_one_guest_then_it_catches_up() {
    let link = LinkParams {
        latency: ms(30),
        jitter: ms(5),
        ..Default::default()
    };
    let mut r = room(3, 5, link);
    let (host, slow) = (PeerId(1), PeerId(3));
    let mut t = 0;
    r.run(14.0, |r| {
        t += 1;
        if t == 4000 {
            r.net
                .set_link_both(host, slow, LinkParams { down: true, ..link });
        }
        if t == 7000 {
            r.net.set_link_both(host, slow, link);
        }
    });
    r.assert_agree(380);
    let g = &r.peers[2];
    let starved = g.frames.iter().filter(|f| f.starved).count();
    let extra: u32 = g.frames.iter().map(|f| f.extra).sum();
    let max_extra = g.frames.iter().map(|f| f.extra).max().unwrap();
    eprintln!(
        "dead link 3 s: {starved} starved frames, {extra} catch-up ticks (≤ {max_extra}/frame), {} gap requests, host delay {}",
        g.s.stats.gap_fills,
        r.peers[0].s.delay()
    );
    assert!(starved > 100, "it waited");
    assert!(extra > 60, "it caught up");
    assert!(max_extra <= CatchUp::default().max_extra);
    assert!(g.s.stats.gap_fills > 0, "asked for the gap");
    // The stall did not drag everyone's delay up.
    assert!(r.peers[0].s.delay() <= 6, "delay {}", r.peers[0].s.delay());
    // Back in step with the host at the end.
    assert!(r.peers[0].sim.tick - g.sim.tick <= 10);
    // The other guest never noticed.
    assert!(r.peers[1].frames.iter().filter(|f| f.starved).count() < 50);
    r.latencies(60);
}

/// One guest's state is perturbed: the host sees it at the next hash
/// frame, everyone stops at the same tick and dumps.
#[test]
fn a_desync_stops_everyone_at_the_same_tick() {
    let link = LinkParams {
        latency: ms(40),
        jitter: ms(10),
        loss: 0.02,
        ..Default::default()
    };
    let mut r = room(4, 6, link);
    r.peers[2].sim.perturb_after = Some(100);
    r.run(10.0, |_| {});
    let d = r.peers[0].s.desync().cloned().expect("the host noticed");
    assert_eq!(d.frame, 120, "the first hash frame after the change");
    assert_eq!(d.peers, vec![PeerId(3)]);
    for peer in &r.peers {
        assert_eq!(peer.s.desync().map(|x| x.stop_at), Some(d.stop_at));
        assert_eq!(peer.sim.tick, d.stop_at, "{} stopped there", peer.id);
        assert_eq!(peer.sim.dumps, 1, "{} dumped once", peer.id);
    }
    // Detected within one hash interval plus the report's trip.
    assert!(d.stop_at <= 120 + 10, "stopped after {}", d.stop_at);
}

/// A guest leaves: its commands stop, everyone else plays on. The host
/// leaves: the match is over for everyone.
#[test]
fn leaving_peers() {
    let mut r = room(3, 7, LinkParams::latency_ms(20));
    let mut t = 0;
    r.run(8.0, |r| {
        t += 1;
        if t == 3000 {
            r.net.leave(PeerId(3));
            r.peers[2].every = 0;
        }
    });
    let host_ticks = r.peers[0].sim.tick;
    assert!(host_ticks > 220, "{host_ticks}");
    assert!(r.peers[0].s.guests() == vec![PeerId(2)]);
    // The two left agree; the leaver stopped around tick 90.
    let gone = r.peers.pop().unwrap();
    assert!(gone.sim.tick < 100);
    r.assert_agree(200);

    r.net.leave(PeerId(1));
    r.run(1.0, |_| {});
    assert_eq!(r.peers[1].s.ended(), Some(Ended::HostLeft));
    let stopped = r.peers[1].sim.tick;
    r.run(1.0, |_| {});
    assert_eq!(r.peers[1].sim.tick, stopped, "no host, no clock");
}

/// Pause holds every clock; a command sequenced while paused is let
/// through at once and resumes.
#[test]
fn pause_and_resume() {
    let mut r = room(2, 9, LinkParams::latency_ms(40));
    for p in &mut r.peers {
        p.every = 0;
    }
    let mut t = 0;
    let pause = |on| SimCommand::Pause { on };
    let mut paused_at = 0;
    r.run(8.0, |r| {
        t += 1;
        if t == 2000 {
            r.peers[1].sim.staged.push(pause(true));
        }
        if t == 4000 {
            paused_at = r.peers[0].sim.tick;
            assert!(r.peers.iter().all(|p| p.sim.speed.paused));
        }
        if t == 5000 {
            assert_eq!(r.peers[0].sim.tick, paused_at, "paused");
            r.peers[1].sim.staged.push(pause(false));
        }
    });
    r.assert_agree(120);
    assert!(r.peers.iter().all(|p| !p.sim.speed.paused));
    assert!(r.peers[0].sim.tick > paused_at + 60, "running again");
}

/// Bytes per second per peer at 8 players, each issuing a command every
/// ~6 ticks (far above a human's APM), 50 ms links with 2 % loss.
#[test]
fn bandwidth_at_eight_players() {
    let link = LinkParams {
        latency: ms(50),
        jitter: ms(10),
        loss: 0.02,
        ..Default::default()
    };
    let mut r = room(8, 10, link);
    let secs = 20.0;
    r.run(secs, |_| {});
    r.assert_agree(500);
    // WebRTC (SCTP + DTLS + UDP + IP) costs roughly 90 bytes per message
    // on top of the payload (`x-commands.md` §3).
    const OVERHEAD: u64 = 90;
    let mut host_up = 0.0;
    for peer in &r.peers {
        let t = r.net.traffic(peer.id);
        let up: u64 = t.iter().map(|c| c.sent_bytes).sum();
        let up_msgs: u64 = t.iter().map(|c| c.sent_msgs).sum();
        let down: u64 = t.iter().map(|c| c.recv_bytes).sum();
        let down_msgs: u64 = t.iter().map(|c| c.recv_msgs).sum();
        eprintln!(
            "  {}: up {:>6.0} B/s payload ({:>5.1} msg/s, {:>6.0} B/s with overhead), down {:>6.0} B/s ({:>6.0} with overhead)",
            peer.id,
            up as f64 / secs,
            up_msgs as f64 / secs,
            (up + up_msgs * OVERHEAD) as f64 / secs,
            down as f64 / secs,
            (down + down_msgs * OVERHEAD) as f64 / secs,
        );
        if peer.s.is_host() {
            host_up = (up + up_msgs * OVERHEAD) as f64 / secs;
        }
    }
    assert!(
        host_up < 60_000.0,
        "the host's upload stays small: {host_up}"
    );
}

/// Every present peer applied `Dropped` for `player` exactly once, at
/// the same tick; returns it.
fn dropped_at(r: &Room, player: u8) -> u32 {
    let ticks: Vec<Vec<u32>> = r
        .peers
        .iter()
        .filter(|p| p.s.ended().is_none())
        .map(|p| {
            p.sim
                .dropped
                .iter()
                .filter(|(_, pl)| *pl == player)
                .map(|(t, _)| *t)
                .collect()
        })
        .collect();
    let first = &ticks[0];
    assert_eq!(first.len(), 1, "dropped once: {ticks:?}");
    assert!(ticks.iter().all(|t| t == first), "same tick: {ticks:?}");
    first[0]
}

/// M2 drops: a guest disconnects mid-match. The host puts a `Dropped`
/// in its name into the turn stream, after everything of its own, and
/// every peer applies it on the same tick.
#[test]
fn a_guest_that_leaves_is_dropped_on_one_tick() {
    let mut r = room(3, 21, LinkParams::latency_ms(30));
    let mut t = 0;
    r.run(6.0, |r| {
        t += 1;
        if t == 2400 {
            // The player list: the host relays every guest's ping.
            let list = r.peers[1].s.peers();
            let pings: Vec<Option<u32>> = list.iter().map(|p| p.ping_ms).collect();
            assert_eq!(pings[0], Some(0), "the host");
            for p in &pings[1..] {
                assert!(p.is_some_and(|ms| (50..=90).contains(&ms)), "{pings:?}");
            }
        }
        if t == 2500 {
            r.net.leave(PeerId(3));
            r.peers[2].every = 0;
        }
    });
    let gone = r.peers.pop().unwrap();
    assert!(gone.sim.dropped.is_empty());
    let at = dropped_at(&r, 2);
    eprintln!(
        "guest gone at host tick ~{}: dropped at tick {at} on every peer",
        gone.sim.tick
    );
    // Sequenced at the host's clock when it noticed (± the delay).
    assert!((70..=90).contains(&at), "dropped at tick {at}");
    r.assert_agree(150);
    assert!(r.peers[1].s.peers()[2].gone, "the guest knows it left");
}

/// A guest whose packets stop reaching the host is dropped after the
/// running timeout, and learns it when its link heals. A guest that
/// hears nothing from the host gives up on it (`HostLost`).
#[test]
fn silence_times_peers_out() {
    let link = LinkParams::latency_ms(25);
    let mut r = room(3, 23, link);
    let short = Timeouts {
        initial: ms(3000),
        running: ms(1500),
    };
    for p in &mut r.peers {
        p.s.set_timeouts(short);
    }
    let (host, mute, deaf) = (PeerId(1), PeerId(2), PeerId(3));
    let mut t = 0;
    r.run(8.0, |r| {
        t += 1;
        if t == 2000 {
            // Peer 2 hears the host, but the host never hears it; peer 3
            // can talk, but never hears the host.
            let down = LinkParams { down: true, ..link };
            r.net.set_link(mute, host, down);
            r.net.set_link(host, deaf, down);
            r.peers[1].every = 0;
        }
    });
    assert_eq!(r.peers[1].s.ended(), Some(Ended::Dropped), "told");
    assert_eq!(r.peers[2].s.ended(), Some(Ended::HostLost));
    let h = &r.peers[0];
    assert_eq!(h.sim.dropped.len(), 1, "only the mute one");
    assert_eq!(h.sim.dropped[0].1, 1);
    assert_eq!(h.s.guests(), vec![deaf], "the deaf one still sends");
    // Dropped ~1.5 s after its last packet (tick ~60 + 45).
    assert!(
        (95..=125).contains(&h.sim.dropped[0].0),
        "{:?}",
        h.sim.dropped
    );
}

/// The host's match rules: what they refuse is never sequenced (the
/// guest's batch still counts as received), and a guest can't send a
/// `Dropped` of its own.
#[test]
fn the_host_refuses_what_the_rules_forbid() {
    let mut r = room(2, 24, LinkParams::latency_ms(30));
    for p in &mut r.peers {
        p.every = 0;
    }
    r.peers[0].s.set_admit(Box::new(|player, cmd| match cmd {
        SimCommand::Pause { .. } if player != 0 => Err("pausing is off".into()),
        _ => Ok(()),
    }));
    let mut t = 0;
    r.run(6.0, |r| {
        t += 1;
        if t == 1000 {
            r.peers[1].sim.staged.push(SimCommand::Pause { on: true });
            r.peers[1].sim.staged.push(SimCommand::Dropped);
            r.peers[1].sim.issue(42);
        }
    });
    r.assert_agree(150);
    assert!(r.peers.iter().all(|p| !p.sim.speed.paused), "never paused");
    assert!(r.peers.iter().all(|p| p.sim.dropped.is_empty()));
    assert!(
        r.peers[0].sim.applied.contains_key(&42),
        "the rest went through"
    );
    assert_eq!(r.peers[0].s.stats.refused, 2);
}

/// "Leave match" on a guest: what it issued last (the resignation)
/// reaches the host before the transport closes; the host then drops
/// the seat after it.
#[test]
fn leaving_sends_the_resignation_first() {
    let mut r = room(3, 25, LinkParams::latency_ms(60));
    for p in &mut r.peers {
        p.every = 0;
    }
    let mut t = 0;
    let mut waited = Duration::ZERO;
    r.run(5.0, |r| {
        t += 1;
        if t == 2000 {
            r.peers[2].sim.issue(7);
        }
        if t >= 2000 && !r.peers[2].s.closed() {
            let now = r.net.now();
            let p = &mut r.peers[2];
            p.s.poll(now);
            super::driver::leave_step(&mut p.s, &mut p.sim, waited);
            p.s.flush(now);
            waited += ms(1);
        }
    });
    let leaver = &r.peers[2];
    assert_eq!(leaver.s.ended(), Some(Ended::Left));
    eprintln!("leave: closed {waited:?} after asking (60 ms one way)");
    // About a round trip (120 ms), well under the grace.
    assert!(waited < ms(400), "closed after {waited:?}");
    let applied = r.peers[0].sim.applied.get(&7).copied().expect("sent first");
    let at = dropped_at(&r, 2);
    assert!(
        at >= applied,
        "dropped at {at}, its last command at {applied}"
    );
}

//! Networked headless matches: one [`SimHarness`] per peer, each running
//! the real tick driver over a real [`Transport`], every human peer
//! playing its seat with the golden command script
//! ([`golden::script_for`]). The pieces the real-WebRTC tests
//! (`net::matchbox_tests`, natively, all peers in one process) and the
//! browser peer (`kp_net_peer`, `headless::web`) share, so a native and a
//! browser peer set up the very same match.
//!
//! Without a lobby, peers agree on the match from what all of them see:
//! the room's peer ids. Once a peer sees the expected number of others
//! ([`Rendezvous`]), the roster is every id in ascending order, seat `i`
//! for the `i`-th ([`roster`]); the lowest id is the host, as the
//! session elects it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::{Scenario, SimHarness, golden};
use crate::game_setup::PlayerSpec;
use crate::net::CommandBus;
use crate::net::driver::{CatchUp, NetSession, Pacer};
use crate::net::session::{DelayBounds, Session, SessionConfig};
use crate::net::transport::{PeerEvent, PeerId, Transport};
use crate::units::components::Faction;

/// `humans` human seats (owners 0..), then `ai` AI seats, on `map`;
/// every seat its own ally team, factions cycling.
pub fn scenario(map: &str, humans: u8, ai: u8, seed: u64) -> Scenario {
    let mut s = Scenario::ai_match(map, humans + ai, 1, seed);
    for (i, seat) in s.seats.iter_mut().enumerate() {
        *seat = PlayerSpec {
            faction: Faction::ALL[i % 3],
            ally_team: i as u8,
            ai: i as u8 >= humans,
        };
    }
    s
}

/// The session roster of a room: the ids in ascending order, seat `i`
/// for the `i`-th, spectators past the human seats.
pub fn roster(ids: &[PeerId], humans: u8) -> BTreeMap<PeerId, Option<u8>> {
    let sorted: BTreeSet<PeerId> = ids.iter().copied().collect();
    sorted
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p, (i < humans as usize).then_some(i as u8)))
        .collect()
}

/// Waiting for the room to fill: counts who is connected.
#[derive(Default)]
pub struct Rendezvous {
    present: BTreeSet<PeerId>,
}

impl Rendezvous {
    /// How many others this peer sees.
    pub fn seen(&self) -> usize {
        self.present.len()
    }

    /// Whether this peer sees `peer`.
    pub fn sees(&self, peer: PeerId) -> bool {
        self.present.contains(&peer)
    }

    /// Take in `t`'s peer events; once this peer has its id and sees
    /// `others` other peers, every id of the room (this one included).
    /// Only peer events are read: whatever arrives meanwhile stays
    /// queued for the session.
    pub fn poll(&mut self, t: &mut dyn Transport, others: usize) -> Option<Vec<PeerId>> {
        for ev in t.poll_peers() {
            match ev {
                PeerEvent::Joined(p) => self.present.insert(p),
                PeerEvent::Left(p) => self.present.remove(&p),
            };
        }
        let me = t.local_id()?;
        (self.present.len() >= others).then(|| {
            let mut all: Vec<PeerId> = self.present.iter().copied().collect();
            all.push(me);
            all
        })
    }
}

/// One peer of a networked headless match.
pub struct NetPeer {
    pub h: SimHarness,
    pub me: PeerId,
    /// The seat this peer plays (`None`: spectating).
    pub team: Option<u8>,
    humans: u8,
    /// The last bus tick the script ran for.
    scripted: u32,
}

impl NetPeer {
    /// Turn `h` (loaded, not started) into peer `me` of the match the
    /// room `ids` makes, over `transport`.
    pub fn join(
        mut h: SimHarness,
        me: PeerId,
        ids: &[PeerId],
        humans: u8,
        match_id: u64,
        transport: Box<dyn Transport>,
    ) -> Self {
        let roster = roster(ids, humans);
        let team = roster.get(&me).copied().flatten();
        let cfg = SessionConfig {
            match_id,
            me,
            roster,
            delay: DelayBounds::default(),
        };
        let mut ns = NetSession::new(Session::networked(cfg, transport));
        ns.pacer = Pacer {
            catch_up: CatchUp {
                budget: None,
                ..CatchUp::default()
            },
            // A fast machine whatever this one is: the host's speed
            // control (`net::speed`) leaves the game at full speed.
            cost: Some(Duration::from_millis(1)),
            ..Pacer::default()
        };
        let w = h.world_mut();
        w.insert_resource(ns);
        w.resource_mut::<super::HashLog>().enabled = true;
        Self {
            h,
            me,
            team,
            humans,
            scripted: 0,
        }
    }

    /// One frame `dt` long: the script's orders for the ticks reached so
    /// far, then the frame (the driver runs what is due).
    pub fn frame(&mut self, dt: Duration) -> u32 {
        let n = self.humans;
        let world = self.h.world_mut();
        let tick = world.resource::<CommandBus>().tick;
        if let Some(team) = self.team {
            for t in self.scripted + 1..=tick {
                let cmds = golden::script_for(world, t, &[team], |me| (me + 1) % n.max(2));
                let mut bus = world.resource_mut::<CommandBus>();
                for (_, list) in cmds {
                    for cmd in list {
                        bus.issue(cmd);
                    }
                }
            }
        }
        self.scripted = self.scripted.max(tick);
        self.h.frame_dt(dt)
    }

    /// The per-tick sim hashes so far.
    pub fn trail(&mut self) -> Vec<u64> {
        self.h
            .world_mut()
            .resource::<super::HashLog>()
            .ticks
            .iter()
            .map(|t| t.sim)
            .collect()
    }

    pub fn session<R>(&mut self, f: impl FnOnce(&Session) -> R) -> R {
        f(&self.h.world_mut().resource::<NetSession>().session)
    }
}

/// The trail as text, one `<tick> <hash>` line per tick (what the
/// browser peer hands back).
pub fn trail_text(trail: &[u64]) -> String {
    trail
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{} {h:016x}\n", i + 1))
        .collect()
}

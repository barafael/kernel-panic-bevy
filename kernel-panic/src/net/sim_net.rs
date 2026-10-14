//! An in-memory network for tests: every peer of a room in one process,
//! each link with its own latency, jitter, loss, duplication and
//! reordering, and links that can go dead and come back.
//!
//! **Deterministic.** Nothing reads the wall clock or an outside RNG: the
//! network has its own clock ([`SimNetwork::advance`]) and its own seeded
//! random stream, drawn in send order. Peers that send in the same order
//! (the multi-peer tests update their apps in a fixed order) see the
//! very same deliveries on every run.
//!
//! The two channels behave like matchbox's over WebRTC
//! (`net-experiments/x-transport.md`):
//!
//! - **unreliable**: every packet is delayed by `latency + U(0, jitter)`
//!   on its own, so jitter reorders; it may be lost, duplicated, or held
//!   back an extra latency (`reorder`); dropped while the link is down.
//! - **reliable**: in order and never lost. A "lost" packet arrives one
//!   retransmission timeout later ([`RELIABLE_RTO`], webrtc-rs's
//!   minimum), and holds up everything behind it (head-of-line blocking);
//!   while the link is down, messages wait and go out when it is back.
//!
//! All peers see each other (a full mesh, as the signaling server
//! introduces them); the session only uses the host links.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::transport::{Channel, PeerEvent, PeerId, Transport};

/// What a lost reliable message costs: SCTP's minimum retransmission
/// timeout in webrtc-rs (`RTO_MIN = 1000` ms, `x-transport.md` §4).
pub const RELIABLE_RTO: Duration = Duration::from_millis(1000);

/// One direction of a link.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkParams {
    /// One-way delay.
    pub latency: Duration,
    /// Extra delay, uniform in `0..=jitter`, per packet.
    pub jitter: Duration,
    /// Probability that an unreliable packet is lost (a reliable one is
    /// late by [`RELIABLE_RTO`] instead).
    pub loss: f64,
    /// Probability that an unreliable packet arrives twice.
    pub duplicate: f64,
    /// Probability that an unreliable packet is held back one extra
    /// `latency + jitter`, behind packets sent after it.
    pub reorder: f64,
    /// The link is dead: unreliable packets vanish, reliable ones wait.
    pub down: bool,
}

impl Default for LinkParams {
    fn default() -> Self {
        Self {
            latency: Duration::ZERO,
            jitter: Duration::ZERO,
            loss: 0.0,
            duplicate: 0.0,
            reorder: 0.0,
            down: false,
        }
    }
}

impl LinkParams {
    /// A link with this one-way latency and nothing else.
    pub fn latency_ms(ms: u64) -> Self {
        Self {
            latency: Duration::from_millis(ms),
            ..Self::default()
        }
    }
}

/// Bytes and messages one peer sent and received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Traffic {
    pub sent_bytes: u64,
    pub sent_msgs: u64,
    pub recv_bytes: u64,
    pub recv_msgs: u64,
}

struct Packet {
    from: PeerId,
    to: PeerId,
    channel: Channel,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Inner {
    now: Duration,
    rng: u64,
    /// Connected peers and their pending peer events.
    peers: BTreeMap<PeerId, Vec<PeerEvent>>,
    links: BTreeMap<(PeerId, PeerId), LinkParams>,
    default_link: LinkParams,
    /// In flight, by (delivery time, send order).
    flight: BTreeMap<(Duration, u64), Packet>,
    sent: u64,
    /// Reliable messages held while their link is down, per link.
    held: BTreeMap<(PeerId, PeerId), VecDeque<Vec<u8>>>,
    /// Delivery time of the last reliable message per link (in order).
    reliable_tail: BTreeMap<(PeerId, PeerId), Duration>,
    traffic: BTreeMap<PeerId, [Traffic; 2]>,
}

impl Inner {
    /// SplitMix64: the network's own stream.
    fn next_u64(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn chance(&mut self, p: f64) -> bool {
        p > 0.0 && (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }

    fn jitter(&mut self, link: &LinkParams) -> Duration {
        let j = link.jitter.as_micros() as u64;
        let extra = if j == 0 { 0 } else { self.next_u64() % (j + 1) };
        link.latency + Duration::from_micros(extra)
    }

    fn link(&self, from: PeerId, to: PeerId) -> LinkParams {
        self.links
            .get(&(from, to))
            .copied()
            .unwrap_or(self.default_link)
    }

    fn launch(&mut self, at: Duration, packet: Packet) {
        self.sent += 1;
        self.flight.insert((at, self.sent), packet);
    }

    fn send(&mut self, from: PeerId, to: PeerId, channel: Channel, bytes: Vec<u8>) {
        if !self.peers.contains_key(&to) || !self.peers.contains_key(&from) {
            return;
        }
        let t = &mut self.traffic.entry(from).or_default()[channel as usize];
        t.sent_bytes += bytes.len() as u64;
        t.sent_msgs += 1;
        let link = self.link(from, to);
        match channel {
            Channel::Unreliable => {
                if link.down || self.chance(link.loss) {
                    return;
                }
                let mut delay = self.jitter(&link);
                if self.chance(link.reorder) {
                    delay += self.jitter(&link);
                }
                let now = self.now;
                if self.chance(link.duplicate) {
                    let again = self.jitter(&link);
                    self.launch(
                        now + again,
                        Packet {
                            from,
                            to,
                            channel,
                            bytes: bytes.clone(),
                        },
                    );
                }
                self.launch(
                    now + delay,
                    Packet {
                        from,
                        to,
                        channel,
                        bytes,
                    },
                );
            }
            Channel::Reliable => {
                if link.down {
                    self.held.entry((from, to)).or_default().push_back(bytes);
                    return;
                }
                self.send_reliable(from, to, link, bytes);
            }
        }
    }

    fn send_reliable(&mut self, from: PeerId, to: PeerId, link: LinkParams, bytes: Vec<u8>) {
        let mut at = self.now + self.jitter(&link);
        if self.chance(link.loss) {
            at += RELIABLE_RTO;
        }
        let tail = self.reliable_tail.entry((from, to)).or_default();
        at = at.max(*tail);
        *tail = at;
        self.launch(
            at,
            Packet {
                from,
                to,
                channel: Channel::Reliable,
                bytes,
            },
        );
    }

    /// Send what waited for links that are up again.
    fn release_held(&mut self) {
        let ready: Vec<(PeerId, PeerId)> = self
            .held
            .keys()
            .copied()
            .filter(|&(a, b)| !self.link(a, b).down)
            .collect();
        for (from, to) in ready {
            let link = self.link(from, to);
            for bytes in self.held.remove(&(from, to)).unwrap_or_default() {
                self.send_reliable(from, to, link, bytes);
            }
        }
    }
}

/// A simulated room. Cheap to clone (a shared handle).
#[derive(Clone, Default)]
pub struct SimNetwork(Arc<Mutex<Inner>>);

impl SimNetwork {
    pub fn new(seed: u64) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            rng: seed,
            ..Default::default()
        })))
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().expect("sim network poisoned")
    }

    /// Connect a new peer: everyone present learns about it and it
    /// learns about everyone.
    pub fn join(&self, id: PeerId) -> SimEndpoint {
        let mut net = self.inner();
        let present: Vec<PeerId> = net.peers.keys().copied().collect();
        for events in net.peers.values_mut() {
            events.push(PeerEvent::Joined(id));
        }
        net.peers
            .insert(id, present.into_iter().map(PeerEvent::Joined).collect());
        SimEndpoint {
            id,
            net: self.clone(),
        }
    }

    /// Disconnect `id`: the others see it leave, its traffic in flight
    /// is gone.
    pub fn leave(&self, id: PeerId) {
        let mut net = self.inner();
        if net.peers.remove(&id).is_none() {
            return;
        }
        for events in net.peers.values_mut() {
            events.push(PeerEvent::Left(id));
        }
        net.flight.retain(|_, p| p.from != id && p.to != id);
    }

    /// The parameters of every link without its own.
    pub fn set_default_link(&self, link: LinkParams) {
        self.inner().default_link = link;
    }

    /// One direction of one link.
    pub fn set_link(&self, from: PeerId, to: PeerId, link: LinkParams) {
        let mut net = self.inner();
        net.links.insert((from, to), link);
        net.release_held();
    }

    /// Both directions between `a` and `b`.
    pub fn set_link_both(&self, a: PeerId, b: PeerId, link: LinkParams) {
        self.set_link(a, b, link);
        self.set_link(b, a, link);
    }

    /// Advance the network clock.
    pub fn advance(&self, dt: Duration) {
        let mut net = self.inner();
        net.now += dt;
        net.release_held();
    }

    pub fn now(&self) -> Duration {
        self.inner().now
    }

    /// What `peer` sent and received so far, per channel
    /// (`[reliable, unreliable]`).
    pub fn traffic(&self, peer: PeerId) -> [Traffic; 2] {
        self.inner().traffic.get(&peer).copied().unwrap_or_default()
    }
}

/// One peer's connection to a [`SimNetwork`].
pub struct SimEndpoint {
    id: PeerId,
    net: SimNetwork,
}

impl Transport for SimEndpoint {
    fn local_id(&self) -> Option<PeerId> {
        Some(self.id)
    }

    fn poll_peers(&mut self) -> Vec<PeerEvent> {
        self.net
            .inner()
            .peers
            .get_mut(&self.id)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    fn send(&mut self, peer: PeerId, channel: Channel, bytes: Vec<u8>) {
        debug_assert!(!bytes.is_empty() && bytes.len() <= super::transport::MAX_MESSAGE);
        self.net.inner().send(self.id, peer, channel, bytes);
    }

    fn receive(&mut self, channel: Channel) -> Vec<(PeerId, Vec<u8>)> {
        let mut net = self.net.inner();
        if !net.peers.contains_key(&self.id) {
            return Vec::new();
        }
        let now = net.now;
        let due: Vec<(Duration, u64)> = net
            .flight
            .range(..=(now, u64::MAX))
            .filter(|(_, p)| p.to == self.id && p.channel == channel)
            .map(|(k, _)| *k)
            .collect();
        let mut out = Vec::with_capacity(due.len());
        for key in due {
            if let Some(p) = net.flight.remove(&key) {
                let t = &mut net.traffic.entry(self.id).or_default()[channel as usize];
                t.recv_bytes += p.bytes.len() as u64;
                t.recv_msgs += 1;
                out.push((p.from, p.bytes));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn latency_holds_messages_until_due() {
        let net = SimNetwork::new(1);
        let mut a = net.join(PeerId(1));
        let mut b = net.join(PeerId(2));
        assert_eq!(a.poll_peers(), vec![PeerEvent::Joined(PeerId(2))]);
        assert_eq!(b.poll_peers(), vec![PeerEvent::Joined(PeerId(1))]);
        net.set_default_link(LinkParams::latency_ms(50));
        a.send(PeerId(2), Channel::Unreliable, vec![7]);
        net.advance(ms(49));
        assert!(b.receive(Channel::Unreliable).is_empty());
        net.advance(ms(1));
        assert_eq!(b.receive(Channel::Unreliable), vec![(PeerId(1), vec![7])]);
    }

    #[test]
    fn reliable_stays_in_order_and_waits_out_a_dead_link() {
        let net = SimNetwork::new(2);
        let mut a = net.join(PeerId(1));
        let mut b = net.join(PeerId(2));
        net.set_default_link(LinkParams {
            latency: ms(10),
            jitter: ms(30),
            loss: 0.3,
            ..Default::default()
        });
        for i in 0..50u8 {
            a.send(PeerId(2), Channel::Reliable, vec![i]);
        }
        net.set_link(
            PeerId(1),
            PeerId(2),
            LinkParams {
                down: true,
                ..LinkParams::latency_ms(10)
            },
        );
        for i in 50..60u8 {
            a.send(PeerId(2), Channel::Reliable, vec![i]);
            a.send(PeerId(2), Channel::Unreliable, vec![i]);
        }
        net.advance(Duration::from_secs(30));
        assert!(b.receive(Channel::Unreliable).is_empty(), "dead link");
        let got: Vec<u8> = b
            .receive(Channel::Reliable)
            .into_iter()
            .map(|(_, m)| m[0])
            .collect();
        assert_eq!(got, (0..50).collect::<Vec<_>>(), "in order, held back");
        net.set_link(PeerId(1), PeerId(2), LinkParams::latency_ms(10));
        net.advance(ms(10));
        let got: Vec<u8> = b
            .receive(Channel::Reliable)
            .into_iter()
            .map(|(_, m)| m[0])
            .collect();
        assert_eq!(got, (50..60).collect::<Vec<_>>(), "released on recovery");
    }

    #[test]
    fn same_seed_same_deliveries() {
        let run = |seed| {
            let net = SimNetwork::new(seed);
            let mut a = net.join(PeerId(1));
            let mut b = net.join(PeerId(2));
            net.set_default_link(LinkParams {
                latency: ms(40),
                jitter: ms(20),
                loss: 0.05,
                duplicate: 0.02,
                reorder: 0.05,
                down: false,
            });
            let mut log = Vec::new();
            for i in 0..500u32 {
                a.send(PeerId(2), Channel::Unreliable, i.to_le_bytes().to_vec());
                net.advance(ms(16));
                for (_, m) in b.receive(Channel::Unreliable) {
                    log.push(u32::from_le_bytes(m.try_into().unwrap()));
                }
            }
            log
        };
        let (x, y) = (run(9), run(9));
        assert_eq!(x, y);
        assert_ne!(x, run(10));
        // The conditions show: something lost, something reordered.
        assert!(x.len() < 500 + 20 && x.len() > 400, "{}", x.len());
        assert!(x.windows(2).any(|w| w[1] < w[0]), "jitter reorders");
    }

    #[test]
    fn leaving_is_seen_by_the_others() {
        let net = SimNetwork::new(3);
        let mut a = net.join(PeerId(1));
        let _b = net.join(PeerId(2));
        a.poll_peers();
        net.leave(PeerId(2));
        assert_eq!(a.poll_peers(), vec![PeerEvent::Left(PeerId(2))]);
        a.send(PeerId(2), Channel::Reliable, vec![1]);
        assert_eq!(net.traffic(PeerId(1))[0].sent_msgs, 0, "nobody to send to");
    }
}

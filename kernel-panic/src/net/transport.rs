//! What the lockstep session needs from a network: peers that come and
//! go, and two byte channels to each of them.
//!
//! - **Reliable** (ordered, retransmitted): rare control traffic — state
//!   hashes, desync announcements, turn gap fills. Under loss it stalls
//!   for a retransmission timeout (webrtc-rs SCTP: 1 s minimum), so
//!   nothing the tick rate waits on travels here.
//! - **Unreliable** (unordered, may lose): the turn stream and
//!   the guests' command batches, each packet repeating what the other
//!   side has not acknowledged yet (`net-experiments/x-transport.md` §4).
//!
//! The session never sends a message over [`MAX_MESSAGE`] bytes: WebRTC
//! data channels kill the connection at exactly 64 KiB and silently drop
//! anything larger, and 16 KiB is the cross-browser safe size (§5).
//!
//! Two implementations: [`super::sim_net`] (in-memory, simulated latency
//! and loss, for tests) and [`super::matchbox`] (WebRTC), whose API this mirrors —
//! `try_update_peers` for the peer events, `channel_mut(i).try_send` /
//! `receive`. Sending is best effort and never blocks: a send to a peer
//! that is gone, or that the network drops, is not an error the caller
//! sees (matchbox's `try_send` returns `Ok` for those too).

use serde::{Deserialize, Serialize};

/// A peer's id in the room. matchbox's ids are UUIDs (128 bit); the
/// host is the lowest id present (cake's election), so the ordering
/// matters and must be the same on every peer: plain integer order.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct PeerId(pub u128);

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The last 4 hex digits, as a short readable handle.
        write!(f, "peer-{:04x}", self.0 & 0xffff)
    }
}

/// The two channels every peer link has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Channel {
    Reliable,
    Unreliable,
}

/// A change in who is connected. (Made by the transports: the simulated
/// network in tests, matchbox.)
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerEvent {
    Joined(PeerId),
    Left(PeerId),
}

/// The largest message the session sends (`x-transport.md` §5).
pub const MAX_MESSAGE: usize = 16 * 1024;

/// A connection to the room.
pub trait Transport: Send + Sync + 'static {
    /// This peer's id (`None` until the signaling server assigned one).
    #[allow(dead_code)]
    fn local_id(&self) -> Option<PeerId>;

    /// Peers that connected or disconnected since the last call
    /// (matchbox: `try_update_peers`, never the panicking
    /// `update_peers`).
    fn poll_peers(&mut self) -> Vec<PeerEvent>;

    /// Queue `bytes` for `peer`. Best effort: never blocks, never fails
    /// visibly. `bytes` is never empty (a WebRTC data channel may drop a
    /// zero-length payload) and at most [`MAX_MESSAGE`] long.
    fn send(&mut self, peer: PeerId, channel: Channel, bytes: Vec<u8>);

    /// Everything that arrived on `channel` since the last call.
    fn receive(&mut self, channel: Channel) -> Vec<(PeerId, Vec<u8>)>;

    /// The connection to the room is gone for good, and why (matchbox:
    /// the signaling server could not be reached, or the socket closed).
    /// Its peers were reported [`PeerEvent::Left`].
    fn failure(&self) -> Option<String> {
        None
    }
}

/// A room nobody else can reach: an id of its own, no peers, every send
/// dropped. The menu screenshots (`ui::menu_shots`) open their rooms
/// here, so a dev tool never shows up in a real room on the public
/// signaling server.
pub struct LoneTransport {
    id: PeerId,
}

impl LoneTransport {
    pub fn new(id: PeerId) -> Self {
        Self { id }
    }
}

impl Transport for LoneTransport {
    fn local_id(&self) -> Option<PeerId> {
        Some(self.id)
    }

    fn poll_peers(&mut self) -> Vec<PeerEvent> {
        Vec::new()
    }

    fn send(&mut self, _peer: PeerId, _channel: Channel, _bytes: Vec<u8>) {}

    fn receive(&mut self, _channel: Channel) -> Vec<(PeerId, Vec<u8>)> {
        Vec::new()
    }
}

//! The real network: a [`Transport`] over WebRTC data channels, through
//! barafael's matchbox fork (`matchbox_socket`; the fork and revision
//! cake and omdurman use). Native peers run webrtc-rs, the web build
//! the browser's own WebRTC; both meet in the same rooms, so native and
//! web players share a match.
//!
//! - **Rooms.** A room is a [`RoomId`] (a short code players type or
//!   share as `#room=<code>`); peers find each other through the
//!   signaling server at `<signaling>/kp-<room>` ([`NetConfig`]), namespaced
//!   like cake's `cake-<room>`. Only introductions cross the server
//!   (SDP offers and answers, ICE candidates); game data goes peer to
//!   peer. The server introduces everyone to everyone (full mesh); the
//!   session only uses the host's links.
//! - **Entry point:** [`open_room`]. It never blocks: the socket connects
//!   in the background (native: a thread of its own; web: the event
//!   loop), and peers show up as [`PeerEvent::Joined`] when their data
//!   channels are open.
//! - **Ids.** matchbox's peer ids are UUIDs, assigned by the signaling
//!   server; [`PeerId`] is the same 128 bits as an integer, so the
//!   lowest-id host election is the same on every peer.
//! - **Channels.** matchbox channel 0 is [`Channel::Reliable`] (ordered,
//!   retransmitted), channel 1 [`Channel::Unreliable`] (unordered, at
//!   most [`UNRELIABLE_RETRANSMITS`] sends) — `net-experiments/x-transport.md`
//!   §4: both share one SCTP association and its congestion window. Native
//!   peers need the patched `webrtc-data` (`vendor/webrtc-data`) for the
//!   channel to be unordered at all (`m2-transport.md` §7).
//! - **Size.** Nothing over [`MAX_MESSAGE`] (16 KiB) is ever handed to
//!   matchbox (asserted): a message of exactly 64 KiB kills the whole
//!   peer connection and a larger one vanishes while `try_send` says
//!   `Ok` (x-transport §5).
//! - **Flow control.** matchbox queues without limit and exposes no
//!   `bufferedAmount`, so a sender that outruns the link builds a
//!   backlog that starves everything after it for seconds (x-transport
//!   §6, "the backlog matters"). Every peer link here has a token bucket
//!   ([`SEND_RATE`], [`SEND_BURST`]) and bounded queues in front of it:
//!   unreliable packets beyond [`UNRELIABLE_QUEUE`] drop the oldest (the
//!   session repeats unacknowledged content in every packet, so the
//!   newest supersedes), reliable ones beyond [`RELIABLE_QUEUE_BYTES`]
//!   are refused and counted (gap fills are asked for again).
//! - **Peer events** come from `try_update_peers` only (`update_peers`
//!   panics once the socket's loop has ended). A peer matchbox reports
//!   disconnected is not gone at once: the fork reports a peer whose
//!   *signaling* websocket dropped as disconnected, while its data
//!   channels (all a match needs) live on. Such a link leaves once it has
//!   been silent for [`LEAVE_GRACE`] (the session sends to every peer it
//!   talks to at least every 100 ms), however long it kept talking first:
//!   matchbox reports a disconnect only once.
//! - **Closing.** Dropping the transport closes the socket and ends its
//!   loop. A socket whose loop ended (signaling server unreachable after
//!   [`SIGNALING_ATTEMPTS`] tries, or every peer and the server gone)
//!   reports its peers as left and says why in [`Transport::failure`].
//!
//! Findings and measurements: `net-experiments/m2-transport.md`.

// Until the lobby (M2) opens rooms, only the tests call into this.
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Poll, Waker};
use std::time::Duration;

use crate::sim::DetFloat;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use matchbox_socket::{
    ChannelConfig, MessageLoopFuture, PeerState, RtcIceServerConfig, WebRtcSocket,
    WebRtcSocketBuilder,
};

use super::transport::{Channel, MAX_MESSAGE, PeerEvent, PeerId, Transport};

/// The signaling server: omdurman's shared `matchbox_server` on fly.io
/// (cake uses it too), unless the build sets `MATCHBOX_SERVER`. It stops
/// while idle, so the first join after a quiet spell takes a few
/// seconds.
pub const SIGNALING_SERVER: &str = match option_env!("MATCHBOX_SERVER") {
    Some(s) => s,
    None => "wss://omdurman-matchbox.fly.dev",
};

/// ICE servers baked in at build time (`KP_ICE`, [`IceServer::parse_list`]
/// syntax), e.g. a TURN relay for the web build. Unset: Google's public
/// STUN servers, as cake and omdurman.
const BUILD_ICE: Option<&str> = option_env!("KP_ICE");

/// Bytes per second each peer link may hand to matchbox. Lockstep needs
/// ~1 KB/s per guest (`m1-runtime.md` §4); this is the order of what one
/// WebRTC association sustains at 0.5 % loss and 40 ms RTT (~500 KB/s,
/// x-transport §6), so a burst of gap fills drains without building a
/// multi-second backlog inside SCTP.
pub const SEND_RATE: f64 = 256.0 * 1024.0;
/// What a link may send at once after a quiet spell.
pub const SEND_BURST: f64 = 64.0 * 1024.0;
/// Reliable bytes a link may hold back before it refuses more.
pub const RELIABLE_QUEUE_BYTES: usize = 1024 * 1024;
/// Unreliable packets a link may hold back; older ones are dropped.
pub const UNRELIABLE_QUEUE: usize = 8;
/// How long a link matchbox calls disconnected may stay silent before
/// it counts as left (x-transport §7: an app-level timeout of 3–5 s,
/// where the fork's own ICE failure takes ~30 s).
pub const LEAVE_GRACE: Duration = Duration::from_secs(3);
/// Tries to reach the signaling server, 3 s apart (fly.io wakes the
/// stopped server in a few seconds).
pub const SIGNALING_ATTEMPTS: u16 = 5;

/// `max_retransmits` of the unreliable channel. Not 0: webrtc-rs counts
/// the first send against the limit (`nsent >= max_retransmits`), so with
/// 0 every chunk is abandoned the moment it is sent, and the next SACK's
/// FORWARD-TSN tells the receiver to skip chunks still in flight; on a path
/// that reorders, each one it overtakes is dropped on arrival (9–17 % of
/// all packets at 0.5–2 % loss with 5–10 ms jitter, `m2-transport.md` §7).
/// With 2, webrtc-rs sends a chunk at most twice (a browser three times):
/// a lost packet comes again ~a round trip later, and since the channel is
/// unordered it holds nothing up; one lost twice is given up.
pub const UNRELIABLE_RETRANSMITS: u16 = 2;

const RELIABLE: usize = 0;
const UNRELIABLE: usize = 1;

// --- room codes ---------------------------------------------------------

/// A room's code: what players type, paste or share (`#room=<code>` on
/// the web, `--room <code>` natively). 1 to [`RoomId::MAX_LEN`]
/// characters of `a-z`, `0-9`, `-` and `_`; letters are taken in either
/// case and kept lower case, so a code read over voice chat works
/// whichever way it is typed. It goes into the signaling URL's path
/// unescaped, so nothing else is allowed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomId(String);

/// Why a room code was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomIdError {
    Empty,
    TooLong {
        len: usize,
    },
    /// The offending character, so the message can name it.
    BadChar(char),
}

impl fmt::Display for RoomIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "the room code is empty"),
            Self::TooLong { len } => write!(
                f,
                "a room code has at most {} characters, this one {len}",
                RoomId::MAX_LEN
            ),
            Self::BadChar(c) => write!(
                f,
                "'{c}' cannot be part of a room code (letters, digits, '-' and '_')"
            ),
        }
    }
}

impl std::error::Error for RoomIdError {}

impl RoomId {
    pub const MAX_LEN: usize = 24;
    /// The letters [`RoomId::random`] draws from: no `0 o 1 l i`, which
    /// read alike.
    const ALPHABET: &'static [u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    /// Length of a [`RoomId::random`] code: 31^6 ≈ 9 × 10^8 codes.
    const RANDOM_LEN: usize = 6;

    /// Validate `code` (surrounding whitespace ignored).
    pub fn parse(code: &str) -> Result<Self, RoomIdError> {
        let code = code.trim();
        let len = code.chars().count();
        if len == 0 {
            return Err(RoomIdError::Empty);
        }
        if len > Self::MAX_LEN {
            return Err(RoomIdError::TooLong { len });
        }
        if let Some(c) = code
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        {
            return Err(RoomIdError::BadChar(c));
        }
        Ok(Self(code.to_ascii_lowercase()))
    }

    /// A fresh code for a new room (launch entropy, `rng::clock_f64`).
    pub fn random() -> Self {
        let n = Self::ALPHABET.len() as f64;
        Self(
            (0..Self::RANDOM_LEN)
                .map(|_| {
                    let i = (crate::rng::clock_f64() * n) as usize;
                    Self::ALPHABET[i.min(Self::ALPHABET.len() - 1)] as char
                })
                .collect(),
        )
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for RoomId {
    type Err = RoomIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

// --- configuration -------------------------------------------------------

/// One ICE server (`RTCIceServer`): STUN tells a peer its public address
/// so two peers behind NATs can punch through; TURN relays everything
/// when that fails (symmetric NAT, CGNAT, UDP-blocking networks).
/// Credentials are for TURN; STUN ignores them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IceServer {
    pub urls: Vec<String>,
    pub username: Option<String>,
    pub credential: Option<String>,
}

impl IceServer {
    pub fn stun(url: &str) -> Self {
        Self {
            urls: vec![url.to_string()],
            username: None,
            credential: None,
        }
    }

    /// `KP_ICE` / `--ice` syntax: servers separated by whitespace, each
    /// `url[,url…][|username|credential]`, e.g.
    /// `stun:stun.l.google.com:19302 turn:turn.example.org:3478|kp|secret`.
    /// `none` is no server at all (host candidates only: LAN, tests).
    pub fn parse_list(spec: &str) -> Result<Vec<Self>, String> {
        if spec.trim() == "none" {
            return Ok(Vec::new());
        }
        spec.split_whitespace()
            .map(|entry| {
                let mut parts = entry.split('|');
                let urls: Vec<String> = parts
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter(|u| !u.is_empty())
                    .map(str::to_string)
                    .collect();
                let username = parts.next().map(str::to_string);
                let credential = parts.next().map(str::to_string);
                if urls.is_empty() || parts.next().is_some() {
                    return Err(format!("bad ICE server entry {entry:?}"));
                }
                if let Some(u) = urls.iter().find(|u| {
                    !["stun:", "stuns:", "turn:", "turns:"]
                        .iter()
                        .any(|s| u.starts_with(s))
                }) {
                    return Err(format!("{u:?} is not a stun: or turn: URL"));
                }
                if urls.iter().any(|u| u.starts_with("turn")) && credential.is_none() {
                    return Err(format!("TURN entry {entry:?} needs |username|credential"));
                }
                Ok(Self {
                    urls,
                    username,
                    credential,
                })
            })
            .collect()
    }
}

/// Where peers meet and how they get through NATs. [`Default`]: the
/// build's settings ([`SIGNALING_SERVER`], `KP_ICE` or Google STUN);
/// [`NetConfig::from_launch`] adds the native overrides.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct NetConfig {
    /// `ws://` or `wss://` base URL of a matchbox signaling server.
    pub signaling: String,
    /// STUN and TURN servers. Empty: host candidates only.
    pub ice: Vec<IceServer>,
}

impl Default for NetConfig {
    fn default() -> Self {
        let ice = BUILD_ICE
            .map(|spec| IceServer::parse_list(spec).expect("KP_ICE (build time)"))
            .unwrap_or_else(|| {
                vec![IceServer {
                    urls: vec![
                        "stun:stun.l.google.com:19302".into(),
                        "stun:stun1.l.google.com:19302".into(),
                    ],
                    username: None,
                    credential: None,
                }]
            });
        Self {
            signaling: SIGNALING_SERVER.to_string(),
            ice,
        }
    }
}

impl NetConfig {
    /// The build's settings, overridden natively by `--signaling <url>`
    /// / `KP_SIGNALING` and `--ice <servers>` / `KP_ICE` (arguments win).
    /// A malformed `--ice` is reported and ignored.
    pub fn from_launch() -> Self {
        #[allow(unused_mut)]
        let mut cfg = Self::default();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let args: Vec<String> = std::env::args().collect();
            let arg = |name: &str| {
                args.iter()
                    .position(|a| a == name)
                    .and_then(|i| args.get(i + 1).cloned())
            };
            let pick = |name: &str, var: &str| arg(name).or_else(|| std::env::var(var).ok());
            if let Some(url) = pick("--signaling", "KP_SIGNALING") {
                cfg.signaling = url;
            }
            if let Some(spec) = pick("--ice", "KP_ICE") {
                match IceServer::parse_list(&spec) {
                    Ok(ice) => cfg.ice = ice,
                    Err(e) => error!("ignoring the ICE servers: {e}"),
                }
            }
        }
        cfg
    }

    /// The signaling URL of `room`: `<signaling>/kp-<room>`.
    pub fn room_url(&self, room: &RoomId) -> String {
        format!("{}/kp-{room}", self.signaling.trim_end_matches('/'))
    }

    /// matchbox takes one `RTCIceServer`: every entry's URLs go into it,
    /// with the credentials of the first entry that has some (STUN URLs
    /// ignore them; browsers and webrtc-rs require them only for TURN).
    /// A second TURN entry with other credentials cannot be expressed
    /// and is left out, with a warning.
    fn matchbox_ice(&self) -> RtcIceServerConfig {
        let mut out = RtcIceServerConfig {
            urls: Vec::new(),
            username: None,
            credential: None,
        };
        for s in &self.ice {
            let has_creds = s.username.is_some() || s.credential.is_some();
            if has_creds {
                if out.username.is_none() && out.credential.is_none() {
                    out.username = s.username.clone();
                    out.credential = s.credential.clone();
                } else if (&out.username, &out.credential) != (&s.username, &s.credential) {
                    warn!(
                        "one set of TURN credentials per socket (matchbox); {:?} left out",
                        s.urls
                    );
                    continue;
                }
            }
            out.urls.extend(s.urls.iter().cloned());
        }
        out
    }
}

/// Open `room`: a [`MatchboxTransport`] to every peer in it.
pub fn open_room(room: &RoomId, cfg: &NetConfig) -> Box<dyn Transport> {
    Box::new(MatchboxTransport::open(&cfg.room_url(room), cfg))
}

// --- the transport --------------------------------------------------------

/// Traffic through one transport (all peers), for tests and the
/// write-up. Per channel: `[reliable, unreliable]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransportStats {
    pub sent_bytes: [u64; 2],
    pub sent_msgs: [u64; 2],
    pub recv_bytes: [u64; 2],
    pub recv_msgs: [u64; 2],
    /// Dropped here: unreliable superseded in a full queue or sent to a
    /// peer not connected; reliable refused by a full queue or sent to a
    /// peer that left.
    pub dropped: [u64; 2],
    /// Sends that had to wait for the token bucket.
    pub throttled: u64,
    /// The most bytes one link ever held back.
    pub peak_queued: usize,
}

/// The socket's background loop, shared with the transport.
#[derive(Default)]
struct LoopState {
    /// The transport is gone: end the loop.
    stop: bool,
    waker: Option<Waker>,
    /// The loop ended, and how.
    ended: Option<Result<(), String>>,
}

type SharedLoop = Arc<Mutex<LoopState>>;

fn lock(state: &SharedLoop) -> std::sync::MutexGuard<'_, LoopState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run matchbox's message loop until it ends or the transport stops it.
fn run_loop(fut: MessageLoopFuture, state: SharedLoop) -> impl Future<Output = ()> {
    use bevy::tasks::futures_lite::future;
    let stopper = state.clone();
    let stop = future::poll_fn(move |cx| {
        let mut s = lock(&stopper);
        if s.stop {
            Poll::Ready(Ok(()))
        } else {
            s.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    });
    async move {
        let result = future::or(fut, stop).await;
        if let Err(e) = &result {
            warn!("matchbox socket ended: {e}");
        }
        lock(&state).ended = Some(result.map_err(|e| e.to_string()));
    }
}

/// One peer link's send side.
struct Link {
    /// matchbox reports its data channels open.
    connected: bool,
    /// matchbox reported it disconnected: it leaves once it has been
    /// silent for [`LEAVE_GRACE`] (now or later).
    suspect: bool,
    last_heard: Instant,
    reliable: VecDeque<Box<[u8]>>,
    reliable_bytes: usize,
    unreliable: VecDeque<Box<[u8]>>,
    tokens: f64,
    refilled: Instant,
    /// A full reliable queue was reported.
    warned: bool,
}

impl Link {
    fn new(now: Instant) -> Self {
        Self {
            connected: false,
            suspect: false,
            last_heard: now,
            reliable: VecDeque::new(),
            reliable_bytes: 0,
            unreliable: VecDeque::new(),
            tokens: SEND_BURST,
            refilled: now,
            warned: false,
        }
    }

    fn queued(&self) -> usize {
        self.reliable_bytes + self.unreliable.iter().map(|p| p.len()).sum::<usize>()
    }
}

/// A [`Transport`] over one matchbox WebRTC socket (see the module doc).
pub struct MatchboxTransport {
    /// Behind a lock only for [`Transport::local_id`]'s `&self` (matchbox
    /// reads the id through `&mut`); everything else uses `get_mut`.
    socket: Mutex<WebRtcSocket>,
    links: BTreeMap<PeerId, Link>,
    /// Peers that left: nothing more is queued for them.
    gone: BTreeSet<PeerId>,
    state: SharedLoop,
    /// The loop ended and every link was reported left.
    closed: bool,
    stats: Arc<Mutex<TransportStats>>,
}

fn to_matchbox(peer: PeerId) -> matchbox_socket::PeerId {
    matchbox_socket::PeerId(uuid::Uuid::from_u128(peer.0))
}

fn from_matchbox(peer: matchbox_socket::PeerId) -> PeerId {
    PeerId(peer.0.as_u128())
}

fn index(channel: Channel) -> usize {
    match channel {
        Channel::Reliable => RELIABLE,
        Channel::Unreliable => UNRELIABLE,
    }
}

impl MatchboxTransport {
    /// Connect to `room_url` (a full signaling URL; [`open_room`] builds
    /// it from a [`RoomId`]) with `cfg`'s ICE servers.
    pub fn open(room_url: &str, cfg: &NetConfig) -> Self {
        info!("opening room {room_url}");
        let mut builder = WebRtcSocketBuilder::new(room_url)
            .reconnect_attempts(Some(SIGNALING_ATTEMPTS))
            .add_channel(ChannelConfig::reliable())
            .add_channel(ChannelConfig {
                ordered: false,
                max_retransmits: Some(UNRELIABLE_RETRANSMITS),
            });
        // No servers: host candidates only. A browser refuses an ICE
        // server without URLs, so the web build keeps matchbox's default
        // (Google STUN) instead; host candidates still come first.
        if !cfg.ice.is_empty() || cfg!(not(target_arch = "wasm32")) {
            builder = builder.ice_server(cfg.matchbox_ice());
        }
        let (socket, fut) = builder.build();
        let state = SharedLoop::default();
        let task = run_loop(fut, state.clone());
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::Builder::new()
            .name("kp-matchbox".into())
            .spawn(move || bevy::tasks::block_on(task))
            .expect("cannot start the network thread");
        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(task);
        Self {
            socket: Mutex::new(socket),
            links: BTreeMap::new(),
            gone: BTreeSet::new(),
            state,
            closed: false,
            stats: Arc::default(),
        }
    }

    /// The traffic counters, shared (they keep counting while the
    /// session owns the transport).
    pub fn stats(&self) -> Arc<Mutex<TransportStats>> {
        self.stats.clone()
    }

    fn socket(&mut self) -> &mut WebRtcSocket {
        self.socket
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn count(&self, f: impl FnOnce(&mut TransportStats)) {
        f(&mut self.stats.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// Hand `peer`'s queued packets to matchbox as its tokens allow.
    fn pump(&mut self, peer: PeerId, now: Instant) {
        let Some(link) = self.links.get_mut(&peer) else {
            return;
        };
        if !link.connected {
            return;
        }
        let dt = now.saturating_duration_since(link.refilled).as_secs_f64();
        link.refilled = now;
        link.tokens = (link.tokens + dt * SEND_RATE).det_min(SEND_BURST);
        let mut out: Vec<(usize, Box<[u8]>)> = Vec::new();
        while link.tokens > 0.0 {
            let next = match link.reliable.pop_front() {
                Some(p) => {
                    link.reliable_bytes -= p.len();
                    (RELIABLE, p)
                }
                None => match link.unreliable.pop_front() {
                    Some(p) => (UNRELIABLE, p),
                    None => break,
                },
            };
            link.tokens -= next.1.len() as f64;
            out.push(next);
        }
        let held = link.queued();
        let to = to_matchbox(peer);
        let mut failed = false;
        for (ch, packet) in out {
            let len = packet.len() as u64;
            if self.socket().channel_mut(ch).try_send(packet, to).is_err() {
                failed = true;
                break;
            }
            self.count(|s| {
                s.sent_bytes[ch] += len;
                s.sent_msgs[ch] += 1;
            });
        }
        if held > 0 {
            self.count(|s| {
                s.throttled += 1;
                s.peak_queued = s.peak_queued.max(held);
            });
        }
        if failed {
            // The socket's loop is gone; `poll_peers` reports it.
            self.links.values_mut().for_each(|l| l.connected = false);
        }
    }

    fn close(&mut self, out: &mut Vec<PeerEvent>) {
        if self.closed {
            return;
        }
        self.closed = true;
        for (&peer, link) in &self.links {
            if link.connected || link.suspect {
                out.push(PeerEvent::Left(peer));
            }
        }
        self.gone.extend(self.links.keys().copied());
        self.links.clear();
        if let Some(Err(e)) = &lock(&self.state).ended {
            error!("the room connection is gone: {e}");
        }
    }
}

impl Transport for MatchboxTransport {
    fn local_id(&self) -> Option<PeerId> {
        self.socket
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .id()
            .map(from_matchbox)
    }

    fn poll_peers(&mut self) -> Vec<PeerEvent> {
        let mut out = Vec::new();
        if self.closed {
            return out;
        }
        let now = Instant::now();
        match self.socket().try_update_peers() {
            Ok(changes) => {
                for (id, state) in changes {
                    let peer = from_matchbox(id);
                    match state {
                        PeerState::Connected => {
                            self.gone.remove(&peer);
                            let link = self.links.entry(peer).or_insert_with(|| Link::new(now));
                            let known = link.connected || link.suspect;
                            link.connected = true;
                            link.suspect = false;
                            link.last_heard = now;
                            if !known {
                                out.push(PeerEvent::Joined(peer));
                            }
                        }
                        PeerState::Disconnected => {
                            if let Some(link) = self.links.get_mut(&peer) {
                                link.suspect = true;
                            }
                        }
                    }
                }
            }
            Err(_) => self.close(&mut out),
        }
        // Disconnected and silent for the grace period: gone.
        let left: Vec<PeerId> = self
            .links
            .iter()
            .filter(|(_, l)| {
                l.suspect && now.saturating_duration_since(l.last_heard) >= LEAVE_GRACE
            })
            .map(|(p, _)| *p)
            .collect();
        for peer in left {
            self.links.remove(&peer);
            self.gone.insert(peer);
            out.push(PeerEvent::Left(peer));
        }
        let peers: Vec<PeerId> = self.links.keys().copied().collect();
        for peer in peers {
            self.pump(peer, now);
        }
        out
    }

    fn send(&mut self, peer: PeerId, channel: Channel, bytes: Vec<u8>) {
        assert!(
            !bytes.is_empty() && bytes.len() <= MAX_MESSAGE,
            "a {} byte message: the transport carries 1..={MAX_MESSAGE}",
            bytes.len()
        );
        let ch = index(channel);
        if self.closed || self.gone.contains(&peer) {
            self.count(|s| s.dropped[ch] += 1);
            return;
        }
        let now = Instant::now();
        let link = self.links.entry(peer).or_insert_with(|| Link::new(now));
        let mut dropped = 0;
        match channel {
            Channel::Unreliable => {
                if link.connected {
                    link.unreliable.push_back(bytes.into_boxed_slice());
                    while link.unreliable.len() > UNRELIABLE_QUEUE {
                        link.unreliable.pop_front();
                        dropped += 1;
                    }
                } else {
                    dropped += 1;
                }
            }
            Channel::Reliable => {
                // Held until the peer connects, like a reliable message
                // waiting out a dead link.
                if link.reliable_bytes + bytes.len() <= RELIABLE_QUEUE_BYTES {
                    link.reliable_bytes += bytes.len();
                    link.reliable.push_back(bytes.into_boxed_slice());
                } else {
                    if !link.warned {
                        link.warned = true;
                        warn!("{peer}: reliable send queue full, dropping");
                    }
                    dropped += 1;
                }
            }
        }
        if dropped > 0 {
            self.count(|s| s.dropped[ch] += dropped);
        }
        self.pump(peer, now);
    }

    fn receive(&mut self, channel: Channel) -> Vec<(PeerId, Vec<u8>)> {
        if self.closed {
            return Vec::new();
        }
        let ch = index(channel);
        let now = Instant::now();
        let got: Vec<(PeerId, Vec<u8>)> = self
            .socket()
            .channel_mut(ch)
            .receive()
            .into_iter()
            .map(|(id, packet)| (from_matchbox(id), packet.into_vec()))
            .collect();
        let bytes: u64 = got.iter().map(|(_, b)| b.len() as u64).sum();
        self.count(|s| {
            s.recv_bytes[ch] += bytes;
            s.recv_msgs[ch] += got.len() as u64;
        });
        for (peer, _) in &got {
            if let Some(link) = self.links.get_mut(peer) {
                // A suspect link that talks is alive (its signaling went
                // away, its data channels did not), until it falls silent:
                // matchbox reports a peer disconnected only once.
                link.last_heard = now;
            }
        }
        got
    }

    fn failure(&self) -> Option<String> {
        match &lock(&self.state).ended {
            Some(Err(e)) => Some(e.clone()),
            Some(Ok(())) if self.closed => Some("the room connection closed".into()),
            _ => None,
        }
    }
}

impl Drop for MatchboxTransport {
    fn drop(&mut self) {
        self.socket().close();
        let waker = {
            let mut s = lock(&self.state);
            s.stop = true;
            s.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_codes() {
        assert_eq!(RoomId::parse(" AbC-12_x ").unwrap().as_str(), "abc-12_x");
        assert_eq!("q7".parse::<RoomId>().unwrap().to_string(), "q7");
        assert_eq!(RoomId::parse(""), Err(RoomIdError::Empty));
        assert_eq!(RoomId::parse("a/b"), Err(RoomIdError::BadChar('/')));
        assert_eq!(RoomId::parse("café"), Err(RoomIdError::BadChar('é')));
        assert_eq!(
            RoomId::parse(&"a".repeat(25)),
            Err(RoomIdError::TooLong { len: 25 })
        );
        let r = RoomId::random();
        assert_eq!(r.as_str().len(), 6);
        assert_eq!(RoomId::parse(r.as_str()), Ok(r.clone()));
        assert_ne!(RoomId::random(), r, "fresh codes differ");
        let cfg = NetConfig {
            signaling: "wss://example.org/".into(),
            ice: Vec::new(),
        };
        assert_eq!(cfg.room_url(&r), format!("wss://example.org/kp-{r}"));
    }

    #[test]
    fn ice_servers() {
        let list = IceServer::parse_list(
            "stun:a.org:3478,stun:b.org turn:t.org:3478?transport=udp|kp|secret",
        )
        .unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].urls, ["stun:a.org:3478", "stun:b.org"]);
        assert_eq!(list[1].username.as_deref(), Some("kp"));
        assert_eq!(IceServer::parse_list("none").unwrap(), Vec::new());
        assert!(
            IceServer::parse_list("turn:t.org").is_err(),
            "TURN needs credentials"
        );
        assert!(IceServer::parse_list("http://x").is_err());
        assert!(IceServer::parse_list("stun:x|a|b|c").is_err());
        // One matchbox server: all URLs, the TURN credentials; a second
        // credential set is left out.
        let mut cfg = NetConfig {
            signaling: String::new(),
            ice: list,
        };
        cfg.ice.push(IceServer {
            urls: vec!["turn:other.org".into()],
            username: Some("x".into()),
            credential: Some("y".into()),
        });
        let m = cfg.matchbox_ice();
        assert_eq!(m.urls.len(), 3);
        assert_eq!(m.username.as_deref(), Some("kp"));
        assert_eq!(m.credential.as_deref(), Some("secret"));
        assert_eq!(NetConfig::default().signaling, SIGNALING_SERVER);
        assert!(!NetConfig::default().ice.is_empty(), "STUN by default");
        // No `--signaling` / `--ice` in a test run.
        if std::env::var_os("KP_SIGNALING").is_none() && std::env::var_os("KP_ICE").is_none() {
            assert_eq!(NetConfig::from_launch(), NetConfig::default());
        }
        assert_eq!(
            IceServer::parse_list("stun:s.org").unwrap(),
            vec![IceServer::stun("stun:s.org")]
        );
    }
}

//! The lockstep session over real WebRTC: [`MatchboxTransport`]s on
//! localhost, introduced by a signaling server in this process
//! ([`Signaling`]), every peer a full headless game
//! ([`netplay::NetPeer`]) playing its seat with the golden script; every
//! peer's per-tick state hash trail must equal the host's. Unlike the
//! `sim_net` tests ([`super::multipeer_tests`]) these run in wall-clock
//! time: the network runs on its own threads.
//!
//! In CI: the transport basics and the 2- and 8-peer matches. The
//! `#[ignore]`d ones need a set-up machine:
//!
//! - `netem_match`: under emulated loss / latency / jitter,
//!   `scripts/netem-matchbox.sh "<netem args>"` (`unshare -rn` locally,
//!   `--sudo` for CI's `tc` on `lo`); size from `KP_NET_PEERS`,
//!   `KP_NET_AI`, `KP_NET_SECS`, `KP_NET_MAP`.
//! - `browser_peer`: one peer is the wasm build in a headless browser
//!   (`.github/scripts/kp-netpeer.mjs`, `KP_NETPEER_PKG=<pkg dir>`,
//!   `KP_NETPEER_RUNTIME`, `KP_NETPEER_SECS`), the other this process.
//!
//! [`MatchboxTransport`]: super::matchbox::MatchboxTransport

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::ws::Message;
use bevy::tasks::futures_lite::StreamExt;
use matchbox_protocol::{JsonPeerEvent, PeerRequest};
use matchbox_signaling::common_logic::{SignalingChannel, parse_request, try_send};
use matchbox_signaling::{
    ClientRequestError, NoCallbacks, SignalingServerBuilder, SignalingState, SignalingTopology,
    WsStateMeta,
};

use super::driver::NetStatus;
use super::matchbox::{
    LEAVE_GRACE, MatchboxTransport, NetConfig, RELIABLE_QUEUE_BYTES, RoomId, SEND_RATE,
    TransportStats,
};
use super::multipeer_tests::SERIAL;
use super::transport::{Channel, MAX_MESSAGE, PeerEvent, PeerId, Transport};
use crate::headless::SimHarness;
use crate::headless::netplay::{self, NetPeer, Rendezvous};

// --- the signaling server ----------------------------------------------------

/// Rooms by URL path, each a full mesh: who joins is introduced to
/// everyone already there. `matchbox_server`'s semantics (the deployed
/// server); `matchbox_signaling`'s own full-mesh topology reads the room
/// and adds the newcomer under separate locks, so two peers joining at
/// the same moment may never meet (`x-transport.md` §2). Here one lock
/// covers both.
#[derive(Clone, Debug, Default)]
struct Rooms(Arc<Mutex<BTreeMap<String, BTreeMap<matchbox_protocol::PeerId, SignalingChannel>>>>);

impl SignalingState for Rooms {}

struct RoomMesh;

fn text(event: JsonPeerEvent) -> Message {
    Message::Text(event.to_string().into())
}

#[matchbox_socket::async_trait::async_trait]
impl SignalingTopology<NoCallbacks, Rooms> for RoomMesh {
    async fn state_machine(upgrade: WsStateMeta<NoCallbacks, Rooms>) {
        let WsStateMeta {
            room,
            peer_id,
            sender,
            mut receiver,
            state,
            ..
        } = upgrade;
        {
            let mut rooms = state.0.lock().unwrap();
            let peers = rooms.entry(room.clone()).or_default();
            for ch in peers.values() {
                let _ = try_send(ch, text(JsonPeerEvent::NewPeer(peer_id)));
            }
            peers.insert(peer_id, sender);
        }
        while let Some(request) = receiver.next().await {
            match parse_request(request) {
                Ok(PeerRequest::Signal { receiver, data }) => {
                    let to = state
                        .0
                        .lock()
                        .unwrap()
                        .get(&room)
                        .and_then(|r| r.get(&receiver).cloned());
                    if let Some(ch) = to {
                        let event = JsonPeerEvent::Signal {
                            sender: peer_id,
                            data,
                        };
                        let _ = try_send(&ch, text(event));
                    }
                }
                Ok(PeerRequest::KeepAlive) => {}
                Err(ClientRequestError::Json(_) | ClientRequestError::UnsupportedType(_)) => {}
                Err(_) => break,
            }
        }
        let mut rooms = state.0.lock().unwrap();
        if let Some(peers) = rooms.get_mut(&room) {
            peers.remove(&peer_id);
            for ch in peers.values() {
                let _ = try_send(ch, text(JsonPeerEvent::PeerLeft(peer_id)));
            }
            if peers.is_empty() {
                rooms.remove(&room);
            }
        }
    }
}

/// A signaling server on a free localhost port, for as long as it lives.
pub(super) struct Signaling {
    rt: Option<tokio::runtime::Runtime>,
    rooms: Rooms,
    /// `ws://127.0.0.1:<port>`.
    pub url: String,
}

impl Signaling {
    pub fn start() -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let rooms = Rooms::default();
        let mut server =
            SignalingServerBuilder::new((Ipv4Addr::LOCALHOST, 0), RoomMesh, rooms.clone()).build();
        let addr = server.bind().expect("bind the signaling server");
        rt.spawn(server.serve());
        Self {
            rt: Some(rt),
            rooms,
            url: format!("ws://{addr}"),
        }
    }

    /// Tell everyone else that `peer` left, without it leaving: what the
    /// others hear when its signaling websocket drops (a server restart
    /// does not, it just goes quiet).
    pub fn announce_left(&self, peer: PeerId) {
        let id = matchbox_protocol::PeerId(uuid::Uuid::from_u128(peer.0));
        for peers in self.rooms.0.lock().unwrap().values() {
            for (_, ch) in peers.iter().filter(|(p, _)| **p != id) {
                let _ = try_send(ch, text(JsonPeerEvent::PeerLeft(id)));
            }
        }
    }

    /// Shut the server down (its connections drop).
    pub fn stop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }

    /// How many peers are in a room (signaling, not WebRTC: connected to
    /// the server, whether or not anyone has a data channel to them).
    pub fn peers_in_rooms(&self) -> usize {
        self.rooms.0.lock().unwrap().values().map(|r| r.len()).sum()
    }

    /// Host candidates only: nothing leaves the machine.
    pub fn config(&self) -> NetConfig {
        NetConfig {
            signaling: self.url.clone(),
            ice: Vec::new(),
        }
    }
}

/// A STUN server on a free localhost UDP port that answers every binding
/// request with the sender's address (RFC 5389 `XOR-MAPPED-ADDRESS`),
/// for as long as it lives. For the browser peers: a browser always
/// gets an ICE server from matchbox (Google STUN when the list is empty,
/// [`super::matchbox::MatchboxTransport::open`]), and matchbox waits for
/// ICE gathering to complete before it answers an offer. Where Google's
/// STUN does not answer, gathering ends only when its requests time out
/// (~10 s for Chromium with the packets dropped; ~40 s on GitHub's
/// runners, where every passing run took 39.9 s to meet), and the whole
/// handshake waits for that. This one answers at once, and the test no
/// longer needs the internet.
pub(super) struct Stun {
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// `stun:127.0.0.1:<port>`.
    pub url: String,
}

impl Stun {
    pub fn start() -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind STUN");
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        let url = format!("stun:{}", socket.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            while !stopped.load(Ordering::Relaxed) {
                let Ok((n, from)) = socket.recv_from(&mut buf) else {
                    continue;
                };
                if let Some(reply) = stun_binding_response(&buf[..n], from) {
                    let _ = socket.send_to(&reply, from);
                }
            }
        });
        Self { stop, url }
    }
}

impl Drop for Stun {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The success response to a STUN binding request from `from`, or
/// `None` for anything else.
fn stun_binding_response(req: &[u8], from: std::net::SocketAddr) -> Option<Vec<u8>> {
    const COOKIE: [u8; 4] = 0x2112_a442u32.to_be_bytes();
    if req.len() < 20 || req[..2] != [0x00, 0x01] || req[4..8] != COOKIE {
        return None;
    }
    let txid = &req[8..20];
    let port = from.port() ^ 0x2112;
    let (family, addr): (u8, Vec<u8>) = match from.ip() {
        std::net::IpAddr::V4(ip) => (1, ip.octets().to_vec()),
        std::net::IpAddr::V6(ip) => (2, ip.octets().to_vec()),
    };
    // XOR with the cookie, then (IPv6) the transaction id.
    let key: Vec<u8> = COOKIE.iter().chain(txid).copied().collect();
    let addr: Vec<u8> = addr.iter().zip(&key).map(|(a, k)| a ^ k).collect();
    let mut attr = vec![0x00, 0x20, 0, (4 + addr.len()) as u8, 0, family];
    attr.extend_from_slice(&port.to_be_bytes());
    attr.extend_from_slice(&addr);
    let mut out = vec![0x01, 0x01];
    out.extend_from_slice(&(attr.len() as u16).to_be_bytes());
    out.extend_from_slice(&COOKIE);
    out.extend_from_slice(txid);
    out.extend_from_slice(&attr);
    Some(out)
}

/// The browser peer's ICE servers: `stun`, unless `KP_NETPEER_ICE` says
/// otherwise (empty: matchbox's default, Google STUN).
fn browser_ice(stun: &Stun) -> String {
    std::env::var("KP_NETPEER_ICE").unwrap_or_else(|_| stun.url.clone())
}

/// Join `room` on `server` from this process and wait (at most `limit`)
/// until `others` peers have open data channels to it; the transport and
/// every id of the room. `child` is the browser runner: its early end
/// fails the test at once.
///
/// matchbox never gives up on a handshake (no timeout, no retry), so a
/// pair that once failed to connect would wait forever. Once everyone is
/// in the room (the server's view), attempt `n` gives the handshake
/// `n` × [`MEET_RETRY`] (growing, so a slow but working handshake gets
/// through); then this side leaves the room and joins it again under a
/// new id, which starts a fresh handshake with every other peer.
fn meet_browser(
    server: &Signaling,
    room: &RoomId,
    others: usize,
    child: &mut std::process::Child,
    limit: Duration,
) -> (MatchboxTransport, Vec<PeerId>) {
    let cfg = server.config();
    let t0 = Instant::now();
    for attempt in 1u32.. {
        let mut t = MatchboxTransport::open(&cfg.room_url(room), &cfg);
        let mut rv = Rendezvous::default();
        let mut all_in: Option<Instant> = None;
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                panic!("the browser runner ended early: {status}");
            }
            if let Some(ids) = rv.poll(&mut t, others) {
                eprintln!(
                    "browser peer connected after {:?} (attempt {attempt})",
                    t0.elapsed()
                );
                return (t, ids);
            }
            assert!(
                t0.elapsed() < limit,
                "the browser peer: not connected within {limit:?} ({attempt} attempts)"
            );
            if server.peers_in_rooms() > others {
                let since = *all_in.get_or_insert_with(Instant::now);
                let patience = MEET_RETRY * attempt;
                if since.elapsed() > patience {
                    eprintln!(
                        "attempt {attempt}: everyone in the room for {patience:?}, \
                         no data channel: rejoining"
                    );
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    unreachable!()
}

/// How long [`meet_browser`]'s first attempt waits for a handshake once
/// everyone is in the room. With the local [`Stun`] it takes well under
/// a second.
const MEET_RETRY: Duration = Duration::from_secs(10);

fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) -> Duration {
    let t0 = Instant::now();
    while !done() {
        assert!(t0.elapsed() < limit, "{what}: not within {limit:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    t0.elapsed()
}

// --- the transport alone -------------------------------------------------------

/// Two transports in a room: they meet, both channels carry messages up
/// to the 16 KiB cap, the reliable channel in order; a burst beyond the
/// send rate waits in the queue (and beyond the queue is refused); a
/// peer that goes away is reported left.
#[test]
fn transport_basics() {
    let server = Signaling::start();
    let cfg = server.config();
    let room = RoomId::random();
    let mut a = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    std::thread::sleep(Duration::from_millis(50));
    let mut b = super::matchbox::open_room(&room, &cfg);
    let (mut a_sees, mut b_sees) = (Vec::new(), Vec::new());
    let setup = wait_for("connect", Duration::from_secs(20), || {
        a_sees.extend(a.poll_peers());
        b_sees.extend(b.poll_peers());
        !a_sees.is_empty() && !b_sees.is_empty()
    });
    let (ida, idb) = (a.local_id().unwrap(), b.local_id().unwrap());
    assert_eq!(a_sees, vec![PeerEvent::Joined(idb)]);
    assert_eq!(b_sees, vec![PeerEvent::Joined(ida)]);
    eprintln!("2 transports connected in {setup:?}");

    // Both channels, every size class up to the cap.
    for (i, len) in [1usize, 100, 1200, 4096, MAX_MESSAGE]
        .into_iter()
        .enumerate()
    {
        a.send(idb, Channel::Reliable, vec![i as u8; len]);
        b.send(ida, Channel::Unreliable, vec![i as u8; len]);
    }
    let (mut at_b, mut at_a) = (Vec::new(), Vec::new());
    wait_for("delivery", Duration::from_secs(5), || {
        a.poll_peers();
        b.poll_peers();
        at_b.extend(b.receive(Channel::Reliable));
        at_a.extend(a.receive(Channel::Unreliable));
        at_b.len() == 5 && at_a.len() == 5
    });
    let lens: Vec<usize> = at_b.iter().map(|(_, m)| m.len()).collect();
    assert_eq!(
        lens,
        [1, 100, 1200, 4096, MAX_MESSAGE],
        "reliable: in order"
    );
    assert!(at_b.iter().all(|(p, _)| *p == ida));
    assert!(
        at_a.iter()
            .all(|(p, m)| *p == idb && m.len() <= MAX_MESSAGE)
    );

    // A reliable burst of 1.5 MiB in 16 KiB messages: the queue takes
    // 1 MiB, refuses the rest, and drains at the send rate.
    let stats = a.stats();
    let before = *stats.lock().unwrap();
    let burst = 96;
    for i in 0..burst {
        let mut m = vec![0u8; MAX_MESSAGE];
        m[..4].copy_from_slice(&(i as u32).to_le_bytes());
        a.send(idb, Channel::Reliable, m);
    }
    let after_send: TransportStats = *stats.lock().unwrap();
    let refused = after_send.dropped[0] - before.dropped[0];
    let accepted = burst - refused as usize;
    assert!(refused > 0, "beyond the queue");
    assert!(accepted * MAX_MESSAGE >= RELIABLE_QUEUE_BYTES / 2);
    let mut got = Vec::new();
    let t0 = Instant::now();
    let drain = wait_for("drain", Duration::from_secs(20), || {
        a.poll_peers();
        b.poll_peers();
        got.extend(
            b.receive(Channel::Reliable)
                .into_iter()
                .map(|(_, m)| u32::from_le_bytes(m[..4].try_into().unwrap())),
        );
        got.len() == accepted
    });
    assert_eq!(got, (0..accepted as u32).collect::<Vec<_>>(), "in order");
    let rate = (accepted * MAX_MESSAGE) as f64 / t0.elapsed().as_secs_f64();
    let s = *stats.lock().unwrap();
    eprintln!(
        "burst: {accepted} of {burst} accepted, drained in {drain:?} ({:.0} KiB/s, cap {:.0}); peak queue {} KiB",
        rate / 1024.0,
        SEND_RATE / 1024.0,
        s.peak_queued / 1024
    );
    assert!(rate < SEND_RATE * 1.5, "paced: {rate:.0} B/s");
    assert!(s.peak_queued <= RELIABLE_QUEUE_BYTES);

    // B goes away: A sees it leave within the grace period (plus the
    // signaling server's notice).
    drop(b);
    let mut left = false;
    let gone = wait_for("leave", LEAVE_GRACE + Duration::from_secs(5), || {
        left |= a.poll_peers().contains(&PeerEvent::Left(idb));
        left
    });
    eprintln!("left noticed after {gone:?}");
    assert!(a.failure().is_none(), "A itself is fine");
}

/// The signaling server says a peer left while its data channels live
/// on (its websocket dropped): the link stays as long as traffic flows,
/// and leaves once it falls silent. The server going away altogether
/// changes nothing for connected peers (the fork keeps the socket).
#[test]
fn signaling_loss_keeps_links() {
    let mut server = Signaling::start();
    let cfg = server.config();
    let room = RoomId::random();
    let mut a = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    std::thread::sleep(Duration::from_millis(50));
    let mut b = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    let (mut na, mut nb) = (0, 0);
    wait_for("connect", Duration::from_secs(20), || {
        na += a.poll_peers().len();
        nb += b.poll_peers().len();
        na == 1 && nb == 1
    });
    let (ida, idb) = (a.local_id().unwrap(), b.local_id().unwrap());
    // The server stops; then B is announced gone (a stand-in for its
    // websocket dropping) while it keeps talking.
    server.announce_left(idb);
    std::thread::sleep(Duration::from_millis(100));
    server.stop();
    let mut events = Vec::new();
    let (mut sent, mut heard) = (0, 0);
    let t0 = Instant::now();
    while t0.elapsed() < LEAVE_GRACE * 2 {
        b.send(ida, Channel::Unreliable, vec![1]);
        sent += 1;
        events.extend(a.poll_peers());
        heard += a.receive(Channel::Unreliable).len();
        b.poll_peers();
        std::thread::sleep(Duration::from_millis(50));
    }
    let quiet_since = Instant::now();
    assert_eq!(events, Vec::new(), "B kept talking: still in");
    // Against what was sent: a busy runner's 50 ms sleeps stretch (CI
    // macOS managed 47 sends' worth in the 6 s).
    assert!(heard * 2 > sent, "{heard} of {sent} messages");
    a.send(idb, Channel::Reliable, vec![2]);
    wait_for("reliable after the server", Duration::from_secs(2), || {
        b.poll_peers();
        !b.receive(Channel::Reliable).is_empty()
    });
    // B falls silent: gone after the grace period.
    wait_for("leave", LEAVE_GRACE * 2, || {
        a.poll_peers().contains(&PeerEvent::Left(idb))
    });
    let silent = quiet_since.elapsed();
    eprintln!("silent link left after {silent:?}");
    assert!(silent >= LEAVE_GRACE - Duration::from_millis(200));
}

/// No signaling server at the address: the transport gives up and says
/// why.
#[test]
fn unreachable_signaling_fails() {
    // A port nobody listens on.
    let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = NetConfig {
        signaling: format!("ws://127.0.0.1:{port}"),
        ice: Vec::new(),
    };
    let mut t = MatchboxTransport::open(&cfg.room_url(&RoomId::random()), &cfg);
    // `SIGNALING_ATTEMPTS` tries, 3 s apart.
    wait_for("give up", Duration::from_secs(30), || {
        t.poll_peers();
        t.failure().is_some()
    });
    eprintln!("failure: {}", t.failure().unwrap());
    assert!(t.local_id().is_none());
}

// --- whole matches -------------------------------------------------------------

struct RealRoom {
    _server: Signaling,
    peers: Vec<NetPeer>,
    stats: BTreeMap<PeerId, Arc<Mutex<TransportStats>>>,
    /// Wall time the match ran.
    played: Duration,
    lag: LagStats,
}

/// How the lockstep pace went, for comparing delay rules.
#[derive(Default)]
struct LagStats {
    /// The host's input delay, summed per round, and the most.
    delay_sum: u64,
    rounds: u64,
    delay_max: u32,
    /// Per guest: starved since (wall time).
    starved_since: BTreeMap<PeerId, Instant>,
    /// Guest stalls of 100 ms or more after the first second, and the
    /// longest of them.
    stalls: u32,
    /// When the long ones ended (s into the match) and how long (ms).
    stall_log: Vec<(f32, u32)>,
    start: Option<Instant>,
    longest: Duration,
    /// Extra (catch-up) ticks, all guests.
    extra: u64,
}

/// Load `humans` full games of the scenario, then open their transports
/// in one room (signaling at `server`), wait until everyone sees
/// everyone, and start the sessions.
fn real_match(map: &str, humans: u8, ai: u8, seed: u64) -> RealRoom {
    let scn = netplay::scenario(map, humans, ai, seed);
    // Maps first: loading takes seconds a connected peer would spend
    // silent.
    let hs: Vec<SimHarness> = (0..humans).map(|_| SimHarness::new(scn.clone())).collect();
    let server = Signaling::start();
    let cfg = server.config();
    let url = cfg.room_url(&RoomId::random());
    let n = humans as usize;
    let mut ts: Vec<MatchboxTransport> = (0..n)
        .map(|_| MatchboxTransport::open(&url, &cfg))
        .collect();
    // Only the host's links carry the match (the lowest id sequences):
    // the room is ready once the host sees every guest and every guest
    // sees the host. A guest-guest pair of the full mesh may never connect
    // under loss (about 3 rooms in 20 at 2-3 %, m2-transport.md); the
    // session ignores those links. The lobby hands out the roster in the
    // game; here every transport is in this process, so read the ids.
    let mut rv: Vec<Rendezvous> = (0..n).map(|_| Rendezvous::default()).collect();
    let t0 = Instant::now();
    let all: Vec<PeerId> = loop {
        for i in 0..n {
            rv[i].poll(&mut ts[i], usize::MAX);
        }
        let ids: Option<Vec<PeerId>> = ts.iter().map(|t| t.local_id()).collect();
        if let Some(mut all) = ids {
            all.sort();
            let host = all[0];
            let h = ts.iter().position(|t| t.local_id() == Some(host)).unwrap();
            let ready = (0..n).all(|i| {
                let me = ts[i].local_id().unwrap();
                if i == h {
                    all.iter().all(|&p| p == me || rv[i].sees(p))
                } else {
                    rv[i].sees(host)
                }
            });
            if ready {
                break all;
            }
        }
        let seen: Vec<usize> = rv.iter().map(Rendezvous::seen).collect();
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "room of {n}: host links not up in 60 s; each sees {seen:?} others"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let setup = t0.elapsed();
    let ids: Vec<Vec<PeerId>> = vec![all; n];
    eprintln!("room of {n} connected in {setup:?}");
    let stats = ts
        .iter()
        .map(|t| (t.local_id().unwrap(), t.stats()))
        .collect();
    let peers = hs
        .into_iter()
        .zip(ts)
        .zip(ids)
        .map(|((h, t), ids)| {
            let me = t.local_id().unwrap();
            NetPeer::join(h, me, &ids, humans, seed, Box::new(t))
        })
        .collect::<Vec<_>>();
    // Host first: the trails compare against it.
    let mut peers = peers;
    peers.sort_by_key(|p| p.me);
    RealRoom {
        _server: server,
        peers,
        stats,
        played: Duration::ZERO,
        lag: LagStats::default(),
    }
}

impl RealRoom {
    /// Run every peer, in turn, for `wall` of wall time: each frame as
    /// long as the last round took (at most 100 ms, Bevy's virtual-clock
    /// cap), about 120 rounds a second at most.
    fn run(&mut self, wall: Duration) {
        let t0 = Instant::now();
        let mut last = t0;
        while t0.elapsed() < wall {
            let now = Instant::now();
            let dt = (now - last).min(Duration::from_millis(100));
            last = now;
            for p in &mut self.peers {
                p.frame(dt);
            }
            self.observe();
            if let Some(rest) = Duration::from_millis(8).checked_sub(now.elapsed()) {
                std::thread::sleep(rest);
            }
        }
        self.played += t0.elapsed();
    }

    fn observe(&mut self) {
        let lag = &mut self.lag;
        let delay = self.peers[0].session(|s| s.delay());
        lag.delay_sum += delay as u64;
        lag.rounds += 1;
        lag.delay_max = lag.delay_max.max(delay);
        let now = Instant::now();
        let at = (now - *lag.start.get_or_insert(now)).as_secs_f32();
        // The first second is the start: the guests wait for the host's
        // first turns, then catch up.
        let started = at > 1.0;
        for p in &mut self.peers[1..] {
            let me = p.me;
            let st = p.h.world_mut().resource::<NetStatus>().last_frame;
            if started {
                lag.extra += st.extra as u64;
            }
            if st.starved {
                lag.starved_since.entry(me).or_insert(now);
            } else if let Some(since) = lag.starved_since.remove(&me) {
                let d = now - since;
                if d >= Duration::from_millis(100) && started {
                    lag.stalls += 1;
                    lag.longest = lag.longest.max(d);
                    lag.stall_log.push((at, d.as_millis() as u32));
                }
            }
        }
    }

    /// Every peer's trail equals the host's over their common prefix,
    /// at least `min` ticks long; the prefix length.
    fn assert_agree(&mut self, min: usize) -> usize {
        let host = self.peers[0].trail();
        let mut shortest = host.len();
        for i in 1..self.peers.len() {
            let t = self.peers[i].trail();
            shortest = shortest.min(t.len());
            let n = t.len().min(host.len());
            if let Some(k) = (0..n).find(|&k| t[k] != host[k]) {
                panic!(
                    "{} diverges from the host at tick {}",
                    self.peers[i].me,
                    k + 1
                );
            }
        }
        assert!(shortest >= min, "only {shortest} ticks in common");
        assert!(self.peers[0].session(|s| s.desync().is_none()));
        shortest
    }

    /// Delay, lateness, round trips and traffic, for the write-up.
    fn report(&mut self, label: &str) {
        let host = self.peers[0].me;
        let delay = self.peers[0].session(|s| s.delay());
        let late = self.peers[0].session(|s| s.stats.late_batches);
        let rtts: Vec<String> = (1..self.peers.len())
            .map(|i| {
                let id = self.peers[i].me;
                let r = self.peers[0].session(|s| s.rtt(id)).unwrap_or(0.0);
                format!("{r:.1}")
            })
            .collect();
        let ticks = self.peers[0].trail().len();
        let secs = self.played.as_secs_f64();
        eprintln!(
            "{label}: {ticks} ticks in {secs:.1} s ({:.1}/s), delay {delay}, late batches {late}, rtt [{}] ms",
            ticks as f64 / secs,
            rtts.join(" ")
        );
        let l = &self.lag;
        eprintln!(
            "  delay mean {:.1} max {}; guest stalls >= 100 ms: {}, longest {:.0} ms; catch-up ticks {}",
            l.delay_sum as f64 / l.rounds.max(1) as f64,
            l.delay_max,
            l.stalls,
            l.longest.as_secs_f64() * 1000.0,
            l.extra
        );
        if !l.stall_log.is_empty() {
            let log: Vec<String> = l
                .stall_log
                .iter()
                .map(|(at, ms)| format!("{ms} ms at {at:.1} s"))
                .collect();
            eprintln!("  stalls: {}", log.join(", "));
        }
        for peer in self.peers.iter().take(2) {
            let me = peer.me;
            let s = *self.stats[&me].lock().unwrap();
            eprintln!(
                "  {}: up {:.0} B/s in {:.0} msg/s, down {:.0} B/s; dropped {:?}, throttled {}",
                if me == host { "host " } else { "guest" },
                s.sent_bytes.iter().sum::<u64>() as f64 / secs,
                s.sent_msgs.iter().sum::<u64>() as f64 / secs,
                s.recv_bytes.iter().sum::<u64>() as f64 / secs,
                s.dropped,
                s.throttled,
            );
        }
    }
}

/// Two peers and an AI on Marble Madness over WebRTC on localhost:
/// identical every tick.
#[test]
fn two_peers_webrtc_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = real_match("Marble_Madness_Map", 2, 1, 0x2c);
    r.run(Duration::from_secs(20));
    r.assert_agree(450);
    r.report("2 peers over WebRTC");
}

/// Eight peers on Quad Core over WebRTC on localhost (28 links in the
/// mesh, 7 used): identical every tick.
#[test]
fn eight_peers_webrtc_full_sim() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut r = real_match("Quad_Core", 8, 0, 0x8c);
    r.run(Duration::from_secs(15));
    r.assert_agree(200);
    r.report("8 peers over WebRTC");
}

fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

/// A match under whatever the network does to it: run inside
/// `scripts/netem-matchbox.sh`.
#[test]
#[ignore = "run under netem: scripts/netem-matchbox.sh"]
fn netem_match() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let peers: u8 = env("KP_NET_PEERS", 4);
    let ai: u8 = env("KP_NET_AI", 0);
    let secs: u64 = env("KP_NET_SECS", 30);
    let map = std::env::var("KP_NET_MAP").unwrap_or_else(|_| "Quad_Core".into());
    let mut r = real_match(&map, peers, ai, 0x4e7e);
    r.run(Duration::from_secs(secs));
    // Under loss and delay the clock still runs: at least half the
    // ticks of the wall time.
    r.assert_agree((secs * 15) as usize);
    let label = std::env::var("KP_NET_LABEL").unwrap_or_else(|_| "netem".into());
    r.report(&format!("{label}: {peers} peers on {map}"));
}

/// What the network does to a lockstep-like stream: one 64-byte packet
/// every 33 ms each way on the unreliable channel for `KP_NET_SECS`, one
/// way delay percentiles (one clock, one process). Run under netem:
/// `KP_NETEM_TEST=net::matchbox_tests::netem_probe scripts/netem-matchbox.sh …`.
#[test]
#[ignore = "run under netem: scripts/netem-matchbox.sh"]
fn netem_probe() {
    let secs: u64 = env("KP_NET_SECS", 30);
    let server = Signaling::start();
    let cfg = server.config();
    let room = RoomId::random();
    let mut a = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    std::thread::sleep(Duration::from_millis(50));
    let mut b = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    let (mut na, mut nb) = (0, 0);
    wait_for("connect", Duration::from_secs(20), || {
        na += a.poll_peers().len();
        nb += b.poll_peers().len();
        na == 1 && nb == 1
    });
    let (ida, idb) = (a.local_id().unwrap(), b.local_id().unwrap());
    let t0 = Instant::now();
    let us = |t0: Instant| t0.elapsed().as_micros() as u64;
    // Per direction: send time by seq, arrivals as (seq, time).
    let mut sent_at: [Vec<u64>; 2] = [Vec::new(), Vec::new()];
    let mut arrived: [Vec<(u64, u64)>; 2] = [Vec::new(), Vec::new()];
    let mut next = t0;
    while t0.elapsed() < Duration::from_secs(secs) {
        if Instant::now() >= next {
            for (dir, (from, to)) in [(&mut a, idb), (&mut b, ida)].into_iter().enumerate() {
                let seq = sent_at[dir].len() as u64;
                sent_at[dir].push(us(t0));
                let mut m = vec![0u8; 64];
                m[..8].copy_from_slice(&seq.to_le_bytes());
                from.send(to, Channel::Unreliable, m);
            }
            next += Duration::from_millis(33);
        }
        a.poll_peers();
        b.poll_peers();
        let now = us(t0);
        for (dir, t) in [(0, &mut b), (1, &mut a)] {
            for (_, m) in t.receive(Channel::Unreliable) {
                arrived[dir].push((u64::from_le_bytes(m[..8].try_into().unwrap()), now));
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // The last second is still in flight.
    let cut = |sent: &[u64]| sent.iter().filter(|&&t| t + 1_000_000 < us(t0)).count();
    let pct = |v: &mut Vec<u64>, p: f64| {
        v.sort_unstable();
        v.get(((v.len().max(1) - 1) as f64 * p) as usize)
            .map_or(0.0, |x| *x as f64 / 1000.0)
    };
    let label = std::env::var("KP_NET_LABEL").unwrap_or_default();
    let (mut lat, mut late_retx, mut eff) = (Vec::new(), Vec::new(), Vec::new());
    let (mut n_sent, mut missing, mut retx) = (0, 0, 0);
    for dir in 0..2 {
        let n = cut(&sent_at[dir]);
        n_sent += n;
        let mut first: Vec<Option<u64>> = vec![None; sent_at[dir].len()];
        let mut newest = None::<u64>;
        for &(seq, at) in &arrived[dir] {
            let l = at - sent_at[dir][seq as usize];
            if newest.is_some_and(|m| seq < m) {
                // Overtaken by a later packet: the lost one, resent.
                retx += 1;
                late_retx.push(l);
            } else {
                lat.push(l);
            }
            newest = newest.max(Some(seq));
            first[seq as usize].get_or_insert(at);
        }
        // What a stream that repeats unacknowledged content sees: seq k
        // is known once any packet >= k is in.
        let mut best = None::<u64>;
        for k in (0..first.len()).rev() {
            best = match (best, first[k]) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (x, y) => x.or(y),
            };
            if k < n {
                missing += usize::from(first[k].is_none());
                if let Some(b) = best {
                    eff.push(b - sent_at[dir][k]);
                }
            }
        }
    }
    eprintln!(
        "{label}: {n_sent} sent, {missing} never arrived, {retx} arrived after a later one \
         (p50 {:.0} / max {:.0} ms)\n  in order: p50 {:.1} / p95 {:.1} / p99 {:.1} / max {:.1} ms\n  \
         newest-known (with repetition): p50 {:.1} / p95 {:.1} / p99 {:.1} / max {:.1} ms",
        pct(&mut late_retx, 0.5),
        pct(&mut late_retx, 1.0),
        pct(&mut lat, 0.5),
        pct(&mut lat, 0.95),
        pct(&mut lat, 0.99),
        pct(&mut lat, 1.0),
        pct(&mut eff, 0.5),
        pct(&mut eff, 0.95),
        pct(&mut eff, 0.99),
        pct(&mut eff, 1.0),
    );
    // CI's guard against a channel that is reliable after all (each loss
    // holding up everything behind it): the newest-known delay's p99
    // within this many ms of its median.
    if let Some(spread) = std::env::var("KP_NET_PROBE_SPREAD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
    {
        let (p50, p99) = (pct(&mut eff, 0.5), pct(&mut eff, 0.99));
        assert!(
            p99 - p50 <= spread,
            "newest-known p99 {p99:.1} ms vs median {p50:.1} ms: packets wait for lost ones"
        );
        assert!(missing > 0 || retx > 0, "no loss seen: is netem on?");
    }
}

/// What a browser's SCTP (Chromium: dcsctp) does to the unreliable
/// channel: the browser (`kp_net_probe`) sends a numbered 64-byte packet
/// every 33 ms, this process counts what arrives and how long the newest
/// packet in hand goes stale. Run under netem:
/// `KP_NETEM_TEST=net::matchbox_tests::browser_probe KP_NETPEER_PKG=… scripts/netem-matchbox.sh …`.
#[test]
#[ignore = "needs the wasm replay build and Playwright: KP_NETPEER_PKG=<pkg dir>"]
fn browser_probe() {
    let pkg = std::env::var("KP_NETPEER_PKG").expect("KP_NETPEER_PKG: the wasm pkg dir");
    let runtime = std::env::var("KP_NETPEER_RUNTIME").unwrap_or_else(|_| "chromium".into());
    let secs: u64 = env("KP_NET_SECS", 30);
    let root = crate::paths::project_root();
    let out = root.join("target").join(format!("netprobe-{runtime}.txt"));
    let server = Signaling::start();
    let stun = Stun::start();
    let room = RoomId::random();
    let mut child = std::process::Command::new("node")
        .current_dir(&root)
        .env("KP_NETPEER_ICE", browser_ice(&stun))
        .args([".github/scripts/kp-netpeer.mjs", &runtime, &pkg, "-"])
        .arg(&server.url)
        .arg(room.as_str())
        .args(["probe", "0", "0", "0"])
        .arg(secs.to_string())
        .arg(&out)
        .spawn()
        .expect("node");
    let (mut t, _) = meet_browser(&server, &room, 1, &mut child, Duration::from_secs(180));
    let mut got: Vec<(u64, Instant)> = Vec::new();
    let status = loop {
        t.poll_peers();
        let now = Instant::now();
        for (_, m) in t.receive(Channel::Unreliable) {
            got.push((u64::from_le_bytes(m[..8].try_into().unwrap()), now));
        }
        if let Some(status) = child.try_wait().expect("node") {
            break status;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(status.success(), "the browser runner failed: {status}");
    let newest = got.iter().map(|(s, _)| *s).max().unwrap_or(0);
    let mut seen = vec![false; newest as usize + 1];
    let (mut late, mut top, mut gaps) = (0, None::<u64>, Vec::new());
    let mut last_new: Option<Instant> = None;
    for &(seq, at) in &got {
        seen[seq as usize] = true;
        if top.is_some_and(|m| seq < m) {
            late += 1;
            continue;
        }
        if let Some(l) = last_new {
            gaps.push((at - l).as_secs_f64() * 1000.0);
        }
        last_new = Some(at);
        top = Some(seq);
    }
    gaps.sort_by(f64::total_cmp);
    let q = |p: f64| gaps[((gaps.len() - 1) as f64 * p) as usize];
    let label = std::env::var("KP_NET_LABEL").unwrap_or_default();
    eprintln!(
        "{label}: {runtime} sent {}, {} never arrived, {late} arrived after a later one; \
         newest-in-hand gaps p50 {:.1} / p99 {:.1} / max {:.1} ms",
        newest + 1,
        seen.iter().filter(|s| !**s).count(),
        q(0.5),
        q(0.99),
        q(1.0)
    );
}

/// A native peer (this process) and a browser peer (the wasm build in
/// headless Chromium) in one room: the same match, the same trail.
#[test]
#[ignore = "needs the wasm replay build and Playwright: KP_NETPEER_PKG=<pkg dir>"]
fn browser_peer() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let pkg = std::env::var("KP_NETPEER_PKG").expect("KP_NETPEER_PKG: the wasm pkg dir");
    let runtime = std::env::var("KP_NETPEER_RUNTIME").unwrap_or_else(|_| "chromium".into());
    let secs: u64 = env("KP_NETPEER_SECS", 30);
    let (map, humans, ai, seed) = ("Marble_Madness_Map", 2u8, 1u8, 0xb4u64);
    let root = crate::paths::project_root();
    let out = root.join("target").join(format!("netpeer-{runtime}.trail"));
    let _ = std::fs::remove_file(&out);

    let mut h = Some(SimHarness::new(netplay::scenario(map, humans, ai, seed)));
    let server = Signaling::start();
    let stun = Stun::start();
    let room = RoomId::random();
    let mut child = std::process::Command::new("node")
        .current_dir(&root)
        .env("KP_NETPEER_ICE", browser_ice(&stun))
        .arg(".github/scripts/kp-netpeer.mjs")
        .arg(&runtime)
        .arg(&pkg)
        .arg("kernel-panic/assets/maps")
        .arg(&server.url)
        .arg(room.as_str())
        .arg(map)
        .arg(humans.to_string())
        .arg(ai.to_string())
        .arg(seed.to_string())
        .arg(secs.to_string())
        .arg(&out)
        .spawn()
        .expect("node");
    let (t, ids) = meet_browser(&server, &room, 1, &mut child, Duration::from_secs(180));
    let stats = t.stats();
    let me = t.local_id().unwrap();
    let mut peer = NetPeer::join(h.take().unwrap(), me, &ids, humans, seed, Box::new(t));
    eprintln!(
        "native peer {me} is the {}",
        if ids.iter().all(|&p| p >= me) {
            "host"
        } else {
            "guest"
        }
    );
    // Play until the browser is done (it plays `secs` once connected),
    // then a little longer so it never waits for us.
    let t0 = Instant::now();
    let mut last = t0;
    let status = loop {
        let now = Instant::now();
        let dt = (now - last).min(Duration::from_millis(100));
        last = now;
        peer.frame(dt);
        std::thread::sleep(Duration::from_millis(5));
        if let Some(status) = child.try_wait().expect("node") {
            break status;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(secs + 120),
            "the browser peer never finished"
        );
    };
    assert!(status.success(), "the browser runner failed: {status}");
    let browser: Vec<u64> = std::fs::read_to_string(&out)
        .expect("the browser's trail")
        .lines()
        .map(|l| u64::from_str_radix(l.split(' ').nth(1).unwrap(), 16).unwrap())
        .collect();
    let native = peer.trail();
    let n = browser.len().min(native.len());
    if let Some(k) = (0..n).find(|&k| browser[k] != native[k]) {
        panic!("browser and native differ at tick {}", k + 1);
    }
    let s = *stats.lock().unwrap();
    eprintln!(
        "browser ({runtime}) and native agree over {n} ticks (browser {}, native {}); native sent {:?} B, received {:?} B",
        browser.len(),
        native.len(),
        s.sent_bytes,
        s.recv_bytes
    );
    assert!(n >= (secs * 15) as usize, "only {n} ticks in common");
}

// --- the deployed signaling server ----------------------------------------------

/// Two native peers on this machine through the deployed signaling
/// server (`wss://omdurman-matchbox.fly.dev`, or `KP_SIGNALING`) and the
/// default ICE servers (Google STUN): the server answers (it sleeps when
/// idle; the first join wakes it), the peers meet and connect, both
/// channels carry traffic, and a room forms over it (the lobby: host,
/// seats, a chat line). `scripts/signaling-smoke.sh`; never in CI (it
/// needs the internet and the server up).
#[test]
#[ignore = "talks to the deployed signaling server: scripts/signaling-smoke.sh"]
fn deployed_signaling_smoke() {
    use super::lobby::{Lobby, MapHave};
    use super::rejoin::PlayerKey;

    let mut cfg = NetConfig::default();
    if let Ok(url) = std::env::var("KP_SIGNALING") {
        cfg.signaling = url;
    }
    let room: RoomId = format!("smoke-{}", RoomId::random())
        .parse()
        .expect("a room code");
    eprintln!("signaling {} room {room}", cfg.room_url(&room));
    let t0 = Instant::now();
    let mut a = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    let id_a = wait_for("the server's answer", Duration::from_secs(60), || {
        assert_eq!(a.failure(), None, "the signaling server");
        a.poll_peers();
        a.local_id().is_some()
    });
    let mut b = MatchboxTransport::open(&cfg.room_url(&room), &cfg);
    let (mut a_sees, mut b_sees) = (Vec::new(), Vec::new());
    let meet = wait_for("the peers to connect", Duration::from_secs(60), || {
        a_sees.extend(a.poll_peers());
        b_sees.extend(b.poll_peers());
        !a_sees.is_empty() && !b_sees.is_empty()
    });
    let (ida, idb) = (a.local_id().unwrap(), b.local_id().unwrap());
    assert_eq!(a_sees, vec![PeerEvent::Joined(idb)]);
    assert_eq!(b_sees, vec![PeerEvent::Joined(ida)]);

    // Round trips on the reliable channel; a burst on the unreliable one.
    let mut rtts = Vec::new();
    for i in 0..20u8 {
        let sent = Instant::now();
        a.send(idb, Channel::Reliable, vec![i; 64]);
        let mut echoed = false;
        wait_for("an echo", Duration::from_secs(10), || {
            a.poll_peers();
            b.poll_peers();
            for (from, m) in b.receive(Channel::Reliable) {
                b.send(from, Channel::Reliable, m);
            }
            echoed |= a.receive(Channel::Reliable).iter().any(|(_, m)| m[0] == i);
            echoed
        });
        rtts.push(sent.elapsed().as_secs_f64() * 1000.0);
    }
    rtts.sort_by(f64::total_cmp);
    for i in 0..100u8 {
        b.send(ida, Channel::Unreliable, vec![i; 200]);
    }
    let mut got = 0;
    let _ = wait_for("the unreliable burst", Duration::from_secs(5), || {
        a.poll_peers();
        b.poll_peers();
        got += a.receive(Channel::Unreliable).len();
        got >= 100
    });

    // A room over the same connections (already connected: as a match
    // hands its connection back).
    let map = "Marble_Madness_Map";
    let mut la = Lobby::resume(
        Box::new(a),
        &[idb],
        "smoke-a",
        map,
        None,
        PlayerKey::random(),
    );
    let mut lb = Lobby::resume(
        Box::new(b),
        &[ida],
        "smoke-b",
        map,
        None,
        PlayerKey::random(),
    );
    for l in [&mut la, &mut lb] {
        l.provide_map(map, MapHave::Hash(1));
    }
    let t1 = Instant::now();
    let mut said = false;
    let mut heard = Vec::new();
    let formed = wait_for("the room", Duration::from_secs(20), || {
        let now = t1.elapsed();
        la.poll(now);
        lb.poll(now);
        heard.extend(la.take_chat());
        let full = la.room().is_some_and(|r| r.members.len() == 2)
            && lb.room().is_some_and(|r| r.members.len() == 2);
        if full && !said {
            said = true;
            let guest = if la.is_host() { &mut lb } else { &mut la };
            guest.say("smoke").unwrap();
        }
        full && heard.iter().any(|l| l.text == "smoke")
    });
    eprintln!(
        "deployed signaling: server answered in {id_a:?}, peers connected {meet:?} later; \
         reliable round trip p50 {:.1} / max {:.1} ms; unreliable {got}/100; \
         room formed and a chat line went round in {formed:?}; {:?} in all",
        rtts[rtts.len() / 2],
        rtts[rtts.len() - 1],
        t0.elapsed()
    );
}

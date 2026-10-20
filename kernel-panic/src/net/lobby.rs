//! The room: who is here, who plays which seat, and the handshake that
//! starts a networked match (`docs/multiplayer-plan.md` M2, write-up
//! `net-experiments/m2-lobby.md`). The protocol ([`Lobby`]) runs on any
//! [`Transport`] — the simulated network in the tests, matchbox in the
//! game — and the Bevy side ([`LobbyPlugin`]) drives it from the launch
//! menu's Multiplayer page.
//!
//! The shape is cake's lobby (`../cake/cake-bevy/src/lobby.rs`) with
//! Spring's battle room on top:
//!
//! - **Host**: the lowest [`PeerId`] among the peers that introduced
//!   themselves with a compatible build ([`LobbyMsg::Hello`]), as the
//!   session elects its sequencing host (`session.rs`), so the lobby's
//!   host is the match's. Re-elected whenever that set changes: when
//!   the host leaves, the next lowest takes over the room as it last
//!   saw it; a newcomer with a lower id takes it over from the
//!   `Hello`s, which carry each peer's latest [`RoomState`]. A peer
//!   only acts as host once every connected peer said hello (or
//!   [`SETTLE`] passed), so a newcomer never broadcasts a blank room.
//! - **The room** ([`RoomState`]) is the host's: map, AI difficulty,
//!   seats (faction, ally team, open / AI / a peer), the members with
//!   their names, ready flags and the host's verdict on their build and
//!   map. The host sends it to everyone whenever it changes; guests
//!   take it verbatim and ask the host for changes to their own seat.
//!   Spring's battle room the same: players pick their side, the host
//!   the map, teams and AIs.
//! - **Ready** (Spring's `readyToStart`, `CGameServer::CheckForGameStart`,
//!   `rts/Net/GameServer.cpp:2135-2177`): every seated guest must be
//!   ready and hold the host's map before the host can start; a map
//!   change clears the flags (the status names the map it is for).
//! - **Start** ([`StartMsg`]): the full match setup (map, seats, AI
//!   difficulty, the seed the host drew), the session roster (every
//!   member that may join: its seat, or a spectator) and the host's
//!   compatibility stamp ([`Compat`]: sim version, unit data, map
//!   bytes). A guest whose own stamp differs refuses with the reason
//!   and leaves the room, so the host's session does not wait for it;
//!   a peer outside the roster leaves too. Everyone else inserts
//!   [`NextNetMatch`] and starts the match through the normal match
//!   swap.
//!
//! - **A running match** (M3, [`super::rejoin`]): a newcomer's `Hello`
//!   reaches the host's session, which answers [`LobbyMsg::Running`]: the
//!   `Start` with the roster as it is now. Every `Hello` carries the
//!   peer's [`PlayerKey`], so a player who comes back gets its seat
//!   back; anyone else watches. The newcomer checks the stamp as at a
//!   `Start` and joins late ([`MatchStart::join`]).
//!
//! Lobby and session traffic share the transport: every lobby message
//! starts with [`LOBBY_TAG`] where a session [`super::wire::Envelope`] has its
//! format version, so neither side mistakes the other's packets (a late
//! `Status` reaching the host's running session is dropped there).
//!
//! M3 (`net-experiments/m3-rooms.md`):
//!
//! - **Stuck rooms.** Under loss a pair of peers may never connect
//!   (`m2-transport.md` §6: 3 of ~20 four-peer rooms at 2–3 %). The
//!   rooms the peers show each other name everyone the room has; a peer
//!   that has not reached one of them within [`STUCK`] — the host or
//!   another member, the higher id of the pair acts — reopens its
//!   connection ([`Lobby::with_reopen`]): a new socket, a new id, a new
//!   round of connections to everyone. Its `Hello` carries the same
//!   [`PlayerKey`] under the new id, so the host gives it its seat back.
//! - **Back to the room.** A match hands its connection back
//!   ([`super::driver::BackToRoom`]); [`Lobby::resume`] picks up the
//!   room as it was at the start, same code, same seats. Peers still in
//!   the match keep their seats ("away") and answer once they are back:
//!   a first `Hello` from a peer is always answered with ours.
//! - **Names and chat.** [`Lobby::set_name`] says hello again under the
//!   new name; [`Lobby::say`] sends a line through the host
//!   ([`super::chat`]).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use super::MatchRules;
use super::chat::{self, ChatLine};
use super::compat::{Build, Compat};
use super::driver::{CatchUp, NextNetMatch, PeerNames};
use super::rejoin::{Door, PlayerKey, Running};
use super::replay_log::{MatchSetup, Seat};
use super::session::{DelayBounds, SessionConfig};
use super::transport::{Channel, MAX_MESSAGE, PeerEvent, PeerId, Transport};
use crate::game_setup::{AppState, GameSetup, PlayerSpec};
use crate::units::components::Faction;

// --- the room code ------------------------------------------------------

pub use super::matchbox::RoomId;

/// How the lobby opens (and reopens) its connection to a room: in the
/// game matchbox over WebRTC, through the launch's signaling server and
/// ICE servers
/// ([`NetConfig::from_launch`](super::matchbox::NetConfig::from_launch)).
/// The menu screenshots (`ui::menu_shots`) open rooms nobody else
/// reaches ([`Self::offline`]), so a dev tool never shows up on the
/// public signaling server; tests meet over a simulated network
/// ([`Self::sim`]) or stay alone (the default under test).
#[derive(Resource, Clone)]
pub struct RoomConnector(Arc<OpenRoom>);

type OpenRoom = dyn Fn(&RoomId) -> Box<dyn Transport> + Send + Sync;

#[cfg(not(test))]
impl Default for RoomConnector {
    fn default() -> Self {
        Self::matchbox()
    }
}

#[cfg(test)]
impl Default for RoomConnector {
    fn default() -> Self {
        Self::offline()
    }
}

impl RoomConnector {
    /// The room on the launch's signaling server.
    #[cfg(not(test))]
    pub fn matchbox() -> Self {
        Self(Arc::new(|room| {
            super::matchbox::open_room(room, &super::matchbox::NetConfig::from_launch())
        }))
    }

    /// A [`LoneTransport`](super::transport::LoneTransport) under a fresh
    /// id: the lobby hosts a room of its own.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn offline() -> Self {
        Self(Arc::new(|_| {
            Box::new(super::transport::LoneTransport::new(random_peer()))
        }))
    }

    /// Tests: an in-process [`SimNetwork`](super::sim_net::SimNetwork)
    /// per room code (no latency), shared by every clone, so apps in one
    /// test process meet without a signaling server.
    #[cfg(test)]
    pub fn sim() -> Self {
        use std::sync::Mutex;

        use super::sim_net::SimNetwork;

        let rooms: Mutex<BTreeMap<String, SimNetwork>> = Mutex::default();
        Self(Arc::new(move |room| {
            let net = rooms
                .lock()
                .expect("room registry poisoned")
                .entry(room.to_string())
                .or_insert_with(|| SimNetwork::new(1))
                .clone();
            Box::new(net.join(random_peer()))
        }))
    }

    /// Opens (again) the connection to `room`.
    fn to(&self, room: RoomId) -> Connector {
        let open = self.0.clone();
        Box::new(move || open(&room))
    }
}

/// A fresh random id.
fn random_peer() -> PeerId {
    PeerId(
        (crate::game_setup::random_seed() as u128) << 64 | crate::game_setup::random_seed() as u128,
    )
}

// --- the wire -----------------------------------------------------------

/// The first field of every lobby message, where a session
/// [`super::wire::Envelope`] carries its small format version: the session
/// drops lobby packets and the lobby session packets.
pub const LOBBY_TAG: u16 = 0x4b4c;

/// Bumped whenever a lobby message changes shape. A peer with another
/// one runs another build: it is listed, never seated.
/// 2 (M3): [`Hello::key`], [`LobbyMsg::Running`] (turn log),
/// [`LobbyMsg::Chat`] (rooms).
pub const LOBBY_VERSION: u16 = 2;

/// A peer's announcement of itself, to everyone, when it meets them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub name: String,
    pub build: Build,
    /// The newest room this peer knows (as host or guest): a host
    /// elected after it takes the room over from here.
    pub room: Option<RoomState>,
    /// The player's [`PlayerKey`]: who it is across connections, so a
    /// running match gives a returning player its seat back, and a room
    /// the seat of a peer that reopened its connection.
    pub key: u64,
}

/// Whether a peer has a map, as its content hash ([`Compat::map_hash`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MapHave {
    /// Still reading or fetching it.
    Pending,
    Missing,
    Hash(u64),
}

/// A peer's readiness for the room's map, to everyone (a host elected
/// later needs it too).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub ready: bool,
    /// The map `have` and `ready` refer to.
    pub map: String,
    pub have: MapHave,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LobbyMsg {
    Hello(Hello),
    Status(Status),
    /// Guest → host: put me on this open seat.
    Sit {
        seat: u8,
    },
    /// Guest → host: take me off my seat (spectate).
    Stand,
    /// Guest → host: my seat's faction ([`Faction::ALL`] index).
    Faction {
        faction: u8,
    },
    /// Host → all: the room, whenever it changes.
    Room(RoomState),
    /// Host → all: the match begins.
    Start(StartMsg),
    // M3.
    /// Host of a running match → a peer that said hello: join it
    /// ([`super::rejoin`]).
    Running(Box<Running>),
    /// A chat line: guest → host with `from` = itself, host → all
    /// (every line, its own included, in the host's order).
    Chat {
        from: PeerId,
        text: String,
    },
}

#[derive(Serialize, Deserialize)]
struct LobbyEnvelope {
    tag: u16,
    version: u16,
    msg: LobbyMsg,
}

/// Encode a lobby message; `None` if it would exceed [`MAX_MESSAGE`].
pub fn encode(msg: LobbyMsg) -> Option<Vec<u8>> {
    let env = LobbyEnvelope {
        tag: LOBBY_TAG,
        version: LOBBY_VERSION,
        msg,
    };
    match postcard::to_allocvec(&env) {
        Ok(v) if !v.is_empty() && v.len() <= MAX_MESSAGE => Some(v),
        Ok(v) => {
            warn!("{} byte lobby message not sent", v.len());
            None
        }
        Err(e) => {
            error!("lobby encode failed: {e}");
            None
        }
    }
}

/// What arrived, as far as the lobby is concerned.
#[derive(Debug, PartialEq, Eq)]
pub enum Decoded {
    Msg(LobbyMsg),
    /// A lobby message of another [`LOBBY_VERSION`]: another build.
    Foreign,
    /// Not lobby traffic (a session packet) or garbage.
    Other,
}

pub fn decode(bytes: &[u8]) -> Decoded {
    let Ok(((tag, version), rest)) = postcard::take_from_bytes::<(u16, u16)>(bytes) else {
        return Decoded::Other;
    };
    if tag != LOBBY_TAG {
        return Decoded::Other;
    }
    if version != LOBBY_VERSION {
        return Decoded::Foreign;
    }
    postcard::from_bytes(rest).map_or(Decoded::Other, Decoded::Msg)
}

// --- the room -----------------------------------------------------------

/// Seats a room offers at most: 4v4 (`docs/multiplayer-plan.md`).
pub const MAX_SEATS: usize = 8;

/// Characters of a name shown.
pub const NAME_MAX: usize = 20;

/// How long a peer waits for every connected peer's hello before it
/// acts as host anyway (a peer that never answers is in a match, or
/// broken).
pub const SETTLE: Duration = Duration::from_secs(3);

/// How long a peer the room names may stay unreached before the higher
/// id of the pair reopens its connection. WebRTC connects in 0.5–4 s
/// here (`m2-transport.md` §4, 8 peers at once); a pair that failed
/// takes webrtc-rs ~30 s to give up on its own.
pub const STUCK: Duration = Duration::from_secs(10);

/// Reopens per room before the lobby gives up and says so.
pub const MAX_REOPENS: u32 = 3;

/// When a stalled handshake is started afresh. matchbox never retries
/// one (no timeout, no retry), so a link missing since some time is
/// given up on: the waiting side reopens its connection under a new id,
/// a new round of handshakes with everyone. The lobby's stuck-room
/// check ([`Self::fixed`]: [`STUCK`], [`MAX_REOPENS`]) and the
/// real-WebRTC tests' rendezvous ([`Self::growing`]) decide with it.
#[derive(Clone, Copy, Debug)]
pub struct Retry {
    /// How long the first attempt waits.
    patience: Duration,
    /// Attempt `n` waits `n` × `patience` (a slow but working handshake
    /// gets through).
    growing: bool,
    /// Reopens before giving up.
    max: u32,
    reopens: u32,
}

impl Retry {
    /// Every attempt waits `patience`; at most `max` reopens.
    pub const fn fixed(patience: Duration, max: u32) -> Self {
        Self {
            patience,
            growing: false,
            max,
            reopens: 0,
        }
    }

    /// Attempt `n` waits `n` × `patience`, as often as it takes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const fn growing(patience: Duration) -> Self {
        Self {
            patience,
            growing: true,
            max: u32::MAX,
            reopens: 0,
        }
    }

    /// How long the current attempt waits.
    pub fn patience(&self) -> Duration {
        if self.growing {
            self.patience * (self.reopens + 1)
        } else {
            self.patience
        }
    }

    /// A link missing since `since` (any monotonic clock) is given up on
    /// at `now`: reopen.
    pub fn due(&self, since: Duration, now: Duration) -> bool {
        !self.exhausted() && now.saturating_sub(since) >= self.patience()
    }

    /// The connection was reopened.
    pub fn reopened(&mut self) {
        self.reopens += 1;
    }

    pub fn reopens(&self) -> u32 {
        self.reopens
    }

    /// No reopens left.
    pub fn exhausted(&self) -> bool {
        self.reopens >= self.max
    }
}

/// Opens (again) the connection to the room the lobby is in.
pub type Connector = Box<dyn FnMut() -> Box<dyn Transport> + Send + Sync>;

/// Who sits on a seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Occupant {
    Open,
    Ai,
    Human(PeerId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatSpec {
    /// [`Faction::ALL`] index.
    pub faction: u8,
    /// Spring's allyteam (`PlayerSpec::ally_team`).
    pub ally_team: u8,
    pub who: Occupant,
}

impl SeatSpec {
    pub fn faction(&self) -> Faction {
        Faction::from_team_id(self.faction)
    }
}

/// The host's verdict on a member: may it play this match?
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Check {
    /// Its build or map hash is not in yet.
    Pending,
    Ok,
    /// Why not, readable ("runs game version 5 …", "has no copy of map
    /// X").
    Refused(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub peer: PeerId,
    pub name: String,
    /// Ready for the room's current map.
    pub ready: bool,
    pub check: Check,
}

/// The room as the host keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomState {
    /// Counts the host's changes; the newest wins when a new host takes
    /// the room over.
    pub rev: u32,
    pub host: PeerId,
    /// Map file stem (`GameSetup::map`).
    pub map: String,
    /// AI difficulty, 1 Easy … 4 Extreme.
    pub difficulty: u8,
    pub seats: Vec<SeatSpec>,
    /// Pausing, game speed and cheats (the host's to set; the `Start`
    /// carries them in the setup).
    pub rules: MatchRules,
    /// Everyone in the room, by id (the host included).
    pub members: Vec<Member>,
}

impl RoomState {
    /// A fresh room: the host on seat 0 and an open seat against it.
    pub fn new(host: PeerId, map: &str) -> Self {
        Self {
            rev: 0,
            host,
            map: map.to_string(),
            difficulty: 2,
            seats: vec![
                SeatSpec {
                    faction: 0,
                    ally_team: 0,
                    who: Occupant::Human(host),
                },
                SeatSpec {
                    faction: 1,
                    ally_team: 1,
                    who: Occupant::Open,
                },
            ],
            rules: MatchRules::default(),
            members: Vec::new(),
        }
    }

    pub fn member(&self, peer: PeerId) -> Option<&Member> {
        self.members.iter().find(|m| m.peer == peer)
    }

    /// The seat `peer` sits on.
    pub fn seat_of(&self, peer: PeerId) -> Option<usize> {
        self.seats
            .iter()
            .position(|s| s.who == Occupant::Human(peer))
    }

    /// Equal but for the revision.
    fn same(&self, other: &RoomState) -> bool {
        let Self {
            rev: _,
            host,
            map,
            difficulty,
            seats,
            rules,
            members,
        } = self;
        *host == other.host
            && *map == other.map
            && *difficulty == other.difficulty
            && *seats == other.seats
            && *rules == other.rules
            && *members == other.members
    }
}

/// The match setup as the `Start` carries it: a replay's
/// ([`MatchSetup`]) without `sandbox`, so the two conversions
/// ([`encode_setup`], [`decode_setup`]) are the replay's, the one place
/// a new `GameSetup` field joins the handshake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupWire {
    pub map: String,
    /// Per seat, in owner order.
    pub seats: Vec<Seat>,
    pub difficulty: u8,
    pub seed: u64,
    pub rules: MatchRules,
}

/// The networked part of `setup`: a match a lobby starts is never the
/// demo, a showcase or a sandbox.
pub fn encode_setup(setup: &GameSetup) -> SetupWire {
    let MatchSetup {
        map,
        seats,
        difficulty,
        seed,
        sandbox: _,
        rules,
    } = MatchSetup::of(setup);
    SetupWire {
        map,
        seats,
        difficulty,
        seed,
        rules,
    }
}

pub fn decode_setup(wire: &SetupWire) -> GameSetup {
    MatchSetup {
        map: wire.map.clone(),
        seats: wire.seats.clone(),
        difficulty: wire.difficulty,
        seed: wire.seed,
        sandbox: false,
        rules: wire.rules,
    }
    .game_setup()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartMsg {
    /// The session's match id: an earlier match's packets are dropped.
    pub match_id: u64,
    pub setup: SetupWire,
    /// Every peer of the match and its seat (`None`: spectator).
    pub roster: Vec<(PeerId, Option<u8>)>,
    /// The host's stamp; every peer must have the same.
    pub compat: Compat,
}

/// A match to start, as the lobby hands it over: the setup the map
/// loader reads and this peer's session.
#[derive(Clone, Debug)]
pub struct MatchStart {
    pub setup: GameSetup,
    pub session: SessionConfig,
    /// Joining a running match ([`LobbyMsg::Running`]): its host and the
    /// peers that already left ([`super::session::Session::late`]).
    pub join: Option<(PeerId, Vec<PeerId>)>,
    /// Host: what lets late joiners in ([`super::session::Session::set_door`]).
    pub door: Option<Door>,
    /// Everyone's name, for the player list.
    pub names: Vec<(PeerId, String)>,
}

impl MatchStart {
    fn of(msg: &StartMsg, me: PeerId) -> Self {
        Self {
            setup: decode_setup(&msg.setup),
            session: SessionConfig {
                match_id: msg.match_id,
                me,
                roster: msg.roster.iter().copied().collect(),
                delay: DelayBounds::default(),
            },
            join: None,
            door: None,
            names: Vec::new(),
        }
    }

    /// A late joiner's start: the match `running` describes.
    fn late(running: &Running, me: PeerId) -> Self {
        Self {
            join: Some((running.host, running.gone.clone())),
            names: running.names.clone(),
            ..Self::of(&running.start, me)
        }
    }
}

/// What the lobby tells its owner.
#[derive(Clone, Debug)]
pub enum LobbyEvent {
    /// The match begins: take the transport ([`Lobby::into_transport`]).
    Start(Box<MatchStart>),
    /// The room is over for this peer (refused, or left out of the
    /// match): why. Drop the lobby, which leaves the room.
    Closed(String),
}

/// What this peer knows about another.
#[derive(Debug, Default)]
struct PeerInfo {
    name: Option<String>,
    /// Why its build cannot play with ours.
    incompatible: Option<String>,
    /// It speaks another lobby version.
    foreign: bool,
    status: Option<Status>,
    /// The room its latest `Hello` showed (who the room has).
    room: Option<RoomState>,
}

impl PeerInfo {
    fn introduced(&self) -> bool {
        self.name.is_some() || self.foreign
    }

    fn compatible(&self) -> bool {
        self.name.is_some() && self.incompatible.is_none() && !self.foreign
    }
}

/// A name as rooms show it: no control characters, at most
/// [`NAME_MAX`] characters, trimmed.
pub(crate) fn clean_name(name: &str) -> String {
    chat::clean_to(name, NAME_MAX)
}

/// One peer's side of a room.
pub struct Lobby {
    transport: Box<dyn Transport>,
    name: String,
    build: Build,
    connected: BTreeSet<PeerId>,
    peers: BTreeMap<PeerId, PeerInfo>,
    /// The room: the host's (as the last `Room` said), or our own.
    room: Option<RoomState>,
    /// What we last broadcast as host.
    sent: Option<RoomState>,
    /// Host: something the members depend on changed (a peer, a hello
    /// or status, our name or maps, the room): [`Self::keep_room`]
    /// judges them again.
    rejudge: bool,
    /// The newest room a hello brought.
    offered: Option<RoomState>,
    /// The map a fresh room starts on.
    default_map: String,
    /// Our status, and what we last sent of it.
    ready: bool,
    sent_status: Option<Status>,
    /// Our hashes of the maps asked about.
    maps: BTreeMap<String, MapHave>,
    last_join: Duration,
    settled: bool,
    done: bool,
    events: Vec<LobbyEvent>,
    /// Something on view changed since the last [`Self::take_dirty`].
    dirty: bool,
    /// Why the connection to the room is gone ([`Transport::failure`]).
    failure: Option<String>,
    /// Who this player is across connections ([`Hello::key`]).
    key: PlayerKey,
    /// A running match's host asked us in ([`LobbyMsg::Running`]); we
    /// join once our hash of its map is in.
    running: Option<Running>,
    /// Reopens the connection when the room is stuck.
    reopen: Option<Connector>,
    retry: Retry,
    /// Our id before the last reopen (not missing: it is us).
    was: Option<PeerId>,
    /// When the connection was (re)opened (the first poll after).
    opened_at: Option<Duration>,
    /// Peers the room names that we have not reached, since when.
    missing: BTreeMap<PeerId, Duration>,
    /// Chat lines that arrived, for the owner ([`Self::take_chat`]).
    said: Vec<ChatLine>,
    /// Host: the seats opened because their peer left, by that peer: a
    /// peer that reopened its connection (the same [`Hello::key`]) may
    /// come back to it.
    vacated: BTreeMap<PeerId, usize>,
    /// Every [`Hello::key`] heard, by connection, kept after the peer
    /// leaves (for [`Self::vacated`]).
    keys: BTreeMap<PeerId, PlayerKey>,
}

impl Lobby {
    /// Enter a room over `transport` as `name`; a room this peer ends up
    /// hosting starts on `default_map`.
    pub fn new(transport: Box<dyn Transport>, name: &str, default_map: &str) -> Self {
        Self::with_build(transport, name, default_map, Build::current())
    }

    /// [`Self::new`] claiming `build` (tests: another version).
    pub fn with_build(
        transport: Box<dyn Transport>,
        name: &str,
        default_map: &str,
        build: Build,
    ) -> Self {
        Self {
            transport,
            name: clean_name(name),
            build,
            connected: BTreeSet::new(),
            peers: BTreeMap::new(),
            room: None,
            sent: None,
            rejudge: true,
            offered: None,
            default_map: default_map.to_string(),
            ready: false,
            sent_status: None,
            maps: BTreeMap::new(),
            last_join: Duration::ZERO,
            settled: false,
            done: false,
            events: Vec::new(),
            dirty: true,
            failure: None,
            key: PlayerKey::random(),
            running: None,
            reopen: None,
            retry: Retry::fixed(STUCK, MAX_REOPENS),
            was: None,
            opened_at: None,
            missing: BTreeMap::new(),
            said: Vec::new(),
            vacated: BTreeMap::new(),
            keys: BTreeMap::new(),
        }
    }

    /// Introduce ourselves as `key` (the game: [`PlayerKey::local`]; a
    /// fresh lobby has a random one).
    pub fn with_key(mut self, key: PlayerKey) -> Self {
        self.key = key;
        self
    }

    /// Reopen the connection through `open` when the room is stuck (see
    /// the module doc).
    pub fn with_reopen(mut self, open: Connector) -> Self {
        self.reopen = Some(open);
        self
    }

    /// Back in the room after a match: the match's `transport`, the
    /// `peers` it is still connected to, the room as it was when the
    /// match started. Everyone hears our hello again; those still in
    /// the match ignore it and say hello themselves when they are back
    /// (the host's session ignores a hello from a connection of its match:
    /// see `Session::knock`). `key`: ours, as in the match.
    pub fn resume(
        transport: Box<dyn Transport>,
        peers: &[PeerId],
        name: &str,
        default_map: &str,
        room: Option<RoomState>,
        key: PlayerKey,
    ) -> Self {
        let mut l = Self::new(transport, name, default_map).with_key(key);
        l.room = room;
        let hello = l.hello();
        for &peer in peers {
            l.connected.insert(peer);
            l.send(peer, hello.clone());
        }
        l
    }

    /// Hand the connection over (to the match's session).
    pub fn into_transport(self) -> Box<dyn Transport> {
        self.transport
    }

    /// Why the connection to the room is gone for good (the signaling
    /// server unreachable, the socket closed); the page shows it.
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn me(&self) -> Option<PeerId> {
        self.transport.local_id()
    }

    /// The elected host: the lowest id among us and every connected peer
    /// that introduced itself with a compatible build.
    pub fn host(&self) -> Option<PeerId> {
        let me = self.me()?;
        self.connected
            .iter()
            .copied()
            .filter(|p| self.peers.get(p).is_some_and(PeerInfo::compatible))
            .chain([me])
            .min()
    }

    pub fn is_host(&self) -> bool {
        self.me().is_some() && self.host() == self.me()
    }

    /// The room as this peer sees it (`None` until a host spoke).
    pub fn room(&self) -> Option<&RoomState> {
        self.room.as_ref()
    }

    /// Our seat in the room.
    pub fn my_seat(&self) -> Option<usize> {
        self.room.as_ref()?.seat_of(self.me()?)
    }

    pub fn ready(&self) -> bool {
        self.ready
    }

    /// Connected peers that never introduced themselves: other builds
    /// that cannot even say hello, or peers in a running match.
    pub fn silent(&self) -> Vec<PeerId> {
        self.connected
            .iter()
            .copied()
            .filter(|p| self.peers.get(p).is_none_or(|i| !i.introduced()))
            .collect()
    }

    /// The map whose hash this peer still needs ([`Self::provide_map`]):
    /// a running match's we were asked into, else the room's.
    pub fn map_needed(&self) -> Option<&str> {
        let map = match &self.running {
            Some(r) => &r.start.setup.map,
            None => &self.room.as_ref()?.map,
        };
        (!self.maps.contains_key(map)).then_some(map.as_str())
    }

    /// A running match asked us in and we are checking its map.
    pub fn joining(&self) -> bool {
        self.running.is_some()
    }

    /// Our hash of `map` (`have`), once read.
    pub fn provide_map(&mut self, map: &str, have: MapHave) {
        if self.maps.get(map) != Some(&have) {
            self.maps.insert(map.to_string(), have);
            self.dirty = true;
            self.rejudge = true;
        }
    }

    fn my_map(&self, map: &str) -> MapHave {
        self.maps.get(map).copied().unwrap_or(MapHave::Pending)
    }

    /// Something on view changed since the last call.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn take_events(&mut self) -> Vec<LobbyEvent> {
        std::mem::take(&mut self.events)
    }

    fn send(&mut self, to: PeerId, msg: LobbyMsg) {
        if let Some(bytes) = encode(msg) {
            self.transport.send(to, Channel::Reliable, bytes);
        }
    }

    fn broadcast(&mut self, msg: LobbyMsg) {
        let Some(bytes) = encode(msg) else {
            return;
        };
        for &peer in &self.connected {
            self.transport.send(peer, Channel::Reliable, bytes.clone());
        }
    }

    fn hello(&self) -> LobbyMsg {
        LobbyMsg::Hello(Hello {
            name: self.name.clone(),
            build: self.build,
            room: self.room.clone(),
            key: self.key.0,
        })
    }

    /// Our name in the room.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Go by `name` from now on: everyone hears our hello again.
    pub fn set_name(&mut self, name: &str) {
        let name = clean_name(name);
        if name.is_empty() || name == self.name {
            return;
        }
        self.name = name;
        let hello = self.hello();
        self.broadcast(hello);
        self.dirty = true;
        self.rejudge = true;
    }

    /// Say `text` to the room: through the host, which relays it to
    /// everyone ([`super::chat`]).
    pub fn say(&mut self, text: &str) -> Result<(), String> {
        let text = chat::clean(text);
        if text.is_empty() {
            return Ok(());
        }
        let me = self.me().ok_or("not connected yet")?;
        if self.is_host() {
            self.relay_chat(me, text);
        } else {
            let host = self.host().ok_or("no host")?;
            self.send(host, LobbyMsg::Chat { from: me, text });
        }
        Ok(())
    }

    /// Host: a line from `from` to everyone, and to us.
    fn relay_chat(&mut self, from: PeerId, text: String) {
        self.broadcast(LobbyMsg::Chat {
            from,
            text: text.clone(),
        });
        self.heard_chat(from, text);
    }

    fn heard_chat(&mut self, from: PeerId, text: String) {
        let name = if Some(from) == self.me() {
            self.name.clone()
        } else {
            self.display_name(from)
        };
        self.said.push(ChatLine { from, name, text });
        self.dirty = true;
    }

    /// The chat lines that arrived since the last call.
    pub fn take_chat(&mut self) -> Vec<ChatLine> {
        std::mem::take(&mut self.said)
    }

    /// What the stuck-room check is doing, for the page: the peers the
    /// room names that we have not reached (for a few seconds now), and
    /// whether we gave up reopening.
    pub fn unreached(&self, now: Duration) -> Option<String> {
        let late: Vec<String> = self
            .missing
            .iter()
            .filter(|(_, since)| now.saturating_sub(**since) >= SETTLE)
            .map(|(p, _)| self.display_name(*p))
            .collect();
        if late.is_empty() {
            return None;
        }
        let who = late.join(", ");
        Some(if self.retry.exhausted() || self.reopen.is_none() {
            format!("no connection to {who}: leave and join again, or try another network")
        } else {
            format!("connecting to {who}...")
        })
    }

    /// `peer`'s name as the room (or a hello's room, or its own hello)
    /// gives it.
    fn display_name(&self, peer: PeerId) -> String {
        self.room
            .as_ref()
            .and_then(|r| r.member(peer))
            .map(|m| m.name.clone())
            .or_else(|| self.peers.get(&peer).and_then(|i| i.name.clone()))
            .or_else(|| {
                self.peers
                    .values()
                    .filter_map(|i| i.room.as_ref())
                    .find_map(|r| r.member(peer).map(|m| m.name.clone()))
            })
            .unwrap_or_else(|| peer.to_string())
    }

    /// How often this lobby reopened its connection.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn reopens(&self) -> u32 {
        self.retry.reopens()
    }

    /// Take in the network, (host) keep the room, send what changed.
    /// `now`: any monotonic clock (the [`SETTLE`] wait).
    pub fn poll(&mut self, now: Duration) {
        if self.done {
            return;
        }
        if self.opened_at.is_none() {
            // A fresh connection (or a match's, handed back): the settle
            // wait counts from here.
            self.opened_at = Some(now);
            self.last_join = now;
        }
        if self.failure.is_none() {
            self.failure = self.transport.failure();
            self.dirty |= self.failure.is_some();
        }
        for ev in self.transport.poll_peers() {
            match ev {
                PeerEvent::Joined(peer) => {
                    self.connected.insert(peer);
                    self.last_join = now;
                    let hello = self.hello();
                    self.send(peer, hello);
                    // Our status too: a host elected later needs it.
                    if let Some(status) = self.sent_status.clone() {
                        self.send(peer, LobbyMsg::Status(status));
                    }
                }
                PeerEvent::Left(peer) => {
                    self.connected.remove(&peer);
                    self.peers.remove(&peer);
                }
            }
            self.dirty = true;
            self.rejudge = true;
        }
        // Lobby traffic is reliable; whatever came unreliably is an
        // earlier match's session talking.
        let _ = self.transport.receive(Channel::Unreliable);
        for (from, bytes) in self.transport.receive(Channel::Reliable) {
            if !self.connected.contains(&from) {
                continue;
            }
            match decode(&bytes) {
                Decoded::Msg(msg) => self.handle(from, msg),
                Decoded::Foreign => {
                    let info = self.peers.entry(from).or_default();
                    if !info.foreign {
                        info.foreign = true;
                        self.dirty = true;
                        self.rejudge = true;
                    }
                }
                Decoded::Other => {}
            }
            if self.done {
                return;
            }
        }
        if self.running.is_some() {
            self.join_running();
            return;
        }
        self.settled = self
            .connected
            .iter()
            .all(|p| self.peers.get(p).is_some_and(PeerInfo::introduced))
            || now.saturating_sub(self.last_join) >= SETTLE;
        if self.is_host() {
            if self.settled {
                self.keep_room();
            }
        } else {
            self.send_status();
        }
        self.check_stuck(now);
    }

    /// Everyone the newest room we know of names (its host and members):
    /// ours, or one a connected peer showed us in its hello.
    fn expected(&self) -> BTreeSet<PeerId> {
        let newest = self
            .connected
            .iter()
            .filter_map(|p| self.peers.get(p)?.room.as_ref())
            .chain(self.room.as_ref())
            .max_by_key(|r| r.rev);
        newest.map_or_else(BTreeSet::new, |r| {
            r.members.iter().map(|m| m.peer).chain([r.host]).collect()
        })
    }

    /// The stuck-room check (module doc): note who the room names that
    /// we have not reached; past [`STUCK`] for one with a lower id,
    /// reopen.
    fn check_stuck(&mut self, now: Duration) {
        let Some(me) = self.me() else {
            return;
        };
        let mut expected = self.expected();
        expected.remove(&me);
        if let Some(was) = self.was {
            expected.remove(&was);
        }
        let unreached: BTreeSet<PeerId> = expected
            .into_iter()
            .filter(|p| !self.connected.contains(p))
            .collect();
        let before = self.missing.len();
        self.missing.retain(|p, _| unreached.contains(p));
        for p in unreached {
            self.missing.entry(p).or_insert(now);
        }
        self.dirty |= self.missing.len() != before;
        // Missing since it was, or since our connection opened.
        let opened = self.opened_at.unwrap_or(now);
        let stuck = self
            .missing
            .iter()
            .find(|(p, since)| **p < me && self.retry.due((**since).max(opened), now))
            .map(|(p, _)| *p);
        if let Some(peer) = stuck
            && self.reopen.is_some()
        {
            let after = self.retry.patience();
            info!("no connection to {peer} after {after:?}: reopening the room");
            self.reopen_now();
        }
    }

    /// Drop the connection and open a new one (a new id; the others see
    /// the old one leave). The room as we knew it goes along in our
    /// hellos; the host gives our seat to the new id (the same
    /// [`Hello::key`]).
    pub fn reopen_now(&mut self) {
        let Some(open) = self.reopen.as_mut() else {
            return;
        };
        let old = self.transport.local_id();
        self.transport = open();
        self.was = old.or(self.was);
        self.retry.reopened();
        self.connected.clear();
        self.peers.clear();
        self.missing.clear();
        self.sent = None;
        self.sent_status = None;
        self.settled = false;
        self.opened_at = None;
        self.dirty = true;
        self.rejudge = true;
    }

    fn handle(&mut self, from: PeerId, msg: LobbyMsg) {
        match msg {
            LobbyMsg::Hello(h) => {
                let mismatch = self.build.mismatch(&h.build);
                let info = self.peers.entry(from).or_default();
                let first = !info.introduced();
                info.name = Some(clean_name(&h.name));
                info.incompatible = mismatch;
                info.room = h.room.clone();
                self.keys.insert(from, PlayerKey(h.key));
                if info.compatible()
                    && let Some(room) = h.room
                    && self.offered.as_ref().is_none_or(|o| room.rev > o.rev)
                {
                    self.offered = Some(room);
                }
                if first {
                    // Our hello may have reached it while it was in a
                    // match (dropped there): it hears ours again.
                    let hello = self.hello();
                    self.send(from, hello);
                    if let Some(status) = self.sent_status.clone() {
                        self.send(from, LobbyMsg::Status(status));
                    }
                }
                if self.is_host()
                    && let Some(room) = self.room.as_ref()
                    && room.seat_of(from).is_none()
                    && let Some(seat) = self.seat_of_key(PlayerKey(h.key), from)
                    && let Some(room) = self.room.as_mut()
                {
                    // It reopened its connection (the same key, a new
                    // id): its seat follows it, whether its old id is
                    // still here or already gone.
                    room.seats[seat].who = Occupant::Human(from);
                }
                self.dirty = true;
                self.rejudge = true;
            }
            LobbyMsg::Status(s) => {
                self.peers.entry(from).or_default().status = Some(s);
                self.dirty = true;
                self.rejudge = true;
            }
            LobbyMsg::Room(room) => {
                if Some(from) != self.host() || self.is_host() {
                    return;
                }
                if self.room.as_ref().is_none_or(|r| r.map != room.map) {
                    // A new map: ready was for the old one.
                    self.ready = false;
                }
                self.room = Some(room);
                self.sent = None;
                self.dirty = true;
                self.rejudge = true;
            }
            LobbyMsg::Sit { seat } if self.is_host() => self.sit(from, seat as usize),
            LobbyMsg::Stand if self.is_host() => self.stand(from),
            LobbyMsg::Faction { faction } if self.is_host() => {
                if let Some(room) = self.room.as_mut()
                    && let Some(i) = room.seat_of(from)
                {
                    room.seats[i].faction = faction % Faction::ALL.len() as u8;
                }
            }
            LobbyMsg::Start(start) => {
                if Some(from) == self.host() && !self.is_host() {
                    self.started(&start);
                }
            }
            // A match runs in this room (its peers' sessions never say
            // hello): its host asks us in.
            LobbyMsg::Running(running) => {
                if running.host == from && self.running.is_none() {
                    info!(
                        "a match runs in this room ({} turns in): joining",
                        running.newest
                    );
                    self.running = Some(*running);
                    self.dirty = true;
                }
            }
            // Spring drops a line that claims another sender
            // (`GameServer.cpp:1150`).
            LobbyMsg::Chat { from: said, text } if self.is_host() && said == from => {
                let text = chat::clean(&text);
                if !text.is_empty() {
                    self.relay_chat(from, text);
                }
            }
            LobbyMsg::Chat { from: said, text } if Some(from) == self.host() => {
                self.heard_chat(said, chat::clean(&text));
            }
            LobbyMsg::Sit { .. }
            | LobbyMsg::Stand
            | LobbyMsg::Faction { .. }
            | LobbyMsg::Chat { .. } => {}
        }
    }

    /// The running match we were asked into, once our hash of its map is
    /// in: join it late, or refuse with the reason (as at a `Start`).
    fn join_running(&mut self) {
        let Some(me) = self.me() else {
            return;
        };
        let Some(running) = self.running.as_ref() else {
            return;
        };
        if self.my_map(&running.start.setup.map) == MapHave::Pending {
            return;
        }
        let Some(running) = self.running.take() else {
            return;
        };
        self.accept_start(
            &running.start,
            "A match runs in this room, without you.".to_string(),
            || MatchStart::late(&running, me),
        );
    }

    /// Guest: the host's `Start`. Join the match if our stamp is the
    /// host's, else refuse and leave.
    fn started(&mut self, start: &StartMsg) {
        let Some(me) = self.me() else {
            return;
        };
        let absent = match self.room.as_ref().and_then(|r| r.member(me)) {
            Some(Member {
                check: Check::Refused(why),
                ..
            }) => format!("The match started without you: you {why}."),
            _ => "The match started without you.".to_string(),
        };
        self.accept_start(start, absent, || MatchStart::of(start, me));
    }

    /// The room is over for us: the match `start` describes begins
    /// (`make` it), or we close with why not (`absent`: not in its
    /// roster).
    fn accept_start(
        &mut self,
        start: &StartMsg,
        absent: String,
        make: impl FnOnce() -> MatchStart,
    ) {
        self.done = true;
        let event = if !start.roster.iter().any(|(p, _)| Some(*p) == self.me()) {
            LobbyEvent::Closed(absent)
        } else if let Some(why) = self.refusal(start) {
            warn!("refused the match: {why}");
            LobbyEvent::Closed(why)
        } else {
            LobbyEvent::Start(Box::new(make()))
        };
        self.events.push(event);
    }

    /// Why this peer cannot play the match `start` describes (another
    /// build, another copy of the map), if it cannot.
    fn refusal(&self, start: &StartMsg) -> Option<String> {
        let map = &start.setup.map;
        if let Some(why) = self.build.mismatch(&start.compat.build()) {
            Some(format!("The host {why}."))
        } else {
            match self.my_map(map) {
                MapHave::Hash(h) if h == start.compat.map_hash => None,
                MapHave::Hash(_) => {
                    Some(format!("Your copy of map {map} differs from the host's."))
                }
                MapHave::Missing => Some(format!("You don't have map {map}.")),
                MapHave::Pending => Some(format!("Map {map} was not checked yet.")),
            }
        }
    }

    /// Guest: tell everyone our status when it changed.
    fn send_status(&mut self) {
        let Some(room) = self.room.as_ref() else {
            return;
        };
        let have = self.my_map(&room.map);
        let ready = self.ready && matches!(have, MapHave::Hash(_));
        if self
            .sent_status
            .as_ref()
            .is_some_and(|s| s.ready == ready && s.have == have && s.map == room.map)
        {
            return;
        }
        let status = Status {
            ready,
            map: room.map.clone(),
            have,
        };
        self.sent_status = Some(status.clone());
        self.broadcast(LobbyMsg::Status(status));
    }

    /// Host: bring the room up to date with who is here, and send it if
    /// it changed.
    fn keep_room(&mut self) {
        let Some(me) = self.me() else {
            return;
        };
        let rejudge = std::mem::take(&mut self.rejudge) || self.sent.is_none();
        let mut room = match self.room.take() {
            // Newly elected: take over the newest room anyone showed us.
            Some(r) if self.sent.is_some() => r,
            r => [r, self.offered.take()]
                .into_iter()
                .flatten()
                .max_by_key(|r| r.rev)
                .unwrap_or_else(|| RoomState::new(me, &self.default_map)),
        };
        room.host = me;
        if rejudge {
            self.judge_members(me, &mut room);
        }
        if self.sent.as_ref().is_none_or(|s| !s.same(&room)) {
            room.rev = room.rev.max(self.sent.as_ref().map_or(0, |s| s.rev)) + 1;
            self.sent = Some(room.clone());
            self.dirty = true;
            self.broadcast(LobbyMsg::Room(room.clone()));
        }
        self.room = Some(room);
    }

    /// Host: the members of `room` as they are now (who is here, their
    /// names, readiness and checks), and what follows for the seats: a
    /// seat whose player may no longer sit opens, a newcomer takes the
    /// first open one.
    fn judge_members(&mut self, me: PeerId, room: &mut RoomState) {
        let before: BTreeSet<PeerId> = room.members.iter().map(|m| m.peer).collect();
        let mut members = vec![self.judge(me, &room.map)];
        for &peer in &self.connected {
            if self.peers.get(&peer).is_some_and(PeerInfo::introduced) {
                members.push(self.judge(peer, &room.map));
            } else if let Some(m) = room.member(peer) {
                // Still connected but silent: in a match still (or just
                // arriving). It stays a member, not ready.
                members.push(Member {
                    ready: false,
                    check: Check::Pending,
                    ..m.clone()
                });
            }
        }
        members.sort_by_key(|m| m.peer);
        // Another build cannot sit; a missing or different map can be
        // fixed (or the map changed), so it only blocks the start. A
        // connected member that has not said hello again is still in a
        // match: its seat waits for it.
        let seatable: BTreeSet<PeerId> = members
            .iter()
            .map(|m| m.peer)
            .filter(|p| self.may_sit(*p) || self.away(*p))
            .collect();
        for (i, seat) in room.seats.iter_mut().enumerate() {
            if let Occupant::Human(p) = seat.who
                && !seatable.contains(&p)
            {
                seat.who = Occupant::Open;
                self.vacated.insert(p, i);
            }
        }
        // Newcomers take the first open seats.
        for m in &members {
            if !before.contains(&m.peer)
                && seatable.contains(&m.peer)
                && room.seat_of(m.peer).is_none()
                && let Some(seat) = room.seats.iter_mut().find(|s| s.who == Occupant::Open)
            {
                seat.who = Occupant::Human(m.peer);
            }
        }
        room.members = members;
    }

    /// May `peer` take a seat: us, or a compatible peer.
    fn may_sit(&self, peer: PeerId) -> bool {
        Some(peer) == self.me() || self.peers.get(&peer).is_some_and(PeerInfo::compatible)
    }

    /// Host: the seat a connection with `key` other than `from` held —
    /// sits on, or had until its peer left ([`Self::vacated`], if still
    /// open). Never the host's own seat; never for the "none" key.
    fn seat_of_key(&self, key: PlayerKey, from: PeerId) -> Option<usize> {
        if key == PlayerKey::default() {
            return None;
        }
        let me = self.me();
        let room = self.room.as_ref()?;
        let old: Vec<PeerId> = self
            .keys
            .iter()
            .filter(|(p, k)| **k == key && **p != from && Some(**p) != me)
            .map(|(p, _)| *p)
            .collect();
        old.iter().find_map(|p| room.seat_of(*p)).or_else(|| {
            old.iter().find_map(|p| {
                self.vacated
                    .get(p)
                    .copied()
                    .filter(|&s| room.seats.get(s).is_some_and(|s| s.who == Occupant::Open))
            })
        })
    }

    /// `peer` is connected but has not said hello (to this lobby): in a
    /// match still, or just arriving.
    pub fn away(&self, peer: PeerId) -> bool {
        self.connected.contains(&peer) && self.peers.get(&peer).is_none_or(|i| !i.introduced())
    }

    /// Host: `peer` as a member of a room on `map`.
    fn judge(&self, peer: PeerId, map: &str) -> Member {
        let me = self.me() == Some(peer);
        let info = self.peers.get(&peer);
        let name = if me {
            self.name.clone()
        } else {
            info.and_then(|i| i.name.clone()).unwrap_or_default()
        };
        let name = if name.is_empty() {
            peer.to_string()
        } else {
            name
        };
        let mine = self.my_map(map);
        let theirs = if me {
            Some(mine)
        } else {
            info.and_then(|i| i.status.as_ref())
                .filter(|s| s.map == map)
                .map(|s| s.have)
        };
        let check = if let Some(why) = info.and_then(|i| i.incompatible.clone()) {
            Check::Refused(why)
        } else if info.is_some_and(|i| i.foreign) {
            Check::Refused("runs another version of the game".into())
        } else {
            match (mine, theirs) {
                (_, Some(MapHave::Missing)) => Check::Refused(format!("has no copy of map {map}")),
                (MapHave::Hash(a), Some(MapHave::Hash(b))) if a != b => {
                    Check::Refused(format!("has a different copy of map {map}"))
                }
                (MapHave::Hash(_), Some(MapHave::Hash(_))) => Check::Ok,
                _ => Check::Pending,
            }
        };
        let ready = me
            || info
                .and_then(|i| i.status.as_ref())
                .is_some_and(|s| s.ready && s.map == map);
        Member {
            peer,
            name,
            ready,
            check,
        }
    }

    /// Host: put `peer` on `seat` if it is open.
    fn sit(&mut self, peer: PeerId, seat: usize) {
        if !self.may_sit(peer) {
            return;
        }
        let Some(room) = self.room.as_mut() else {
            return;
        };
        if room.seats.get(seat).is_none_or(|s| s.who != Occupant::Open) {
            return;
        }
        if let Some(old) = room.seat_of(peer) {
            room.seats[old].who = Occupant::Open;
        }
        room.seats[seat].who = Occupant::Human(peer);
    }

    /// Host: take `peer` off its seat.
    fn stand(&mut self, peer: PeerId) {
        if let Some(room) = self.room.as_mut()
            && let Some(old) = room.seat_of(peer)
        {
            room.seats[old].who = Occupant::Open;
        }
    }

    // --- what the player does --------------------------------------------

    /// Guest: ready (or not) for the room's map.
    pub fn set_ready(&mut self, ready: bool) {
        if self.ready != ready {
            self.ready = ready;
            self.dirty = true;
        }
    }

    /// The seat's occupant: the host cycles it (open → AI → each member
    /// → open), a guest sits down on an open seat or stands up from its
    /// own.
    pub fn occupant(&mut self, seat: usize) -> Result<(), String> {
        let me = self.me().ok_or("not connected yet")?;
        let room = self.room.as_ref().ok_or("no room yet")?;
        let current = room.seats.get(seat).ok_or("no such seat")?.who;
        if !self.is_host() {
            let host = self.host().ok_or("no host")?;
            match current {
                Occupant::Open => self.send(host, LobbyMsg::Sit { seat: seat as u8 }),
                Occupant::Human(p) if p == me => self.send(host, LobbyMsg::Stand),
                _ => return Err("only the host changes other seats".into()),
            }
            return Ok(());
        }
        let mut cycle = vec![Occupant::Open, Occupant::Ai];
        cycle.extend(
            room.members
                .iter()
                .filter(|m| self.may_sit(m.peer) && !matches!(m.check, Check::Refused(_)))
                .map(|m| Occupant::Human(m.peer)),
        );
        let next = cycle
            .iter()
            .position(|o| *o == current)
            .map_or(Occupant::Open, |i| cycle[(i + 1) % cycle.len()]);
        let room = self.room.as_mut().expect("checked above");
        if let Occupant::Human(p) = next
            && let Some(old) = room.seat_of(p)
        {
            room.seats[old].who = Occupant::Open;
        }
        room.seats[seat].who = next;
        Ok(())
    }

    /// The seat's faction, cycled: the host's for any seat, a guest's for
    /// its own.
    pub fn faction(&mut self, seat: usize) -> Result<(), String> {
        let me = self.me().ok_or("not connected yet")?;
        let host = self.host().ok_or("no host")?;
        let room = self.room.as_mut().ok_or("no room yet")?;
        let spec = room.seats.get_mut(seat).ok_or("no such seat")?;
        let next = (spec.faction + 1) % Faction::ALL.len() as u8;
        if host == me {
            spec.faction = next;
        } else if spec.who == Occupant::Human(me) {
            self.send(host, LobbyMsg::Faction { faction: next });
        } else {
            return Err("only the host changes other seats".into());
        }
        Ok(())
    }

    /// Host: the seat's ally team, cycled through one per seat.
    pub fn ally_team(&mut self, seat: usize) -> Result<(), String> {
        let room = self.host_room()?;
        let n = room.seats.len() as u8;
        let spec = room.seats.get_mut(seat).ok_or("no such seat")?;
        spec.ally_team = (spec.ally_team + 1) % n.max(1);
        Ok(())
    }

    /// Host: one more (open) seat, up to [`MAX_SEATS`]; its team
    /// alternates.
    pub fn add_seat(&mut self) -> Result<(), String> {
        let room = self.host_room()?;
        if room.seats.len() >= MAX_SEATS {
            return Err(format!("a match seats at most {MAX_SEATS}"));
        }
        let n = room.seats.len();
        room.seats.push(SeatSpec {
            faction: (n % Faction::ALL.len()) as u8,
            ally_team: (n % 2) as u8,
            who: Occupant::Open,
        });
        Ok(())
    }

    /// Host: drop the last seat (its player spectates), down to two.
    pub fn remove_seat(&mut self) -> Result<(), String> {
        let room = self.host_room()?;
        if room.seats.len() <= 2 {
            return Err("a match needs two seats".into());
        }
        room.seats.pop();
        Ok(())
    }

    /// Host: play on `map` (a catalog stem).
    pub fn set_map(&mut self, map: &str) -> Result<(), String> {
        let room = self.host_room()?;
        room.map = map.to_string();
        Ok(())
    }

    /// Host: whether every player may pause (off: only the host).
    pub fn set_pausable(&mut self, pausable: bool) -> Result<(), String> {
        self.host_room()?.rules.pausable = pausable;
        Ok(())
    }

    /// Host: cheats for everyone (off: only with a single human seat).
    pub fn set_cheats(&mut self, cheats: bool) -> Result<(), String> {
        self.host_room()?.rules.cheats = cheats;
        Ok(())
    }

    /// Host: the AI difficulty, 1 Easy … 4 Extreme.
    pub fn set_difficulty(&mut self, difficulty: u8) -> Result<(), String> {
        let room = self.host_room()?;
        room.difficulty = difficulty.clamp(1, 4);
        Ok(())
    }

    fn host_room(&mut self) -> Result<&mut RoomState, String> {
        if !self.is_host() {
            return Err("only the host changes the match".into());
        }
        // A new map changes every member's check.
        self.rejudge = true;
        self.room.as_mut().ok_or_else(|| "no room yet".into())
    }

    /// Host: why the match cannot start now (`None`: it can).
    pub fn start_blocker(&self) -> Option<String> {
        self.startable().err()
    }

    /// Host: the room, cleared to start, or why it cannot.
    fn startable(&self) -> Result<Startable<'_>, String> {
        let Some(me) = self.me().filter(|_| self.is_host()) else {
            return Err("only the host starts the match".into());
        };
        let Some(room) = self.room.as_ref() else {
            return Err("setting up the room...".into());
        };
        if !self.settled {
            return Err("waiting for everyone to say hello...".into());
        }
        let seated: Vec<&SeatSpec> = room
            .seats
            .iter()
            .filter(|s| s.who != Occupant::Open)
            .collect();
        if seated.len() < 2 {
            return Err("seat at least two players or AIs".into());
        }
        let teams: BTreeSet<u8> = seated.iter().map(|s| s.ally_team).collect();
        if teams.len() < 2 {
            return Err("put the seats on at least two teams".into());
        }
        let map_hash = match self.my_map(&room.map) {
            MapHave::Hash(h) => h,
            MapHave::Pending => return Err(format!("reading map {}...", room.map)),
            MapHave::Missing => return Err(format!("you have no map {}", room.map)),
        };
        for s in &seated {
            let Occupant::Human(p) = s.who else {
                continue;
            };
            let Some(m) = room.member(p) else {
                return Err("a seated player left".into());
            };
            if p == me {
                continue;
            }
            if self.away(p) {
                return Err(format!("waiting for {} to come back to the room", m.name));
            }
            match &m.check {
                Check::Refused(why) => return Err(format!("{} {why}", m.name)),
                Check::Pending => return Err(format!("checking {}'s map...", m.name)),
                Check::Ok if !m.ready => {
                    return Err(format!("waiting for {} to be ready", m.name));
                }
                Check::Ok => {}
            }
        }
        Ok(Startable {
            me,
            room,
            seated,
            map_hash,
        })
    }

    /// Host: start the match with `seed`, as `match_id`. Every member
    /// cleared to play gets the `Start`; the caller takes the transport
    /// ([`Self::into_transport`]).
    pub fn start(&mut self, seed: u64, match_id: u64) -> Result<MatchStart, String> {
        let Startable {
            me,
            room,
            seated,
            map_hash,
        } = self.startable()?;
        // Open seats drop out; seat `i` of the match is owner `i`.
        let setup = encode_setup(&GameSetup {
            map: room.map.clone(),
            players: seated
                .iter()
                .map(|s| PlayerSpec {
                    faction: s.faction(),
                    ally_team: s.ally_team,
                    ai: s.who == Occupant::Ai,
                })
                .collect(),
            difficulty: room.difficulty,
            demo: false,
            showcase: None,
            sandbox: false,
            seed,
            rules: room.rules,
        });
        let roster: Vec<(PeerId, Option<u8>)> = room
            .members
            .iter()
            .filter(|m| m.peer == me || m.check == Check::Ok)
            .map(|m| {
                let seat = seated
                    .iter()
                    .position(|s| s.who == Occupant::Human(m.peer))
                    .map(|i| i as u8);
                (m.peer, seat)
            })
            .collect();
        // The door for late joiners: every player's key and name.
        let keys = roster
            .iter()
            .map(|&(p, _)| {
                let key = if p == me {
                    self.key
                } else {
                    self.keys.get(&p).copied().unwrap_or_default()
                };
                (p, key)
            })
            .collect();
        let names: BTreeMap<PeerId, String> = room
            .members
            .iter()
            .map(|m| (m.peer, m.name.clone()))
            .collect();
        let msg = StartMsg {
            match_id,
            setup,
            roster,
            compat: Compat::of(self.build, map_hash),
        };
        self.broadcast(LobbyMsg::Start(msg.clone()));
        self.done = true;
        Ok(MatchStart {
            door: Some(Door {
                start: msg.clone(),
                keys,
                names: names.clone(),
                in_room: BTreeSet::new(),
            }),
            names: names.into_iter().collect(),
            ..MatchStart::of(&msg, me)
        })
    }
}

/// A room cleared to start ([`Lobby::startable`]): us, the room, its
/// seated seats (the match's owners, in order) and our hash of its map.
struct Startable<'a> {
    me: PeerId,
    room: &'a RoomState,
    seated: Vec<&'a SeatSpec>,
    map_hash: u64,
}

// --- the game side ------------------------------------------------------

/// The lobby as the menu sees it: the room this game is in (if any),
/// the room code being typed, and the last thing worth telling.
#[derive(Resource, Default)]
pub struct LobbyRoom {
    pub lobby: Option<Lobby>,
    pub room: Option<RoomId>,
    /// The Multiplayer page's room code field.
    pub code: String,
    /// The last refusal or closing reason.
    pub notice: Option<String>,
    /// Our name in rooms.
    pub name: String,
    /// The text field keys go to on the Multiplayer page.
    pub field: Field,
    /// The chat line being typed in the room.
    pub chat: String,
    /// The stuck-room check's word, if any ([`Lobby::unreached`]).
    pub unreached: Option<String>,
    /// The room this game's last networked match came from, as it was
    /// at the start: "Back to room" resumes it ([`Lobby::resume`]).
    pub last: Option<(RoomId, Option<RoomState>)>,
    #[cfg(target_arch = "wasm32")]
    maps: MapHashes,
}

impl LobbyRoom {
    /// In `room` from now on, through `lobby`.
    fn open(&mut self, room: RoomId, lobby: Lobby) {
        #[cfg(target_arch = "wasm32")]
        web::share_room(Some(&room));
        self.lobby = Some(lobby);
        self.room = Some(room);
        self.notice = None;
        self.field = Field::Main;
    }

    /// Out of the room (dropping the lobby leaves it).
    fn close(&mut self) {
        self.lobby = None;
        self.room = None;
        #[cfg(target_arch = "wasm32")]
        web::share_room(None);
    }
}

/// The Multiplayer page's text fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    /// The room code (outside a room) or the chat line (in one).
    #[default]
    Main,
    /// The player's name.
    Name,
}

/// What the Multiplayer page asks of the lobby (via the menu's
/// buttons and the launch arguments).
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyAction {
    /// Open a fresh room.
    Create,
    /// Join the room typed in [`LobbyRoom::code`].
    Join,
    Leave,
    Start,
    Ready(bool),
    /// Cycle a seat's occupant (host) / sit down or stand up (guest).
    Occupant(u8),
    Faction(u8),
    AllyTeam(u8),
    AddSeat,
    RemoveSeat,
    /// Host: step the map through the catalog.
    Map(i32),
    Difficulty(u8),
    /// Host: [`MatchRules::pausable`].
    Pausable(bool),
    /// Host: [`MatchRules::cheats`].
    Cheats(bool),
    /// Type into this field from now on.
    Focus(Field),
}

/// Web: the map fetches the lobby's hashes wait for.
#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct MapHashes {
    loading: BTreeMap<String, Handle<crate::map_loading::BytesAsset>>,
}

pub struct LobbyPlugin;

impl Plugin for LobbyPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LobbyRoom>()
            .init_resource::<RoomConnector>()
            .init_resource::<super::chat::ChatLog>()
            .add_message::<LobbyAction>()
            .add_systems(Startup, join_launch_room)
            // After `Update`: a `Start` applies its `GameSetup` after the
            // menu's demo director wrote any of its own this frame.
            .add_systems(
                PostUpdate,
                (lobby_actions, back_to_room, pump_lobby)
                    .chain()
                    .run_if(in_state(AppState::Menu)),
            )
            .add_systems(OnEnter(AppState::InGame), leave_for_other_match);
    }
}

/// A name for rooms: `KP_NAME` (native), else a random kernel word.
fn player_name() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(name) = std::env::var("KP_NAME")
        && !clean_name(&name).is_empty()
    {
        return clean_name(&name);
    }
    const WORDS: &[&str] = &[
        "daemon", "socket", "pointer", "thread", "buffer", "cache", "packet", "vector", "signal",
        "kernel", "inode", "mutex", "pipe", "stack", "heap", "trap",
    ];
    let r = crate::game_setup::random_seed();
    format!(
        "{}{}",
        WORDS[(r % WORDS.len() as u64) as usize],
        (r >> 32) % 100
    )
}

/// The room the game was launched into: `#room=<code>` in the page's
/// URL (web), `--room <code>` or `KP_ROOM` (native).
fn launch_room() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        fragment_param(&web::read_fragment()?, "room")
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let args: Vec<String> = std::env::args().collect();
        args.iter()
            .position(|a| a == "--room")
            .and_then(|i| args.get(i + 1).cloned())
            .or_else(|| std::env::var("KP_ROOM").ok())
    }
}

/// Startup: a launch room opens the Multiplayer page and joins it.
fn join_launch_room(
    mut lobby: ResMut<LobbyRoom>,
    mut page: ResMut<crate::ui::menu::MenuPage>,
    mut actions: MessageWriter<LobbyAction>,
) {
    lobby.name = player_name();
    if let Some(code) = launch_room() {
        info!("launched into room {code}");
        lobby.code = code;
        *page = crate::ui::menu::MenuPage::Multiplayer;
        actions.write(LobbyAction::Join);
    }
}

/// Entering a match the lobby did not start (single player from
/// another page) leaves the room.
fn leave_for_other_match(mut lobby: ResMut<LobbyRoom>) {
    if lobby.lobby.is_some() {
        info!("left room {:?} for another match", lobby.room);
        lobby.close();
    }
}

/// The map a room this game ends up hosting starts on: the weighted
/// pick of single player's random map, if this catalog has it.
fn default_map(catalog: &crate::map_loading::MapCatalog) -> String {
    let names = catalog.names();
    let pick = crate::game_setup::random_weighted_map(2);
    names
        .iter()
        .find(|n| **n == pick)
        .or(names.first())
        .cloned()
        .unwrap_or_default()
}

fn enter(
    lobby: &mut LobbyRoom,
    room: RoomId,
    catalog: &crate::map_loading::MapCatalog,
    connector: &RoomConnector,
) {
    info!("entering room {room} as {}", lobby.name);
    let mut open = connector.to(room.clone());
    let l = Lobby::new(open(), &lobby.name, &default_map(catalog))
        .with_key(PlayerKey::local())
        .with_reopen(open);
    lobby.open(room, l);
}

/// A match handed its connection back ("Back to room"): into the room
/// it came from, as it was at the start ([`Lobby::resume`]). Without a
/// room to go back to (not a lobby's match) the connection closes.
fn back_to_room(world: &mut World) {
    if !world.contains_resource::<super::driver::BackToRoom>() {
        return;
    }
    let Some(map) = world
        .get_resource::<crate::map_loading::MapCatalog>()
        .map(default_map)
    else {
        return;
    };
    let Some(super::driver::BackToRoom { transport, peers }) =
        world.remove_resource::<super::driver::BackToRoom>()
    else {
        return;
    };
    let connector = world.resource::<RoomConnector>().clone();
    let mut lobby = world.resource_mut::<LobbyRoom>();
    let lobby = &mut *lobby;
    let Some((room, state)) = lobby.last.clone() else {
        return;
    };
    if lobby.lobby.is_some() {
        return;
    }
    info!("back in room {room} with {} peers", peers.len());
    let open = connector.to(room.clone());
    let l = Lobby::resume(
        transport,
        &peers,
        &lobby.name,
        &map,
        state,
        PlayerKey::local(),
    )
    .with_reopen(open);
    lobby.open(room, l);
}

fn lobby_actions(
    mut actions: MessageReader<LobbyAction>,
    mut lobby: ResMut<LobbyRoom>,
    catalog: Option<Res<crate::map_loading::MapCatalog>>,
    connector: Res<RoomConnector>,
    mut launch: Launch,
) {
    let Some(catalog) = catalog else {
        return;
    };
    for &action in actions.read() {
        let lobby = &mut *lobby;
        let result = match (action, lobby.lobby.as_mut()) {
            (LobbyAction::Focus(field), l) => {
                if lobby.field == Field::Name
                    && field != Field::Name
                    && let Some(l) = l
                {
                    // Clicking away keeps the name typed so far.
                    l.set_name(&lobby.name);
                    lobby.name = l.name().to_string();
                }
                lobby.field = field;
                Ok(())
            }
            (LobbyAction::Create, None) => {
                let room = RoomId::random();
                lobby.code = room.to_string();
                enter(lobby, room, &catalog, &connector);
                Ok(())
            }
            (LobbyAction::Join, None) => match lobby.code.parse::<RoomId>() {
                Ok(room) => {
                    enter(lobby, room, &catalog, &connector);
                    Ok(())
                }
                Err(why) => Err(why.to_string()),
            },
            (LobbyAction::Create | LobbyAction::Join, Some(_)) => Ok(()),
            (LobbyAction::Leave, _) => {
                lobby.close();
                lobby.last = None;
                Ok(())
            }
            (_, None) => Ok(()),
            (LobbyAction::Start, Some(l)) => {
                let seed = crate::game_setup::random_seed();
                match l.start(seed, crate::game_setup::random_seed() | 1) {
                    Ok(start) => {
                        launch.start(lobby, start);
                        continue;
                    }
                    Err(why) => Err(why),
                }
            }
            (LobbyAction::Ready(on), Some(l)) => {
                l.set_ready(on);
                Ok(())
            }
            (LobbyAction::Occupant(seat), Some(l)) => l.occupant(seat as usize),
            (LobbyAction::Faction(seat), Some(l)) => l.faction(seat as usize),
            (LobbyAction::AllyTeam(seat), Some(l)) => l.ally_team(seat as usize),
            (LobbyAction::AddSeat, Some(l)) => l.add_seat(),
            (LobbyAction::RemoveSeat, Some(l)) => l.remove_seat(),
            (LobbyAction::Map(step), Some(l)) => {
                let names = catalog.names();
                let current = l
                    .room()
                    .and_then(|r| names.iter().position(|n| *n == r.map));
                match current {
                    _ if names.is_empty() => Ok(()),
                    Some(i) => {
                        let next = (i as i32 + step).rem_euclid(names.len() as i32) as usize;
                        l.set_map(&names[next])
                    }
                    None => l.set_map(&names[0]),
                }
            }
            (LobbyAction::Difficulty(d), Some(l)) => l.set_difficulty(d),
            (LobbyAction::Pausable(on), Some(l)) => l.set_pausable(on),
            (LobbyAction::Cheats(on), Some(l)) => l.set_cheats(on),
        };
        lobby.notice = result.err();
    }
}

/// What starting a networked match touches.
#[derive(bevy::ecs::system::SystemParam)]
struct Launch<'w, 's> {
    commands: Commands<'w, 's>,
    app_state: ResMut<'w, NextState<AppState>>,
    page: ResMut<'w, crate::ui::menu::MenuPage>,
}

impl Launch<'_, '_> {
    /// Leave the menu for the match `start` describes, over the room's
    /// transport: its setup is the next match's ([`GameSetup`]), its
    /// session waits in [`NextNetMatch`] for the swap.
    fn start(&mut self, lobby: &mut LobbyRoom, start: MatchStart) {
        let Some(l) = lobby.lobby.take() else {
            return;
        };
        info!(
            "starting match {:x} on {}: {} seats, seat {:?}",
            start.session.match_id,
            start.setup.map,
            start.setup.players.len(),
            start.session.roster.get(&start.session.me)
        );
        // "Back to room" comes back to this room as it is now.
        lobby.last = lobby.room.take().map(|code| (code, l.room().cloned()));
        // The player list's names: the start's (a running match's, or
        // the host's room), else the room's as this peer last saw it.
        let names = if start.names.is_empty() {
            l.room()
                .map(|r| r.members.iter().map(|m| (m.peer, m.name.clone())).collect())
                .unwrap_or_default()
        } else {
            start.names.iter().cloned().collect()
        };
        self.commands.insert_resource(PeerNames(names));
        self.commands.insert_resource(start.setup);
        self.commands.insert_resource(NextNetMatch {
            config: start.session,
            transport: l.into_transport(),
            catch_up: CatchUp::default(),
            join: start.join,
            door: start.door,
        });
        self.app_state.set(AppState::InGame);
        *self.page = crate::ui::menu::MenuPage::Main;
    }
}

fn pump_lobby(
    mut lobby: ResMut<LobbyRoom>,
    mut chat_log: ResMut<super::chat::ChatLog>,
    time: Res<Time<Real>>,
    catalog: Option<Res<crate::map_loading::MapCatalog>>,
    #[cfg(target_arch = "wasm32")] server: Res<AssetServer>,
    #[cfg(target_arch = "wasm32")] bytes: Res<Assets<crate::map_loading::BytesAsset>>,
    mut launch: Launch,
) {
    let room = lobby.bypass_change_detection();
    let Some(l) = room.lobby.as_mut() else {
        return;
    };
    l.poll(time.elapsed());
    if let (Some(map), Some(catalog)) = (l.map_needed().map(str::to_string), catalog.as_deref()) {
        #[cfg(not(target_arch = "wasm32"))]
        let have = Some(read_map_hash(catalog, &map));
        #[cfg(target_arch = "wasm32")]
        let have = room.maps.fetch(catalog, &map, &server, &bytes);
        if let Some(have) = have {
            l.provide_map(&map, have);
        }
    }
    let mut changed = l.take_dirty();
    for line in l.take_chat() {
        chat_log.push(time.elapsed(), line);
    }
    let unreached = l.unreached(time.elapsed());
    if unreached != room.unreached {
        room.unreached = unreached;
        changed = true;
    }
    for ev in l.take_events() {
        changed = true;
        match ev {
            LobbyEvent::Start(start) => launch.start(room, *start),
            LobbyEvent::Closed(why) => {
                room.close();
                room.notice = Some(why);
            }
        }
    }
    if changed {
        lobby.set_changed();
    }
}

/// The catalog file of `map` (a file stem).
fn catalog_file<'a>(
    catalog: &'a crate::map_loading::MapCatalog,
    map: &str,
) -> Option<&'a std::path::PathBuf> {
    catalog
        .0
        .iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy() == map))
}

/// Native: the catalog file of `map`, hashed.
#[cfg(not(target_arch = "wasm32"))]
fn read_map_hash(catalog: &crate::map_loading::MapCatalog, map: &str) -> MapHave {
    match catalog_file(catalog, map).map(std::fs::read) {
        Some(Ok(bytes)) => MapHave::Hash(super::compat::map_hash(&bytes)),
        Some(Err(e)) => {
            warn!("cannot read map {map}: {e}");
            MapHave::Missing
        }
        None => MapHave::Missing,
    }
}

#[cfg(target_arch = "wasm32")]
impl MapHashes {
    /// Web: fetch `map`'s `.kpmap` through the asset server and hash it
    /// once it is there (`None` while in flight).
    fn fetch(
        &mut self,
        catalog: &crate::map_loading::MapCatalog,
        map: &str,
        server: &AssetServer,
        assets: &Assets<crate::map_loading::BytesAsset>,
    ) -> Option<MapHave> {
        use bevy::asset::LoadState;

        let Some(path) = catalog_file(catalog, map) else {
            return Some(MapHave::Missing);
        };
        let handle = self
            .loading
            .entry(map.to_string())
            .or_insert_with(|| server.load(path.to_string_lossy().into_owned()))
            .clone();
        match server.load_state(&handle) {
            LoadState::Loaded => {
                self.loading.remove(map);
                Some(assets.get(&handle).map_or(MapHave::Missing, |b| {
                    MapHave::Hash(super::compat::map_hash(&b.0))
                }))
            }
            LoadState::Failed(e) => {
                warn!("cannot fetch map {map}: {e}");
                self.loading.remove(map);
                Some(MapHave::Missing)
            }
            _ => None,
        }
    }
}

/// `key`'s value in a URL fragment (with or without its `#`).
#[cfg(any(target_arch = "wasm32", test))]
fn fragment_param(fragment: &str, key: &str) -> Option<String> {
    let fragment = fragment.strip_prefix('#').unwrap_or(fragment);
    fragment
        .split(['&', ';'])
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_string)
}

/// `fragment` with `key` set to `value` (or removed), every other pair
/// kept in place; without the leading `#` (cake's
/// `with_fragment_param`).
#[cfg(any(target_arch = "wasm32", test))]
fn with_fragment_param(fragment: &str, key: &str, value: Option<&str>) -> String {
    let fragment = fragment.strip_prefix('#').unwrap_or(fragment);
    let mut pairs: Vec<String> = fragment
        .split(['&', ';'])
        .filter(|pair| !pair.is_empty() && pair.split('=').next() != Some(key))
        .map(str::to_string)
        .collect();
    if let Some(value) = value {
        pairs.push(format!("{key}={value}"));
    }
    pairs.join("&")
}

/// The room in the page's URL: the address bar is the share link.
#[cfg(target_arch = "wasm32")]
mod web {
    use super::{RoomId, with_fragment_param};

    pub fn read_fragment() -> Option<String> {
        web_sys::window()?.location().hash().ok()
    }

    /// Put `room` into the fragment (`None`: take it out), in place: no
    /// navigation, no history entry.
    pub fn share_room(room: Option<&RoomId>) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let code = room.map(ToString::to_string);
        let fragment = with_fragment_param(
            &read_fragment().unwrap_or_default(),
            "room",
            code.as_deref(),
        );
        let url = format!("#{fragment}");
        let written = window.history().is_ok_and(|h| {
            h.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url))
                .is_ok()
        });
        if !written {
            let _ = window.location().set_hash(&fragment);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::sim_net::{LinkParams, SimNetwork};
    use crate::net::wire;

    const MAP: &str = "Marble_Madness_Map";
    const STEP: Duration = Duration::from_millis(20);

    struct Net {
        net: SimNetwork,
        now: Duration,
        peers: BTreeMap<PeerId, Lobby>,
        /// Connections in a match: drained like a session that drops
        /// lobby traffic, with the room as it was at the start.
        away: BTreeMap<PeerId, (Box<dyn Transport>, Option<RoomState>)>,
    }

    impl Net {
        fn new(seed: u64) -> Self {
            let net = SimNetwork::new(seed);
            net.set_default_link(LinkParams {
                latency: Duration::from_millis(30),
                jitter: Duration::from_millis(20),
                loss: 0.05,
                ..Default::default()
            });
            Self {
                net,
                now: Duration::ZERO,
                peers: BTreeMap::new(),
                away: BTreeMap::new(),
            }
        }

        fn join(&mut self, id: u128, name: &str) -> PeerId {
            self.join_as(id, name, Build::current())
        }

        fn join_as(&mut self, id: u128, name: &str, build: Build) -> PeerId {
            let peer = PeerId(id);
            let mut l = Lobby::with_build(Box::new(self.net.join(peer)), name, MAP, build);
            l.provide_map(MAP, MapHave::Hash(7));
            self.peers.insert(peer, l);
            peer
        }

        fn leave(&mut self, id: PeerId) {
            self.peers.remove(&id);
            self.net.leave(id);
        }

        /// Run the room for `secs` of network time (a lost reliable
        /// message costs a second: [`crate::net::sim_net::RELIABLE_RTO`]).
        fn run(&mut self, secs: f32) {
            let n = (secs / STEP.as_secs_f32()) as u32;
            for _ in 0..n {
                self.net.advance(STEP);
                self.now += STEP;
                for l in self.peers.values_mut() {
                    l.poll(self.now);
                }
                for (t, _) in self.away.values_mut() {
                    t.poll_peers();
                    t.receive(Channel::Reliable);
                    t.receive(Channel::Unreliable);
                }
            }
        }

        fn lobby(&mut self, id: PeerId) -> &mut Lobby {
            self.peers.get_mut(&id).unwrap()
        }

        fn room(&self, id: PeerId) -> RoomState {
            self.peers[&id].room().cloned().expect("a room")
        }
    }

    #[test]
    fn lobby_and_session_packets_never_mix() {
        let lobby = encode(LobbyMsg::Stand).unwrap();
        for match_id in [0, 1, 0x4b4c, u64::MAX] {
            assert_eq!(wire::decode(match_id, &lobby), None);
        }
        let session = wire::encode(7, wire::Msg::Hash { frame: 30, hash: 1 }).unwrap();
        assert_eq!(decode(&session), Decoded::Other);
        assert_eq!(decode(&[]), Decoded::Other);
        assert_eq!(decode(&lobby), Decoded::Msg(LobbyMsg::Stand));
        let mut other = postcard::to_allocvec(&(LOBBY_TAG, LOBBY_VERSION + 1)).unwrap();
        other.push(0);
        assert_eq!(decode(&other), Decoded::Foreign);
    }

    #[test]
    fn room_codes() {
        assert_eq!("AbC-9_".parse::<RoomId>().unwrap().to_string(), "abc-9_");
        assert!("".parse::<RoomId>().is_err());
        assert!("a/b".parse::<RoomId>().is_err());
        assert!("x".repeat(25).parse::<RoomId>().is_err());
        for _ in 0..50 {
            let r = RoomId::random();
            assert_eq!(r.to_string().parse::<RoomId>(), Ok(r.clone()));
        }
        assert_eq!(fragment_param("#room=abc", "room").as_deref(), Some("abc"));
        assert_eq!(
            fragment_param("x=1&room=q-7", "room").as_deref(),
            Some("q-7")
        );
        assert_eq!(fragment_param("#roomy=1", "room"), None);
        assert_eq!(
            with_fragment_param("#x=1", "room", Some("r1")),
            "x=1&room=r1"
        );
        assert_eq!(with_fragment_param("#room=old&x=1", "room", None), "x=1");
    }

    #[test]
    fn setup_round_trips() {
        let mut setup = crate::game_setup::GameSetup::default();
        setup.players.push(PlayerSpec {
            faction: Faction::Network,
            ally_team: 0,
            ai: true,
        });
        setup.rules.pausable = false;
        setup.rules.max_speed = 200;
        let back = decode_setup(&encode_setup(&setup));
        assert_eq!(back.rules, setup.rules);
        assert_eq!(back.map, setup.map);
        assert_eq!(back.seed, setup.seed);
        assert_eq!(back.difficulty, setup.difficulty);
        assert_eq!(
            back.players
                .iter()
                .map(|p| (p.faction, p.ally_team, p.ai))
                .collect::<Vec<_>>(),
            setup
                .players
                .iter()
                .map(|p| (p.faction, p.ally_team, p.ai))
                .collect::<Vec<_>>()
        );
    }

    /// The `Start`'s setup in the shape it had before it shared the
    /// replay's [`Seat`]: the bytes must not change (`LOBBY_VERSION`).
    #[test]
    fn setup_wire_bytes_are_the_tuple_encoding() {
        #[derive(Serialize)]
        struct TupleWire {
            map: String,
            seats: Vec<(u8, u8, bool)>,
            difficulty: u8,
            seed: u64,
            rules: MatchRules,
        }
        let mut setup = crate::game_setup::GameSetup {
            players: [Faction::Network, Faction::System, Faction::Hacker]
                .into_iter()
                .enumerate()
                .map(|(i, faction)| PlayerSpec {
                    faction,
                    ally_team: i as u8 % 2,
                    ai: i == 1,
                })
                .collect(),
            seed: 0x0123_4567_89ab_cdef,
            ..Default::default()
        };
        setup.rules.max_speed = 300;
        let old = TupleWire {
            map: setup.map.clone(),
            seats: setup
                .players
                .iter()
                .map(|p| {
                    let faction = Faction::ALL.iter().position(|f| *f == p.faction);
                    (faction.unwrap_or(0) as u8, p.ally_team, p.ai)
                })
                .collect(),
            difficulty: setup.difficulty,
            seed: setup.seed,
            rules: setup.rules,
        };
        assert_eq!(
            postcard::to_allocvec(&encode_setup(&setup)).unwrap(),
            postcard::to_allocvec(&old).unwrap()
        );
    }

    /// Three peers over a lossy, jittery network: the lowest id hosts,
    /// newcomers take the open seats, the host's edits reach everyone,
    /// and the `Start` gives every peer the same setup and its own seat.
    #[test]
    fn three_peers_meet_and_start() {
        let mut n = Net::new(1);
        let a = n.join(30, "alice");
        n.run(4.0);
        assert!(n.lobby(a).is_host(), "alone, a peer hosts its room");
        let b = n.join(20, "bob");
        n.run(3.0);
        let c = n.join(40, "carol");
        n.run(3.0);
        for p in [a, b, c] {
            assert_eq!(n.lobby(p).host(), Some(b), "the lowest id hosts");
        }
        // Bob took the room over from alice's hello: alice keeps seat 0.
        let room = n.room(c);
        assert_eq!(room.host, b);
        assert_eq!(room.seats[0].who, Occupant::Human(a));
        assert_eq!(room.seats[1].who, Occupant::Human(b), "first open seat");
        assert_eq!(
            room.members
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["bob", "alice", "carol"]
        );
        assert_eq!(n.lobby(c).my_seat(), None, "no seat left: spectating");

        n.lobby(b).add_seat().unwrap();
        n.lobby(b).ally_team(2).unwrap();
        n.lobby(b).set_difficulty(3).unwrap();
        n.lobby(b).set_pausable(false).unwrap();
        n.lobby(b).set_cheats(true).unwrap();
        assert!(
            n.lobby(a).set_cheats(false).is_err(),
            "only the host sets the rules"
        );
        n.run(3.0);
        // Carol sits on the new seat and picks her faction.
        n.lobby(c).occupant(2).unwrap();
        n.run(3.0);
        n.lobby(c).faction(2).unwrap();
        n.run(3.0);
        let room = n.room(a);
        assert_eq!(room.seats[2].who, Occupant::Human(c));
        assert_eq!(room.seats[2].faction, 0, "2 + 1 wraps to System");
        assert_eq!(room.seats[2].ally_team, 1);
        assert_eq!(room.difficulty, 3);
        assert!(!room.rules.pausable && room.rules.cheats);
        assert!(
            n.lobby(a).occupant(1).is_err(),
            "a guest edits no seat but its own"
        );

        assert_eq!(
            n.lobby(b).start_blocker().as_deref(),
            Some("waiting for alice to be ready")
        );
        n.lobby(a).set_ready(true);
        n.lobby(c).set_ready(true);
        n.run(3.0);
        assert_eq!(n.lobby(b).start_blocker(), None);
        let host_start = n.lobby(b).start(99, 5).unwrap();
        n.run(3.0);
        let mut starts = vec![(b, host_start)];
        for p in [a, c] {
            let evs = n.lobby(p).take_events();
            let Some(LobbyEvent::Start(s)) = evs.into_iter().next() else {
                panic!("{p} got no start");
            };
            starts.push((p, *s));
        }
        for (p, s) in &starts {
            assert_eq!(s.session.me, *p);
            assert_eq!(s.session.match_id, 5);
            assert_eq!(s.setup.seed, 99);
            assert_eq!(s.setup.map, MAP);
            assert_eq!(s.setup.difficulty, 3);
            assert_eq!(s.setup.rules, room.rules);
            assert_eq!(s.setup.players.len(), 3);
            assert!(s.setup.players.iter().all(|p| !p.ai));
            assert_eq!(s.session.roster, starts[0].1.session.roster);
        }
        let roster = &starts[0].1.session.roster;
        assert_eq!(roster[&a], Some(0));
        assert_eq!(roster[&b], Some(1));
        assert_eq!(roster[&c], Some(2));
        assert_eq!(
            roster.keys().next(),
            Some(&b),
            "the session elects the lobby's host"
        );
    }

    /// The host leaves while still in the lobby: the next lowest id takes
    /// over the room as it was, with the leaver's seat open.
    #[test]
    fn host_leaving_hands_the_room_on() {
        let mut n = Net::new(2);
        let a = n.join(10, "ann");
        let b = n.join(20, "ben");
        let c = n.join(30, "cat");
        n.run(4.0);
        n.lobby(a).add_seat().unwrap();
        n.lobby(a).set_map("Data_Cache_L1").unwrap();
        n.lobby(a).occupant(2).unwrap(); // open → AI
        n.run(3.0);
        let before = n.room(c);
        assert_eq!(before.seats[1].who, Occupant::Human(b));
        assert_eq!(
            before.seats[2].who,
            Occupant::Ai,
            "cat was not seated: no seat was open"
        );
        n.leave(a);
        n.run(3.0);
        assert!(n.lobby(b).is_host());
        let after = n.room(c);
        assert_eq!(after.host, b);
        assert!(after.rev > before.rev);
        assert_eq!(after.map, "Data_Cache_L1");
        assert_eq!(after.seats[0].who, Occupant::Open, "ann's seat");
        assert_eq!(after.seats[1].who, Occupant::Human(b));
        assert_eq!(after.seats[2].who, Occupant::Ai);
        assert_eq!(after.members.len(), 2);
    }

    /// Another build is listed with the reason, never hosts or sits; a
    /// guest without the map blocks the start with a readable reason; a
    /// map change clears the ready flags.
    #[test]
    fn mismatches_are_refused_with_reasons() {
        let mut n = Net::new(3);
        let host = n.join(10, "host");
        let old = Build {
            sim_version: 3,
            ..Build::current()
        };
        let x = n.join_as(5, "oldie", old);
        let g = n.join(20, "guest");
        n.run(4.0);
        assert_eq!(
            n.lobby(host).host(),
            Some(host),
            "the old build is not elected"
        );
        let room = n.room(g);
        let m = room.member(x).unwrap();
        assert!(
            matches!(&m.check, Check::Refused(why) if why.starts_with("runs game version 3")),
            "{m:?}"
        );
        assert_eq!(room.seat_of(x), None);
        assert_eq!(room.seat_of(g), Some(1));
        n.lobby(g).set_ready(true);
        n.run(3.0);
        assert_eq!(n.lobby(host).start_blocker(), None);

        // The host picks a map the guest's copy differs on.
        n.lobby(host).set_map("Dual_Core").unwrap();
        n.lobby(host).provide_map("Dual_Core", MapHave::Hash(1));
        n.lobby(g).provide_map("Dual_Core", MapHave::Hash(2));
        n.run(3.0);
        assert!(!n.lobby(g).ready(), "a new map clears ready");
        assert_eq!(
            n.lobby(host).start_blocker().as_deref(),
            Some("guest has a different copy of map Dual_Core")
        );
        n.lobby(host).set_map("Quad_Core").unwrap();
        n.lobby(host).provide_map("Quad_Core", MapHave::Hash(1));
        n.lobby(g).provide_map("Quad_Core", MapHave::Missing);
        n.run(3.0);
        assert_eq!(
            n.lobby(host).start_blocker().as_deref(),
            Some("guest has no copy of map Quad_Core")
        );
        assert!(n.lobby(host).start(1, 1).is_err());
    }

    /// The guest's map changed after it got ready (a race the host's
    /// checks cannot see): it refuses the `Start` with the reason and
    /// leaves, so the host's session stops waiting for it.
    #[test]
    fn a_guest_refuses_a_start_it_cannot_play() {
        let mut n = Net::new(4);
        let host = n.join(1, "h");
        let g = n.join(2, "g");
        n.run(4.0);
        n.lobby(g).set_ready(true);
        n.run(3.0);
        n.lobby(g).provide_map(MAP, MapHave::Hash(8));
        let start = n.lobby(host).start(3, 3).unwrap();
        assert_eq!(start.session.roster.len(), 2);
        n.run(3.0);
        let evs = n.lobby(g).take_events();
        assert!(
            matches!(&evs[..], [LobbyEvent::Closed(why)] if why.contains("differs from the host's")),
            "{evs:?}"
        );
    }

    /// After the `Start` the host's session owns the transport; a guest's
    /// late lobby traffic reaches it and is dropped there.
    #[test]
    fn late_lobby_packets_do_not_upset_the_session() {
        let mut n = Net::new(5);
        let host = n.join(1, "h");
        let g = n.join(2, "g");
        n.run(4.0);
        n.lobby(g).set_ready(true);
        n.run(3.0);
        let start = n.lobby(host).start(3, 3).unwrap();
        let transport = n.peers.remove(&host).unwrap().into_transport();
        let mut session = super::super::session::Session::networked(start.session, transport);
        // The guest's lobby keeps talking before it sees the start.
        let mut late = Lobby::new(Box::new(n.net.join(PeerId(3))), "late", MAP);
        late.poll(n.now);
        n.lobby(g).set_ready(false);
        for _ in 0..50 {
            n.net.advance(STEP);
            n.now += STEP;
            let now = n.now;
            n.lobby(g).poll(now);
            late.poll(now);
            session.poll(n.now);
        }
        assert_eq!(session.ended(), None);
        assert!(session.desync().is_none());
        assert_eq!(
            session.waiting_for(),
            vec![g],
            "still waiting for the guest's session"
        );
    }

    impl Net {
        /// [`Self::join`], with a connector that reopens the connection
        /// under a fresh id (`id + 1000 × n`).
        fn join_reopening(&mut self, id: u128, name: &str) -> PeerId {
            let peer = self.join(id, name);
            let net = self.net.clone();
            let mut n = 0;
            let l = self.peers.remove(&peer).unwrap();
            let l = l.with_reopen(Box::new(move || {
                n += 1;
                Box::new(net.join(PeerId(id + 1000 * n)))
            }));
            self.peers.insert(peer, l);
            peer
        }

        /// Everyone's view of the room agrees with `host`'s.
        fn assert_one_room(&self, host: PeerId) {
            let room = self.room(host);
            for (id, l) in &self.peers {
                assert_eq!(l.room(), Some(&room), "{id}'s room");
            }
        }
    }

    /// A pair that never connects (the host and a newcomer): the
    /// newcomer learns from the others' hellos that the room has a host
    /// it never reached, reopens its connection after [`STUCK`] and is
    /// seated like any newcomer. Then a guest pair: the higher id
    /// reopens, and the host gives it its seat back under the new id.
    #[test]
    fn a_stuck_room_reopens() {
        let mut n = Net::new(5);
        n.net.sever(PeerId(10), PeerId(30));
        let a = n.join(10, "ann");
        let b = n.join_reopening(20, "ben");
        n.run(4.0);
        n.lobby(a).add_seat().unwrap();
        let c = n.join_reopening(30, "cat");
        n.run(4.0);
        assert_eq!(n.room(a).members.len(), 2, "ann never heard of cat");
        assert_eq!(n.lobby(c).room(), None, "cat has no host");
        let now = n.now;
        let unreached = n.lobby(c).unreached(now);
        assert!(
            unreached.as_deref().is_some_and(|s| s.contains("ann")),
            "{unreached:?}"
        );
        n.run(STUCK.as_secs_f32() + 2.0);
        let c2 = n.lobby(c).me().unwrap();
        assert_eq!(c2, PeerId(1030), "reopened");
        assert_eq!(n.lobby(c).reopens(), 1);
        n.run(3.0);
        let room = n.room(a);
        assert_eq!(room.members.len(), 3);
        assert_eq!(room.seats[2].who, Occupant::Human(c2), "seated");
        n.assert_one_room(a);
        let now = n.now;
        assert_eq!(n.lobby(c).unreached(now), None);

        // Cat moves to a fourth seat, so its seat is not the first open
        // one. Then ben and cat lose each other (cat's next id is 2030):
        // cat, the higher id, reopens; its seat follows it (its old id is
        // gone at once on this network, before the new one says hello).
        n.lobby(a).add_seat().unwrap();
        n.run(2.0);
        n.lobby(c).occupant(3).unwrap();
        n.run(2.0);
        assert_eq!(n.room(a).seats[3].who, Occupant::Human(c2));
        assert_eq!(n.room(a).seats[2].who, Occupant::Open);
        n.net.sever(PeerId(20), PeerId(2030));
        n.lobby(c).reopen_now();
        n.run(STUCK.as_secs_f32() + 5.0);
        let c3 = n.lobby(c).me().unwrap();
        assert_eq!(c3, PeerId(3030), "reopened again: no link to ben");
        assert_eq!(n.lobby(b).reopens(), 0, "ben has the lower id");
        n.run(3.0);
        let room = n.room(a);
        assert_eq!(room.seats[3].who, Occupant::Human(c3), "the seat followed");
        assert_eq!(room.seats[2].who, Occupant::Open);
        assert_eq!(room.members.len(), 3, "the old ids are gone");
        n.assert_one_room(a);
    }

    /// Names change in place; chat goes through the host to everyone in
    /// one order, its own lines included; a line claiming another sender
    /// is dropped (Spring's server does the same).
    #[test]
    fn names_and_chat() {
        let mut n = Net::new(6);
        let a = n.join(10, "ann");
        let b = n.join(20, "ben");
        let c = n.join(30, "cat");
        n.run(4.0);
        n.lobby(b).set_name("  benny\u{7} ");
        n.run(2.0);
        assert_eq!(n.room(c).member(b).unwrap().name, "benny");
        n.lobby(b).say(" hi all ").unwrap();
        // Past a lost message's retransmission (1 s), so the order is
        // known.
        n.run(2.5);
        n.lobby(a).say("gl").unwrap();
        // Cat claims to be ben.
        n.lobby(c).send(
            a,
            LobbyMsg::Chat {
                from: b,
                text: "I resign".into(),
            },
        );
        n.lobby(c).say("").unwrap();
        n.run(3.0);
        for id in [a, b, c] {
            let said: Vec<(PeerId, String, String)> = n
                .lobby(id)
                .take_chat()
                .into_iter()
                .map(|l| (l.from, l.name, l.text))
                .collect();
            assert_eq!(
                said,
                vec![
                    (b, "benny".into(), "hi all".into()),
                    (a, "ann".into(), "gl".into())
                ],
                "{id}"
            );
        }
    }

    /// After a match: the connections come back to the room one by one
    /// (`Lobby::resume`). The first back hosts its own view after the
    /// settle wait, with the seats of those still in the match kept;
    /// the room's lowest id takes over again when it is back; everyone
    /// back, the room is the one the match started from.
    #[test]
    fn back_in_the_room_after_a_match() {
        let mut n = Net::new(7);
        let a = n.join(10, "ann");
        let b = n.join(20, "ben");
        let c = n.join(30, "cat");
        n.run(4.0);
        n.lobby(a).add_seat().unwrap();
        n.lobby(a).set_map(MAP).unwrap();
        n.run(2.0);
        n.lobby(c).occupant(2).unwrap();
        n.run(2.0);
        assert_eq!(n.room(a).seats[2].who, Occupant::Human(c));
        n.lobby(b).set_ready(true);
        n.lobby(c).set_ready(true);
        n.run(2.0);
        let start_room = n.room(a);
        assert_eq!(n.lobby(a).start_blocker(), None);
        n.lobby(a).start(7, 99).unwrap();
        n.run(1.0);
        // Into the match: the transports leave the lobbies.
        for id in [a, b, c] {
            let l = n.peers.remove(&id).unwrap();
            let room = l.room().cloned();
            n.away.insert(id, (l.into_transport(), room));
        }
        n.run(2.0);
        let back = |n: &mut Net, id| {
            let (t, room) = n.away.remove(&id).unwrap();
            let others: Vec<PeerId> = [a, b, c].into_iter().filter(|p| *p != id).collect();
            let name = start_room.member(id).unwrap().name.clone();
            let mut l = Lobby::resume(t, &others, &name, MAP, room, PlayerKey::random());
            l.provide_map(MAP, MapHave::Hash(7));
            n.peers.insert(id, l);
        };
        // Ben first: alone, it hosts its view once settled; the others'
        // seats wait for them.
        back(&mut n, b);
        n.run(4.0);
        assert!(n.lobby(b).is_host());
        let room = n.room(b);
        assert_eq!(room.seats, start_room.seats, "seats kept");
        assert_eq!(room.map, MAP);
        assert!(
            n.lobby(b)
                .start_blocker()
                .is_some_and(|w| w.contains("come back")),
            "{:?}",
            n.lobby(b).start_blocker()
        );
        // Ann: the lowest id hosts again. Ben gets ready meanwhile.
        back(&mut n, a);
        n.lobby(b).set_ready(true);
        n.run(4.0);
        assert!(n.lobby(a).is_host() && !n.lobby(b).is_host());
        assert_eq!(n.room(a).seats, start_room.seats);
        assert!(
            n.lobby(a)
                .start_blocker()
                .is_some_and(|w| w.contains("cat to come back")),
            "{:?}",
            n.lobby(a).start_blocker()
        );
        // Cat: all back, seats as they were, ready again before a start.
        back(&mut n, c);
        n.run(4.0);
        let room = n.room(a);
        assert_eq!(room.seats, start_room.seats);
        assert_eq!(room.members.len(), 3);
        n.assert_one_room(a);
        assert!(
            n.lobby(a)
                .start_blocker()
                .is_some_and(|w| w.contains("to be ready")),
            "{:?}",
            n.lobby(a).start_blocker()
        );
        n.lobby(c).set_ready(true);
        n.run(2.0);
        assert_eq!(n.lobby(a).start_blocker(), None, "a rematch can start");
    }

    /// The menu screenshots' rooms ([`RoomConnector::offline`]): a
    /// connection nobody else reaches, whose lobby hosts a fresh room
    /// alone (and reopens the same way).
    #[test]
    fn offline_rooms_stay_on_this_machine() {
        let mut open = RoomConnector::offline().to(RoomId::random());
        let mut l = Lobby::new(open(), "shots", MAP).with_reopen(open);
        l.provide_map(MAP, MapHave::Hash(7));
        for i in 0..5 {
            l.poll(STEP * i);
        }
        assert!(l.is_host());
        let room = l.room().expect("a room of its own");
        assert_eq!(room.members.len(), 1);
        assert_eq!(room.seats[0].who, Occupant::Human(l.me().unwrap()));
        let before = l.me();
        l.reopen_now();
        assert_ne!(l.me(), before, "a new id, still alone");
    }
}

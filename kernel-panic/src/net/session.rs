//! Host-sequenced lockstep: the protocol state of one peer in one match
//! (`docs/multiplayer-plan.md` M1; write-up `net-experiments/m1-runtime.md`).
//!
//! The shape is Spring's (`CGameServer`: the server is the frame clock,
//! relays every command, and every client simulates) in cake's form
//! (`../cake/cake-net`: the lowest `PeerId` is the host and sequences;
//! everyone, the host included, applies only the host's turn stream):
//!
//! 1. Every peer seals its local commands at the head of a tick `t` for
//!    tick `t + delay` ([`super::CommandBus::seal`]) and hands them over
//!    as a numbered [`Batch`]: guests send theirs to the host, the host
//!    files its own the same way.
//! 2. The host is the clock: on every tick of its own clock it *cuts* a
//!    [`Turn`] — every batch scheduled for that tick, by player — and
//!    applies it itself. It never waits for guests (Spring's server
//!    doesn't either: `CGameServer::CreateNewFrame`,
//!    `rts/Net/GameServer.cpp:2578`), except before the first tick, until
//!    every guest has shown up.
//! 3. Guests apply turns in tick order as they arrive. A batch that
//!    reaches the host after its tick was cut goes into the next turn
//!    (late, never dropped), and the input delay grows.
//!
//! So every peer, the host included, sees its commands take effect
//! `delay` ticks after issuing them (no host advantage), as long as the
//! delay covers the round trip; [`DelayController`] keeps it there. In
//! single player the session is a solo host ([`Session::solo`]) with
//! delay 0: commands apply on the very next tick, as before lockstep.
//!
//! Desync detection is Spring's "dictatorship" (`CGameServer::CheckSync`,
//! `GameServer.cpp:588-618`: with a local client, every checksum must
//! match the host's): every peer reports its state hash after every
//! [`HASH_INTERVAL`]th frame on the reliable channel
//! (`NETMSG_SYNCRESPONSE`, `GameServer.cpp:1354`), the host compares it
//! with its own, and on a mismatch tells everyone where to stop. Spring
//! announces the error and plays on (`GameServer.cpp:719-760`); we stop
//! and dump the state for the bisect.
//!
//! Peers leaving (M2, `net-experiments/m2-networked-play.md`): a guest
//! the transport reports gone, or one silent past [`Timeouts`], is
//! dropped by the host, which sequences a [`SimCommand::Dropped`] in its
//! seat's name, so its leave applies on every peer at the same tick. The
//! host leaving (or going silent) ends the match for the guests: no host
//! migration yet. The host also refuses to sequence what the match rules
//! forbid ([`Admit`]).
//!
//! The turn log (M3, `net-experiments/m3-turn-log.md`): the host keeps
//! every turn of the match, so a player who dropped can come back and a
//! newcomer can watch ([`super::rejoin`]): the host's [`Door`] answers
//! a newcomer's lobby `Hello`, gives a returning player its seat back
//! (by its [`PlayerKey`](super::rejoin::PlayerKey), not its connection)
//! and the newcomer replays the whole log, fetched by gap fills, before
//! it plays along ([`Session::late`]). A seat whose peer disconnected is
//! dropped only after [`Timeouts::rejoin`], so a reload can come back
//! in time. Every peer keeps the turns it applied ([`Session::log`]),
//! which is what a replay file holds; [`Session::replay`] plays one.
//!
//! M3 (`net-experiments/m3-rooms.md`): a peer may leave the match but
//! keep its connection to the room ([`Session::hand_back`], "Back to
//! room"): a [`Msg::Bye`] is its leave. Chat lines and hidden browser
//! tabs ([`Session::say`], [`Session::set_away`]) go through the host
//! like everything else, outside the sim.
//!
//! Speed control (M3, [`super::speed`]): every peer reports its sim load
//! once a second ([`Session::report_load`]); the host slows the game for
//! the slowest and speeds it back up ([`Session::protect_speed`]),
//! sequencing [`SimCommand::InternalSpeed`] as the server.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use super::lobby::{self, LobbyMsg, StartMsg};
use super::rejoin::{Door, PlayerKey, Running};
use super::speed::{PeerLoad, SERVER_PLAYER, SpeedControl, SpeedPolicy};
use super::transport::{Channel, PeerEvent, PeerId, Transport};
use super::wire::{self, Batch, Echo, InputPacket, Msg, PlayerCommands, Turn, TurnPacket, TurnRun};
use super::{CommandFrame, SimCommand};
use crate::sim::DetFloat;

/// Frames between state-hash reports (a second of game time).
pub const HASH_INTERVAL: u32 = 30;

/// Turns each unreliable host packet repeats (`x-transport.md` §4:
/// K ≈ 3–5, about one round trip of history at 30 Hz).
pub const REDUNDANCY: usize = 5;

/// Turns accepted ahead of the next one to apply. A turn further out is
/// not a lagging host, it is a wrong one (cake's `TURN_HORIZON`).
const TURN_HORIZON: u32 = 3600;

/// Turns per gap fill request: a minute of the match. (The host keeps
/// every turn, so a guest stalled for longer, or a late joiner replaying
/// the match from its start, asks again for the next run.)
const FILL_MAX: u32 = 1800;

/// A link with nothing new still gets a packet this often (acks, round
/// trip stamps).
const KEEPALIVE: Duration = Duration::from_millis(100);

/// Own hashes the host keeps to compare late reports against (cake's
/// `HashLog::KEEP`).
const HASH_KEEP: usize = 64;

/// Turns in hand beyond which a guest counts as catching up (its
/// normal backlog is the catch-up target, 2, plus a frame's ticks).
const CATCHING_UP: u32 = 6;

/// How often the host tells every guest everyone's round trip (for the
/// player list; Spring relays each player's ping the same way,
/// `NETMSG_PLAYERINFO`).
const PING_EVERY: Duration = Duration::from_secs(1);

/// How a peer left the match ([`Session`]'s `peer_left`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gone {
    /// The transport reports it gone: it may come back within
    /// [`Timeouts::rejoin`].
    Closed,
    /// Silent past [`Timeouts`]: dropped at once.
    TimedOut,
    /// It went back to the room ([`Msg::Bye`]): dropped at once, and its
    /// key stays in the room ([`Door::in_room`]).
    Bye,
}

/// When silence means a peer is gone (Spring: `CheckTimeout`,
/// `System/Net/UDPConnection.cpp:767`; the host drops a guest,
/// `Net/GameServer.cpp:1963`, a guest gives up on the host).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Before the first packet arrived (Spring's
    /// `InitialNetworkTimeout`, 30 s, `System/GlobalConfig.cpp:15`).
    pub initial: Duration,
    /// After that. Spring waits 120 s (`NetworkTimeout`); here a
    /// minute: a guest that silent is better off rejoining.
    pub running: Duration,
    /// A seat whose peer disconnected (the transport said so) stays in
    /// play this long before the host drops it, so a player whose page
    /// reloaded or whose connection broke can come back to it
    /// ([`super::rejoin`]) before the leaving rules act (in a 1v1 they
    /// end the match). Meanwhile its units do what they were told, as a
    /// Spring team without its player does for good
    /// (`CPlayerHandler::PlayerLeft`, `Game/Players/PlayerHandler.cpp:66`).
    /// A guest timed out by silence is dropped at once: it had
    /// [`Self::running`] already.
    pub rejoin: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(30),
            running: Duration::from_secs(60),
            rejoin: Duration::from_secs(30),
        }
    }
}

/// The host's check of one command before it sequences it: the match
/// rules (`net::apply::rules_admit`), set by the driver. `Err` names
/// why it was refused.
pub type Admit = Box<dyn Fn(u8, &SimCommand) -> Result<(), String> + Send + Sync>;

/// One peer of the match as the player list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    pub peer: PeerId,
    /// The seat it plays (`None`: spectator).
    pub player: Option<u8>,
    pub host: bool,
    /// This peer.
    pub me: bool,
    /// Round trip to the host, ms (the host's own: 0; unknown yet:
    /// `None`).
    pub ping_ms: Option<u32>,
    /// Left the match (disconnected, timed out).
    pub gone: bool,
    /// Its browser tab is hidden ([`Msg::Away`]).
    pub away: bool,
    /// Its sim load, permille of one core at the game speed
    /// ([`super::speed`]; unknown yet: `None`).
    pub load_permille: Option<u16>,
    /// Turns it held without having applied them, at its last report.
    pub behind: Option<u16>,
}

/// The bounds of the adaptive input delay, in ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DelayBounds {
    pub initial: u32,
    pub min: u32,
    pub max: u32,
}

impl Default for DelayBounds {
    /// Start at 3 ticks (100 ms); never below 1 (a guest's batch needs at
    /// least one tick to reach the host) or above 15 (500 ms, where
    /// lockstep stops being playable anyway).
    fn default() -> Self {
        Self {
            initial: 3,
            min: 1,
            max: 15,
        }
    }
}

/// Who is in a networked match.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    /// Every packet carries it; another match's packets are dropped.
    pub match_id: u64,
    pub me: PeerId,
    /// Every peer of the match and the player (team) it commands;
    /// `None`: a spectator. The lowest id is the host.
    pub roster: BTreeMap<PeerId, Option<u8>>,
    pub delay: DelayBounds,
}

/// Turns received and not yet applied: applied strictly in tick order,
/// duplicates dropped, early ones held until the gap fills (cake's
/// `TurnBuffer`).
#[derive(Debug)]
pub struct TurnBuffer {
    next: u32,
    held: BTreeMap<u32, Turn>,
    /// The first tick from `next` on not in hand.
    through: u32,
    /// Turns accepted ahead of `next` ([`TURN_HORIZON`]; a late joiner,
    /// which fetches the whole match, and a replay take any).
    horizon: u32,
}

impl Default for TurnBuffer {
    fn default() -> Self {
        Self {
            next: 0,
            held: BTreeMap::new(),
            through: 0,
            horizon: TURN_HORIZON,
        }
    }
}

impl TurnBuffer {
    /// A buffer without a horizon: it holds a whole match.
    pub fn unbounded() -> Self {
        Self {
            horizon: u32::MAX,
            ..Self::default()
        }
    }

    /// Store a turn; `false` for one applied, held or past the horizon.
    pub fn push(&mut self, turn: Turn) -> bool {
        if turn.tick < self.next
            || turn.tick >= self.next.saturating_add(self.horizon)
            || self.held.contains_key(&turn.tick)
        {
            return false;
        }
        self.held.insert(turn.tick, turn);
        while self.held.contains_key(&self.through) {
            self.through += 1;
        }
        true
    }

    /// The next turn in order, if it is here.
    pub fn pop(&mut self) -> Option<Turn> {
        let turn = self.held.remove(&self.next)?;
        self.next += 1;
        Some(turn)
    }

    /// Turns applicable right now, in order.
    pub fn ready(&self) -> usize {
        (self.through - self.next) as usize
    }

    /// The tick the next applied turn carries (= turns applied).
    pub fn next(&self) -> u32 {
        self.next
    }

    /// The first tick not in hand.
    pub fn have_through(&self) -> u32 {
        self.through
    }
}

/// Round-trip estimate (RFC 6298's SRTT / RTTVAR), in ms.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rtt {
    pub srtt: Option<f32>,
    pub rttvar: f32,
}

impl Rtt {
    fn sample(&mut self, ms: f32) {
        match self.srtt {
            None => {
                self.srtt = Some(ms);
                self.rttvar = ms / 2.0;
            }
            Some(s) => {
                self.rttvar = 0.75 * self.rttvar + 0.25 * (s - ms).abs();
                self.srtt = Some(0.875 * s + 0.125 * ms);
            }
        }
    }

    /// The other side echoed our stamp (if it did), received `now`.
    fn echo(&mut self, echo: Option<Echo>, now: Duration) {
        if let Some(e) = echo {
            let rtt = ms(now)
                .wrapping_sub(e.stamp)
                .saturating_sub(e.held_ms as u32);
            self.sample(rtt as f32);
        }
    }
}

/// The host's adaptive input delay.
///
/// Every guest packet says which tick the guest sealed at; the host's
/// clock has meanwhile reached `next_cut`. `next_cut - tick` is the
/// delay a command sealed at that moment needed to arrive in time: the
/// round trip, the guest's turn backlog and this packet's jitter, all
/// measured at once. The delay follows the typical need: the
/// [`Self::QUANTILE`] of the needs of the last [`Self::WINDOW`] (every
/// guest's samples pooled), plus one tick of margin, rising at once and
/// falling one tick per [`Self::LOWER_HOLD`] of a lower target
/// (hysteresis: a route that flaps does not drag the delay up and
/// down).
///
/// Not the worst need: one packet held up by a loss (a WebRTC data
/// channel repairs it in 100–300 ms, or a 1 s retransmission timeout)
/// would set the delay for everyone for the whole window, and on a
/// lossy link that is always. The rare command that misses its tick is
/// filed into the next turn instead (a late batch, still applied on
/// every peer alike); `net-experiments/m2-transport.md` §7 measured both
/// rules. A guest that lags for good shows in its share of the samples
/// (1/7 at 4v4, above the 5 % the quantile leaves out) within about a
/// second.
#[derive(Clone, Debug)]
pub struct DelayController {
    bounds: DelayBounds,
    delay: u32,
    samples: VecDeque<(Duration, u32)>,
    below_since: Option<Duration>,
}

impl DelayController {
    const WINDOW: Duration = Duration::from_secs(4);
    const LOWER_HOLD: Duration = Duration::from_secs(2);
    const MARGIN: u32 = 1;
    /// Share of the window's needs the delay covers.
    const QUANTILE: f64 = 0.95;

    pub fn new(bounds: DelayBounds) -> Self {
        Self {
            bounds,
            delay: bounds.initial.clamp(bounds.min, bounds.max),
            samples: VecDeque::new(),
            below_since: None,
        }
    }

    pub fn sample(&mut self, now: Duration, need: u32) {
        self.samples.push_back((now, need));
    }

    /// The delay for the turn cut now.
    pub fn update(&mut self, now: Duration) -> u32 {
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| now.saturating_sub(*t) > Self::WINDOW)
        {
            self.samples.pop_front();
        }
        if self.samples.is_empty() {
            return self.delay;
        }
        let mut needs: Vec<u32> = self.samples.iter().map(|(_, n)| *n).collect();
        let k = ((needs.len() - 1) as f64 * Self::QUANTILE).ceil() as usize;
        let (_, typical, _) = needs.select_nth_unstable(k);
        let target = (*typical + Self::MARGIN).clamp(self.bounds.min, self.bounds.max);
        if target > self.delay {
            self.delay = target;
            self.below_since = None;
        } else if target < self.delay {
            match self.below_since {
                Some(t) if now.saturating_sub(t) >= Self::LOWER_HOLD => {
                    self.delay -= 1;
                    self.below_since = Some(now);
                }
                Some(_) => {}
                None => self.below_since = Some(now),
            }
        } else {
            self.below_since = None;
        }
        self.delay
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn delay(&self) -> u32 {
        self.delay
    }
}

/// A desync, once announced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Desync {
    /// The sim frame whose hashes differed.
    pub frame: u32,
    /// Every peer stops after this many ticks, so the dumps compare the
    /// same tick.
    pub stop_at: u32,
    /// The peers whose hash differed from the host's.
    pub peers: Vec<PeerId>,
    /// Where this peer's state dump went, once written (the driver).
    pub dump: Option<String>,
}

/// Why a match ended for this peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// No host migration (M1): without the host there is no clock.
    HostLeft,
    /// Nothing from the host for [`Timeouts`] (Spring: "Lost connection
    /// to the server").
    HostLost,
    /// The host dropped this peer (timed out on its side).
    Dropped,
    /// This peer left ([`Session::close`], [`Session::hand_back`]).
    Left,
}

/// Something worth telling the player about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Left(PeerId),
    Desync(Desync),
    HostLeft,
    /// Host: a guest silent for too long was dropped.
    TimedOut(PeerId),
    /// Guest: no word from the host for too long.
    HostLost,
    /// Guest: the host dropped us.
    Dropped,
    /// Someone joined the running match: back on its seat (`returned`)
    /// or a newcomer on a seat or watching.
    Joined {
        peer: PeerId,
        player: Option<u8>,
        returned: bool,
        name: String,
    },
    /// A chat line, as the host relayed it.
    Chat {
        from: PeerId,
        text: String,
    },
}

/// The host's view of one guest.
#[derive(Debug, Default)]
struct GuestLink {
    player: Option<u8>,
    heard: bool,
    gone: bool,
    /// When the last packet came (any message).
    last_heard: Option<Duration>,
    /// When it joined the running match (a late joiner: its initial
    /// timeout counts from here, not from the match's start).
    since: Option<Duration>,
    /// The guest holds every turn before this.
    next_turn: u32,
    /// When `next_turn` last moved on ([`stalled`]).
    advanced: Option<Duration>,
    rtt: Rtt,
    /// The guest's latest stamp and when it arrived.
    stamp_in: Option<(u32, Duration)>,
    last_send: Option<Duration>,
    sent_newest: Option<u32>,
    sent_ack: u32,
}

/// One player's batches as the host receives them.
#[derive(Debug, Default)]
struct PlayerIn {
    next_seq: u32,
    held: BTreeMap<u32, Batch>,
    last_target: u32,
}

struct Host {
    started: bool,
    links: BTreeMap<PeerId, GuestLink>,
    inputs: BTreeMap<u8, PlayerIn>,
    /// (tick, player, seq) → commands.
    scheduled: BTreeMap<(u32, u8, u32), Vec<SimCommand>>,
    next_cut: u32,
    /// Every turn cut (networked only), from tick 0 to `next_cut - 1`:
    /// the match's turn log, for gap fills and late joiners.
    history: Vec<Turn>,
    hashes: BTreeMap<u32, u64>,
    /// Guest hashes for frames the host hasn't hashed yet.
    early: Vec<(PeerId, u32, u64)>,
    controller: DelayController,
    paused: bool,
    own_seq: u32,
    /// The match rules' check ([`Admit`]); `None` admits everything.
    admit: Option<Admit>,
    /// Seats whose last peer left: their `Dropped` is sequenced.
    dropped: std::collections::BTreeSet<u8>,
    /// Seats whose peer disconnected, and when the grace for its return
    /// ends ([`Timeouts::rejoin`]): then the `Dropped` is sequenced.
    pending_drops: BTreeMap<u8, Duration>,
    /// Lets late joiners in (`None`: nobody gets in; protocol tests).
    door: Option<Door>,
    last_pings: Option<Duration>,
    /// Everyone's load and Spring's lag protection over them.
    speed: SpeedControl,
    /// Numbers the speed control's commands in the schedule.
    server_seq: u32,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("started", &self.started)
            .field("links", &self.links)
            .field("next_cut", &self.next_cut)
            .field("paused", &self.paused)
            .field("dropped", &self.dropped)
            .field("pending_drops", &self.pending_drops)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct Guest {
    next_seq: u32,
    unacked: VecDeque<Batch>,
    host_newest: Option<u32>,
    rtt: Rtt,
    stamp_in: Option<(u32, Duration)>,
    last_send: Option<Duration>,
    sent_through: u32,
    /// A gap first seen (tick, when), and the last resend request.
    gap_seen: Option<(u32, Duration)>,
    resend_at: Option<Duration>,
    /// When the host was last heard from (any message).
    last_heard: Option<Duration>,
    /// Everyone's round trip to the host, as it last told us.
    pings: BTreeMap<PeerId, u16>,
    /// Peers the host said left.
    gone: std::collections::BTreeSet<PeerId>,
    /// Everyone's load and backlog, as the host last told us (permille,
    /// turns; `u16::MAX`: unknown).
    loads: BTreeMap<PeerId, (u16, u16)>,
}

/// Turns a guest may lack before [`stalled`] looks at it: a minute. Less
/// is a slow link or a short hiccup (a deaf guest gives up on the host
/// by itself after [`Timeouts::running`]).
const STALL_BEHIND: u32 = 1800;

/// A guest that still talks but lacks more than [`STALL_BEHIND`] turns
/// the host cut, and has not got any further for longer than `limit`
/// (its downlink is dead, or its tab was frozen): the host drops it. A
/// guest far behind but catching up (a late joiner replaying the log, a
/// throttled tab) keeps advancing and stays
/// (`net-experiments/m3-rooms.md`; the whole log is kept, so behind
/// alone is never hopeless).
fn stalled(l: &GuestLink, next_cut: u32, now: Duration, limit: Duration) -> bool {
    l.heard
        && next_cut.saturating_sub(l.next_turn) > STALL_BEHIND
        && l.advanced
            .or(l.since)
            .is_some_and(|at| now.saturating_sub(at) > limit)
}

#[derive(Debug)]
enum Role {
    Host(Box<Host>),
    Guest(Box<Guest>),
}

/// Counters for the measurements (`m1-runtime.md` §5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionStats {
    /// Host: batches that arrived after their tick was cut.
    pub late_batches: u64,
    /// Guest: resend requests sent; host: fills sent.
    pub gap_fills: u64,
    /// Turns received again (redundancy and duplicates).
    pub dup_turns: u64,
    /// Host: commands it refused to sequence (match rules, a guest's
    /// `Dropped`).
    pub refused: u64,
}

/// One peer's lockstep state for one match.
pub struct Session {
    match_id: u64,
    me: PeerId,
    host: PeerId,
    roster: BTreeMap<PeerId, Option<u8>>,
    /// Over a network (stays true once [`Self::close`] dropped the
    /// transport).
    networked: bool,
    transport: Option<Box<dyn Transport>>,
    /// Who the transport is connected to (the roster to begin with,
    /// then its peer events): what [`Self::hand_back`] hands on.
    connected: std::collections::BTreeSet<PeerId>,
    /// Peers whose browser tab is hidden (this one included).
    away: std::collections::BTreeSet<PeerId>,
    timeouts: Timeouts,
    /// The first poll: where the initial timeouts count from.
    opened: Option<Duration>,
    turns: TurnBuffer,
    delay: u32,
    role: Role,
    desync: Option<Desync>,
    ended: Option<Ended>,
    events: Vec<SessionEvent>,
    /// The tick of this frame's first seal (what [`InputPacket::tick`]
    /// reports).
    frame_tick: u32,
    /// The commands of every turn applied here, in tick then player
    /// order: the match's turn log as a replay file keeps it
    /// ([`Self::log`]). Not kept while playing a replay.
    log: Vec<CommandFrame>,
    /// A replay ([`Self::replay`]): the ticks it has.
    replay_end: Option<u32>,
    pub stats: SessionStats,
}

fn ms(t: Duration) -> u32 {
    t.as_millis() as u32
}

/// A load as [`Msg::Load`] carries it (`u16::MAX` stays "unknown").
fn permille(cpu: f32) -> u16 {
    (cpu * 1000.0).round().det_clamp(0.0, 65_000.0) as u16
}

fn clip(n: u32) -> u16 {
    n.min(65_000) as u16
}

fn echo(stamp_in: Option<(u32, Duration)>, now: Duration) -> Option<Echo> {
    stamp_in.map(|(stamp, at)| Echo {
        stamp,
        held_ms: now.saturating_sub(at).as_millis().min(u16::MAX as u128) as u16,
    })
}

impl Session {
    /// Single player: a host alone, delay 0, no transport.
    pub fn solo() -> Self {
        let bounds = DelayBounds {
            initial: 0,
            min: 0,
            max: 0,
        };
        Self {
            match_id: 0,
            me: PeerId::default(),
            host: PeerId::default(),
            roster: BTreeMap::new(),
            networked: false,
            transport: None,
            connected: Default::default(),
            away: Default::default(),
            timeouts: Timeouts::default(),
            opened: None,
            turns: TurnBuffer::default(),
            delay: 0,
            role: Role::Host(Box::new(Host::new(bounds, true))),
            desync: None,
            ended: None,
            events: Vec::new(),
            frame_tick: 0,
            log: Vec::new(),
            replay_end: None,
            stats: SessionStats::default(),
        }
    }

    /// A recorded match played back: every turn of `frames` (a replay
    /// file's command log) in hand for `ticks` ticks, then it halts. A
    /// guest of nobody: no transport, no hashing; the driver plays one
    /// turn per tick of its clock. A recorded pause is followed by its
    /// resume on the next turn (the pause took no turns), so a replay
    /// never stops for it.
    pub fn replay(frames: &[CommandFrame], ticks: u32) -> Self {
        let mut turns: BTreeMap<u32, Vec<PlayerCommands>> = BTreeMap::new();
        for f in frames.iter().filter(|f| f.tick < ticks) {
            let frames = turns.entry(f.tick).or_default();
            match frames.iter_mut().find(|(p, _)| *p == f.player) {
                Some((_, list)) => list.extend(f.commands.iter().cloned()),
                None => frames.push((f.player, f.commands.clone())),
            }
        }
        let mut s = Self::solo();
        s.role = Role::Guest(Box::default());
        s.turns = TurnBuffer::unbounded();
        for tick in 0..ticks {
            let mut frames = turns.remove(&tick).unwrap_or_default();
            frames.sort_by_key(|(p, _)| *p);
            s.turns.push(Turn {
                tick,
                delay: 0,
                frames,
            });
        }
        s.replay_end = Some(ticks);
        s
    }

    /// A networked match over `transport`.
    pub fn networked(cfg: SessionConfig, transport: Box<dyn Transport>) -> Self {
        let host = cfg
            .roster
            .keys()
            .next()
            .copied()
            .expect("a match has players");
        let role = if host == cfg.me {
            let mut h = Host::new(cfg.delay, false);
            for (&peer, &player) in cfg.roster.iter().filter(|(p, _)| **p != cfg.me) {
                h.links.insert(
                    peer,
                    GuestLink {
                        player,
                        ..Default::default()
                    },
                );
            }
            h.check_started();
            Role::Host(Box::new(h))
        } else {
            Role::Guest(Box::default())
        };
        let connected = cfg
            .roster
            .keys()
            .copied()
            .filter(|p| *p != cfg.me)
            .collect();
        Self {
            match_id: cfg.match_id,
            me: cfg.me,
            host,
            delay: cfg.delay.initial,
            roster: cfg.roster,
            networked: true,
            transport: Some(transport),
            connected,
            away: Default::default(),
            timeouts: Timeouts::default(),
            opened: None,
            turns: TurnBuffer::default(),
            role,
            desync: None,
            ended: None,
            events: Vec::new(),
            frame_tick: 0,
            log: Vec::new(),
            replay_end: None,
            stats: SessionStats::default(),
        }
    }

    /// Joining a running match (M3, [`super::rejoin`]): a player back on
    /// its seat or a newcomer watching, let in by `host` (the match's
    /// host, whatever this peer's id), with `gone` already left. It
    /// holds no turn yet: the whole log arrives as gap fills from tick
    /// 0 (no horizon), and the driver runs it as fast as it can
    /// ([`super::driver::Pacer::fast_forward`]) until it is live.
    pub fn late(
        cfg: SessionConfig,
        host: PeerId,
        gone: &[PeerId],
        transport: Box<dyn Transport>,
    ) -> Self {
        let mut s = Self::networked(cfg, transport);
        s.host = host;
        s.role = Role::Guest(Box::new(Guest {
            gone: gone.iter().copied().collect(),
            ..Default::default()
        }));
        s.turns = TurnBuffer::unbounded();
        s
    }

    pub fn is_networked(&self) -> bool {
        self.networked
    }

    /// Playing a replay ([`Self::replay`]).
    pub fn is_replay(&self) -> bool {
        self.replay_end.is_some()
    }

    /// The ticks a replay has.
    pub fn replay_end(&self) -> Option<u32> {
        self.replay_end
    }

    /// Host: let late joiners in ([`Door`]; the lobby hands it over with
    /// the `Start`).
    pub fn set_door(&mut self, door: Door) {
        if let Role::Host(h) = &mut self.role {
            h.door = Some(door);
        }
    }

    /// The commands of every turn applied so far, by tick then player
    /// (non-empty frames only): a replay's command log.
    pub fn log(&self) -> &[CommandFrame] {
        &self.log
    }

    /// The newest turn the host has cut, as far as this peer knows (how
    /// far a late joiner still has to go).
    pub fn host_newest(&self) -> Option<u32> {
        match &self.role {
            Role::Host(h) => h.next_cut.checked_sub(1),
            Role::Guest(g) => g.host_newest,
        }
    }

    /// When silence counts as gone ([`Timeouts`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_timeouts(&mut self, timeouts: Timeouts) {
        self.timeouts = timeouts;
    }

    /// Host: check every command before sequencing it ([`Admit`]).
    pub fn set_admit(&mut self, admit: Admit) {
        if let Role::Host(h) = &mut self.role {
            h.admit = Some(admit);
        }
    }

    /// Leave the match: the transport is dropped (closed; the others see
    /// this peer leave), nothing runs any more. Idempotent.
    pub fn close(&mut self) {
        self.transport = None;
        if self.networked && self.ended.is_none() {
            self.ended = Some(Ended::Left);
        }
    }

    /// Leave the match but not the room ("Back to room"): tell the host
    /// (a guest) or every guest (the host) with a [`Msg::Bye`] — the
    /// leave the transport would report if it closed — and hand the
    /// transport over, with the peers it is connected to, for the lobby.
    /// `None` without one (single player, already closed).
    pub fn hand_back(&mut self) -> Option<(Box<dyn Transport>, Vec<PeerId>)> {
        self.transport.as_ref()?;
        let to = self.others();
        self.send_to(&to, Channel::Reliable, Msg::Bye);
        let transport = self.transport.take()?;
        if self.ended.is_none() {
            self.ended = Some(Ended::Left);
        }
        Some((transport, self.connected.iter().copied().collect()))
    }

    /// Say `text` to everyone in the match (the host relays it; see
    /// [`super::chat`]). Nothing in single player.
    pub fn say(&mut self, text: &str) {
        let text = super::chat::clean(text);
        if text.is_empty() || self.transport.is_none() {
            return;
        }
        let me = self.me;
        match &self.role {
            Role::Host(_) => self.relay_chat(me, text),
            Role::Guest(_) => self.send(self.host, Channel::Reliable, Msg::Chat { from: me, text }),
        }
    }

    /// Host: `text` from `from` to every guest and to ourselves.
    fn relay_chat(&mut self, from: PeerId, text: String) {
        self.broadcast(
            Msg::Chat {
                from,
                text: text.clone(),
            },
            None,
        );
        self.events.push(SessionEvent::Chat { from, text });
    }

    /// This peer's browser tab was hidden (`true`) or shown again: the
    /// others see it in their player lists. Sent only when it changes.
    pub fn set_away(&mut self, away: bool) {
        let me = self.me;
        if self.away.contains(&me) == away || !self.networked {
            return;
        }
        self.mark_away(me, away);
        let to = self.others();
        self.send_to(&to, Channel::Reliable, Msg::Away { peer: me, away });
    }

    fn mark_away(&mut self, peer: PeerId, away: bool) {
        if away {
            self.away.insert(peer);
        } else {
            self.away.remove(&peer);
        }
    }

    /// The transport is gone ([`Self::close`]).
    pub fn closed(&self) -> bool {
        self.networked && self.transport.is_none()
    }

    /// Guest: command batches the host hasn't acknowledged yet (0 for
    /// the host: it files its own at once).
    pub fn unacked(&self) -> usize {
        match &self.role {
            Role::Host(_) => 0,
            Role::Guest(g) => g.unacked.len(),
        }
    }

    /// Everyone in the match, in roster order, for the player list.
    pub fn peers(&self) -> Vec<PeerInfo> {
        self.roster
            .iter()
            .map(|(&peer, &player)| {
                let host = peer == self.host;
                let me = peer == self.me;
                let (load_permille, behind) = self.load_of(peer);
                let (ping_ms, gone) = match &self.role {
                    Role::Host(h) => match h.links.get(&peer) {
                        Some(l) => (l.rtt.srtt.map(|r| r as u32), l.gone),
                        None => (Some(0), false),
                    },
                    Role::Guest(g) => {
                        let ping = if host {
                            Some(0)
                        } else if me {
                            g.rtt.srtt.map(|r| r as u32)
                        } else {
                            g.pings
                                .get(&peer)
                                .filter(|&&p| p != u16::MAX)
                                .map(|&p| u32::from(p))
                        };
                        let gone = g.gone.contains(&peer)
                            || (host
                                && matches!(self.ended, Some(Ended::HostLeft | Ended::HostLost)));
                        (ping, gone)
                    }
                };
                PeerInfo {
                    peer,
                    player,
                    host,
                    me,
                    ping_ms,
                    gone,
                    away: self.away.contains(&peer),
                    load_permille,
                    behind,
                }
            })
            .collect()
    }

    /// `peer`'s last load report (permille) and backlog, if known.
    fn load_of(&self, peer: PeerId) -> (Option<u16>, Option<u16>) {
        let known = |v: u16| (v != u16::MAX).then_some(v);
        match &self.role {
            Role::Host(h) => h.speed.load(peer).map_or((None, None), |l| {
                (Some(permille(l.cpu)), Some(clip(l.behind)))
            }),
            Role::Guest(g) => g
                .loads
                .get(&peer)
                .map_or((None, None), |&(l, b)| (known(l), known(b))),
        }
    }

    /// Host: which load sets the game speed ([`SpeedPolicy`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_speed_policy(&mut self, policy: SpeedPolicy) {
        if let Role::Host(h) = &mut self.role {
            h.speed.policy = policy;
        }
    }

    /// This peer's sim load over the last second ([`super::speed`]):
    /// the host keeps its own, a guest sends it with its backlog.
    pub fn report_load(&mut self, now: Duration, cpu: f32) {
        let behind = match &self.role {
            Role::Host(_) => 0,
            Role::Guest(g) => g
                .host_newest
                .map_or(0, |n| (n + 1).saturating_sub(self.turns.next())),
        };
        let me = self.me;
        match &mut self.role {
            Role::Host(h) => h.speed.report(
                me,
                PeerLoad {
                    cpu,
                    behind,
                    at: now,
                },
            ),
            Role::Guest(_) => {
                self.send(
                    self.host,
                    Channel::Unreliable,
                    Msg::Load {
                        permille: permille(cpu),
                        behind: clip(behind),
                    },
                );
            }
        }
    }

    /// Host, every frame: Spring's lag protection
    /// ([`SpeedControl::update`]) over its own load and the seated
    /// guests' (spectators don't count, `GameServer.cpp:902`; nor do
    /// hidden tabs, nor late joiners still catching up, which send no
    /// load), at the game's `internal` and `wanted` speed in percent; a
    /// new speed goes into the next turn as the server's
    /// [`SimCommand::InternalSpeed`].
    /// Nothing while paused (`GameServer.cpp:929`) or before the start.
    pub fn protect_speed(&mut self, now: Duration, internal: u16, wanted: u16) {
        let me = self.me;
        let Role::Host(h) = &mut self.role else {
            return;
        };
        if !h.started || h.paused {
            return;
        }
        // Nor does a hidden browser tab (`Msg::Away`): the browser may
        // starve it whatever the game does, and it is not watching.
        let away = &self.away;
        let counted: Vec<PeerId> = std::iter::once(me)
            .chain(
                h.links
                    .iter()
                    .filter(|(p, l)| l.player.is_some() && !l.gone && !away.contains(p))
                    .map(|(p, _)| *p),
            )
            .collect();
        if let Some(percent) = h.speed.update(now, counted, internal, wanted) {
            bevy::log::info!("speed control: game speed {internal} % -> {percent} %");
            let seq = h.server_seq;
            h.server_seq += 1;
            h.scheduled.insert(
                (h.next_cut, SERVER_PLAYER, seq),
                vec![SimCommand::InternalSpeed { percent }],
            );
        }
    }

    pub fn is_host(&self) -> bool {
        matches!(self.role, Role::Host(_))
    }

    pub fn me(&self) -> PeerId {
        self.me
    }

    /// The player this peer commands, by the roster (`None` in single
    /// player, where the local seat decides, and for spectators).
    pub fn player(&self) -> Option<u8> {
        self.roster.get(&self.me).copied().flatten()
    }

    /// The host's seat (`None`: a spectating host, or single player).
    pub fn host_player(&self) -> Option<u8> {
        self.roster.get(&self.host).copied().flatten()
    }

    /// The input delay in force (the last applied turn's).
    pub fn delay(&self) -> u32 {
        self.delay
    }

    /// Ticks applied.
    pub fn applied(&self) -> u32 {
        self.turns.next()
    }

    pub fn desync(&self) -> Option<&Desync> {
        self.desync.as_ref()
    }

    pub fn desync_mut(&mut self) -> Option<&mut Desync> {
        self.desync.as_mut()
    }

    pub fn ended(&self) -> Option<Ended> {
        self.ended
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<SessionEvent> {
        std::mem::take(&mut self.events)
    }

    /// The host has heard from every guest and runs its clock (always
    /// true for guests: they follow the turns).
    pub fn started(&self) -> bool {
        match &self.role {
            Role::Host(h) => h.started,
            Role::Guest(_) => true,
        }
    }

    /// Peers this one waits for: the guests not heard from yet (host,
    /// before the start), or the host (guest).
    pub fn waiting_for(&self) -> Vec<PeerId> {
        match &self.role {
            Role::Host(h) => h
                .links
                .iter()
                .filter(|(_, l)| !l.heard && !l.gone)
                .map(|(p, _)| *p)
                .collect(),
            Role::Guest(_) => vec![self.host],
        }
    }

    /// The round trip to `peer` as measured here, ms.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn rtt(&self, peer: PeerId) -> Option<f32> {
        match &self.role {
            Role::Host(h) => h.links.get(&peer).and_then(|l| l.rtt.srtt),
            Role::Guest(g) => (peer == self.host).then_some(g.rtt.srtt).flatten(),
        }
    }

    /// The connected guests (host only).
    pub fn guests(&self) -> Vec<PeerId> {
        match &self.role {
            Role::Host(h) => h
                .links
                .iter()
                .filter(|(_, l)| !l.gone)
                .map(|(p, _)| *p)
                .collect(),
            Role::Guest(_) => Vec::new(),
        }
    }

    /// Who this peer talks to: every connected guest (the host), the
    /// host (a guest).
    fn others(&self) -> Vec<PeerId> {
        match &self.role {
            Role::Host(_) => self.guests(),
            Role::Guest(_) => vec![self.host],
        }
    }

    /// Turns applicable now: in hand, in order, and before a desync stop.
    pub fn ready(&self) -> usize {
        if self.ended.is_some() {
            return 0;
        }
        let n = self.turns.ready();
        match &self.desync {
            Some(d) => n.min(d.stop_at.saturating_sub(self.turns.next()) as usize),
            None => n,
        }
    }

    /// No more ticks: the match ended, a desync stop was reached, or a
    /// replay is over.
    pub fn halted(&self) -> bool {
        self.ended.is_some()
            || self
                .desync
                .as_ref()
                .is_some_and(|d| self.turns.next() >= d.stop_at)
            || self.replay_end.is_some_and(|e| self.turns.next() >= e)
    }

    /// The next turn, if applicable now. Its delay becomes ours, its
    /// commands go into the log.
    pub fn pop(&mut self) -> Option<Turn> {
        if self.ready() == 0 {
            return None;
        }
        let turn = self.turns.pop()?;
        self.delay = turn.delay as u32;
        if self.replay_end.is_none() {
            self.log
                .extend(turn.frames.iter().filter(|(_, cmds)| !cmds.is_empty()).map(
                    |(player, cmds)| CommandFrame {
                        tick: turn.tick,
                        player: *player,
                        commands: cmds.clone(),
                    },
                ));
        }
        Some(turn)
    }

    /// A frame's ticks start at `tick`.
    pub fn begin_frame(&mut self) {
        self.frame_tick = self.turns.next();
    }

    /// Host: the sim is paused (from the sim's game speed): batches
    /// arriving now are due on the next tick, which is cut only for a
    /// command that resumes ([`Self::resume_pending`]).
    pub fn set_paused(&mut self, paused: bool) {
        if let Role::Host(h) = &mut self.role {
            h.paused = paused;
        }
    }

    /// Host: a scheduled command satisfies `resumes`.
    pub fn resume_pending(&self, resumes: impl Fn(&SimCommand) -> bool) -> bool {
        match &self.role {
            Role::Host(h) => h.scheduled.values().flatten().any(resumes),
            Role::Guest(_) => false,
        }
    }

    /// One of our sealed frames (non-empty) for its tick.
    pub fn submit(&mut self, frame: CommandFrame) {
        if frame.commands.is_empty() {
            return;
        }
        let player = self.player().unwrap_or(frame.player);
        match &mut self.role {
            Role::Host(h) => {
                let seq = h.own_seq;
                h.own_seq += 1;
                h.accept(
                    player,
                    Batch {
                        seq,
                        target: frame.tick,
                        commands: frame.commands,
                    },
                    false,
                    &mut self.stats,
                );
            }
            Role::Guest(g) => {
                let seq = g.next_seq;
                g.next_seq += 1;
                g.unacked.push_back(Batch {
                    seq,
                    target: frame.tick,
                    commands: frame.commands,
                });
            }
        }
    }

    /// Host: cut the next turn from what is scheduled for it and take it
    /// into our own buffer. Returns its tick.
    pub fn cut(&mut self, now: Duration) -> Option<u32> {
        let networked = self.is_networked();
        let Role::Host(h) = &mut self.role else {
            return None;
        };
        let tick = h.next_cut;
        h.next_cut += 1;
        let delay = h.controller.update(now).min(u8::MAX as u32) as u8;
        let keys: Vec<(u32, u8, u32)> = h
            .scheduled
            .range((tick, 0, 0)..=(tick, u8::MAX, u32::MAX))
            .map(|(k, _)| *k)
            .collect();
        let mut frames: Vec<(u8, Vec<SimCommand>)> = Vec::new();
        for key in keys {
            let cmds = h.scheduled.remove(&key).unwrap_or_default();
            match frames.last_mut() {
                Some((p, list)) if *p == key.1 => list.extend(cmds),
                _ => frames.push((key.1, cmds)),
            }
        }
        let turn = Turn {
            tick,
            delay,
            frames,
        };
        if networked {
            h.history.push(turn.clone());
        }
        self.turns.push(turn);
        Some(tick)
    }

    /// Our state hash after sim frame `frame`.
    pub fn record_hash(&mut self, frame: u32, hash: u64) {
        let applied = self.turns.next();
        match &mut self.role {
            Role::Host(h) => {
                h.hashes.insert(frame, hash);
                while h.hashes.len() > HASH_KEEP {
                    h.hashes.pop_first();
                }
                let early = std::mem::take(&mut h.early);
                let mut bad = Vec::new();
                for (peer, f, theirs) in early {
                    match h.hashes.get(&f) {
                        Some(own) if *own != theirs => bad.push(peer),
                        Some(_) => {}
                        None => h.early.push((peer, f, theirs)),
                    }
                }
                if !bad.is_empty() {
                    self.announce_desync(frame, applied, bad);
                }
            }
            Role::Guest(_) => self.send(self.host, Channel::Reliable, Msg::Hash { frame, hash }),
        }
    }

    /// Host: `peers` disagree with us at `frame`. Everyone stops at our
    /// current tick (guests are behind the host, never ahead).
    fn announce_desync(&mut self, frame: u32, stop_at: u32, peers: Vec<PeerId>) {
        if self.desync.is_some() {
            return;
        }
        let d = Desync {
            frame,
            stop_at,
            peers: peers.clone(),
            dump: None,
        };
        self.events.push(SessionEvent::Desync(d.clone()));
        self.desync = Some(d);
        self.broadcast(
            Msg::Desync {
                frame,
                stop_at,
                peers,
            },
            None,
        );
    }

    fn send(&mut self, to: PeerId, channel: Channel, msg: Msg) {
        self.send_to(&[to], channel, msg);
    }

    /// `msg` to each of `to`, encoded once.
    fn send_to(&mut self, to: &[PeerId], channel: Channel, msg: Msg) {
        if let Some(bytes) = wire::encode(self.match_id, msg) {
            self.send_bytes(to, channel, bytes);
        }
    }

    /// An encoded message to each of `to`.
    fn send_bytes(&mut self, to: &[PeerId], channel: Channel, bytes: Vec<u8>) {
        let Some(t) = self.transport.as_mut() else {
            return;
        };
        if let Some((last, rest)) = to.split_last() {
            for &peer in rest {
                t.send(peer, channel, bytes.clone());
            }
            t.send(*last, channel, bytes);
        }
    }

    /// Host: `msg` to every connected guest but `except`, reliably.
    fn broadcast(&mut self, msg: Msg, except: Option<PeerId>) {
        let to: Vec<PeerId> = self
            .guests()
            .into_iter()
            .filter(|g| Some(*g) != except)
            .collect();
        self.send_to(&to, Channel::Reliable, msg);
    }

    /// Take in everything that arrived; notice who left or went silent.
    pub fn poll(&mut self, now: Duration) {
        let Some(t) = self.transport.as_mut() else {
            return;
        };
        let opened = *self.opened.get_or_insert(now);
        let events = t.poll_peers();
        let mut inbox = t.receive(Channel::Reliable);
        inbox.extend(t.receive(Channel::Unreliable));
        for ev in events {
            // A peer joining the room introduces itself with a lobby
            // `Hello` (below); its `Joined` says nothing yet.
            match ev {
                PeerEvent::Joined(peer) => {
                    self.connected.insert(peer);
                }
                PeerEvent::Left(peer) => {
                    self.connected.remove(&peer);
                    self.peer_left(peer, Gone::Closed, now);
                }
            }
        }
        for (from, bytes) in inbox {
            if let Some(msg) = wire::decode(self.match_id, &bytes) {
                self.handle(from, msg, now);
            } else if self.is_host()
                && let lobby::Decoded::Msg(LobbyMsg::Hello(hello)) = lobby::decode(&bytes)
            {
                self.knock(from, hello, now);
            }
        }
        self.check_timeouts(now, opened);
    }

    /// Host: `from` introduced itself in the room while the match runs
    /// (a lobby `Hello`): let it in through the [`Door`] — back on the
    /// seat its [`PlayerKey`] played, or watching — and answer with the
    /// match ([`Running`]), which it joins as a late joiner
    /// ([`Self::late`]). Spring's server likewise accepts a known name
    /// back onto its player slot, killing a link still open under it,
    /// and lets anyone else spectate (`CGameServer::BindConnection`,
    /// `Net/GameServer.cpp:2875-3053`). Another build is answered too
    /// (it refuses the match with the reason, as at a `Start`) but not
    /// let in.
    fn knock(&mut self, from: PeerId, hello: lobby::Hello, now: Duration) {
        let me = self.me;
        let Role::Host(h) = &mut self.role else {
            return;
        };
        if from == me {
            return;
        }
        // Out while the host's state changes; back at the end.
        let Some(mut door) = h.door.take() else {
            return;
        };
        // Not a knock but the room's business (`rejoin`'s module doc): a
        // connection the match has (back in the room after a `Bye`, or
        // still to be told of its own start), or a player that went back
        // to the room, reconnected.
        let key = PlayerKey(hello.key);
        if self.roster.contains_key(&from) || door.in_room.contains(&key) {
            h.door = Some(door);
            return;
        }
        let mut out: Vec<(PeerId, Msg)> = Vec::new();
        let compatible = door.start.compat.build().mismatch(&hello.build).is_none();
        if compatible {
            let (seat, replaces) = door.seat_for(&self.roster, key, me);
            let name = lobby::clean_name(&hello.name);
            if let Some(old) = replaces {
                // Its previous connection: gone from the roster; told,
                // should it still be listening (Spring: "terminating
                // existing connection").
                self.roster.remove(&old);
                if h.links.remove(&old).is_some_and(|l| !l.gone) {
                    out.push((old, Msg::Left { peer: old }));
                }
                door.forget(old);
            }
            self.roster.insert(from, seat);
            door.keys.insert(from, key);
            door.names.insert(from, name.clone());
            h.links.insert(
                from,
                GuestLink {
                    player: seat,
                    since: Some(now),
                    ..Default::default()
                },
            );
            if let Some(seat) = seat {
                h.rejoin_seat(seat);
            }
            bevy::log::info!(
                "{from} ({name}) joined the match {}",
                match (seat, replaces) {
                    (Some(s), Some(_)) => format!("back on seat {s}"),
                    (Some(s), None) => format!("on seat {s}"),
                    _ => "to watch".to_string(),
                }
            );
            self.events.push(SessionEvent::Joined {
                peer: from,
                player: seat,
                returned: replaces.is_some(),
                name: name.clone(),
            });
            for (&peer, _) in h.links.iter().filter(|(p, l)| !l.gone && **p != from) {
                out.push((
                    peer,
                    Msg::Joined {
                        peer: from,
                        player: seat,
                        replaces,
                        name: name.clone(),
                    },
                ));
            }
        }
        let running = Running {
            start: StartMsg {
                roster: self.roster.iter().map(|(p, s)| (*p, *s)).collect(),
                ..door.start.clone()
            },
            host: me,
            gone: h
                .links
                .iter()
                .filter(|(_, l)| l.gone)
                .map(|(p, _)| *p)
                .collect(),
            names: door.names.iter().map(|(p, n)| (*p, n.clone())).collect(),
            newest: h.next_cut,
        };
        h.door = Some(door);
        for (to, msg) in out {
            self.send(to, Channel::Reliable, msg);
        }
        if let (Some(t), Some(bytes)) = (
            self.transport.as_mut(),
            lobby::encode(LobbyMsg::Running(Box::new(running))),
        ) {
            t.send(from, Channel::Reliable, bytes);
        }
    }

    /// Silence for longer than [`Timeouts`]: the host drops the guest
    /// (and tells it, should it still be listening), a guest gives up on
    /// the host. Spring: `CGameServer` kills a timed-out player's link
    /// (`Net/GameServer.cpp:1963`).
    fn check_timeouts(&mut self, now: Duration, opened: Duration) {
        let t = self.timeouts;
        let silent = |last: Option<Duration>, joined: Option<Duration>| {
            let (since, limit) = match last {
                Some(at) => (at, t.running),
                None => (joined.unwrap_or(opened), t.initial),
            };
            now.saturating_sub(since) > limit
        };
        if let Role::Host(h) = &mut self.role {
            // Seats whose peer did not come back in time leave play.
            let due: Vec<u8> = h
                .pending_drops
                .iter()
                .filter(|(_, at)| now >= **at)
                .map(|(s, _)| *s)
                .collect();
            for seat in due {
                bevy::log::info!("seat {seat}'s player did not come back: dropped");
                h.drop_seat(seat);
            }
        }
        match &self.role {
            Role::Host(h) => {
                let lost: Vec<PeerId> = h
                    .links
                    .iter()
                    .filter(|(_, l)| {
                        !l.gone
                            && (silent(l.last_heard, l.since)
                                || stalled(l, h.next_cut, now, t.running))
                    })
                    .map(|(p, _)| *p)
                    .collect();
                for peer in lost {
                    bevy::log::warn!("{peer} timed out: dropped");
                    self.send(peer, Channel::Reliable, Msg::Left { peer });
                    self.peer_left(peer, Gone::TimedOut, now);
                }
            }
            Role::Guest(g) => {
                if self.ended.is_none() && silent(g.last_heard, None) {
                    bevy::log::warn!("no word from the host {}: giving up", self.host);
                    self.ended = Some(Ended::HostLost);
                    self.events.push(SessionEvent::HostLost);
                }
            }
        }
    }

    /// `peer` is gone: the transport said so, it was silent too long, or
    /// it went back to the room ([`Gone`]).
    fn peer_left(&mut self, peer: PeerId, how: Gone, now: Duration) {
        let grace = self.timeouts.rejoin;
        match &mut self.role {
            Role::Host(h) => {
                let Some(link) = h.links.get_mut(&peer) else {
                    return;
                };
                if link.gone {
                    return;
                }
                // Spring: the player goes inactive, its team and units
                // stay (`CPlayerHandler::PlayerLeft`,
                // `rts/Game/Players/PlayerHandler.cpp:66`). Here its seat
                // leaves play (the user's leaving rules): a `Dropped` in
                // its name, after anything of its own already sequenced,
                // applied by every peer on the same tick. (A seat has one
                // peer: batches are numbered per seat.) A disconnect gets
                // `grace` to come back first ([`Timeouts::rejoin`]); a
                // timeout had its minute, and "Back to room" is a choice.
                link.gone = true;
                let seat = link.player;
                h.check_started();
                if how == Gone::Bye
                    && let Some(door) = h.door.as_mut()
                    && let Some(&key) = door.keys.get(&peer)
                {
                    door.in_room.insert(key);
                }
                if let Some(seat) = seat {
                    if how != Gone::Closed || grace.is_zero() {
                        h.drop_seat(seat);
                    } else {
                        h.pending_drops.insert(seat, now + grace);
                    }
                }
                self.events.push(if how == Gone::TimedOut {
                    SessionEvent::TimedOut(peer)
                } else {
                    SessionEvent::Left(peer)
                });
                self.broadcast(Msg::Left { peer }, None);
            }
            Role::Guest(_) => {
                if peer == self.host && self.ended.is_none() {
                    self.ended = Some(Ended::HostLeft);
                    self.events.push(SessionEvent::HostLeft);
                }
            }
        }
    }

    fn handle(&mut self, from: PeerId, msg: Msg, now: Duration) {
        if self.is_host() {
            self.handle_as_host(from, msg, now);
        } else if from == self.host {
            self.handle_as_guest(msg, now);
        }
    }

    fn handle_as_host(&mut self, from: PeerId, msg: Msg, now: Duration) {
        let applied = self.turns.next();
        let Role::Host(h) = &mut self.role else {
            return;
        };
        let Some(link) = h.links.get_mut(&from).filter(|l| !l.gone) else {
            return;
        };
        link.last_heard = Some(now);
        match msg {
            Msg::Input(p) => {
                link.heard = true;
                if p.next_turn > link.next_turn || link.advanced.is_none() {
                    link.advanced = Some(now);
                }
                link.next_turn = link.next_turn.max(p.next_turn);
                link.stamp_in = Some((p.stamp, now));
                link.rtt.echo(p.echo, now);
                let player = link.player;
                h.check_started();
                // A guest catching up after a stall (turns missing, or a
                // pile of them in hand) is behind by far more than the
                // network: its lag says nothing about the delay the rest
                // need, and would hold everyone at the maximum for long.
                let missing = h.next_cut.saturating_sub(p.next_turn);
                let backlog = p.next_turn.saturating_sub(p.tick);
                if h.started && missing <= 2 * h.controller.bounds.max && backlog <= CATCHING_UP {
                    h.controller.sample(now, h.next_cut.saturating_sub(p.tick));
                }
                if let Some(player) = player {
                    for b in p.batches {
                        h.accept(player, b, true, &mut self.stats);
                    }
                }
            }
            Msg::Resend { from: tick } => self.fill(from, tick),
            Msg::Load { permille, behind } => {
                let load = PeerLoad {
                    cpu: f32::from(permille) / 1000.0,
                    behind: u32::from(behind),
                    at: now,
                };
                h.speed.report(from, load);
            }
            Msg::Hash { frame, hash } => {
                let own = h.hashes.get(&frame).copied();
                let newest = h.hashes.last_key_value().map(|(f, _)| *f);
                match own {
                    Some(own) if own != hash => self.announce_desync(frame, applied, vec![from]),
                    Some(_) => {}
                    None if newest.is_none_or(|n| frame > n) => h.early.push((from, frame, hash)),
                    // Older than anything kept: unverifiable (Spring
                    // only warns, `GameServer.cpp:701-709`).
                    None => {}
                }
            }
            Msg::Bye => self.peer_left(from, Gone::Bye, now),
            // Spring drops a chat line that claims another sender
            // (`GameServer.cpp:1150`).
            Msg::Chat { from: said, text } if said == from => {
                let text = super::chat::clean(&text);
                if !text.is_empty() {
                    self.relay_chat(from, text);
                }
            }
            Msg::Away { peer, away } if peer == from => {
                self.mark_away(from, away);
                self.broadcast(Msg::Away { peer, away }, Some(from));
            }
            _ => {}
        }
    }

    fn handle_as_guest(&mut self, msg: Msg, now: Duration) {
        let me = self.me;
        let Role::Guest(g) = &mut self.role else {
            return;
        };
        g.last_heard = Some(now);
        match msg {
            Msg::Turns(p) => {
                g.host_newest = g.host_newest.max(p.newest);
                g.stamp_in = Some((p.stamp, now));
                g.rtt.echo(p.echo, now);
                while g.unacked.front().is_some_and(|b| b.seq < p.cmd_ack) {
                    g.unacked.pop_front();
                }
                for turn in p.run.into_turns() {
                    if !self.turns.push(turn) {
                        self.stats.dup_turns += 1;
                    }
                }
            }
            Msg::Fill(run) => {
                for turn in run.into_turns() {
                    g.host_newest = g.host_newest.max(Some(turn.tick));
                    if !self.turns.push(turn) {
                        self.stats.dup_turns += 1;
                    }
                }
            }
            Msg::Desync {
                frame,
                stop_at,
                peers,
            } => {
                if self.desync.is_none() {
                    let d = Desync {
                        frame,
                        stop_at,
                        peers,
                        dump: None,
                    };
                    self.events.push(SessionEvent::Desync(d.clone()));
                    self.desync = Some(d);
                }
            }
            Msg::Left { peer } if peer == me => {
                // Dropped by the host (it timed us out): the match goes
                // on without us.
                if self.ended.is_none() {
                    self.ended = Some(Ended::Dropped);
                    self.events.push(SessionEvent::Dropped);
                }
            }
            Msg::Left { peer } => {
                if g.gone.insert(peer) {
                    self.events.push(SessionEvent::Left(peer));
                }
            }
            Msg::Pings { ms } => g.pings = self.roster.keys().copied().zip(ms).collect(),
            Msg::Joined {
                peer,
                player,
                replaces,
                name,
            } => {
                if let Some(old) = replaces {
                    self.roster.remove(&old);
                    g.gone.remove(&old);
                    g.pings.remove(&old);
                }
                g.gone.remove(&peer);
                if self.roster.insert(peer, player).is_none() {
                    self.events.push(SessionEvent::Joined {
                        peer,
                        player,
                        returned: replaces.is_some(),
                        name,
                    });
                }
            }
            Msg::Bye => {
                // The host went back to its room: no clock any more.
                if self.ended.is_none() {
                    self.ended = Some(Ended::HostLeft);
                    self.events.push(SessionEvent::HostLeft);
                }
            }
            Msg::Chat { from, text } => self.events.push(SessionEvent::Chat { from, text }),
            Msg::Away { peer, away } => self.mark_away(peer, away),
            Msg::Loads { permille, behind } => {
                g.loads = self
                    .roster
                    .keys()
                    .copied()
                    .zip(permille.into_iter().zip(behind))
                    .collect();
            }
            _ => {}
        }
    }

    /// Host: resend turns from `from` to a guest on the reliable channel,
    /// in messages of at most [`MAX_MESSAGE`](super::transport::MAX_MESSAGE) bytes.
    fn fill(&mut self, to: PeerId, from: u32) {
        let Role::Host(h) = &self.role else {
            return;
        };
        let end = h.next_cut.min(from.saturating_add(FILL_MAX)) as usize;
        let mut start = from as usize;
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        while start < end {
            // Halve until it fits (a quiet minute fits in one). A single
            // turn too big for a message is not sent.
            let mut n = end - start;
            loop {
                let msg = Msg::Fill(TurnRun::of(&h.history[start..start + n]));
                match wire::encode_if_fits(self.match_id, &msg) {
                    Some(bytes) => chunks.push(bytes),
                    None if n > 1 => {
                        n /= 2;
                        continue;
                    }
                    None => {}
                }
                break;
            }
            start += n;
        }
        self.stats.gap_fills += 1;
        for bytes in chunks {
            self.send_bytes(&[to], Channel::Reliable, bytes);
        }
    }

    /// Send what is due: the host its turn packets, a guest its input
    /// packet and gap requests.
    pub fn flush(&mut self, now: Duration) {
        if self.transport.is_none() {
            return;
        }
        let mut out: Vec<(PeerId, Channel, Vec<u8>)> = Vec::new();
        let have = self.turns.have_through();
        let host = self.host;
        let match_id = self.match_id;
        match &mut self.role {
            Role::Host(h) => {
                let newest = h.next_cut.checked_sub(1);
                for (&peer, link) in h.links.iter_mut().filter(|(_, l)| !l.gone) {
                    let ack = link
                        .player
                        .map_or(0, |p| h.inputs.get(&p).map_or(0, |i| i.next_seq));
                    let fresh = newest != link.sent_newest || ack != link.sent_ack;
                    if !fresh && link.last_send.is_some_and(|t| now < t + KEEPALIVE) {
                        continue;
                    }
                    // The newest few turns the guest lacks.
                    let (mut from, end) = match newest {
                        Some(n) if link.next_turn <= n => {
                            let from = link
                                .next_turn
                                .max((n + 1).saturating_sub(REDUNDANCY as u32));
                            (from as usize, n as usize + 1)
                        }
                        _ => (0, 0),
                    };
                    // Oversized (a burst of commands): send the newest
                    // turns that fit; the rest come as gap fills.
                    let bytes = loop {
                        let msg = Msg::Turns(TurnPacket {
                            cmd_ack: ack,
                            newest,
                            stamp: ms(now),
                            echo: echo(link.stamp_in, now),
                            run: TurnRun::of(&h.history[from..end]),
                        });
                        match wire::encode_if_fits(match_id, &msg) {
                            Some(bytes) => break Some(bytes),
                            None if from < end => from += 1,
                            None => break None,
                        }
                    };
                    link.sent_newest = newest;
                    link.sent_ack = ack;
                    link.last_send = Some(now);
                    if let Some(bytes) = bytes {
                        out.push((peer, Channel::Unreliable, bytes));
                    }
                }
                // Everyone's round trip, for the player lists (lossy is
                // fine: the next one comes in a second).
                if h.last_pings.is_none_or(|t| now >= t + PING_EVERY) {
                    h.last_pings = Some(now);
                    // In roster order (every peer has the same roster):
                    // a few bytes, not a 16-byte id per peer.
                    let ms: Vec<u16> = self
                        .roster
                        .keys()
                        .map(|p| match h.links.get(p) {
                            Some(l) if !l.gone => {
                                l.rtt.srtt.map_or(u16::MAX, |r| r.det_min(65_000.0) as u16)
                            }
                            Some(_) => u16::MAX,
                            None => 0,
                        })
                        .collect();
                    // And everyone's load (`net::speed`).
                    let (permille, behind): (Vec<u16>, Vec<u16>) = self
                        .roster
                        .keys()
                        .map(|p| {
                            h.speed.load(*p).map_or((u16::MAX, u16::MAX), |l| {
                                (self::permille(l.cpu), clip(l.behind))
                            })
                        })
                        .unzip();
                    let pings = wire::encode(match_id, Msg::Pings { ms });
                    let loads = wire::encode(match_id, Msg::Loads { permille, behind });
                    for (&peer, _) in h.links.iter().filter(|(_, l)| !l.gone) {
                        for bytes in [&pings, &loads].into_iter().flatten() {
                            out.push((peer, Channel::Unreliable, bytes.clone()));
                        }
                    }
                }
            }
            Role::Guest(g) => {
                let fresh = !g.unacked.is_empty() || self.frame_tick != g.sent_through;
                if fresh || g.last_send.is_none_or(|t| now >= t + KEEPALIVE) {
                    let mut n = g.unacked.len();
                    let bytes = loop {
                        let msg = Msg::Input(InputPacket {
                            next_turn: have,
                            tick: self.frame_tick,
                            stamp: ms(now),
                            echo: echo(g.stamp_in, now),
                            batches: g.unacked.iter().take(n).cloned().collect(),
                        });
                        match wire::encode_if_fits(match_id, &msg) {
                            Some(bytes) => break Some(bytes),
                            None if n > 0 => n /= 2,
                            None => break None,
                        }
                    };
                    g.sent_through = self.frame_tick;
                    g.last_send = Some(now);
                    if let Some(bytes) = bytes {
                        out.push((host, Channel::Unreliable, bytes));
                    }
                }
                // A turn exists that we lack, and redundancy did not
                // bring it: ask for a fill, again every two round trips.
                let gap = g.host_newest.is_some_and(|n| have <= n);
                if !gap {
                    g.gap_seen = None;
                } else {
                    let rtt = Duration::from_millis(g.rtt.srtt.unwrap_or(100.0) as u64);
                    let seen = match g.gap_seen {
                        Some((t, at)) if t == have => at,
                        _ => {
                            g.gap_seen = Some((have, now));
                            now
                        }
                    };
                    let wait = Duration::from_millis(100) + rtt;
                    let again = (rtt * 2).max(Duration::from_millis(250));
                    if now >= seen + wait && g.resend_at.is_none_or(|t| now >= t + again) {
                        g.resend_at = Some(now);
                        self.stats.gap_fills += 1;
                        if let Some(bytes) = wire::encode(match_id, Msg::Resend { from: have }) {
                            out.push((host, Channel::Reliable, bytes));
                        }
                    }
                }
            }
        }
        for (to, channel, bytes) in out {
            self.send_bytes(&[to], channel, bytes);
        }
    }
}

impl Host {
    fn new(bounds: DelayBounds, started: bool) -> Self {
        Self {
            started,
            links: BTreeMap::new(),
            inputs: BTreeMap::new(),
            scheduled: BTreeMap::new(),
            next_cut: 0,
            history: Vec::new(),
            hashes: BTreeMap::new(),
            early: Vec::new(),
            controller: DelayController::new(bounds),
            paused: false,
            own_seq: 0,
            admit: None,
            dropped: Default::default(),
            pending_drops: BTreeMap::new(),
            door: None,
            last_pings: None,
            speed: SpeedControl::default(),
            server_seq: 0,
        }
    }

    /// `seat` leaves play: a `Dropped` in its name, after anything of
    /// its own already sequenced (once).
    fn drop_seat(&mut self, seat: u8) {
        self.pending_drops.remove(&seat);
        if self.dropped.insert(seat) {
            let input = self.inputs.entry(seat).or_default();
            let at = input.last_target.max(self.next_cut);
            input.last_target = at;
            self.scheduled
                .insert((at, seat, u32::MAX), vec![SimCommand::Dropped]);
        }
    }

    /// `seat` has a peer again (a returning player, [`Session::knock`]):
    /// its batches count from 0 again (a new connection numbers anew;
    /// what the old one sent and was not filed is dropped), a drop still
    /// in its grace is off, and a drop already sequenced is followed by
    /// a `Returned` in its name, which every peer applies alike
    /// (`ApplyCtx::returned`: the seat comes back if nothing took it).
    fn rejoin_seat(&mut self, seat: u8) {
        self.pending_drops.remove(&seat);
        let last_target = self.inputs.get(&seat).map_or(0, |i| i.last_target);
        let mut input = PlayerIn {
            last_target,
            ..Default::default()
        };
        if self.dropped.remove(&seat) {
            let mut at = last_target.max(self.next_cut);
            // After its `Dropped`, should both fall on one tick.
            if self.scheduled.contains_key(&(at, seat, u32::MAX)) {
                at += 1;
            }
            input.last_target = at;
            self.scheduled
                .insert((at, seat, u32::MAX), vec![SimCommand::Returned]);
        }
        self.inputs.insert(seat, input);
    }

    /// The host has heard from every guest still in the match: its
    /// clock runs.
    fn check_started(&mut self) {
        self.started |= self.links.values().all(|l| l.heard || l.gone);
    }

    /// File `batch` from `player`: exactly once, in seq order, never in
    /// the past; late batches and refused commands are counted in
    /// `stats`.
    /// A guest's batch (`guest`) may not drop anyone, bring anyone back
    /// or set the speed control's speed: only the host issues `Dropped`,
    /// `Returned` and `InternalSpeed`.
    fn accept(&mut self, player: u8, mut batch: Batch, guest: bool, stats: &mut SessionStats) {
        let input = self.inputs.entry(player).or_default();
        if batch.seq < input.next_seq || input.held.contains_key(&batch.seq) {
            return;
        }
        if guest {
            let before = batch.commands.len();
            batch.commands.retain(|c| {
                !matches!(
                    c,
                    SimCommand::Dropped | SimCommand::Returned | SimCommand::InternalSpeed { .. }
                )
            });
            stats.refused += (before - batch.commands.len()) as u64;
        }
        // The match rules: what they forbid is never sequenced (the
        // batch keeps its number, so the guest's acks still line up).
        if let Some(admit) = &self.admit {
            batch.commands.retain(|cmd| match admit(player, cmd) {
                Ok(()) => true,
                Err(why) => {
                    bevy::log::warn!("host: refused {cmd:?} from player {player}: {why}");
                    stats.refused += 1;
                    false
                }
            });
        }
        input.held.insert(batch.seq, batch);
        while let Some(b) = input.held.remove(&input.next_seq) {
            input.next_seq += 1;
            let floor = input.last_target.max(self.next_cut);
            let at = if self.paused {
                floor
            } else {
                b.target.max(floor)
            };
            stats.late_batches += (b.target < self.next_cut) as u64;
            input.last_target = at;
            self.scheduled.insert((at, player, b.seq), b.commands);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(tick: u32) -> Turn {
        Turn {
            tick,
            ..Default::default()
        }
    }

    #[test]
    fn turns_apply_in_order_and_wait_for_gaps() {
        let mut buf = TurnBuffer::default();
        assert!(buf.push(turn(1)));
        assert_eq!(buf.ready(), 0, "turn 0 is missing");
        assert!(buf.pop().is_none());
        assert!(buf.push(turn(0)));
        assert!(!buf.push(turn(0)), "held already");
        assert_eq!(buf.ready(), 2);
        assert_eq!(buf.have_through(), 2);
        assert_eq!(buf.pop().map(|t| t.tick), Some(0));
        assert!(!buf.push(turn(0)), "applied already");
        assert!(!buf.push(turn(TURN_HORIZON + 1)), "past the horizon");
        assert_eq!(buf.pop().map(|t| t.tick), Some(1));
        assert_eq!(buf.next(), 2);
    }

    fn stop() -> SimCommand {
        SimCommand::Stop {
            units: super::super::Units::of([super::super::NetId(1)]),
        }
    }

    #[test]
    fn the_host_files_batches_once_in_order_and_never_in_the_past() {
        let mut h = Host::new(DelayBounds::default(), true);
        h.next_cut = 10;
        let b = |seq, target| Batch {
            seq,
            target,
            commands: vec![stop()],
        };
        let mut stats = SessionStats::default();
        h.accept(1, b(1, 14), false, &mut stats);
        assert_eq!(stats.late_batches, 0, "held for seq 0");
        assert!(h.scheduled.is_empty());
        h.accept(1, b(0, 8), false, &mut stats);
        assert_eq!(stats.late_batches, 1, "late: due at 8, cut is at 10");
        h.accept(1, b(0, 8), false, &mut stats);
        assert_eq!(stats.late_batches, 1, "duplicate");
        let keys: Vec<_> = h.scheduled.keys().copied().collect();
        assert_eq!(keys, vec![(10, 1, 0), (14, 1, 1)]);
        // A lower target later (the delay dropped) keeps the order.
        h.accept(1, b(2, 12), false, &mut stats);
        assert!(h.scheduled.contains_key(&(14, 1, 2)));
        // Paused: due on the next tick, whatever the target.
        h.paused = true;
        h.accept(2, b(0, 40), false, &mut stats);
        assert!(h.scheduled.contains_key(&(10, 2, 0)));
    }

    #[test]
    fn the_delay_rises_at_once_and_falls_slowly() {
        let ms = Duration::from_millis;
        let mut c = DelayController::new(DelayBounds::default());
        assert_eq!(c.update(ms(0)), 3, "no samples: the initial delay");
        c.sample(ms(0), 6);
        assert_eq!(c.update(ms(0)), 7, "need + 1 margin, at once");
        // 30 samples a second of need 1: the window (4 s) still holds
        // the 6s until they are a twentieth of it.
        let mut t = 0;
        for _ in 0..30 {
            t += 33;
            c.sample(ms(t), 1);
        }
        assert_eq!(c.update(ms(t)), 7, "6 is still above the 95th percentile");
        while t < 20_000 {
            t += 33;
            c.sample(ms(t), 1);
            c.update(ms(t));
        }
        assert_eq!(c.delay(), 2, "down one per hold, to need + 1");
        for _ in 0..30 {
            t += 33;
            c.sample(ms(t), 40);
        }
        assert_eq!(c.update(ms(t)), 15, "bounded");
    }

    /// A rare spike (a packet held up by a repaired loss) leaves the
    /// delay alone; a guest that lags all the time moves it, even as one
    /// of seven.
    #[test]
    fn the_delay_follows_the_typical_need() {
        let ms = Duration::from_millis;
        let mut c = DelayController::new(DelayBounds::default());
        let mut t = 0;
        // Seven guests, 30 packets a second each, needing 3; one in 50
        // packets 12 ticks late.
        for i in 0..7 * 30 * 10u64 {
            t = i * 33 / 7;
            c.sample(ms(t), if i % 50 == 49 { 15 } else { 3 });
            c.update(ms(t));
        }
        assert_eq!(c.delay(), 4, "the spikes are 2 %: need 3 + 1");
        // Now guest 0 needs 9 on every packet.
        let start = t;
        let mut rose = None;
        for i in 0..7 * 30 * 5u64 {
            t = start + i * 33 / 7;
            c.sample(ms(t), if i % 7 == 0 { 9 } else { 3 });
            if c.update(ms(t)) == 10 && rose.is_none() {
                rose = Some(t - start);
            }
        }
        let rose = rose.expect("the lagging guest moved the delay");
        assert!(rose < 1500, "within {rose} ms");
        assert_eq!(c.delay(), 10);
    }
}

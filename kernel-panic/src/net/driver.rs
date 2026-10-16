//! The tick driver: the fixed sim advances only by applying turns
//! ([`super::session`]), paced by the frame clock.
//!
//! It replaces Bevy's `run_fixed_main_schedule` (whose set is disabled
//! by [`LockstepPlugin`]): each frame [`drive_frame`] takes in the
//! network, works out how many ticks to run, and runs them one by one
//! — seal the local commands, (host) cut the turn, apply it through the
//! bus, run `FixedMain`, report the state hash every
//! [`HASH_INTERVAL`] frames — then sends what is due. Single player is
//! the same path with a solo host ([`Session::solo`], delay 0): one
//! tick per 1/30 s of virtual time, at most three per frame (the
//! `Time<Virtual>` 100 ms cap), commands applied on the next tick,
//! exactly as Bevy's own loop did.
//!
//! - **Host:** its clock is the match clock (Spring's server,
//!   `CGameServer::CreateNewFrame`, `rts/Net/GameServer.cpp:2578`): one
//!   turn cut and applied per tick of game time; nothing waits for
//!   guests once they have all shown up.
//! - **Guest:** applies the turns it holds, one per tick of its own
//!   clock; when more than [`CatchUp::target_backlog`] are waiting it
//!   runs extra ticks to catch up (cake's `TARGET_BACKLOG`; Spring's
//!   client likewise eats its queue faster the longer it gets, aiming to
//!   stay two frames behind, `CGame::UpdateNumQueuedSimFrames`,
//!   `rts/Net/NetCommands.cpp:161-216`), capped per frame so drawing
//!   keeps going (Spring: `GetNetMessageProcessingTimeLimit`, :248).
//!   With no turn in hand it waits: [`NetStatus::waiting`] says for
//!   whom, and the session asks for a gap fill.
//! - **Game speed and pause** come from the sim: the `SetSpeed` /
//!   `Pause` commands set [`MatchControl`](super::MatchControl) as they
//!   apply, so every peer changes pace on the same tick ([`GameSpeed`]
//!   is the driver's view of it). This is the one place game speed
//!   scales the clock (`net::control`).
//! - **Speed control** (M3, `net::speed`): the driver times every tick
//!   ([`LoadMeter`]) and hands the session this peer's load once a
//!   second; on the host the session then runs Spring's lag protection
//!   and may slow the game, as a command like the others.
//! - **Desync:** every peer stops at the tick the host names and writes
//!   a state dump ([`DumpDir`]; the console and a download on the web).
//!   The match stays paused (Spring plays on).
//! - **A networked match starts** by making the roster's seat the local
//!   team and handing the host the match rules' check
//!   ([`seat_networked`]); **it ends for this peer** when the host
//!   leaves, is lost or drops us, or when the player leaves
//!   ([`NetSession::request_leave`]: the resignation goes out first,
//!   then the transport closes — or, "Back to room", goes back to the
//!   lobby as [`BackToRoom`]). [`NetStatus`] tells the UI all of it.
//! - **Joining late** (M3, [`super::rejoin`]): a session that starts in
//!   a running match fast-forwards ([`Pacer::fast_forward`]): as many
//!   turns per frame as a quarter second of wall time allows, until it
//!   holds no more than its normal backlog — the sim replays the match
//!   headless-fast, a frame drawn every quarter second.
//! - **Replays** ([`super::replay_log`]): a replay session plays one
//!   turn per tick of the clock, never catching up; the viewer's own
//!   pause and speed ([`Pacer::hold`], [`Pacer::rate`]) act on the
//!   clock only, never on the sim.
//! - **Chat and hidden tabs** (M3): [`NetSession::say`] and the tab's
//!   visibility ([`TabHidden`]) go to the session; the lines it relays
//!   land in the [`ChatLog`].

use std::time::Duration;

use bevy::app::{RunFixedMainLoop, RunFixedMainLoopSystems};
use bevy::platform::time::Instant;
use bevy::prelude::*;

use crate::sim::DetFloat;

use super::chat::{ChatLine, ChatLog};
use super::session::{
    Desync, Ended, HASH_INTERVAL, PeerInfo, Session, SessionConfig, SessionEvent,
};
use super::speed::LoadMeter;
use super::transport::{PeerId, Transport};
use super::wire::Turn;
use super::{CommandBus, CommandFrame, SimCommand};
use crate::map_loading::MatchScopeAppExt;

/// The sim's pace as the driver sees it: [`MatchControl`]'s speed and
/// pause, which the speed / pause commands set as they apply (so every
/// peer reads the same value at the same tick). Not hashed: the sim
/// never reads it.
///
/// [`MatchControl`]: super::MatchControl
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GameSpeed {
    /// Game seconds per real second the game runs at (Spring's
    /// `speedFactor`: the players' speed, or less while the speed
    /// control slows the game).
    pub factor: f32,
    /// The players' speed (Spring's `wantedSpeedFactor`).
    pub wanted: f32,
    pub paused: bool,
}

impl Default for GameSpeed {
    fn default() -> Self {
        Self {
            factor: 1.0,
            wanted: 1.0,
            paused: false,
        }
    }
}

/// A speed factor in percent, as `MatchControl` holds it.
fn percent(factor: f32) -> u16 {
    (factor * 100.0).round() as u16
}

/// Whether a command, sequenced while the sim is paused, makes the host
/// cut the next turn at once: the unpause (`m1-runtime.md` §3). Anything
/// else ordered meanwhile waits in the schedule and rides along.
pub fn resumes_sim(cmd: &SimCommand) -> bool {
    matches!(cmd, SimCommand::Pause { on: false })
}

/// Catch-up limits of a guest that fell behind.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CatchUp {
    /// Turns a guest may hold before it runs extra ticks.
    pub target_backlog: usize,
    /// Extra ticks per frame, at most.
    pub max_extra: u32,
    /// Wall time per frame the extra ticks may take (`None`: no limit —
    /// tests, which must not depend on the machine's speed).
    pub budget: Option<Duration>,
}

impl Default for CatchUp {
    fn default() -> Self {
        Self {
            target_backlog: 2,
            max_extra: 8,
            // A third of a 60 Hz frame stays for drawing at least.
            budget: Some(Duration::from_millis(11)),
        }
    }
}

impl CatchUp {
    /// A replay holds every turn from the start: one per tick of the
    /// clock, never more.
    pub fn replay() -> Self {
        Self {
            target_backlog: usize::MAX,
            max_extra: 0,
            budget: None,
        }
    }
}

/// Wall time a fast-forwarding frame may spend on ticks: the screen
/// still updates four times a second.
pub const FAST_FORWARD_BUDGET: Duration = Duration::from_millis(250);

/// The frame clock: game time not yet turned into ticks.
#[derive(Clone, Debug)]
pub struct Pacer {
    pub acc: Duration,
    pub catch_up: CatchUp,
    /// Waiting since: for a turn (guest), for every guest to show up
    /// (host, before the first tick).
    pub starved_since: Option<Duration>,
    /// Joining a running match: run the turns in hand as fast as
    /// [`FAST_FORWARD_BUDGET`] allows (no limit when the catch-up has
    /// no budget: tests), until no more than the normal backlog is
    /// left. Spring's reconnecting client the same: it simulates the
    /// frames it was sent as fast as it can, drawing at least
    /// `MinDrawFPS` (2) frames a second (`CGame::GetNetMessageProcessingTimeLimit`,
    /// `rts/Net/NetCommands.cpp:245-256`; `System/GlobalConfig.cpp:71-72`).
    pub fast_forward: bool,
    /// The viewer's speed of a replay (1.0: the recorded speed). Scales
    /// the clock only: the recorded speed commands still apply.
    pub rate: f32,
    /// The viewer paused a replay: no tick runs.
    pub hold: bool,
    /// This peer's sim load (`net::speed`).
    pub load: LoadMeter,
    /// What a tick costs, instead of timing it: a machine of a given
    /// speed in tests (`None`: measured). A frame then also runs only
    /// as many ticks as fit in its length, as on a machine that slow.
    pub cost: Option<Duration>,
    /// With [`Self::cost`]: the frame time left for ticks.
    pub capacity: Duration,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            acc: Duration::ZERO,
            catch_up: CatchUp::default(),
            starved_since: None,
            fast_forward: false,
            rate: 1.0,
            hold: false,
            load: LoadMeter::default(),
            cost: None,
            capacity: Duration::ZERO,
        }
    }
}

/// What one frame did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameReport {
    /// Ticks run on the clock.
    pub paced: u32,
    /// Extra ticks run to catch up.
    pub extra: u32,
    /// The clock was due a tick and no turn was there.
    pub starved: bool,
    /// The machine had no time for more ticks ([`Pacer::cost`]).
    pub busy: bool,
}

/// The sim side of a frame, as [`drive_frame`] needs it: the game world
/// ([`WorldRunner`]), or a stand-in in the protocol tests.
pub trait TickRunner {
    /// The sim's [`GameSpeed`].
    fn speed(&self) -> GameSpeed;
    /// Seal the local commands issued since the last seal for the next
    /// tick, `delay` ahead (again on the same tick while paused): the
    /// non-empty frames.
    fn seal(&mut self, delay: u32) -> Vec<CommandFrame>;
    /// Apply `turn` and run its tick.
    fn run(&mut self, turn: &Turn);
    /// The state hash now.
    fn hash(&mut self) -> u64;
    /// Write the desync dump; where it went.
    fn dump(&mut self, session: &Session, desync: &Desync) -> String;
}

/// One frame of the lockstep driver: take in the network, run the ticks
/// that are due and possible, send.
pub fn drive_frame(
    s: &mut Session,
    p: &mut Pacer,
    r: &mut impl TickRunner,
    delta: Duration,
    now: Duration,
    step: Duration,
) -> FrameReport {
    s.poll(now);
    s.begin_frame();
    let mut rep = FrameReport::default();
    let speed = r.speed();
    if !s.started() {
        // The match clock starts once everyone is here.
        p.acc = Duration::ZERO;
        p.starved_since.get_or_insert(now);
    } else if !s.halted() && !speed.paused && !p.hold {
        p.acc += delta.mul_f64(speed.factor.det_max(0.0) as f64 * p.rate.det_max(0.0) as f64);
    }
    // Joining late: no cap on the extra ticks but wall time.
    let (max_extra, budget) = if p.fast_forward {
        (u32::MAX, p.catch_up.budget.map(|_| FAST_FORWARD_BUDGET))
    } else {
        (p.catch_up.max_extra, p.catch_up.budget)
    };
    let t0 = budget.map(|_| Instant::now());
    if let Some(cost) = p.cost {
        // A frame is as long as its ticks at least: no more of them than
        // fit, with nothing banked beyond one.
        p.capacity = (p.capacity + delta).min(delta + cost);
    }
    while !s.halted() && !p.hold {
        if p.cost.is_some_and(|c| p.capacity < c) {
            rep.busy = true;
            break;
        }
        let paused = r.speed().paused;
        if s.is_host() {
            if !s.started() {
                break;
            }
            s.set_paused(paused);
            if paused {
                if !s.resume_pending(resumes_sim) {
                    break;
                }
            } else if p.acc >= step {
                p.acc -= step;
            } else {
                break;
            }
            run_one(s, p, r, now);
            rep.paced += 1;
            continue;
        }
        let ready = s.ready();
        if ready == 0 {
            rep.starved |= !paused && p.acc >= step;
            break;
        }
        if paused {
            // Only a turn the host cut to resume can be here.
            run_one(s, p, r, now);
            rep.paced += 1;
        } else if p.acc >= step {
            p.acc -= step;
            run_one(s, p, r, now);
            rep.paced += 1;
        } else if ready > p.catch_up.target_backlog
            && rep.extra < max_extra
            && budget.zip(t0).is_none_or(|(b, t0)| t0.elapsed() < b)
        {
            run_one(s, p, r, now);
            rep.extra += 1;
        } else {
            break;
        }
    }
    if rep.paced + rep.extra == 0 && r.speed().paused && !s.halted() {
        // No tick while paused, but commands still go out: the one that
        // resumes, and whatever the players order meanwhile.
        for frame in r.seal(s.delay()) {
            s.submit(frame);
        }
    }
    if rep.starved || rep.busy {
        // Bank no more than one tick, or a burst of arrivals would play
        // back in fast-forward (cake's `Match::advance`). A machine too
        // slow for the clock loses the time, as Bevy's capped frame
        // delta loses it.
        p.acc = p.acc.min(step);
    }
    if rep.starved {
        p.starved_since.get_or_insert(now);
    } else if rep.paced + rep.extra > 0 {
        p.starved_since = None;
    }
    // Live once the turns in hand are the normal backlog, and the host
    // has nothing newer.
    if p.fast_forward
        && s.ready() <= p.catch_up.target_backlog
        && s.host_newest()
            .is_some_and(|n| s.applied() + 1 + p.catch_up.target_backlog as u32 > n)
    {
        p.fast_forward = false;
        info!("caught up with the match at tick {}", s.applied());
    }
    if s.is_networked() && !s.halted() {
        // Speed control (`net::speed`): this peer's load once a second;
        // the host decides. Not while catching up with a match joined
        // late: Spring leaves a reconnecting player out until it is
        // nearly live (`GameServer.cpp:900-904`).
        let speed = r.speed();
        if let Some(cpu) = p.load.report(now, speed.factor)
            && !p.fast_forward
        {
            s.report_load(now, cpu);
        }
        if !speed.paused {
            s.protect_speed(now, percent(speed.factor), percent(speed.wanted));
        }
    }
    if s.halted()
        && let Some(d) = s.desync().filter(|d| d.dump.is_none()).cloned()
    {
        let place = r.dump(s, &d);
        if let Some(d) = s.desync_mut() {
            d.dump = Some(place);
        }
    }
    s.flush(now);
    rep
}

fn run_one(s: &mut Session, p: &mut Pacer, r: &mut impl TickRunner, now: Duration) {
    for frame in r.seal(s.delay()) {
        s.submit(frame);
    }
    if s.is_host() {
        s.cut(now);
    }
    let Some(turn) = s.pop() else {
        return;
    };
    let t0 = p.cost.is_none().then(Instant::now);
    r.run(&turn);
    let frame = turn.tick + 1;
    if s.is_networked() && frame.is_multiple_of(HASH_INTERVAL) {
        let hash = r.hash();
        s.record_hash(frame, hash);
    }
    let cost = p.cost.or(t0.map(|t| t.elapsed())).unwrap_or_default();
    p.capacity = p.capacity.saturating_sub(cost);
    p.load.tick(cost);
}

// --- the game side -----------------------------------------------------

/// How long "Leave match" waits for the host to acknowledge the
/// resignation before it disconnects anyway.
pub const LEAVE_GRACE: Duration = Duration::from_millis(1500);

/// How long a notice ("X left the match") stays on screen.
pub const NOTICE_SECS: Duration = Duration::from_secs(8);

/// The match's lockstep state. Match-scoped: every match starts a fresh
/// session — networked if a [`NextNetMatch`] is waiting, else solo.
#[derive(Resource)]
pub struct NetSession {
    pub session: Session,
    pub pacer: Pacer,
    /// The networked seat was made the local team.
    seated: bool,
    /// "Leave match" was asked; when the driver first saw it.
    leave: Option<Option<Duration>>,
    /// ... and the connection goes back to the room, not closed.
    to_room: bool,
    /// Recent notices, oldest first.
    notices: Vec<(Duration, Notice)>,
    /// The match's replay was kept (`replay_log::finish_match`).
    pub replay_kept: bool,
    /// Playing a replay: what the viewer shows about it.
    pub viewing: Option<super::replay_log::ReplayView>,
}

impl NetSession {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            pacer: Pacer::default(),
            seated: false,
            leave: None,
            to_room: false,
            notices: Vec::new(),
            replay_kept: false,
            viewing: None,
        }
    }

    /// The session a lobby's [`NextNetMatch`] describes: a match from its
    /// start, or (`join`) a running one, entered late; the host gets
    /// the door for late joiners.
    pub fn from_next(next: NextNetMatch) -> Self {
        let session = match &next.join {
            Some((host, gone)) => Session::late(next.config, *host, gone, next.transport),
            None => Session::networked(next.config, next.transport),
        };
        let mut s = Self::new(session);
        if let Some(door) = next.door {
            s.session.set_door(door);
        }
        s.pacer.catch_up = next.catch_up;
        s.pacer.fast_forward = next.join.is_some();
        s
    }

    /// A recorded match played back ([`Session::replay`]): one turn per
    /// tick of the clock ([`CatchUp::replay`]).
    pub fn replay(frames: &[CommandFrame], ticks: u32) -> Self {
        let mut s = Self::new(Session::replay(frames, ticks));
        s.pacer.catch_up = CatchUp::replay();
        s
    }

    /// A replay played back ([`super::replay_log`]).
    pub fn from_replay(next: super::replay_log::NextReplay) -> Self {
        let r = next.replay;
        let mut s = Self::replay(&r.frames, r.ticks);
        s.viewing = Some(super::replay_log::ReplayView {
            name: next.name,
            map_hash: r.compat.map_hash,
            replay: r,
        });
        s
    }

    /// Leave the match ("Leave match", or the menu from a networked
    /// match): the caller issues the resignation first; the driver sends
    /// it, waits until the host has it (at most [`LEAVE_GRACE`]) and
    /// closes the transport ([`Session::close`]; [`NetStatus::closed`]
    /// says when). The host and a peer whose match is over close at
    /// once.
    pub fn request_leave(&mut self) {
        self.leave.get_or_insert(None);
    }

    /// "Back to room": [`Self::request_leave`], but the transport goes
    /// back to the room's lobby ([`BackToRoom`]) instead of closing; the
    /// others see this peer leave the match ([`super::wire::Msg::Bye`])
    /// and meet it again in the room.
    pub fn request_back_to_room(&mut self) {
        self.to_room = true;
        self.request_leave();
    }

    /// Say `text` to the match ([`Session::say`]).
    pub fn say(&mut self, text: &str) {
        self.session.say(text);
    }

    /// "Leave match" was asked.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn leaving(&self) -> bool {
        self.leave.is_some()
    }
}

/// The connection a match handed back to its room ("Back to room"):
/// the lobby takes it from here (`net::lobby`), with the peers it is
/// still connected to.
#[derive(Resource)]
pub struct BackToRoom {
    pub transport: Box<dyn Transport>,
    pub peers: Vec<PeerId>,
}

/// The browser tab is hidden (web: `visibilitychange`; never set
/// natively). The driver tells the match ([`Session::set_away`]).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabHidden(pub bool);

/// A peer came or went, for a few seconds on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Left(PeerId),
    TimedOut(PeerId),
    /// Back on its seat.
    Returned(PeerId),
    /// Joined the running match to watch (or on a seat nobody had).
    Joined(PeerId),
}

/// Display names of the peers (the lobby fills it; the player list
/// falls back to the short [`PeerId`] form).
#[derive(Resource, Clone, Debug, Default)]
pub struct PeerNames(pub std::collections::BTreeMap<PeerId, String>);

impl PeerNames {
    pub fn name(&self, peer: PeerId) -> String {
        self.0
            .get(&peer)
            .cloned()
            .unwrap_or_else(|| peer.to_string())
    }
}

impl FromWorld for NetSession {
    fn from_world(world: &mut World) -> Self {
        if let Some(next) = world.remove_resource::<super::replay_log::NextReplay>() {
            return Self::from_replay(next);
        }
        match world.remove_resource::<NextNetMatch>() {
            Some(next) => Self::from_next(next),
            None => Self::new(Session::solo()),
        }
    }
}

/// A networked match to start at the next match swap (the lobby, M2,
/// inserts it with the `Start` it got).
#[derive(Resource)]
pub struct NextNetMatch {
    pub config: SessionConfig,
    pub transport: Box<dyn Transport>,
    pub catch_up: CatchUp,
    /// M3: joining a running match ([`Session::late`]): its host and
    /// the peers that already left.
    pub join: Option<(PeerId, Vec<PeerId>)>,
    /// M3, host: what lets late joiners in ([`Session::set_door`]).
    pub door: Option<super::rejoin::Door>,
}

/// Where desync dumps go (native). Default: `desync-dumps/` in the
/// project root (next to the executable in a release package).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Clone, Debug)]
pub struct DumpDir(pub std::path::PathBuf);

/// The lockstep state as the UI shows it. Rewritten every frame; never
/// read by the sim. (The HUD's network overlay, `ui::hud::net_overlay`,
/// shows it: player list, banners, notices.)
#[derive(Resource, Clone, Debug, Default)]
pub struct NetStatus {
    pub networked: bool,
    /// Everyone in the match: seat, ping, gone.
    pub peers: Vec<PeerInfo>,
    pub delay: u32,
    /// Stalled for a turn (guest) or for players to show up (host): who,
    /// and since when (the driver's clock).
    pub waiting: Option<(Vec<PeerId>, Duration)>,
    /// A desync, and where this peer's state dump went.
    pub desync: Option<Desync>,
    pub ended: Option<Ended>,
    /// "Leave match" is under way.
    pub leaving: bool,
    /// The transport is closed: this peer is out of the match.
    pub closed: bool,
    /// Still connected to a networked match ([`Session::connected`]).
    pub connected: bool,
    /// Who came and went in the last [`NOTICE_SECS`].
    pub notices: Vec<Notice>,
    /// Joining late, still replaying the match: (ticks applied, the
    /// host's newest).
    pub catching_up: Option<(u32, u32)>,
    /// Playing a replay.
    pub replay: Option<ReplayStatus>,
    /// (Read by the WebRTC tests.)
    #[cfg_attr(not(test), allow(dead_code))]
    pub last_frame: FrameReport,
    pub now: Duration,
}

/// Where a replay is, for the viewer's bar.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayStatus {
    pub tick: u32,
    pub ticks: u32,
    pub rate: f32,
    pub hold: bool,
    /// The recorded speed at this tick (percent).
    pub speed: u16,
}

/// The game world as a [`TickRunner`].
struct WorldRunner<'w> {
    world: &'w mut World,
    step: Duration,
}

/// The player whose input the bus seals: the local seat, nobody when
/// spectating (`apply_sim_commands` too).
pub(crate) fn local_player(world: &World) -> Option<u8> {
    world
        .get_resource::<crate::units::player::LocalTeam>()
        .map_or(Some(0), |t| {
            (t.0 != crate::game_setup::SPECTATOR_TEAM).then_some(t.0)
        })
}

impl TickRunner for WorldRunner<'_> {
    fn speed(&self) -> GameSpeed {
        self.world
            .get_resource::<super::MatchControl>()
            .map_or_else(GameSpeed::default, |c| GameSpeed {
                factor: c.speed(),
                wanted: c.wanted_speed(),
                paused: c.paused,
            })
    }

    fn seal(&mut self, delay: u32) -> Vec<CommandFrame> {
        let local = local_player(self.world);
        let Some(mut bus) = self.world.get_resource_mut::<CommandBus>() else {
            return Vec::new();
        };
        bus.routed = true;
        bus.input_delay = delay;
        bus.reseal(local);
        std::mem::take(&mut bus.outbox)
            .into_iter()
            .filter(|f| !f.commands.is_empty())
            .collect()
    }

    fn run(&mut self, turn: &Turn) {
        if let Some(mut bus) = self.world.get_resource_mut::<CommandBus>() {
            bus.deliver_turn(turn);
        }
        // What Bevy's `run_fixed_main_schedule` does per step.
        self.world
            .resource_mut::<Time<Fixed>>()
            .advance_by(self.step);
        let fixed = self.world.resource::<Time<Fixed>>().as_generic();
        *self.world.resource_mut::<Time>() = fixed;
        let _ = self.world.try_run_schedule(bevy::app::FixedMain);
    }

    fn hash(&mut self) -> u64 {
        super::state::capture(self.world).sim_hash
    }

    fn dump(&mut self, session: &Session, d: &Desync) -> String {
        let me = session.me();
        let mut text = format!(
            "kernel-panic desync dump\npeer {me} ({}), player {:?}\nhashes differed at frame {} (peers {:?}); stopped after {} ticks\n",
            if session.is_host() { "host" } else { "guest" },
            session.player(),
            d.frame,
            d.peers,
            session.applied(),
        );
        let hash = super::state::capture(self.world).sim_hash;
        text.push_str(&format!("state hash now {hash:016x}\n"));
        for line in super::state::describe(self.world) {
            text.push_str(&line);
            text.push('\n');
        }
        let name = format!(
            "desync-f{}-t{}-{}.txt",
            d.frame,
            session.applied(),
            me.to_string().replace("peer-", "p")
        );
        let place = write_dump(self.world, &name, &text);
        error!("desync at frame {}: state dumped to {place}", d.frame);
        place
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn write_dump(world: &World, name: &str, text: &str) -> String {
    let dir = world
        .get_resource::<DumpDir>()
        .map(|d| d.0.clone())
        .unwrap_or_else(|| crate::paths::project_root().join("desync-dumps"));
    let path = dir.join(name);
    match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text)) {
        Ok(()) => path.display().to_string(),
        Err(e) => {
            error!("cannot write {}: {e}; the dump follows", path.display());
            for line in text.lines() {
                error!("{line}");
            }
            "the log".into()
        }
    }
}

/// The web: the dump goes to the console and is offered as a download.
#[cfg(target_arch = "wasm32")]
fn write_dump(_world: &World, name: &str, text: &str) -> String {
    for line in text.lines() {
        error!("{line}");
    }
    if super::replay_log::download(name, text.as_bytes()) {
        format!("the console and the download {name}")
    } else {
        "the console".into()
    }
}

/// Replaces Bevy's fixed-step loop (see the module doc).
pub fn drive_fixed_main(world: &mut World) {
    let delta = world.resource::<Time<Virtual>>().delta();
    let now = world.resource::<Time<Real>>().elapsed();
    let step = world.resource::<Time<Fixed>>().timestep();
    world.resource_scope(|world, mut net: Mut<NetSession>| {
        let net = &mut *net;
        if !net.seated {
            net.seated = true;
            if net.session.is_networked() {
                seat_networked(world, &mut net.session);
            }
        }
        let mut runner = WorldRunner { world, step };
        let rep = drive_frame(
            &mut net.session,
            &mut net.pacer,
            &mut runner,
            delta,
            now,
            step,
        );
        if let Some(since) = net.leave.as_mut() {
            let since = *since.get_or_insert(now);
            let back = leave_step(
                &mut net.session,
                &mut runner,
                now.saturating_sub(since),
                net.to_room,
            );
            if let Some((transport, peers)) = back {
                runner
                    .world
                    .insert_resource(BackToRoom { transport, peers });
            }
        }
        // The render blend: how far the clock is into the next tick.
        let mut fixed = world.resource_mut::<Time<Fixed>>();
        let over = fixed.overstep();
        fixed.discard_overstep(over);
        fixed.accumulate_overstep(net.pacer.acc.min(step));
        let virt = world.resource::<Time<Virtual>>().as_generic();
        *world.resource_mut::<Time>() = virt;

        if let Some(hidden) = world.get_resource::<TabHidden>() {
            net.session.set_away(hidden.0);
        }
        let names = world
            .get_resource::<PeerNames>()
            .cloned()
            .unwrap_or_default();
        let mut said = Vec::new();
        for ev in net.session.take_events() {
            match ev {
                SessionEvent::Chat { from, text } => said.push(ChatLine {
                    from,
                    name: names.name(from),
                    text,
                }),
                SessionEvent::Left(peer) => {
                    info!("{peer} left the match");
                    net.notices.push((now, Notice::Left(peer)));
                }
                SessionEvent::TimedOut(peer) => {
                    net.notices.push((now, Notice::TimedOut(peer)));
                }
                SessionEvent::HostLeft => warn!("the host left: the match is over"),
                SessionEvent::HostLost => warn!("lost the connection to the host"),
                SessionEvent::Dropped => warn!("the host dropped us from the match"),
                SessionEvent::Desync(d) => error!(
                    "desync at frame {}: {:?} disagree with the host; stopping after tick {}",
                    d.frame, d.peers, d.stop_at
                ),
                SessionEvent::Joined {
                    peer,
                    player,
                    returned,
                    name,
                } => {
                    info!(
                        "{peer} ({name}) joined the match{}",
                        player.map_or(" to watch".to_string(), |s| format!(" on seat {s}"))
                    );
                    world
                        .get_resource_or_insert_with(PeerNames::default)
                        .0
                        .insert(peer, name);
                    net.notices.push((
                        now,
                        if returned {
                            Notice::Returned(peer)
                        } else {
                            Notice::Joined(peer)
                        },
                    ));
                }
            }
        }
        if !said.is_empty() {
            let mut log = world.get_resource_or_insert_with(ChatLog::default);
            for line in said {
                log.push(now, line);
            }
        }
        net.notices
            .retain(|(at, _)| now.saturating_sub(*at) < NOTICE_SECS);
        let notices = net.notices.iter().map(|(_, n)| *n).collect();
        let leaving = net.leave.is_some();
        let s = &net.session;
        let waiting = net
            .pacer
            .starved_since
            .map(|since| (s.waiting_for(), since));
        let catching_up = net
            .pacer
            .fast_forward
            .then(|| (s.applied(), s.host_newest().map_or(0, |n| n + 1)));
        let replay = s.replay_end().map(|ticks| ReplayStatus {
            tick: s.applied(),
            ticks,
            rate: net.pacer.rate,
            hold: net.pacer.hold,
            speed: world
                .get_resource::<super::MatchControl>()
                .map_or(100, |c| c.internal_percent),
        });
        let mut status = world.get_resource_or_insert_with(NetStatus::default);
        *status = NetStatus {
            networked: s.is_networked(),
            peers: if s.is_networked() {
                s.peers()
            } else {
                Vec::new()
            },
            delay: s.delay(),
            waiting,
            desync: s.desync().cloned(),
            ended: s.ended(),
            leaving,
            closed: s.closed(),
            connected: s.connected(),
            notices,
            catching_up,
            replay,
            last_frame: rep,
            now,
        };
    });
}

/// The first frame of a networked match: the roster's seat becomes the
/// local team, the host's seat goes into the rules (unless the lobby set
/// it; every peer has the same roster, so every peer fills in the same),
/// and the host checks what it sequences against those rules.
fn seat_networked(world: &mut World, session: &mut Session) {
    let team = session
        .player()
        .unwrap_or(crate::game_setup::SPECTATOR_TEAM);
    world.insert_resource(crate::units::player::LocalTeam(team));
    let host = session.host_player();
    let Some(mut active) = world.get_resource_mut::<crate::game_setup::ActiveMatch>() else {
        return;
    };
    if active.0.rules.host.is_none() {
        active.0.rules.host = host;
    }
    let active = crate::game_setup::ActiveMatch::clone(&active);
    session.set_admit(Box::new(move |player, cmd| {
        super::apply::rules_admit(Some(&active), player, cmd).map_err(|why| format!("{why:?}"))
    }));
}

/// One frame of "Leave match", `waited` after it was asked: send what
/// the player issued (the resignation) even if no tick runs, then close
/// once the host has it — at once for the host, or when the match is
/// over anyway — or after [`LEAVE_GRACE`]. With `to_room` the
/// transport is handed back instead of closed ([`Session::hand_back`]):
/// it and the peers it is connected to.
pub(crate) fn leave_step(
    s: &mut Session,
    r: &mut impl TickRunner,
    waited: Duration,
    to_room: bool,
) -> Option<(Box<dyn Transport>, Vec<PeerId>)> {
    if s.closed() {
        return None;
    }
    if !s.halted() {
        for frame in r.seal(s.delay()) {
            s.submit(frame);
        }
    }
    if s.is_host() || s.ended().is_some() || s.unacked() == 0 || waited >= LEAVE_GRACE {
        if to_room {
            return s.hand_back();
        }
        s.close();
    }
    None
}

/// Runs the fixed sim through [`drive_fixed_main`] instead of Bevy's
/// fixed-step loop.
pub struct LockstepPlugin;

impl Plugin for LockstepPlugin {
    fn build(&self, app: &mut App) {
        app.init_match_resource::<NetSession>()
            .init_resource::<NetStatus>()
            // Bevy's own loop is off; ours runs in its place.
            .configure_sets(
                RunFixedMainLoop,
                RunFixedMainLoopSystems::FixedMainLoop.run_if(|| false),
            )
            .add_systems(
                RunFixedMainLoop,
                drive_fixed_main
                    .after(RunFixedMainLoopSystems::BeforeFixedMainLoop)
                    .before(RunFixedMainLoopSystems::AfterFixedMainLoop),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rendering::interpolation::{SimInterpolationPlugin, SimPose};
    use bevy::time::TimeUpdateStrategy;

    #[derive(Component)]
    struct Mover;

    #[derive(Resource, Default)]
    struct Ticks(u32);

    fn advance(mut q: Query<&mut Transform, With<Mover>>, mut n: ResMut<Ticks>) {
        n.0 += 1;
        for mut tf in &mut q {
            tf.translation.x += 1.0;
        }
    }

    /// Single player through the driver, frames of uneven length that
    /// run 0, 1, 2 and 3 ticks: as many ticks as Bevy's own fixed loop
    /// would run, and the drawn pose (the `SimPose` blend) never steps
    /// back and trails the sim by at most one tick.
    #[test]
    fn uneven_frames_stay_smooth() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((SimInterpolationPlugin, LockstepPlugin))
            .insert_resource(Time::<Fixed>::from_hz(crate::sim::SIMULATION_HZ))
            .init_resource::<Ticks>()
            .add_systems(FixedUpdate, advance);
        let e = app
            .world_mut()
            .spawn((Mover, Transform::default(), SimPose::default()))
            .id();
        app.update();
        let pattern_ms = [16u64, 5, 70, 100, 9, 33, 0, 50, 16, 16, 120, 4, 34];
        let (mut last_x, mut total) = (0.0f32, Duration::ZERO);
        let mut seen = [false; 4];
        for f in 0..400 {
            let dt = Duration::from_millis(pattern_ms[f % pattern_ms.len()]);
            total += dt;
            app.insert_resource(TimeUpdateStrategy::ManualDuration(dt));
            let before = app.world().resource::<Ticks>().0;
            app.update();
            let ran = app.world().resource::<Ticks>().0 - before;
            seen[ran.min(3) as usize] = true;
            let x = app.world().get::<Transform>(e).unwrap().translation.x;
            let sim = app.world().resource::<Ticks>().0 as f32;
            assert!(x >= last_x - 1e-4, "frame {f}: drawn {x} after {last_x}");
            assert!(
                x <= sim + 1e-4 && sim - x <= 1.0 + 1e-4,
                "frame {f}: drawn {x}, sim {sim}"
            );
            last_x = x;
        }
        assert_eq!(
            seen, [true; 4],
            "0, 1, 2 and 3 ticks per frame all happened"
        );
        // Bevy's loop: ⌊time / step⌋ ticks (no frame here reaches the
        // virtual clock's delta cap).
        let step = Duration::from_nanos(1_000_000_000 / 30);
        let expect = (total.as_nanos() / step.as_nanos()) as u32;
        let got = app.world().resource::<Ticks>().0;
        assert!(
            got.abs_diff(expect) <= 1,
            "{got} ticks, Bevy's loop {expect} ({total:?})"
        );
    }
}

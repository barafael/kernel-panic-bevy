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
//! - **Game speed and pause** come from the sim ([`GameSpeed`], set by
//!   the speed / pause commands as they apply, so every peer changes
//!   pace on the same tick).
//! - **Desync:** every peer stops at the tick the host names and writes
//!   a state dump ([`DumpDir`]; the console and a download on the web).
//!   The match stays paused (Spring plays on).

use std::time::Duration;

use bevy::app::{RunFixedMainLoop, RunFixedMainLoopSystems};
use bevy::platform::time::Instant;
use bevy::prelude::*;

use super::session::{Desync, Ended, HASH_INTERVAL, Session, SessionConfig, SessionEvent};
use super::transport::{PeerId, Transport};
use super::wire::Turn;
use super::{CommandBus, CommandFrame, SimCommand};
use crate::map_loading::MatchScopeAppExt;

/// The sim's pace, as the speed / pause commands set it (they apply in
/// the sim, so every peer reads the same value at the same tick). Only
/// the driver reads it; it is not hashed.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct GameSpeed {
    /// Game seconds per real second (Spring's `userSpeedFactor`).
    pub factor: f32,
    pub paused: bool,
}

impl Default for GameSpeed {
    fn default() -> Self {
        Self {
            factor: 1.0,
            paused: false,
        }
    }
}

/// Whether a command, sequenced while the sim is paused, should be let
/// through at once (the host cuts the next turn for it). Until the pause
/// command exists every command does; then only the unpause should
/// (`m1-runtime.md` §3, the shape agreed with the commands work).
pub fn resumes_sim(_cmd: &SimCommand) -> bool {
    true
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

/// The frame clock: game time not yet turned into ticks.
#[derive(Clone, Debug, Default)]
pub struct Pacer {
    pub acc: Duration,
    pub catch_up: CatchUp,
    /// The guest has been waiting for a turn since.
    pub starved_since: Option<Duration>,
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
    } else if !s.halted() && !speed.paused {
        p.acc += delta.mul_f64(speed.factor.max(0.0) as f64);
    }
    let t0 = p.catch_up.budget.map(|_| Instant::now());
    while !s.halted() {
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
            run_one(s, r, now);
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
            run_one(s, r, now);
            rep.paced += 1;
        } else if p.acc >= step {
            p.acc -= step;
            run_one(s, r, now);
            rep.paced += 1;
        } else if ready > p.catch_up.target_backlog
            && rep.extra < p.catch_up.max_extra
            && p.catch_up
                .budget
                .zip(t0)
                .is_none_or(|(b, t0)| t0.elapsed() < b)
        {
            run_one(s, r, now);
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
    if rep.starved {
        // Bank no more than one tick, or a burst of arrivals would play
        // back in fast-forward (cake's `Match::advance`).
        p.acc = p.acc.min(step);
        p.starved_since.get_or_insert(now);
    } else if rep.paced + rep.extra > 0 {
        p.starved_since = None;
    }
    if s.halted()
        && let Some(d) = s.desync().filter(|d| !d.dumped).cloned()
    {
        let _where = r.dump(s, &d);
        if let Some(d) = s.desync_mut() {
            d.dumped = true;
        }
    }
    s.flush(now);
    rep
}

fn run_one(s: &mut Session, r: &mut impl TickRunner, now: Duration) {
    for frame in r.seal(s.delay()) {
        s.submit(frame);
    }
    if s.is_host() {
        s.cut(now);
    }
    let Some(turn) = s.pop() else {
        return;
    };
    r.run(&turn);
    let frame = turn.tick + 1;
    if s.is_networked() && frame.is_multiple_of(HASH_INTERVAL) {
        let hash = r.hash();
        s.record_hash(frame, hash);
    }
}

// --- the game side -----------------------------------------------------

/// The match's lockstep state. Match-scoped: every match starts a fresh
/// session — networked if a [`NextNetMatch`] is waiting, else solo.
#[derive(Resource)]
pub struct NetSession {
    pub session: Session,
    pub pacer: Pacer,
    /// The networked seat was made the local team.
    seated: bool,
}

impl NetSession {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            pacer: Pacer::default(),
            seated: false,
        }
    }
}

impl FromWorld for NetSession {
    fn from_world(world: &mut World) -> Self {
        match world.remove_resource::<NextNetMatch>() {
            Some(next) => {
                let mut s = Self::new(Session::networked(next.config, next.transport));
                s.pacer.catch_up = next.catch_up;
                s
            }
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
}

/// Where desync dumps go (native). Default: `desync-dumps/` in the
/// project root (next to the executable in a release package).
#[cfg(not(target_arch = "wasm32"))]
#[derive(Resource, Clone, Debug)]
pub struct DumpDir(pub std::path::PathBuf);

/// The lockstep state as the UI shows it. Rewritten every frame; never
/// read by the sim. (The banner reads part of it; the rest is for the
/// M2 lobby's network overlay and the tests.)
#[derive(Resource, Clone, Debug, Default)]
#[allow(dead_code)]
pub struct NetStatus {
    pub networked: bool,
    pub host: bool,
    pub delay: u32,
    /// Stalled for a turn (guest) or for players to show up (host): who,
    /// and since when (the driver's clock).
    pub waiting: Option<(Vec<PeerId>, Duration)>,
    pub desync: Option<Desync>,
    /// Where this peer's desync dump went.
    pub dump: Option<String>,
    pub ended: Option<Ended>,
    pub last_frame: FrameReport,
    pub now: Duration,
}

/// The game world as a [`TickRunner`].
struct WorldRunner<'w> {
    world: &'w mut World,
    step: Duration,
    dump: Option<String>,
}

/// The player whose input the bus seals: the local seat, nobody when
/// spectating (`apply_sim_commands` decides the same way).
fn local_player(world: &World) -> Option<u8> {
    world
        .get_resource::<crate::units::player::LocalTeam>()
        .map_or(Some(0), |t| {
            (t.0 != crate::game_setup::SPECTATOR_TEAM).then_some(t.0)
        })
}

impl TickRunner for WorldRunner<'_> {
    fn speed(&self) -> GameSpeed {
        self.world
            .get_resource::<GameSpeed>()
            .copied()
            .unwrap_or_default()
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
        self.dump = Some(place.clone());
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
    let downloaded = (|| {
        use wasm_bindgen::JsCast;
        let parts = js_sys::Array::of1(&wasm_bindgen::JsValue::from_str(text));
        let blob = web_sys::Blob::new_with_str_sequence(&parts).ok()?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
        let document = web_sys::window()?.document()?;
        let a = document
            .create_element("a")
            .ok()?
            .dyn_into::<web_sys::HtmlAnchorElement>()
            .ok()?;
        a.set_href(&url);
        a.set_download(name);
        a.click();
        web_sys::Url::revoke_object_url(&url).ok()?;
        Some(())
    })()
    .is_some();
    if downloaded {
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
        // A networked seat is the local team (the setup's first human
        // seat otherwise).
        if !net.seated {
            net.seated = true;
            if net.session.is_networked() {
                let team = net
                    .session
                    .player()
                    .unwrap_or(crate::game_setup::SPECTATOR_TEAM);
                world.insert_resource(crate::units::player::LocalTeam(team));
            }
        }
        let mut runner = WorldRunner {
            world,
            step,
            dump: None,
        };
        let rep = drive_frame(
            &mut net.session,
            &mut net.pacer,
            &mut runner,
            delta,
            now,
            step,
        );
        let dumped = runner.dump.take();
        // The render blend: how far the clock is into the next tick.
        let mut fixed = world.resource_mut::<Time<Fixed>>();
        let over = fixed.overstep();
        fixed.discard_overstep(over);
        fixed.accumulate_overstep(net.pacer.acc.min(step));
        let virt = world.resource::<Time<Virtual>>().as_generic();
        *world.resource_mut::<Time>() = virt;

        for ev in net.session.take_events() {
            match ev {
                SessionEvent::Left(peer) => info!("{peer} left the match"),
                SessionEvent::HostLeft => warn!("the host left: the match is over"),
                SessionEvent::Desync(d) => error!(
                    "desync at frame {}: {:?} disagree with the host; stopping after tick {}",
                    d.frame, d.peers, d.stop_at
                ),
            }
        }
        let s = &net.session;
        let waiting = if !s.started() {
            Some((s.waiting_for(), now))
        } else {
            net.pacer
                .starved_since
                .map(|since| (s.waiting_for(), since))
        };
        let mut status = world.get_resource_or_insert_with(NetStatus::default);
        let dump = dumped.or(status.dump.take().filter(|_| s.desync().is_some()));
        *status = NetStatus {
            networked: s.is_networked(),
            host: s.is_host(),
            delay: s.delay(),
            waiting,
            desync: s.desync().cloned(),
            dump,
            ended: s.ended(),
            last_frame: rep,
            now,
        };
    });
}

/// The banner: waiting, desync, match over. Out of the sim.
#[derive(Component)]
struct NetBanner;

fn net_banner(
    mut commands: Commands,
    status: Option<Res<NetStatus>>,
    mut banner: Query<(Entity, &mut Text), With<NetBanner>>,
) {
    let Some(status) = status.filter(|s| s.networked) else {
        for (e, _) in &banner {
            commands.entity(e).despawn();
        }
        return;
    };
    let message = if let Some(d) = &status.desync {
        Some(format!(
            "Desync at frame {} — the game is stopped.\nState dump: {}",
            d.frame,
            status.dump.as_deref().unwrap_or("pending")
        ))
    } else if status.ended == Some(Ended::HostLeft) {
        Some("The host left — the match is over.".to_string())
    } else {
        status.waiting.as_ref().and_then(|(who, since)| {
            // Brief stalls are just jitter.
            (status.now.saturating_sub(*since) > Duration::from_millis(300)).then(|| {
                let names: Vec<String> = who.iter().map(|p| p.to_string()).collect();
                format!("Waiting for {}…", names.join(", "))
            })
        })
    };
    match (message, banner.single_mut()) {
        (Some(m), Ok((_, mut text))) => {
            if text.0 != m {
                text.0 = m;
            }
        }
        (Some(m), Err(_)) => {
            commands.spawn((
                NetBanner,
                Text::new(m),
                TextFont::from_font_size(22.0),
                TextColor(Color::srgb(1.0, 0.85, 0.3)),
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(72.0),
                    left: Val::Px(16.0),
                    ..default()
                },
                GlobalZIndex(50),
            ));
        }
        (None, Ok((e, _))) => commands.entity(e).despawn(),
        (None, Err(_)) => {}
    }
}

/// Runs the fixed sim through [`drive_fixed_main`] instead of Bevy's
/// fixed-step loop.
pub struct LockstepPlugin;

impl Plugin for LockstepPlugin {
    fn build(&self, app: &mut App) {
        app.init_match_resource::<NetSession>()
            .init_match_resource::<GameSpeed>()
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
            )
            .add_systems(Update, net_banner);
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

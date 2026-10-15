//! Speed control: the host slows the game down for a player whose
//! machine can't keep up, and speeds it back up once it can — Spring's
//! lag protection (`net-experiments/m3-speed.md`).
//!
//! ## Upstream
//!
//! - Every client measures the share of wall time its sim took over
//!   the last second and sends it to the server once a second
//!   (`CGame::SendClientProcUsage`, `rts/Net/NetCommands.cpp:71-117`,
//!   `NETMSG_CPU_USAGE`).
//! - Every 2 s (`playerInfoTime`, `rts/Net/GameServer.cpp:99`, `:811`)
//!   the server runs `CGameServer::LagProtection` (`:881-967`): it
//!   broadcasts every player's CPU and ping (`NETMSG_PLAYERINFO`), takes
//!   the **median** CPU of the playing players (`SpeedControl` 1, the
//!   default, aiming at 60 %) or the **highest** (`SpeedControl` 2,
//!   aiming at 75 %; `:71`, `:914-942`), and sets the game speed to
//!   `internalSpeed / ref * wanted`, clamped to `[0.1, userSpeed]` and
//!   averaged with the current speed (`:948-952`). Spectators don't
//!   count (`:902`), the server's own client does; a reconnecting
//!   player is left out of the maximum until it is less than 2 s behind
//!   (`:900-904`).
//! - The new speed goes to everyone as `NETMSG_INTERNAL_SPEED`
//!   (`CGameServer::InternalSpeedChange`, `:3076`); a player's speed
//!   request moves it at once only downwards, or when nothing slowed the
//!   game (`CGameServer::UserSpeedChange`, `:3084-3093`).
//!
//! ## Here
//!
//! - [`LoadMeter`]: every networked peer measures its **sim load**, the
//!   share of one core the sim needs at the current speed: the mean wall
//!   time of a tick × the ticks per second the speed asks for. Spring
//!   measures the share the sim took, which stops growing near 100 %
//!   once the peer falls behind; the need keeps growing, so one step
//!   lands near the speed the peer can hold. Together with how far
//!   behind it is (turns it holds and hasn't applied) it goes to the host
//!   once a second (`Msg::Load`).
//! - [`SpeedControl`]: every [`PROTECT_EVERY`] the host runs Spring's
//!   rule over the seated players' loads and its own and, when the speed
//!   changes, sequences [`SimCommand::InternalSpeed`] in the server's
//!   name ([`SERVER_PLAYER`]); every peer applies it on the same tick
//!   ([`MatchControl::internal_percent`]), which is what paces the tick
//!   driver.
//! - The host broadcasts everyone's load with the pings (`Msg::Loads`)
//!   for the player list's CPU column.
//!
//! **Policy:** [`SpeedPolicy::Max`] by default, Spring's
//! `SpeedControl` 2 — not its default (median). Under the median the
//! slowest player of a 1v1 or of a big team game is never waited for:
//! it plays further and further behind (Spring tolerates that; here a
//! guest more than a minute behind can no longer be served gap fills,
//! and the web build in Firefox is several times slower than the
//! others). The median stays available ([`Session::set_speed_policy`]).
//!
//! Single player has no speed control (Spring's runs, as a local server,
//! but a solo machine slows down by itself: the frame clock just gets
//! fewer ticks in).
//!
//! [`SimCommand::InternalSpeed`]: super::SimCommand::InternalSpeed
//! [`MatchControl::internal_percent`]: super::MatchControl::internal_percent
//! [`Session::set_speed_policy`]: super::session::Session::set_speed_policy

use std::collections::BTreeMap;
use std::time::Duration;

use super::control::{MAX_SPEED_PERCENT, MIN_SPEED_PERCENT};
use super::transport::PeerId;
use crate::sim::{DetFloat, SIMULATION_HZ};

/// The sender of host-issued commands that belong to no seat (Spring's
/// `SERVER_PLAYER`, `rts/Net/GameServer.h:28`): only
/// [`SimCommand::InternalSpeed`](super::SimCommand::InternalSpeed).
pub const SERVER_PLAYER: u8 = 255;

/// How often a peer reports its load (Spring: once a second,
/// `NetCommands.cpp:76`).
pub const REPORT_EVERY: Duration = Duration::from_secs(1);

/// How often the host adjusts the speed (Spring's `playerInfoTime`).
pub const PROTECT_EVERY: Duration = Duration::from_secs(2);

/// Reports older than this don't count (a peer that stopped reporting:
/// paused, or gone without the transport noticing yet).
const STALE: Duration = Duration::from_secs(5);

/// Loads above this are a broken report, not a slow machine.
const MAX_LOAD: f32 = 10.0;

/// Which load sets the speed (Spring's `SpeedControl`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpeedPolicy {
    /// The median load, aiming at 60 % (Spring's default, 1). Not used
    /// in the game (see the module doc); the tests compare it.
    #[cfg_attr(not(test), allow(dead_code))]
    Median,
    /// The highest load, aiming at 75 % (Spring's 2): the slowest
    /// player keeps up.
    #[default]
    Max,
}

impl SpeedPolicy {
    /// The load the policy aims at (`GameServer.cpp:942`: below 100 %,
    /// because ticks vary and a long one must still fit).
    pub fn wanted(self) -> f32 {
        match self {
            SpeedPolicy::Median => 0.60,
            SpeedPolicy::Max => 0.75,
        }
    }

    /// The reference load of `loads` (not empty).
    fn reference(self, loads: &mut [f32]) -> f32 {
        match self {
            SpeedPolicy::Max => loads.iter().fold(0.0f32, |a, &b| a.det_max(b)),
            SpeedPolicy::Median => {
                loads.sort_by(f32::total_cmp);
                let mid = loads.len() / 2;
                if loads.len().is_multiple_of(2) {
                    (loads[mid] + loads[mid - 1]) / 2.0
                } else {
                    loads[mid]
                }
            }
        }
    }
}

/// One step of Spring's `CGameServer::LagProtection`
/// (`GameServer.cpp:929-966`): the game speed (game seconds per real
/// second) for `loads` measured at `internal`, with the players asking
/// for `wanted`; `None` when nothing is known.
pub fn lag_protection(
    policy: SpeedPolicy,
    loads: &[f32],
    internal: f32,
    wanted: f32,
) -> Option<f32> {
    let mut loads: Vec<f32> = loads.iter().map(|l| l.det_clamp(0.0, MAX_LOAD)).collect();
    if loads.is_empty() {
        return None;
    }
    let reference = policy.reference(&mut loads);
    if reference <= 0.0 {
        return None;
    }
    let lo = f32::from(MIN_SPEED_PERCENT) / 100.0;
    // Linear: the load scales with the speed. Never faster than asked.
    let new = (internal / reference * policy.wanted()).det_clamp(lo, wanted.det_max(lo));
    // Halfway there, against a spike in one report.
    Some((new + internal) / 2.0)
}

/// A peer's sim load, measured over [`REPORT_EVERY`].
#[derive(Clone, Debug, Default)]
pub struct LoadMeter {
    since: Option<Duration>,
    busy: Duration,
    ticks: u32,
}

impl LoadMeter {
    /// A tick took `cost` of wall time.
    pub fn tick(&mut self, cost: Duration) {
        self.busy += cost;
        self.ticks += 1;
    }

    /// Once per [`REPORT_EVERY`]: the window's load at game speed
    /// `speed` (1.0 = one core's worth of sim). `None` until a window is
    /// complete, and for a window without ticks (paused, stalled).
    pub fn report(&mut self, now: Duration, speed: f32) -> Option<f32> {
        let since = *self.since.get_or_insert(now);
        if now.saturating_sub(since) < REPORT_EVERY {
            return None;
        }
        let load = (self.ticks > 0).then(|| {
            let per_tick = self.busy.as_secs_f64() / f64::from(self.ticks);
            (per_tick * SIMULATION_HZ * f64::from(speed.det_max(0.0))) as f32
        });
        *self = Self {
            since: Some(now),
            ..Default::default()
        };
        load
    }
}

/// What a peer said about itself (or the host measured of itself).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerLoad {
    /// The sim load ([`LoadMeter`]).
    pub cpu: f32,
    /// Turns it holds and hasn't applied yet.
    pub behind: u32,
    /// When it arrived.
    pub at: Duration,
}

/// The host's speed control: the latest load of every peer and Spring's
/// rule over them.
#[derive(Clone, Debug, Default)]
pub struct SpeedControl {
    pub policy: SpeedPolicy,
    loads: BTreeMap<PeerId, PeerLoad>,
    last: Option<Duration>,
}

impl SpeedControl {
    pub fn report(&mut self, peer: PeerId, load: PeerLoad) {
        self.loads.insert(peer, load);
    }

    pub fn load(&self, peer: PeerId) -> Option<&PeerLoad> {
        self.loads.get(&peer)
    }

    /// Every [`PROTECT_EVERY`]: the internal speed in percent for the
    /// fresh loads of `counted` (the playing peers and the host), if it
    /// differs from `internal` (percent; `wanted` the players' speed).
    pub fn update(
        &mut self,
        now: Duration,
        counted: impl IntoIterator<Item = PeerId>,
        internal: u16,
        wanted: u16,
    ) -> Option<u16> {
        if self.last.is_some_and(|t| now < t + PROTECT_EVERY) {
            return None;
        }
        self.last = Some(now);
        let loads: Vec<f32> = counted
            .into_iter()
            .filter_map(|p| self.loads.get(&p))
            .filter(|l| now.saturating_sub(l.at) <= STALE)
            .map(|l| l.cpu)
            .collect();
        let speed = lag_protection(
            self.policy,
            &loads,
            f32::from(internal) / 100.0,
            f32::from(wanted) / 100.0,
        )?;
        let percent = ((speed * 100.0).round() as u16).clamp(MIN_SPEED_PERCENT, MAX_SPEED_PERCENT);
        (percent != internal).then_some(percent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spring's numbers: a peer at twice the wanted load halves the
    /// speed in steps, each half of the way; light loads go back up to
    /// the players' speed and no further.
    #[test]
    fn lag_protection_follows_spring() {
        let max = SpeedPolicy::Max;
        let step = |policy, loads: &[f32], internal, wanted| {
            lag_protection(policy, loads, internal, wanted).map(|s| (s * 1000.0).round() / 1000.0)
        };
        // Load 1.5 at 1×: 1 / 1.5 × 0.75 = 0.5, halfway: 0.75.
        assert_eq!(step(max, &[0.2, 1.5, 0.3], 1.0, 1.0), Some(0.75));
        // Light loads: up, but never past what the players asked for.
        assert_eq!(step(max, &[0.1, 0.2], 0.5, 1.0), Some(0.75));
        assert_eq!(step(max, &[0.1], 1.0, 1.0), Some(1.0));
        // Nothing measured.
        assert_eq!(step(max, &[], 1.0, 1.0), None);
        assert_eq!(step(max, &[0.0], 1.0, 1.0), None);
        // Never below 0.1.
        assert_eq!(step(max, &[10.0], 0.1, 1.0), Some(0.1));
        // The median of an even count averages the middle two
        // (`GameServer.cpp:919-923`): (0.3 + 0.9) / 2 = 0.6, on target.
        let med = SpeedPolicy::Median;
        assert_eq!(step(med, &[0.9, 0.2, 3.0, 0.3], 1.0, 1.0), Some(1.0));
        assert_eq!(step(med, &[1.2], 1.0, 1.0), Some(0.75));
    }

    /// Repeated steps settle where the slowest peer is at the wanted
    /// load, and return to full speed when it recovers.
    #[test]
    fn the_speed_settles_and_recovers() {
        let mut c = SpeedControl::default();
        let (host, slow) = (PeerId(1), PeerId(2));
        let mut internal = 100u16;
        // The slow peer needs 1.5 cores at 1×: it can hold 0.5×.
        for step in 0..12u64 {
            let now = Duration::from_secs(2 * step);
            let speed = f32::from(internal) / 100.0;
            for (peer, at_1x) in [(host, 0.2), (slow, 1.5)] {
                let load = PeerLoad {
                    cpu: at_1x * speed,
                    behind: 0,
                    at: now,
                };
                c.report(peer, load);
            }
            if let Some(p) = c.update(now, [host, slow], internal, 100) {
                internal = p;
            }
        }
        assert!((49..=51).contains(&internal), "settled at {internal} %");
        let mut steps = 0;
        while internal < 100 {
            steps += 1;
            assert!(steps < 12, "back up to 100 %: {internal}");
            let now = Duration::from_secs(100 + 2 * steps);
            for peer in [host, slow] {
                c.report(
                    peer,
                    PeerLoad {
                        cpu: 0.1,
                        behind: 0,
                        at: now,
                    },
                );
            }
            internal = c
                .update(now, [host, slow], internal, 100)
                .unwrap_or(internal);
        }
        // Not every frame: every 2 s.
        let now = Duration::from_secs(100 + 2 * steps);
        c.report(
            slow,
            PeerLoad {
                cpu: 5.0,
                behind: 0,
                at: now,
            },
        );
        assert_eq!(
            c.update(now + Duration::from_millis(500), [host, slow], 100, 100),
            None
        );
    }

    #[test]
    fn the_meter_reports_the_need_at_the_current_speed() {
        let mut m = LoadMeter::default();
        let s = Duration::from_secs;
        assert_eq!(m.report(s(0), 1.0), None, "a window starts");
        for _ in 0..30 {
            m.tick(Duration::from_millis(10));
        }
        let load = m.report(s(1), 2.0).unwrap();
        // 10 ms a tick at 60 ticks a second: 60 %.
        assert!((load - 0.6).abs() < 1e-4, "{load}");
        assert_eq!(m.report(s(2), 1.0), None, "no ticks: nothing to say");
    }
}

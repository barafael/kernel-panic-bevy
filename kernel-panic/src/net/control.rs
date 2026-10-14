//! Match control: game speed, pause, resignations and the cheat switch
//! — the state the [`SimCommand::SetSpeed`], [`SimCommand::Pause`],
//! [`SimCommand::Resign`] and [`SimCommand::Dev`] commands set when
//! they apply ([`MatchControl`]), and the in-game keys that issue speed
//! and pause.
//!
//! ## Speed and pause
//!
//! Spring keeps both outside the synced frame: the server owns the
//! frame clock, clamps a player's `NETMSG_USER_SPEED` to the setup's
//! `MinSpeed` / `MaxSpeed` (`CGameServer::UserSpeedChange`,
//! `Net/GameServer.cpp:3084`; defaults 0.1 and 20,
//! `Game/GameSetup.cpp:636`) and stops sending `NETMSG_NEWFRAME` while
//! paused; a client toggles `gs->paused` when `NETMSG_PAUSE` arrives
//! (`Net/NetCommands.cpp:384`). Here both are commands in the turn
//! stream, so every peer learns them at the same tick:
//!
//! - `SetSpeed { percent }` applies at its tick:
//!   [`MatchControl::speed`] is the wall-clock rate the tick driver
//!   paces ticks at (1.0 = 30 ticks per second). The driver's pacer is
//!   the one place the speed scales the clock (`driver::drive_frame`);
//!   `Time<Virtual>` stays at 1× (the dev tools' `KP_TIME_SCALE` and the
//!   recorder still set it, which speeds up everything, UI included).
//! - `Pause { on: true }` applies at the head of its tick; that tick
//!   runs to the end, then **no further tick runs** on any peer. The
//!   driver keeps sealing while paused; the host files what arrives on
//!   the next tick and cuts that turn at once for an unpause
//!   (`driver::resumes_sim`), which applies at its head, with whatever
//!   was ordered meanwhile.
//!
//! Neither changes sim state: the sim is a function of the turns, so
//! *when* a peer runs its ticks never matters. Pause and speed are
//! commands so that every peer agrees on them (and the HUD can say who
//! paused), not for determinism. Single player is a solo host on the
//! same path.

use std::collections::BTreeSet;

use bevy::prelude::*;

use serde::{Deserialize, Serialize};

use super::{CommandBus, IdList, NetPos, SimCommand, codec};
use crate::game_setup::AppState;
use crate::map_loading::MatchScopeAppExt;
use crate::units::content::definitions::UnitKind;

/// Slowest game speed, percent (Spring's default `MinSpeed` 0.1).
pub const MIN_SPEED_PERCENT: u16 = 10;
/// Fastest game speed, percent (Spring's default `MaxSpeed` 20).
pub const MAX_SPEED_PERCENT: u16 = 2000;

/// Dev tools and cheats ([`SimCommand::Dev`]): Spring's cheat-only
/// synced actions `/give`, `/destroy` and `/godmode`
/// (`Game/SyncedGameCommands.cpp`, `cheatRequired`; refused while
/// `gs->cheatEnabled` is off, `Game/IActionExecutor.h:111`). Here a
/// match allows cheats when it has at most one human seat (single
/// player, the dev tools' sandbox); the seats are part of the match
/// setup every peer agreed to, so every peer decides alike. Units are
/// any team's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevCommand {
    /// `count` units of `kind` for `team`, on a square grid from `at`,
    /// `spacing` elmos apart (`/give`).
    Spawn {
        #[serde(with = "codec::kind_index")]
        kind: UnitKind,
        team: u8,
        at: NetPos,
        count: u16,
        spacing: u16,
    },
    /// Remove outright: no death blast, no death script.
    Kill { units: IdList },
    /// Set health; 0 kills through the normal death path (`/destroy`).
    SetHealth { units: IdList, hp: u32 },
    /// Every player commands every team (`/godmode`).
    GodMode { on: bool },
}

/// Entities a local dev tool's `/give` spawned, for the tool to pick
/// up (the KP_BOT `last` selector). Only that tool inserts it; the sim
/// never reads it.
#[derive(Resource, Debug, Default)]
pub struct DevSpawned(pub Vec<Entity>);

/// What the match-control commands decided. Match-scoped, set only by
/// `apply_sim_commands`, the same on every peer.
#[derive(Resource, Debug, Clone, PartialEq, Eq)]
pub struct MatchControl {
    /// Game speed in percent of real time.
    pub speed_percent: u16,
    /// No tick runs after the one that applied `Pause { on: true }`
    /// until one applies `Pause { on: false }`.
    pub paused: bool,
    /// The player whose command paused (for the HUD).
    pub paused_by: Option<u8>,
    /// Players who resigned: spectators from then on, whose commands
    /// are refused.
    pub resigned: BTreeSet<u8>,
    /// Cheat (`/godmode`): every player commands every team.
    pub god_mode: bool,
}

impl Default for MatchControl {
    fn default() -> Self {
        Self {
            speed_percent: 100,
            paused: false,
            paused_by: None,
            resigned: BTreeSet::new(),
            god_mode: false,
        }
    }
}

impl MatchControl {
    /// The wall-clock rate the driver paces ticks at (1.0 = 30 Hz).
    pub fn speed(&self) -> f32 {
        f32::from(self.speed_percent) / 100.0
    }
}

/// Spring's `SpeedUp` action (`Game/UnsyncedGameCommands.cpp:2042`) in
/// percent: +10 below 2×, +20 below 5×, +50 below 10×, then +100.
pub fn speed_up(percent: u16) -> u16 {
    let step = match percent {
        0..200 => 10,
        200..500 => 20,
        500..1000 => 50,
        _ => 100,
    };
    percent.saturating_add(step).min(MAX_SPEED_PERCENT)
}

/// Spring's `SlowDown` action (`Game/UnsyncedGameCommands.cpp:2068`):
/// the step of the range the speed is in (ranges closed at the top).
pub fn slow_down(percent: u16) -> u16 {
    let step = match percent {
        0..=200 => 10,
        201..=500 => 20,
        501..=1000 => 50,
        _ => 100,
    };
    percent.saturating_sub(step).max(MIN_SPEED_PERCENT)
}

pub struct MatchControlPlugin;

impl Plugin for MatchControlPlugin {
    fn build(&self, app: &mut App) {
        app.init_match_resource::<MatchControl>()
            .init_resource::<super::apply::Refusals>()
            .add_systems(
                Update,
                match_control_keys.run_if(in_state(AppState::InGame)),
            );
    }
}

/// Pause and speed keys, Spring's default bindings
/// (`Game/UI/KeyBindings.cpp:65`, `:104`): Pause toggles,
/// Alt+= / Alt++ / Alt+numpad+ speed up, Alt+- / Alt+numpad- slow
/// down.
fn match_control_keys(
    keys: Res<ButtonInput<KeyCode>>,
    control: Option<Res<MatchControl>>,
    mut bus: ResMut<CommandBus>,
) {
    let Some(control) = control else {
        return;
    };
    if keys.just_pressed(KeyCode::Pause) {
        bus.issue(SimCommand::Pause {
            on: !control.paused,
        });
    }
    if !keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]) {
        return;
    }
    let percent = if keys.any_just_pressed([KeyCode::Equal, KeyCode::NumpadAdd]) {
        speed_up(control.speed_percent)
    } else if keys.any_just_pressed([KeyCode::Minus, KeyCode::NumpadSubtract]) {
        slow_down(control.speed_percent)
    } else {
        return;
    };
    if percent != control.speed_percent {
        bus.issue(SimCommand::SetSpeed { percent });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spring's steps, from 1× up and back down.
    #[test]
    fn speed_steps_follow_spring() {
        assert_eq!(speed_up(100), 110);
        assert_eq!(speed_up(190), 200);
        assert_eq!(speed_up(200), 220);
        assert_eq!(speed_up(500), 550);
        assert_eq!(speed_up(1000), 1100);
        assert_eq!(speed_up(2000), 2000);
        assert_eq!(slow_down(200), 190);
        assert_eq!(slow_down(220), 200);
        assert_eq!(slow_down(550), 500);
        assert_eq!(slow_down(1100), 1000);
        assert_eq!(slow_down(10), 10);
        // Up then down returns to the start.
        let mut s = 100;
        let ups: Vec<u16> = (0..30)
            .map(|_| {
                s = speed_up(s);
                s
            })
            .collect();
        for &u in ups.iter().rev().skip(1) {
            s = slow_down(s);
            assert_eq!(s, u);
        }
    }
}

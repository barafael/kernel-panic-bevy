//! Dev tool: the Multiplayer page's buttons pressed by themselves, for
//! `cargo testbox` (`xtask`), which launches several games into one
//! room on a local signaling server and wants a match without anyone
//! clicking.
//!
//! `KP_AUTO_START=<players>` (with `--room` / `KP_ROOM`): a guest sits
//! down on the first open seat and says it is ready; whichever game is
//! elected host adds seats until there are `<players>`, and starts the
//! match once that many players sit and nothing blocks the start
//! ([`Lobby::start_blocker`]). Every game gets the variable: who hosts
//! is only known once the room is up. The host also applies
//! `KP_AUTO_MAP=<stem>` (the room's map), `KP_AUTO_CHEATS=1` (cheats
//! on, so the bot's `spawn` / `kill` / `hp` work with several human
//! seats) and `KP_AUTO_AI=<n>` (`<n>` more seats, for AIs: the last open
//! seats, so the players' stay first).
//!
//! Everything goes through [`LobbyAction`]s, as the page's clicks do
//! (the map directly: the page only steps through the catalog). One
//! step per [`STEP`], so a guest's request reaches the host and the
//! room comes back before the next. Back in the room after a match
//! (the bot's `room`), a room full and ready again starts a rematch.
//!
//! Not compiled for wasm (no environment).
//!
//! [`Lobby::start_blocker`]: super::lobby::Lobby::start_blocker

use std::time::Duration;

use bevy::prelude::*;

use super::lobby::{LobbyAction, LobbyRoom, Occupant};
use crate::game_setup::AppState;

/// How often the autopilot looks at the room.
const STEP: Duration = Duration::from_millis(500);

pub struct AutopilotPlugin;

impl Plugin for AutopilotPlugin {
    fn build(&self, app: &mut App) {
        let var = |name: &str| std::env::var(name).ok();
        let Some(players) = var("KP_AUTO_START").and_then(|n| n.parse().ok()) else {
            return;
        };
        app.insert_resource(Autopilot {
            players,
            map: var("KP_AUTO_MAP"),
            cheats: var("KP_AUTO_CHEATS").is_some_and(|v| v != "0"),
            ai: var("KP_AUTO_AI").and_then(|n| n.parse().ok()).unwrap_or(0),
            step: Timer::new(STEP, TimerMode::Repeating),
        })
        .add_systems(
            Update,
            // `Update`: the page's actions are read in `PostUpdate`.
            autopilot.run_if(in_state(AppState::Menu)),
        );
    }
}

#[derive(Resource)]
struct Autopilot {
    /// Seated players the host waits for.
    players: usize,
    map: Option<String>,
    cheats: bool,
    /// AI seats the host adds.
    ai: usize,
    step: Timer,
}

fn autopilot(
    time: Res<Time>,
    mut auto: ResMut<Autopilot>,
    mut lobby: ResMut<LobbyRoom>,
    mut actions: MessageWriter<LobbyAction>,
) {
    if !auto.step.tick(time.delta()).just_finished() {
        return;
    }
    let Some(l) = lobby.lobby.as_mut() else {
        return;
    };
    let Some(room) = l.room() else {
        return;
    };
    if !l.is_host() {
        match l.my_seat() {
            None => {
                if let Some(seat) = room.seats.iter().position(|s| s.who == Occupant::Open) {
                    actions.write(LobbyAction::Occupant(seat as u8));
                }
            }
            Some(_) if !l.ready() => {
                actions.write(LobbyAction::Ready(true));
            }
            Some(_) => {}
        }
        return;
    }
    if let Some(map) = auto.map.as_deref()
        && room.map != map
    {
        info!("autopilot: map {map}");
        if let Err(why) = l.set_map(map) {
            warn!("autopilot: map {map}: {why}");
        }
        return;
    }
    if auto.cheats && !room.rules.cheats {
        actions.write(LobbyAction::Cheats(true));
        return;
    }
    if room.seats.len() < auto.players + auto.ai {
        actions.write(LobbyAction::AddSeat);
        return;
    }
    let count = |who: fn(&Occupant) -> bool| room.seats.iter().filter(|s| who(&s.who)).count();
    let ais = count(|o| *o == Occupant::Ai);
    if ais < auto.ai
        && let Some(seat) = room.seats.iter().rposition(|s| s.who == Occupant::Open)
    {
        // The host's click on an open seat makes it an AI.
        actions.write(LobbyAction::Occupant(seat as u8));
        return;
    }
    let seated = count(|o| matches!(o, Occupant::Human(_)));
    if seated < auto.players || ais < auto.ai {
        return;
    }
    match l.start_blocker() {
        None => {
            info!("autopilot: starting with {seated} players on {}", room.map);
            actions.write(LobbyAction::Start);
        }
        Some(why) => debug!("autopilot: not yet: {why}"),
    }
}

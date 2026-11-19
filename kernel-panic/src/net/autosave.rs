//! Single-player autosave (`spawn_units.lua`: every `30*60*5` frames,
//! with at most one player, `MakeDumpNao("AutoSave", "AutoSave.sav")`).
//!
//! Upstream's save is a dump of the whole battle as a start script. A
//! lockstep match is its setup, seed and the players' commands, so the
//! port's save is the match's replay log up to the save
//! ([`super::replay_log::replay_of`]); loading it plays those turns again
//! as fast as the machine allows ([`super::session::Session::resume`])
//! and the match goes on live from there, the AI re-deriving its own
//! commands as in any replay.
//!
//! Natively the save is `autosave.kpsave` beside the replays (another
//! extension, so the Replays list leaves it out); on the web it lasts
//! the page session ([`AutosaveBytes`]).

use bevy::prelude::*;

use super::driver::NetSession;
use super::replay_log::{self, Replay};
use crate::game_setup::AppState;

/// `spawn_units.lua`: `frame > LastAutoSaveFrame + 30*60*5`.
pub const AUTOSAVE_FRAMES: u32 = 30 * 60 * 5;

/// The autosave's file name in the replay folder.
#[cfg(not(target_arch = "wasm32"))]
pub const AUTOSAVE_FILE: &str = "autosave.kpsave";

/// The newest autosave of this app session (the web's only copy).
#[derive(Resource, Default)]
pub struct AutosaveBytes(pub Option<Vec<u8>>);

/// The autosave slot (`applied / AUTOSAVE_FRAMES`) last written in this
/// match.
#[derive(Resource, Default)]
pub struct AutosaveMark(u32);

/// An autosave to resume at the next match swap (the session takes it).
#[derive(Resource, Clone, Debug)]
pub struct NextResume {
    pub replay: Replay,
}

/// Natively: where the autosave goes, if anywhere (the headless harness
/// keeps no files: no [`replay_log::ReplayDir`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn autosave_path(dir: Option<&std::path::Path>) -> Option<std::path::PathBuf> {
    dir.map(|d| d.join(AUTOSAVE_FILE))
}

/// The autosave's bytes, if there is one.
pub fn read_autosave(memory: &AutosaveBytes) -> Option<Vec<u8>> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok(bytes) = std::fs::read(replay_log::replay_dir().join(AUTOSAVE_FILE)) {
        return Some(bytes);
    }
    memory.0.clone()
}

/// Every [`AUTOSAVE_FRAMES`] of a single-player match, save it. Not a
/// networked match (`#Spring.GetPlayerList() <= 1`), a replay, the
/// menu's demo, a showcase or a sandbox ([`replay_log::replay_of`]), nor
/// while an autosave is still being loaded.
pub fn write_autosave(world: &mut World) {
    let Some(net) = world.get_resource::<NetSession>() else {
        return;
    };
    let session = &net.session;
    if session.is_networked() || session.is_replay() || session.resuming() {
        return;
    }
    let slot = session.applied() / AUTOSAVE_FRAMES;
    if slot == 0
        || world
            .get_resource::<AutosaveMark>()
            .is_none_or(|mark| mark.0 >= slot)
    {
        return;
    }
    let log = session.log().to_vec();
    world.resource_mut::<AutosaveMark>().0 = slot;
    let Some(replay) = replay_log::replay_of(world, log) else {
        return;
    };
    let bytes = replay.encode();
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = autosave_path(
        world
            .get_resource::<replay_log::ReplayDir>()
            .and_then(|d| d.0.as_deref()),
    ) {
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, &bytes));
        match written {
            Ok(()) => info!("autosaved at tick {} to {}", replay.ticks, path.display()),
            Err(e) => warn!("cannot autosave to {}: {e}", path.display()),
        }
    }
    if let Some(mut memory) = world.get_resource_mut::<AutosaveBytes>() {
        memory.0 = Some(bytes);
    }
}

/// Resume `replay` (an autosave): its setup is the next match's, the
/// save waits in [`NextResume`] for the swap.
pub fn resume(commands: &mut Commands, replay: Replay) {
    info!(
        "loading the autosave: {} on {}, {} ticks",
        replay.label(),
        replay.setup.map,
        replay.ticks
    );
    commands.insert_resource(replay.setup.game_setup());
    commands.insert_resource(NextResume { replay });
    commands.queue(|world: &mut World| {
        world
            .resource_mut::<NextState<AppState>>()
            .set(AppState::InGame);
    });
}

#[cfg(test)]
mod tests {
    use crate::net::session::Session;
    use crate::net::{CommandFrame, SimCommand};
    use std::time::Duration;

    /// A resumed session runs the saved commands on their own ticks,
    /// logs them like any turn, and is live from the saved tick on.
    #[test]
    fn a_resumed_match_replays_its_log_then_goes_live() {
        let frame = |tick, player| CommandFrame {
            tick,
            player,
            commands: vec![SimCommand::SetSpeed { percent: 100 }],
        };
        let saved = vec![frame(3, 0), frame(7, 0), frame(12, 0)];
        let mut s = Session::resume(&saved, 10);
        assert!(s.resuming());
        let mut seen = Vec::new();
        while s.resuming() {
            s.cut(Duration::ZERO);
            let turn = s.pop().expect("a turn a tick");
            if turn.frames.iter().any(|(_, c)| !c.is_empty()) {
                seen.push(turn.tick);
            }
        }
        assert_eq!(seen, [3, 7], "the saved commands, on their ticks");
        assert_eq!(s.applied(), 10);
        assert_eq!(s.log(), &saved[..2]);
        // Live: what the player orders now is sequenced as usual.
        s.submit(frame(10, 0));
        s.cut(Duration::ZERO);
        assert!(
            s.pop()
                .is_some_and(|t| t.tick == 10 && !t.frames.is_empty())
        );
    }
}

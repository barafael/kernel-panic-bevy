use bevy::prelude::*;

use crate::game_setup::GameOverDismissed;
use crate::units::components::{Homebase, TeamId};
use crate::units::player::LocalTeam;

/// Current game state. Systems in gameplay sets only run in `Playing`.
/// The menu system watches for transitions into Victory/Defeat and opens
/// the game-over panel (see `ui::menu`).
#[derive(States, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameState {
    #[default]
    Playing,
    Victory,
    Defeat,
}

/// Player-relative game-over check: **Defeat** when the local team has
/// no homebases left, **Victory** once every surviving homebase belongs
/// to the local team. A map with no homebases at all (test / sandbox
/// variants) never flips state. Once the player dismisses the game-over
/// panel ("Keep on playing"), the check stops re-triggering.
pub fn check_game_over(
    local: Res<LocalTeam>,
    dismissed: Res<GameOverDismissed>,
    homebases: Query<&TeamId, With<Homebase>>,
    mut next_state: ResMut<NextState<GameState>>,
) {
    if dismissed.0 {
        return;
    }

    // Maps with no homebases (test / sandbox variants) never flip state.
    if homebases.is_empty() {
        return;
    }

    let mut local_bases: usize = 0;
    let mut enemy_bases: usize = 0;
    for t in &homebases {
        if t.0 == local.0 {
            local_bases += 1;
        } else {
            enemy_bases += 1;
        }
    }

    let new_state = if local_bases == 0 {
        GameState::Defeat
    } else if enemy_bases == 0 {
        GameState::Victory
    } else {
        return;
    };

    next_state.set(new_state);
}

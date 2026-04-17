use bevy::prelude::*;

use super::components::{Homebase, TeamId};

/// Current game state. Systems in gameplay sets only run in `Playing`.
#[derive(States, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameState {
    #[default]
    Playing,
    Victory,
    Defeat,
}

/// The player's team ID (team 0 by default).
#[derive(Resource, Default)]
pub struct PlayerTeam(pub u8);

/// UI entity for the game-over overlay.
#[derive(Component)]
pub struct GameOverUi;

/// Check if the game is over: player loses all homebases → defeat,
/// all enemy homebases destroyed → victory.
pub fn check_game_over(
    homebases: Query<&TeamId, With<Homebase>>,
    player_team: Res<PlayerTeam>,
    mut next_state: ResMut<NextState<GameState>>,
    mut commands: Commands,
    existing_ui: Query<Entity, With<GameOverUi>>,
) {
    // Sandbox / showcase maps don't spawn any homebases. Without this
    // guard the player would be flagged "no homebase = defeat" on the
    // very first frame, freezing every gameplay system before the user
    // could see anything.
    if homebases.is_empty() {
        return;
    }
    let player_alive = homebases.iter().any(|t| t.0 == player_team.0);
    let enemies_alive = homebases.iter().any(|t| t.0 != player_team.0);

    let new_state = if !player_alive {
        GameState::Defeat
    } else if !enemies_alive {
        GameState::Victory
    } else {
        return;
    };

    let (text, color) = match new_state {
        GameState::Victory => ("VICTORY", Color::linear_rgb(0.0, 1.0, 0.3)),
        GameState::Defeat => ("DEFEAT", Color::linear_rgb(1.0, 0.0, 0.2)),
        GameState::Playing => unreachable!(),
    };

    if existing_ui.is_empty() {
        commands.spawn((
            GameOverUi,
            Text::new(text),
            TextFont {
                font_size: 120.0,
                ..default()
            },
            TextColor(color),
            TextLayout::new_with_justify(Justify::Center),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                top: Val::Percent(35.0),
                justify_self: JustifySelf::Center,
                ..default()
            },
        ));
    }

    next_state.set(new_state);
}

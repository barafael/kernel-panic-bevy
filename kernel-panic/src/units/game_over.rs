use bevy::prelude::*;

use super::components::{Homebase, TeamId};

/// Current game state.
#[derive(Resource, Default, PartialEq, Eq)]
pub enum GameState {
    #[default]
    Playing,
    Victory,
    Defeat,
}

/// The player's team ID (team 0 by default).
#[derive(Resource)]
pub struct PlayerTeam(pub u8);

impl Default for PlayerTeam {
    fn default() -> Self {
        Self(0)
    }
}

/// UI entity for the game-over overlay.
#[derive(Component)]
pub struct GameOverUi;

/// Check if the game is over: player loses all homebases → defeat,
/// all enemy homebases destroyed → victory.
pub fn check_game_over(
    homebases: Query<&TeamId, With<Homebase>>,
    player_team: Res<PlayerTeam>,
    mut game_state: ResMut<GameState>,
    mut commands: Commands,
    existing_ui: Query<Entity, With<GameOverUi>>,
) {
    if *game_state != GameState::Playing {
        return;
    }

    let player_alive = homebases.iter().any(|t| t.0 == player_team.0);
    let enemies_alive = homebases.iter().any(|t| t.0 != player_team.0);

    let new_state = if !player_alive {
        Some(GameState::Defeat)
    } else if !enemies_alive {
        Some(GameState::Victory)
    } else {
        None
    };

    if let Some(state) = new_state {
        let (text, color) = match state {
            GameState::Victory => ("VICTORY", Color::linear_rgb(0.0, 1.0, 0.3)),
            GameState::Defeat => ("DEFEAT", Color::linear_rgb(1.0, 0.0, 0.2)),
            GameState::Playing => unreachable!(),
        };

        // Only spawn UI once.
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

        *game_state = state;
    }
}

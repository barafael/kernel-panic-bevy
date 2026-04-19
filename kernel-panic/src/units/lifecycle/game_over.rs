use bevy::prelude::*;

use crate::units::components::{Homebase, TeamId};

/// Current game state. Systems in gameplay sets only run in `Playing`.
#[derive(States, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameState {
    #[default]
    Playing,
    Victory,
    Defeat,
}

/// UI entity for the game-over overlay.
#[derive(Component)]
pub struct GameOverUi;

/// Sandbox-mode game-over check: with AI removed and every team
/// player-controllable, there's no "enemy" perspective. We declare
/// victory once a single team has all surviving homebases, and defeat
/// if every homebase on the map is gone.
pub fn check_game_over(
    homebases: Query<&TeamId, With<Homebase>>,
    mut next_state: ResMut<NextState<GameState>>,
    mut commands: Commands,
    existing_ui: Query<Entity, With<GameOverUi>>,
) {
    // Maps with no homebases (test / sandbox variants) never flip state.
    if homebases.is_empty() && existing_ui.is_empty() {
        return;
    }

    let mut first_team: Option<u8> = None;
    let mut only_one = true;
    let mut any = false;
    for t in &homebases {
        any = true;
        match first_team {
            None => first_team = Some(t.0),
            Some(ft) if ft != t.0 => {
                only_one = false;
                break;
            }
            _ => {}
        }
    }

    let new_state = if !any {
        GameState::Defeat
    } else if only_one {
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

//! The match-control status line, top centre: "Paused" while the match
//! is paused and the game speed while it isn't 1× (Pause, Alt+= /
//! Alt+-; `net::control`), as Spring prints both over the view. While
//! the host's speed control holds the game below the players' speed
//! (`net::speed`) the line says so; Spring shows that speed in red
//! (`Game/Game.cpp:1615`).

use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::net::MatchControl;

/// The status line's text entity.
#[derive(Component)]
struct MatchStatus;

pub(super) struct MatchStatusPlugin;

impl Plugin for MatchStatusPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, show_match_status.run_if(in_state(AppState::InGame)));
    }
}

/// What the line says: empty at full speed, unpaused.
fn status_line(control: &MatchControl) -> String {
    let mut parts = Vec::new();
    if control.paused {
        parts.push("Paused".to_string());
    }
    if control.slowed() {
        parts.push(format!(
            "Speed {:.2}x (slowed from {:.1}x: a player can't keep up)",
            control.speed(),
            control.wanted_speed()
        ));
    } else if control.speed_percent != 100 {
        parts.push(format!("Speed {:.1}x", control.speed()));
    }
    parts.join("  ·  ")
}

fn show_match_status(
    mut commands: Commands,
    control: Option<Res<MatchControl>>,
    mut line: Query<(&mut Text, &mut Visibility), With<MatchStatus>>,
) {
    let text = control.map(|c| status_line(&c)).unwrap_or_default();
    let visibility = if text.is_empty() {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if let Ok((mut shown, mut vis)) = line.single_mut() {
        if shown.0 != text {
            shown.0 = text;
        }
        vis.set_if_neq(visibility);
        return;
    }
    commands.spawn((
        MatchStatus,
        Text::new(text),
        TextFont::from_font_size(20.0),
        TextColor(Color::srgb(1.0, 1.0, 0.4)),
        TextLayout::new(Justify::Center, LineBreak::NoWrap),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            width: Val::Percent(100.0),
            ..default()
        },
        visibility,
        Pickable::IGNORE,
        DespawnOnExit(AppState::InGame),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_shows_pause_and_speed() {
        let mut c = MatchControl::default();
        assert_eq!(status_line(&c), "");
        c.paused = true;
        assert_eq!(status_line(&c), "Paused");
        c.set_wanted(150);
        assert_eq!(status_line(&c), "Paused  ·  Speed 1.5x");
        c.paused = false;
        assert_eq!(status_line(&c), "Speed 1.5x");
        c.set_internal(62);
        assert_eq!(
            status_line(&c),
            "Speed 0.62x (slowed from 1.5x: a player can't keep up)"
        );
    }
}

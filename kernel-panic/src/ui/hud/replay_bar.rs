//! The replay viewer's bar, top centre, while a replay plays
//! (`net::replay_log`): the file, where the replay is (game time of the
//! recording), the viewer's speed, and buttons to slow down, pause and
//! speed up. Keys: Pause or Space pause / resume, `-` / `+` (with or
//! without Alt, Spring's speed keys) slow down / speed up. All of it acts
//! on the driver's clock only ([`Pacer::hold`], [`Pacer::rate`]); the
//! recorded pauses and speed changes still apply as they were played.
//!
//! [`Pacer::hold`]: crate::net::driver::Pacer::hold
//! [`Pacer::rate`]: crate::net::driver::Pacer::rate

use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::net::driver::{NetSession, NetStatus, ReplayStatus};
use crate::ui::game_clock;

use super::widgets::{set_text, sync_single};

/// The viewer's speeds.
const RATES: [f32; 7] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

#[derive(Component)]
struct ReplayReadout;

/// What a bar button does.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
enum ReplayButton {
    Slower,
    Pause,
    Faster,
}

pub(super) struct ReplayBarPlugin;

impl Plugin for ReplayBarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (replay_keys, show_replay_bar)
                .chain()
                .run_if(in_state(AppState::InGame)),
        )
        // With the HUD's other mouse input (the TAKE button's place).
        .add_systems(
            PreUpdate,
            press_replay_button
                .after(crate::ui::menu::MenuMouseInput)
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// The next speed up (`faster`) or down from `rate`.
fn step_rate(rate: f32, faster: bool) -> f32 {
    let i = RATES
        .iter()
        .position(|r| *r >= rate)
        .unwrap_or(RATES.len() - 1);
    if faster {
        RATES[(i + 1).min(RATES.len() - 1)]
    } else {
        RATES[i.saturating_sub(1)]
    }
}

fn act(net: &mut NetSession, button: ReplayButton) {
    match button {
        ReplayButton::Slower => net.pacer.rate = step_rate(net.pacer.rate, false),
        ReplayButton::Faster => net.pacer.rate = step_rate(net.pacer.rate, true),
        ReplayButton::Pause => net.pacer.hold = !net.pacer.hold,
    }
}

fn replay_keys(
    keys: Res<ButtonInput<KeyCode>>,
    esc_open: Option<Res<crate::ui::menu::EscMenuOpen>>,
    net: Option<ResMut<NetSession>>,
) {
    let Some(mut net) = net.filter(|n| n.session.is_replay()) else {
        return;
    };
    if esc_open.is_some_and(|o| o.0) {
        return;
    }
    if keys.any_just_pressed([KeyCode::Pause, KeyCode::Space]) {
        act(&mut net, ReplayButton::Pause);
    }
    if keys.any_just_pressed([KeyCode::Equal, KeyCode::NumpadAdd]) {
        act(&mut net, ReplayButton::Faster);
    }
    if keys.any_just_pressed([KeyCode::Minus, KeyCode::NumpadSubtract]) {
        act(&mut net, ReplayButton::Slower);
    }
}

/// The bar's text.
fn readout(name: &str, r: &ReplayStatus, other_map: bool) -> String {
    let mut text = format!(
        "Replay {name}   {} / {}   {}x",
        game_clock(r.tick),
        game_clock(r.ticks),
        r.rate
    );
    if r.hold {
        text.push_str("   paused");
    } else if r.tick >= r.ticks {
        text.push_str("   the end");
    }
    if other_map {
        text.push_str("\nRecorded on another copy of this map: it may play differently.");
    }
    text
}

fn show_replay_bar(
    mut commands: Commands,
    status: Option<Res<NetStatus>>,
    net: Option<Res<NetSession>>,
    map_hash: Option<Res<crate::net::replay_log::ActiveMapHash>>,
    mut readout_text: Query<(&ChildOf, &mut Text), With<ReplayReadout>>,
) {
    let view = net.as_deref().and_then(|n| n.viewing.as_ref());
    let replay = status.as_deref().and_then(|s| s.replay.as_ref());
    let line = view.zip(replay).map(|(view, r)| {
        let other_map = map_hash.is_some_and(|h| h.0 != view.map_hash);
        readout(view.name.trim_end_matches(".kpreplay"), r, other_map)
    });
    // The bar is the readout's parent.
    let bar = readout_text
        .single_mut()
        .map(|(bar, text)| (bar.parent(), text));
    sync_single(&mut commands, line, bar, set_text, spawn_bar);
}

/// The bar: the readout saying `line`, then the buttons.
fn spawn_bar(commands: &mut Commands, line: String) {
    let root = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(36.0),
                left: Val::Percent(50.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(8.0),
                padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)),
                ..default()
            },
            UiTransform::from_translation(Val2::percent(-50.0, 0.0)),
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
            GlobalZIndex(40),
            DespawnOnExit(AppState::InGame),
        ))
        .id();
    commands.spawn((
        ReplayReadout,
        Text::new(line),
        TextFont::from_font_size(18.0),
        TextColor(Color::srgb(0.6, 1.0, 1.0)),
        TextLayout::new(Justify::Left, LineBreak::NoWrap),
        Pickable::IGNORE,
        ChildOf(root),
    ));
    for (button, label) in [
        (ReplayButton::Slower, " - "),
        (ReplayButton::Pause, "||/>"),
        (ReplayButton::Faster, " + "),
    ] {
        commands
            .spawn((
                button,
                Node {
                    padding: UiRect::axes(Val::Px(8.0), Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.0, 0.35, 0.45)),
                ChildOf(root),
            ))
            .with_child((
                Text::new(label),
                TextFont::from_font_size(18.0),
                TextColor(Color::WHITE),
                Pickable::IGNORE,
            ));
    }
}

/// A left click on a bar button; the press is consumed (the TAKE
/// button's hit test).
fn press_replay_button(
    buttons: Query<(&ReplayButton, &ComputedNode, &UiGlobalTransform)>,
    windows: Query<&Window>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    net: Option<ResMut<NetSession>>,
) {
    if !mouse.just_pressed(MouseButton::Left) || buttons.is_empty() {
        return;
    }
    let Some(cursor) = windows
        .single()
        .ok()
        .and_then(|w| Some(w.cursor_position()? * w.scale_factor()))
    else {
        return;
    };
    let Some(button) = buttons
        .iter()
        .find(|(_, node, t)| node.contains_point(**t, cursor))
        .map(|(b, ..)| *b)
    else {
        return;
    };
    mouse.clear_just_pressed(MouseButton::Left);
    if let Some(mut net) = net.filter(|n| n.session.is_replay()) {
        act(&mut net, button);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_step_and_stop_at_the_ends() {
        assert_eq!(step_rate(1.0, true), 2.0);
        assert_eq!(step_rate(1.0, false), 0.5);
        assert_eq!(step_rate(16.0, true), 16.0);
        assert_eq!(step_rate(0.25, false), 0.25);
        let r = ReplayStatus {
            tick: 1830,
            ticks: 5400,
            rate: 2.0,
            hold: true,
            speed: 100,
        };
        assert_eq!(
            readout("x", &r, false),
            "Replay x   1:01 / 3:00   2x   paused"
        );
    }
}

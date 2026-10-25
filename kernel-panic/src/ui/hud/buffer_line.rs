//! The Network's packet buffer on screen (`network_buffer.lua`'s
//! `DrawScreen`): while the player's team has Packets in its buffer or
//! owns a Port, "Buffer fill state: N" (22 px, centred at three
//! quarters of the width, 50 px down), "MAXED" in pink instead of "fill
//! state" once the buffer and the team's units reach its unit limit,
//! and under it (14 px, 80 px down) what to do with them. Not for
//! spectators, a player out of the match included.

use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::lifecycle::bookkeeping::TeamUnitCounts;
use crate::units::mechanics::network_buffer::PacketBuffer;
use crate::units::player::LocalTeam;
use crate::units::teams::TeamFates;

/// The widget's root.
#[derive(Component)]
struct BufferLine;

/// The span saying "fill state" or "MAXED".
#[derive(Component)]
struct BufferState;

/// The span with the count.
#[derive(Component)]
struct BufferCount;

/// `\255\255\196\196`.
const MAXED: Color = Color::srgb_u8(255, 196, 196);

const HINT: &str = "Select a Port or Connection and use the Dispatch command to use these packets.";

pub(super) struct BufferLinePlugin;

impl Plugin for BufferLinePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, show_buffer_line.run_if(in_state(AppState::InGame)));
    }
}

/// What the line shows: the count and whether it is maxed, or nothing.
fn buffer_line(packets: u32, has_port: bool, units: u32, limit: u32) -> Option<(u32, bool)> {
    (packets > 0 || has_port).then_some((packets, packets + units >= limit))
}

#[allow(clippy::too_many_arguments)]
fn show_buffer_line(
    mut commands: Commands,
    local: Option<Res<LocalTeam>>,
    fates: Option<Res<TeamFates>>,
    buffer: Option<Res<PacketBuffer>>,
    counts: Option<Res<TeamUnitCounts>>,
    units: Query<(&UnitType, &TeamId), Without<Dying>>,
    roots: Query<Entity, With<BufferLine>>,
    mut state: Query<(&mut TextSpan, &mut TextColor), With<BufferState>>,
    mut count: Query<&mut TextSpan, (With<BufferCount>, Without<BufferState>)>,
) {
    let line = local
        .and_then(|l| l.seat())
        .filter(|team| !fates.as_deref().is_some_and(|f| f.is_dead(*team)))
        .and_then(|team| {
            // `GetTeamUnitsByDefs`: a Port still being built counts.
            let has_port = units
                .iter()
                .any(|(u, t)| u.0 == UnitKind::Port && t.0 == team);
            buffer_line(
                buffer.as_deref().map_or(0, |b| b.peek(team)),
                has_port,
                counts.as_deref().map_or(0, |c| c.get(team)),
                counts.as_deref().map_or(u32::MAX, |c| c.limit(team)),
            )
        });
    let Some((packets, maxed)) = line else {
        for root in &roots {
            commands.entity(root).despawn();
        }
        return;
    };
    let (word, color) = if maxed {
        ("MAXED", MAXED)
    } else {
        ("fill state", Color::WHITE)
    };
    if roots.is_empty() {
        spawn_buffer_line(&mut commands, word, color, packets);
        return;
    }
    for (mut span, mut text_color) in &mut state {
        if span.0 != word {
            span.0 = word.to_string();
        }
        text_color.set_if_neq(TextColor(color));
    }
    let n = format!(": {packets}");
    for mut span in &mut count {
        if span.0 != n {
            span.0.clone_from(&n);
        }
    }
}

/// Both lines, centred on three quarters of the width. Bevy places a
/// text box by its top, the gadget by its baseline: about an ascent
/// higher.
fn spawn_buffer_line(commands: &mut Commands, word: &str, color: Color, packets: u32) {
    let centred = |top: f32| Node {
        position_type: PositionType::Absolute,
        top: Val::Px(top),
        left: Val::Percent(50.0),
        width: Val::Percent(50.0),
        ..default()
    };
    let root = commands
        .spawn((
            BufferLine,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            Pickable::IGNORE,
            DespawnOnExit(AppState::InGame),
        ))
        .id();
    commands
        .spawn((
            Text::new("Buffer "),
            TextFont::from_font_size(22.0),
            TextColor(Color::WHITE),
            // Laid out across the node's width (`NoWrap` measures the
            // text alone and leaves it at the node's left), so `Center`
            // centres it there; the line is far too short to wrap.
            TextLayout::new(Justify::Center, LineBreak::WordBoundary),
            centred(31.0),
            Pickable::IGNORE,
            ChildOf(root),
        ))
        .with_children(|line| {
            line.spawn((
                BufferState,
                TextSpan::new(word),
                TextFont::from_font_size(22.0),
                TextColor(color),
            ));
            line.spawn((
                BufferCount,
                TextSpan::new(format!(": {packets}")),
                TextFont::from_font_size(22.0),
                TextColor(Color::WHITE),
            ));
        });
    commands.spawn((
        Text::new(HINT),
        TextFont::from_font_size(14.0),
        TextColor(Color::WHITE),
        TextLayout::new(Justify::Center, LineBreak::WordBoundary),
        centred(68.0),
        Pickable::IGNORE,
        ChildOf(root),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shown with Packets buffered or a Port owned; "MAXED" once the
    /// buffer and the units fill the unit limit.
    #[test]
    fn the_line_follows_the_gadget() {
        assert_eq!(buffer_line(0, false, 10, 1000), None);
        assert_eq!(buffer_line(0, true, 10, 1000), Some((0, false)));
        assert_eq!(buffer_line(25, false, 10, 1000), Some((25, false)));
        assert_eq!(buffer_line(990, true, 10, 1000), Some((990, true)));
    }
}

//! Bottom-left tooltip box — Kernel Panic's `kp_tooltip.lua` (with
//! `kp_midknight_bg.lua`'s background), which replaces Spring's own
//! tooltip console.
//!
//! The box always sits in the bottom-left corner and sizes itself to its
//! text (`FontSize × (1 + widest line)` by `FontSize × (1 + lines)`,
//! `FontSize = max(8, 4 + height / 100)`), over `bitmaps/tooltipbg.png`
//! stretched to 1.2× that. What it says, in kp_tooltip's order:
//!
//! - over a command button (control panel or build bar): the command's
//!   tooltip, its action name in green, then `Hotkeys:` in orange;
//! - over a build option: "N units selected", then the unit's name and
//!   description, build time, health and speed;
//! - otherwise: the selection count and the unit under the cursor (or
//!   the last selected unit) — name, description, health, speed, and a
//!   teleporter's buffered packets.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::game_setup::AppState;
use crate::interaction::selection::{Hovered, Selected};
use crate::sim::GAME_SPEED;
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::mechanics::network_buffer::PacketBuffer;
use crate::units::player::LocalTeam;

use super::command_panel::commands::{CmdDesc, CmdId};
use super::command_panel::layout::FONT_ADVANCE_EM;

pub(super) struct TooltipPlugin;

impl Plugin for TooltipPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HoverTip>().add_systems(
            Update,
            refresh_tooltip
                .after(super::command_panel::CommandPanelSet)
                .after(crate::map_loading::GameWorldRebuild),
        );
    }
}

/// What the HUD widgets report under the cursor this frame.
#[derive(Resource, Default)]
pub(crate) struct HoverTip {
    /// Control-panel button.
    pub panel: Option<CmdDesc>,
    /// Build-bar icon: a command, or a plain text tooltip.
    pub bar: Option<BarTip>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BarTip {
    Command(CmdDesc),
    Text(String),
}

/// kp_tooltip colours (`\255 r g b` escapes).
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::srgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
}
const SEL_COUNT: Color = rgb(128, 255, 128);
const SEL_TEXT: Color = rgb(196, 255, 196);
const ACTION: Color = rgb(170, 255, 170);
const HOTKEY_LABEL: Color = rgb(255, 196, 128);
const HOTKEY_KEYS: Color = rgb(255, 128, 1);
const BUILD_LABEL: Color = rgb(213, 213, 255);
const BUILD_VALUE: Color = rgb(170, 170, 255);
const HEALTH_LABEL: Color = rgb(255, 213, 213);
const HEALTH_VALUE: Color = rgb(255, 170, 170);
const SPEED_LABEL: Color = rgb(193, 255, 187);
const SPEED_VALUE: Color = rgb(134, 255, 121);
const BUFFER_LABEL: Color = rgb(255, 255, 128);
const BUFFER_VALUE: Color = rgb(255, 255, 57);

/// One tooltip line as coloured segments.
type Line = Vec<(String, Color)>;

fn line(parts: &[(&str, Color)]) -> Line {
    parts.iter().map(|(t, c)| (t.to_string(), *c)).collect()
}

/// `FormatNbr`: whole numbers without decimals, otherwise `digits`.
fn format_nbr(x: f32, digits: usize) -> String {
    let frac = x.fract().abs();
    if frac == 0.0 || frac < 0.01 {
        format!("{}", x.floor() as i64)
    } else if frac > 0.99 {
        format!("{}", x.ceil() as i64)
    } else {
        let s = format!("{x:.digits$}");
        if digits > 0 {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s
        }
    }
}

/// "One unit selected" / "N units selected".
fn selection_line(count: usize) -> Option<Line> {
    match count {
        0 => None,
        1 => Some(line(&[("One", SEL_COUNT), (" unit selected", SEL_TEXT)])),
        n => Some(line(&[
            (&n.to_string(), SEL_COUNT),
            (" units selected", SEL_TEXT),
        ])),
    }
}

/// A command button's tooltip: `"Action: text"` with the action in
/// green, then the hotkeys.
fn command_lines(desc: &CmdDesc) -> Vec<Line> {
    let text = if desc.tooltip.is_empty() {
        desc.name.as_str()
    } else {
        desc.tooltip
    };
    let mut out: Vec<Line> = Vec::new();
    for (i, l) in text.lines().filter(|l| !l.is_empty()).enumerate() {
        if i == 0
            && let Some((action, rest)) = l.split_once(": ")
        {
            out.push(vec![
                (format!("{action}:"), ACTION),
                (format!(" {rest}"), Color::WHITE),
            ]);
        } else {
            out.push(line(&[(l, Color::WHITE)]));
        }
    }
    if !desc.hotkeys.is_empty() {
        out.push(line(&[
            ("Hotkeys: ", HOTKEY_LABEL),
            (desc.hotkeys, HOTKEY_KEYS),
        ]));
    }
    out
}

/// kp_tooltip's build-button text: name (description), build time at a
/// 128-worktime builder, health, speed.
fn build_lines(kind: UnitKind, registry: &UnitRegistry) -> Vec<Line> {
    let name = registry.name(kind).to_string();
    let desc = registry
        .def(kind)
        .map(|d| d.description.clone())
        .unwrap_or_default();
    let build_time = registry.raw_build_time(kind);
    let workertime = 128.0;
    let secs = ((29.0 + (31.0 + build_time / (workertime / 32.0)).floor()) / GAME_SPEED).floor();
    let mut out = vec![
        line(&[(&format!("{name} ({desc})"), Color::WHITE)]),
        line(&[
            ("Build time: ", BUILD_LABEL),
            (&format!("{secs}s"), BUILD_VALUE),
        ]),
        line(&[
            ("Health: ", HEALTH_LABEL),
            (&format_nbr(registry.max_health(kind), 0), HEALTH_VALUE),
        ]),
    ];
    let speed = registry.speed(kind);
    if speed > 0.0 {
        out.push(line(&[
            ("Speed: ", SPEED_LABEL),
            (&format_nbr(speed, 2), SPEED_VALUE),
        ]));
    }
    out
}

fn tooltip_lines(
    hover: &HoverTip,
    selected: usize,
    unit: Option<(UnitKind, Option<&Health>, Option<u32>)>,
    registry: &UnitRegistry,
) -> Vec<Line> {
    let cmd = hover.panel.clone().or_else(|| match &hover.bar {
        Some(BarTip::Command(d)) => Some(d.clone()),
        _ => None,
    });
    if let Some(desc) = cmd {
        return match desc.id {
            CmdId::Build(kind) | CmdId::Produce(kind) => {
                let mut out: Vec<Line> = selection_line(selected).into_iter().collect();
                out.extend(build_lines(kind, registry));
                if !desc.hotkeys.is_empty() {
                    out.push(line(&[
                        ("Hotkeys: ", HOTKEY_LABEL),
                        (desc.hotkeys, HOTKEY_KEYS),
                    ]));
                }
                out
            }
            _ => command_lines(&desc),
        };
    }
    if let Some(BarTip::Text(t)) = &hover.bar {
        return t.lines().map(|l| line(&[(l, Color::WHITE)])).collect();
    }
    let mut out: Vec<Line> = selection_line(selected).into_iter().collect();
    if let Some((kind, health, packets)) = unit {
        let desc = registry
            .def(kind)
            .map(|d| d.description.clone())
            .unwrap_or_default();
        out.push(line(&[(
            &format!("{} ({desc})", registry.name(kind)),
            Color::WHITE,
        )]));
        let (cur, max) = health.map_or(
            (registry.max_health(kind), registry.max_health(kind)),
            |h| (h.current, h.max),
        );
        out.push(line(&[
            ("Health: ", HEALTH_LABEL),
            (&format!("{}", cur.floor() as i64), HEALTH_VALUE),
            ("/", HEALTH_LABEL),
            (&format!("{}", max.floor() as i64), HEALTH_VALUE),
        ]));
        if let Some(n) = packets {
            out.push(line(&[
                ("Bufferised Packets: ", BUFFER_LABEL),
                (&n.to_string(), BUFFER_VALUE),
            ]));
        }
        let speed = registry.speed(kind);
        if speed > 0.0 {
            out.push(line(&[
                ("Speed: ", SPEED_LABEL),
                (&format_nbr(speed, 2), SPEED_VALUE),
            ]));
        }
    }
    out
}

#[derive(Component)]
struct TooltipRoot;

type UnitInfo<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static UnitType,
        Option<&'static Health>,
        Option<&'static TeamId>,
        Has<Selected>,
        Has<Hovered>,
    ),
>;

#[allow(clippy::too_many_arguments)]
fn refresh_tooltip(
    mut commands: Commands,
    windows: Query<&Window, With<PrimaryWindow>>,
    hover: Res<HoverTip>,
    units: UnitInfo,
    registry: Res<UnitRegistry>,
    buffer: Option<Res<PacketBuffer>>,
    local: Option<Res<LocalTeam>>,
    state: Option<Res<State<AppState>>>,
    asset_server: Res<AssetServer>,
    roots: Query<Entity, With<TooltipRoot>>,
    mut last: Local<Option<(Vec<Line>, u32)>>,
) {
    let in_game = state.is_some_and(|s| *s.get() == AppState::InGame);
    let Ok(window) = windows.single() else {
        return;
    };
    if !in_game {
        for e in &roots {
            commands.entity(e).despawn();
        }
        *last = None;
        return;
    }
    let selected = units.iter().filter(|u| u.4).count();
    // The unit under the cursor, else the last selected one.
    let shown = units
        .iter()
        .find(|u| u.5)
        .or_else(|| units.iter().filter(|u| u.4).max_by_key(|u| u.0));
    let local_team = local.map(|l| l.0);
    let unit = shown.map(|(_, ut, health, team, ..)| {
        let packets =
            (ut.0.is_teleporter() && team.is_some_and(|t| Some(t.0) == local_team)).then(|| {
                buffer
                    .as_ref()
                    .map_or(0, |b| b.peek(team.map_or(0, |t| t.0)))
            });
        (ut.0, health, packets)
    });
    let lines = tooltip_lines(&hover, selected, unit, &registry);
    let font_size = (4.0 + window.height() / 100.0).max(8.0);
    let key = (lines.clone(), font_size as u32);
    if last.as_ref() == Some(&key) && !roots.is_empty() {
        return;
    }
    *last = Some(key);
    for e in &roots {
        commands.entity(e).despawn();
    }

    let widest = lines
        .iter()
        .map(|l| l.iter().map(|(t, _)| t.chars().count()).sum::<usize>())
        .max()
        .unwrap_or(0) as f32;
    let w = font_size * (1.0 + widest * FONT_ADVANCE_EM);
    let h = font_size * (1.0 + lines.len() as f32);
    let root = commands
        .spawn((
            TooltipRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                bottom: Val::Px(0.0),
                width: Val::Px(w * 1.2),
                height: Val::Px(h * 1.2),
                ..default()
            },
            ImageNode::new(asset_server.load("bitmaps/tooltipbg.png")),
            GlobalZIndex(-1),
        ))
        .id();
    let column = commands
        .spawn((
            ChildOf(root),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(font_size / 2.0),
                bottom: Val::Px(font_size * 0.5),
                flex_direction: FlexDirection::Column,
                ..default()
            },
        ))
        .id();
    for l in &lines {
        let mut text = commands.spawn((
            ChildOf(column),
            Text::default(),
            TextFont::from_font_size(font_size),
            TextLayout::new(Justify::Left, LineBreak::NoWrap),
            TextShadow {
                offset: Vec2::splat(1.0),
                color: Color::BLACK,
            },
            Node {
                height: Val::Px(font_size),
                ..default()
            },
        ));
        text.with_children(|t| {
            for (s, c) in l {
                t.spawn((
                    TextSpan::new(s.clone()),
                    TextFont::from_font_size(font_size),
                    TextColor(*c),
                ));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::hud::command_panel::commands::{UnitCmdState, unit_commands};

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.iter().map(|(t, _)| t.as_str()).collect())
            .collect()
    }

    /// Command tooltip: action in green, then the hotkeys.
    #[test]
    fn command_tooltip_has_action_and_hotkeys() {
        let stop = unit_commands(
            &UnitCmdState {
                kind: Some(UnitKind::Bit),
                ..Default::default()
            },
            &[],
        )
        .remove(0);
        let lines = command_lines(&stop);
        assert_eq!(
            text(&lines),
            vec!["Stop: Cancel the units current actions", "Hotkeys: s"]
        );
        assert_eq!(lines[0][0].1, ACTION);
    }

    /// Selection count wording.
    #[test]
    fn selection_count_line() {
        assert!(selection_line(0).is_none());
        assert_eq!(
            text(&[selection_line(1).unwrap()]),
            vec!["One unit selected"]
        );
        assert_eq!(
            text(&[selection_line(7).unwrap()]),
            vec!["7 units selected"]
        );
    }

    #[test]
    fn format_nbr_trims() {
        assert_eq!(format_nbr(2.0, 2), "2");
        assert_eq!(format_nbr(2.5, 2), "2.5");
        assert_eq!(format_nbr(1.999, 2), "2");
    }
}

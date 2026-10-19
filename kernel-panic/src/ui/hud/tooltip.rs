//! Bottom-left tooltip box — Kernel Panic's `kp_tooltip.lua` (with
//! `kp_midknight_bg.lua`'s background), which replaces Spring's own
//! tooltip console.
//!
//! The box always sits in the bottom-left corner and grows with its
//! text (laid out by the UI, `FontSize = max(8, 4 + height / 100)`),
//! over `bitmaps/tooltipbg.png` stretched to fit. What it says, in
//! kp_tooltip's order:
//!
//! - over a command button (control panel or build bar): the command's
//!   tooltip, its action name in green, then `Hotkeys:` in orange;
//! - over a build option: "N units selected", then the unit's name and
//!   description, build time, health and speed;
//! - otherwise: the selection count; with several unit types selected,
//!   a row per type (click one to keep just that type, right-click to
//!   drop it from the selection); then the unit under the cursor (or the
//!   last selected unit) — name, description, health, speed, and a
//!   teleporter's buffered packets.

use crate::map_loading::MatchScopeAppExt;
use bevy::input::InputSystems;
use bevy::picking::PickingSystems;
// Picking's hover marker (the selection's unit hover is `Hovered`).
use bevy::picking::hover::Hovered as PointerOver;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::game_setup::AppState;
use crate::interaction::selection::{Hovered, Selected};
use crate::sim::{DetFloat, GAME_SPEED};
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::mechanics::network_buffer::PacketBuffer;
use crate::units::player::LocalTeam;

use super::command_panel::commands::{CmdDesc, CmdId};

pub(super) struct TooltipPlugin;

impl Plugin for TooltipPlugin {
    fn build(&self, app: &mut App) {
        app.init_match_resource::<HoverTip>()
            .init_match_resource::<RowPress>()
            // Before every Update system, so a click on a type row is
            // gone before selection and orders look at the mouse.
            .add_systems(
                PreUpdate,
                type_row_clicks
                    .after(InputSystems)
                    .after(PickingSystems::Hover)
                    .run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                Update,
                (
                    refresh_tooltip
                        .run_if(in_state(AppState::InGame))
                        .after(super::command_panel::CommandPanelSet)
                        .after(crate::map_loading::GameWorldRebuild),
                    highlight_type_rows,
                ),
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
const TYPE_NAME: Color = rgb(255, 255, 255);
const ROW_HINT: Color = rgb(150, 150, 150);
/// Behind a type row under the cursor.
const ROW_HOVER: Color = Color::srgba(1.0, 1.0, 1.0, 0.12);

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
        desc.name.as_ref()
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

/// The tooltip's lines, and for each the unit type it is a row of (a
/// clickable line of the selection's types), if any.
fn tooltip_lines(
    hover: &HoverTip,
    selected: usize,
    types: &[(UnitKind, usize)],
    unit: Option<(UnitKind, Option<&Health>, Option<u32>)>,
    registry: &UnitRegistry,
) -> (Vec<Line>, Vec<Option<UnitKind>>) {
    let lines = tooltip_text(hover, selected, types, unit, registry);
    let mut rows = vec![None; lines.0.len()];
    for (i, kind) in lines.1 {
        rows[i] = Some(kind);
    }
    (lines.0, rows)
}

/// [`tooltip_lines`]: the lines and the (line, type) of each type row.
fn tooltip_text(
    hover: &HoverTip,
    selected: usize,
    types: &[(UnitKind, usize)],
    unit: Option<(UnitKind, Option<&Health>, Option<u32>)>,
    registry: &UnitRegistry,
) -> (Vec<Line>, Vec<(usize, UnitKind)>) {
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
                (out, Vec::new())
            }
            _ => (command_lines(&desc), Vec::new()),
        };
    }
    if let Some(BarTip::Text(t)) = &hover.bar {
        return (
            t.lines().map(|l| line(&[(l, Color::WHITE)])).collect(),
            Vec::new(),
        );
    }
    let mut out: Vec<Line> = selection_line(selected).into_iter().collect();
    let mut rows = Vec::new();
    if types.len() > 1 {
        for &(kind, count) in types {
            rows.push((out.len(), kind));
            out.push(line(&[
                (&format!("{count:>4} "), SEL_COUNT),
                (registry.name(kind), TYPE_NAME),
            ]));
        }
        out.push(line(&[(
            "click: only this type, right-click: drop it",
            ROW_HINT,
        )]));
    }
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
    (out, rows)
}

#[derive(Component)]
struct TooltipRoot;

/// The spawned tooltip tree: its root, the `TextSpan` entity behind
/// every segment (per line), and the lines they currently show. The
/// tree is kept as long as the *shape* (line and segment counts) and
/// the font size hold; the segment texts and colours are then updated
/// in place, so a unit's health ticking down or the selection count
/// changing never respawns the ~20 UI entities.
struct TipTree {
    root: Entity,
    spans: Vec<Vec<Entity>>,
    lines: Vec<Line>,
    /// The type each line is a row of (its [`TypeRow`]).
    rows: Vec<Option<UnitKind>>,
    font_size: u32,
}

/// A line of the selection's types: a click keeps only this type
/// selected, a right-click drops it ([`type_row_clicks`]).
#[derive(Component)]
struct TypeRow(UnitKind);

/// A press on a type row, run on the release over the same row.
#[derive(Resource, Default)]
struct RowPress(Option<(MouseButton, UnitKind)>);

/// Same number of lines with the same number of segments each: the
/// existing text entities can take the new lines' content in place.
fn same_shape(a: &[Line], b: &[Line]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.len() == y.len())
}

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
    mut spans: Query<(&mut TextSpan, &mut TextColor)>,
    mut tree: Local<Option<TipTree>>,
    mut bg: Local<Option<Handle<Image>>>,
) {
    let in_game = state.is_some_and(|s| *s.get() == AppState::InGame);
    let Ok(window) = windows.single() else {
        return;
    };
    if !in_game {
        for e in &roots {
            commands.entity(e).despawn();
        }
        *tree = None;
        return;
    }
    let selected = units.iter().filter(|u| u.4).count();
    // The selection's types, most numerous first.
    let mut types: Vec<(UnitKind, usize)> = Vec::new();
    for (_, ut, ..) in units.iter().filter(|u| u.4) {
        match types.iter_mut().find(|(k, _)| *k == ut.0) {
            Some((_, n)) => *n += 1,
            None => types.push((ut.0, 1)),
        }
    }
    types.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| registry.name(a.0).cmp(registry.name(b.0)))
    });
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
    let (lines, rows) = tooltip_lines(&hover, selected, &types, unit, &registry);
    let font_size = (4.0 + window.height() / 100.0).det_max(8.0);

    // Same shape as the tree on screen (and the same type rows): refresh
    // the changed segments in place; the box grows with them by itself.
    if let Some(t) = tree.as_mut()
        && t.font_size == font_size as u32
        && roots.contains(t.root)
        && same_shape(&t.lines, &lines)
        && t.rows == rows
    {
        if t.lines == lines {
            return;
        }
        for (li, l) in lines.iter().enumerate() {
            for (si, (s, c)) in l.iter().enumerate() {
                let (old_s, old_c) = &t.lines[li][si];
                if (old_s, old_c) == (s, c) {
                    continue;
                }
                if let Ok((mut span, mut color)) = spans.get_mut(t.spans[li][si]) {
                    if old_s != s {
                        span.0.clone_from(s);
                    }
                    if old_c != c {
                        color.0 = *c;
                    }
                }
            }
        }
        t.lines = lines;
        return;
    }

    for e in &roots {
        commands.entity(e).despawn();
    }

    let bg = bg
        .get_or_insert_with(|| asset_server.load("bitmaps/tooltipbg.png"))
        .clone();
    // The box is laid out around its text (no fixed size, so it grows
    // with whatever it shows), with kp_tooltip's margins: half an em to
    // the left and below. The background is kp_tooltip's 1.2× of that box,
    // anchored at its bottom-left corner: the picture's visible panel
    // sits inside a transparent margin.
    let root = commands
        .spawn((
            TooltipRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                bottom: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect {
                    left: Val::Px(font_size * 0.5),
                    right: Val::Px(font_size * 0.5),
                    top: Val::Px(font_size * 0.5),
                    bottom: Val::Px(font_size * 0.5),
                },
                ..default()
            },
            GlobalZIndex(-1),
        ))
        .id();
    commands.spawn((
        ChildOf(root),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            bottom: Val::Px(0.0),
            width: Val::Percent(120.0),
            height: Val::Percent(120.0),
            ..default()
        },
        ImageNode::new(bg).with_mode(NodeImageMode::Stretch),
        ZIndex(-1),
        Pickable::IGNORE,
    ));
    let mut spans: Vec<Vec<Entity>> = Vec::with_capacity(lines.len());
    for (l, row) in lines.iter().zip(&rows) {
        let mut text = commands.spawn((
            ChildOf(root),
            Text::default(),
            TextFont::from_font_size(font_size),
            TextLayout::new(Justify::Left, LineBreak::NoWrap),
            TextShadow {
                offset: Vec2::splat(1.0),
                color: Color::BLACK,
            },
            Node {
                min_height: Val::Px(font_size),
                ..default()
            },
        ));
        if let Some(kind) = row {
            text.insert((
                TypeRow(*kind),
                PointerOver::default(),
                BackgroundColor(Color::NONE),
            ));
        }
        let text = text.id();
        spans.push(
            l.iter()
                .map(|(s, c)| {
                    commands
                        .spawn((
                            ChildOf(text),
                            TextSpan::new(s.clone()),
                            TextFont::from_font_size(font_size),
                            TextColor(*c),
                        ))
                        .id()
                })
                .collect(),
        );
    }
    *tree = Some(TipTree {
        root,
        spans,
        lines,
        rows,
        font_size: font_size as u32,
    });
}

/// Clicks on the selection's type rows: a left click keeps only that
/// type selected, a right click drops it from the selection. The press
/// and the release are consumed (selection and orders never see them),
/// and the action runs on a release over the row it was pressed on.
fn type_row_clicks(
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    rows: Query<(&TypeRow, &PointerOver)>,
    selected: Query<(Entity, &UnitType), With<Selected>>,
    mut press: ResMut<RowPress>,
    mut commands: Commands,
) {
    let under = rows
        .iter()
        .find(|(_, over)| over.get())
        .map(|(row, _)| row.0);
    for button in [MouseButton::Left, MouseButton::Right] {
        if mouse.just_pressed(button)
            && let Some(kind) = under
        {
            mouse.clear_just_pressed(button);
            press.0 = Some((button, kind));
        }
        if mouse.just_released(button)
            && let Some((pressed, kind)) = press.0
            && pressed == button
        {
            mouse.clear_just_released(button);
            press.0 = None;
            if under != Some(kind) {
                continue;
            }
            for (e, ut) in &selected {
                // Left: keep only `kind`. Right: drop `kind`.
                if (ut.0 == kind) == (button == MouseButton::Right) {
                    commands.entity(e).remove::<Selected>();
                }
            }
        }
    }
}

/// Type rows whose hover changed.
type RowHover<'w, 's> = Query<
    'w,
    's,
    (&'static PointerOver, &'static mut BackgroundColor),
    (With<TypeRow>, Changed<PointerOver>),
>;

/// A type row under the cursor gets a light backdrop.
fn highlight_type_rows(mut rows: RowHover) {
    for (over, mut bg) in &mut rows {
        bg.0 = if over.get() { ROW_HOVER } else { Color::NONE };
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

    /// A health tick keeps the tooltip's shape (the tree is updated in
    /// place); a new line — a teleporter's packets — changes it.
    #[test]
    fn health_tick_keeps_tooltip_shape() {
        let registry = UnitRegistry::empty();
        let hover = HoverTip::default();
        let at = |hp: f32, packets: Option<u32>| {
            let health = Health {
                current: hp,
                max: 100.0,
            };
            tooltip_lines(
                &hover,
                1,
                &[(UnitKind::Bit, 1)],
                Some((UnitKind::Bit, Some(&health), packets)),
                &registry,
            )
            .0
        };
        let full = at(100.0, None);
        let hurt = at(99.4, None);
        assert_ne!(full, hurt);
        assert!(same_shape(&full, &hurt));
        assert!(!same_shape(&full, &at(100.0, Some(3))));
    }

    /// Several selected types: a row each after the count, most
    /// numerous first, then the hint; one type gets no rows.
    #[test]
    fn type_rows_for_a_mixed_selection() {
        let registry = UnitRegistry::empty();
        let hover = HoverTip::default();
        let types = [(UnitKind::Bit, 12), (UnitKind::Byte, 3)];
        let (lines, rows) = tooltip_lines(&hover, 15, &types, None, &registry);
        assert_eq!(
            text(&lines),
            vec![
                "15 units selected".to_string(),
                format!("  12 {}", registry.name(UnitKind::Bit)),
                format!("   3 {}", registry.name(UnitKind::Byte)),
                "click: only this type, right-click: drop it".to_string(),
            ]
        );
        assert_eq!(
            rows,
            vec![None, Some(UnitKind::Bit), Some(UnitKind::Byte), None]
        );
        let (_, rows) = tooltip_lines(&hover, 4, &[(UnitKind::Bit, 4)], None, &registry);
        assert!(rows.iter().all(Option::is_none));
    }

    #[test]
    fn format_nbr_trims() {
        assert_eq!(format_nbr(2.0, 2), "2");
        assert_eq!(format_nbr(2.5, 2), "2.5");
        assert_eq!(format_nbr(1.999, 2), "2");
    }
}

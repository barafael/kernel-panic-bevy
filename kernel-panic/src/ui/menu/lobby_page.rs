//! The launch menu's Multiplayer page: open a room or join one by its
//! code, then the room itself (`net::lobby`) — the map and AI
//! difficulty, one row per seat (occupant, faction, team), ready /
//! start / leave.
//!
//! The host edits every seat; a guest sits down on an open seat (or
//! stands up from its own) and picks its own faction. Cells that are
//! not this player's to change are plain labels. Every row is one flex
//! row of frames, so long names and map names never run into the next
//! cell.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::text::FontSize;

use crate::sim::DetFloat;

use super::{
    Anchor, BUTTON_GREEN, ButtonSpec, DESC_BLUE, EASY_CYAN, EXTREME_ORANGE, HARD_YELLOW, LabelSpec,
    MAP_GREEN, MEDIUM_GREEN, MenuAction, MenuButton, MenuFrame, MenuPage, NAV_BLUE, TEAL,
    TEXT_WHITE, VERY_HARD_RED, anchored, back_button, border, brighten, fill, frame_border,
    frame_padding, page_heading, readable, spawn_button_text,
};
use crate::net::lobby::{Check, Lobby, LobbyAction, LobbyRoom, Occupant, RoomId, RoomState};
use crate::units::components::Faction;

/// The page: the door (create / join) outside a room, the room inside.
pub(super) fn multiplayer_page(
    commands: &mut Commands,
    root: Entity,
    (window_w, page_size): (f32, f32),
    lobby: &LobbyRoom,
) {
    match (lobby.lobby.as_ref(), lobby.room.as_ref()) {
        (Some(l), Some(room)) => room_view(commands, root, (window_w, page_size), lobby, l, room),
        _ => door(commands, root, page_size, lobby),
    }
}

/// Outside a room: create one, or type a code and join.
fn door(commands: &mut Commands, root: Entity, page_size: f32, lobby: &LobbyRoom) {
    page_heading(commands, root, page_size, "Multiplayer");
    LabelSpec::new(
        &format!("You play as {}", lobby.name),
        page_size * 0.7,
        (0.5, 0.75),
        Anchor::Cc,
    )
    .spawn(commands, root);
    ButtonSpec::new(
        "Create room",
        BUTTON_GREEN,
        page_size,
        (0.5, 0.64),
        Anchor::Cc,
        MenuAction::Lobby(LobbyAction::Create),
    )
    .min_width(24.0)
    .spawn(commands, root);
    LabelSpec::new(
        &format!("Room code: {}_", lobby.code),
        page_size,
        (0.5, 0.5),
        Anchor::Cc,
    )
    .frame(MAP_GREEN)
    .spawn(commands, root);
    ButtonSpec::new(
        "Join",
        EASY_CYAN,
        page_size,
        (0.5, 0.4),
        Anchor::Cc,
        MenuAction::Lobby(LobbyAction::Join),
    )
    .min_width(24.0)
    .spawn(commands, root);
    LabelSpec::new(
        "Type the code the host shared, then Join",
        page_size * 0.6,
        (0.5, 0.32),
        Anchor::Cc,
    )
    .spawn(commands, root);
    if let Some(notice) = &lobby.notice {
        LabelSpec::new(notice, page_size * 0.7, (0.5, 0.26), Anchor::Cc)
            .frame(VERY_HARD_RED)
            .spawn(commands, root);
    }
    back_button(page_size, (0.5, 0.15), MenuPage::Main).spawn(commands, root);
}

/// Seat rows start here and step down this much (eight fit above the
/// member list).
/// Top edge of the seat table, just under the AI row.
const SEAT_TOP: f32 = 0.685;

/// Inside a room.
fn room_view(
    commands: &mut Commands,
    root: Entity,
    (window_w, page_size): (f32, f32),
    lobby: &LobbyRoom,
    l: &Lobby,
    code: &RoomId,
) {
    page_heading(
        commands,
        root,
        page_size,
        &format!("Multiplayer: room {code}"),
    );
    let host = l.is_host();
    let me = l.me();
    let size = readable(page_size * 0.75);
    let Some(room) = l.room() else {
        LabelSpec::new("Meeting the room…", page_size * 0.8, (0.5, 0.6), Anchor::Cc)
            .spawn(commands, root);
        bottom(commands, root, size, l, lobby);
        return;
    };

    // Map and AI difficulty: the host's to change.
    let map_row = row(commands, root, (0.5, 0.8), Anchor::Cc, size);
    if host {
        cell_button(
            commands,
            map_row,
            "<",
            MAP_GREEN,
            size,
            None,
            LobbyAction::Map(-1),
        );
    }
    cell_label(
        commands,
        map_row,
        &format!("Map: {}", room.map),
        MAP_GREEN,
        size,
        None,
    );
    if host {
        cell_button(
            commands,
            map_row,
            ">",
            MAP_GREEN,
            size,
            None,
            LobbyAction::Map(1),
        );
    }
    let ai_row = row(commands, root, (0.5, 0.72), Anchor::Cc, size);
    cell_label(commands, ai_row, "AI:", TEXT_WHITE, size, None);
    for (i, (name, color)) in [
        ("Easy", EASY_CYAN),
        ("Medium", MEDIUM_GREEN),
        ("Hard", HARD_YELLOW),
        ("Extreme", EXTREME_ORANGE),
    ]
    .into_iter()
    .enumerate()
    {
        let d = 1 + i as u8;
        let chosen = room.difficulty == d;
        if host {
            choice_button(
                commands,
                ai_row,
                (name, color, size),
                Some(window_w * 0.09),
                LobbyAction::Difficulty(d),
                chosen,
            );
        } else if chosen {
            cell_label(commands, ai_row, name, color, size, None);
        }
    }
    // The host's rules: who may pause, cheats for everyone.
    let rules = room.rules;
    let pause = if rules.pausable {
        "Pause: all"
    } else {
        "Pause: host"
    };
    let cheats = if rules.cheats {
        "Cheats: on"
    } else {
        "Cheats: off"
    };
    if host {
        choice_button(
            commands,
            ai_row,
            (pause, NAV_BLUE, size),
            None,
            LobbyAction::Pausable(!rules.pausable),
            false,
        );
        choice_button(
            commands,
            ai_row,
            (cheats, NAV_BLUE, size),
            None,
            LobbyAction::Cheats(!rules.cheats),
            false,
        );
    } else {
        cell_label(commands, ai_row, pause, TEXT_WHITE, size, None);
        cell_label(commands, ai_row, cheats, TEXT_WHITE, size, None);
    }

    // One row per seat, stacked by layout (rows are as tall as their
    // text, whatever the window).
    let (mut node, transform) = anchored(0.04, SEAT_TOP, Anchor::Lt);
    node.flex_direction = FlexDirection::Column;
    node.row_gap = Val::Px(size * 0.15);
    let table = commands.spawn((node, transform, Pickable::IGNORE)).id();
    commands.entity(root).add_child(table);
    for (i, seat) in room.seats.iter().enumerate() {
        let r = commands
            .spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(size * 0.4),
                    ..default()
                },
                Pickable::IGNORE,
            ))
            .id();
        commands.entity(table).add_child(r);
        cell_label(
            commands,
            r,
            &format!("Seat {}", i + 1),
            TEXT_WHITE,
            size,
            Some(window_w * 0.08),
        );
        let mine = me.is_some_and(|me| seat.who == Occupant::Human(me));
        let (text, color) = occupant_text(room, seat.who, me);
        let occupant_editable = host || mine || seat.who == Occupant::Open;
        let wide = Some(window_w * 0.36);
        if occupant_editable {
            cell_button(
                commands,
                r,
                &text,
                color,
                size,
                wide,
                LobbyAction::Occupant(i as u8),
            );
        } else {
            cell_label(commands, r, &text, color, size, wide);
        }
        let faction = seat.faction();
        let fcolor = faction_color(faction);
        let narrow = Some(window_w * 0.13);
        if host || mine {
            cell_button(
                commands,
                r,
                &format!("{faction:?}"),
                fcolor,
                size,
                narrow,
                LobbyAction::Faction(i as u8),
            );
        } else {
            cell_label(commands, r, &format!("{faction:?}"), fcolor, size, narrow);
        }
        let team = format!("Team {}", seat.ally_team + 1);
        if host {
            cell_button(
                commands,
                r,
                &team,
                TEAL,
                size,
                narrow,
                LobbyAction::AllyTeam(i as u8),
            );
        } else {
            cell_label(commands, r, &team, TEAL, size, narrow);
        }
    }
    // Who else is here: spectators, and who cannot play and why.
    let mut lines = Vec::new();
    let spectators: Vec<&str> = room
        .members
        .iter()
        .filter(|m| room.seat_of(m.peer).is_none() && !matches!(m.check, Check::Refused(_)))
        .map(|m| m.name.as_str())
        .collect();
    if !spectators.is_empty() {
        lines.push(format!("Watching: {}", spectators.join(", ")));
    }
    for m in &room.members {
        if let Check::Refused(why) = &m.check {
            lines.push(format!("{} {why}", m.name));
        }
    }
    let silent = l.silent().len();
    if silent > 0 {
        lines.push(format!(
            "{silent} more in the room not answering (in a match?)"
        ));
    }
    if !lines.is_empty() {
        LabelSpec::new(&lines.join("\n"), page_size * 0.6, (0.5, 0.17), Anchor::Cb)
            .spawn(commands, root);
    }
    bottom(commands, root, size, l, lobby);
}

/// Ready / Start and Leave, and the status line under them.
fn bottom(commands: &mut Commands, root: Entity, size: f32, l: &Lobby, lobby: &LobbyRoom) {
    let r = row(commands, root, (0.5, 0.1), Anchor::Cc, size);
    let status = if let Some(why) = l.failure() {
        format!("No connection to the room: {why}")
    } else if l.is_host() {
        // The seat count next to Start: the seat table may fill the
        // page's height, and a narrow window has no room beside it.
        cell_button(
            commands,
            r,
            "+ seat",
            DESC_BLUE,
            size,
            None,
            LobbyAction::AddSeat,
        );
        cell_button(
            commands,
            r,
            "- seat",
            DESC_BLUE,
            size,
            None,
            LobbyAction::RemoveSeat,
        );
        cell_button(
            commands,
            r,
            "Start",
            NAV_BLUE,
            size,
            None,
            LobbyAction::Start,
        );
        l.start_blocker()
            .map_or_else(|| "Everyone is ready: start!".to_string(), capitalised)
    } else if l.my_seat().is_some() {
        let ready = l.ready();
        cell_button(
            commands,
            r,
            if ready { "Not ready" } else { "Ready" },
            if ready { EXTREME_ORANGE } else { MEDIUM_GREEN },
            size,
            None,
            LobbyAction::Ready(!ready),
        );
        if ready {
            "Waiting for the host to start".to_string()
        } else {
            "Click Ready when you are".to_string()
        }
    } else if l.room().is_some() {
        "Watching: sit on an open seat to play".to_string()
    } else {
        "Waiting for the host…".to_string()
    };
    cell_button(
        commands,
        r,
        "Leave",
        NAV_BLUE,
        size,
        None,
        LobbyAction::Leave,
    );
    let (text, color) = match &lobby.notice {
        Some(n) => (capitalised(n.clone()), VERY_HARD_RED),
        None => (status, DESC_BLUE),
    };
    LabelSpec::new(&text, size * 0.8, (0.5, 0.035), Anchor::Cc)
        .frame(color)
        .spawn(commands, root);
}

fn capitalised(s: String) -> String {
    let mut c = s.chars();
    c.next()
        .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

/// A seat's occupant as its cell says it, and the cell's colour.
fn occupant_text(
    room: &RoomState,
    who: Occupant,
    me: Option<crate::net::transport::PeerId>,
) -> (String, Color) {
    match who {
        Occupant::Open => ("open".to_string(), DESC_BLUE),
        Occupant::Ai => ("AI".to_string(), HARD_YELLOW),
        Occupant::Human(p) => {
            let Some(m) = room.member(p) else {
                return (p.to_string(), DESC_BLUE);
            };
            let mut text = m.name.clone();
            if Some(p) == me {
                text.push_str(" (you)");
            }
            let color = if p == room.host {
                text.push_str(" - host");
                MEDIUM_GREEN
            } else {
                match &m.check {
                    Check::Refused(_) => {
                        text.push_str(" - can't play");
                        VERY_HARD_RED
                    }
                    Check::Pending => {
                        text.push_str(" - checking map");
                        EXTREME_ORANGE
                    }
                    Check::Ok if m.ready => {
                        text.push_str(" - ready");
                        MEDIUM_GREEN
                    }
                    Check::Ok => {
                        text.push_str(" - not ready");
                        EXTREME_ORANGE
                    }
                }
            };
            (text, color)
        }
    }
}

/// The faction colours of the showcase page.
fn faction_color(faction: Faction) -> Color {
    match faction {
        Faction::System => EASY_CYAN,
        Faction::Hacker => VERY_HARD_RED,
        Faction::Network => DESC_BLUE,
    }
}

/// A row of cells hung at `pos` (original coordinates, see
/// [`anchored`]).
fn row(
    commands: &mut Commands,
    root: Entity,
    pos: (f32, f32),
    anchor: Anchor,
    size: f32,
) -> Entity {
    let (mut node, transform) = anchored(pos.0, pos.1, anchor);
    node.flex_direction = FlexDirection::Row;
    node.align_items = AlignItems::Center;
    node.column_gap = Val::Px(size * 0.4);
    let r = commands.spawn((node, transform, Pickable::IGNORE)).id();
    commands.entity(root).add_child(r);
    r
}

fn cell_node(size: f32, min_width: Option<f32>) -> Node {
    Node {
        padding: frame_padding(size),
        border: frame_border(size),
        justify_content: JustifyContent::Center,
        min_width: min_width.map_or(Val::Auto, Val::Px),
        ..default()
    }
}

/// A clickable cell handing `action` to the lobby.
fn cell_button(
    commands: &mut Commands,
    row: Entity,
    label: &str,
    color: Color,
    size: f32,
    min_width: Option<f32>,
    action: LobbyAction,
) {
    choice_button(
        commands,
        row,
        (label, color, size),
        min_width,
        action,
        false,
    );
}

/// [`cell_button`]; `chosen`: the current pick of a choice row, drawn
/// as `ButtonSpec::chosen` draws it (brightened, heavier border).
fn choice_button(
    commands: &mut Commands,
    row: Entity,
    (label, color, size): (&str, Color, f32),
    min_width: Option<f32>,
    action: LobbyAction,
    chosen: bool,
) {
    let base = if chosen { brighten(color) } else { color };
    let text = spawn_button_text(commands, label, size);
    let mut node = cell_node(size, min_width);
    if chosen {
        node.border = UiRect::all(Val::Px((size * 0.18).det_max(2.0)));
    }
    let e = commands
        .spawn((
            MenuButton {
                action: MenuAction::Lobby(action),
                base,
            },
            node,
            fill(base),
            border(base),
        ))
        .add_child(text)
        .id();
    commands.entity(row).add_child(e);
}

/// A plain cell (a plate without a click).
fn cell_label(
    commands: &mut Commands,
    row: Entity,
    label: &str,
    color: Color,
    size: f32,
    min_width: Option<f32>,
) {
    let text = commands
        .spawn((
            Text::new(label),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size: FontSize::Px(size),
                ..default()
            },
            TextLayout::new(Justify::Center, LineBreak::NoWrap),
            Pickable::IGNORE,
        ))
        .id();
    let mut node = cell_node(size, min_width);
    if color == TEXT_WHITE {
        node.border = UiRect::ZERO;
    }
    let e = commands
        .spawn((MenuFrame, node, Pickable::IGNORE))
        .add_child(text)
        .id();
    if color != TEXT_WHITE {
        commands.entity(e).insert((fill(color), border(color)));
    }
    commands.entity(row).add_child(e);
}

/// The room code field: typed while the Multiplayer page shows the door.
pub(super) fn type_room_code(
    page: Res<MenuPage>,
    mut keys: MessageReader<KeyboardInput>,
    mut lobby: ResMut<LobbyRoom>,
) {
    if *page != MenuPage::Multiplayer || lobby.lobby.is_some() {
        keys.clear();
        return;
    }
    for key in keys.read() {
        if !key.state.is_pressed() {
            continue;
        }
        match &key.logical_key {
            Key::Character(s) => {
                for c in s.chars() {
                    if (c.is_ascii_alphanumeric() || c == '-' || c == '_')
                        && lobby.code.chars().count() < RoomId::MAX_LEN
                    {
                        lobby.code.push(c.to_ascii_lowercase());
                    }
                }
            }
            Key::Backspace => {
                lobby.code.pop();
            }
            _ => {}
        }
    }
}

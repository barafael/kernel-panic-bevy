//! The launch menu's Multiplayer page: open a room or join one by its
//! code, then the room itself (`net::lobby`) — the map and AI
//! difficulty, one row per seat (occupant, faction, team), ready /
//! start / leave.
//!
//! The host edits every seat; a guest sits down on an open seat (or
//! stands up from its own) and picks its own faction. Cells that are
//! not this player's to change are plain labels. Every row is one flex
//! row of frames, so long names and map names never run into the next
//! cell; the seat table and the chat column share one row too, and an
//! occupant too long for the table is clipped.
//!
//! Text fields (M3): the player's name (at the door and in the room),
//! the room code (at the door) and the chat line (in the room, with the
//! room's chat beside the seats). Keys go to the field last clicked
//! ([`Field`]); Enter joins, renames or sends. Typing updates the
//! field's label in place; the page is rebuilt when the room changes.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::text::FontSize;

use super::{
    Anchor, BUTTON_GREEN, ButtonSpec, DESC_BLUE, DIFFICULTIES, EASY_CYAN, EXTREME_ORANGE,
    HARD_YELLOW, Keep, LabelSpec, MAP_GREEN, MEDIUM_GREEN, MenuAction, MenuPage, NAV_BLUE, TEAL,
    TEXT_WHITE, VERY_HARD_RED, anchored, back_button, faction_color, page_heading, readable,
};
use crate::net::chat::{CHAT_MAX, ChatLog, type_into};
use crate::net::lobby::{
    Check, Field, Lobby, LobbyAction, LobbyRoom, NAME_MAX, Occupant, RoomId, RoomState,
};
use crate::units::team_colors::TeamColors;

/// The frame of a text field's button; its label follows the field
/// as it is typed into ([`type_into_fields`]).
#[derive(Component)]
pub(super) struct FieldLabel(Field);

/// The page: the door (create / join) outside a room, the room inside.
pub(super) fn multiplayer_page(
    commands: &mut Commands,
    root: Entity,
    (window_w, page_size): (f32, f32),
    lobby: &LobbyRoom,
    chat: &ChatLog,
) {
    match (lobby.lobby.as_ref(), lobby.room.as_ref()) {
        (Some(l), Some(room)) => {
            // The seat table and the chat column share a row from x=4 %
            // to 97.5 %: the table takes what the column leaves.
            let (mut node, transform) = anchored(0.04, SEAT_TOP, Anchor::Lt);
            node.width = Val::Percent(93.5);
            node.flex_direction = FlexDirection::Row;
            node.align_items = AlignItems::FlexStart;
            node.justify_content = JustifyContent::FlexEnd;
            node.column_gap = Val::Px(readable(page_size * 0.75));
            let columns = commands.spawn((node, transform, Pickable::IGNORE)).id();
            commands.entity(root).add_child(columns);
            room_view(commands, (root, columns), (window_w, page_size), l, room);
            bottom(commands, root, readable(page_size * 0.75), l, lobby);
            chat_panel(commands, columns, (window_w * 0.23, page_size), lobby, chat);
        }
        _ => door(commands, root, page_size, lobby),
    }
}

/// `text` as its field shows it: a cursor while it takes the keys.
fn field_text(lobby: &LobbyRoom, field: Field, text: &str) -> String {
    if lobby.field == field {
        format!("{text}_")
    } else {
        text.to_string()
    }
}

/// What a text field's button says, at the door or in a room.
fn field_label(lobby: &LobbyRoom, field: Field) -> String {
    let in_room = lobby.lobby.is_some() && lobby.room.is_some();
    match (field, in_room) {
        (Field::Name, false) => format!("Your name: {}", field_text(lobby, field, &lobby.name)),
        (Field::Main, false) => format!("Room code: {}", field_text(lobby, field, &lobby.code)),
        (Field::Name, true) => format!("Name: {}", field_text(lobby, field, &lobby.name)),
        (Field::Main, true) if lobby.chat.is_empty() && lobby.field == Field::Main => {
            "Type to chat_".to_string()
        }
        (Field::Main, true) => format!("Say: {}", field_text(lobby, field, &lobby.chat)),
    }
}

/// Outside a room: create one, or type a code and join.
fn door(commands: &mut Commands, root: Entity, page_size: f32, lobby: &LobbyRoom) {
    page_heading(commands, root, page_size, "Multiplayer");
    let name = ButtonSpec::new(
        &field_label(lobby, Field::Name),
        DESC_BLUE,
        page_size * 0.7,
        (0.5, 0.75),
        Anchor::Cc,
        MenuAction::Lobby(LobbyAction::Focus(Field::Name)),
    )
    .spawn(commands, root);
    commands.entity(name).insert(FieldLabel(Field::Name));
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
    let code = ButtonSpec::new(
        &field_label(lobby, Field::Main),
        MAP_GREEN,
        page_size,
        (0.5, 0.5),
        Anchor::Cc,
        MenuAction::Lobby(LobbyAction::Focus(Field::Main)),
    )
    .min_width(24.0)
    .spawn(commands, root);
    commands.entity(code).insert(FieldLabel(Field::Main));
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

/// Top edge of the seat table and the chat column, just under the AI
/// row.
const SEAT_TOP: f32 = 0.685;

/// Inside a room: the map, AI and rules rows on `root`, the seat table
/// first in `columns`.
fn room_view(
    commands: &mut Commands,
    (root, columns): (Entity, Entity),
    (window_w, page_size): (f32, f32),
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
    let Some(room) = l.room().filter(|_| !l.joining()) else {
        // A running match asked us in (`net::rejoin`): its map is
        // being checked.
        let text = if l.joining() {
            "A match is running in this room: joining it..."
        } else {
            "Meeting the room..."
        };
        LabelSpec::new(text, page_size * 0.8, (0.5, 0.6), Anchor::Cc).spawn(commands, root);
        return;
    };

    // Map and AI difficulty: the host's to change.
    let map_row = row(commands, root, (0.5, 0.8), Anchor::Cc, size);
    if host {
        button_cell("<", MAP_GREEN, size, LobbyAction::Map(-1)).spawn(commands, map_row);
    }
    label_cell(
        &format!("Map: {}", crate::map_loading::display_name(&room.map)),
        MAP_GREEN,
        size,
    )
    .spawn(commands, map_row);
    if host {
        button_cell(">", MAP_GREEN, size, LobbyAction::Map(1)).spawn(commands, map_row);
    }
    let ai_row = row(commands, root, (0.5, 0.72), Anchor::Cc, size);
    label_cell("AI:", TEXT_WHITE, size).spawn(commands, ai_row);
    for (i, (name, color)) in DIFFICULTIES.into_iter().enumerate() {
        let d = 1 + i as u8;
        let chosen = room.difficulty == d;
        if host {
            button_cell(name, color, size, LobbyAction::Difficulty(d))
                .min_width_px(window_w * 0.09)
                .chosen(chosen)
                .spawn(commands, ai_row);
        } else if chosen {
            label_cell(name, color, size).spawn(commands, ai_row);
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
        button_cell(
            pause,
            NAV_BLUE,
            size,
            LobbyAction::Pausable(!rules.pausable),
        )
        .spawn(commands, ai_row);
        button_cell(cheats, NAV_BLUE, size, LobbyAction::Cheats(!rules.cheats))
            .spawn(commands, ai_row);
    } else {
        label_cell(pause, TEXT_WHITE, size).spawn(commands, ai_row);
        label_cell(cheats, TEXT_WHITE, size).spawn(commands, ai_row);
    }

    // One row per seat, stacked by layout (rows are as tall as their
    // text, whatever the window). The table may shrink below its
    // rows' width: the occupant cell gives way.
    let table = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(size * 0.15),
                flex_grow: 1.0,
                min_width: Val::Px(0.0),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(columns).add_child(table);
    // What each seat's units will wear: open seats drop out at the
    // start, so the colours go to the occupied seats in order.
    let occupied = room.seats.iter().filter(|s| s.who != Occupant::Open);
    let colors = TeamColors::from_factions(occupied.map(|s| s.faction()));
    let mut owner = 0u8;
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
        label_cell(&format!("Seat {}", i + 1), TEXT_WHITE, size)
            .min_width_px(window_w * 0.08)
            .spawn(commands, r);
        let swatch = (seat.who != Occupant::Open).then(|| {
            owner += 1;
            colors.of(owner - 1, seat.faction()).color()
        });
        let swatch = commands
            .spawn((
                Node {
                    width: Val::Px(size * 0.7),
                    height: Val::Px(size * 0.7),
                    align_self: AlignSelf::Center,
                    flex_shrink: 0.0,
                    ..default()
                },
                BackgroundColor(swatch.unwrap_or(Color::NONE)),
                Pickable::IGNORE,
            ))
            .id();
        commands.entity(r).add_child(swatch);
        let mine = me.is_some_and(|me| seat.who == Occupant::Human(me));
        let (text, color) = occupant_text(room, seat.who, me);
        let wide = window_w * 0.3;
        if host || mine || seat.who == Occupant::Open {
            button_cell(&text, color, size, LobbyAction::Occupant(i as u8))
                .min_width_px(wide)
                .clip(Keep::Start)
                .spawn(commands, r);
        } else {
            label_cell(&text, color, size)
                .min_width_px(wide)
                .clip(Keep::Start)
                .spawn(commands, r);
        }
        let faction = seat.faction();
        let narrow = window_w * 0.11;
        seat_cell(
            commands,
            r,
            (&format!("{faction:?}"), faction_color(faction), size),
            narrow,
            (host || mine).then_some(LobbyAction::Faction(i as u8)),
        );
        seat_cell(
            commands,
            r,
            (&format!("Team {}", seat.ally_team + 1), TEAL, size),
            narrow,
            host.then_some(LobbyAction::AllyTeam(i as u8)),
        );
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
}

/// Chat lines shown beside the seats.
const CHAT_LINES: usize = 8;

/// The right-hand column of a room, `width` wide: our name (click to
/// rename), the room's chat, the line being typed. The two fields keep
/// the end of what is typed in view.
fn chat_panel(
    commands: &mut Commands,
    columns: Entity,
    (width, page_size): (f32, f32),
    lobby: &LobbyRoom,
    chat: &ChatLog,
) {
    let size = readable(page_size * 0.6);
    let panel = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(size * 0.25),
                width: Val::Px(width),
                flex_shrink: 0.0,
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(columns).add_child(panel);
    let name = button_cell(
        &field_label(lobby, Field::Name),
        DESC_BLUE,
        size,
        LobbyAction::Focus(Field::Name),
    )
    .clip(Keep::End)
    .spawn(commands, panel);
    commands.entity(name).insert(FieldLabel(Field::Name));
    let mut text = String::new();
    for (_, line) in chat.last(CHAT_LINES) {
        text.push_str(&format!("{}: {}\n", line.name, line.text));
    }
    if text.is_empty() {
        text.push_str("No one has said anything yet.\n");
    }
    let said = commands
        .spawn((
            Text::new(text.trim_end()),
            TextColor(TEXT_WHITE),
            TextFont {
                font_size: FontSize::Px(size * 0.85),
                ..default()
            },
            TextLayout::new(Justify::Left, LineBreak::WordBoundary),
            Node {
                width: Val::Px(width),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .id();
    commands.entity(panel).add_child(said);
    let typing = button_cell(
        &field_label(lobby, Field::Main),
        TEAL,
        size,
        LobbyAction::Focus(Field::Main),
    )
    .clip(Keep::End)
    .spawn(commands, panel);
    commands.entity(typing).insert(FieldLabel(Field::Main));
}

/// Ready / Start and Leave, and the status line under them.
fn bottom(commands: &mut Commands, root: Entity, size: f32, l: &Lobby, lobby: &LobbyRoom) {
    let r = row(commands, root, (0.5, 0.1), Anchor::Cc, size);
    let status = if let Some(why) = l.failure() {
        format!("No connection to the room: {why}")
    } else if let Some(stuck) = &lobby.unreached {
        capitalised(stuck.clone())
    } else if l.is_host() {
        // The seat count next to Start: the seat table may fill the
        // page's height, and a narrow window has no room beside it.
        button_cell("+ seat", DESC_BLUE, size, LobbyAction::AddSeat).spawn(commands, r);
        button_cell("- seat", DESC_BLUE, size, LobbyAction::RemoveSeat).spawn(commands, r);
        button_cell("Start", NAV_BLUE, size, LobbyAction::Start).spawn(commands, r);
        l.start_blocker()
            .map_or_else(|| "Everyone is ready: start!".to_string(), capitalised)
    } else if l.my_seat().is_some() {
        let ready = l.ready();
        button_cell(
            if ready { "Not ready" } else { "Ready" },
            if ready { EXTREME_ORANGE } else { MEDIUM_GREEN },
            size,
            LobbyAction::Ready(!ready),
        )
        .spawn(commands, r);
        if ready {
            "Waiting for the host to start".to_string()
        } else {
            "Click Ready when you are".to_string()
        }
    } else if l.room().is_some() {
        "Watching: sit on an open seat to play".to_string()
    } else {
        "Waiting for the host...".to_string()
    };
    button_cell("Leave", NAV_BLUE, size, LobbyAction::Leave).spawn(commands, r);
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

/// A clickable cell handing `action` to the lobby.
fn button_cell(label: &str, color: Color, size: f32, action: LobbyAction) -> ButtonSpec<'_> {
    ButtonSpec::cell(label, color, size, MenuAction::Lobby(action))
}

/// A plain cell: a plate in `color`, bare text in [`TEXT_WHITE`].
fn label_cell(label: &str, color: Color, size: f32) -> LabelSpec<'_> {
    let cell = LabelSpec::cell(label, size);
    if color == TEXT_WHITE {
        cell
    } else {
        cell.frame(color)
    }
}

/// A seat table cell at least `min_width` wide: a button while it is
/// this player's to change (`action`), a plain cell otherwise.
fn seat_cell(
    commands: &mut Commands,
    row: Entity,
    (label, color, size): (&str, Color, f32),
    min_width: f32,
    action: Option<LobbyAction>,
) {
    match action {
        Some(action) => button_cell(label, color, size, action)
            .min_width_px(min_width)
            .spawn(commands, row),
        None => label_cell(label, color, size)
            .min_width_px(min_width)
            .spawn(commands, row),
    };
}

/// The page's text fields ([`Field`]): the room code (at the door) or
/// the chat line (in a room), and the player's name. Enter joins, sends
/// the line or keeps the name; Space and Enter on an empty field are the
/// menu's (its keyboard navigation), and so are keys outside the page.
/// Typing only relabels the field's button ([`FieldLabel`]); what Enter
/// does rebuilds the page.
pub(super) fn type_into_fields(
    page: Res<MenuPage>,
    mut keys: MessageReader<KeyboardInput>,
    mut buttons: ResMut<ButtonInput<KeyCode>>,
    mut lobby: ResMut<LobbyRoom>,
    mut actions: MessageWriter<LobbyAction>,
    labels: Query<(&FieldLabel, &Children)>,
    mut texts: Query<&mut Text>,
) {
    if *page != MenuPage::Multiplayer {
        keys.clear();
        return;
    }
    let in_room = lobby.lobby.is_some();
    let mut typed = false;
    for key in keys.read() {
        if !key.state.is_pressed() {
            continue;
        }
        let room = lobby.bypass_change_detection();
        let (text, max, accept): (&mut String, usize, fn(char) -> bool) =
            match (room.field, in_room) {
                (Field::Name, _) => (&mut room.name, NAME_MAX, |_| true),
                (Field::Main, true) => (&mut room.chat, CHAT_MAX, |_| true),
                (Field::Main, false) => (&mut room.code, RoomId::MAX_LEN, |c| {
                    c.is_ascii_alphanumeric() || c == '-' || c == '_'
                }),
            };
        // A space or Enter in an empty field is the menu's.
        if text.is_empty() && matches!(key.logical_key, Key::Space | Key::Enter) {
            continue;
        }
        let acted = type_into(text, key, max, accept);
        typed = true;
        if room.field == Field::Main && !in_room {
            room.code = room.code.to_ascii_lowercase();
        }
        match key.logical_key {
            Key::Space => {
                buttons.clear_just_pressed(KeyCode::Space);
            }
            Key::Enter => {
                buttons.clear_just_pressed(KeyCode::Enter);
            }
            _ => {}
        }
        if acted != Some(Key::Enter) {
            continue;
        }
        let lobby = &mut *lobby;
        match (lobby.field, lobby.lobby.as_mut()) {
            (Field::Name, Some(l)) => {
                l.set_name(&lobby.name);
                lobby.name = l.name().to_string();
                lobby.field = Field::Main;
            }
            (Field::Name, None) => lobby.field = Field::Main,
            (Field::Main, Some(l)) => {
                let line = std::mem::take(&mut lobby.chat);
                if let Some(name) = line.strip_prefix("/name ") {
                    l.set_name(name);
                    lobby.name = l.name().to_string();
                } else if let Err(why) = l.say(&line) {
                    lobby.notice = Some(why);
                }
            }
            (Field::Main, None) => {
                actions.write(LobbyAction::Join);
            }
        }
    }
    if !typed {
        return;
    }
    for (field, children) in &labels {
        let label = field_label(&lobby, field.0);
        for child in children {
            if let Ok(mut text) = texts.get_mut(*child)
                && text.0 != label
            {
                text.0.clone_from(&label);
            }
        }
    }
}

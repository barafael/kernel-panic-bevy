use bevy::prelude::*;

use crate::interaction::Selected;
use crate::units::components::{Faction, Health, UnitType};
use crate::units::definitions::{self, UnitKind, UnitStats};

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (update_info_panel, update_build_menu, update_order_palette),
        );
    }
}

// ---------------------------------------------------------------------------
// Marker components for UI nodes
// ---------------------------------------------------------------------------

/// Root container for the unit info panel (bottom-left, above minimap).
#[derive(Component)]
struct InfoPanel;

/// Root container for the build menu (left side).
#[derive(Component)]
struct BuildMenu;

/// Root container for the order palette (bottom-right).
#[derive(Component)]
struct OrderPalette;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Faction-tinted green used for UI borders/accents (matches the original KP Tron aesthetic).
const UI_BORDER_COLOR: Color = Color::linear_rgb(0.0, 0.7, 0.2);
const UI_BG_COLOR: Color = Color::srgba(0.0, 0.05, 0.0, 0.75);
const UI_TEXT_COLOR: Color = Color::linear_rgb(0.0, 1.0, 0.3);
const UI_TEXT_DIM: Color = Color::linear_rgb(0.0, 0.5, 0.15);
const UI_HEALTH_GREEN: Color = Color::linear_rgb(0.0, 1.0, 0.2);
const UI_HEALTH_YELLOW: Color = Color::linear_rgb(1.0, 1.0, 0.0);
const UI_HEALTH_RED: Color = Color::linear_rgb(1.0, 0.0, 0.0);

const FONT_SIZE_TITLE: f32 = 18.0;
const FONT_SIZE_BODY: f32 = 14.0;
const FONT_SIZE_SMALL: f32 = 12.0;

const BUILD_ICON_SIZE: f32 = 56.0;

// ---------------------------------------------------------------------------
// Unit info panel (bottom-left)
// ---------------------------------------------------------------------------

fn update_info_panel(
    selected_q: Query<(&UnitType, &Health, &Faction), With<Selected>>,
    existing: Query<Entity, With<InfoPanel>>,
    mut commands: Commands,
) {
    // Always despawn old panel, rebuild from scratch.
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    let selected: Vec<_> = selected_q.iter().collect();
    if selected.is_empty() {
        return;
    }

    // If a single unit is selected, show detailed info.
    // If multiple, show a count summary.
    let panel = commands
        .spawn((
            InfoPanel,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                bottom: Val::Px(220.0), // above the minimap
                width: Val::Px(200.0),
                padding: UiRect::all(Val::Px(8.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(UI_BORDER_COLOR),
            BackgroundColor(UI_BG_COLOR),
        ))
        .id();

    if selected.len() == 1 {
        let (unit_type, health, faction) = selected[0];
        let stats = definitions::stats(unit_type.0);
        spawn_single_unit_info(&mut commands, panel, stats, health, faction);
    } else {
        spawn_multi_unit_info(&mut commands, panel, &selected);
    }
}

fn spawn_single_unit_info(
    commands: &mut Commands,
    parent: Entity,
    stats: &UnitStats,
    health: &Health,
    faction: &Faction,
) {
    // Unit name
    let name_node = commands
        .spawn((
            Text::new(stats.name),
            TextFont {
                font_size: FONT_SIZE_TITLE,
                ..default()
            },
            TextColor(faction.color()),
        ))
        .id();
    commands.entity(parent).add_child(name_node);

    // Health bar
    let health_fraction = health.fraction();
    let bar_color = health_bar_color(health_fraction);

    let bar_container = commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Px(10.0),
            border: UiRect::all(Val::Px(1.0)),
            ..default()
        })
        .id();

    let bar_bg = commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.1, 0.1, 0.1, 0.8)),
        ))
        .id();

    let bar_fg = commands
        .spawn((
            Node {
                width: Val::Percent(health_fraction * 100.0),
                height: Val::Percent(100.0),
                position_type: PositionType::Absolute,
                ..default()
            },
            BackgroundColor(bar_color),
        ))
        .id();

    commands
        .entity(bar_container)
        .add_children(&[bar_bg, bar_fg]);
    commands.entity(parent).add_child(bar_container);

    // HP text
    let hp_text = format!("{:.0} / {:.0}", health.current, health.max);
    let hp_node = commands
        .spawn((
            Text::new(hp_text),
            TextFont {
                font_size: FONT_SIZE_SMALL,
                ..default()
            },
            TextColor(UI_TEXT_DIM),
        ))
        .id();
    commands.entity(parent).add_child(hp_node);

    // Weapon info (if armed)
    if !stats.weapon.is_empty() {
        let weapon_text = format!(
            "DMG {:.0}  RNG {:.0}",
            stats.attack_damage, stats.attack_range,
        );
        let weapon_node = commands
            .spawn((
                Text::new(weapon_text),
                TextFont {
                    font_size: FONT_SIZE_BODY,
                    ..default()
                },
                TextColor(UI_TEXT_COLOR),
            ))
            .id();
        commands.entity(parent).add_child(weapon_node);
    }

    // Speed (if mobile)
    if stats.speed > 0.0 {
        let speed_text = format!("SPD {:.0}", stats.speed);
        let speed_node = commands
            .spawn((
                Text::new(speed_text),
                TextFont {
                    font_size: FONT_SIZE_BODY,
                    ..default()
                },
                TextColor(UI_TEXT_COLOR),
            ))
            .id();
        commands.entity(parent).add_child(speed_node);
    }
}

fn spawn_multi_unit_info(
    commands: &mut Commands,
    parent: Entity,
    selected: &[(&UnitType, &Health, &Faction)],
) {
    // Count by type.
    let mut counts: Vec<(UnitKind, u32)> = Vec::new();
    for (unit_type, _, _) in selected {
        if let Some(entry) = counts.iter_mut().find(|(k, _)| *k == unit_type.0) {
            entry.1 += 1;
        } else {
            counts.push((unit_type.0, 1));
        }
    }

    let header = format!("{} units selected", selected.len());
    let header_node = commands
        .spawn((
            Text::new(header),
            TextFont {
                font_size: FONT_SIZE_TITLE,
                ..default()
            },
            TextColor(UI_TEXT_COLOR),
        ))
        .id();
    commands.entity(parent).add_child(header_node);

    for (kind, count) in &counts {
        let stats = definitions::stats(*kind);
        let line = if *count > 1 {
            format!("{}x {}", count, stats.name)
        } else {
            stats.name.to_string()
        };
        let line_node = commands
            .spawn((
                Text::new(line),
                TextFont {
                    font_size: FONT_SIZE_BODY,
                    ..default()
                },
                TextColor(UI_TEXT_DIM),
            ))
            .id();
        commands.entity(parent).add_child(line_node);
    }
}

fn health_bar_color(fraction: f32) -> Color {
    if fraction > 0.5 {
        UI_HEALTH_GREEN
    } else if fraction > 0.25 {
        UI_HEALTH_YELLOW
    } else {
        UI_HEALTH_RED
    }
}

// ---------------------------------------------------------------------------
// Build menu (left side) — for factories
// ---------------------------------------------------------------------------

/// What a factory type can build. Returns a list of buildable unit kinds.
fn buildable_units(kind: UnitKind) -> &'static [UnitKind] {
    match kind {
        // Kernel produces Bits, and the player can queue Assemblers, Bytes, Pointers, Firewalls.
        UnitKind::Kernel => &[
            UnitKind::Bit,
            UnitKind::Assembler,
            UnitKind::Byte,
            UnitKind::Pointer,
            UnitKind::Firewall,
        ],
        UnitKind::Socket => &[UnitKind::Bit],
        // Hole produces Bugs, and the player can queue Worms, DOS, Logic Bombs.
        UnitKind::Hole => &[
            UnitKind::Bug,
            UnitKind::Worm,
            UnitKind::Dos,
            UnitKind::LogicBomb,
        ],
        UnitKind::Window => &[UnitKind::Bug],
        // Connection produces Packets, and the player can queue Signals.
        UnitKind::Connection => &[UnitKind::Packet, UnitKind::Signal],
        UnitKind::Port => &[UnitKind::Packet],
        // Assembler is a mobile builder that can build Sockets and Firewalls on datavents.
        UnitKind::Assembler => &[UnitKind::Socket, UnitKind::Firewall],
        _ => &[],
    }
}

fn update_build_menu(
    selected_q: Query<(&UnitType, &Faction), With<Selected>>,
    existing: Query<Entity, With<BuildMenu>>,
    mut commands: Commands,
) {
    // Always rebuild.
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    // Find the first selected unit that has build options.
    let builder = selected_q
        .iter()
        .find(|(ut, _)| !buildable_units(ut.0).is_empty());
    let Some((unit_type, faction)) = builder else {
        return;
    };
    let options = buildable_units(unit_type.0);
    if options.is_empty() {
        return;
    }

    let menu = commands
        .spawn((
            BuildMenu,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                top: Val::Px(8.0),
                width: Val::Px(BUILD_ICON_SIZE * 2.0 + 24.0),
                padding: UiRect::all(Val::Px(8.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(UI_BORDER_COLOR),
            BackgroundColor(UI_BG_COLOR),
        ))
        .id();

    // Title
    let title = commands
        .spawn((
            Text::new("BUILD"),
            TextFont {
                font_size: FONT_SIZE_TITLE,
                ..default()
            },
            TextColor(UI_TEXT_COLOR),
        ))
        .id();
    commands.entity(menu).add_child(title);

    // Grid of build option icons (2 columns)
    let grid = commands
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            column_gap: Val::Px(4.0),
            row_gap: Val::Px(4.0),
            ..default()
        })
        .id();
    commands.entity(menu).add_child(grid);

    for kind in options {
        let stats = definitions::stats(*kind);
        let icon = spawn_build_icon(&mut commands, stats, faction);
        commands.entity(grid).add_child(icon);
    }
}

fn spawn_build_icon(commands: &mut Commands, stats: &UnitStats, faction: &Faction) -> Entity {
    // Each build icon is a bordered box with the unit name and build time.
    let icon = commands
        .spawn((
            Node {
                width: Val::Px(BUILD_ICON_SIZE),
                height: Val::Px(BUILD_ICON_SIZE),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(faction.color()),
            BackgroundColor(Color::srgba(0.0, 0.1, 0.0, 0.6)),
        ))
        .id();

    // Unit name (short, centered)
    let name = commands
        .spawn((
            Text::new(stats.name),
            TextFont {
                font_size: FONT_SIZE_SMALL,
                ..default()
            },
            TextColor(UI_TEXT_COLOR),
            TextLayout::new_with_justify(Justify::Center),
        ))
        .id();
    commands.entity(icon).add_child(name);

    // Build time
    if stats.build_time > 0.0 {
        let time_text = format!("{:.0}s", stats.build_time);
        let time_node = commands
            .spawn((
                Text::new(time_text),
                TextFont {
                    font_size: FONT_SIZE_SMALL,
                    ..default()
                },
                TextColor(UI_TEXT_DIM),
                TextLayout::new_with_justify(Justify::Center),
            ))
            .id();
        commands.entity(icon).add_child(time_node);
    }

    icon
}

// ---------------------------------------------------------------------------
// Order palette (bottom-right) — for non-factory combat units
// ---------------------------------------------------------------------------

/// Available orders for non-builder units.
const ORDERS: &[(&str, &str)] = &[
    ("Stop", "S"),
    ("Move", "M"),
    ("Attack", "A"),
    ("Patrol", "P"),
    ("Guard", "G"),
];

fn update_order_palette(
    selected_q: Query<&UnitType, With<Selected>>,
    existing: Query<Entity, With<OrderPalette>>,
    mut commands: Commands,
) {
    // Always rebuild.
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    if selected_q.is_empty() {
        return;
    }

    // Only show when at least one selected unit is mobile.
    let has_mobile = selected_q.iter().any(|ut| {
        let stats = definitions::stats(ut.0);
        stats.speed > 0.0
    });

    if !has_mobile {
        return;
    }

    let palette = commands
        .spawn((
            OrderPalette,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(8.0),
                bottom: Val::Px(8.0),
                padding: UiRect::all(Val::Px(8.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BorderColor::all(UI_BORDER_COLOR),
            BackgroundColor(UI_BG_COLOR),
        ))
        .id();

    // Title
    let title = commands
        .spawn((
            Text::new("ORDERS"),
            TextFont {
                font_size: FONT_SIZE_TITLE,
                ..default()
            },
            TextColor(UI_TEXT_COLOR),
        ))
        .id();
    commands.entity(palette).add_child(title);

    // Grid of order buttons (3 columns)
    let grid = commands
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            column_gap: Val::Px(4.0),
            row_gap: Val::Px(4.0),
            ..default()
        })
        .id();
    commands.entity(palette).add_child(grid);

    for (name, hotkey) in ORDERS {
        let btn = spawn_order_button(&mut commands, name, hotkey);
        commands.entity(grid).add_child(btn);
    }
}

fn spawn_order_button(commands: &mut Commands, name: &str, hotkey: &str) -> Entity {
    let btn = commands
        .spawn((
            Node {
                width: Val::Px(60.0),
                height: Val::Px(40.0),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(UI_BORDER_COLOR),
            BackgroundColor(Color::srgba(0.0, 0.1, 0.0, 0.6)),
        ))
        .id();

    let label = commands
        .spawn((
            Text::new(name.to_string()),
            TextFont {
                font_size: FONT_SIZE_SMALL,
                ..default()
            },
            TextColor(UI_TEXT_COLOR),
            TextLayout::new_with_justify(Justify::Center),
        ))
        .id();
    commands.entity(btn).add_child(label);

    let key_label = commands
        .spawn((
            Text::new(format!("[{hotkey}]")),
            TextFont {
                font_size: FONT_SIZE_SMALL,
                ..default()
            },
            TextColor(UI_TEXT_DIM),
            TextLayout::new_with_justify(Justify::Center),
        ))
        .id();
    commands.entity(btn).add_child(key_label);

    btn
}

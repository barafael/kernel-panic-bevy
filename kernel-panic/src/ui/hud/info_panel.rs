//! Bottom-left info panel: shows HP, weapon, and speed for the current
//! selection. Switches to a multi-unit summary when more than one unit is
//! selected.

use bevy::prelude::*;

use super::style::{
    FONT_SIZE_BODY, FONT_SIZE_SMALL, FONT_SIZE_TITLE, UI_BG_COLOR, UI_BORDER_COLOR, UI_ROW_BG,
    UI_TEXT_COLOR, UI_TEXT_DIM,
};
use crate::interaction::Selected;
use crate::units::components::{Faction, Health, UnitType, health_color};
use crate::units::definitions::UnitKind;
use crate::units::unit_registry::UnitRegistry;

pub struct InfoPanelPlugin;

impl Plugin for InfoPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, update_info_panel);
    }
}

#[derive(Component)]
struct InfoPanel;

/// Snapshot of the state the info panel renders. Rebuilding the panel
/// every frame was pointless — the panel only changes when the selection
/// or HP of a selected unit changes. Hashing is cheap compared to
/// despawning + respawning ~10 UI entities per frame.
fn update_info_panel(
    selected_q: Query<(&UnitType, &Health, &Faction), With<Selected>>,
    existing: Query<Entity, With<InfoPanel>>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
    mut last_hash: Local<u64>,
) {
    let selected: Vec<_> = selected_q.iter().collect();

    let mut hash: u64 = selected.len() as u64;
    for (unit_type, health, _) in &selected {
        hash = hash
            .wrapping_mul(1315423911)
            .wrapping_add(unit_type.0 as u64);
        // HP changes frame-to-frame while the unit heals or bleeds, but
        // the panel only shows rounded integers — bucket on those.
        hash = hash
            .wrapping_mul(1315423911)
            .wrapping_add(health.current.round() as i64 as u64);
        hash = hash
            .wrapping_mul(1315423911)
            .wrapping_add(health.max.round() as i64 as u64);
    }
    if hash == *last_hash {
        return;
    }
    *last_hash = hash;

    for entity in &existing {
        commands.entity(entity).despawn();
    }

    if selected.is_empty() {
        return;
    }

    let panel = commands
        .spawn((
            InfoPanel,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                bottom: Val::Px(0.0),
                width: Val::Px(220.0),
                padding: UiRect {
                    left: Val::Px(8.0),
                    top: Val::Px(8.0),
                    right: Val::Px(8.0),
                    bottom: Val::Px(0.0),
                },
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(4.0),
                border: UiRect {
                    left: Val::Px(0.0),
                    top: Val::Px(1.0),
                    right: Val::Px(1.0),
                    bottom: Val::Px(0.0),
                },
                ..default()
            },
            BorderColor::all(UI_BORDER_COLOR),
            BackgroundColor(UI_BG_COLOR),
        ))
        .id();

    if selected.len() == 1 {
        let (unit_type, health, faction) = selected[0];
        spawn_single_unit_info(
            &mut commands,
            panel,
            unit_type.0,
            health,
            faction,
            &unit_registry,
        );
    } else {
        spawn_multi_unit_info(&mut commands, panel, &selected, &unit_registry);
    }
}

fn spawn_single_unit_info(
    commands: &mut Commands,
    parent: Entity,
    kind: UnitKind,
    health: &Health,
    faction: &Faction,
    unit_registry: &UnitRegistry,
) {
    // Unit name
    let name_node = commands
        .spawn((
            Text::new(unit_registry.name(kind)),
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
    let bar_color = health_color(health_fraction);

    let bar_container = commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Px(10.0),
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
            BackgroundColor(UI_ROW_BG),
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

    // Weapon info
    let weapon_name = unit_registry.weapon(kind);
    if !weapon_name.is_empty() {
        let weapon_text = format!("WPN {weapon_name}");
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

    // Speed
    let speed = unit_registry.speed(kind);
    if speed > 0.0 {
        let speed_text = format!("SPD {speed:.0}");
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
    unit_registry: &UnitRegistry,
) {
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
        let name = unit_registry.name(*kind);
        let line = if *count > 1 {
            format!("{}x {}", count, name)
        } else {
            name.to_string()
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

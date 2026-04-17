//! Top-left build menu: shows buildable units for the selected factory
//! and its current production progress/queue. Clicking an icon enqueues a
//! unit on a stationary factory, or enters datavent placement mode on a
//! mobile constructor (Assembler / Trojan / Gateway).

use bevy::prelude::*;

use super::previews::UnitPreviews;
use super::style::{
    BUILD_ICON_SIZE, FONT_SIZE_SMALL, FONT_SIZE_TITLE, UI_BG_COLOR, UI_BORDER_COLOR,
    UI_PROGRESS_COLOR, UI_TEXT_COLOR, UI_TEXT_DIM,
};
use crate::interaction::Selected;
use crate::units::components::{Faction, UnitType};
use crate::units::construction::{buildings_for, is_constructor};
use crate::units::definitions::UnitKind;
use crate::units::production::Producer;
use crate::units::unit_registry::UnitRegistry;

pub struct BuildMenuPlugin;

impl Plugin for BuildMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BuildOrderEvent>()
            .add_message::<BeginPlacementEvent>()
            .add_systems(
                Update,
                (update_build_menu, handle_build_clicks, apply_build_orders),
            );
    }
}

/// Fired when the player clicks a build icon on a stationary factory.
#[derive(Message)]
struct BuildOrderEvent {
    kind: UnitKind,
}

/// Fired when the player clicks a build icon on a mobile constructor.
/// The placement module (`placement.rs`) picks this up, spawns a ghost
/// preview of the building, and waits for a left-click on a datavent
/// before issuing a `BuildAt` command (shift-queued if shift is held).
#[derive(Message)]
pub struct BeginPlacementEvent {
    pub builder: Entity,
    pub kind: UnitKind,
}

#[derive(Component)]
struct BuildMenu;

/// Attached to a build icon button. Carries the unit kind to build.
#[derive(Component)]
struct BuildIcon(UnitKind);

/// Build options per factory/constructor type.
///
/// Matches upstream `SIDEDATA.TDF` with one game-design rule overlaid:
/// **homebases (Kernel / Hole / Connection) build only mobile units**;
/// static structures (Socket, Window, Port, Firewall, LogicBomb, …) are
/// produced by the mobile builder line (Assembler / Trojan / Gateway),
/// which requires placing the building on a datavent. Construction unit
/// build lists live in `construction::buildings_for`.
fn buildable_units(kind: UnitKind) -> &'static [UnitKind] {
    match kind {
        UnitKind::Kernel => &[
            UnitKind::Bit,
            UnitKind::Assembler,
            UnitKind::Byte,
            UnitKind::Pointer,
        ],
        UnitKind::Socket => &[UnitKind::Bit],
        UnitKind::Hole => &[
            UnitKind::Bug,
            UnitKind::Worm,
            UnitKind::Dos,
            UnitKind::Trojan,
        ],
        UnitKind::Window => &[UnitKind::Bug],
        UnitKind::Connection => &[UnitKind::Packet, UnitKind::Signal, UnitKind::Gateway],
        UnitKind::Port => &[UnitKind::Packet],
        kind if is_constructor(kind) => buildings_for(kind),
        _ => &[],
    }
}

fn update_build_menu(
    selected_q: Query<(&UnitType, &Faction), With<Selected>>,
    producer_q: Query<(&Producer, &UnitType), With<Selected>>,
    existing: Query<Entity, With<BuildMenu>>,
    previews: Res<UnitPreviews>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
) {
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

    let menu = commands
        .spawn((
            BuildMenu,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                top: Val::Px(8.0),
                width: Val::Px(BUILD_ICON_SIZE * 2.0 + 28.0),
                padding: UiRect::all(Val::Px(8.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
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

    // Build progress (if this unit is a producer and has something queued)
    if let Some((producer, _)) = producer_q.iter().next()
        && let Some(current_kind) = producer.current_production()
    {
        spawn_build_progress(&mut commands, menu, producer, current_kind, &unit_registry);
    }

    // Grid of build icons (2 columns)
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
        let name = unit_registry.name(*kind);
        let preview = previews.get(*kind).cloned();
        let icon = spawn_build_icon(&mut commands, name, faction, *kind, preview);
        commands.entity(grid).add_child(icon);
    }
}

/// Render the "Building: Bit (75%)" label, progress bar, and remaining queue.
fn spawn_build_progress(
    commands: &mut Commands,
    menu: Entity,
    producer: &Producer,
    current_kind: UnitKind,
    unit_registry: &UnitRegistry,
) {
    let producing_name = unit_registry.name(current_kind);
    let progress = producer.progress_fraction(unit_registry);

    let progress_text = format!("Building: {} ({:.0}%)", producing_name, progress * 100.0);
    let progress_label = commands
        .spawn((
            Text::new(progress_text),
            TextFont {
                font_size: FONT_SIZE_SMALL,
                ..default()
            },
            TextColor(UI_TEXT_DIM),
        ))
        .id();
    commands.entity(menu).add_child(progress_label);

    let bar_container = commands
        .spawn(Node {
            width: Val::Percent(100.0),
            height: Val::Px(6.0),
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
                width: Val::Percent(progress * 100.0),
                height: Val::Percent(100.0),
                position_type: PositionType::Absolute,
                ..default()
            },
            BackgroundColor(UI_PROGRESS_COLOR),
        ))
        .id();

    commands
        .entity(bar_container)
        .add_children(&[bar_bg, bar_fg]);
    commands.entity(menu).add_child(bar_container);

    // Queue display — coalesce consecutive same-kind entries into "3x Bit".
    let queue = producer.queue();
    if !queue.is_empty() {
        let mut queue_parts: Vec<String> = Vec::new();
        let mut prev_kind: Option<UnitKind> = None;
        let mut count = 0u32;
        for kind in queue {
            if prev_kind == Some(*kind) {
                count += 1;
            } else {
                if let Some(pk) = prev_kind {
                    let name = unit_registry.name(pk);
                    if count > 1 {
                        queue_parts.push(format!("{count}x {name}"));
                    } else {
                        queue_parts.push(name.to_string());
                    }
                }
                prev_kind = Some(*kind);
                count = 1;
            }
        }
        if let Some(pk) = prev_kind {
            let name = unit_registry.name(pk);
            if count > 1 {
                queue_parts.push(format!("{count}x {name}"));
            } else {
                queue_parts.push(name.to_string());
            }
        }

        let queue_str = format!("Queue: {}", queue_parts.join(", "));
        let queue_node = commands
            .spawn((
                Text::new(queue_str),
                TextFont {
                    font_size: FONT_SIZE_SMALL,
                    ..default()
                },
                TextColor(UI_TEXT_DIM),
            ))
            .id();
        commands.entity(menu).add_child(queue_node);
    }
}

fn spawn_build_icon(
    commands: &mut Commands,
    name: &str,
    faction: &Faction,
    kind: UnitKind,
    preview: Option<Handle<Image>>,
) -> Entity {
    let icon = commands
        .spawn((
            BuildIcon(kind),
            Button,
            Node {
                width: Val::Px(BUILD_ICON_SIZE),
                height: Val::Px(BUILD_ICON_SIZE),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::End,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BorderColor::all(faction.color()),
            BackgroundColor(Color::srgba(0.0, 0.1, 0.0, 0.6)),
        ))
        .id();

    // Preview image
    if let Some(image_handle) = preview {
        let img = commands
            .spawn((
                ImageNode::new(image_handle),
                Node {
                    width: Val::Px(BUILD_ICON_SIZE - 8.0),
                    height: Val::Px(BUILD_ICON_SIZE - 24.0),
                    ..default()
                },
            ))
            .id();
        commands.entity(icon).add_child(img);
    }

    // Unit name
    let name_label = commands
        .spawn((
            Text::new(name),
            TextFont {
                font_size: FONT_SIZE_SMALL,
                ..default()
            },
            TextColor(UI_TEXT_COLOR),
            TextLayout::new_with_justify(Justify::Center),
        ))
        .id();
    commands.entity(icon).add_child(name_label);

    icon
}

fn handle_build_clicks(
    interaction_q: Query<(&Interaction, &BuildIcon), Changed<Interaction>>,
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    mut ev_build: MessageWriter<BuildOrderEvent>,
    mut ev_placement: MessageWriter<BeginPlacementEvent>,
) {
    for (interaction, build_icon) in &interaction_q {
        if *interaction != Interaction::Pressed {
            continue;
        }
        // Route the click based on what the first selected unit is: a
        // mobile constructor enters placement mode, a stationary factory
        // just enqueues. If nothing that can build is selected, drop the
        // click — the icons shouldn't be visible in that case anyway.
        let Some((builder_entity, ut)) = selected_q
            .iter()
            .find(|(_, ut)| is_constructor(ut.0) || !buildable_units(ut.0).is_empty())
        else {
            continue;
        };
        if is_constructor(ut.0) {
            ev_placement.write(BeginPlacementEvent {
                builder: builder_entity,
                kind: build_icon.0,
            });
        } else {
            ev_build.write(BuildOrderEvent { kind: build_icon.0 });
        }
    }
}

fn apply_build_orders(
    mut ev_build: MessageReader<BuildOrderEvent>,
    mut producers: Query<&mut Producer, With<Selected>>,
) {
    for event in ev_build.read() {
        // Enqueue on the first selected producer.
        if let Some(mut producer) = producers.iter_mut().next() {
            producer.enqueue(event.kind);
        }
    }
}

//! Top-left build menu: shows buildable units for the selected factory
//! and its current production progress/queue. Clicking an icon enqueues a
//! unit on a stationary factory, or enters datavent placement mode on a
//! mobile constructor (Assembler / Trojan / Gateway).

use bevy::prelude::*;

use super::previews::UnitPreviews;
use super::style::{
    BUILD_ICON_SIZE, FONT_SIZE_SMALL, FONT_SIZE_TITLE, UI_BG_COLOR, UI_BORDER_COLOR, UI_PANEL_TINT,
    UI_TEXT_COLOR, UI_TEXT_DIM,
};
use crate::interaction::Selected;
use crate::units::components::{Faction, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::construction::buildings_for;
use crate::units::lifecycle::production::Producer;

pub struct BuildMenuPlugin;

impl Plugin for BuildMenuPlugin {
    fn build(&self, app: &mut App) {
        // Order matters: `update_build_menu` rebuilds the icon subtree
        // whenever production progress shifts the hash (queue advances,
        // current production changes). If it runs before
        // `handle_build_clicks` within a frame, the just-pressed icon
        // entity is despawned before its `Changed<Interaction>` is
        // read — clicks evaporate. Run handler first, then rebuild.
        app.add_message::<BuildOrderEvent>()
            .add_message::<BeginPlacementEvent>()
            .add_systems(
                Update,
                (handle_build_clicks, apply_build_orders, update_build_menu).chain(),
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
        // Upstream `SIDEDATA.TDF [carrier]`: packet, connection, flow,
        // gateway. All mobile — buildings come from the Gateway, which
        // is the Network line's mobile constructor.
        UnitKind::Carrier => &[
            UnitKind::Packet,
            UnitKind::Connection,
            UnitKind::Flow,
            UnitKind::Gateway,
        ],
        UnitKind::Port => &[UnitKind::Packet],
        kind if kind.is_constructor() => buildings_for(kind),
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
    mut last_hash: Local<u64>,
) {
    // Rebuild only when the selection + production state actually
    // changes. Without this guard the system despawns and respawns the
    // entire build-icon subtree every Update tick even on fully static
    // frames.
    //
    // Seeded with a non-zero FNV-1a 64-bit offset basis so a
    // legitimate "Kernel + System" selection (both enum variants
    // discriminate to 0) doesn't collide with the `Local<u64>::default()
    // == 0` first-frame sentinel — the unguarded version returned
    // early on every frame for the System homebase, and the build
    // menu never appeared.
    let builder = selected_q
        .iter()
        .find(|(ut, _)| !buildable_units(ut.0).is_empty());
    let producer_slot = producer_q.iter().next();

    let mut hash: u64 = 0xcbf29ce484222325;
    if let Some((ut, faction)) = builder {
        hash = hash.wrapping_mul(2654435761).wrapping_add(ut.0 as u64);
        hash = hash.wrapping_mul(2654435761).wrapping_add(*faction as u64);
    }
    if let Some((producer, _)) = producer_slot {
        if let Some(kind) = producer.current_production() {
            hash = hash
                .wrapping_mul(2654435761)
                .wrapping_add(kind as u64 | 0x1000);
        }
        hash = hash
            .wrapping_mul(2654435761)
            .wrapping_add(producer.queue().len() as u64);
        for kind in producer.queue() {
            hash = hash.wrapping_mul(2654435761).wrapping_add(*kind as u64);
        }
    }
    if hash == *last_hash {
        return;
    }
    *last_hash = hash;

    for entity in &existing {
        commands.entity(entity).despawn();
    }

    let Some((unit_type, faction)) = builder else {
        return;
    };
    let options = buildable_units(unit_type.0);

    // Mid-left: anchor at 50% top with a translateY trick via `top` minus
    // half the menu's expected height. Bevy 0.18 doesn't expose CSS
    // transforms on Nodes, so we use top + a margin-top offset of half
    // the icon block; this keeps the menu visually centered on the
    // left edge.
    let menu = commands
        .spawn((
            BuildMenu,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Percent(50.0),
                width: Val::Px(BUILD_ICON_SIZE * 2.0 + 28.0),
                padding: UiRect::all(Val::Px(8.0)),
                margin: UiRect {
                    top: Val::Px(-((BUILD_ICON_SIZE * 2.0) + 24.0)),
                    ..default()
                },
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                border: UiRect {
                    left: Val::Px(0.0),
                    top: Val::Px(1.0),
                    right: Val::Px(1.0),
                    bottom: Val::Px(1.0),
                },
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

    // Queue summary (if this unit is a producer with something queued)
    if let Some((producer, _)) = producer_q.iter().next()
        && producer.current_production().is_some()
    {
        spawn_build_queue(&mut commands, menu, producer, &unit_registry);
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

    // Count how many of each kind are queued on the producer in focus, so
    // each build icon can surface its own pending-count badge (FEATURES.md §4).
    let queue_counts: std::collections::HashMap<UnitKind, u32> = producer_q
        .iter()
        .next()
        .map(|(producer, _)| {
            let mut counts = std::collections::HashMap::new();
            for kind in producer.queue() {
                *counts.entry(*kind).or_insert(0u32) += 1;
            }
            counts
        })
        .unwrap_or_default();

    for kind in options {
        let name = unit_registry.name(*kind);
        let preview = previews.get(*kind).cloned();
        let count = queue_counts.get(kind).copied().unwrap_or(0);
        let icon = spawn_build_icon(&mut commands, name, faction, *kind, preview, count);
        commands.entity(grid).add_child(icon);
    }
}

/// Render the remaining build queue ("Queue: 3x Bit, Byte"). The player
/// tracks production progress via the rising HP bar of the unit still in
/// the factory — there is no numeric progress bar in the HUD.
fn spawn_build_queue(
    commands: &mut Commands,
    menu: Entity,
    producer: &Producer,
    unit_registry: &UnitRegistry,
) {
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
    queue_count: u32,
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
            BackgroundColor(UI_PANEL_TINT),
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

    // Queue-count badge in the bottom-left corner (FEATURES.md §4).
    // Hidden entirely when the queue is empty so the empty state reads
    // clean; rendered as an absolutely-positioned text node so it sits
    // inside the icon's bounds regardless of layout rounding.
    if queue_count > 0 {
        let badge = commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(2.0),
                    bottom: Val::Px(2.0),
                    ..default()
                },
                Text::new(queue_count.to_string()),
                TextFont {
                    font_size: FONT_SIZE_SMALL,
                    ..default()
                },
                TextColor(UI_TEXT_COLOR),
                BackgroundColor(UI_PANEL_TINT),
            ))
            .id();
        commands.entity(icon).add_child(badge);
    }

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
            .find(|(_, ut)| ut.0.is_constructor() || !buildable_units(ut.0).is_empty())
        else {
            continue;
        };
        if ut.0.is_constructor() {
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

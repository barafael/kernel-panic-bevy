use bevy::prelude::*;

use crate::interaction::Selected;
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::definitions::{self, UnitKind, UnitStats};
use crate::units::game_over::PlayerTeam;
use crate::units::production::Producer;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BuildOrderEvent>()
            .add_message::<UnitOrderEvent>()
            .init_resource::<UnitPreviews>()
            .add_systems(Startup, generate_unit_previews)
            .add_systems(
                Update,
                (
                    update_info_panel,
                    update_build_menu,
                    update_order_palette,
                    handle_build_clicks,
                    handle_order_clicks,
                    apply_build_orders,
                    apply_unit_orders,
                ),
            );
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Fired when the player clicks a build icon.
#[derive(Message)]
struct BuildOrderEvent {
    kind: UnitKind,
}

/// Fired when the player clicks an order button or presses a hotkey.
#[derive(Message)]
struct UnitOrderEvent {
    order: UnitOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitOrder {
    Stop,
    AttackMove,
}

// ---------------------------------------------------------------------------
// Marker components for UI nodes
// ---------------------------------------------------------------------------

#[derive(Component)]
struct InfoPanel;

#[derive(Component)]
struct BuildMenu;

#[derive(Component)]
struct OrderPalette;

/// Attached to a build icon button. Carries the unit kind to build.
#[derive(Component)]
struct BuildIcon(UnitKind);

/// Attached to an order button. Carries the order type.
#[derive(Component)]
struct OrderButton(UnitOrder);

/// Build progress bar foreground node.
#[derive(Component)]
struct BuildProgressBar;

/// Build queue text node.
#[derive(Component)]
struct BuildQueueText;

// ---------------------------------------------------------------------------
// Unit preview textures
// ---------------------------------------------------------------------------

/// Cached preview images for each unit kind, generated at startup.
#[derive(Resource, Default)]
struct UnitPreviews {
    images: Vec<(UnitKind, Handle<Image>)>,
}

impl UnitPreviews {
    fn get(&self, kind: UnitKind) -> Option<&Handle<Image>> {
        self.images.iter().find(|(k, _)| *k == kind).map(|(_, h)| h)
    }
}

/// Generate simple procedural preview images for each unit kind.
/// These are small colored squares with the faction color, since we don't have
/// a way to render 3D models to texture without a second camera pipeline.
fn generate_unit_previews(mut previews: ResMut<UnitPreviews>, mut images: ResMut<Assets<Image>>) {
    let all_kinds = [
        // System
        UnitKind::Kernel,
        UnitKind::Assembler,
        UnitKind::Bit,
        UnitKind::Byte,
        UnitKind::Pointer,
        UnitKind::Socket,
        UnitKind::Firewall,
        // Hacker
        UnitKind::Hole,
        UnitKind::Bug,
        UnitKind::Exploit,
        UnitKind::Worm,
        UnitKind::Virus,
        UnitKind::Dos,
        UnitKind::Window,
        UnitKind::LogicBomb,
        // Network
        UnitKind::Connection,
        UnitKind::Port,
        UnitKind::Packet,
        UnitKind::Signal,
    ];

    for kind in all_kinds {
        let faction_color = faction_for_kind(kind).color();
        let image = generate_preview_image(kind, faction_color);
        let handle = images.add(image);
        previews.images.push((kind, handle));
    }
}

/// Create a 48x48 RGBA preview image for a unit kind.
fn generate_preview_image(kind: UnitKind, faction_color: Color) -> Image {
    const SIZE: u32 = 48;

    let srgba = Srgba::from(faction_color);
    let r = (srgba.red * 255.0) as u8;
    let g = (srgba.green * 255.0) as u8;
    let b = (srgba.blue * 255.0) as u8;

    let mut pixels = vec![0u8; (SIZE * SIZE * 4) as usize];

    let stats = definitions::stats(kind);

    // Draw a shape based on the unit type: circle for mobile, diamond for buildings.
    let center = SIZE as f32 / 2.0;
    let radius = center - 4.0;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let idx = ((y * SIZE + x) * 4) as usize;

            let inside = if stats.is_building {
                // Diamond shape for buildings.
                dx.abs() + dy.abs() < radius
            } else {
                // Circle for mobile units.
                dx * dx + dy * dy < radius * radius
            };

            if inside {
                // Brighter center, darker edges.
                let dist = if stats.is_building {
                    (dx.abs() + dy.abs()) / radius
                } else {
                    (dx * dx + dy * dy).sqrt() / radius
                };
                let brightness = 1.0 - dist * 0.6;
                pixels[idx] = (r as f32 * brightness) as u8;
                pixels[idx + 1] = (g as f32 * brightness) as u8;
                pixels[idx + 2] = (b as f32 * brightness) as u8;
                pixels[idx + 3] = 220;
            } else {
                pixels[idx + 3] = 0;
            }
        }
    }

    // Draw a 1px border in brighter faction color.
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let idx = ((y * SIZE + x) * 4) as usize;

            let on_border = if stats.is_building {
                let d = dx.abs() + dy.abs();
                d >= radius - 1.5 && d < radius + 0.5
            } else {
                let d = (dx * dx + dy * dy).sqrt();
                d >= radius - 1.5 && d < radius + 0.5
            };

            if on_border {
                pixels[idx] = r.saturating_add(40);
                pixels[idx + 1] = g.saturating_add(40);
                pixels[idx + 2] = b.saturating_add(40);
                pixels[idx + 3] = 255;
            }
        }
    }

    Image::new(
        bevy::render::render_resource::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        bevy::render::render_resource::TextureDimension::D2,
        pixels,
        bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
        bevy::asset::RenderAssetUsages::RENDER_WORLD | bevy::asset::RenderAssetUsages::MAIN_WORLD,
    )
}

fn faction_for_kind(kind: UnitKind) -> Faction {
    match kind {
        UnitKind::Kernel
        | UnitKind::Assembler
        | UnitKind::Bit
        | UnitKind::Byte
        | UnitKind::Pointer
        | UnitKind::Socket
        | UnitKind::Firewall => Faction::System,

        UnitKind::Hole
        | UnitKind::Bug
        | UnitKind::Exploit
        | UnitKind::Worm
        | UnitKind::Virus
        | UnitKind::Dos
        | UnitKind::Window
        | UnitKind::LogicBomb => Faction::Hacker,

        UnitKind::Connection | UnitKind::Port | UnitKind::Packet | UnitKind::Signal => {
            Faction::Network
        }
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const UI_BORDER_COLOR: Color = Color::linear_rgb(0.0, 0.7, 0.2);
const UI_BG_COLOR: Color = Color::srgba(0.0, 0.05, 0.0, 0.75);
const UI_TEXT_COLOR: Color = Color::linear_rgb(0.0, 1.0, 0.3);
const UI_TEXT_DIM: Color = Color::linear_rgb(0.0, 0.5, 0.15);
const UI_HEALTH_GREEN: Color = Color::linear_rgb(0.0, 1.0, 0.2);
const UI_HEALTH_YELLOW: Color = Color::linear_rgb(1.0, 1.0, 0.0);
const UI_HEALTH_RED: Color = Color::linear_rgb(1.0, 0.0, 0.0);
const UI_PROGRESS_COLOR: Color = Color::linear_rgb(0.0, 0.8, 0.3);

const FONT_SIZE_TITLE: f32 = 18.0;
const FONT_SIZE_BODY: f32 = 14.0;
const FONT_SIZE_SMALL: f32 = 12.0;

const BUILD_ICON_SIZE: f32 = 64.0;

// ---------------------------------------------------------------------------
// Unit info panel (bottom-left)
// ---------------------------------------------------------------------------

fn update_info_panel(
    selected_q: Query<(&UnitType, &Health, &Faction), With<Selected>>,
    existing: Query<Entity, With<InfoPanel>>,
    mut commands: Commands,
) {
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    let selected: Vec<_> = selected_q.iter().collect();
    if selected.is_empty() {
        return;
    }

    let panel = commands
        .spawn((
            InfoPanel,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                bottom: Val::Px(220.0),
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

    // Weapon info
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

    // Speed
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
// Build menu (left side)
// ---------------------------------------------------------------------------

fn buildable_units(kind: UnitKind) -> &'static [UnitKind] {
    match kind {
        UnitKind::Kernel => &[
            UnitKind::Bit,
            UnitKind::Assembler,
            UnitKind::Byte,
            UnitKind::Pointer,
            UnitKind::Firewall,
        ],
        UnitKind::Socket => &[UnitKind::Bit],
        UnitKind::Hole => &[
            UnitKind::Bug,
            UnitKind::Worm,
            UnitKind::Dos,
            UnitKind::LogicBomb,
        ],
        UnitKind::Window => &[UnitKind::Bug],
        UnitKind::Connection => &[UnitKind::Packet, UnitKind::Signal],
        UnitKind::Port => &[UnitKind::Packet],
        UnitKind::Assembler => &[UnitKind::Socket, UnitKind::Firewall],
        _ => &[],
    }
}

fn update_build_menu(
    selected_q: Query<(&UnitType, &Faction), With<Selected>>,
    producer_q: Query<(&Producer, &UnitType), With<Selected>>,
    existing: Query<Entity, With<BuildMenu>>,
    previews: Res<UnitPreviews>,
    mut commands: Commands,
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

    // Build progress (if this unit is a producer)
    if let Some((producer, _)) = producer_q.iter().next() {
        let producing_stats = definitions::stats(producer.current_production());
        let progress = producer.progress_fraction();

        // "Building: Bit (75%)"
        let progress_text = format!(
            "Building: {} ({:.0}%)",
            producing_stats.name,
            progress * 100.0
        );
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

        // Progress bar
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
                BuildProgressBar,
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

        // Queue display
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
                        let name = definitions::stats(pk).name;
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
                let name = definitions::stats(pk).name;
                if count > 1 {
                    queue_parts.push(format!("{count}x {name}"));
                } else {
                    queue_parts.push(name.to_string());
                }
            }

            let queue_str = format!("Queue: {}", queue_parts.join(", "));
            let queue_node = commands
                .spawn((
                    BuildQueueText,
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
        let stats = definitions::stats(*kind);
        let preview = previews.get(*kind).cloned();
        let icon = spawn_build_icon(&mut commands, stats, faction, *kind, preview);
        commands.entity(grid).add_child(icon);
    }
}

fn spawn_build_icon(
    commands: &mut Commands,
    stats: &UnitStats,
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

    icon
}

// ---------------------------------------------------------------------------
// Build click handling
// ---------------------------------------------------------------------------

fn handle_build_clicks(
    interaction_q: Query<(&Interaction, &BuildIcon), Changed<Interaction>>,
    mut ev_build: MessageWriter<BuildOrderEvent>,
) {
    for (interaction, build_icon) in &interaction_q {
        if *interaction == Interaction::Pressed {
            ev_build.write(BuildOrderEvent { kind: build_icon.0 });
        }
    }
}

fn apply_build_orders(
    mut ev_build: MessageReader<BuildOrderEvent>,
    mut producers: Query<(&mut Producer, &TeamId), With<Selected>>,
    player_team: Res<PlayerTeam>,
) {
    for event in ev_build.read() {
        // Enqueue on the first selected producer that belongs to the player.
        for (mut producer, team) in &mut producers {
            if team.0 == player_team.0 {
                producer.enqueue(event.kind);
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Order palette (bottom-right)
// ---------------------------------------------------------------------------

const ORDERS: &[(&str, &str, UnitOrder)] = &[
    ("Stop", "S", UnitOrder::Stop),
    ("Attack", "A", UnitOrder::AttackMove),
];

fn update_order_palette(
    selected_q: Query<&UnitType, With<Selected>>,
    existing: Query<Entity, With<OrderPalette>>,
    mut commands: Commands,
) {
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    if selected_q.is_empty() {
        return;
    }

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

    for (name, hotkey, order) in ORDERS {
        let btn = spawn_order_button(&mut commands, name, hotkey, *order);
        commands.entity(grid).add_child(btn);
    }
}

fn spawn_order_button(
    commands: &mut Commands,
    name: &str,
    hotkey: &str,
    order: UnitOrder,
) -> Entity {
    let btn = commands
        .spawn((
            OrderButton(order),
            Button,
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

// ---------------------------------------------------------------------------
// Order click + hotkey handling
// ---------------------------------------------------------------------------

fn handle_order_clicks(
    interaction_q: Query<(&Interaction, &OrderButton), Changed<Interaction>>,
    keys: Res<ButtonInput<KeyCode>>,
    mut ev_order: MessageWriter<UnitOrderEvent>,
) {
    // Button clicks
    for (interaction, order_btn) in &interaction_q {
        if *interaction == Interaction::Pressed {
            ev_order.write(UnitOrderEvent { order: order_btn.0 });
        }
    }

    // Keyboard hotkeys
    if keys.just_pressed(KeyCode::KeyS) {
        ev_order.write(UnitOrderEvent {
            order: UnitOrder::Stop,
        });
    }
    if keys.just_pressed(KeyCode::KeyA) {
        ev_order.write(UnitOrderEvent {
            order: UnitOrder::AttackMove,
        });
    }
}

fn apply_unit_orders(
    mut ev_order: MessageReader<UnitOrderEvent>,
    selected_q: Query<Entity, With<Selected>>,
    mut commands: Commands,
) {
    use crate::interaction::movement::{MovePath, MoveTarget};

    for event in ev_order.read() {
        match event.order {
            UnitOrder::Stop => {
                for entity in &selected_q {
                    commands
                        .entity(entity)
                        .remove::<MoveTarget>()
                        .remove::<MovePath>();
                }
            }
            UnitOrder::AttackMove => {
                // TODO: implement attack-move cursor mode.
                // For now this just acts as a visual indicator that the
                // command was received.
            }
        }
    }
}

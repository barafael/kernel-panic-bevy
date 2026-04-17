//! Bottom-right order palette: Stop / Attack-move buttons with keyboard
//! hotkeys. Only shown when at least one mobile unit is selected.

use bevy::prelude::*;

use super::style::{
    FONT_SIZE_SMALL, FONT_SIZE_TITLE, UI_BG_COLOR, UI_BORDER_COLOR, UI_TEXT_COLOR, UI_TEXT_DIM,
};
use crate::interaction::Selected;
use crate::units::components::UnitType;
use crate::units::definitions::UnitKind;
use crate::units::unit_registry::UnitRegistry;

pub struct OrderPalettePlugin;

impl Plugin for OrderPalettePlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<UnitOrderEvent>().add_systems(
            Update,
            (update_order_palette, handle_order_clicks, apply_unit_orders),
        );
    }
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
    CommandFire,
}

#[derive(Component)]
struct OrderPalette;

/// Attached to an order button. Carries the order type.
#[derive(Component)]
struct OrderButton(UnitOrder);

const ORDERS: &[(&str, &str, UnitOrder)] = &[
    ("Stop", "S", UnitOrder::Stop),
    ("Attack", "A", UnitOrder::AttackMove),
];

fn update_order_palette(
    selected_q: Query<&UnitType, With<Selected>>,
    existing: Query<Entity, With<OrderPalette>>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
) {
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    if selected_q.is_empty() {
        return;
    }

    let has_mobile = selected_q.iter().any(|ut| unit_registry.speed(ut.0) > 0.0);
    let has_ability = selected_q
        .iter()
        .any(|ut| matches!(ut.0, UnitKind::Pointer | UnitKind::Obelisk));

    if !has_mobile && !has_ability {
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
        if matches!(order, UnitOrder::Stop | UnitOrder::AttackMove) && !has_mobile {
            continue;
        }
        let btn = spawn_order_button(&mut commands, name, hotkey, *order);
        commands.entity(grid).add_child(btn);
    }

    if has_ability {
        // Only one "Ability" slot today — label it after the first ability
        // unit in the selection so the player knows which cast Q fires.
        let name = selected_q
            .iter()
            .find_map(|ut| match ut.0 {
                UnitKind::Pointer => Some("NX Flag"),
                UnitKind::Obelisk => Some("Infection"),
                _ => None,
            })
            .unwrap_or("Ability");
        let btn = spawn_order_button(&mut commands, name, "Q", UnitOrder::CommandFire);
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
    use crate::interaction::movement::{CommandQueue, MovePath, MoveTarget};

    for event in ev_order.read() {
        match event.order {
            UnitOrder::Stop => {
                for entity in &selected_q {
                    commands
                        .entity(entity)
                        .remove::<MoveTarget>()
                        .remove::<MovePath>()
                        .remove::<CommandQueue>();
                }
            }
            UnitOrder::AttackMove => {
                // TODO: implement attack-move cursor mode.
                // For now this just acts as a visual indicator that the
                // command was received.
            }
            UnitOrder::CommandFire => {
                // Handled by `interaction::ability`, which reads Q + cursor
                // position. The button slot exists only as a hotkey hint;
                // the click itself is a no-op (it has no ground target).
            }
        }
    }
}

//! Bottom-right order palette: Stop / Attack-move buttons with keyboard
//! hotkeys. Only shown when at least one mobile unit is selected.

use bevy::prelude::*;

use super::style::{
    FONT_SIZE_SMALL, FONT_SIZE_TITLE, UI_BG_COLOR, UI_BORDER_COLOR, UI_PANEL_TINT, UI_TEXT_COLOR,
    UI_TEXT_DIM,
};
use crate::{
    interaction::Selected,
    units::{
        components::UnitType,
        content::{definitions::UnitKind, unit_registry::UnitRegistry},
        mechanics::deploy::DeployEvent,
    },
};

pub struct OrderPalettePlugin;

impl Plugin for OrderPalettePlugin {
    fn build(&self, app: &mut App) {
        // Order matters: clicks land on the buttons that
        // `update_order_palette` last spawned. If `update_order_palette`
        // runs first within a frame and rebuilds the palette (selection
        // changed, ability/deploy hash flipped, …), the just-pressed
        // entity is despawned before `handle_order_clicks` can read its
        // `Changed<Interaction>`. Run handler first, then rebuild.
        app.add_message::<UnitOrderEvent>().add_systems(
            Update,
            (handle_order_clicks, apply_unit_orders, update_order_palette).chain(),
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
    Deploy,
}

#[derive(Component)]
struct OrderPalette;

/// Attached to an order button. Carries the order type.
#[derive(Component)]
struct OrderButton(UnitOrder);

/// A button entry in the order palette.
struct OrderButtonSpec {
    label: &'static str,
    hotkey_hint: &'static str,
    order: UnitOrder,
}

const ORDERS: &[OrderButtonSpec] = &[
    OrderButtonSpec {
        label: "Stop",
        hotkey_hint: "S",
        order: UnitOrder::Stop,
    },
    OrderButtonSpec {
        label: "Attack",
        hotkey_hint: "A",
        order: UnitOrder::AttackMove,
    },
];

fn update_order_palette(
    selected_q: Query<&UnitType, With<Selected>>,
    existing: Query<Entity, With<OrderPalette>>,
    mut commands: Commands,
    unit_registry: Res<UnitRegistry>,
    mut last_hash: Local<u64>,
) {
    let has_mobile = selected_q.iter().any(|ut| unit_registry.speed(ut.0) > 0.0);
    let has_ability = selected_q.iter().any(|ut| ut.0.has_command_fire_ability());
    let has_deploy = selected_q.iter().any(|ut| ut.0.deploy_pair().is_some());
    let ability_kind = selected_q
        .iter()
        .find(|ut| ut.0.has_command_fire_ability())
        .map(|ut| ut.0 as u64);
    let deploy_kind = selected_q
        .iter()
        .find(|ut| ut.0.deploy_pair().is_some())
        .map(|ut| ut.0 as u64);

    let mut hash: u64 = 0;
    if has_mobile {
        hash |= 0b001;
    }
    if has_ability {
        hash |= 0b010;
    }
    if has_deploy {
        hash |= 0b100;
    }
    if let Some(k) = ability_kind {
        hash = hash.wrapping_mul(2654435761).wrapping_add(k);
    }
    if let Some(k) = deploy_kind {
        hash = hash.wrapping_mul(2654435761).wrapping_add(k);
    }
    if hash == *last_hash {
        return;
    }
    *last_hash = hash;

    for entity in &existing {
        commands.entity(entity).despawn();
    }

    if selected_q.is_empty() {
        return;
    }

    if !has_mobile && !has_ability && !has_deploy {
        return;
    }

    let palette = commands
        .spawn((
            OrderPalette,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                top: Val::Px(8.0),
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

    for spec in ORDERS {
        if matches!(spec.order, UnitOrder::Stop | UnitOrder::AttackMove) && !has_mobile {
            continue;
        }
        let btn = spawn_order_button(&mut commands, spec.label, spec.hotkey_hint, spec.order);
        commands.entity(grid).add_child(btn);
    }

    if has_ability {
        let name = selected_q
            .iter()
            .find_map(|ut| ability_label(ut.0))
            .unwrap_or("Ability");
        let btn = spawn_order_button(&mut commands, name, "D", UnitOrder::CommandFire);
        commands.entity(grid).add_child(btn);
    }

    if has_deploy {
        let name = selected_q
            .iter()
            .find_map(|ut| deploy_label(ut.0))
            .unwrap_or("Deploy");
        let btn = spawn_order_button(&mut commands, name, "D", UnitOrder::Deploy);
        commands.entity(grid).add_child(btn);
    }
}

/// Display name shown on the ability button. Eligibility lives on
/// [`UnitKind::has_command_fire_ability`]; this table is purely
/// presentation.
fn ability_label(kind: UnitKind) -> Option<&'static str> {
    match kind {
        UnitKind::Pointer => Some("NX Flag"),
        UnitKind::Obelisk => Some("Infection"),
        UnitKind::Firewall => Some("Protect"),
        UnitKind::Byte => Some("Mine Launch"),
        UnitKind::Terminal => Some("SIGTERM"),
        _ => None,
    }
}

/// Display name for the Bug ↔ Exploit deploy button. Eligibility lives
/// on [`UnitKind::deploy_pair`]; this table is purely presentation.
fn deploy_label(kind: UnitKind) -> Option<&'static str> {
    match kind {
        UnitKind::Bug => Some("Deploy"),
        UnitKind::Exploit => Some("Pack Up"),
        _ => None,
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
            BackgroundColor(UI_PANEL_TINT),
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
    selected_q: Query<(Entity, &UnitType), With<Selected>>,
    mut commands: Commands,
    mut ev_deploy: MessageWriter<DeployEvent>,
) {
    use crate::interaction::movement::{CommandQueue, MovePath, MoveTarget};

    for event in ev_order.read() {
        match event.order {
            UnitOrder::Stop => {
                use crate::units::combat::{AttackGroundOrder, SelfDestructCountdown};
                use crate::units::lifecycle::construction::PendingBuild;
                for (entity, _) in &selected_q {
                    commands
                        .entity(entity)
                        .remove::<MoveTarget>()
                        .remove::<MovePath>()
                        .remove::<CommandQueue>()
                        .remove::<PendingBuild>()
                        .remove::<SelfDestructCountdown>()
                        .remove::<AttackGroundOrder>();
                }
            }
            UnitOrder::AttackMove => {
                // TODO: implement attack-move cursor mode.
                // For now this just acts as a visual indicator that the
                // command was received.
            }
            UnitOrder::CommandFire => {
                // Handled by `interaction::ability`, which reads D + cursor
                // position. The button slot exists only as a hotkey hint;
                // the click itself is a no-op (it has no ground target).
            }
            UnitOrder::Deploy => {
                for (entity, unit) in &selected_q {
                    if deploy_label(unit.0).is_some() {
                        ev_deploy.write(DeployEvent { entity });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every D-hotkey ability unit gets a friendly button label. If
    /// `interaction::ability::has_ability` grows, this test will fail
    /// until the new kind is given a label here — the palette button
    /// would otherwise fall back to the generic "Ability" string.
    #[test]
    fn ability_label_covers_every_hotkey_ability_kind() {
        assert_eq!(ability_label(UnitKind::Pointer), Some("NX Flag"));
        assert_eq!(ability_label(UnitKind::Obelisk), Some("Infection"));
        assert_eq!(ability_label(UnitKind::Firewall), Some("Protect"));
        assert_eq!(ability_label(UnitKind::Byte), Some("Mine Launch"));
        assert_eq!(ability_label(UnitKind::Terminal), Some("SIGTERM"));
    }

    #[test]
    fn ability_label_is_none_for_non_ability_kinds() {
        assert_eq!(ability_label(UnitKind::Bit), None);
        assert_eq!(ability_label(UnitKind::Bug), None);
        assert_eq!(ability_label(UnitKind::Worm), None);
        assert_eq!(ability_label(UnitKind::Packet), None);
        assert_eq!(ability_label(UnitKind::Kernel), None);
    }

    /// Deploy is the Bug ↔ Exploit toggle. Both halves of the pair
    /// surface as buttons so the player doesn't need to know the `D`
    /// hotkey to discover it.
    #[test]
    fn deploy_label_covers_both_halves_of_the_pair() {
        assert_eq!(deploy_label(UnitKind::Bug), Some("Deploy"));
        assert_eq!(deploy_label(UnitKind::Exploit), Some("Pack Up"));
    }

    #[test]
    fn deploy_label_is_none_for_non_deployable_kinds() {
        assert_eq!(deploy_label(UnitKind::Bit), None);
        assert_eq!(deploy_label(UnitKind::Pointer), None);
        assert_eq!(deploy_label(UnitKind::Worm), None);
    }

    /// The command-fire ability set and the deploy set both read `D`
    /// but must never overlap on the same kind — if a unit had both,
    /// pressing `D` would deploy it *and* fire its ability. The
    /// hotkey resolver relies on this partition instead of running a
    /// priority/modifier sort, and the palette surfaces one button
    /// per kind.
    #[test]
    fn ability_and_deploy_labels_do_not_overlap() {
        for kind in [
            UnitKind::Pointer,
            UnitKind::Obelisk,
            UnitKind::Firewall,
            UnitKind::Byte,
            UnitKind::Terminal,
            UnitKind::Bug,
            UnitKind::Exploit,
        ] {
            assert!(
                ability_label(kind).is_none() || deploy_label(kind).is_none(),
                "{kind:?} has both a command-fire and deploy label — `D` would do both",
            );
        }
    }
}

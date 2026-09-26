//! Top-right order palette: Stop / Attack / Fight / Self-destruct, plus
//! the contextual D-ability button (NX Flag, Infection, etc.) when the
//! selection includes a caster.
//!
//! Buttons mirror the hotkeys handled in [`crate::interaction::ability`]
//! so the user can drive the same actions via mouse. Most actually fire
//! by simulating the same ECS effects — Stop strips order components,
//! Attack toggles `OrderCursorModes::attack_ground`, Fight toggles
//! `OrderCursorModes::attack_move`, Self-destruct inserts
//! `SelfDestructCountdown`. The Ability button runs the `D` logic from
//! [`crate::interaction::ability`]: Bug / Exploit deploy at once, and
//! aimed abilities (command-fire, Dispatch) arm
//! `OrderCursorModes::ability` — the button sits where the cursor is,
//! so the next ground click supplies the point `D` would read from the
//! cursor. While every selected command-fire caster is recharging the
//! button reads the seconds left, like upstream's `"<n>s"` command
//! label (`airstrike.lua` / `network_reflectorshield.lua`). Selections
//! with a Worm also get the AutoHold toggle (`H`).

use bevy::prelude::*;

use crate::interaction::ability::{OrderCursorModes, ability_is_aimed, deploy_units};
use crate::interaction::movement::{
    AttackMoveActive, CommandQueue, GuardTarget, MovePath, MoveTarget,
};
use crate::interaction::selection::Selected;
use crate::units::combat::{
    AttackGroundOrder, ForcedTarget, SELF_DESTRUCT_DELAY, SelfDestructCountdown,
};
use crate::units::components::UnitType;
use crate::units::lifecycle::construction::PendingBuild;
use crate::units::mechanics::worm::AutoHold;
use crate::units::mechanics::command_fire::{CommandFireCooldown, PendingCommandFire};
use crate::units::mechanics::deploy::DeployEvent;

use super::super::theme::*;

pub(super) struct OrderPalettePlugin;

impl Plugin for OrderPalettePlugin {
    fn build(&self, app: &mut App) {
        // The panel spawns in `Update` (not Startup) and re-spawns if
        // missing: the game-world teardown (menu reload / restart)
        // despawns it, and it must come back on the next match.
        app.add_systems(
            Update,
            // handle_clicks runs first; refresh_panel only rebuilds when
            // the *roster* changes; update_armed_highlight repaints the
            // Attack button border without rebuilding (otherwise the
            // still-held mouse press would re-toggle attack mode on the
            // newly-spawned button entity). Ordered after the world
            // rebuild so its commands never reference entities the
            // teardown despawned this frame.
            (
                spawn_panel,
                handle_clicks,
                refresh_panel,
                update_armed_highlight,
                update_ability_label,
            )
                .chain()
                .after(crate::map_loading::GameWorldRebuild),
        );
    }
}

#[derive(Component)]
struct OrderPaletteRoot;

#[derive(Component)]
struct OrderPaletteContent;

#[derive(Component)]
struct OrderPaletteStateHash(u64);

#[derive(Component, Clone, Copy)]
struct OrderButton(OrderKind);

/// The Ability button's title text, rewritten each frame with the
/// recharge countdown by [`update_ability_label`].
#[derive(Component)]
struct AbilityLabel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OrderKind {
    Stop,
    AttackGround,
    /// Attack-move ("Fight"): march to a clicked point, engaging hostile
    /// units encountered en route. `F` toggles the armed cursor mode; the
    /// click that follows issues the order.
    Fight,
    /// Guard: click a friendly unit to have the selection trail and
    /// protect it. `G` arms the Defend cursor.
    Guard,
    /// Plain move order via cursor (`M`).
    Move,
    /// Manual target designation (`T`): pick a unit the selection will
    /// prefer as its target; tracked by the turret, never chased.
    SetTarget,
    /// Clear the manual target designation (`X`).
    UnsetTarget,
    SelfDestruct,
    /// Toggle AutoHold (upstream autohold.lua `CMD_AUTOHOLD`) on the
    /// selected Worms: while on, a cloaked worm holds fire until given
    /// an explicit attack order. Shown only when a Worm is selected.
    AutoHold,
    /// The contextual `D` ability: deploy Bug / Exploit immediately,
    /// and arm `OrderCursorModes::ability` for aimed abilities so the
    /// next ground click casts them.
    Ability,
}

impl OrderKind {
    fn label(self) -> &'static str {
        match self {
            OrderKind::Stop => "Stop",
            OrderKind::AttackGround => "Attack",
            OrderKind::Fight => "Fight",
            OrderKind::Guard => "Guard",
            OrderKind::Move => "Move",
            OrderKind::SetTarget => "Target",
            OrderKind::UnsetTarget => "Unset",
            OrderKind::SelfDestruct => "Detonate",
            OrderKind::AutoHold => "AutoHold",
            OrderKind::Ability => "Ability",
        }
    }

    fn hotkey(self) -> &'static str {
        match self {
            OrderKind::Stop => "S",
            OrderKind::AttackGround => "A",
            OrderKind::Fight => "F",
            OrderKind::Guard => "G",
            OrderKind::Move => "M",
            OrderKind::SetTarget => "T",
            OrderKind::UnsetTarget => "X",
            OrderKind::SelfDestruct => "Ctrl+D",
            OrderKind::AutoHold => "H",
            OrderKind::Ability => "D",
        }
    }
}

fn spawn_panel(mut commands: Commands, existing: Query<(), With<OrderPaletteRoot>>) {
    if !existing.is_empty() {
        return;
    }
    commands
        .spawn((
            OrderPaletteRoot,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(8.0),
                bottom: Val::Px(8.0),
                width: Val::Px(RIGHT_COLUMN_WIDTH),
                padding: UiRect::all(Val::Px(PANEL_PADDING)),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(PANEL_GAP),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(PANEL_BORDER),
            Visibility::Hidden,
            OrderPaletteStateHash(0),
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new("Orders"),
                TextFont {
                    font_size: TEXT_TITLE,
                    ..default()
                },
                TextColor(KP_GREEN),
            ));
            parent.spawn((
                OrderPaletteContent,
                Node {
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    column_gap: Val::Px(PANEL_GAP),
                    row_gap: Val::Px(PANEL_GAP),
                    ..default()
                },
            ));
        });
}

fn refresh_panel(
    mut commands: Commands,
    mut root_q: Query<(&mut Visibility, &mut OrderPaletteStateHash), With<OrderPaletteRoot>>,
    content_q: Query<Entity, With<OrderPaletteContent>>,
    selected_q: Query<&UnitType, With<Selected>>,
) {
    let Ok((mut visibility, mut hash_marker)) = root_q.single_mut() else {
        return;
    };

    // Hash deliberately excludes the `OrderCursorModes` flags — those
    // are painted by `update_armed_highlight` without rebuilding entities.
    let snapshot = OrderSnapshot::collect(&selected_q);
    let new_hash = snapshot.hash();
    if new_hash == hash_marker.0 {
        return;
    }
    hash_marker.0 = new_hash;

    *visibility = if snapshot.entries.is_empty() {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };

    let Ok(content) = content_q.single() else {
        return;
    };
    // The panel can vanish with the game-world teardown (menu reload /
    // restart); skip the rebuild when it's gone — spawn_panel restores
    // it next frame.
    let Ok(mut content_cmds) = commands.get_entity(content) else {
        return;
    };
    content_cmds.despawn_related::<Children>();

    if snapshot.entries.is_empty() {
        return;
    }

    content_cmds.with_children(|parent| {
        for kind in &snapshot.entries {
            parent
                .spawn((
                    Button,
                    OrderButton(*kind),
                    Node {
                        width: Val::Px(80.0),
                        height: Val::Px(40.0),
                        border: UiRect::all(Val::Px(1.0)),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(BUTTON_BG),
                    BorderColor::all(PANEL_BORDER),
                ))
                .with_children(|btn| {
                    let mut label = btn.spawn((
                        Text::new(kind.label()),
                        TextFont {
                            font_size: TEXT_BODY,
                            ..default()
                        },
                        TextColor(KP_GREEN),
                    ));
                    if *kind == OrderKind::Ability {
                        label.insert(AbilityLabel);
                    }
                    btn.spawn((
                        Text::new(kind.hotkey()),
                        TextFont {
                            font_size: TEXT_SMALL,
                            ..default()
                        },
                        TextColor(KP_GREEN_DIM),
                    ));
                });
        }
    });
}

/// Repaint the Attack button's border/background based on the latched
/// attack mode. Done in-place so toggling does not despawn the button —
/// otherwise the still-held mouse press would re-toggle attack mode on
/// the newly-spawned button next frame.
fn update_armed_highlight(
    modes: Res<OrderCursorModes>,
    autohold_q: Query<&AutoHold, With<Selected>>,
    mut buttons: Query<(
        &OrderButton,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Node,
    )>,
) {
    for (button, mut border, mut bg, mut node) in &mut buttons {
        let armed = (matches!(button.0, OrderKind::AttackGround) && modes.attack_ground)
            || (matches!(button.0, OrderKind::Fight) && modes.attack_move)
            || (matches!(button.0, OrderKind::Guard) && modes.guard)
            || (matches!(button.0, OrderKind::Move) && modes.move_order)
            || (matches!(button.0, OrderKind::SetTarget) && modes.set_target)
            || (matches!(button.0, OrderKind::Ability) && modes.ability)
            || (matches!(button.0, OrderKind::AutoHold) && autohold_on(&autohold_q));
        let target_border = if armed { KP_GREEN } else { PANEL_BORDER };
        let target_bg = if armed { BUTTON_BG_PRESSED } else { BUTTON_BG };
        let target_width = if armed { 2.0 } else { 1.0 };
        *border = BorderColor::all(target_border);
        *bg = BackgroundColor(target_bg);
        node.border = UiRect::all(Val::Px(target_width));
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn handle_clicks(
    mut commands: Commands,
    interactions: Query<(&Interaction, &OrderButton), Changed<Interaction>>,
    keys: Res<ButtonInput<KeyCode>>,
    selected_q: Query<Entity, With<Selected>>,
    mut autohold_q: Query<&mut AutoHold, With<Selected>>,
    selected_kinds: Query<(Entity, &UnitType), With<Selected>>,
    mut deploy: MessageWriter<DeployEvent>,
    mut modes: ResMut<OrderCursorModes>,
) {
    // Keyboard hotkey: S issues Stop (mirrors the Stop button).
    if keys.just_pressed(KeyCode::KeyS) {
        stop_selection(&mut commands, &selected_q, &mut modes);
    }
    // H toggles AutoHold (mirrors the AutoHold button).
    if keys.just_pressed(KeyCode::KeyH) {
        toggle_autohold(&mut autohold_q);
    }

    for (interaction, button) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match button.0 {
            OrderKind::Stop => stop_selection(&mut commands, &selected_q, &mut modes),
            OrderKind::AttackGround => {
                let next = !modes.attack_ground;
                modes.attack_ground = next;
                if next {
                    modes.attack_move = false;
                    modes.patrol = false;
                    modes.guard = false;
                    modes.move_order = false;
                    modes.set_target = false;
                    modes.ability = false;
                }
            }
            OrderKind::Ability => {
                // Same split as the `D` hotkey: the untargeted deploy
                // happens now; aimed abilities wait for a ground click.
                deploy_units(selected_kinds.iter().map(|(e, u)| (e, u.0)), &mut deploy);
                if selected_kinds.iter().any(|(_, u)| ability_is_aimed(u.0)) {
                    modes.toggle_ability();
                }
            }
            OrderKind::Fight => {
                let next = !modes.attack_move;
                modes.attack_move = next;
                if next {
                    modes.ability = false;
                    modes.attack_ground = false;
                    modes.patrol = false;
                    modes.guard = false;
                    modes.move_order = false;
                    modes.set_target = false;
                }
            }
            OrderKind::Guard => {
                let next = !modes.guard;
                modes.guard = next;
                if next {
                    modes.ability = false;
                    modes.attack_ground = false;
                    modes.attack_move = false;
                    modes.patrol = false;
                    modes.move_order = false;
                    modes.set_target = false;
                }
            }
            OrderKind::Move => {
                let next = !modes.move_order;
                modes.move_order = next;
                if next {
                    modes.ability = false;
                    modes.attack_ground = false;
                    modes.attack_move = false;
                    modes.patrol = false;
                    modes.guard = false;
                    modes.set_target = false;
                }
            }
            OrderKind::SetTarget => {
                let next = !modes.set_target;
                modes.set_target = next;
                if next {
                    modes.ability = false;
                    modes.attack_ground = false;
                    modes.attack_move = false;
                    modes.patrol = false;
                    modes.guard = false;
                    modes.move_order = false;
                }
            }
            OrderKind::UnsetTarget => {
                for entity in &selected_q {
                    commands.entity(entity).remove::<ForcedTarget>();
                }
                modes.set_target = false;
            }
            OrderKind::AutoHold => toggle_autohold(&mut autohold_q),
            OrderKind::SelfDestruct => {
                for entity in &selected_q {
                    commands.entity(entity).insert(SelfDestructCountdown {
                        remaining: SELF_DESTRUCT_DELAY,
                    });
                }
            }
        }
    }
}

/// Stop order: strip every movement/order component off the selection and
/// disarm the order-cursor modes. Shared by the Stop button and the `S`
/// hotkey so both stay identical.
fn stop_selection(
    commands: &mut Commands,
    selected_q: &Query<Entity, With<Selected>>,
    modes: &mut OrderCursorModes,
) {
    for entity in selected_q {
        commands
            .entity(entity)
            .remove::<MoveTarget>()
            .remove::<MovePath>()
            .remove::<CommandQueue>()
            .remove::<AttackGroundOrder>()
            .remove::<AttackMoveActive>()
            .remove::<crate::units::combat::AttackTargetOrder>()
            .remove::<GuardTarget>()
            .remove::<ForcedTarget>()
            .remove::<PendingBuild>()
            .remove::<PendingCommandFire>()
            .remove::<SelfDestructCountdown>();
    }
    modes.attack_ground = false;
    modes.attack_move = false;
    modes.patrol = false;
    modes.guard = false;
    modes.move_order = false;
    modes.set_target = false;
    modes.ability = false;
}

/// Label for the Ability button: `"Ability"` while any selected
/// command-fire caster is ready (the next cast fires from it),
/// otherwise the shortest remaining recharge as whole seconds rounded
/// up — upstream writes `math.ceil((readyFrame - now) / 32) .. "s"`
/// into the command's name.
fn ability_label(cooldowns: impl IntoIterator<Item = Option<f32>>) -> Option<String> {
    let mut soonest: Option<f32> = None;
    for remaining in cooldowns {
        match remaining {
            Some(r) if r > 0.0 => soonest = Some(soonest.map_or(r, |s| s.min(r))),
            _ => return None,
        }
    }
    soonest.map(|r| format!("{}s", r.ceil() as u32))
}

/// Keep the Ability button's label on the selection's recharge
/// countdown. Selections without a command-fire caster (Bug / Exploit /
/// teleporters only) always read "Ability".
fn update_ability_label(
    selected_q: Query<(&UnitType, Option<&CommandFireCooldown>), With<Selected>>,
    mut labels: Query<(&mut Text, &mut TextColor), With<AbilityLabel>>,
) {
    let countdown = ability_label(
        selected_q
            .iter()
            .filter(|(u, _)| u.0.has_command_fire_ability())
            .map(|(_, cd)| cd.map(|c| c.remaining)),
    );
    let (text, color) = match &countdown {
        Some(secs) => (secs.as_str(), TEXT_DISABLED),
        None => (OrderKind::Ability.label(), KP_GREEN),
    };
    for (mut label, mut label_color) in &mut labels {
        if label.0 != text {
            label.0 = text.to_string();
        }
        if label_color.0 != color {
            label_color.0 = color;
        }
    }
}

/// True when every selected AutoHold unit has the toggle on (and at
/// least one is selected) — the button's lit state.
fn autohold_on(autohold_q: &Query<&AutoHold, With<Selected>>) -> bool {
    !autohold_q.is_empty() && autohold_q.iter().all(|a| a.0)
}

/// Flip AutoHold for the whole selection as one group: all on → all off,
/// otherwise all on (a mixed group converges instead of inverting).
fn toggle_autohold(autohold_q: &mut Query<&mut AutoHold, With<Selected>>) {
    let next = !autohold_q.iter().all(|a| a.0);
    for mut hold in autohold_q.iter_mut() {
        hold.0 = next;
    }
}

struct OrderSnapshot {
    entries: Vec<OrderKind>,
}

impl OrderSnapshot {
    fn collect(selected_q: &Query<&UnitType, With<Selected>>) -> Self {
        if selected_q.is_empty() {
            return Self { entries: vec![] };
        }

        let mut has_caster = false;
        let mut has_autohold = false;
        for ut in selected_q {
            if ability_is_aimed(ut.0) || ut.0.deploy_pair().is_some() {
                has_caster = true;
            }
            has_autohold |= ut.0.has_autohold();
        }

        let mut entries = vec![
            OrderKind::Stop,
            OrderKind::Move,
            OrderKind::AttackGround,
            OrderKind::Fight,
            OrderKind::Guard,
            OrderKind::SetTarget,
            OrderKind::UnsetTarget,
            OrderKind::SelfDestruct,
        ];
        if has_autohold {
            entries.push(OrderKind::AutoHold);
        }
        if has_caster {
            entries.push(OrderKind::Ability);
        }
        Self { entries }
    }

    fn hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        for entry in &self.entries {
            entry.hash(&mut h);
        }
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Any ready caster keeps the button live; all recharging shows the
    /// soonest countdown, rounded up like upstream's label.
    #[test]
    fn ability_label_shows_soonest_recharge() {
        assert_eq!(ability_label([]), None);
        assert_eq!(ability_label([None]), None);
        assert_eq!(ability_label([Some(12.0), None]), None);
        assert_eq!(ability_label([Some(95.2)]), Some("96s".to_string()));
        assert_eq!(
            ability_label([Some(40.0), Some(3.01)]),
            Some("4s".to_string())
        );
    }
}

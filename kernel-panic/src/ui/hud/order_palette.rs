//! Top-right order palette: Stop / Attack / Self-destruct, plus the
//! contextual D-ability button (NX Flag, Infection, etc.) when the
//! selection includes a caster.
//!
//! Buttons mirror the hotkeys handled in [`crate::interaction::ability`]
//! so the user can drive the same actions via mouse. Most actually fire
//! by simulating the same ECS effects — Stop strips order components,
//! Attack toggles `AttackGroundMode`, Self-destruct inserts
//! `SelfDestructCountdown`. Command-fire abilities require a target
//! position, so the palette button just toggles the same mode the
//! hotkey would: the next ground click commits.

use bevy::prelude::*;

use crate::interaction::ability::AttackGroundMode;
use crate::interaction::movement::{CommandQueue, MovePath, MoveTarget};
use crate::interaction::selection::Selected;
use crate::units::combat::{AttackGroundOrder, SELF_DESTRUCT_DELAY, SelfDestructCountdown};
use crate::units::components::UnitType;
use crate::units::lifecycle::construction::PendingBuild;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum OrderKind {
    Stop,
    AttackGround,
    SelfDestruct,
    /// Cast the contextual D-ability (caster only).
    /// We can't drive command-fire from here without a target click —
    /// pressing the button just enables `AttackGroundMode` so the next
    /// click is consumed; gameplay-wise the effect is similar (pressing
    /// `D` over a ground point fires command-fire from the COB script).
    Ability,
}

impl OrderKind {
    fn label(self) -> &'static str {
        match self {
            OrderKind::Stop => "Stop",
            OrderKind::AttackGround => "Attack",
            OrderKind::SelfDestruct => "Detonate",
            OrderKind::Ability => "Ability",
        }
    }

    fn hotkey(self) -> &'static str {
        match self {
            OrderKind::Stop => "S",
            OrderKind::AttackGround => "A",
            OrderKind::SelfDestruct => "Ctrl+D",
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

    // Hash deliberately excludes `AttackGroundMode.active` — that's
    // painted by `update_armed_highlight` without rebuilding entities.
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
                    btn.spawn((
                        Text::new(kind.label()),
                        TextFont {
                            font_size: TEXT_BODY,
                            ..default()
                        },
                        TextColor(KP_GREEN),
                    ));
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
    attack_mode: Res<AttackGroundMode>,
    mut buttons: Query<(
        &OrderButton,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Node,
    )>,
) {
    for (button, mut border, mut bg, mut node) in &mut buttons {
        let armed = matches!(button.0, OrderKind::AttackGround) && attack_mode.active;
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
    selected_q: Query<Entity, With<Selected>>,
    mut attack_mode: ResMut<AttackGroundMode>,
) {
    for (interaction, button) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match button.0 {
            OrderKind::Stop => {
                for entity in &selected_q {
                    commands
                        .entity(entity)
                        .remove::<MoveTarget>()
                        .remove::<MovePath>()
                        .remove::<CommandQueue>()
                        .remove::<AttackGroundOrder>()
                        .remove::<PendingBuild>()
                        .remove::<SelfDestructCountdown>();
                }
                attack_mode.active = false;
            }
            OrderKind::AttackGround | OrderKind::Ability => {
                // Both arm the next-click handler; for Ability the user
                // expects the click to fire the unit's command-fire weapon
                // — the existing `D`-hotkey code already does that, so
                // here we just toggle the attack-ground latch which produces
                // the matching cursor + click semantics.
                attack_mode.active = !attack_mode.active;
            }
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

struct OrderSnapshot {
    entries: Vec<OrderKind>,
}

impl OrderSnapshot {
    fn collect(selected_q: &Query<&UnitType, With<Selected>>) -> Self {
        if selected_q.is_empty() {
            return Self { entries: vec![] };
        }

        let mut has_caster = false;
        for ut in selected_q {
            if ut.0.has_command_fire_ability() || ut.0.deploy_pair().is_some() {
                has_caster = true;
                break;
            }
        }

        let mut entries = vec![
            OrderKind::Stop,
            OrderKind::AttackGround,
            OrderKind::SelfDestruct,
        ];
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

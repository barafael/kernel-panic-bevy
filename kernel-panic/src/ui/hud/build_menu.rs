//! Right-side build menu.
//!
//! Two roles:
//! 1. **Factories** (Producer-bearing units): clicking an icon enqueues
//!    that unit on the factory's [`Producer::queue`].
//! 2. **Mobile constructors** (Assembler / Trojan / Gateway): clicking an
//!    icon arms the global [`PlacementMode`] — the placement system then
//!    converts the next ground click into a `BuildAt` command for each
//!    selected constructor.
//!
//! Click handling uses Bevy's `Interaction::Pressed` watching for
//! `Changed<Interaction>` — proven path, paired with a system ordering
//! that runs the click handler **before** the panel rebuilds so an icon
//! about to be despawned doesn't drop its press.

use bevy::prelude::*;

use crate::interaction::selection::Selected;
use crate::units::components::{Faction, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::construction::buildings_for;
use crate::units::lifecycle::production::Producer;

use super::super::theme::*;
use super::previews::UnitPreviews;

pub(super) struct BuildMenuPlugin;

impl Plugin for BuildMenuPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlacementMode>()
            // Click handler runs first so the just-pressed icon entity
            // is still alive when its `Changed<Interaction>` is read.
            // Armed-highlight runs separately so toggling placement
            // does *not* rebuild the panel — otherwise the new icon
            // entities inherit the still-held mouse press and re-toggle
            // placement on the next frame, eating the click.
            .add_systems(
                Update,
                (handle_clicks, refresh_panel, update_armed_highlight)
                    .chain()
                    // After the game-world rebuild: the teardown despawns
                    // the icon entities this panel rebuilds from; running
                    // before it would queue commands on despawned entities.
                    .after(crate::map_loading::GameWorldRebuild),
            );
    }
}

/// Active placement order: the next ground click commits a `BuildAt`
/// for `kind` to every selected constructor unit. Cleared on commit /
/// right-click / Escape.
///
/// Read by the placement system.
#[derive(Resource, Default, Debug, Clone, Copy)]
pub(crate) struct PlacementMode {
    pub kind: Option<UnitKind>,
}

/// Marker on the panel root so we can find/despawn the whole tree.
#[derive(Component)]
struct BuildMenuRoot;

#[derive(Component, Clone, Copy)]
struct BuildIcon {
    kind: UnitKind,
    /// `true` for a constructor's buildable structure (commits a
    /// placement); `false` for a factory's producible unit (enqueues
    /// directly).
    is_construction: bool,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn refresh_panel(
    mut commands: Commands,
    previews: Res<UnitPreviews>,
    registry: Res<UnitRegistry>,
    selected_q: Query<(&UnitType, Option<&Producer>, Option<&Faction>), With<Selected>>,
    existing: Query<Entity, With<BuildMenuRoot>>,
    mut last_hash: Local<u64>,
) {
    let snapshot = MenuSnapshot::collect(&selected_q);
    // Hash deliberately excludes `PlacementMode` — see
    // `update_armed_highlight`: rebuilding the panel on arm/disarm
    // would let the still-held mouse press re-toggle placement on the
    // newly-spawned icon entity.
    let new_hash = snapshot.hash();

    if new_hash == *last_hash {
        return;
    }
    *last_hash = new_hash;

    for entity in &existing {
        commands.entity(entity).despawn();
    }

    if snapshot.is_empty() {
        return;
    }

    commands
        .spawn((
            BuildMenuRoot,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(8.0),
                top: Val::Px(8.0),
                width: Val::Px(RIGHT_COLUMN_WIDTH),
                padding: UiRect::all(Val::Px(PANEL_PADDING)),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(PANEL_GAP),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(PANEL_BORDER),
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new("Build"),
                TextFont {
                    font_size: TEXT_TITLE,
                    ..default()
                },
                TextColor(KP_GREEN),
            ));
            parent
                .spawn(Node {
                    flex_direction: FlexDirection::Row,
                    flex_wrap: FlexWrap::Wrap,
                    column_gap: Val::Px(PANEL_GAP),
                    row_gap: Val::Px(PANEL_GAP),
                    ..default()
                })
                .with_children(|grid| {
                    for (kind, queue_count, is_construction) in snapshot.entries() {
                        spawn_icon(
                            grid,
                            kind,
                            queue_count,
                            is_construction,
                            &registry,
                            &previews,
                        );
                    }
                });
        });
}

/// Per-frame update that mutates each icon's border + background based
/// on whether the placement order is currently armed for its kind.
/// Decoupled from `refresh_panel` so toggling placement does **not**
/// despawn the icon entities — the still-held mouse press would
/// otherwise re-trigger `Changed<Interaction>` on the new icons and
/// flip placement back off, eating the user's click.
fn update_armed_highlight(
    placement: Res<PlacementMode>,
    mut icons: Query<(
        &BuildIcon,
        &mut BorderColor,
        &mut BackgroundColor,
        &mut Node,
    )>,
) {
    let active = placement.kind;
    for (icon, mut border, mut bg, mut node) in &mut icons {
        let armed = icon.is_construction && active == Some(icon.kind);
        let target_border = if armed { KP_GREEN } else { PANEL_BORDER };
        let target_bg = if armed { BUTTON_BG_PRESSED } else { BUTTON_BG };
        let target_width = if armed { 2.0 } else { 1.0 };
        *border = BorderColor::all(target_border);
        *bg = BackgroundColor(target_bg);
        node.border = UiRect::all(Val::Px(target_width));
    }
}

fn spawn_icon(
    grid: &mut ChildSpawnerCommands,
    kind: UnitKind,
    queue_count: u32,
    is_construction: bool,
    registry: &UnitRegistry,
    previews: &UnitPreviews,
) {
    // Spawn neutral; `update_armed_highlight` paints the armed visual
    // every frame without rebuilding the icon entity.
    grid.spawn((
        Button,
        BuildIcon {
            kind,
            is_construction,
        },
        Node {
            width: Val::Px(ICON_SIZE),
            height: Val::Px(ICON_SIZE),
            border: UiRect::all(Val::Px(1.0)),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::FlexEnd,
            align_items: AlignItems::Center,
            padding: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BackgroundColor(BUTTON_BG),
        BorderColor::all(PANEL_BORDER),
    ))
    .with_children(|btn| {
        // Image as a sized child so its aspect doesn't get stretched
        // by the icon's flex layout. The square slot leaves room
        // beneath for the unit name.
        if let Some(handle) = previews.get(kind) {
            btn.spawn((
                ImageNode::new(handle.clone()),
                Node {
                    width: Val::Px(ICON_SIZE - 6.0),
                    height: Val::Px(ICON_SIZE - 22.0),
                    ..default()
                },
            ));
        }

        btn.spawn((
            Text::new(registry.name(kind).to_string()),
            TextFont {
                font_size: TEXT_SMALL,
                ..default()
            },
            TextColor(KP_GREEN_DIM),
            TextLayout::new_with_justify(Justify::Center),
        ));

        if queue_count > 0 {
            btn.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    right: Val::Px(2.0),
                    top: Val::Px(1.0),
                    padding: UiRect::axes(Val::Px(3.0), Val::Px(0.0)),
                    ..default()
                },
                BackgroundColor(TEXT_BG),
            ))
            .with_children(|badge| {
                badge.spawn((
                    Text::new(format!("{queue_count}")),
                    TextFont {
                        font_size: TEXT_SMALL,
                        ..default()
                    },
                    TextColor(KP_GREEN),
                ));
            });
        }
    });
}

#[allow(clippy::type_complexity)]
fn handle_clicks(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    interactions: Query<(&Interaction, &BuildIcon), Changed<Interaction>>,
    mut producers: Query<&mut Producer, With<Selected>>,
    mut placement: ResMut<PlacementMode>,
) {
    // Right-click anywhere cancels armed placement (mirrors Spring).
    if mouse.just_pressed(MouseButton::Right) || keys.just_pressed(KeyCode::Escape) {
        placement.kind = None;
    }

    for (interaction, icon) in &interactions {
        if *interaction != Interaction::Pressed {
            continue;
        }
        if icon.is_construction {
            placement.kind = if placement.kind == Some(icon.kind) {
                None
            } else {
                Some(icon.kind)
            };
        } else {
            for mut producer in &mut producers {
                producer.enqueue(icon.kind);
            }
        }
    }
}

/// Snapshot of the buildable roster the menu would render this frame —
/// drives both rendering and the panel-state-hash idempotency check.
struct MenuSnapshot {
    /// `(kind, queue_count, is_construction)`.
    entries: Vec<(UnitKind, u32, bool)>,
}

impl MenuSnapshot {
    #[allow(clippy::type_complexity)]
    fn collect(
        selected_q: &Query<(&UnitType, Option<&Producer>, Option<&Faction>), With<Selected>>,
    ) -> Self {
        // Pick the first selected unit that has *any* roster.
        // Multi-builder tabs are deferred (see plan B1).
        let mut entries: Vec<(UnitKind, u32, bool)> = Vec::new();
        for (ut, producer, faction) in selected_q {
            if ut.0.is_constructor() {
                let buildings = buildings_for(ut.0);
                entries = buildings.iter().map(|k| (*k, 0, true)).collect();
                break;
            }
            if producer.is_some()
                && let Some(faction) = faction
            {
                let roster = factory_roster(ut.0, *faction);
                let queue_counts = producer
                    .map(|p| {
                        let mut counts = std::collections::HashMap::<UnitKind, u32>::new();
                        for kind in p.queue() {
                            *counts.entry(*kind).or_default() += 1;
                        }
                        counts
                    })
                    .unwrap_or_default();
                entries = roster
                    .iter()
                    .map(|k| (*k, queue_counts.get(k).copied().unwrap_or(0), false))
                    .collect();
                break;
            }
        }

        Self { entries }
    }

    fn entries(&self) -> impl Iterator<Item = (UnitKind, u32, bool)> + '_ {
        self.entries.iter().copied()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        // Non-zero seed so a roster of UnitKind::Kernel + Faction::System
        // (both discriminate to 0) doesn't collide with the
        // `Local<u64>::default() == 0` first-frame sentinel.
        0xcbf29ce484222325u64.hash(&mut h);
        for entry in &self.entries {
            entry.hash(&mut h);
        }
        h.finish()
    }
}

/// Per-faction factory rosters. Hardcoded to mirror upstream
/// `SIDEDATA.TDF`'s `[BUILDOPTIONS]`. Only the homebases and secondary
/// factories produce units; mobile builders use the construction
/// pipeline and have their own [`buildings_for`] roster.
fn factory_roster(factory: UnitKind, _faction: Faction) -> &'static [UnitKind] {
    match factory {
        UnitKind::Kernel => &[
            UnitKind::Bit,
            UnitKind::Byte,
            UnitKind::Pointer,
            UnitKind::Assembler,
        ],
        UnitKind::Hole => &[
            UnitKind::Bug,
            UnitKind::Worm,
            UnitKind::Dos,
            UnitKind::Trojan,
        ],
        UnitKind::Carrier => &[
            UnitKind::Packet,
            UnitKind::Signal,
            UnitKind::Flow,
            UnitKind::Connection,
            UnitKind::Gateway,
        ],
        UnitKind::Socket => &[UnitKind::Bit],
        UnitKind::Window => &[UnitKind::Bug],
        UnitKind::Port => &[UnitKind::Packet],
        _ => &[],
    }
}

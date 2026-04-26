//! Bottom-left info panel. Single selection: name, HP bar, weapon, speed.
//! Multi selection: per-kind tally.

use bevy::prelude::*;

use crate::interaction::selection::Selected;
use crate::units::components::{Faction, Health, UnitType, health_color};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

use super::super::theme::*;

pub(super) struct InfoPanelPlugin;

impl Plugin for InfoPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_panel)
            .add_systems(Update, refresh_panel);
    }
}

#[derive(Component)]
struct InfoPanelRoot;

#[derive(Component)]
struct InfoPanelContent;

#[derive(Component)]
struct InfoPanelStateHash(u64);

fn spawn_panel(mut commands: Commands) {
    commands
        .spawn((
            InfoPanelRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(8.0),
                bottom: Val::Px(8.0),
                width: Val::Px(LEFT_COLUMN_WIDTH),
                padding: UiRect::all(Val::Px(PANEL_PADDING)),
                border: UiRect::all(Val::Px(1.0)),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(PANEL_GAP),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(PANEL_BORDER),
            Visibility::Hidden,
            InfoPanelStateHash(0),
        ))
        .with_children(|parent| {
            parent.spawn((
                InfoPanelContent,
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(PANEL_GAP),
                    ..default()
                },
            ));
        });
}

#[allow(clippy::type_complexity)]
fn refresh_panel(
    mut commands: Commands,
    mut root_q: Query<(Entity, &mut Visibility, &mut InfoPanelStateHash), With<InfoPanelRoot>>,
    content_q: Query<Entity, With<InfoPanelContent>>,
    selected_q: Query<(&UnitType, Option<&Faction>, Option<&Health>), With<Selected>>,
    registry: Res<UnitRegistry>,
) {
    let Ok((root, mut visibility, mut hash_marker)) = root_q.single_mut() else {
        return;
    };

    // Build a stable signature of what we'd render — only rebuild when it
    // changes. This keeps Text node reuse idiomatic and avoids per-frame
    // despawn churn that would otherwise reflow the layout.
    let snapshot = collect_snapshot(&selected_q);
    let new_hash = snapshot.hash();
    if new_hash == hash_marker.0 {
        return;
    }
    hash_marker.0 = new_hash;

    let Ok(content) = content_q.single() else {
        return;
    };

    *visibility = if snapshot.is_empty() {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };

    commands.entity(content).despawn_related::<Children>();

    if snapshot.is_empty() {
        let _ = root;
        return;
    }

    match snapshot {
        Snapshot::Single {
            kind,
            faction,
            health_frac,
        } => {
            commands.entity(content).with_children(|parent| {
                build_single_unit_panel(parent, kind, faction, health_frac, &registry);
            });
        }
        Snapshot::Multi { tally, total } => {
            commands.entity(content).with_children(|parent| {
                build_multi_summary(parent, &tally, total);
            });
        }
        Snapshot::Empty => {}
    }
}

fn build_single_unit_panel(
    parent: &mut ChildSpawnerCommands,
    kind: UnitKind,
    faction: Option<Faction>,
    health_frac: Option<f32>,
    registry: &UnitRegistry,
) {
    let title = registry.name(kind).to_string();
    let title_color = faction.map_or(KP_GREEN, |f| f.color());

    parent.spawn((
        Text::new(title),
        TextFont {
            font_size: TEXT_TITLE,
            ..default()
        },
        TextColor(title_color),
    ));

    if let Some(frac) = health_frac {
        let max = registry.max_health(kind).max(1.0);
        let current = (frac * max).round() as i32;
        let max_i = max.round() as i32;
        parent
            .spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(2.0),
                ..default()
            })
            .with_children(|inner| {
                inner.spawn((
                    Text::new(format!("HP {current} / {max_i}")),
                    TextFont {
                        font_size: TEXT_SMALL,
                        ..default()
                    },
                    TextColor(KP_GREEN_DIM),
                ));
                // Health bar: black backing, colored fill, 8px tall.
                inner
                    .spawn((
                        Node {
                            width: Val::Percent(100.0),
                            height: Val::Px(6.0),
                            border: UiRect::all(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(TEXT_BG),
                        BorderColor::all(PANEL_BORDER),
                    ))
                    .with_children(|bar| {
                        bar.spawn((
                            Node {
                                width: Val::Percent((frac * 100.0).clamp(0.0, 100.0)),
                                height: Val::Percent(100.0),
                                ..default()
                            },
                            BackgroundColor(health_color(frac)),
                        ));
                    });
            });
    }

    let weapon = registry.weapon(kind);
    let speed = registry.speed(kind);

    if !weapon.is_empty() {
        parent.spawn((
            Text::new(format!("Weapon: {weapon}")),
            TextFont {
                font_size: TEXT_BODY,
                ..default()
            },
            TextColor(KP_GREEN_DIM),
        ));
    }

    if speed > 0.0 {
        parent.spawn((
            Text::new(format!("Speed: {speed:.0}")),
            TextFont {
                font_size: TEXT_BODY,
                ..default()
            },
            TextColor(KP_GREEN_DIM),
        ));
    } else {
        parent.spawn((
            Text::new("Stationary"),
            TextFont {
                font_size: TEXT_BODY,
                ..default()
            },
            TextColor(KP_GREEN_DIM),
        ));
    }
}

fn build_multi_summary(parent: &mut ChildSpawnerCommands, tally: &[(UnitKind, u32)], total: u32) {
    parent.spawn((
        Text::new(format!("Selection: {total} units")),
        TextFont {
            font_size: TEXT_TITLE,
            ..default()
        },
        TextColor(KP_GREEN),
    ));
    for (kind, count) in tally {
        parent.spawn((
            Text::new(format!("{:>3} × {}", count, kind.unitname())),
            TextFont {
                font_size: TEXT_BODY,
                ..default()
            },
            TextColor(KP_GREEN_DIM),
        ));
    }
}

enum Snapshot {
    Empty,
    Single {
        kind: UnitKind,
        faction: Option<Faction>,
        health_frac: Option<f32>,
    },
    Multi {
        /// Per-kind counts, sorted by `unitname()` for deterministic
        /// rendering + stable hashing.
        tally: Vec<(UnitKind, u32)>,
        total: u32,
    },
}

impl Snapshot {
    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Hash that flips when the rendered content would differ. Health is
    /// quantised to whole percent so the panel doesn't rebuild every
    /// frame as a unit takes damage.
    fn hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        match self {
            Snapshot::Empty => 0u8.hash(&mut h),
            Snapshot::Single {
                kind,
                faction,
                health_frac,
            } => {
                1u8.hash(&mut h);
                kind.hash(&mut h);
                faction.hash(&mut h);
                let pct = health_frac
                    .map(|f| (f * 100.0).round() as i32)
                    .unwrap_or(-1);
                pct.hash(&mut h);
            }
            Snapshot::Multi { tally, total } => {
                2u8.hash(&mut h);
                total.hash(&mut h);
                for (k, c) in tally {
                    k.hash(&mut h);
                    c.hash(&mut h);
                }
            }
        }
        h.finish()
    }
}

#[allow(clippy::type_complexity)]
fn collect_snapshot(
    selected_q: &Query<(&UnitType, Option<&Faction>, Option<&Health>), With<Selected>>,
) -> Snapshot {
    let mut iter = selected_q.iter();
    let Some(first) = iter.next() else {
        return Snapshot::Empty;
    };

    if iter.next().is_none() {
        return Snapshot::Single {
            kind: first.0.0,
            faction: first.1.copied(),
            health_frac: first.2.map(|h| h.fraction()),
        };
    }

    let mut tally = std::collections::HashMap::<UnitKind, u32>::new();
    let mut total = 0u32;
    for (ut, _, _) in selected_q {
        *tally.entry(ut.0).or_default() += 1;
        total += 1;
    }
    let mut tally: Vec<(UnitKind, u32)> = tally.into_iter().collect();
    tally.sort_by_key(|(k, _)| k.unitname());
    Snapshot::Multi { tally, total }
}

//! Scaffolding shared by the immediate-mode HUD widgets (command panel,
//! build bar, tooltip): they rebuild their node tree from scratch when a
//! signature of what they show changes, under a zero-size root that
//! lives as long as the match. The one-node overlays (banner, chat,
//! TAKE, status lines) keep their node and update it ([`sync_single`]).

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::game_setup::AppState;

/// The primary window's logical size and the cursor on it, if any.
pub(super) fn view_and_cursor(
    windows: &Query<&Window, With<PrimaryWindow>>,
) -> Option<(Vec2, Option<Vec2>)> {
    let w = windows.single().ok()?;
    Some((w.size(), w.cursor_position()))
}

/// Whether a widget must be rebuilt for signature `sig` (`0` = show
/// nothing): the signature changed, or the widget's root went away (a
/// game-world teardown). Despawns the stale roots and records `sig`
/// when it returns `true`; the caller then builds unless `sig` is `0`.
pub(super) fn needs_rebuild<M: Component>(
    commands: &mut Commands,
    sig: u64,
    last: &mut u64,
    roots: &Query<Entity, With<M>>,
) -> bool {
    if sig == *last && (sig == 0 || !roots.is_empty()) {
        return false;
    }
    *last = sig;
    for e in roots {
        commands.entity(e).despawn();
    }
    true
}

/// Keeps a widget of one node in step with `want` (`None` = show
/// nothing): `node` is its query's `single_mut()`; the node is updated
/// in place when it is there, spawned when it is missing and despawned
/// when there is nothing to show.
pub(super) fn sync_single<T, N, E>(
    commands: &mut Commands,
    want: Option<T>,
    node: Result<(Entity, N), E>,
    update: impl FnOnce(N, T),
    spawn: impl FnOnce(&mut Commands, T),
) {
    match (want, node) {
        (Some(t), Ok((_, n))) => update(n, t),
        (Some(t), Err(_)) => spawn(commands, t),
        (None, Ok((e, _))) => commands.entity(e).despawn(),
        (None, Err(_)) => {}
    }
}

/// `text` says `s` (change detection fires only when it changes).
pub(super) fn set_text(mut text: Mut<Text>, s: String) {
    if text.0 != s {
        text.0 = s;
    }
}

/// A zero-size root at the window's top-left corner for absolutely
/// placed widget nodes, drawn behind the menus and dropped with the
/// match.
pub(super) fn hud_root(commands: &mut Commands, marker: impl Bundle) -> Entity {
    commands
        .spawn((
            marker,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Px(0.0),
                height: Val::Px(0.0),
                ..default()
            },
            GlobalZIndex(-1),
            DespawnOnExit(AppState::InGame),
        ))
        .id()
}

/// A node covering its whole parent (washes, frames, outlines).
pub(super) fn cover() -> Node {
    Node {
        position_type: PositionType::Absolute,
        left: Val::Px(0.0),
        top: Val::Px(0.0),
        width: Val::Percent(100.0),
        height: Val::Percent(100.0),
        ..default()
    }
}

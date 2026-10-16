//! The flashing TAKE button (the user's rule, 2026-10-15): while an
//! allied owner that has left play still has units, every active ally
//! sees it; a click sends `SimCommand::Take` for each such owner, and the
//! first take in turn order gets the units (`net::apply`). Also drops the
//! selection of units that changed hands.

use bevy::picking::Pickable;
use bevy::prelude::*;

use crate::game_setup::AppState;
use crate::interaction::selection::Selected;
use crate::net::{CommandBus, SimCommand};
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::player::LocalTeam;
use crate::units::teams::{Alliances, TeamFates};

use super::widgets::sync_single;

/// The button.
#[derive(Component)]
struct TakeButton;

pub(super) struct TakeButtonPlugin;

impl Plugin for TakeButtonPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (show_take_button, drop_foreign_selection).run_if(in_state(AppState::InGame)),
        )
        // With the HUD's other mouse input: after the menu's, before
        // selection and orders, which never see a press it took.
        .add_systems(
            PreUpdate,
            press_take_button
                .after(crate::ui::menu::MenuMouseInput)
                .run_if(in_state(AppState::InGame)),
        );
    }
}

/// The allied owners whose units the local player may take: left play,
/// units left; none while the local player is out or spectating.
fn takeable(
    local: LocalTeam,
    alliances: &Alliances,
    fates: &TeamFates,
    units: &Query<&TeamId, (With<UnitType>, Without<Dying>)>,
) -> Vec<u8> {
    let Some(local) = local
        .seat()
        .filter(|&l| !fates.is_dead(l) && alliances.is_seated(l))
    else {
        return Vec::new();
    };
    alliances
        .allies_of(local)
        .filter(|o| *o != local && fates.is_dead(*o))
        .filter(|o| units.iter().any(|t| t.0 == *o))
        .collect()
}

fn show_take_button(
    mut commands: Commands,
    local: Option<Res<LocalTeam>>,
    alliances: Option<Res<Alliances>>,
    fates: Option<Res<TeamFates>>,
    units: Query<&TeamId, (With<UnitType>, Without<Dying>)>,
    time: Res<Time<Real>>,
    mut button: Query<(Entity, &mut BackgroundColor), With<TakeButton>>,
) {
    let show = match (local, alliances, fates) {
        (Some(l), Some(a), Some(f)) => !takeable(*l, &a, &f, &units).is_empty(),
        _ => false,
    };
    sync_single(
        &mut commands,
        show.then_some(()),
        button.single_mut(),
        |mut bg, ()| {
            // Flash: twice a second.
            let on = ((time.elapsed_secs() * 4.0) as u32).is_multiple_of(2);
            bg.0 = if on {
                Color::srgb(0.9, 0.6, 0.0)
            } else {
                Color::srgb(0.35, 0.2, 0.0)
            };
        },
        |commands, ()| {
            commands
                .spawn((
                    TakeButton,
                    Node {
                        position_type: PositionType::Absolute,
                        top: Val::Px(40.0),
                        left: Val::Percent(50.0),
                        padding: UiRect::axes(Val::Px(14.0), Val::Px(6.0)),
                        ..default()
                    },
                    UiTransform::from_translation(Val2::percent(-50.0, 0.0)),
                    BackgroundColor(Color::srgb(0.9, 0.6, 0.0)),
                    DespawnOnExit(AppState::InGame),
                ))
                .with_child((
                    Text::new("TAKE"),
                    TextFont::from_font_size(22.0),
                    TextColor(Color::BLACK),
                    Pickable::IGNORE,
                ));
        },
    );
}

/// A left click on the button takes every takeable owner's units; the
/// press is consumed (the menu's own hit test, `ComputedNode`).
#[allow(clippy::too_many_arguments)]
fn press_take_button(
    button: Query<(&ComputedNode, &UiGlobalTransform), With<TakeButton>>,
    windows: Query<&Window>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    local: Option<Res<LocalTeam>>,
    alliances: Option<Res<Alliances>>,
    fates: Option<Res<TeamFates>>,
    units: Query<&TeamId, (With<UnitType>, Without<Dying>)>,
    mut bus: ResMut<CommandBus>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }
    let Ok((node, transform)) = button.single() else {
        return;
    };
    let Some(cursor) = windows
        .single()
        .ok()
        .and_then(|w| Some(w.cursor_position()? * w.scale_factor()))
    else {
        return;
    };
    if !node.contains_point(*transform, cursor) {
        return;
    }
    mouse.clear_just_pressed(MouseButton::Left);
    let (Some(local), Some(alliances), Some(fates)) = (local, alliances, fates) else {
        return;
    };
    for from in takeable(*local, &alliances, &fates, &units) {
        bus.issue(SimCommand::Take { from });
    }
}

/// Units that changed hands leave the old owner's selection (the new
/// owner selects them itself).
#[allow(clippy::type_complexity)]
fn drop_foreign_selection(
    mut commands: Commands,
    local: Option<Res<LocalTeam>>,
    changed: Query<(Entity, &TeamId), (Changed<TeamId>, With<Selected>)>,
) {
    let Some(local) = local else {
        return;
    };
    for (e, team) in &changed {
        if team.0 != local.0 {
            commands.entity(e).remove::<Selected>();
        }
    }
}

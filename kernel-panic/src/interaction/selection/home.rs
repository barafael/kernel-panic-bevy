//! The local player's homebase and the camera: a match opens looking at
//! it, and `Tab` selects it and brings the camera there (again: the
//! next homebase, for a player with several).
//!
//! Only in the player's own match: not in the menu's demo, and not for
//! a spectator, who has no homebase.

use bevy::prelude::*;

use super::core::{Selected, SelectionSet};
use crate::game_setup::ActiveMatch;
use crate::map_loading::MatchScopeAppExt;
use crate::rendering::camera::{RtsCamera, RtsCameraState};
use crate::units::combat::Dying;
use crate::units::components::{Homebase, TeamId};
use crate::units::player::LocalTeam;

/// How far the opening shot stands from the homebase (elmos): the base
/// and the ground round it, not the whole map.
const OPENING_DISTANCE: f32 = 1400.0;

pub(super) struct HomeCameraPlugin;

impl Plugin for HomeCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_match_resource::<Opened>().add_systems(
            Update,
            (open_on_home, select_home.in_set(SelectionSet::Select)).run_if(own_match),
        );
    }
}

fn own_match(active: Option<Res<ActiveMatch>>) -> bool {
    active.is_some_and(|m| !m.0.demo)
}

/// Every living homebase: whose and where.
type HomeQuery<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static TeamId, &'static GlobalTransform),
    (With<Homebase>, Without<Dying>),
>;

/// The opening shot has been taken this match.
#[derive(Resource, Default)]
struct Opened(bool);

/// The local player's living homebases, in a fixed order.
fn homes(local: LocalTeam, bases: &HomeQuery) -> Vec<(Entity, Vec3)> {
    let mut homes: Vec<(Entity, Vec3)> = bases
        .iter()
        .filter(|(_, team, _)| local.seat() == Some(team.0))
        .map(|(e, _, tf)| (e, tf.translation()))
        .collect();
    homes.sort_unstable_by_key(|(e, _)| *e);
    homes
}

/// Once the local homebase stands, look at it. The map's load framed
/// the whole map (`map_loading::setup_camera`); a spectator keeps that.
fn open_on_home(
    local: Res<LocalTeam>,
    mut opened: ResMut<Opened>,
    bases: HomeQuery,
    mut cam: Query<&mut RtsCameraState, With<RtsCamera>>,
) {
    if opened.0 {
        return;
    }
    if local.seat().is_none() {
        opened.0 = true;
        return;
    }
    let Some(&(_, at)) = homes(*local, &bases).first() else {
        return;
    };
    if let Ok(mut state) = cam.single_mut() {
        state.snap_to(at, OPENING_DISTANCE);
        opened.0 = true;
    }
}

/// `Tab`: select the homebase and glide the camera to it. With the
/// selection already a lone homebase, the next one.
fn select_home(
    keys: Res<ButtonInput<KeyCode>>,
    local: Res<LocalTeam>,
    esc: Option<Res<crate::ui::menu::EscMenuOpen>>,
    selected: Query<Entity, With<Selected>>,
    bases: HomeQuery,
    mut cam: Query<&mut RtsCameraState, With<RtsCamera>>,
    mut commands: Commands,
) {
    // With the Esc overlay up, `Tab` walks its buttons.
    if !keys.just_pressed(KeyCode::Tab) || esc.is_some_and(|o| o.0) {
        return;
    }
    let homes = homes(*local, &bases);
    if homes.is_empty() {
        return;
    }
    let current: Vec<Entity> = selected.iter().collect();
    let next = match current[..] {
        [one] => homes
            .iter()
            .position(|&(e, _)| e == one)
            .map_or(0, |i| (i + 1) % homes.len()),
        _ => 0,
    };
    let (home, at) = homes[next];
    for e in current {
        commands.entity(e).remove::<Selected>();
    }
    commands.entity(home).insert(Selected);
    if let Ok(mut state) = cam.single_mut() {
        state.focus = at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn world_with_bases() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.insert_resource(LocalTeam(1));
        world.spawn((RtsCamera, RtsCameraState::default()));
        world.spawn((
            Homebase,
            TeamId(0),
            GlobalTransform::from_xyz(100.0, 0.0, 100.0),
        ));
        let a = world
            .spawn((
                Homebase,
                TeamId(1),
                GlobalTransform::from_xyz(3000.0, 50.0, 2000.0),
            ))
            .id();
        let b = world
            .spawn((
                Homebase,
                TeamId(1),
                GlobalTransform::from_xyz(500.0, 50.0, 2500.0),
            ))
            .id();
        (world, a, b)
    }

    fn focus(world: &mut World) -> Vec3 {
        world
            .query::<&RtsCameraState>()
            .single(world)
            .unwrap()
            .focus
    }

    #[test]
    fn the_match_opens_on_the_local_homebase() {
        let (mut world, a, b) = world_with_bases();
        world.insert_resource(Opened::default());
        world.run_system_once(open_on_home).unwrap();
        let at = |e| world.get::<GlobalTransform>(e).unwrap().translation();
        let mine = [at(a), at(b)];
        assert!(mine.contains(&focus(&mut world)));
        assert!(world.resource::<Opened>().0);
    }

    #[test]
    fn a_spectator_keeps_the_map_view() {
        let (mut world, ..) = world_with_bases();
        world.insert_resource(LocalTeam(crate::game_setup::SPECTATOR_TEAM));
        world.insert_resource(Opened::default());
        let before = focus(&mut world);
        world.run_system_once(open_on_home).unwrap();
        assert_eq!(focus(&mut world), before);
    }

    #[test]
    fn tab_selects_the_homebase_then_the_next() {
        let (mut world, a, b) = world_with_bases();
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::Tab);
        world.insert_resource(keys);
        let unit = world.spawn((TeamId(1), Selected)).id();
        let selected = |world: &mut World| -> Vec<Entity> {
            let mut q = world.query_filtered::<Entity, With<Selected>>();
            q.iter(world).collect()
        };
        let at = |world: &World, e| world.get::<GlobalTransform>(e).unwrap().translation();
        world.run_system_once(select_home).unwrap();
        let first = selected(&mut world);
        assert!(first == [a] || first == [b], "{first:?}");
        assert!(world.get::<Selected>(unit).is_none());
        assert_eq!(focus(&mut world), at(&world, first[0]));
        // The other homebase, then back to the first.
        let other = if first == [a] { b } else { a };
        world.run_system_once(select_home).unwrap();
        assert_eq!(selected(&mut world), vec![other]);
        assert_eq!(focus(&mut world), at(&world, other));
        world.run_system_once(select_home).unwrap();
        assert_eq!(selected(&mut world), first);
    }
}

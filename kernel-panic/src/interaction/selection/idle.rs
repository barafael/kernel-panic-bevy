//! `Ctrl+B`: select an idle builder and bring the camera to it; pressed
//! again, the next one. Spring's default `bind Ctrl+b select
//! AllMap+_Builder_Idle+_ClearSelection_SelectOne+`: its `Builder`
//! (`IsBuilderUnit`) takes factories too, and `Idle` is an empty command
//! queue, so here a constructor without orders or a factory with nothing
//! queued (a minifac on Repeat always has its unit queued).

use bevy::prelude::*;

use super::core::{Selected, SelectionSet};
use crate::interaction::movement::{CommandQueue, OrderState, OrderStateItem};
use crate::rendering::camera::{RtsCamera, RtsCameraState};
use crate::units::combat::Dying;
use crate::units::components::{TeamId, UnitType};
use crate::units::lifecycle::production::Producer;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::player::LocalTeam;

pub(super) struct IdleBuildersPlugin;

impl Plugin for IdleBuildersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, select_idle_builder.in_set(SelectionSet::Select));
    }
}

/// Every finished, living unit, with what it is doing.
type Units<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static UnitType,
        &'static TeamId,
        &'static GlobalTransform,
        OrderState,
        Option<&'static CommandQueue>,
        Option<&'static Producer>,
    ),
    (Without<Dying>, Without<Emerging>),
>;

/// A builder with nothing to do: a factory with an empty queue, a
/// constructor with no order and nothing queued.
fn idle_builder(
    unit: &UnitType,
    order: &OrderStateItem<'_, '_>,
    queue: Option<&CommandQueue>,
    producer: Option<&Producer>,
) -> bool {
    match producer {
        Some(factory) => factory.queue().is_empty(),
        None => {
            unit.0.is_constructor()
                && !order.active()
                && queue.is_none_or(|q| q.commands.is_empty())
        }
    }
}

fn select_idle_builder(
    keys: Res<ButtonInput<KeyCode>>,
    local: Res<LocalTeam>,
    selected: Query<Entity, With<Selected>>,
    units: Units,
    mut cam: Query<&mut RtsCameraState, With<RtsCamera>>,
    mut commands: Commands,
) {
    if !keys.just_pressed(KeyCode::KeyB)
        || !keys.any_pressed([KeyCode::ControlLeft, KeyCode::ControlRight])
        || keys.any_pressed([
            KeyCode::AltLeft,
            KeyCode::AltRight,
            KeyCode::ShiftLeft,
            KeyCode::ShiftRight,
        ])
    {
        return;
    }
    let Some(seat) = local.seat() else {
        return;
    };
    let mut idle: Vec<(Entity, Vec3)> = units
        .iter()
        .filter(|(_, unit, team, _, order, queue, producer)| {
            team.0 == seat && idle_builder(unit, order, *queue, *producer)
        })
        .map(|(e, _, _, tf, ..)| (e, tf.translation()))
        .collect();
    if idle.is_empty() {
        return;
    }
    idle.sort_unstable_by_key(|(e, _)| *e);
    // The one after the unit selected alone (`selectNumber`), else the
    // first.
    let current: Vec<Entity> = selected.iter().collect();
    let next = match current[..] {
        [one] => idle.iter().position(|&(e, _)| e > one).unwrap_or(0),
        _ => 0,
    };
    let (builder, at) = idle[next];
    for e in current {
        commands.entity(e).remove::<Selected>();
    }
    commands.entity(builder).insert(Selected);
    if let Ok(mut state) = cam.single_mut() {
        state.focus = at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::movement::QueuedCommand;
    use crate::units::content::definitions::UnitKind;
    use bevy::ecs::system::RunSystemOnce;

    fn selected(world: &mut World) -> Vec<Entity> {
        let mut q = world.query_filtered::<Entity, With<Selected>>();
        q.iter(world).collect()
    }

    /// Ctrl+B walks the idle constructors and factories, camera along;
    /// busy ones, unfinished ones and the enemy's are skipped.
    #[test]
    fn ctrl_b_cycles_the_idle_builders() {
        let mut world = World::new();
        world.insert_resource(LocalTeam(0));
        world.spawn((RtsCamera, RtsCameraState::default()));
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ControlLeft);
        keys.press(KeyCode::KeyB);
        world.insert_resource(keys);
        let at = |x: f32| GlobalTransform::from_xyz(x, 0.0, 0.0);
        let unit = |kind| (UnitType(kind), TeamId(0));
        let assembler = world.spawn((unit(UnitKind::Assembler), at(100.0))).id();
        let busy = CommandQueue {
            commands: vec![QueuedCommand::Move(Vec3::ZERO)],
        };
        world.spawn((unit(UnitKind::Assembler), at(200.0), busy));
        let rising = Emerging {
            target_y: 0.0,
            remaining: 1.0,
            total: 1.0,
            rally_point: None,
            rally_then: None,
            style: crate::units::lifecycle::spawning::EmergeStyle::Fade,
        };
        world.spawn((unit(UnitKind::Assembler), at(300.0), rising));
        world.spawn((UnitType(UnitKind::Assembler), TeamId(1), at(400.0)));
        let kernel = world
            .spawn((unit(UnitKind::Kernel), at(500.0), Producer::new()))
            .id();
        let mut socket = Producer::new();
        socket.enqueue(UnitKind::Bit);
        world.spawn((unit(UnitKind::Socket), at(600.0), socket));
        // A Bit is no builder.
        let bit = world.spawn((unit(UnitKind::Bit), at(700.0), Selected)).id();

        let focus = |world: &mut World| {
            world
                .query::<&RtsCameraState>()
                .single(world)
                .unwrap()
                .focus
        };
        world.run_system_once(select_idle_builder).unwrap();
        let first = selected(&mut world);
        assert!(world.get::<Selected>(bit).is_none());
        assert!(first == [assembler] || first == [kernel], "{first:?}");
        let other = if first == [assembler] {
            kernel
        } else {
            assembler
        };
        let x = |world: &World, e| world.get::<GlobalTransform>(e).unwrap().translation();
        assert_eq!(focus(&mut world), x(&world, first[0]));
        world.run_system_once(select_idle_builder).unwrap();
        assert_eq!(selected(&mut world), vec![other]);
        assert_eq!(focus(&mut world), x(&world, other));
        world.run_system_once(select_idle_builder).unwrap();
        assert_eq!(selected(&mut world), first, "round again");
    }
}

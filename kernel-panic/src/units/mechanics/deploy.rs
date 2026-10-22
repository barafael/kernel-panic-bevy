//! Bug ↔ Exploit deploy.
//!
//! Bugs can deploy into stationary Exploits (long-range artillery that
//! deal more damage the farther their target is). The player triggers
//! the deploy/pack-up cycle via the `D` hotkey; health is transferred
//! proportionally so a 50%-HP Bug becomes a 50%-HP Exploit.

use bevy::prelude::*;

use crate::interaction::movement::{MovePath, MoveTarget, QueuedCommand, chase_path_stale};
use crate::interaction::selection::Selected;
use crate::net::NetId;
use crate::sim::DetFloat;
use crate::units::combat::{Dying, Stunned};
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::lifecycle::spawning::{SpawnContext, spawn_unit};

/// Event: the listed unit should deploy to its paired form. The
/// deploy system resolves the target kind from the source. `then` is
/// the order the new form starts with (a Bombard's attack).
#[derive(Message, Debug, Clone, Copy)]
pub struct DeployEvent {
    pub entity: Entity,
    pub then: Option<QueuedCommand>,
}

/// `bug_bombard.lua`: the Bug deploys once within this of its target
/// (3D), else walks there with that as its goal radius. The Exploit's
/// BugCannon reaches 1200, so the attack it then gets is in range.
pub const BOMBARD_RANGE: f32 = 1150.0;

/// A Bug's running Bombard (`CMD_BOMBARD`, `CommandFallback`): walk
/// within [`BOMBARD_RANGE`] of the target, then deploy and, unless
/// `deploy_only` (Alt), attack it as the Exploit. Ends with the deploy
/// (the Bug is gone) or when a unit target dies.
#[derive(Component, Clone, Copy, Debug)]
#[component(storage = "SparseSet")]
pub struct BombardOrder {
    /// The unit aimed at, or `None` for a ground point.
    pub target: Option<Entity>,
    /// The ground point, or the unit's position when the order came.
    pub pos: Vec3,
    pub deploy_only: bool,
}

/// `bug_bombard.lua`'s per-frame pass: a Bug with a [`BombardOrder`]
/// chases its target like an attack (goal refreshed when it lags) until
/// within [`BOMBARD_RANGE`], then stops, drops the order and asks for
/// the deploy, with the attack on the target as the Exploit's first
/// order. A target that is gone ends the order (the gadget returns
/// `false`); a stunned Bug waits.
#[allow(clippy::type_complexity)]
pub fn tick_bombard(
    bugs: Query<
        (
            Entity,
            &Transform,
            &BombardOrder,
            Has<MoveTarget>,
            Option<&MovePath>,
            &NetId,
        ),
        (Without<Dying>, Without<Stunned>),
    >,
    targets: Query<&Transform, Without<Dying>>,
    mut deploy: MessageWriter<DeployEvent>,
    mut commands: Commands,
) {
    for (bug, tf, order, has_move, path, _) in bugs.iter().sort::<&NetId>() {
        let target_pos = match order.target {
            Some(t) => match targets.get(t) {
                Ok(ttf) => ttf.translation,
                Err(_) => {
                    commands
                        .entity(bug)
                        .remove::<(BombardOrder, MoveTarget, MovePath)>();
                    continue;
                }
            },
            None => order.pos,
        };
        if tf.translation.distance(target_pos) > BOMBARD_RANGE {
            if chase_path_stale(path, target_pos) {
                commands.entity(bug).insert(MoveTarget(target_pos));
            }
            continue;
        }
        let mut ec = commands.entity(bug);
        ec.remove::<BombardOrder>();
        if has_move || path.is_some() {
            ec.remove::<(MoveTarget, MovePath)>();
        }
        let attack = match order.target {
            Some(target) => QueuedCommand::AttackUnit {
                target,
                pos: target_pos,
            },
            None => QueuedCommand::AttackGround(order.pos),
        };
        let then = (!order.deploy_only).then_some(attack);
        deploy.write(DeployEvent { entity: bug, then });
    }
}

/// Process deploy events by despawning the source and spawning
/// its pair with proportional HP at the same position. The pair starts
/// with the event's `then` order, if any, and keeps the player's
/// selection (`kp_keep_morphed_selected.lua`).
#[allow(clippy::type_complexity)]
pub fn process_deploy(
    mut events: MessageReader<DeployEvent>,
    // `Without<Dying>`: a unit in its death animation must not be
    // swapped for a fresh pair (the corpse would be resurrected as a
    // 1 % pair while the death pipeline still owns it).
    query: Query<
        (
            &UnitType,
            &Faction,
            &TeamId,
            &Transform,
            &Health,
            Has<Selected>,
        ),
        Without<Dying>,
    >,
    mut ctx: SpawnContext,
) {
    for event in events.read() {
        let Ok((unit, faction, team, transform, health, selected)) = query.get(event.entity) else {
            continue;
        };
        if health.current <= 0.0 {
            continue;
        }
        let Some(target_kind) = unit.0.deploy_pair() else {
            continue;
        };

        // HP carry-over: preserve the health fraction. A 1-HP Bug
        // deploys into a 1-HP-equivalent Exploit, not a pristine one.
        let hp_fraction = health.fraction().det_clamp(0.01, 1.0);
        let new_max = ctx.unit_registry.max_health(target_kind);
        let new_current = (new_max * hp_fraction).det_max(1.0);

        // The pair is spawned in its place; the source skips `Dying`,
        // but its counters' marker hooks run on the despawn itself.
        ctx.commands.entity(event.entity).despawn();

        let spawned = spawn_unit(
            target_kind,
            *faction,
            team.0,
            transform.translation,
            &mut ctx,
        );
        let mut ec = ctx.commands.entity(spawned);
        ec.insert(Health {
            current: new_current,
            max: new_max,
        });
        if selected {
            ec.insert(Selected);
        }
        if let Some(then) = event.then {
            crate::interaction::install_command(&mut ec, then);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// `bug_bombard.lua`: out of range the Bug walks at its target; within
    /// 1150 it stops, drops the order and asks to deploy with the attack
    /// on the target as the Exploit's first order (none with Alt).
    #[test]
    fn bombard_walks_in_then_deploys_with_the_attack() {
        let mut world = World::new();
        world.init_resource::<Messages<DeployEvent>>();
        let ground = Vec3::new(2000.0, 0.0, 0.0);
        let bug = world
            .spawn((
                UnitType(crate::units::content::definitions::UnitKind::Bug),
                NetId(1),
                Transform::IDENTITY,
                BombardOrder {
                    target: None,
                    pos: ground,
                    deploy_only: false,
                },
            ))
            .id();
        world.run_system_once(tick_bombard).unwrap();
        assert_eq!(world.get::<MoveTarget>(bug).map(|t| t.0), Some(ground));
        assert!(world.get::<BombardOrder>(bug).is_some());

        world.get_mut::<Transform>(bug).unwrap().translation = Vec3::new(900.0, 0.0, 0.0);
        world.run_system_once(tick_bombard).unwrap();
        assert!(world.get::<BombardOrder>(bug).is_none());
        assert!(world.get::<MoveTarget>(bug).is_none());
        let events: Vec<DeployEvent> = world
            .resource_mut::<Messages<DeployEvent>>()
            .drain()
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].entity, bug);
        assert_eq!(events[0].then, Some(QueuedCommand::AttackGround(ground)));

        // Alt: deploy only. A unit target that is gone ends the order.
        let gone = world.spawn_empty().id();
        world.despawn(gone);
        world.entity_mut(bug).insert(BombardOrder {
            target: None,
            pos: Vec3::new(1000.0, 0.0, 0.0),
            deploy_only: true,
        });
        world.run_system_once(tick_bombard).unwrap();
        let events: Vec<DeployEvent> = world
            .resource_mut::<Messages<DeployEvent>>()
            .drain()
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].then, None);
        world.entity_mut(bug).insert(BombardOrder {
            target: Some(gone),
            pos: Vec3::ZERO,
            deploy_only: false,
        });
        world.run_system_once(tick_bombard).unwrap();
        assert!(world.get::<BombardOrder>(bug).is_none());
        assert!(
            world
                .resource_mut::<Messages<DeployEvent>>()
                .drain()
                .next()
                .is_none(),
            "no deploy for a dead target"
        );
    }
}

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
use crate::units::lifecycle::spawning::SpawnContext;

/// Event: the listed unit should deploy to its paired form. The
/// deploy system resolves the target kind from the source. `then` is
/// the order the new form starts with (a Bombard's attack).
#[derive(Message, Debug, Clone, Copy)]
pub struct DeployEvent {
    pub entity: Entity,
    pub then: Option<QueuedCommand>,
}

/// The scripts' `Deploy()` / `Undeploy()` run 88 frames between the
/// order and `lua_SwitchToExploit` / `lua_SwitchToBug`: the body flips
/// (about 21 frames of moves) and the 67-step `sleep 30` alpha loop
/// (bug.bos:20-47, exploit.bos:18-45).
const MORPH_SECS: f32 = 88.0 / 30.0;

/// bug.bos `Create`: a Bug that was not built (spawned, or unpacked
/// from an Exploit) is `morphable` 2 s after it appears.
const MORPHABLE_AFTER_SPAWN: f32 = 2.0;
/// A Bug leaving its factory: `sleep 3000` once built, then `sleep 2000`.
const MORPHABLE_AFTER_BUILD: f32 = 5.0;
/// exploit.bos `FireWeapon1`: `idle` is back 5 s after each shot.
const IDLE_AFTER_SHOT: f32 = 5.0;

/// A deploy order: when the unit may morph (`morphable` for a Bug,
/// `idle` for an Exploit) it does, and until then the order waits
/// (`while(!morphable) sleep 500`).
#[derive(Component, Clone, Copy, Debug)]
pub struct MorphGate {
    /// Seconds until the unit may morph.
    pub remaining: f32,
}

/// A unit in the middle of its deploy or pack-up script: `Deploy()`
/// first waits for its [`MorphGate`], then runs [`MORPH_SECS`] with the
/// unit held in place and silent, and only then swaps the unit.
#[derive(Component, Clone, Copy, Debug)]
pub struct Morphing {
    pub then: Option<QueuedCommand>,
    /// The gate was open and the sequence is running.
    pub started: bool,
    pub remaining: f32,
}

/// `deployed=1`: the unit neither aims nor fires (a Bug from the start
/// of the sequence, an Exploit from the order on).
#[derive(Component, Clone, Copy, Debug)]
pub struct MorphSilenced;

/// The morph sequence is over: swap the unit for its pair.
#[derive(Message, Debug, Clone, Copy)]
pub struct MorphDone {
    pub entity: Entity,
    pub then: Option<QueuedCommand>,
}

/// Give every new Bug and Exploit its gate: a Bug waits out
/// `morphable` (longer when it was just built), an Exploit starts
/// `idle`.
#[allow(clippy::type_complexity)]
pub fn init_morph_gates(
    new_units: Query<(Entity, &UnitType, Has<crate::units::spawning::Emerging>), Added<UnitType>>,
    mut commands: Commands,
) {
    for (entity, unit, being_built) in &new_units {
        let remaining = match unit.0 {
            crate::units::content::definitions::UnitKind::Bug if being_built => {
                MORPHABLE_AFTER_BUILD
            }
            crate::units::content::definitions::UnitKind::Bug => MORPHABLE_AFTER_SPAWN,
            crate::units::content::definitions::UnitKind::Exploit => 0.0,
            _ => continue,
        };
        commands.entity(entity).try_insert(MorphGate { remaining });
    }
}

/// Run the gates down (an unfinished unit's script hasn't got to
/// `morphable` yet), and push an Exploit's `idle` back 5 s on each shot.
#[allow(clippy::type_complexity)]
pub fn tick_morph_gates(
    time: Res<Time>,
    mut gates: Query<(
        &mut MorphGate,
        &UnitType,
        Has<crate::units::spawning::Emerging>,
    )>,
    fired: Query<
        Entity,
        (
            Changed<crate::units::combat::AttackCooldown>,
            Without<Stunned>,
            With<MorphGate>,
        ),
    >,
) {
    let dt = time.delta_secs();
    for (mut gate, _, being_built) in &mut gates {
        if !being_built {
            gate.remaining = (gate.remaining - dt).det_max(0.0);
        }
    }
    for entity in &fired {
        if let Ok((mut gate, unit, _)) = gates.get_mut(entity)
            && unit.0 == crate::units::content::definitions::UnitKind::Exploit
        {
            gate.remaining = IDLE_AFTER_SHOT;
        }
    }
}

/// A deploy order starts the unit's script, unless it already runs
/// (`if (deployed) return 0; //mutex`).
pub fn start_morph(
    mut events: MessageReader<DeployEvent>,
    units: Query<&UnitType, (Without<Dying>, Without<Morphing>)>,
    mut commands: Commands,
) {
    for event in events.read() {
        let Ok(unit) = units.get(event.entity) else {
            continue;
        };
        if unit.0.deploy_pair().is_none() {
            continue;
        }
        let mut ec = commands.entity(event.entity);
        ec.insert(Morphing {
            then: event.then,
            started: false,
            remaining: MORPH_SECS,
        });
        // An Exploit sets `deployed=1` before it waits for `idle`.
        if unit.0 == crate::units::content::definitions::UnitKind::Exploit {
            ec.insert(MorphSilenced);
        }
    }
}

/// Advance the scripts: wait for the gate, then hold the unit still and
/// silent for [`MORPH_SECS`] and hand it over to [`process_deploy`].
#[allow(clippy::type_complexity)]
pub fn tick_morph(
    time: Res<Time>,
    mut units: Query<(Entity, &mut Morphing, Option<&MorphGate>, &NetId), Without<Dying>>,
    mut animators: Query<&mut crate::units::assets::animation::UnitAnimator>,
    mut done: MessageWriter<MorphDone>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    // By NetId: the order of the `MorphDone`s is the order the pairs are
    // spawned in, so it fixes their NetIds.
    for (entity, mut morph, gate, _) in units.iter_mut().sort::<&NetId>() {
        if !morph.started {
            if gate.is_some_and(|g| g.remaining > 0.0) {
                continue;
            }
            morph.started = true;
            // `deployed=1`, `set MAX_SPEED to 1`.
            commands.entity(entity).try_insert(MorphSilenced);
            // The script's flip starts here.
            if let Ok(mut animator) = animators.get_mut(entity) {
                let crate::units::assets::animation::UnitAnimator { rig, driver, .. } =
                    &mut *animator;
                driver.morph(rig);
            }
        }
        // Pinned: whatever the player orders meanwhile does not move it.
        commands.entity(entity).remove::<(
            MoveTarget,
            MovePath,
            crate::units::combat::AttackTargetOrder,
            crate::units::combat::AimTarget,
        )>();
        morph.remaining -= dt;
        if morph.remaining <= 0.0 {
            done.write(MorphDone {
                entity,
                then: morph.then,
            });
            commands.entity(entity).remove::<Morphing>();
        }
    }
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

/// Finish a morph by despawning the source and spawning its pair with
/// proportional HP at the same position, facing the same way and with
/// the same queued orders (`LuaCOB.lua:34-54`). The pair starts with the
/// event's `then` order, if any, and keeps the player's selection
/// (`kp_keep_morphed_selected.lua`).
#[allow(clippy::type_complexity)]
pub fn process_deploy(
    mut events: MessageReader<MorphDone>,
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
            Option<&crate::units::combat::Infected>,
            Option<&crate::interaction::movement::CommandQueue>,
        ),
        Without<Dying>,
    >,
    mut virus_spawns: ResMut<crate::units::combat::VirusSpawnQueue>,
    mut ctx: SpawnContext,
) {
    for event in events.read() {
        let Ok((unit, faction, team, transform, health, selected, infected, queue)) =
            query.get(event.entity)
        else {
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
        // `UnitDestroyed` runs for the swapped-out unit too: an infected
        // Bug or Exploit releases its Virus (infection.lua).
        if let Some(infected) = infected {
            virus_spawns.push(crate::units::combat::VirusSpawn {
                position: transform.translation,
                faction: infected.attacker_faction,
                team: infected.attacker_team,
            });
        }

        let facing = transform.forward().xz();
        let spawned = crate::units::lifecycle::spawning::spawn_unit_facing(
            target_kind,
            *faction,
            team.0,
            transform.translation,
            crate::sim::Heading::from_vector(facing.x, facing.y),
            &mut ctx,
        );
        let mut ec = ctx.commands.entity(spawned);
        // `Spring.SetUnitBlocking(nu, false)` in `LuaCOB.lua`'s replace
        // queue: units walk through the morph's result.
        ec.insert((
            Health {
                current: new_current,
                max: new_max,
            },
            crate::interaction::ground_move::Unblocked::at(transform.translation),
        ));
        if selected {
            ec.insert(Selected);
        }
        if let Some(queue) = queue {
            ec.insert(crate::interaction::movement::CommandQueue {
                commands: queue.commands.clone(),
            });
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

    /// The deploy script waits for `morphable`, then holds the unit still
    /// and silent for 88 frames, and only then hands it over to the swap.
    #[test]
    fn a_morph_waits_for_its_gate_then_runs_its_sequence() {
        use crate::units::content::definitions::UnitKind;
        let mut world = World::new();
        world.init_resource::<Messages<DeployEvent>>();
        world.init_resource::<Messages<MorphDone>>();
        world.init_resource::<Time>();
        let bug = world
            .spawn((
                UnitType(UnitKind::Bug),
                MorphGate { remaining: 1.0 },
                MoveTarget(Vec3::X * 100.0),
            ))
            .id();
        world
            .resource_mut::<Messages<DeployEvent>>()
            .write(DeployEvent {
                entity: bug,
                then: None,
            });
        world.run_system_once(start_morph).unwrap();
        assert!(world.get::<Morphing>(bug).is_some());
        assert!(
            world.get::<MorphSilenced>(bug).is_none(),
            "a Bug still fires while it waits"
        );
        let step = |world: &mut World, secs: f32| {
            world
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(secs));
            world.run_system_once(tick_morph).unwrap();
        };
        // The gate is shut: nothing starts.
        step(&mut world, 0.5);
        assert!(!world.get::<Morphing>(bug).unwrap().started);
        // Open: the sequence starts, the Bug is pinned and silent.
        world.get_mut::<MorphGate>(bug).unwrap().remaining = 0.0;
        step(&mut world, 0.1);
        assert!(world.get::<Morphing>(bug).unwrap().started);
        assert!(world.get::<MorphSilenced>(bug).is_some());
        assert!(world.get::<MoveTarget>(bug).is_none());
        assert_eq!(world.resource::<Messages<MorphDone>>().len(), 0);
        // 88 frames later the swap is due.
        step(&mut world, MORPH_SECS);
        assert_eq!(world.resource::<Messages<MorphDone>>().len(), 1);
        assert!(world.get::<Morphing>(bug).is_none());
    }

    /// `morphable` is later for a Bug straight out of its factory, and an
    /// Exploit's `idle` returns 5 s after each shot.
    #[test]
    fn gates_follow_the_scripts() {
        use crate::units::content::definitions::UnitKind;
        let mut world = World::new();
        world.init_resource::<Time>();
        let built = world
            .spawn((
                UnitType(UnitKind::Bug),
                crate::units::spawning::Emerging {
                    target_y: 0.0,
                    remaining: 1.0,
                    total: 1.0,
                    rally_point: None,
                    rally_then: None,
                    style: crate::units::spawning::EmergeStyle::Fade,
                },
            ))
            .id();
        let loose = world.spawn(UnitType(UnitKind::Bug)).id();
        let exploit = world.spawn(UnitType(UnitKind::Exploit)).id();
        world.run_system_once(init_morph_gates).unwrap();
        let gate = |world: &World, e| world.get::<MorphGate>(e).unwrap().remaining;
        assert_eq!(gate(&world, built), 5.0);
        assert_eq!(gate(&world, loose), 2.0);
        assert_eq!(gate(&world, exploit), 0.0);
        // The built one's clock starts when it is finished.
        world
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1));
        world.run_system_once(tick_morph_gates).unwrap();
        assert_eq!(gate(&world, built), 5.0);
        assert_eq!(gate(&world, loose), 1.0);
    }
}

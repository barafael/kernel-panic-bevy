//! The command AI's engagement behaviour: idle units and fighting units
//! going after what is just out of reach.
//!
//! Spring units keep the default Maneuver move state (KP hides the
//! button), under which an idle unit — no order, no queue — whose
//! `CMobileCAI::SlowUpdate` finds a visible enemy within its weapon's
//! range plus 200 elmos gives itself a short attack order on it
//! (`MobileCAI.cpp:337-358`, `GenerateAttackCmd`, `:1196-1265`). The unit
//! chases it, but never further than the range plus 500 from where it
//! stood, and for no longer than 5 s per order (`:886-901`;
//! `MoveType.cpp:76`). The Pointer holds its ground (`pointer.bos:158`
//! sets `STANDINGFIREORDERS` to hold position).
//!
//! `combat_system` already fires at whatever is inside the range, so this
//! only supplies the approach: [`idle_engage_system`] gives the order,
//! [`engage_leash_system`] ends it when the leash or the clock runs out.

use bevy::prelude::*;

use super::{AttackTargetOrder, Dying, Stunned};
use crate::interaction::movement::{CommandQueue, MovePath, MoveTarget, OrderState};
use crate::net::NetId;
use crate::sim::{SimTick, slow_update_due};
use crate::units::combat::aim;
use crate::units::components::{TeamId, UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::WeaponRegistry;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::spatial::{SpatialIndex, flat_dist_sq};
use crate::units::teams::Alliances;

/// How far past its weapon range an idle unit looks for something to
/// attack (`MobileCAI::GenerateAttackCmd`'s `maxRange + 200`).
const ENGAGE_MARGIN: f32 = 200.0;
/// How far past its weapon range, from where it stood, it follows.
const LEASH_MARGIN: f32 = 500.0;
/// Seconds an order it gave itself lasts (`GAME_SPEED * 5` frames).
const ENGAGE_FRAMES: u64 = 5 * 30;

/// A unit that gave itself an attack order on `target` (see the module
/// docs). Its order is only this one while `AttackTargetOrder` still
/// names `target`; a new player order replaces both.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[component(storage = "SparseSet")]
pub struct Engaged {
    pub target: Entity,
    /// Where the unit stood when it picked the target.
    pub home: Vec3,
    /// The farthest it may be from `home` (weapon range + 500).
    pub leash: f32,
    /// The sim frame the order lapses.
    pub expires: u64,
}

/// The last enemy unit that damaged this one, and when
/// (`CUnit::lastAttacker` / `lastAttackFrame`): what a guard retaliates
/// against (`ExecuteGuard`: 40 frames) and what a unit's weapons prefer
/// (`GenerateWeaponTargets`' ×0.5 for the unit that hit the shooter).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct LastAttacker {
    pub entity: Entity,
    pub frame: u64,
}

/// `ExecuteGuard`'s window: an attacker of the guarded unit within this
/// many frames is fair game.
pub const GUARD_RETALIATION_FRAMES: u64 = 40;

/// What breaks a tie between two equally distant candidates: the
/// `NetId`, which is the same on every machine and in every run. (The
/// Bevy `Entity` is not: it follows the allocation order, so two peers,
/// a shuffled run and wasm picked different targets.)
fn tie_key(net_ids: &Query<&NetId>, entity: Entity) -> u32 {
    net_ids.get(entity).map_or(u32::MAX, |id| id.0)
}

/// Units that never engage on their own: the Pointer holds position.
fn holds_position(kind: UnitKind) -> bool {
    matches!(kind, UnitKind::Pointer)
}

/// `STANDINGFIREORDERS = 0` (`FIRESTATE_HOLDFIRE`): the one fire state
/// the game sets — worm.bos, cloaked with AutoHold on. A unit holding
/// fire neither walks off after a visible enemy while idle
/// (`CMobileCAI::SlowUpdate`'s `fireState >= FIREATWILL`) nor diverts
/// a Fight.
mod holds_fire {
    use crate::units::mechanics::cloak::Cloaked;
    use crate::units::mechanics::worm::AutoHold;
    use bevy::ecs::query::QueryData;
    use bevy::prelude::*;

    #[derive(QueryData)]
    pub struct HoldFire {
        autohold: Option<&'static AutoHold>,
        cloaked: Has<Cloaked>,
    }

    impl HoldFireItem<'_, '_> {
        pub fn holds(&self) -> bool {
            self.cloaked && self.autohold.is_some_and(|a| a.0)
        }
    }
}

/// Give every idle unit with a visible enemy just beyond its reach an
/// attack order on the nearest one, on the unit's own slow update.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn idle_engage_system(
    tick: Res<SimTick>,
    units: Query<
        (
            Entity,
            &UnitType,
            &UnitStats,
            &TeamId,
            &Transform,
            &NetId,
            OrderState,
            Option<&CommandQueue>,
            holds_fire::HoldFire,
        ),
        (
            Without<Dying>,
            Without<Stunned>,
            Without<Emerging>,
            Without<Engaged>,
        ),
    >,
    spatial: Res<SpatialIndex>,
    net_ids: Query<&NetId>,
    alliances: Res<Alliances>,
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    gravity: Option<Res<crate::sim::MapGravity>>,
    mut commands: Commands,
) {
    let frame = tick.frame();
    let map_gravity = gravity.as_deref().copied().unwrap_or_default().0;
    for (entity, unit, stats, team, tf, net, order, queue, hold) in &units {
        if !slow_update_due(frame, net.0)
            || hold.holds()
            || stats.speed <= 0.0
            || holds_position(unit.0)
            || order.active()
            || queue.is_some_and(|q| !q.commands.is_empty())
        {
            continue;
        }
        let Some(weapon) = unit_registry
            .weapon(unit.0)
            .and_then(|kind| weapon_registry.get(kind))
            .filter(|w| w.range != 0.0 && !w.command_fire)
        else {
            continue;
        };
        let pos = tf.translation;
        let aim_from = pos + Vec3::Y * stats.mid_y;
        let reach = weapon.effective_range();
        let mut nearest: Option<(f32, u32, Entity)> = None;
        spatial.query_radius(pos, reach + ENGAGE_MARGIN, |candidate| {
            if candidate.feature
                || !candidate.hp_positive
                || alliances.friendly(candidate.team, team.0)
                // Seen, not just a radar blip.
                || !candidate.targetable_by(team.0)
                || candidate.radar_only_for(team.0)
                || !unit_registry.auto_target_allowed(unit.0, candidate.kind)
            {
                return;
            }
            let d_sq = flat_dist_sq(candidate.pos, pos);
            if d_sq > (reach + ENGAGE_MARGIN) * (reach + ENGAGE_MARGIN) {
                return;
            }
            // Inside its reach the weapon fires by itself.
            if aim::in_weapon_range(weapon, aim_from, candidate.mid_pos(), false, map_gravity) {
                return;
            }
            let key = tie_key(&net_ids, candidate.entity);
            if nearest.is_none_or(|(best, best_key, _)| (d_sq, key) < (best, best_key)) {
                nearest = Some((d_sq, key, candidate.entity));
            }
        });
        let Some((_, _, target)) = nearest else {
            continue;
        };
        commands.entity(entity).insert((
            AttackTargetOrder { target },
            Engaged {
                target,
                home: pos,
                leash: reach + LEASH_MARGIN,
                expires: frame + ENGAGE_FRAMES,
            },
        ));
    }
}

/// End the self-given orders that went on too long or too far, and
/// forget the ones a player order replaced.
#[allow(clippy::type_complexity)]
pub fn engage_leash_system(
    tick: Res<SimTick>,
    engaged: Query<(Entity, &Engaged, &Transform, Option<&AttackTargetOrder>), Without<Dying>>,
    mut commands: Commands,
) {
    let frame = tick.frame();
    for (entity, engaged, tf, order) in &engaged {
        if order.is_none_or(|o| o.target != engaged.target) {
            commands.entity(entity).remove::<Engaged>();
            continue;
        }
        let away = flat_dist_sq(tf.translation, engaged.home);
        if frame >= engaged.expires || away > engaged.leash * engaged.leash {
            commands
                .entity(entity)
                .remove::<(AttackTargetOrder, Engaged, MoveTarget, MovePath)>();
        }
    }
}

/// How far past its weapon range a fighting unit looks for an enemy
/// worth leaving its line for (`ExecuteFight`'s `maxRange + 100`).
const FIGHT_MARGIN: f32 = 100.0;

/// The point a fighting unit was marching to when it turned aside for an
/// enemy: it goes back to it once the enemy is dealt with
/// (`CMobileCAI::ExecuteFight` keeps the fight command at the head of
/// the queue while its attack is the temporary front).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[component(storage = "SparseSet")]
pub struct FightGoal(pub Vec3);

/// A unit on a fight leg (attack-move, patrol) that has no enemy in
/// range but sees one within range + 100 leaves its line to attack the
/// nearest, on its own slow update.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn fight_divert_system(
    tick: Res<SimTick>,
    units: Query<
        (
            Entity,
            &UnitType,
            &UnitStats,
            &TeamId,
            &Transform,
            &NetId,
            &MoveTarget,
            holds_fire::HoldFire,
        ),
        (
            With<crate::interaction::movement::AttackMoveActive>,
            Without<Dying>,
            Without<Stunned>,
            Without<Emerging>,
            Without<AttackTargetOrder>,
            Without<FightGoal>,
        ),
    >,
    spatial: Res<SpatialIndex>,
    net_ids: Query<&NetId>,
    alliances: Res<Alliances>,
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    gravity: Option<Res<crate::sim::MapGravity>>,
    mut commands: Commands,
) {
    let frame = tick.frame();
    let map_gravity = gravity.as_deref().copied().unwrap_or_default().0;
    for (entity, unit, stats, team, tf, net, goal, hold) in &units {
        if !slow_update_due(frame, net.0) || stats.speed <= 0.0 || hold.holds() {
            continue;
        }
        let Some(weapon) = unit_registry
            .weapon(unit.0)
            .and_then(|kind| weapon_registry.get(kind))
            .filter(|w| w.range != 0.0 && !w.command_fire)
        else {
            continue;
        };
        let pos = tf.translation;
        let aim_from = pos + Vec3::Y * stats.mid_y;
        let reach = weapon.effective_range() + FIGHT_MARGIN;
        let mut nearest: Option<(f32, u32, Entity)> = None;
        let mut in_reach = false;
        spatial.query_radius(pos, reach, |candidate| {
            if candidate.feature
                || !candidate.hp_positive
                || alliances.friendly(candidate.team, team.0)
                || !candidate.targetable_by(team.0)
                || candidate.radar_only_for(team.0)
                || !unit_registry.auto_target_allowed(unit.0, candidate.kind)
            {
                return;
            }
            if aim::in_weapon_range(weapon, aim_from, candidate.mid_pos(), false, map_gravity) {
                // The weapon has it: the fight halts and shoots.
                in_reach = true;
                return;
            }
            let d_sq = flat_dist_sq(candidate.pos, pos);
            if d_sq <= reach * reach {
                let key = tie_key(&net_ids, candidate.entity);
                if nearest.is_none_or(|(best, best_key, _)| (d_sq, key) < (best, best_key)) {
                    nearest = Some((d_sq, key, candidate.entity));
                }
            }
        });
        let Some((_, _, target)) = nearest.filter(|_| !in_reach) else {
            continue;
        };
        commands
            .entity(entity)
            .insert((AttackTargetOrder { target }, FightGoal(goal.0)));
    }
}

/// Back to the line: a fighting unit whose detour attack is over
/// resumes its leg.
#[allow(clippy::type_complexity)]
pub fn fight_resume_system(
    units: Query<(Entity, &FightGoal), (Without<AttackTargetOrder>, Without<Dying>)>,
    mut commands: Commands,
) {
    for (entity, goal) in &units {
        commands
            .entity(entity)
            .insert(MoveTarget(goal.0))
            .remove::<(FightGoal, MovePath)>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::spatial::SpatialEntry;
    use bevy::ecs::system::RunSystemOnce;

    struct Rig {
        world: World,
        reach: f32,
    }

    fn entry(entity: Entity, x: f32) -> SpatialEntry {
        SpatialEntry {
            entity,
            pos: Vec3::new(x, 0.0, 0.0),
            hit_radius: 0.0,
            mid_y: 0.0,
            up: Vec3::Y,
            team: 1,
            kind: UnitKind::Bit,
            hp_positive: true,
            health: 1000.0,
            armored: false,
            is_flying: false,
            cloaked: false,
            detected_by: 0,
            los_by: u64::MAX,
            feature: false,
        }
    }

    /// A team-0 `kind` at the origin and a team-1 Bit `enemy_x` away.
    fn rig(kind: UnitKind, enemy_x: f32) -> (Rig, Entity, Entity) {
        let registry = UnitRegistry::load();
        let weapons = WeaponRegistry::load();
        let reach = weapons
            .get(registry.weapon(kind).expect("armed"))
            .expect("weapon")
            .effective_range();
        let mut world = World::new();
        let unit = world
            .spawn((
                UnitType(kind),
                UnitStats::from_registry(kind, &registry, 10.0, 0.0),
                TeamId(0),
                Transform::default(),
            ))
            .id();
        let enemy = world.spawn_empty().id();
        let mut spatial = SpatialIndex::default();
        spatial.insert_for_test(entry(enemy, enemy_x));
        world.insert_resource(spatial);
        world.insert_resource(Alliances::from_seats([(0, 0), (1, 1)]));
        world.insert_resource(registry);
        world.insert_resource(weapons);
        let id = world.get::<NetId>(unit).unwrap().0;
        world.insert_resource(SimTick(crate::sim::next_slow_update(0, id)));
        (Rig { world, reach }, unit, enemy)
    }

    /// An idle unit goes after a visible enemy just beyond its reach
    /// (and not one far away, one in reach, or when it is a Pointer).
    #[test]
    fn idle_units_engage_what_is_just_out_of_reach() {
        let (mut rig, unit, enemy) = rig(UnitKind::Bit, 0.0);
        let reach = rig.reach;
        let mut spatial = SpatialIndex::default();
        let near = rig.world.spawn_empty().id();
        spatial.insert_for_test(entry(near, reach + 100.0));
        rig.world.insert_resource(spatial);
        let _ = enemy;
        rig.world.run_system_once(idle_engage_system).unwrap();
        let order = rig.world.get::<AttackTargetOrder>(unit).expect("engaged");
        assert_eq!(order.target, near);
        let engaged = rig.world.get::<Engaged>(unit).unwrap();
        assert_eq!(engaged.leash, reach + LEASH_MARGIN);

        let (mut rig, unit, _) = rig_at(UnitKind::Bit, 100_000.0);
        rig.world.run_system_once(idle_engage_system).unwrap();
        assert!(
            rig.world.get::<AttackTargetOrder>(unit).is_none(),
            "too far"
        );

        let (mut rig, unit, _) = rig_at(UnitKind::Bit, 20.0);
        rig.world.run_system_once(idle_engage_system).unwrap();
        assert!(
            rig.world.get::<AttackTargetOrder>(unit).is_none(),
            "inside reach the weapon fires by itself"
        );
    }

    /// A cloaked Worm with AutoHold on (`STANDINGFIREORDERS = 0`) stays
    /// put; with AutoHold off, or once it surfaced, it engages.
    #[test]
    fn a_worm_holding_fire_does_not_engage() {
        use crate::units::mechanics::cloak::Cloaked;
        use crate::units::mechanics::worm::AutoHold;
        let weapons = WeaponRegistry::load();
        let registry = UnitRegistry::load();
        let reach = weapons
            .get(registry.weapon(UnitKind::Worm).expect("armed"))
            .expect("weapon")
            .effective_range();
        let gap = reach + 100.0;
        for (autohold, cloaked, engages) in [
            (true, true, false),
            (false, true, true),
            (true, false, true),
        ] {
            let (mut rig, unit, _) = rig(UnitKind::Worm, gap);
            rig.world.entity_mut(unit).insert(AutoHold(autohold));
            if cloaked {
                rig.world.entity_mut(unit).insert(Cloaked);
            }
            rig.world.run_system_once(idle_engage_system).unwrap();
            assert_eq!(
                rig.world.get::<AttackTargetOrder>(unit).is_some(),
                engages,
                "autohold {autohold}, cloaked {cloaked}"
            );
        }
    }

    fn rig_at(kind: UnitKind, enemy_x: f32) -> (Rig, Entity, Entity) {
        rig(kind, enemy_x)
    }

    /// The leash ends a self-given order after 5 s or past the leash,
    /// and a player's order replacing it clears the bookkeeping only.
    #[test]
    fn the_leash_ends_a_self_given_attack() {
        let (mut rig, unit, enemy) = rig(UnitKind::Bit, 500.0);
        rig.world.entity_mut(unit).insert((
            AttackTargetOrder { target: enemy },
            Engaged {
                target: enemy,
                home: Vec3::ZERO,
                leash: 800.0,
                expires: 100,
            },
        ));
        rig.world.insert_resource(SimTick(50));
        rig.world.run_system_once(engage_leash_system).unwrap();
        assert!(rig.world.get::<AttackTargetOrder>(unit).is_some());
        rig.world.insert_resource(SimTick(100));
        rig.world.run_system_once(engage_leash_system).unwrap();
        assert!(rig.world.get::<AttackTargetOrder>(unit).is_none());
        assert!(rig.world.get::<Engaged>(unit).is_none());

        // A new order names another target: only the marker goes.
        let other = rig.world.spawn_empty().id();
        rig.world.entity_mut(unit).insert((
            AttackTargetOrder { target: other },
            Engaged {
                target: enemy,
                home: Vec3::ZERO,
                leash: 800.0,
                expires: 1000,
            },
        ));
        rig.world.run_system_once(engage_leash_system).unwrap();
        assert!(rig.world.get::<AttackTargetOrder>(unit).is_some());
        assert!(rig.world.get::<Engaged>(unit).is_none());
    }

    /// A fight leg that diverted comes back to its line.
    #[test]
    fn a_fight_detour_resumes_the_leg() {
        let mut world = World::new();
        let unit = world.spawn(FightGoal(Vec3::new(900.0, 0.0, 0.0))).id();
        world.run_system_once(fight_resume_system).unwrap();
        assert_eq!(
            world.get::<MoveTarget>(unit).map(|m| m.0),
            Some(Vec3::new(900.0, 0.0, 0.0))
        );
        assert!(world.get::<FightGoal>(unit).is_none());
        // While the detour attack lasts, nothing resumes.
        let busy = world
            .spawn((
                FightGoal(Vec3::X),
                AttackTargetOrder {
                    target: Entity::PLACEHOLDER,
                },
            ))
            .id();
        world.run_system_once(fight_resume_system).unwrap();
        assert!(world.get::<FightGoal>(busy).is_some());
    }
}

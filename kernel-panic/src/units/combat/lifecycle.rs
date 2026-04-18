//! Unit lifecycle: stun, kamikaze trigger, death detection/teardown,
//! auto-heal.
//!
//! Each system reacts to a different terminal state: [`tick_stun`] decays
//! the [`Stunned`] marker + [`StunCharge`] pool; [`tick_kamikaze`] fires
//! proximity bombs into the damage queue; [`death_system`] watches for
//! zero-HP units and promotes them to [`Dying`]; [`cleanup_dying`]
//! despawns once the death anim finishes; [`auto_heal`] regenerates
//! idle units.

use bevy::prelude::*;

use super::super::animation::CobAnimator;
use super::super::components::{Faction, Health, TeamId, UnitType};
use super::super::spatial::SpatialIndex;
use super::super::unit_registry::UnitRegistry;
use super::super::weapons::WeaponRegistry;
use super::damage::{DamageQueue, Infected, PendingDamage, VirusSpawn, VirusSpawnQueue};
use super::{AimTarget, IdleTimer, StunCharge};
use crate::interaction::movement::{MovePath, MoveTarget};

/// Marks a unit that has reached 0 HP and is playing its death animation.
/// The entity will be despawned once the animation finishes or the timer expires.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct Dying {
    pub timer: f32,
}

/// Maximum time to wait for a death animation before force-despawning (seconds).
const DEATH_ANIM_TIMEOUT: f32 = 2.0;

/// Marks a unit as paralyzed: the combat and movement systems treat it
/// as inert until `remaining` elapses.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct Stunned {
    pub remaining: f32,
}

/// How many seconds it takes for accumulated stun charge to fully
/// dissipate if no further paralyzer damage lands.
const STUN_CHARGE_DECAY: f32 = 4.0;

/// Spring encodes FBI `IdleTime` in sim frames at 30 fps; convert to
/// seconds so we can compare against a `Time`-driven timer.
const IDLE_FRAMES_PER_SECOND: f32 = 30.0;

/// Regenerate HP on units that have been idle long enough.
/// A unit counts as idle when it has no move order and no current aim
/// target. The idle timer is reset in `apply_damage` whenever the unit
/// takes damage. Units whose FBI lacks `IdleAutoHeal` (value 0) opt out.
#[allow(clippy::type_complexity)]
pub fn auto_heal(
    time: Res<Time>,
    unit_registry: Res<UnitRegistry>,
    mut query: Query<
        (
            &UnitType,
            &mut Health,
            &mut IdleTimer,
            Option<&MoveTarget>,
            Option<&MovePath>,
            Option<&AimTarget>,
        ),
        Without<Dying>,
    >,
) {
    let dt = time.delta_secs();
    for (unit, mut health, mut idle, move_target, move_path, aim) in &mut query {
        let heal_rate = unit_registry.idle_auto_heal(unit.0);
        if heal_rate <= 0.0 {
            continue;
        }

        let is_active = move_target.is_some() || move_path.is_some() || aim.is_some();
        if is_active {
            idle.0 = 0.0;
            continue;
        }

        idle.0 += dt;
        let threshold = unit_registry.idle_time(unit.0) / IDLE_FRAMES_PER_SECOND;
        if idle.0 >= threshold && health.current < health.max {
            health.current = (health.current + heal_rate * dt).min(health.max);
        }
    }
}

/// Tick the `Stunned` timer. When it expires, remove the marker
/// and zero out accumulated stun charge so the unit isn't re-stunned on
/// the next DOS hit.
pub fn tick_stun(
    time: Res<Time>,
    mut stunned_q: Query<(Entity, &mut Stunned, Option<&mut StunCharge>), With<Stunned>>,
    mut charge_q: Query<&mut StunCharge, Without<Stunned>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut stun, charge) in &mut stunned_q {
        stun.remaining -= dt;
        if stun.remaining <= 0.0 {
            commands.entity(entity).remove::<Stunned>();
            if let Some(mut charge) = charge {
                charge.0 = 0.0;
            }
        }
    }

    // Decay stun charge on unstunned units so a few scattered DOS pings
    // don't add up to a lockdown hours later.
    let decay_per_sec = 1.0 / STUN_CHARGE_DECAY;
    for mut charge in &mut charge_q {
        if charge.0 > 0.0 {
            charge.0 = (charge.0 - charge.0 * decay_per_sec * dt).max(0.0);
        }
    }
}

/// Trigger kamikaze units (Logic Bombs) when any enemy enters
/// their proximity radius. The bomb queues its ExplodeAs weapon as a
/// self-damage event and forces its own HP to zero; `death_system` +
/// `apply_damage` handle the splash and the corpse teardown.
#[allow(clippy::type_complexity)]
pub fn tick_kamikaze(
    unit_registry: Res<UnitRegistry>,
    bombs: Query<(Entity, &UnitType, &TeamId, &Faction, &GlobalTransform), Without<Dying>>,
    mut health_q: Query<&mut Health>,
    spatial: Res<SpatialIndex>,
    mut damage_queue: ResMut<DamageQueue>,
) {
    for (entity, unit, team, faction, gtf) in &bombs {
        let trigger_radius = unit_registry.kamikaze_distance(unit.0);
        if trigger_radius <= 0.0 {
            continue;
        }
        let trigger_sq = trigger_radius * trigger_radius;
        let self_pos = gtf.translation();
        let mut triggered = false;
        spatial.query_radius(self_pos, trigger_radius, |candidate| {
            if triggered || !candidate.hp_positive {
                return;
            }
            let enemy = candidate.team != team.0 && candidate.faction != *faction;
            if enemy && candidate.pos.distance_squared(self_pos) <= trigger_sq {
                triggered = true;
            }
        });
        if !triggered {
            continue;
        }

        damage_queue.push(PendingDamage {
            target: entity,
            attacker: entity,
            weapon: "logic_bomb".to_string(),
            impact_pos: self_pos,
            attacker_distance: 0.0,
        });
        if let Ok(mut health) = health_q.get_mut(entity) {
            health.current = 0.0;
        }
    }
}

/// When a unit reaches 0 HP, start the Killed() COB script and mark it
/// as `Dying`. If the unit was infected, queue a Virus spawn. The unit's
/// FBI `ExplodeAs` weapon (RetroDeath, RetroDeathBig, VirusDeath, …) is
/// queued as a self-hit at the corpse position so its AoE splash + per-
/// weapon infection window can chain through nearby units — this is what
/// spreads the Virus outbreak, flattens the crowd around a dying Byte,
/// and gives big units their signature death boom.
#[allow(clippy::type_complexity)]
pub fn death_system(
    query: Query<
        (
            Entity,
            &UnitType,
            &Health,
            &GlobalTransform,
            Option<&Infected>,
        ),
        (Without<Dying>, Changed<Health>),
    >,
    mut animators: Query<&mut CobAnimator>,
    mut virus_spawns: ResMut<VirusSpawnQueue>,
    mut damage_queue: ResMut<DamageQueue>,
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
) {
    for (entity, unit, health, gtf, infected) in &query {
        if health.current <= 0.0 {
            // Start the COB Killed() callback if the unit has an animator.
            if let Ok(mut animator) = animators.get_mut(entity) {
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Killed", &[0, 0]);
            }

            // If the dying unit was infected, queue a Virus spawn for the
            // attacker's team at the death location.
            if let Some(infected) = infected {
                virus_spawns.push(VirusSpawn {
                    position: gtf.translation(),
                    faction: infected.attacker_faction,
                    team: infected.attacker_team,
                });
            }

            // Fire the FBI `ExplodeAs` weapon as a self-hit at the corpse.
            // Virus's own ExplodeAs=VirusDeath drives the infection chain;
            // Bit/Byte's RetroDeath/RetroDeathBig give big units their
            // death-AoE. Skip when the named weapon isn't in the registry
            // so we don't warn-spam on missing explosion TDFs.
            if let Some(weapon_name) = unit_registry
                .def(unit.0)
                .map(|d| d.explode_as.as_str())
                .filter(|s| !s.is_empty() && weapon_registry.get(s).is_some())
            {
                damage_queue.push(PendingDamage {
                    target: entity,
                    attacker: entity,
                    weapon: weapon_name.to_string(),
                    impact_pos: gtf.translation(),
                    attacker_distance: 0.0,
                });
            }

            commands.entity(entity).remove::<Infected>().insert(Dying {
                timer: DEATH_ANIM_TIMEOUT,
            });
        }
    }
}

/// Despawn dying units once their death animation finishes or the
/// timeout expires.
pub fn cleanup_dying(
    time: Res<Time>,
    mut query: Query<(Entity, &mut Dying, Option<&CobAnimator>)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut dying, animator) in &mut query {
        dying.timer -= dt;

        let anim_done = animator.is_none_or(|a| !a.vm.has_active_threads());
        if anim_done || dying.timer <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

use bevy::prelude::*;

use super::animation::CobAnimator;
use super::components::{Faction, Health, UnitType};
use super::definitions::stats;
use super::weapon_fx::{AttackEvent, PendingAttacks};
use super::weapons::WeaponRegistry;

/// Tracks time until the unit can fire again.
#[derive(Component)]
pub struct AttackCooldown {
    pub remaining: f32,
}

/// Marks a unit that has reached 0 HP and is playing its death animation.
/// The entity will be despawned once the animation finishes or the timer expires.
#[derive(Component)]
pub struct Dying {
    pub timer: f32,
}

/// Maximum time to wait for a death animation before force-despawning (seconds).
const DEATH_ANIM_TIMEOUT: f32 = 2.0;

/// Pending damage to apply after combat resolution.
#[derive(Resource, Default)]
pub struct DamageQueue(Vec<(Entity, f32)>);

/// System: armed units auto-attack the nearest enemy in range.
pub fn combat_system(
    time: Res<Time>,
    mut cooldowns: Query<&mut AttackCooldown>,
    attackers: Query<(Entity, &UnitType, &Faction, &GlobalTransform), Without<Dying>>,
    potential_targets: Query<(Entity, &Faction, &GlobalTransform, &Health), With<UnitType>>,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    weapon_registry: Res<WeaponRegistry>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    let dt = time.delta_secs();

    // Tick cooldowns.
    for mut cd in &mut cooldowns {
        cd.remaining = (cd.remaining - dt).max(0.0);
    }

    damage_queue.0.clear();

    for (entity, unit_type, attacker_faction, attacker_gtf) in &attackers {
        let unit_stats = stats(unit_type.0);

        // Resolve weapon stats from the TDF registry, falling back to hardcoded values.
        let weapon_def = if unit_stats.weapon.is_empty() {
            None
        } else {
            weapon_registry.get(unit_stats.weapon)
        };
        let range = weapon_def.map_or(unit_stats.attack_range, |w| w.range);
        let damage = weapon_def.map_or(unit_stats.attack_damage, |w| w.damage.default);
        let cooldown = weapon_def.map_or(unit_stats.attack_cooldown, |w| w.reload_time);

        if range == 0.0 {
            continue;
        }

        // Skip if still on cooldown.
        if let Ok(cd) = cooldowns.get(entity) {
            if cd.remaining > 0.0 {
                continue;
            }
        }

        let attacker_pos = attacker_gtf.translation();
        let range_sq = range * range;

        // Find nearest living enemy in range.
        let mut best: Option<(Entity, Vec3, f32)> = None;
        for (target_entity, target_faction, target_gtf, target_health) in &potential_targets {
            if target_faction == attacker_faction || target_health.current <= 0.0 {
                continue;
            }
            let target_pos = target_gtf.translation();
            let dist_sq = attacker_pos.distance_squared(target_pos);
            if dist_sq <= range_sq && best.map_or(true, |(_, _, d)| dist_sq < d) {
                best = Some((target_entity, target_pos, dist_sq));
            }
        }

        if let Some((target_entity, target_pos, _)) = best {
            damage_queue.0.push((target_entity, damage));
            commands.entity(entity).insert(AttackCooldown {
                remaining: cooldown,
            });
            if !unit_stats.weapon.is_empty() {
                pending_attacks.events.push(AttackEvent {
                    attacker_pos,
                    target_pos,
                    weapon_name: unit_stats.weapon,
                });
            }
        }
    }
}

/// System: apply queued damage from combat.
pub fn apply_damage(mut damage_queue: ResMut<DamageQueue>, mut health_q: Query<&mut Health>) {
    for &(target, damage) in &damage_queue.0 {
        if let Ok(mut health) = health_q.get_mut(target) {
            health.current -= damage;
        }
    }
    damage_queue.0.clear();
}

/// System: when a unit reaches 0 HP, start the Killed() COB script and mark it
/// as `Dying` instead of despawning immediately.
pub fn death_system(
    query: Query<(Entity, &Health), (With<UnitType>, Without<Dying>)>,
    mut animators: Query<&mut CobAnimator>,
    mut commands: Commands,
) {
    for (entity, health) in &query {
        if health.current <= 0.0 {
            // Start the COB Killed() callback if the unit has an animator.
            if let Ok(mut animator) = animators.get_mut(entity) {
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Killed", &[0, 0]);
            }
            commands.entity(entity).insert(Dying {
                timer: DEATH_ANIM_TIMEOUT,
            });
        }
    }
}

/// System: despawn dying units once their death animation finishes or the
/// timeout expires.
pub fn cleanup_dying(
    time: Res<Time>,
    mut query: Query<(Entity, &mut Dying, Option<&CobAnimator>)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut dying, animator) in &mut query {
        dying.timer -= dt;

        let anim_done = animator.map_or(true, |a| !a.vm.has_active_threads());
        if anim_done || dying.timer <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

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

use super::damage::{DamageQueue, Infected, PendingDamage, VirusSpawn, VirusSpawnQueue};
use super::{AimTarget, IdleTimer, StunCharge};
use crate::interaction::movement::{MovePath, MoveTarget};
use crate::units::assets::animation::CobAnimator;
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::WeaponRegistry;
use crate::units::spatial::SpatialIndex;
use crate::units::weapon_fx::{ExplosionEvent, PendingExplosions};

/// Marks a unit that has reached 0 HP and is playing its death animation.
/// The entity will be despawned once the animation finishes or the timer expires.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct Dying {
    pub timer: f32,
}

/// Maximum time to wait for a death animation before force-despawning (seconds).
const DEATH_ANIM_TIMEOUT: f32 = 2.0;

/// Countdown placed on a unit that the player ordered to self-destroy
/// via the `Ctrl+D` hotkey (FEATURES.md §3). When `remaining` hits zero
/// [`tick_self_destruct`] drops the unit's HP to zero so the existing
/// death/explode pipeline (incl. `ExplodeAs`) handles the blast.
/// `Stop` removes the component to abort the countdown.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct SelfDestructCountdown {
    pub remaining: f32,
}

/// Seconds between ordering self-destruct and detonation. Matches the
/// 5 s number called out in FEATURES.md §3 — long enough to walk the
/// unit away from allies, short enough that the player doesn't feel
/// the command is lagging.
pub const SELF_DESTRUCT_DELAY: f32 = 5.0;

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
            target: Some(entity),
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
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn death_system(
    query: Query<
        (
            Entity,
            &UnitType,
            &Health,
            &Faction,
            &GlobalTransform,
            Option<&Infected>,
        ),
        (Without<Dying>, Changed<Health>),
    >,
    mut animators: Query<&mut CobAnimator>,
    mut virus_spawns: ResMut<VirusSpawnQueue>,
    mut damage_queue: ResMut<DamageQueue>,
    mut explosions: ResMut<PendingExplosions>,
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
) {
    for (entity, unit, health, faction, gtf, infected) in &query {
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
            //
            // We also push a matching visual explosion so the big-unit
            // boom actually reads on screen — the per-piece `Explode`
            // particles from the COB Killed() script only draw a few
            // little pops at the pieces themselves, not the wide fireball
            // the upstream CEG would render. Scaled by the ExplodeAs
            // weapon's `area_of_effect` and tinted with the weapon's
            // own `rgb_color` so Virus / Logic Bomb / RetroDeathBig each
            // read differently. Fall back to faction colour for weapons
            // without a configured colour so the ring still pops.
            let pos = gtf.translation();
            if let Some(weapon_name) = unit_registry
                .def(unit.0)
                .map(|d| d.explode_as.as_str())
                .filter(|s| !s.is_empty() && weapon_registry.get(s).is_some())
            {
                damage_queue.push(PendingDamage {
                    target: Some(entity),
                    attacker: entity,
                    weapon: weapon_name.to_string(),
                    impact_pos: pos,
                    attacker_distance: 0.0,
                });

                let weapon = weapon_registry.get(weapon_name);
                let radius = weapon.map_or(24.0, |w| w.area_of_effect.max(24.0));
                let rgb = weapon
                    .map(|w| w.rgb_color)
                    .filter(|c| c[0] + c[1] + c[2] > 0.01)
                    .unwrap_or_else(|| faction.rgb_f32());
                let ceg_name = weapon
                    .map(|w| w.explosion_generator.clone())
                    .unwrap_or_default();
                explosions.events.push(ExplosionEvent {
                    pos,
                    rgb,
                    radius,
                    ceg_name,
                });
            } else {
                // Even units without an ExplodeAs get a small faction-
                // coloured pop — otherwise Bits and Packets vanish
                // silently, which reads as a bug.
                explosions.events.push(ExplosionEvent {
                    pos,
                    rgb: faction.rgb_f32(),
                    radius: 16.0,
                    ceg_name: String::new(),
                });
            }

            commands.entity(entity).remove::<Infected>().insert(Dying {
                timer: DEATH_ANIM_TIMEOUT,
            });
        }
    }
}

/// Tick every [`SelfDestructCountdown`] and, when it reaches zero,
/// drop the unit's HP to zero so `death_system` takes over — the
/// existing `ExplodeAs` pipeline spawns the death AoE, so this stays
/// a tiny bridge instead of a parallel detonation path.
pub fn tick_self_destruct(
    time: Res<Time>,
    mut query: Query<(&mut SelfDestructCountdown, &mut Health), Without<Dying>>,
) {
    let dt = time.delta_secs();
    for (mut countdown, mut health) in &mut query {
        countdown.remaining -= dt;
        if countdown.remaining <= 0.0 {
            health.current = 0.0;
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

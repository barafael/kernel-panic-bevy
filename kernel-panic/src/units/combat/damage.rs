//! Damage application, splash falloff, burst follow-ups, and infection.
//!
//! Combat system queues hits into [`DamageQueue`]; [`apply_damage`] drains
//! the queue, resolves primary-hit gating against each target's volumetric
//! hit radius, fans out AoE via [`splash_falloff`], and tags infected
//! targets with [`Infected`]. Burst-fire follow-ups live in
//! [`tick_burst_fire`] which pushes additional [`PendingDamage`] at
//! `burst_rate` intervals after the initial shot.

use bevy::prelude::*;

use super::{Dying, IdleTimer, StunCharge, Stunned, muzzle_world_pos};
use crate::units::assets::animation::{CobAnimator, MuzzlePiece};
use crate::units::components::{Faction, Health, TeamId, UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::WeaponRegistry;
use crate::units::lifecycle::script_triggers::JustFired;
use crate::units::spatial::SpatialIndex;
use crate::units::weapon_fx::{AttackEvent, PendingAttacks};

/// A pending damage event. Damage is resolved at apply-time so the
/// target's armor class can pick the right entry from the weapon's
/// `[DAMAGE]` table. The primary target always takes full damage; if
/// the weapon has `area_of_effect > 0`, other units within that radius
/// of `impact_pos` also take damage with linear falloff from the weapon's
/// `edge_effectiveness`.
#[derive(Debug, Clone)]
pub struct PendingDamage {
    pub target: Entity,
    pub attacker: Entity,
    pub weapon: String,
    pub impact_pos: Vec3,
    /// Distance from attacker to primary target at the moment the hit
    /// was queued. Used by dynamic-damage weapons (BugCannon) to scale
    /// the primary hit; zero is fine for single-range weapons.
    pub attacker_distance: f32,
}

/// Pending damage to apply after combat resolution.
#[derive(Resource, Default)]
pub struct DamageQueue(Vec<PendingDamage>);

impl DamageQueue {
    pub fn push(&mut self, damage: PendingDamage) {
        self.0.push(damage);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, PendingDamage> {
        self.0.drain(..)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// In-progress burst fire. Weapons with `burst > 1` fire the first shot
/// through the normal combat path and attach this component for the
/// remaining shots, which are released at `interval` spacing by
/// [`tick_burst_fire`]. The aim point is frozen at trigger time so the
/// whole burst lands on the same spot regardless of target motion.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct BurstFire {
    pub shots_remaining: u32,
    pub interval: f32,
    pub timer: f32,
    pub target: Entity,
    pub target_pos: Vec3,
    pub weapon: String,
    pub arc_height: f32,
}

/// Marks a unit as infected by a Worm or Virus attack. If the unit dies
/// while this component is present, a Virus spawns at the death location
/// for the attacker's team.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct Infected {
    /// Remaining seconds before the infection expires.
    pub timer: f32,
    /// The faction that will own the spawned Virus.
    pub attacker_faction: Faction,
    /// The team ID that will own the spawned Virus.
    pub attacker_team: u8,
}

/// How long (seconds) a Worm/Virus infection lasts before expiring,
/// when the triggering weapon has no entry in
/// [`weapon_infection_duration`]. The per-weapon map is the source of
/// truth; this is a fallback for programmatic infections (e.g. the
/// area-denial Infection gas spawning Viruses).
pub const INFECTION_DURATION: f32 = 6.0;

/// Per-weapon infection window in seconds. Mirrors upstream
/// `LuaRules/Gadgets/infection.lua`, which expresses the window in sim
/// frames at 30 fps. Keys match weapon TDF section names as-authored
/// (the TDF parser preserves case for section names even though it
/// lowercases inner keys). Returns `None` for weapons that don't infect.
pub fn weapon_infection_duration(weapon: &str) -> Option<f32> {
    let frames = match weapon {
        "VirusBeam" => 90.0,
        "VirusDeath" => 180.0,
        "Wormsplash" => 200.0,
        "Infection" => 30.0,
        _ => return None,
    };
    Some(frames / 30.0)
}

/// Queued virus spawns from infected unit deaths.
#[derive(Debug, Clone, Copy)]
pub struct VirusSpawn {
    pub position: Vec3,
    pub faction: Faction,
    pub team: u8,
}

#[derive(Resource, Default)]
pub struct VirusSpawnQueue(Vec<VirusSpawn>);

impl VirusSpawnQueue {
    pub fn push(&mut self, spawn: VirusSpawn) {
        self.0.push(spawn);
    }

    pub fn drain(&mut self) -> std::vec::Drain<'_, VirusSpawn> {
        self.0.drain(..)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Minimum `area_of_effect` (elmos) at which a weapon triggers a splash
/// pass. Upstream weapons use tiny AoE values (8/16/32) for impact effects
/// on single-target weapons; only lob/explosive weapons set AoE high
/// enough to hit multiple units. This threshold avoids doing an O(n)
/// position scan for every Bit shot.
const AOE_SPLASH_THRESHOLD: f32 = 48.0;

/// Linear splash falloff. `dist` is the distance from the impact point;
/// `radius` is the weapon's `area_of_effect`; `edge_mult` is the weapon's
/// `edge_effectiveness` (1.0 = full damage at the edge, 0.0 = no damage
/// at the edge). Callers must ensure `dist < radius`.
pub(super) fn splash_falloff(dist: f32, radius: f32, edge_mult: f32) -> f32 {
    let t = (dist / radius).clamp(0.0, 1.0);
    1.0 - t * (1.0 - edge_mult)
}

/// Release follow-up shots for units in the middle of a burst.
/// The initial shot fires through the regular combat path; each follow-up
/// queues another damage event and weapon-FX event at `burst_rate` spacing
/// until `shots_remaining` hits zero, then removes the component.
pub fn tick_burst_fire(
    time: Res<Time>,
    mut query: Query<(Entity, &mut BurstFire, &GlobalTransform), Without<Dying>>,
    muzzle_q: Query<&MuzzlePiece>,
    animator_q: Query<&CobAnimator>,
    piece_gtf_q: Query<&GlobalTransform, Without<UnitType>>,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    let dt = time.delta_secs();
    for (entity, mut burst, gtf) in &mut query {
        burst.timer -= dt;
        if burst.timer > 0.0 {
            continue;
        }

        damage_queue.push(PendingDamage {
            target: burst.target,
            attacker: entity,
            weapon: burst.weapon.clone(),
            impact_pos: burst.target_pos,
            attacker_distance: gtf.translation().distance(burst.target_pos),
        });
        let visual_origin = muzzle_world_pos(entity, gtf, &muzzle_q, &animator_q, &piece_gtf_q);
        pending_attacks.events.push(AttackEvent {
            attacker_pos: visual_origin,
            target_pos: burst.target_pos,
            weapon_name: std::borrow::Cow::Owned(burst.weapon.clone()),
        });
        commands.entity(entity).insert(JustFired {
            target_pos: burst.target_pos,
            arc_height: burst.arc_height,
        });

        burst.shots_remaining -= 1;
        if burst.shots_remaining == 0 {
            commands.entity(entity).remove::<BurstFire>();
        } else {
            burst.timer = burst.interval;
        }
    }
}

/// Apply a damage hit to `target`. A shield (if any) soaks damage
/// first; a Firewall-protected target then takes only
/// `FIREWALL_DAMAGE_TAKEN` of the leak and reflects the rest back to
/// the attacker; paralyzer weapons accumulate the final amount on the
/// stun charge, promoting to `Stunned` once it exceeds max HP;
/// non-paralyzer leak subtracts from `Health`.
#[allow(clippy::too_many_arguments)]
fn apply_hit(
    target: Entity,
    attacker: Entity,
    amount: f32,
    paralyzer: bool,
    paralyze_time: f32,
    health_q: &mut Query<&mut Health>,
    stun_q: &mut Query<&mut StunCharge>,
    shield_q: &mut Query<&mut crate::units::mechanics::shield::ShieldState>,
    protected_q: &Query<(), With<crate::units::mechanics::command_fire::Protected>>,
    commands: &mut Commands,
) {
    let leak = match shield_q.get_mut(target) {
        Ok(mut shield) => shield.absorb(amount),
        Err(_) => amount,
    };
    if leak <= 0.0 {
        return;
    }

    let (final_amount, reflected) = if protected_q.get(target).is_ok() {
        let taken = leak * crate::units::mechanics::command_fire::FIREWALL_DAMAGE_TAKEN;
        (taken, leak - taken)
    } else {
        (leak, 0.0)
    };

    if reflected > 0.0
        && target != attacker
        && let Ok(mut health) = health_q.get_mut(attacker)
    {
        health.current -= reflected;
    }

    let leak = final_amount;
    if leak <= 0.0 {
        return;
    }
    if paralyzer {
        if let Ok(max_hp) = health_q.get(target).map(|h| h.max)
            && let Ok(mut charge) = stun_q.get_mut(target)
        {
            charge.0 += leak;
            if charge.0 >= max_hp {
                commands.entity(target).insert(Stunned {
                    remaining: paralyze_time,
                });
            }
        }
    } else if let Ok(mut health) = health_q.get_mut(target) {
        health.current -= leak;
    }
}

/// Apply queued damage and mark targets as infected when hit by
/// Worm or Virus weapons. Weapons with `area_of_effect > AOE_SPLASH_THRESHOLD`
/// also damage other units in radius, with linear falloff from the
/// weapon's `edge_effectiveness`. `avoidfriendly=1` and `noselfdamage=1`
/// filter the splash set so allies / the attacker don't eat stray AoE.
#[allow(clippy::too_many_arguments)]
pub fn apply_damage(
    mut damage_queue: ResMut<DamageQueue>,
    mut health_q: Query<&mut Health>,
    mut stun_q: Query<&mut StunCharge>,
    mut shield_q: Query<&mut crate::units::mechanics::shield::ShieldState>,
    attacker_q: Query<(&UnitType, &Faction, &TeamId)>,
    target_unit_q: Query<&UnitType>,
    target_pos_q: Query<(&GlobalTransform, &UnitStats), With<UnitType>>,
    protected_q: Query<(), With<crate::units::mechanics::command_fire::Protected>>,
    weapon_registry: Res<WeaponRegistry>,
    unit_registry: Res<UnitRegistry>,
    spatial: Res<SpatialIndex>,
    mut commands: Commands,
    mut splash_hits: Local<Vec<(Entity, f32)>>,
) {
    for pending in damage_queue.drain() {
        let Some(weapon_def) = weapon_registry.get(&pending.weapon) else {
            warn!("apply_damage: weapon {:?} not in registry", pending.weapon);
            continue;
        };

        let base = |kind: UnitKind| {
            weapon_def.damage.for_type(kind.armor_class().key())
                * unit_registry.damage_modifier(kind)
        };
        let attacker_info = attacker_q.get(pending.attacker).ok();
        let paralyzer = weapon_def.paralyzer;
        let paralyze_time = weapon_def.paralyze_time;

        let dyn_mult = weapon_def.dyn_damage_multiplier(pending.attacker_distance);
        // Spray-angle miss gate. `spray_angle > 0` weapons perturbed
        // their `impact_pos` in combat_system; here we check whether the
        // perturbed impact still lands inside the target's volumetric
        // `hit_radius` (the S3O bounding sphere, which is what Spring's
        // `CCollisionHandler` sphere test uses — *not* the footprint-
        // derived `UnitStats.radius`, which is 2-3× tighter and scored
        // nearly every shot as a miss on the last attempt). Zero-spread
        // weapons always land; a missed shot still produces splash from
        // `impact_pos` below for AoE weapons.
        let primary_target_kind = target_unit_q.get(pending.target).ok().map(|ut| ut.0);
        let target_hit = if weapon_def.spray_angle > 0.0 {
            if let Ok((tgt_xform, tgt_stats)) = target_pos_q.get(pending.target) {
                tgt_xform.translation().distance(pending.impact_pos) <= tgt_stats.hit_radius
            } else {
                // Target despawned between queueing and apply — no
                // primary hit, splash still handles nearby units.
                false
            }
        } else {
            true
        };
        if target_hit {
            let primary_damage = primary_target_kind
                .map(|k| base(k) * dyn_mult)
                .unwrap_or(weapon_def.damage.default * dyn_mult);
            apply_hit(
                pending.target,
                pending.attacker,
                primary_damage,
                paralyzer,
                paralyze_time,
                &mut health_q,
                &mut stun_q,
                &mut shield_q,
                &protected_q,
                &mut commands,
            );
            commands.entity(pending.target).insert(IdleTimer(0.0));
        }

        let aoe = weapon_def.area_of_effect;
        if aoe > AOE_SPLASH_THRESHOLD {
            let aoe_sq = aoe * aoe;
            let edge_mult = weapon_def.edge_effectiveness;
            let avoid_friendly = weapon_def.avoid_friendly;
            let no_self_damage = weapon_def.no_self_damage;
            // Collect first, then apply — apply_hit borrows mutable queries
            // so we can't stay inside the spatial callback closure. Re-uses
            // a Local buffer across calls to avoid per-hit allocation.
            splash_hits.clear();
            spatial.query_radius(pending.impact_pos, aoe, |candidate| {
                if candidate.entity == pending.target {
                    return;
                }
                if no_self_damage && candidate.entity == pending.attacker {
                    return;
                }
                if avoid_friendly
                    && let Some((_, a_faction, a_team)) = attacker_info
                    && crate::units::components::is_friendly(
                        candidate.team,
                        candidate.faction,
                        a_team.0,
                        *a_faction,
                    )
                {
                    return;
                }
                let d_sq = candidate.pos.distance_squared(pending.impact_pos);
                if d_sq >= aoe_sq {
                    return;
                }
                let kind = match target_unit_q.get(candidate.entity) {
                    Ok(ut) => ut.0,
                    Err(_) => return,
                };
                let splash = base(kind) * splash_falloff(d_sq.sqrt(), aoe, edge_mult);
                splash_hits.push((candidate.entity, splash));
            });
            for (entity, splash) in splash_hits.drain(..) {
                apply_hit(
                    entity,
                    pending.attacker,
                    splash,
                    paralyzer,
                    paralyze_time,
                    &mut health_q,
                    &mut stun_q,
                    &mut shield_q,
                    &protected_q,
                    &mut commands,
                );
                commands.entity(entity).insert(IdleTimer(0.0));
            }
        }

        // Apply infection: keyed on the weapon (not the attacker kind)
        // to match upstream LuaRules/Gadgets/infection.lua. VirusBeam,
        // VirusDeath, Wormsplash, and Obelisk Infection each have their
        // own infection window in seconds. Only fires when the primary
        // hit actually landed — a shot that misses on spray angle
        // shouldn't infect the intended target.
        if target_hit
            && let Some(duration) = weapon_infection_duration(&pending.weapon)
            && let Some((_, attacker_faction, attacker_team)) = attacker_info
        {
            let target_is_virus = target_unit_q
                .get(pending.target)
                .is_ok_and(|ut| ut.0 == UnitKind::Virus);
            if !target_is_virus {
                commands.entity(pending.target).insert(Infected {
                    timer: duration,
                    attacker_faction: *attacker_faction,
                    attacker_team: attacker_team.0,
                });
            }
        }
    }
}

/// Decay the [`Infected`] timer and remove the component when it
/// expires. Works independently of damage application.
pub fn tick_infections(
    time: Res<Time>,
    mut query: Query<(Entity, &mut Infected)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut infected) in &mut query {
        infected.timer -= dt;
        if infected.timer <= 0.0 {
            commands.entity(entity).remove::<Infected>();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weapon_infection_durations_match_upstream_gadget() {
        // Values from upstream LuaRules/Gadgets/infection.lua, converted
        // from sim frames @ 30 fps to seconds.
        assert_eq!(weapon_infection_duration("VirusBeam"), Some(3.0));
        assert_eq!(weapon_infection_duration("VirusDeath"), Some(6.0));
        assert!((weapon_infection_duration("Wormsplash").unwrap() - 6.666_667).abs() < 1e-3);
        assert_eq!(weapon_infection_duration("Infection"), Some(1.0));
        assert_eq!(weapon_infection_duration("BitShot"), None);
        assert_eq!(weapon_infection_duration("Wormbite"), None);
    }

    #[test]
    fn splash_full_damage_at_center() {
        assert!((splash_falloff(0.0, 512.0, 0.0) - 1.0).abs() < 1e-5);
        assert!((splash_falloff(0.0, 100.0, 1.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn splash_edge_matches_edge_effectiveness() {
        // edge_effectiveness = 0.8 → edge damage is 80% of center.
        assert!((splash_falloff(512.0, 512.0, 0.8) - 0.8).abs() < 1e-5);
        // edge_effectiveness = 0.0 → edge damage is zero.
        assert!(splash_falloff(512.0, 512.0, 0.0).abs() < 1e-5);
        // edge_effectiveness = 1.0 → full damage across the radius.
        assert!((splash_falloff(256.0, 512.0, 1.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn splash_linear_between_center_and_edge() {
        // Halfway out at edge_effectiveness=0 → half damage.
        assert!((splash_falloff(256.0, 512.0, 0.0) - 0.5).abs() < 1e-5);
        // Quarter out at edge_effectiveness=0.4 → 1 - 0.25 * 0.6 = 0.85.
        assert!((splash_falloff(128.0, 512.0, 0.4) - 0.85).abs() < 1e-5);
    }

    #[test]
    fn burst_fire_releases_shots_at_interval() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>();

        let target = app.world_mut().spawn_empty().id();
        let attacker = app
            .world_mut()
            .spawn((
                GlobalTransform::default(),
                BurstFire {
                    shots_remaining: 3,
                    interval: 0.25,
                    timer: 0.25,
                    target,
                    target_pos: Vec3::ZERO,
                    weapon: "TestWeapon".to_string(),
                    arc_height: 0.0,
                },
            ))
            .id();

        // Advance one interval: one shot fires, two remain.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(250));
        app.world_mut().run_system_once(tick_burst_fire).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        assert_eq!(
            app.world()
                .get::<BurstFire>(attacker)
                .unwrap()
                .shots_remaining,
            2
        );

        // A fraction later: not yet due.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(100));
        app.world_mut().run_system_once(tick_burst_fire).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);

        // Two more intervals: remaining shots fire, component is gone.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(250));
        app.world_mut().run_system_once(tick_burst_fire).unwrap();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(250));
        app.world_mut().run_system_once(tick_burst_fire).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().len(), 3);
        assert!(app.world().get::<BurstFire>(attacker).is_none());
    }
}

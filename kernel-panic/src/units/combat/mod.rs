//! Combat orchestration: target-selection + fire pipeline, plus the
//! shared marker components every sub-module references.
//!
//! The combat logic is split across four files:
//!
//! - [`mod`](self) — [`combat_system`] (target pick + first-shot dispatch),
//!   plus the shared [`AttackCooldown`] / [`IdleTimer`] / [`StunCharge`]
//!   markers and the [`muzzle_world_pos`] helper used across fire paths.
//! - [`aim`] — deploy-state machine and host-driven turret aim.
//! - [`damage`] — pending-damage queue, splash falloff, burst follow-ups,
//!   infection tagging, and per-weapon infection-duration table.
//! - [`lifecycle`] — stun decay, kamikaze proximity trigger, death
//!   detection, dying-corpse despawn, idle auto-heal.
//!
//! Public items are re-exported below so external callers keep using
//! `super::combat::{...}` unchanged.

use bevy::prelude::*;

use super::assets::animation::{CobAnimator, MuzzlePiece};
use super::components::{Faction, TeamId, UnitStats, UnitType};
use super::content::unit_registry::UnitRegistry;
use super::content::weapons::WeaponRegistry;
use super::lifecycle::script_triggers::JustFired;
use super::spatial::SpatialIndex;
use super::weapon_fx::{AttackEvent, PendingAttacks};
use crate::rng::next_signed;
use crate::terrain::heightmap::Heightmap;

mod aim;
mod damage;
mod lifecycle;

pub use aim::{
    AIM_HEADING_TOLERANCE, AimTarget, DeployState, Deployable, aim_weapons_system,
    tick_deploy_state,
};
pub use damage::{
    BurstFire, DamageQueue, INFECTION_DURATION, Infected, PendingDamage, VirusSpawnQueue,
    apply_damage, tick_burst_fire, tick_infections,
};
pub use lifecycle::{
    Dying, SELF_DESTRUCT_DELAY, SelfDestructCountdown, Stunned, auto_heal, cleanup_dying,
    death_system, tick_kamikaze, tick_self_destruct, tick_stun,
};

/// Player-issued order to fire the unit's weapon at a fixed ground position.
///
/// While present the unit moves toward `pos` if out of weapon range, then
/// fires at `pos` each reload cycle. The normal auto-attack path in
/// [`combat_system`] still runs concurrently — the first enemy that steps
/// into range gets hit as usual.
///
/// Cleared on Stop, right-click move, or any new explicit order.
#[derive(Component, Clone, Copy)]
pub struct AttackGroundOrder {
    pub pos: Vec3,
}

/// Added to each sampled terrain height in LOS checks so the shooter and
/// target standing on a crest don't self-block. Roughly half a heightmap
/// square — tighter than this and slopes read as walls; looser and
/// weapons shoot over short ridges.
const LOS_MARGIN: f32 = 4.0;

/// Spring encodes per-shot spread in "short" angular units where a full
/// revolution is 65536. Conversion to radians for aim-offset math.
const SHORT_ANGLE_TO_RAD: f32 = std::f32::consts::TAU / 65536.0;

/// Tracks time until the unit can fire again.
#[derive(Component)]
pub struct AttackCooldown {
    pub remaining: f32,
}

/// Seconds since this unit last took damage, moved, or picked a target.
/// When this exceeds the unit's FBI `IdleTime`, the `auto_heal` system
/// regenerates HP at `IdleAutoHeal` per second. Reset to zero on every
/// activity signal.
#[derive(Component, Default)]
pub struct IdleTimer(pub f32);

/// Accumulated paralyzer damage from weapons with `paralyzer=1`. Charge
/// bleeds off over `STUN_CHARGE_DECAY` once the unit stops getting hit;
/// when it crosses `max_health` the unit gains a `Stunned` marker for
/// the weapon's `paralyzetime` seconds. The charge is cleared when
/// `Stunned` is removed.
#[derive(Component, Default)]
pub struct StunCharge(pub f32);

/// World-space position of `attacker`'s weapon muzzle, or its transform
/// origin if the unit has no [`MuzzlePiece`] resolved (e.g. Wormbite's
/// melee bite, a factory's BuildLaser, or a unit whose .bos doesn't
/// declare a recognised muzzle name). Falling back to the unit origin
/// keeps existing visuals working while upgrading every unit that *does*
/// name its barrel to fire from that piece's world pos.
pub(super) fn muzzle_world_pos(
    attacker: Entity,
    attacker_gtf: &GlobalTransform,
    muzzle_q: &Query<&MuzzlePiece>,
    animator_q: &Query<&CobAnimator>,
    piece_gtf_q: &Query<&GlobalTransform, Without<UnitType>>,
) -> Vec3 {
    if let Ok(mp) = muzzle_q.get(attacker)
        && let Ok(animator) = animator_q.get(attacker)
        && let Some(&piece_entity) = animator.piece_entities.get(mp.0)
        && let Ok(piece_gtf) = piece_gtf_q.get(piece_entity)
    {
        piece_gtf.translation()
    } else {
        attacker_gtf.translation()
    }
}

/// Armed units auto-attack the nearest enemy in range.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn combat_system(
    time: Res<Time>,
    mut cooldowns: Query<&mut AttackCooldown>,
    attackers: Query<
        (
            Entity,
            &UnitType,
            &UnitStats,
            &Faction,
            &TeamId,
            &GlobalTransform,
            Option<&Deployable>,
        ),
        (
            Without<Dying>,
            Without<super::lifecycle::spawning::Emerging>,
            Without<Stunned>,
            // Cloaked units (Worm, Logic Bomb) hold fire by default —
            // FEATURES.md §20 / upstream KP behaviour. A manual attack
            // order would remove `Cloaked` before firing in the
            // future; today the toggle doesn't exist yet (Worm
            // `autohold` remains in the spec-gaps list).
            Without<crate::units::mechanics::cloak::Cloaked>,
        ),
    >,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    weapon_registry: Res<WeaponRegistry>,
    mut pending_attacks: ResMut<PendingAttacks>,
    unit_registry: Res<UnitRegistry>,
    spatial: Res<SpatialIndex>,
    heightmap: Option<Res<Heightmap>>,
    muzzle_q: Query<&MuzzlePiece>,
    animator_q: Query<&CobAnimator>,
    piece_gtf_q: Query<&GlobalTransform, Without<UnitType>>,
    mut rng: Local<u32>,
) {
    if *rng == 0 {
        // Seed lazily on first tick so we never produce the all-zero
        // xorshift state that locks up the PRNG.
        *rng = 0xDEADBEEF;
    }
    let dt = time.delta_secs();

    // Tick cooldowns.
    for mut cd in &mut cooldowns {
        cd.remaining = (cd.remaining - dt).max(0.0);
    }

    damage_queue.clear();

    for (entity, unit_type, stats, attacker_faction, attacker_team, attacker_gtf, deployable) in
        &attackers
    {
        // Deployable units (Pointer) can only fire while fully open. They
        // can still *aim* while opening (so the gun is pointed when Open
        // completes), but firing is gated on the open state.
        let fire_blocked_by_deploy = deployable.is_some_and(|d| d.state != DeployState::Open);

        let weapon_name = unit_registry.weapon(unit_type.0);

        // Resolve weapon stats from the TDF registry.
        let weapon_def = if weapon_name.is_empty() {
            None
        } else {
            weapon_registry.get(weapon_name)
        };
        let range = weapon_def.map_or(0.0, |w| w.range);
        let cooldown = weapon_def.map_or(0.0, |w| w.reload_time);

        // Command-fire weapons (NX Flag, Infection, FakeBugCannon) only
        // activate when the player issues the ability order explicitly.
        // Without this gate, an Obelisk would spew infection gas at
        // whatever wandered into range.
        let command_fire = weapon_def.is_some_and(|w| w.command_fire);

        if range == 0.0 || command_fire {
            commands.entity(entity).remove::<AimTarget>();
            continue;
        }

        let attacker_pos = attacker_gtf.translation();
        let range_sq = range * range;

        // Pick a target in range. Most weapons prefer the nearest enemy;
        // those with `proximity_priority < 0` (Exploit's BugCannon) prefer
        // the *farthest*, matching upstream's anti-swarm artillery role.
        // Shared team or shared faction = ally (see `is_friendly`).
        let prefer_distant = weapon_def.is_some_and(|w| w.proximity_priority < 0.0);
        // Ballistic weapons (`trajectory_height > 0`) lob over terrain and
        // should never fail an LOS check — the whole point is to clear
        // the ridge. Direct-fire weapons with `lineofsight=1` do.
        let enforce_los = weapon_def
            .is_some_and(|w| w.line_of_sight && w.trajectory_height <= 0.0 && heightmap.is_some());
        // Most KP ground units set `NoChaseCategory=VTOL` so they ignore
        // Flows (and any future flying unit) during auto-target. Flying
        // units themselves still chase fliers — the filter is per-attacker.
        // The Pointer is the documented exception (FEATURES.md §12): its
        // homing projectile explicitly tracks air targets, so we override
        // the FBI filter for that one kind.
        let skip_flying = stats.no_chase_vtol && !unit_type.0.homing_targets_air();
        // Debug (mineblaster) sets `OnlyTargetCategory1=VOID` upstream
        // and only fires Minekiller on mines / walls — it's a
        // defensive turret, not a tickle-turret for infantry. Keep the
        // filter as a per-kind gate so future units can join the
        // "targets mines only" club without touching combat_system.
        let targets_mines_only = unit_type.0.targets_mines_only();
        let mut best: Option<(Entity, Vec3, f32)> = None;
        spatial.query_radius(attacker_pos, range, |candidate| {
            if !candidate.hp_positive
                || candidate.team == attacker_team.0
                || candidate.faction == *attacker_faction
            {
                return;
            }
            if skip_flying && candidate.is_flying {
                return;
            }
            if targets_mines_only && !candidate.kind.is_minekiller_target() {
                return;
            }
            let dist_sq = attacker_pos.distance_squared(candidate.pos);
            if dist_sq > range_sq {
                return;
            }
            if enforce_los
                && let Some(hm) = heightmap.as_deref()
                && !hm.has_line_of_sight(attacker_pos, candidate.pos, LOS_MARGIN)
            {
                return;
            }
            let better = best.is_none_or(|(_, _, d)| {
                if prefer_distant {
                    dist_sq > d
                } else {
                    dist_sq < d
                }
            });
            if better {
                best = Some((candidate.entity, candidate.pos, dist_sq));
            }
        });

        let Some((target_entity, target_pos, _)) = best else {
            commands.entity(entity).remove::<AimTarget>();
            continue;
        };

        let arc_height = weapon_def.map_or(0.0, |w| w.trajectory_height);

        // Always stamp the aim target while we have a candidate — that
        // lets `aim_weapons_system` keep steering the body/gun even
        // while we're still on cooldown or still opening, so the weapon
        // is already on-target when it's allowed to fire.
        commands.entity(entity).insert(AimTarget {
            pos: target_pos,
            arc_height,
        });

        if fire_blocked_by_deploy {
            continue;
        }

        // Skip if still on cooldown.
        if let Ok(cd) = cooldowns.get(entity)
            && cd.remaining > 0.0
        {
            continue;
        }

        // For Deployable units, only fire when the body is actually
        // pointed at the target. The aim system will have been steering
        // us all along; this just delays the shot until the steering
        // has caught up, preventing the Pointer from firing off-axis.
        if deployable.is_some() {
            let forward = attacker_gtf.forward().as_vec3();
            let to_target = Vec3::new(
                target_pos.x - attacker_pos.x,
                0.0,
                target_pos.z - attacker_pos.z,
            );
            let to_target_len_sq = to_target.length_squared();
            if to_target_len_sq > 1e-6 {
                let to_target_n = to_target / to_target_len_sq.sqrt();
                let forward_xz = {
                    let f = Vec3::new(forward.x, 0.0, forward.z);
                    if f.length_squared() < 1e-6 {
                        Vec3::Z
                    } else {
                        f.normalize()
                    }
                };
                let align = forward_xz.dot(to_target_n).clamp(-1.0, 1.0);
                if align.acos() > AIM_HEADING_TOLERANCE {
                    continue;
                }
            }
        }

        // Per-shot spread. Upstream's `sprayangle` is in Spring short-angle
        // units; offset magnitude at the target plane is roughly
        // `tan(angle) × distance`, small-angle ≈ `angle × distance`. We
        // perturb only XZ — vertical aim is already handled by arc_height
        // and the aim pitch computed in `aim_weapons_system`.
        let distance = attacker_pos.distance(target_pos);
        let spray_short = weapon_def.map_or(0.0, |w| w.spray_angle);
        let mut impact_pos = target_pos;
        if spray_short > 0.0 && distance > 0.0 {
            let spray_rad = spray_short * SHORT_ANGLE_TO_RAD;
            let offset_radius = spray_rad.tan() * distance;
            let dx = next_signed(&mut rng) * offset_radius;
            let dz = next_signed(&mut rng) * offset_radius;
            impact_pos = Vec3::new(target_pos.x + dx, target_pos.y, target_pos.z + dz);
        }

        damage_queue.push(PendingDamage {
            target: Some(target_entity),
            attacker: entity,
            weapon: weapon_name.to_string(),
            impact_pos,
            attacker_distance: distance,
        });
        commands.entity(entity).insert((
            AttackCooldown {
                remaining: cooldown,
            },
            JustFired {
                target_pos: impact_pos,
                arc_height,
            },
        ));
        if !weapon_name.is_empty() {
            // Beam/projectile origin comes from the unit's resolved muzzle
            // piece when available — unit-center origin here made Bit's
            // `>>>>>` arrow look like it shot from the torso. Range and
            // LOS checks above intentionally still use the unit center
            // so arm-length-scale piece offsets don't flicker targeting.
            let visual_origin =
                muzzle_world_pos(entity, attacker_gtf, &muzzle_q, &animator_q, &piece_gtf_q);
            let muzzle_ceg = unit_registry
                .preferred_muzzle_ceg(unit_type.0)
                .map(|s| std::borrow::Cow::Owned(s.to_string()));
            pending_attacks.events.push(AttackEvent {
                attacker_pos: visual_origin,
                target_pos: impact_pos,
                weapon_name: std::borrow::Cow::Owned(weapon_name.to_string()),
                muzzle_ceg,
            });
        }

        let burst = weapon_def.map_or(0.0, |w| w.burst) as u32;
        if burst > 1 {
            commands.entity(entity).insert(BurstFire {
                shots_remaining: burst - 1,
                interval: weapon_def.map_or(0.1, |w| w.burst_rate.max(0.05)),
                timer: weapon_def.map_or(0.1, |w| w.burst_rate.max(0.05)),
                target: target_entity,
                target_pos,
                weapon: weapon_name.to_string(),
                arc_height,
            });
        }
    }
}

/// Fire the unit's weapon at a player-specified ground position.
///
/// Each frame where cooldown has expired and the target is in range, the
/// system fires one shot at `order.pos` with no primary entity target
/// (`PendingDamage.target = None`). Splash damage from AoE weapons hits
/// everything within `area_of_effect` around the impact point as normal.
///
/// If the target is out of range the system steers the unit toward it by
/// inserting a `MoveTarget` pointed at the range boundary. The unit stops
/// advancing once it can fire — it won't walk all the way to the impact
/// point unless the weapon range is 0 (unarmed units skip this system).
#[allow(clippy::too_many_arguments)]
pub fn attack_ground_system(
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    attackers: Query<(Entity, &UnitType, &GlobalTransform, &AttackGroundOrder), Without<Dying>>,
    cooldowns: Query<&AttackCooldown>,
    muzzle_q: Query<&MuzzlePiece>,
    animator_q: Query<&CobAnimator>,
    piece_gtf_q: Query<&GlobalTransform, Without<UnitType>>,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    for (entity, unit_type, gtf, order) in &attackers {
        let weapon_name = unit_registry.weapon(unit_type.0);
        let Some(weapon_def) = weapon_registry.get(weapon_name) else {
            continue;
        };
        let range = weapon_def.range;
        if range <= 0.0 {
            continue;
        }

        let attacker_pos = gtf.translation();
        let dist = attacker_pos.distance(order.pos);

        if dist > range {
            // Move toward the target, stopping just inside weapon range so
            // the unit doesn't walk through the blast zone of its own AoE.
            let dir = (order.pos - attacker_pos).normalize_or(Vec3::NEG_Z);
            let stop_at = attacker_pos + dir * (dist - range * 0.85);
            commands
                .entity(entity)
                .insert(crate::interaction::movement::MoveTarget(stop_at));
            continue;
        }

        // Read the shared cooldown WITHOUT decrementing — `combat_system`
        // already ticks every `AttackCooldown` once per frame. Ticking
        // again here would halve the effective reload for ground-target
        // orders, making weapons fire twice as fast as the TDF spec.
        let cd_ready = cooldowns.get(entity).map_or(true, |cd| cd.remaining <= 0.0);
        if !cd_ready {
            continue;
        }

        // Aim at ground target so the barrel sweeps visibly.
        let arc_height = weapon_def.trajectory_height * 0.4;
        commands.entity(entity).insert(AimTarget {
            pos: order.pos,
            arc_height,
        });

        // Fire.
        let visual_origin = muzzle_world_pos(entity, gtf, &muzzle_q, &animator_q, &piece_gtf_q);
        let muzzle_ceg = unit_registry
            .preferred_muzzle_ceg(unit_type.0)
            .map(|s| std::borrow::Cow::Owned(s.to_string()));
        pending_attacks.events.push(AttackEvent {
            attacker_pos: visual_origin,
            target_pos: order.pos,
            weapon_name: std::borrow::Cow::Owned(weapon_name.to_string()),
            muzzle_ceg,
        });
        damage_queue.push(PendingDamage {
            target: None,
            attacker: entity,
            weapon: weapon_name.to_string(),
            impact_pos: order.pos,
            attacker_distance: dist,
        });
        commands.entity(entity).insert((
            AttackCooldown {
                remaining: weapon_def.reload_time,
            },
            JustFired {
                target_pos: order.pos,
                arc_height,
            },
        ));
    }
}

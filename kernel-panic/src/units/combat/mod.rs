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

use super::assets::animation::{MuzzlePiece, UnitAnimator};
use super::components::{Faction, TeamId, UnitStats, UnitType};
use super::content::unit_registry::UnitRegistry;
use super::content::weapons::{WeaponId, WeaponRegistry};
use super::lifecycle::script_triggers::JustFired;
use super::spatial::SpatialIndex;
use super::weapon_fx::{AttackEvent, DelayedHitInfo, PendingAttacks};
use crate::rng::next_signed;
use crate::terrain::heightmap::Heightmap;

mod aim;
mod collision_volume;
mod damage;
mod lifecycle;

pub use collision_volume::CollisionVolume;

pub use aim::{
    AIM_HEADING_TOLERANCE, AIM_PITCH_TOLERANCE, AimScript, AimTarget, BYTE_CLOSE_DELAY, ByteOpen,
    DeployState, Deployable, OpeningDelay, aim_weapons_system, drive_aim_script, tick_byte_open,
    tick_deploy_state, tick_opening_delay,
};
pub use damage::{
    BurstFire, DamageQueue, Infected, PendingDamage, VirusSpawn, VirusSpawnQueue, apply_damage,
    tick_burst_fire, tick_infections, weapon_infection_duration,
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

/// Player-issued order to attack a specific unit (right-click on an enemy).
///
/// [`attack_target_system`] chases the target while it is out of weapon
/// range, holds position once inside, and lets the normal auto-attack path
/// in [`combat_system`] do the shooting. Cleared automatically when the
/// target dies, and on Stop / any new explicit order.
#[derive(Component, Clone, Copy)]
pub struct AttackTargetOrder {
    pub target: Entity,
}

/// Manual target designation (`T` set-target / `X` unset, Spring's
/// `CMD_SET_TARGET` / `CMD_UNSET_TARGET`).
///
/// An *aim designation*, not a move order: the unit prefers the forced
/// target over auto-acquisition once it is inside weapon range, and keeps
/// its turret tracking it even while out of range (no fire, no chase).
/// Persists across move orders; cleared by `X`, Stop, death of the target,
/// or an explicit attack order.
#[derive(Component, Clone, Copy)]
pub struct ForcedTarget(pub Entity);

/// Required clearance (elmos) between the LOS ray and the underlying
/// terrain at every sample point — the ray passes iff `beam_y >=
/// terrain_y + LOS_MARGIN`.
///
/// Held tight (4 elmos) so actual ridges still block, but **only**
/// meaningful once the ray itself is lifted above ground by
/// [`LOS_MUZZLE_HEIGHT`]. Without that lift the shooter and target
/// stand at ground level, `beam_y == terrain_y` along flat terrain,
/// and every check fails the `terrain + 4` margin. That was the
/// observed bit-vs-packet bug: bit saw packet 100 elmos away, well
/// inside its 256 range, but LOS rejected every tick.
const LOS_MARGIN: f32 = 4.0;

/// Muzzle height added to both endpoints of the LOS ray — a stand-in
/// for shooting from the weapon's gun piece rather than from the
/// unit's feet. 16 elmos is roughly the height of the smallest KP
/// unit's gun mount (bit ball.s3o has the gunpoint at z=-3 off a
/// 32-tall body); bigger units (byte, pointer) sit higher but we
/// err on the conservative side so a genuine wall still blocks.
const LOS_MUZZLE_HEIGHT: f32 = 16.0;

/// Spring encodes per-shot spread in "short" angular units where a full
/// revolution is 65536. Conversion to radians for aim-offset math.
const SHORT_ANGLE_TO_RAD: f32 = std::f32::consts::TAU / 65536.0;

/// Seconds between full spatial scans for a unit that already has a
/// cached target. Matches Spring's `CWeapon::lastTargetRetry + 65`
/// guard (`rts/Sim/Weapons/Weapon.cpp`) which re-scans at most every
/// ~2 s (65 sim frames @ 30 fps). Cache is invalidated immediately
/// whenever the cached target dies or leaves weapon range, so this
/// only controls how quickly a unit abandons a valid target for a
/// newly-arrived closer one — not how fast it reacts to kills.
const TARGET_RESCAN_INTERVAL: f32 = 2.0;

/// Cached auto-target for an armed unit. While present and the target
/// is still alive + in-range, `combat_system` skips its spatial
/// `query_radius` sweep — the dominant per-frame cost in big battles.
#[derive(Component)]
pub struct TargetCache {
    pub target: Entity,
    pub expires_at: f32,
}

/// Grouped queries for target caching, bundled into one `SystemParam`
/// so `combat_system` stays under Bevy's 16-param limit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct TargetCachePick<'w, 's> {
    pub cache: Query<'w, 's, &'static TargetCache>,
    pub alive: Query<'w, 's, &'static GlobalTransform, (With<UnitType>, Without<Dying>)>,
}

/// Grouped piece-lookup queries for muzzle position, body animator,
/// piece world transforms, and the `GunbasePiece` / `AimerPiece`
/// marker components. Lets the combat systems stay under Bevy's
/// 16-param system limit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct PieceLookup<'w, 's> {
    pub muzzle: Query<'w, 's, &'static MuzzlePiece>,
    pub animator: Query<'w, 's, &'static UnitAnimator>,
    pub piece_gtf: Query<'w, 's, &'static GlobalTransform, Without<UnitType>>,
    pub gunbase: Query<'w, 's, &'static crate::units::assets::animation::GunbasePiece>,
    pub aimer: Query<'w, 's, &'static crate::units::assets::animation::AimerPiece>,
}

/// XZ-flatten and normalise a forward vector. Falls back to +Z
/// when the projection is degenerate (forward straight up/down).
fn flat_forward(forward: Vec3) -> Vec3 {
    let f = Vec3::new(forward.x, 0.0, forward.z);
    if f.length_squared() < 1e-6 {
        Vec3::Z
    } else {
        f.normalize()
    }
}

/// Tracks time until the unit can fire again.
#[derive(Component)]
pub struct AttackCooldown {
    pub remaining: f32,
}

/// Per-attacker cached primary-weapon id. Inserted at spawn time once
/// the unit's FBI weapon name has been interned by the
/// [`WeaponRegistry`]; from there the per-frame combat hot path looks
/// up the `WeaponDef` via a single `Vec` index instead of hashing the
/// weapon's TDF name on every iteration. Units with no primary weapon
/// (or whose only weapon is BuildLaser, filtered upstream by
/// `unit_registry.weapon`) carry no binding — combat skips them via
/// `Option<&WeaponBinding>`.
#[derive(Component, Copy, Clone, Debug)]
pub struct WeaponBinding(pub WeaponId);

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
    animator_q: &Query<&UnitAnimator>,
    piece_gtf_q: &Query<&GlobalTransform, Without<UnitType>>,
) -> Vec3 {
    if let Ok(mp) = muzzle_q.get(attacker)
        && let Ok(animator) = animator_q.get(attacker)
        && let Some(&piece_entity) = animator.rig.piece_entities.get(mp.0)
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
            Option<&OpeningDelay>,
            Option<&AimScript>,
            Option<&WeaponBinding>,
        ),
        (
            Without<Dying>,
            Without<super::lifecycle::spawning::Emerging>,
            Without<Stunned>,
            // Why: cloaked units (Worm, Logic Bomb) hold fire by
            // default — FEATURES.md §20 / upstream `autohold`.
            Without<crate::units::mechanics::cloak::Cloaked>,
        ),
    >,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    weapon_registry: Res<WeaponRegistry>,
    mut pending_attacks: ResMut<PendingAttacks>,
    unit_registry: Res<UnitRegistry>,
    spatial: Res<SpatialIndex>,
    forced_q: Query<&ForcedTarget>,
    heightmap: Option<Res<Heightmap>>,
    pieces: PieceLookup,
    target_pick: TargetCachePick,
    mut rng: Local<u32>,
) {
    if *rng == 0 {
        // Seed lazily on first tick so we never produce the all-zero
        // xorshift state that locks up the PRNG.
        *rng = 0xDEADBEEF;
    }
    let dt = time.delta_secs();
    let now = time.elapsed_secs();

    // Tick cooldowns.
    for mut cd in &mut cooldowns {
        cd.remaining = (cd.remaining - dt).max(0.0);
    }

    damage_queue.clear();

    for (
        entity,
        unit_type,
        stats,
        attacker_faction,
        attacker_team,
        attacker_gtf,
        deployable,
        opening_delay,
        aim_script,
        weapon_binding,
    ) in &attackers
    {
        // Deployable: keep aiming through Opening so the gun is on
        // target when the state flips, but only fire while Open.
        let fire_blocked_by_deploy = deployable.is_some_and(|d| d.state != DeployState::Open);
        let fire_blocked_by_opening = opening_delay.is_some_and(|d| d.remaining > 0.0);

        // Resolve weapon stats via the cached binding when present.
        // Falling back to the registry name lookup for legacy paths
        // (e.g. units that haven't been migrated to `WeaponBinding`)
        // keeps behaviour identical for any unit class that bypasses
        // `spawn_unit` — known callers cover every armed unit today
        // but the safety net is cheap.
        let (weapon_name, weapon_def) = match weapon_binding {
            Some(binding) => {
                let def = weapon_registry.by_id(binding.0);
                (weapon_registry.name(binding.0), Some(def))
            }
            None => {
                let name = unit_registry.weapon(unit_type.0);
                if name.is_empty() {
                    (name, None)
                } else {
                    (name, weapon_registry.get(name))
                }
            }
        };
        let range = weapon_def.map_or(0.0, |w| w.range);
        let cooldown = weapon_def.map_or(0.0, |w| w.reload_time);

        // Command-fire weapons (NX Flag, Infection, …) need an
        // explicit order; auto-target must skip them.
        let command_fire = weapon_def.is_some_and(|w| w.command_fire);

        if range == 0.0 || command_fire {
            commands.entity(entity).remove::<AimTarget>();
            continue;
        }

        let attacker_pos = attacker_gtf.translation();
        let range_sq = range * range;

        // Why: `proximity_priority < 0` is upstream's anti-swarm
        // marker — Exploit's BugCannon prefers far targets.
        let prefer_distant = weapon_def.is_some_and(|w| w.proximity_priority < 0.0);
        // Ballistic shots clear ridges; only direct-fire enforces LOS.
        let enforce_los = weapon_def
            .is_some_and(|w| w.line_of_sight && w.trajectory_height <= 0.0 && heightmap.is_some());
        // Pointer's homing missile is the only ground unit that
        // overrides upstream's `NoChaseCategory=VTOL` and tracks air.
        let skip_flying = stats.no_chase_vtol && !unit_type.0.homing_targets_air();
        let targets_mines_only = unit_type.0.targets_mines_only();

        // Cached-target fast path mirrors Spring's `lastTargetRetry`.
        let mut best: Option<(Entity, Vec3, f32)> = None;

        // Manual target designation (T / set-target) overrides auto-
        // acquisition: in range → fire at it; out of range → track it
        // with the turret (no fire, no chase); dead → drop the mark.
        if let Ok(forced) = forced_q.get(entity) {
            match target_pick.alive.get(forced.0) {
                Ok(t_gtf) => {
                    let t_pos = t_gtf.translation();
                    let dist_sq = attacker_pos.distance_squared(t_pos);
                    if dist_sq <= range_sq {
                        best = Some((forced.0, t_pos, dist_sq));
                    } else {
                        commands.entity(entity).insert(AimTarget {
                            pos: t_pos,
                            arc_height: weapon_def.map_or(0.0, |w| w.trajectory_height),
                        });
                        commands.entity(entity).remove::<TargetCache>();
                        continue;
                    }
                }
                Err(_) => {
                    commands.entity(entity).remove::<ForcedTarget>();
                }
            }
        }

        if best.is_none() {
            best = target_pick
                .cache
                .get(entity)
                .ok()
                .filter(|cache| cache.expires_at > now)
                .and_then(|cache| {
                    let gtf = target_pick.alive.get(cache.target).ok()?;
                    let pos = gtf.translation();
                    let dist_sq = attacker_pos.distance_squared(pos);
                    (dist_sq <= range_sq).then_some((cache.target, pos, dist_sq))
                });
        }

        if best.is_none() {
            spatial.query_radius(attacker_pos, range, |candidate| {
                if !candidate.hp_positive {
                    return;
                }
                if candidate.team == attacker_team.0 || candidate.faction == *attacker_faction {
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
                    && !hm.has_line_of_sight(
                        attacker_pos + Vec3::Y * LOS_MUZZLE_HEIGHT,
                        candidate.pos + Vec3::Y * LOS_MUZZLE_HEIGHT,
                        LOS_MARGIN,
                    )
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
        }

        let Some((target_entity, target_pos, _)) = best else {
            commands
                .entity(entity)
                .remove::<AimTarget>()
                .remove::<TargetCache>();
            continue;
        };
        commands.entity(entity).insert(TargetCache {
            target: target_entity,
            expires_at: now + TARGET_RESCAN_INTERVAL,
        });

        let arc_height = weapon_def.map_or(0.0, |w| w.trajectory_height);

        // Stamp aim target unconditionally so `aim_weapons_system`
        // keeps steering through cooldown/opening — the weapon is on
        // target the moment firing is allowed.
        commands.entity(entity).insert(AimTarget {
            pos: target_pos,
            arc_height,
        });

        // Why: each in-range frame refreshes Byte's open window —
        // upstream `byte.bos AimWeapon1` signals SIG_AIM and re-
        // schedules Close(), which is what keeps the Byte open while
        // engaging.
        if unit_type.0 == crate::units::content::definitions::UnitKind::Byte {
            commands.entity(entity).insert(ByteOpen {
                open_until: now + BYTE_CLOSE_DELAY,
            });
        }

        if fire_blocked_by_deploy || fire_blocked_by_opening {
            continue;
        }
        // Why: upstream `AimWeapon1` contract — return 1 ⇒ allowed
        // to fire; anything else ⇒ barrel not on-target yet.
        if aim_script.is_some_and(|a| !a.ready) {
            continue;
        }

        if let Ok(cd) = cooldowns.get(entity)
            && cd.remaining > 0.0
        {
            continue;
        }

        // Aim-alignment gate, gated on which piece markers the unit
        // declared. Without it, beams leave mid-slew before the gun
        // is on target. Same arithmetic as `aim_weapons_system` so
        // the two converge cleanly.
        let to_target_xz = Vec3::new(
            target_pos.x - attacker_pos.x,
            0.0,
            target_pos.z - attacker_pos.z,
        );
        let horizontal_dist = to_target_xz.length();

        // Body-heading gate (Pointer): Deployable units rotate the
        // whole body to aim, so wait for that turn to finish. Byte
        // uses an aimer piece and skips this branch.
        if deployable.is_some() && horizontal_dist > 1e-3 {
            let to_target_n = to_target_xz / horizontal_dist;
            let forward_xz = flat_forward(attacker_gtf.forward().as_vec3());
            let align = forward_xz.dot(to_target_n).clamp(-1.0, 1.0);
            if align.acos() > AIM_HEADING_TOLERANCE {
                continue;
            }
        }

        let dy = target_pos.y - attacker_pos.y;
        let direct_pitch = dy.atan2(horizontal_dist.max(1e-6));
        let arc_pitch = if arc_height > 0.0 && horizontal_dist > 1.0 {
            (4.0 * arc_height / horizontal_dist).atan()
        } else {
            0.0
        };
        let target_pitch = direct_pitch + arc_pitch;

        // Gunbase pitch gate (Pointer). pointer.bos's `AimWeapon1`
        // writes `turn gunbase to x-axis (<90>-p)`; cobwtf passes X
        // through unchanged, so `piece_rotations[gunbase][0] == π/2 - p`.
        if let Ok(gb) = pieces.gunbase.get(entity)
            && let Ok(animator) = pieces.animator.get(entity)
            && let Some(rot) = animator.rig.piece_rotations.get(gb.0)
        {
            let target_x = std::f32::consts::FRAC_PI_2 - target_pitch;
            if (rot[0] - target_x).abs() > AIM_PITCH_TOLERANCE {
                continue;
            }
        }

        // Aimer-piece gate (Byte). Mirrors `wait-for-turn aimer
        // around {y,x}-axis` in upstream's AimWeapon1.
        //
        // Why we don't compare against `animator.target_rotations`:
        // `aim_weapons_system` writes those values AFTER combat_system
        // in the same frame, so on the tick where a new target is
        // acquired the cached target rotation is stale (pointing at
        // last frame's target, or default). Reading it would let the
        // aimer-rotation gate pass while the piece is still mid-slew
        // toward the new target, which is exactly the "trail leaves
        // the gun in any direction" symptom. Compute the target axes
        // here from the live target position instead — same
        // arithmetic `aim_weapons_system` runs.
        if let Ok(ap) = pieces.aimer.get(entity)
            && let Ok(animator) = pieces.animator.get(entity)
            && let Some(rot) = animator.rig.piece_rotations.get(ap.0)
        {
            let body_yaw = attacker_gtf.rotation().to_euler(EulerRot::YXZ).0;
            let to_target_n = if horizontal_dist > 1e-3 {
                to_target_xz / horizontal_dist
            } else {
                Vec3::Z
            };
            let target_world_heading = to_target_n.x.atan2(to_target_n.z);
            let mut target_y = target_world_heading - body_yaw;
            while target_y > std::f32::consts::PI {
                target_y -= std::f32::consts::TAU;
            }
            while target_y < -std::f32::consts::PI {
                target_y += std::f32::consts::TAU;
            }
            let target_x = -std::f32::consts::FRAC_PI_2 - target_pitch;

            // Wrap the heading delta into (-π, π] so a 359° turn
            // doesn't read as "off by 359°" when the slew is one
            // frame from completion.
            let mut dy = (rot[1] - target_y).rem_euclid(std::f32::consts::TAU);
            if dy > std::f32::consts::PI {
                dy = std::f32::consts::TAU - dy;
            }
            let dx = (rot[0] - target_x).abs();
            if dy > AIM_HEADING_TOLERANCE || dx > AIM_PITCH_TOLERANCE {
                continue;
            }
        }

        // Why: upstream `sprayangle` is in Spring short-angle units;
        // small-angle offset at target plane ≈ tan(angle) × distance.
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

        // Hitscan lands now; traveling bolts defer via `delayed_hit`.
        let is_traveling = weapon_def.is_some_and(spring_tdf::WeaponDef::is_traveling);
        if !is_traveling {
            damage_queue.push(PendingDamage {
                target: Some(target_entity),
                attacker: entity,
                weapon: weapon_name.to_string(),
                impact_pos,
                attacker_distance: distance,
            });
        }
        commands.entity(entity).insert((
            AttackCooldown {
                remaining: cooldown,
            },
            JustFired,
        ));
        if !weapon_name.is_empty() {
            // Visual origin uses the resolved muzzle piece; range/LOS
            // checks above intentionally stay at unit center so
            // arm-length offsets don't flicker targeting.
            let visual_origin = muzzle_world_pos(
                entity,
                attacker_gtf,
                &pieces.muzzle,
                &pieces.animator,
                &pieces.piece_gtf,
            );
            let muzzle_ceg = unit_registry
                .preferred_muzzle_ceg(unit_type.0)
                .map(|s| std::borrow::Cow::Owned(s.to_string()));
            let delayed_hit = is_traveling.then_some(DelayedHitInfo {
                target: Some(target_entity),
                attacker: entity,
                attacker_distance: distance,
            });
            pending_attacks.events.push(AttackEvent {
                attacker_pos: visual_origin,
                target_pos: impact_pos,
                weapon_name: std::borrow::Cow::Owned(weapon_name.to_string()),
                muzzle_ceg,
                delayed_hit,
            });
        }

        let burst = weapon_def.map_or(0.0, |w| w.burst) as u32;
        if burst > 1 {
            let interval = weapon_def.map_or(0.1, |w| w.burst_rate.max(0.05));
            commands.entity(entity).insert(BurstFire {
                shots_remaining: burst - 1,
                interval,
                timer: interval,
                target: Some(target_entity),
                target_pos,
                weapon: weapon_name.to_string(),
                is_traveling,
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
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn attack_ground_system(
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    attackers: Query<
        (
            Entity,
            &UnitType,
            &GlobalTransform,
            &AttackGroundOrder,
            Option<&Deployable>,
            Option<&OpeningDelay>,
            Option<&WeaponBinding>,
        ),
        Without<Dying>,
    >,
    cooldowns: Query<&AttackCooldown>,
    pieces: PieceLookup,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    for (entity, unit_type, gtf, order, deployable, opening_delay, weapon_binding) in &attackers {
        // Same deploy / opening gates as `combat_system`. Player-issued
        // attack-ground orders MUST honour them too — otherwise the
        // player can force-fire a Pointer that's still folding open or a
        // byte whose blades haven't fanned yet, bypassing upstream's
        // `AimWeapon1`-returns-0 contract.
        if deployable.is_some_and(|d| d.state != DeployState::Open) {
            continue;
        }
        if opening_delay.is_some_and(|d| d.remaining > 0.0) {
            continue;
        }
        let (weapon_name, weapon_def) = match weapon_binding {
            Some(binding) => (
                weapon_registry.name(binding.0),
                Some(weapon_registry.by_id(binding.0)),
            ),
            None => {
                let name = unit_registry.weapon(unit_type.0);
                (name, weapon_registry.get(name))
            }
        };
        let Some(weapon_def) = weapon_def else {
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

        // Same alignment gates as `combat_system` — body / gunbase /
        // aimer per piece markers. Math mirrors `aim_weapons_system`.
        let to_target_xz = Vec3::new(
            order.pos.x - attacker_pos.x,
            0.0,
            order.pos.z - attacker_pos.z,
        );
        let horizontal_dist = to_target_xz.length();
        if deployable.is_some() && horizontal_dist > 1e-3 {
            let to_target_n = to_target_xz / horizontal_dist;
            let forward_xz = flat_forward(gtf.forward().as_vec3());
            let align = forward_xz.dot(to_target_n).clamp(-1.0, 1.0);
            if align.acos() > AIM_HEADING_TOLERANCE {
                continue;
            }
        }
        let dy = order.pos.y - attacker_pos.y;
        let direct_pitch = dy.atan2(horizontal_dist.max(1e-6));
        let arc_pitch = if arc_height > 0.0 && horizontal_dist > 1.0 {
            (4.0 * arc_height / horizontal_dist).atan()
        } else {
            0.0
        };
        let target_pitch = direct_pitch + arc_pitch;
        if let Ok(gb) = pieces.gunbase.get(entity)
            && let Ok(animator) = pieces.animator.get(entity)
            && let Some(rot) = animator.rig.piece_rotations.get(gb.0)
        {
            let target_x = std::f32::consts::FRAC_PI_2 - target_pitch;
            if (rot[0] - target_x).abs() > AIM_PITCH_TOLERANCE {
                continue;
            }
        }
        if let Ok(ap) = pieces.aimer.get(entity)
            && let Ok(animator) = pieces.animator.get(entity)
            && let Some(rot) = animator.rig.piece_rotations.get(ap.0)
            && let Some(target_rot) = animator.rig.target_rotations.get(ap.0)
        {
            let dy_axis = (rot[1] - target_rot[1]).abs();
            let dx_axis = (rot[0] - target_rot[0]).abs();
            if dy_axis > AIM_HEADING_TOLERANCE || dx_axis > AIM_PITCH_TOLERANCE {
                continue;
            }
        }

        // Fire.
        let visual_origin = muzzle_world_pos(
            entity,
            gtf,
            &pieces.muzzle,
            &pieces.animator,
            &pieces.piece_gtf,
        );
        let muzzle_ceg = unit_registry
            .preferred_muzzle_ceg(unit_type.0)
            .map(|s| std::borrow::Cow::Owned(s.to_string()));
        let is_traveling = weapon_def.is_traveling();
        let delayed_hit = is_traveling.then_some(DelayedHitInfo {
            target: None,
            attacker: entity,
            attacker_distance: dist,
        });
        pending_attacks.events.push(AttackEvent {
            attacker_pos: visual_origin,
            target_pos: order.pos,
            weapon_name: std::borrow::Cow::Owned(weapon_name.to_string()),
            muzzle_ceg,
            delayed_hit,
        });
        if !is_traveling {
            damage_queue.push(PendingDamage {
                target: None,
                attacker: entity,
                weapon: weapon_name.to_string(),
                impact_pos: order.pos,
                attacker_distance: dist,
            });
        }
        commands.entity(entity).insert((
            AttackCooldown {
                remaining: weapon_def.reload_time,
            },
            JustFired,
        ));

        // Queue the remaining burst shots exactly like `combat_system`
        // does on the auto-target path. Missing this was the reason a
        // player-commanded byte fired one shot per 2 s reload instead
        // of the authored 4-shot `burstrate=0.25` flurry: the initial
        // shot went through here, cooldown was set, and `tick_burst_fire`
        // never saw a `BurstFire` component because only the other fire
        // site inserted it.
        let burst = weapon_def.burst as u32;
        if burst > 1 {
            let interval = weapon_def.burst_rate.max(0.05);
            commands.entity(entity).insert(BurstFire {
                shots_remaining: burst - 1,
                interval,
                timer: interval,
                // Attack-ground has no primary-hit entity; AoE splash
                // at `target_pos` via `apply_damage` still reaches
                // everything in range.
                target: None,
                target_pos: order.pos,
                weapon: weapon_name.to_string(),
                is_traveling,
            });
        }
    }
}

/// How far a chase path's final waypoint may lag the target's current
/// position before the path is recomputed (elmos). Kept above the movement
/// arrival threshold (~8) so a *stationary* target doesn't thrash the
/// per-frame pathfinding budget with identical repaths.
pub const CHASE_REPATH_DISTANCE: f32 = 24.0;

/// Right-click attack order ([`AttackTargetOrder`]): chase `target` until
/// inside weapon range, then hold and let [`combat_system`]'s auto-attack
/// do the shooting.
///
/// While chasing, the path is only recomputed once the old path's endpoint
/// lags the target by more than [`CHASE_REPATH_DISTANCE`] — repathing a
/// moving chase every frame would starve the `PATHFIND_BUDGET_PER_FRAME`
/// budget and freeze the rest of the army. Static units (buildings) drop
/// the order when the target is unreachable.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn attack_target_system(
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    attackers: Query<
        (
            Entity,
            &UnitType,
            &UnitStats,
            &GlobalTransform,
            &AttackTargetOrder,
            Option<&Deployable>,
            Option<&OpeningDelay>,
            Option<&WeaponBinding>,
        ),
        Without<Dying>,
    >,
    target_q: Query<&GlobalTransform, Without<Dying>>,
    move_path_q: Query<&crate::interaction::movement::MovePath>,
    mut commands: Commands,
) {
    for (
        entity,
        unit_type,
        stats,
        gtf,
        order,
        deployable,
        opening_delay,
        weapon_binding,
    ) in &attackers
    {
        // Same deploy / opening gates as `attack_ground_system`.
        if deployable.is_some_and(|d| d.state != DeployState::Open) {
            continue;
        }
        if opening_delay.is_some_and(|d| d.remaining > 0.0) {
            continue;
        }

        // Resolve weapon range exactly like `combat_system`.
        let weapon_def = match weapon_binding {
            Some(binding) => Some(weapon_registry.by_id(binding.0)),
            None => {
                let name = unit_registry.weapon(unit_type.0);
                if name.is_empty() {
                    None
                } else {
                    weapon_registry.get(name)
                }
            }
        };
        let range = weapon_def.map_or(0.0, |w| w.range);

        let Ok(target_gtf) = target_q.get(order.target) else {
            // Target died / despawned: stand down and clear movement.
            commands
                .entity(entity)
                .remove::<AttackTargetOrder>()
                .remove::<crate::interaction::movement::MoveTarget>()
                .remove::<crate::interaction::movement::MovePath>();
            continue;
        };
        let target_pos = target_gtf.translation();
        let dist = gtf.translation().distance(target_pos);

        if stats.speed <= 0.0 && dist > range {
            // Immobile unit (armed building) can never close the gap —
            // drop the order instead of churning MoveTarget insert/remove
            // against `movement_system` every frame.
            commands.entity(entity).remove::<AttackTargetOrder>();
            continue;
        }

        if dist > range {
            // Out of range: chase. Only repath when the current path's
            // endpoint lags the target beyond `CHASE_REPATH_DISTANCE` —
            // or when there is no path at all (fresh order / budget stall).
            let stale = move_path_q.get(entity).map_or(true, |p| {
                p.waypoints
                    .last()
                    .is_none_or(|w| w.distance(target_pos) > CHASE_REPATH_DISTANCE)
            });
            if stale {
                commands
                    .entity(entity)
                    .insert(crate::interaction::movement::MoveTarget(target_pos))
                    .remove::<crate::interaction::movement::MovePath>();
            }
        } else {
            // In range: hold position and fire.
            commands
                .entity(entity)
                .remove::<crate::interaction::movement::MoveTarget>()
                .remove::<crate::interaction::movement::MovePath>();
        }
    }
}

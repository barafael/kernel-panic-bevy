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
use super::weapon_fx::{AttackEvent, DelayedHitInfo, PendingAttacks};
use crate::rng::next_signed;
use crate::terrain::heightmap::Heightmap;

mod aim;
mod damage;
mod lifecycle;

pub use aim::{
    AIM_HEADING_TOLERANCE, AIM_PITCH_TOLERANCE, AimTarget, BYTE_OPEN_DURATION, DeployState,
    Deployable, OpeningDelay, aim_weapons_system, tick_deploy_state, tick_opening_delay,
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
/// cached target. Mirrors Spring's `CWeapon::lastTargetRetry + 65`
/// guard (`rts/Sim/Weapons/Weapon.cpp`) which re-scans at most every
/// ~2.17 s (65 sim frames @ 30 fps). We tighten it to 0.25 s so new
/// threats are picked up within a few frames while still amortising
/// the spatial sweep ~15× at 60 fps. Cache is invalidated immediately
/// whenever the cached target dies or leaves weapon range.
const TARGET_RESCAN_INTERVAL: f32 = 0.25;

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
    pub animator: Query<'w, 's, &'static CobAnimator>,
    pub piece_gtf: Query<'w, 's, &'static GlobalTransform, Without<UnitType>>,
    pub gunbase: Query<'w, 's, &'static crate::units::assets::animation::GunbasePiece>,
    pub aimer: Query<'w, 's, &'static crate::units::assets::animation::AimerPiece>,
}

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
            Option<&OpeningDelay>,
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
    ) in &attackers
    {
        // Deployable units (Pointer) can only fire while fully open. They
        // can still *aim* while opening (so the gun is pointed when Open
        // completes), but firing is gated on the open state.
        let fire_blocked_by_deploy = deployable.is_some_and(|d| d.state != DeployState::Open);
        // Byte holds fire until its `Open()` script finishes fanning the
        // blades out — upstream's `isOpen` gate, mirrored host-side via
        // `OpeningDelay`. Combat & attack-ground both honour it so the
        // 4-shot MegaBeam burst can't leave while firing positions are
        // still folded together at the unit center.
        let fire_blocked_by_opening = opening_delay.is_some_and(|d| d.remaining > 0.0);

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

        // Try the cached target first. While it's still alive, in
        // range, and the cache hasn't aged out, we skip the spatial
        // sweep entirely. This mirrors Spring's `lastTargetRetry`
        // guard (see `TARGET_RESCAN_INTERVAL`).
        let mut best: Option<(Entity, Vec3, f32)> = target_pick
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

        // Always stamp the aim target while we have a candidate — that
        // lets `aim_weapons_system` keep steering the body/gun even
        // while we're still on cooldown or still opening, so the weapon
        // is already on-target when it's allowed to fire.
        commands.entity(entity).insert(AimTarget {
            pos: target_pos,
            arc_height,
        });

        if fire_blocked_by_deploy || fire_blocked_by_opening {
            continue;
        }

        // Skip if still on cooldown.
        if let Ok(cd) = cooldowns.get(entity)
            && cd.remaining > 0.0
        {
            continue;
        }

        // Aim-alignment gate. Up to three checks, each guarded on which
        // piece markers the unit actually has — Pointer (Deployable +
        // GunbasePiece) needs body heading + gunbase pitch; Byte
        // (AimerPiece, no Deployable) needs aimer Y + aimer X. Without
        // these, a unit fires mid-slew and beams visibly leave at the
        // wrong angle while the aim catches up after the projectile is
        // already flying.
        //
        // Mirrors upstream's `AimWeapon1` returning 0 (not ready) until
        // `wait-for-turn` completes — firing only proceeds once the
        // script returns 1.
        let to_target_xz = Vec3::new(
            target_pos.x - attacker_pos.x,
            0.0,
            target_pos.z - attacker_pos.z,
        );
        let horizontal_dist = to_target_xz.length();

        // Body-heading gate (Pointer): wait for the body to finish
        // turning. Deployable is the marker for "this unit's whole body
        // aims" — Byte uses aimer-piece aim and skips this check.
        if deployable.is_some() && horizontal_dist > 1e-3 {
            let to_target_n = to_target_xz / horizontal_dist;
            let forward = attacker_gtf.forward().as_vec3();
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

        // Compute the elevation `aim_weapons_system` uses as the gunbase
        // / aimer X target this frame so the gate uses the same
        // arithmetic and converges cleanly when the slew finishes.
        let dy = target_pos.y - attacker_pos.y;
        let direct_pitch = dy.atan2(horizontal_dist.max(1e-6));
        let arc_pitch = if arc_height > 0.0 && horizontal_dist > 1.0 {
            (4.0 * arc_height / horizontal_dist).atan()
        } else {
            0.0
        };
        let target_pitch = direct_pitch + arc_pitch;

        // Gunbase pitch gate (Pointer): aim_weapons_system writes
        // `target_x = π/2 - pitch` for gunbase, matching pointer.bos's
        // `(<-90>-p)` convention. Compare against the live piece rotation.
        if let Ok(gb) = pieces.gunbase.get(entity)
            && let Ok(animator) = pieces.animator.get(entity)
            && let Some(rot) = animator.piece_rotations.get(gb.0)
        {
            let target_x = std::f32::consts::FRAC_PI_2 - target_pitch;
            if (rot[0] - target_x).abs() > AIM_PITCH_TOLERANCE {
                continue;
            }
        }

        // Aimer alignment gate (Byte): host-side aim writes both axes of
        // `target_rotations`; compare current rotations against those
        // targets. This is what `AimWeapon1`'s `wait-for-turn aimer
        // around y-axis` / `around x-axis` pair does in upstream.
        if let Ok(ap) = pieces.aimer.get(entity)
            && let Ok(animator) = pieces.animator.get(entity)
            && let Some(rot) = animator.piece_rotations.get(ap.0)
            && let Some(target_rot) = animator.target_rotations.get(ap.0)
        {
            let dy_axis = (rot[1] - target_rot[1]).abs();
            let dx_axis = (rot[0] - target_rot[0]).abs();
            if dy_axis > AIM_HEADING_TOLERANCE || dx_axis > AIM_PITCH_TOLERANCE {
                continue;
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

        // Hitscan lands this frame via `DamageQueue`; traveling bolts
        // defer their hit via `AttackEvent::delayed_hit` — the visual
        // spawned by `spawn_weapon_visuals` carries the payload and
        // fires it on impact (see [`crate::units::weapon_fx`]).
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
            let delayed_hit = is_traveling.then(|| DelayedHitInfo {
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
                arc_height,
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
#[allow(clippy::too_many_arguments)]
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
        ),
        Without<Dying>,
    >,
    cooldowns: Query<&AttackCooldown>,
    pieces: PieceLookup,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    for (entity, unit_type, gtf, order, deployable, opening_delay) in &attackers {
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

        // Same alignment gates as `combat_system` — body-heading for
        // Deployable, gunbase pitch for GunbasePiece, aimer-axis for
        // AimerPiece. The math mirrors `aim_weapons_system` so the
        // gate converges cleanly once the slew finishes.
        let to_target_xz = Vec3::new(
            order.pos.x - attacker_pos.x,
            0.0,
            order.pos.z - attacker_pos.z,
        );
        let horizontal_dist = to_target_xz.length();
        if deployable.is_some() && horizontal_dist > 1e-3 {
            let to_target_n = to_target_xz / horizontal_dist;
            let forward = gtf.forward().as_vec3();
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
            && let Some(rot) = animator.piece_rotations.get(gb.0)
        {
            let target_x = std::f32::consts::FRAC_PI_2 - target_pitch;
            if (rot[0] - target_x).abs() > AIM_PITCH_TOLERANCE {
                continue;
            }
        }
        if let Ok(ap) = pieces.aimer.get(entity)
            && let Ok(animator) = pieces.animator.get(entity)
            && let Some(rot) = animator.piece_rotations.get(ap.0)
            && let Some(target_rot) = animator.target_rotations.get(ap.0)
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
        let delayed_hit = is_traveling.then(|| DelayedHitInfo {
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
            JustFired {
                target_pos: order.pos,
                arc_height,
            },
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
                arc_height,
                is_traveling,
            });
        }
    }
}

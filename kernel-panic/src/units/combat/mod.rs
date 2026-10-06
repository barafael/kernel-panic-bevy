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

use super::assets::animation::{MuzzlePiece, PieceEmit, UnitAnimator};
use super::components::{TeamId, UnitStats, UnitType};
use super::content::definitions::UnitKind;
use super::content::unit_registry::UnitRegistry;
use super::content::weapons::{WeaponId, WeaponRegistry};
use super::lifecycle::bookkeeping::NoAutoTarget;
use super::lifecycle::script_triggers::JustFired;
use super::mechanics::cloak::{Cloaked, DetectedBy};
use super::mechanics::worm::{AutoHold, WormSplash, queue_wormsplash};
use super::spatial::SpatialIndex;
use super::weapon_fx::{AttackEvent, DelayedHitInfo, PendingAttacks};
use crate::rng::next_signed;
use crate::sim::{SHORT_ANGLE_TO_RAD, SIMULATION_HZ, angle_delta, secs_to_frames};
use crate::terrain::heightmap::Heightmap;

mod aim;
mod collision_volume;
mod damage;
mod lifecycle;

pub use collision_volume::CollisionVolume;

pub use aim::{
    AIM_HEADING_TOLERANCE, AIM_PITCH_TOLERANCE, AimLaunch, AimScript, AimTarget, Byte, ByteOpen,
    DeployState, Deployable, aim_weapons_system, drive_aim_script, sync_byte_fold_state,
    tick_deploy_state,
};
pub(crate) use damage::splash_falloff;
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

/// Seconds between full spatial scans for a unit that already has a
/// cached target. Matches Spring's `CWeapon::lastTargetRetry + 65`
/// guard (`rts/Sim/Weapons/Weapon.cpp`) which re-scans at most every
/// ~2 s (65 sim frames @ 30 fps). Cache is invalidated immediately
/// whenever the cached target dies or leaves weapon range, so this
/// only controls how quickly a unit abandons a valid target for a
/// newly-arrived closer one — not how fast it reacts to kills.
const TARGET_RESCAN_INTERVAL: f32 = crate::sim::frames_to_secs(65.0);

/// Cached auto-target for an armed unit. While present and the target
/// is still alive + in-range, `combat_system` skips its spatial
/// `query_radius` sweep — the dominant per-frame cost in big battles.
#[derive(Component)]
pub struct TargetCache {
    pub target: Entity,
    pub expires_at: f32,
}

/// Grouped queries for target validation, bundled into one `SystemParam`
/// so `combat_system` stays under Bevy's 16-param limit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct TargetCachePick<'w, 's> {
    pub alive: Query<'w, 's, &'static GlobalTransform, (With<UnitType>, Without<Dying>)>,
    /// Detection mask of every currently cloaked unit — a cached or
    /// designated target that burrows out of detector range is dropped.
    pub cloaked: Query<'w, 's, &'static DetectedBy, With<Cloaked>>,
}

impl TargetCachePick<'_, '_> {
    /// Position of `target` if it is alive and `team` can currently see
    /// it (not an undetected cloaked unit).
    fn visible_pos(&self, target: Entity, team: u8) -> Option<Vec3> {
        if self.cloaked.get(target).is_ok_and(|d| !d.contains(team)) {
            return None;
        }
        self.alive
            .get(target)
            .ok()
            .map(GlobalTransform::translation)
    }
}

/// The player's explicit targets for a unit: the `T` designation and the
/// right-click attack order. Both override auto-acquisition once in
/// range (Spring's `CMD_ATTACK` / `CMD_SET_TARGET` set the weapon's
/// target directly), and both still fire under hold-fire.
#[derive(bevy::ecs::system::SystemParam)]
pub struct ExplicitTargets<'w, 's> {
    pub forced: Query<'w, 's, &'static ForcedTarget>,
    pub ordered: Query<'w, 's, &'static AttackTargetOrder>,
}

/// Grouped piece-lookup queries for muzzle position, body animator,
/// piece local transforms, and the `GunbasePiece` / `AimerPiece`
/// marker components. Lets the combat systems stay under Bevy's
/// 16-param system limit.
///
/// The animator is mutable because every projectile runs the driver's
/// `Shot1` / `QueryWeapon1` synchronously ([`fire_salvo_shot`]).
#[derive(bevy::ecs::system::SystemParam)]
pub struct PieceLookup<'w, 's> {
    pub muzzle: Query<'w, 's, &'static MuzzlePiece>,
    pub animator: Query<'w, 's, &'static mut UnitAnimator>,
    pub piece_tf: Query<
        'w,
        's,
        (
            &'static Transform,
            &'static ChildOf,
            Option<&'static PieceEmit>,
        ),
        Without<UnitType>,
    >,
    pub gunbase: Query<'w, 's, &'static crate::units::assets::animation::GunbasePiece>,
    pub aimer: Query<'w, 's, &'static crate::units::assets::animation::AimerPiece>,
    pub mover: Query<'w, 's, &'static crate::interaction::ground_move::GroundMover>,
}

/// World-space weapon muzzle of a unit: position and emit direction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Muzzle {
    pub pos: Vec3,
    pub dir: Vec3,
}

impl PieceLookup<'_, '_> {
    /// Upstream `CWeapon::UpdateWeaponVectors` (`Weapon.cpp:282-291`):
    /// the `QueryWeapon` piece's emit point and direction
    /// ([`PieceEmit`]) through the piece's model-space transform and the
    /// unit's full world transform — so a banking / pitching flyer or a
    /// spinning wing carries the muzzle with it.
    ///
    /// Composed from the pieces' *local* `Transform`s up to the unit
    /// root instead of the pieces' `GlobalTransform`s: those are last
    /// render frame's propagation of the render-interpolated pose, a
    /// tick stale (a Flow's gp pieces sweep 6°/tick on the spinning
    /// wings). `restore_sim_pose` puts the true sim pose back into every
    /// piece `Transform` at the start of the tick.
    ///
    /// Falls back to the unit origin / body front when the unit has no
    /// resolved [`MuzzlePiece`] (melee, BuildLaser, unmapped scripts).
    pub fn muzzle(&self, attacker: Entity, attacker_gtf: &GlobalTransform) -> Muzzle {
        let fallback = Muzzle {
            pos: attacker_gtf.translation(),
            dir: attacker_gtf.forward().as_vec3(),
        };
        if self.muzzle.get(attacker).is_err() {
            return fallback;
        }
        let Ok(animator) = self.animator.get(attacker) else {
            return fallback;
        };
        let Some(&piece) = animator.rig.piece_entities.get(animator.rig.muzzle) else {
            return fallback;
        };
        let Ok((_, _, emit)) = self.piece_tf.get(piece) else {
            return fallback;
        };
        let emit = emit.copied().unwrap_or_default();
        // Accumulate local transforms piece → … → model root → unit.
        let mut local = bevy::math::Affine3A::IDENTITY;
        let mut cur = piece;
        for _ in 0..64 {
            if cur == attacker {
                let world = attacker_gtf.affine() * local;
                return Muzzle {
                    pos: world.transform_point3(emit.pos),
                    dir: world
                        .transform_vector3(emit.dir)
                        .try_normalize()
                        .unwrap_or(fallback.dir),
                };
            }
            let Ok((tf, parent, _)) = self.piece_tf.get(cur) else {
                break;
            };
            local = tf.compute_affine() * local;
            cur = parent.parent();
        }
        fallback
    }
}

/// Everything a salvo's shots share, captured when the salvo opens.
#[derive(Clone, Copy, Debug)]
pub struct SalvoShot {
    pub attacker: Entity,
    pub kind: UnitKind,
    pub weapon: WeaponId,
    /// Unit target (`None` for attack-ground).
    pub target: Option<Entity>,
    /// Aim point, including spray.
    pub impact_pos: Vec3,
    pub is_traveling: bool,
    /// `projectiles=` — projectiles per salvo shot.
    pub projectiles: u32,
}

/// Upstream `CWeapon::UpdateSalvo` for one salvo shot
/// (`Weapon.cpp:541-608`): for each of the weapon's `projectiles`,
/// `Shot1` → `QueryWeapon1` → `FireImpl` from the freshly resolved
/// muzzle; after the salvo's last shot, `EndBurst1`.
#[allow(clippy::too_many_arguments)]
pub(super) fn fire_salvo_shot(
    shot: &SalvoShot,
    attacker_gtf: &GlobalTransform,
    last_of_salvo: bool,
    pieces: &mut PieceLookup,
    unit_registry: &UnitRegistry,
    pending_attacks: &mut PendingAttacks,
    damage_queue: &mut DamageQueue,
) {
    let muzzle_ceg = crate::units::assets::animation::fire_weapon_sfx(shot.kind)
        .and_then(|index| unit_registry.sfx_type(shot.kind, index))
        .map(std::sync::Arc::<str>::from);
    let distance = attacker_gtf.translation().distance(shot.impact_pos);
    for _ in 0..shot.projectiles.max(1) {
        if let Ok(mut animator) = pieces.animator.get_mut(shot.attacker) {
            let UnitAnimator { rig, driver, .. } = &mut *animator;
            driver.shot(rig);
        }
        let muzzle = pieces.muzzle(shot.attacker, attacker_gtf);
        if !shot.is_traveling {
            damage_queue.push(PendingDamage {
                target: shot.target,
                attacker: shot.attacker,
                weapon: shot.weapon,
                impact_pos: shot.impact_pos,
                attacker_distance: distance,
            });
        }
        pending_attacks.events.push(AttackEvent {
            attacker_pos: muzzle.pos,
            target_pos: shot.impact_pos,
            weapon_id: shot.weapon,
            muzzle_ceg: muzzle_ceg.clone(),
            delayed_hit: shot.is_traveling.then_some(DelayedHitInfo {
                target: shot.target,
                attacker: shot.attacker,
                attacker_distance: distance,
                muzzle_dir: muzzle.dir,
            }),
            build_arc: false,
        });
    }
    if last_of_salvo && let Ok(mut animator) = pieces.animator.get_mut(shot.attacker) {
        let UnitAnimator { rig, driver, .. } = &mut *animator;
        driver.end_burst(rig);
    }
}

/// Spring sim frame number of the current fixed tick (`gs->frameNum`),
/// from the fixed clock (30 Hz — `GAME_SPEED`).
pub(crate) fn sim_frame(time: &Time) -> u64 {
    (time.elapsed_secs_f64() * SIMULATION_HZ).round() as u64
}

/// `salvoDelay = int(burstRate * GAME_SPEED)` (`WeaponLoader.cpp:171`):
/// whole sim frames between a salvo's shots.
pub(crate) fn salvo_delay_frames(burst_rate: f32) -> u64 {
    secs_to_frames(burst_rate).max(0.0) as u64
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

/// Armed units auto-attack the nearest enemy in range.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn combat_system(
    time: Res<Time>,
    mut cooldowns: Query<(Entity, &mut AttackCooldown)>,
    mut attackers: Query<
        (
            Entity,
            &UnitType,
            &UnitStats,
            &TeamId,
            &GlobalTransform,
            Option<&Deployable>,
            Option<&AimScript>,
            Option<&WeaponBinding>,
            Has<Cloaked>,
            Option<&AutoHold>,
            Option<&WormSplash>,
            Option<&mut AimTarget>,
            Option<&mut TargetCache>,
        ),
        (
            Without<Dying>,
            Without<super::lifecycle::spawning::Emerging>,
            Without<Stunned>,
            // Unarmed / command-fire kinds (tagged by
            // `bookkeeping::tag_unit_kinds`) never reach the pick below;
            // an untagged unit still takes the `range == 0` exit.
            Without<NoAutoTarget>,
        ),
    >,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    weapon_registry: Res<WeaponRegistry>,
    mut pending_attacks: ResMut<PendingAttacks>,
    unit_registry: Res<UnitRegistry>,
    spatial: Res<SpatialIndex>,
    explicit: ExplicitTargets,
    heightmap: Option<Res<Heightmap>>,
    mut pieces: PieceLookup,
    target_pick: TargetCachePick,
    mut rng: Local<u32>,
    // `(dist_sq, entity, pos)` scratch for the deferred LOS pick.
    mut los_candidates: Local<Vec<(f32, Entity, Vec3)>>,
) {
    if *rng == 0 {
        // First-tick seed only: a non-zero xorshift32 state can never
        // emit 0, so this guard cannot re-trigger mid-game — it just
        // keeps the `Local<u32>` default from locking the PRNG up.
        *rng = 0xDEADBEEF;
    }
    let dt = time.delta_secs();
    let now = time.elapsed_secs();

    // Tick cooldowns. Removed at zero (mirroring
    // `tick_command_fire_cooldown`): the component used to linger at
    // 0.0 forever on every unit that had ever fired, rewritten — and
    // flagged changed — every tick for the rest of the match. All
    // readiness checks treat absence as ready (`map_or(true, …)` /
    // `get(..).is_none_or(|cd| cd.remaining <= 0.0)`), so removal is
    // observably identical.
    for (entity, mut cd) in &mut cooldowns {
        cd.remaining = (cd.remaining - dt).max(0.0);
        if cd.remaining <= 0.0 {
            commands.entity(entity).remove::<AttackCooldown>();
        }
    }

    // Why: no `damage_queue.clear()` here. The queue's lifecycle is
    // write-then-drain: `apply_damage` (Resolve) drains it empty, and
    // producers may push on either side of that drain in the same frame
    // — `tick_kamikaze` runs before this system in Simulate, and
    // `death_system` pushes `ExplodeAs` self-hits after `apply_damage`
    // in Resolve. Clearing here destroyed both: kamikaze splash was
    // wiped before any drain, and death-AoE sat in the queue until the
    // next frame's clear killed it. The drain in `apply_damage` is the
    // only consumer; a frame's damage is always delivered by the next
    // Resolve.

    for (
        entity,
        unit_type,
        stats,
        attacker_team,
        attacker_gtf,
        deployable,
        aim_script,
        weapon_binding,
        cloaked,
        autohold,
        worm_splash,
        mut aim_slot,
        mut cache_slot,
    ) in &mut attackers
    {
        // Why: a cloaked unit without a surfacing script (Logic Bomb)
        // never fires — it detonates via `tick_kamikaze`. A cloaked Worm
        // may still pick a target (below) and surfaces to bite.
        if cloaked && autohold.is_none() {
            continue;
        }
        // worm.bos `STANDINGFIREORDERS=0` while cloaked with AutoHold on:
        // hold fire — only the player's explicit targets engage.
        let hold_fire = cloaked && autohold.is_some_and(|a| a.0);

        // Deployable: keep aiming through Opening so the gun is on
        // target when the state flips, but only fire while Open.
        let fire_blocked_by_deploy = deployable.is_some_and(|d| d.state != DeployState::Open);

        // Resolve weapon stats via the cached binding when present.
        // Falling back to the registry name lookup for legacy paths
        // (e.g. units that haven't been migrated to `WeaponBinding`)
        // keeps behaviour identical for any unit class that bypasses
        // `spawn_unit` — known callers cover every armed unit today
        // but the safety net is cheap.
        // The display name is no longer needed here — every consumer
        // (damage queue, fx events, burst component) carries the id.
        let (weapon_id, weapon_def) = match weapon_binding {
            Some(binding) => (Some(binding.0), Some(weapon_registry.by_id(binding.0))),
            None => {
                let name = unit_registry.weapon(unit_type.0);
                if name.is_empty() {
                    (None, None)
                } else {
                    (weapon_registry.intern(name), weapon_registry.get(name))
                }
            }
        };
        let range = weapon_def.map_or(0.0, |w| w.range);
        let cooldown = weapon_def.map_or(0.0, |w| w.reload_time);

        // Command-fire weapons (NX Flag, Infection, …) need an
        // explicit order; auto-target must skip them.
        let command_fire = weapon_def.is_some_and(|w| w.command_fire);

        if range == 0.0 || command_fire {
            clear(&mut commands, entity, &aim_slot);
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
        // with the turret (no fire, no chase); dead or hidden under an
        // undetected cloak → drop the mark (upstream can't target what
        // it can't see).
        if let Ok(forced) = explicit.forced.get(entity) {
            match target_pick.visible_pos(forced.0, attacker_team.0) {
                Some(t_pos) => {
                    let dist_sq = attacker_pos.distance_squared(t_pos);
                    if dist_sq <= range_sq {
                        best = Some((forced.0, t_pos, dist_sq));
                    } else {
                        let aim = AimTarget {
                            pos: t_pos,
                            launch: AimLaunch::of(weapon_def),
                        };
                        put(&mut commands, entity, &mut aim_slot, aim);
                        clear(&mut commands, entity, &cache_slot);
                        continue;
                    }
                }
                None => {
                    commands.entity(entity).remove::<ForcedTarget>();
                }
            }
        }

        // Right-click attack order: once `attack_target_system` has
        // closed to range, shoot the ordered unit rather than whatever
        // is nearest. Out of range the chase continues and auto-
        // acquisition below may engage passers-by.
        if best.is_none()
            && let Ok(order) = explicit.ordered.get(entity)
            && let Some(t_pos) = target_pick.visible_pos(order.target, attacker_team.0)
        {
            let dist_sq = attacker_pos.distance_squared(t_pos);
            if dist_sq <= range_sq {
                best = Some((order.target, t_pos, dist_sq));
            }
        }

        if best.is_none() && !hold_fire {
            best = cache_slot
                .as_deref()
                .filter(|cache| cache.expires_at > now)
                .and_then(|cache| {
                    let pos = target_pick.visible_pos(cache.target, attacker_team.0)?;
                    let dist_sq = attacker_pos.distance_squared(pos);
                    (dist_sq <= range_sq).then_some((cache.target, pos, dist_sq))
                });
        }

        if best.is_none() && !hold_fire {
            // Direct-fire weapons defer the LOS ray march: candidates
            // passing the cheap filters are collected, ranked exactly as
            // the inline pick below ranks them, and ray-marched in that
            // order — only the winner (and the blocked candidates ahead
            // of it) pays for `has_line_of_sight`, not the whole crowd.
            los_candidates.clear();
            spatial.query_radius(attacker_pos, range, |candidate| {
                if !candidate.hp_positive {
                    return;
                }
                if candidate.team == attacker_team.0 {
                    return;
                }
                // Undetected cloaked units (Worms, Logic Bombs) are
                // invisible to this team's weapons.
                if !candidate.targetable_by(attacker_team.0) {
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
                // Upstream `OnlyTargetCategory1` / `BadTargetCategory1`:
                // Bits/Bytes/Packets ignore buildings until ordered not
                // to, the Dos beam only ever looks at mobile units, and
                // artillery won't chase FAST spam. Manual attack orders
                // bypass this via the `can_attack` gate at order time.
                if !unit_registry.auto_target_allowed(unit_type.0, candidate.kind) {
                    return;
                }
                if enforce_los {
                    los_candidates.push((dist_sq, candidate.entity, candidate.pos));
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
            if enforce_los && let Some(hm) = heightmap.as_deref() {
                // Stable sort: equal distances keep the scan order, so
                // the same unit wins as the strict `<` / `>` pick.
                if prefer_distant {
                    los_candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
                } else {
                    los_candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
                }
                best = los_candidates
                    .iter()
                    .find(|(_, _, pos)| {
                        hm.has_line_of_sight(
                            attacker_pos + Vec3::Y * LOS_MUZZLE_HEIGHT,
                            *pos + Vec3::Y * LOS_MUZZLE_HEIGHT,
                            LOS_MARGIN,
                        )
                    })
                    .map(|&(dist_sq, entity, pos)| (entity, pos, dist_sq));
            }
        }

        let Some((target_entity, target_pos, _)) = best else {
            clear(&mut commands, entity, &aim_slot);
            clear(&mut commands, entity, &cache_slot);
            continue;
        };
        let cache = TargetCache {
            target: target_entity,
            expires_at: now + TARGET_RESCAN_INTERVAL,
        };
        put(&mut commands, entity, &mut cache_slot, cache);

        let launch = AimLaunch::of(weapon_def);

        // Stamp aim target unconditionally so `aim_weapons_system`
        // keeps steering through cooldown/opening — the weapon is on
        // target the moment firing is allowed.
        let aim = AimTarget {
            pos: target_pos,
            launch,
        };
        put(&mut commands, entity, &mut aim_slot, aim);

        // A cloaked Worm never bites from under cover: the aim request
        // above surfaces it (`tick_worm_surfacing`, worm.bos AimWeapon1
        // `CLOAKED=FALSE`) and the bite commits on a later tick.
        if fire_blocked_by_deploy || cloaked {
            continue;
        }
        // Why: upstream `AimWeapon1` contract — return 1 ⇒ allowed
        // to fire; anything else ⇒ barrel not on-target yet.
        if aim_script.is_some_and(|a| !a.ready) {
            continue;
        }

        if let Ok((_, cd)) = cooldowns.get(entity)
            && cd.remaining > 0.0
        {
            continue;
        }

        // Aim-alignment gate, gated on which piece markers the unit
        // declared. Without it, beams leave mid-slew before the gun
        // is on target. Same arithmetic as `aim_weapons_system` so
        // the two converge cleanly.
        if !aim_gates_pass(
            entity,
            unit_type.0,
            attacker_gtf,
            target_pos,
            launch,
            deployable.is_some(),
            &pieces,
        ) {
            continue;
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
        commands.entity(entity).insert((
            AttackCooldown {
                remaining: cooldown,
            },
            JustFired,
        ));
        if let (Some(weapon_id), Some(weapon_def)) = (weapon_id, weapon_def) {
            open_salvo(
                SalvoShot {
                    attacker: entity,
                    kind: unit_type.0,
                    weapon: weapon_id,
                    target: Some(target_entity),
                    impact_pos,
                    is_traveling,
                    projectiles: weapon_def.projectiles as u32,
                },
                weapon_def,
                attacker_gtf,
                sim_frame(&time),
                &mut pieces,
                &unit_registry,
                &mut pending_attacks,
                &mut damage_queue,
                &mut commands,
            );
        }
        if let Some(splash) = worm_splash {
            queue_wormsplash(
                &mut damage_queue,
                entity,
                splash.0,
                impact_pos,
                attacker_pos,
            );
        }
    }
}

/// Overwrite `slot`'s component in place, or insert it when absent —
/// units that keep a target skip the per-tick archetype move.
fn put<C: Component<Mutability = bevy::ecs::component::Mutable>>(
    commands: &mut Commands,
    entity: Entity,
    slot: &mut Option<Mut<C>>,
    value: C,
) {
    match slot {
        Some(c) => **c = value,
        None => {
            commands.entity(entity).insert(value);
        }
    }
}

/// Remove `C` only when the unit actually carries it.
fn clear<C: Component>(commands: &mut Commands, entity: Entity, slot: &Option<Mut<C>>) {
    if slot.is_some() {
        commands.entity(entity).remove::<C>();
    }
}

/// Fire a salvo's first shot now and, for `burst > 1`, queue the rest
/// as a [`BurstFire`] spaced `salvoDelay` sim frames apart — upstream
/// `CWeapon::UpdateFire` (`salvoLeft = salvoSize`, `nextSalvo = now`)
/// followed by the same frame's `UpdateSalvo`.
#[allow(clippy::too_many_arguments)]
fn open_salvo(
    shot: SalvoShot,
    weapon_def: &spring_tdf::WeaponDef,
    attacker_gtf: &GlobalTransform,
    frame: u64,
    pieces: &mut PieceLookup,
    unit_registry: &UnitRegistry,
    pending_attacks: &mut PendingAttacks,
    damage_queue: &mut DamageQueue,
    commands: &mut Commands,
) {
    let burst = (weapon_def.burst as u32).max(1);
    fire_salvo_shot(
        &shot,
        attacker_gtf,
        burst == 1,
        pieces,
        unit_registry,
        pending_attacks,
        damage_queue,
    );
    if burst > 1 {
        let delay = salvo_delay_frames(weapon_def.burst_rate);
        commands.entity(shot.attacker).insert(BurstFire {
            shot,
            shots_remaining: burst - 1,
            last_frame: frame,
            next_frame: frame.saturating_add(delay),
            salvo_delay: delay,
        });
    }
}

/// The aim-before-fire gates for units whose script doesn't gate
/// itself: body heading (Deployable — the Pointer turns its whole body),
/// the Pointer's `gunbase` pitch, the Byte's `aimer` yaw/pitch. Uses the
/// same unit-relative [`aim::local_aim_angles`] the aim script receives,
/// so gate and slew converge on the same numbers.
#[allow(clippy::too_many_arguments)]
fn aim_gates_pass(
    entity: Entity,
    kind: UnitKind,
    attacker_gtf: &GlobalTransform,
    target_pos: Vec3,
    launch: AimLaunch,
    deployable: bool,
    pieces: &PieceLookup,
) -> bool {
    let (heading, pitch) = aim::local_aim_angles(
        attacker_gtf.rotation(),
        target_pos - attacker_gtf.translation(),
        launch,
    );
    let to_target_xz = (target_pos - attacker_gtf.translation()) * Vec3::new(1.0, 0.0, 1.0);

    // Body-heading gate (Pointer): Deployable units rotate the whole
    // body to aim, so wait for that turn to finish. Byte uses an aimer
    // piece and skips this branch.
    if deployable && to_target_xz.length() > 1e-3 {
        // A ground unit's yaw is its mover heading (what `set HEADING`
        // and `aim_weapons_system` turn); the body's forward vector
        // projected to the ground drifts from it by several degrees on
        // a slope, which kept Pointers on ramps from ever passing.
        let error = match pieces.mover.get(entity) {
            Ok(m) => (crate::sim::Heading::from_vector(to_target_xz.x, to_target_xz.z)
                .wrapping_sub(m.heading) as f32
                * crate::sim::SHORT_ANGLE_TO_RAD)
                .abs(),
            Err(_) => {
                let forward_xz = flat_forward(attacker_gtf.forward().as_vec3());
                forward_xz
                    .dot(to_target_xz.normalize())
                    .clamp(-1.0, 1.0)
                    .acos()
            }
        };
        if error > AIM_HEADING_TOLERANCE {
            return false;
        }
    }

    // Gunbase pitch gate (Pointer only). pointer.bos's `AimWeapon1`
    // slews `turn gunbase to x-axis (<90>-p) speed <50>`, so wait for
    // that turn. The Bit has a gunbase too, but bit.bos turns it to
    // `(0-p)` instantly (`now`) — the `<90>-p` test could never pass
    // for it and kept every Bit from ever firing.
    if kind == UnitKind::Pointer
        && let Ok(gb) = pieces.gunbase.get(entity)
        && let Ok(animator) = pieces.animator.get(entity)
        && let Some(rot) = animator.rig.piece_rotations.get(gb.0)
    {
        let target_x = std::f32::consts::FRAC_PI_2 - pitch;
        if (rot[0] - target_x).abs() > AIM_PITCH_TOLERANCE {
            return false;
        }
    }

    // Aimer-piece gate (Byte): `wait-for-turn aimer around {y,x}-axis`
    // after `turn aimer to y-axis h` / `x-axis (<-90>-p)`. Computed from
    // the live target rather than `target_rotations`, which
    // `drive_aim_script` may not have refreshed for a new target yet.
    if let Ok(ap) = pieces.aimer.get(entity)
        && let Ok(animator) = pieces.animator.get(entity)
        && let Some(rot) = animator.rig.piece_rotations.get(ap.0)
    {
        let target_x = -std::f32::consts::FRAC_PI_2 - pitch;
        let dy = angle_delta(rot[1], heading);
        let dx = (rot[0] - target_x).abs();
        if dy > AIM_HEADING_TOLERANCE || dx > AIM_PITCH_TOLERANCE {
            return false;
        }
    }
    true
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
            Option<&WeaponBinding>,
            Has<Cloaked>,
            Option<&WormSplash>,
            Option<&crate::interaction::movement::MoveTarget>,
            Option<&AimTarget>,
        ),
        Without<Dying>,
    >,
    cooldowns: Query<&AttackCooldown>,
    mut pieces: PieceLookup,
    time: Res<Time>,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    for (
        entity,
        unit_type,
        gtf,
        order,
        deployable,
        weapon_binding,
        cloaked,
        worm_splash,
        move_target,
        aim,
    ) in &attackers
    {
        // Same deploy / opening gates as `combat_system`. Player-issued
        // attack-ground orders MUST honour them too — otherwise the
        // player can force-fire a Pointer that's still folding open or a
        // byte whose blades haven't fanned yet, bypassing upstream's
        // `AimWeapon1`-returns-0 contract. The gate probes the driver's
        // fold state rather than AimScript.ready: a ground order on
        // empty terrain never produces an AimTarget, so a ready gate
        // would deadlock the shot behind an aim request that can't
        // exist.
        if deployable.is_some_and(|d| d.state != DeployState::Open) {
            continue;
        }
        if pieces
            .animator
            .get(entity)
            .ok()
            .and_then(|a| a.driver.is_open())
            .is_some_and(|open| !open)
        {
            continue;
        }
        let (weapon_id, weapon_def) = match weapon_binding {
            Some(binding) => (Some(binding.0), Some(weapon_registry.by_id(binding.0))),
            None => {
                let name = unit_registry.weapon(unit_type.0);
                let def = weapon_registry.get(name);
                (weapon_registry.intern(name), def)
            }
        };
        let Some(weapon_def) = weapon_def else {
            continue;
        };
        let weapon_id = weapon_id.expect("registered weapon is interned");
        let range = weapon_def.range;
        if range <= 0.0 {
            continue;
        }

        let attacker_pos = gtf.translation();
        let dist = attacker_pos.distance(order.pos);

        if dist > range {
            // Move toward the target, stopping just inside weapon range so
            // the unit doesn't walk through the blast zone of its own AoE.
            // The stop point only drifts as the approach bearing changes,
            // so — like `attack_target_system`'s chase — the goal is
            // re-issued only once it lags by `CHASE_REPATH_DISTANCE`: a
            // `MoveTarget` that changes every tick is a full path search
            // every tick per approaching unit.
            let dir = (order.pos - attacker_pos).normalize_or(Vec3::NEG_Z);
            let stop_at = attacker_pos + dir * (dist - range * 0.85);
            let stale = move_target.is_none_or(|t| t.0.distance(stop_at) > CHASE_REPATH_DISTANCE);
            if stale {
                commands
                    .entity(entity)
                    .insert(crate::interaction::movement::MoveTarget(stop_at));
            }
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

        // Aim at ground target so the barrel sweeps visibly. The order
        // position is fixed, so the stamp is only queued when it is
        // missing or differs (nothing reads `Changed<AimTarget>`).
        let launch = AimLaunch::of(Some(weapon_def));
        if aim.is_none_or(|a| a.pos != order.pos || a.launch != launch) {
            commands.entity(entity).insert(AimTarget {
                pos: order.pos,
                launch,
            });
        }
        // Same surfacing rule as `combat_system`: the aim request above
        // decloaks a Worm; the bite waits for it to be out of cover.
        if cloaked {
            continue;
        }

        // Same alignment gates as `combat_system` — body / gunbase /
        // aimer per piece markers.
        if !aim_gates_pass(
            entity,
            unit_type.0,
            gtf,
            order.pos,
            launch,
            deployable.is_some(),
            &pieces,
        ) {
            continue;
        }

        // Fire.
        let is_traveling = weapon_def.is_traveling();
        commands.entity(entity).insert((
            AttackCooldown {
                remaining: weapon_def.reload_time,
            },
            JustFired,
        ));
        // The salvo's remaining shots go through `BurstFire` exactly like
        // `combat_system`'s — a player-commanded byte fires its authored
        // 4-shot MegaBeam burst, not one shot per reload. Attack-ground
        // has no primary-hit entity; AoE splash at `impact_pos` via
        // `apply_damage` still reaches everything in range.
        open_salvo(
            SalvoShot {
                attacker: entity,
                kind: unit_type.0,
                weapon: weapon_id,
                target: None,
                impact_pos: order.pos,
                is_traveling,
                projectiles: weapon_def.projectiles as u32,
            },
            weapon_def,
            gtf,
            sim_frame(&time),
            &mut pieces,
            &unit_registry,
            &mut pending_attacks,
            &mut damage_queue,
            &mut commands,
        );
        if let Some(splash) = worm_splash {
            queue_wormsplash(&mut damage_queue, entity, splash.0, order.pos, attacker_pos);
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
            Option<&UnitAnimator>,
            Option<&WeaponBinding>,
            Has<crate::interaction::movement::MoveTarget>,
        ),
        Without<Dying>,
    >,
    target_q: Query<&GlobalTransform, Without<Dying>>,
    move_path_q: Query<&crate::interaction::movement::MovePath>,
    mut commands: Commands,
) {
    for (entity, unit_type, stats, gtf, order, deployable, animator, weapon_binding, has_move) in
        &attackers
    {
        // Same deploy / opening gates as `attack_ground_system`.
        if deployable.is_some_and(|d| d.state != DeployState::Open) {
            continue;
        }
        if animator
            .and_then(|a| a.driver.is_open())
            .is_some_and(|open| !open)
        {
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
            let mut ec = commands.entity(entity);
            ec.remove::<AttackTargetOrder>();
            if has_move {
                ec.remove::<crate::interaction::movement::MoveTarget>();
            }
            if move_path_q.contains(entity) {
                ec.remove::<crate::interaction::movement::MovePath>();
            }
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
                // The old path is followed until the new one is ready
                // (Spring's `nextPathId` swap) — no standstill per repath.
                commands
                    .entity(entity)
                    .insert(crate::interaction::movement::MoveTarget(target_pos));
            }
        } else {
            // In range: hold position and fire. Only queue the removals
            // while there is something to remove — a unit holding in
            // range would otherwise push two no-op commands every tick.
            if has_move {
                commands
                    .entity(entity)
                    .remove::<crate::interaction::movement::MoveTarget>();
            }
            if move_path_q.contains(entity) {
                commands
                    .entity(entity)
                    .remove::<crate::interaction::movement::MovePath>();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::components::{Faction, TeamId, UnitStats};
    use crate::units::content::definitions::UnitKind;
    use crate::units::spatial::{SpatialEntry, SpatialIndex};
    use bevy::ecs::system::RunSystemOnce;
    use spring_tdf::{UnitDef, UnitDefs};

    fn stats() -> UnitStats {
        UnitStats {
            radius: 12.0,
            hit_radius: 20.0,
            speed: 90.0,
            acc_rate: 0.03,
            dec_rate: 0.067,
            turn_rate: 3.0,
            can_fly: false,
            no_chase_vtol: true,
        }
    }

    /// Category tables copied from the upstream FBIs: bit.fbi declares
    /// `Category=FAST EDIBLE UNIT NOTFACTORY TARGET` with
    /// `OnlyTargetCategory1=TARGET` / `BadTargetCategory1=FACTORY`;
    /// socket.fbi declares `Category=EDIBLE FACTORY TARGET`.
    fn bit_vs_socket_registry() -> UnitRegistry {
        let mut defs = UnitDefs::default();
        defs.units.insert(
            "bit".into(),
            UnitDef {
                category: "FAST EDIBLE UNIT NOTFACTORY TARGET".into(),
                only_target_category1: "TARGET".into(),
                bad_target_category1: "FACTORY".into(),
                weapon1: "Line".into(),
                ..UnitDef::default()
            },
        );
        defs.units.insert(
            "socket".into(),
            UnitDef {
                category: "EDIBLE FACTORY TARGET".into(),
                ..UnitDef::default()
            },
        );
        UnitRegistry::for_test(defs)
    }

    fn line_weapon_registry() -> WeaponRegistry {
        let mut weapons = WeaponRegistry::default();
        weapons.insert_for_test(
            "Line",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 80.0,
                    ..Default::default()
                },
                range: 256.0,
                reload_time: 0.5,
                ..Default::default()
            },
        );
        weapons
    }

    /// Upstream `BadTargetCategory1=FACTORY` (bit.fbi): a Bit must not
    /// auto-acquire a Socket standing in weapon range — auto-targeting
    /// ignores buildings until the player issues an explicit attack.
    /// A Bit-vs-Bit scan under identical conditions still acquires.
    #[test]
    fn bit_ignores_factories_but_scans_bits() {
        // --- Socket enemy in range: must NOT be acquired. ---
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>()
            .init_resource::<SpatialIndex>();
        app.insert_resource(bit_vs_socket_registry());
        let weapons = line_weapon_registry();
        let line = weapons.intern("Line").unwrap();
        app.insert_resource(weapons);

        let bit = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Bit),
                stats(),
                Faction::System,
                TeamId(0),
                GlobalTransform::from_xyz(0.0, 0.0, 0.0),
                WeaponBinding(line),
            ))
            .id();
        let socket = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<SpatialIndex>()
            .insert_for_test(SpatialEntry {
                entity: socket,
                pos: Vec3::new(100.0, 0.0, 0.0),
                team: 1,
                kind: UnitKind::Socket,
                hp_positive: true,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
            });

        app.world_mut().run_system_once(combat_system).unwrap();

        assert!(app.world().get::<TargetCache>(bit).is_none());
        assert!(app.world().get::<AimTarget>(bit).is_none());
        assert!(app.world().get::<AttackCooldown>(bit).is_none());
        assert!(app.world().resource::<DamageQueue>().is_empty());

        // --- Same setup, but the enemy is another Bit: acquired. ---
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>()
            .init_resource::<SpatialIndex>();
        app.insert_resource(bit_vs_socket_registry());
        let weapons = line_weapon_registry();
        let line = weapons.intern("Line").unwrap();
        app.insert_resource(weapons);

        let bit = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Bit),
                stats(),
                Faction::System,
                TeamId(0),
                GlobalTransform::from_xyz(0.0, 0.0, 0.0),
                WeaponBinding(line),
            ))
            .id();
        let enemy_bit = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<SpatialIndex>()
            .insert_for_test(SpatialEntry {
                entity: enemy_bit,
                pos: Vec3::new(100.0, 0.0, 0.0),
                team: 1,
                kind: UnitKind::Bit,
                hp_positive: true,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
            });

        app.world_mut().run_system_once(combat_system).unwrap();

        let cache = app.world().get::<TargetCache>(bit).unwrap();
        assert_eq!(cache.target, enemy_bit);
        // `Line` is a traveling LaserCannon: the shot goes out as an
        // AttackEvent with a deferred `DelayedHit`, not straight into
        // the damage queue.
        assert_eq!(app.world().resource::<PendingAttacks>().events.len(), 1);
        assert!(
            app.world().resource::<PendingAttacks>().events[0]
                .delayed_hit
                .is_some()
        );
    }
    /// Worm vs Bit registry (no category restrictions — the upstream
    /// worm.fbi `OnlyTargetCategory1=EDIBLE` would need Bit's category
    /// too; the cloak rules under test don't depend on it).
    fn worm_weapons() -> (WeaponRegistry, WeaponId, WeaponId) {
        let mut weapons = WeaponRegistry::default();
        let bite = weapons.insert_for_test(
            "Wormbite",
            spring_tdf::WeaponDef {
                weapon_type: "Melee".into(),
                damage: spring_tdf::DamageMap {
                    default: 3200.0,
                    ..Default::default()
                },
                range: 200.0,
                reload_time: 6.0,
                area_of_effect: 140.0,
                ..Default::default()
            },
        );
        let splash = weapons.insert_for_test(
            "Wormsplash",
            spring_tdf::WeaponDef {
                weapon_type: "Melee".into(),
                damage: spring_tdf::DamageMap {
                    default: 800.0,
                    ..Default::default()
                },
                range: 200.0,
                area_of_effect: 210.0,
                edge_effectiveness: 1.0,
                avoid_friendly: true,
                ..Default::default()
            },
        );
        (weapons, bite, splash)
    }

    fn combat_app(weapons: WeaponRegistry) -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>()
            .init_resource::<SpatialIndex>()
            .insert_resource(UnitRegistry::empty())
            .insert_resource(weapons);
        app
    }

    fn enemy_entry(app: &mut App, pos: Vec3, cloaked: bool, detected_by: u64) -> Entity {
        let enemy = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<SpatialIndex>()
            .insert_for_test(SpatialEntry {
                entity: enemy,
                pos,
                team: 1,
                kind: UnitKind::Worm,
                hp_positive: true,
                is_flying: false,
                cloaked,
                detected_by,
            });
        enemy
    }

    fn spawn_bit_shooter(app: &mut App, line: WeaponId) -> Entity {
        app.world_mut()
            .spawn((
                UnitType(UnitKind::Bit),
                stats(),
                Faction::System,
                TeamId(0),
                GlobalTransform::from_xyz(0.0, 0.0, 0.0),
                WeaponBinding(line),
            ))
            .id()
    }

    /// A cloaked enemy is invisible to auto-targeting until the
    /// shooter's team has a detector on it (`DetectedBy` bit set).
    #[test]
    fn undetected_cloaked_enemy_is_not_auto_targeted() {
        // --- Detected only by team 1 (its own side): ignored. ---
        let mut app = combat_app(line_weapon_registry());
        let line = app
            .world()
            .resource::<WeaponRegistry>()
            .intern("Line")
            .unwrap();
        let bit = spawn_bit_shooter(&mut app, line);
        enemy_entry(&mut app, Vec3::new(100.0, 0.0, 0.0), true, 0b10);
        app.world_mut().run_system_once(combat_system).unwrap();
        assert!(app.world().get::<TargetCache>(bit).is_none());
        assert!(app.world().get::<AimTarget>(bit).is_none());

        // --- Detected by team 0: acquired. ---
        let mut app = combat_app(line_weapon_registry());
        let bit = spawn_bit_shooter(&mut app, line);
        let worm = enemy_entry(&mut app, Vec3::new(100.0, 0.0, 0.0), true, 0b01);
        app.world_mut().run_system_once(combat_system).unwrap();
        assert_eq!(app.world().get::<TargetCache>(bit).unwrap().target, worm);
    }

    /// AutoHold OFF (AI worms): a cloaked worm acquires on its own and
    /// stamps the aim request that surfaces it — but never bites from
    /// under cover. AutoHold ON (human worms): holds fire entirely.
    #[test]
    fn cloaked_worm_autohold_gates_auto_targeting() {
        for hold in [false, true] {
            let (weapons, bite, splash) = worm_weapons();
            let mut app = combat_app(weapons);
            let worm = app
                .world_mut()
                .spawn((
                    UnitType(UnitKind::Worm),
                    stats(),
                    Faction::Hacker,
                    TeamId(0),
                    GlobalTransform::from_xyz(0.0, 0.0, 0.0),
                    WeaponBinding(bite),
                    WormSplash(splash),
                    AutoHold(hold),
                    Cloaked,
                ))
                .id();
            enemy_entry(&mut app, Vec3::new(100.0, 0.0, 0.0), false, 0);
            app.world_mut().run_system_once(combat_system).unwrap();

            assert_eq!(
                app.world().get::<AimTarget>(worm).is_some(),
                !hold,
                "hold={hold}"
            );
            assert_eq!(
                app.world().get::<TargetCache>(worm).is_some(),
                !hold,
                "hold={hold}"
            );
            assert!(
                app.world().resource::<DamageQueue>().is_empty(),
                "hold={hold}"
            );
            assert!(
                app.world().get::<AttackCooldown>(worm).is_none(),
                "hold={hold}"
            );
        }
    }

    /// An explicit attack order still engages under AutoHold: the
    /// ordered target in range gets the aim request (which surfaces
    /// the worm) even though auto-acquisition is off.
    #[test]
    fn autohold_worm_engages_ordered_target() {
        let (weapons, bite, splash) = worm_weapons();
        let mut app = combat_app(weapons);
        let enemy = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Bit),
                GlobalTransform::from_xyz(100.0, 0.0, 0.0),
            ))
            .id();
        let worm = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Worm),
                stats(),
                Faction::Hacker,
                TeamId(0),
                GlobalTransform::from_xyz(0.0, 0.0, 0.0),
                WeaponBinding(bite),
                WormSplash(splash),
                AutoHold(true),
                Cloaked,
                AttackTargetOrder { target: enemy },
            ))
            .id();
        app.world_mut().run_system_once(combat_system).unwrap();
        assert_eq!(app.world().get::<TargetCache>(worm).unwrap().target, enemy);
        assert!(app.world().get::<AimTarget>(worm).is_some());
    }

    /// A surfaced worm's bite queues the Wormbite hit plus two target-
    /// less Wormsplash detonations (worm.bos `emit-sfx 4097 from head`
    /// and `from end`) so `apply_damage` runs AoE + infection for them.
    #[test]
    fn worm_bite_queues_wormsplash_aoe() {
        let (weapons, bite, splash) = worm_weapons();
        let mut app = combat_app(weapons);
        let worm = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Worm),
                stats(),
                Faction::Hacker,
                TeamId(0),
                GlobalTransform::from_xyz(0.0, 0.0, 0.0),
                WeaponBinding(bite),
                WormSplash(splash),
                AutoHold(true),
            ))
            .id();
        let enemy = enemy_entry(&mut app, Vec3::new(100.0, 0.0, 0.0), false, 0);
        app.world_mut().run_system_once(combat_system).unwrap();

        let queue = app.world().resource::<DamageQueue>();
        let hits: Vec<_> = queue.iter_snapshot_for_test().collect();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].weapon, bite);
        assert_eq!(hits[0].target, Some(enemy));
        let splashes: Vec<_> = hits.iter().filter(|h| h.weapon == splash).collect();
        assert_eq!(splashes.len(), 2);
        assert!(
            splashes
                .iter()
                .all(|h| h.target.is_none() && h.attacker == worm)
        );
        assert!(
            splashes
                .iter()
                .any(|h| h.impact_pos == Vec3::new(100.0, 0.0, 0.0))
        );
    }

    /// A Flow banked in flight and aimed by its `base` piece: every
    /// projectile of a salvo shot runs `Shot1` → `QueryWeapon1` and
    /// leaves from the *next* gunpoint, at that piece's world position
    /// through the unit's full attitude (bank included) and the spinning
    /// wing — and, `base` having been turned by the unit-relative
    /// `AimWeapon1(h, p)`, the fixed-launcher emit direction (the
    /// gunpoints' +Z) points straight at the target.
    #[test]
    fn flow_salvo_shot_uses_each_gunpoint_through_the_flyer_attitude() {
        use crate::units::assets::animation::{
            AnimRig, MuzzlePiece, PieceEmit, UnitAnimator, driver_for, piece_names,
        };

        let mut app = App::new();
        app.init_resource::<PendingAttacks>()
            .init_resource::<DamageQueue>()
            .insert_resource(UnitRegistry::empty());

        let unit_tf = Transform::from_xyz(300.0, 140.0, 200.0)
            .with_rotation(crate::interaction::air_movement::attitude(0.7, 0.25));
        let target = Vec3::new(420.0, 20.0, 150.0);
        let (h, p) = aim::local_aim_angles(
            unit_tf.rotation,
            target - unit_tf.translation,
            AimLaunch::Direct,
        );
        // Rig → Bevy mapping (`apply_and_drain`): euler YXZ (y, x, −z).
        let base_tf = Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, h, -p, 0.0));
        let wing_tf = Transform::from_rotation(Quat::from_euler(EulerRot::YXZ, 0.0, 0.0, -0.8));
        let root_tf = Transform::from_rotation(Quat::from_rotation_y(std::f32::consts::PI));
        let gp0_tf = Transform::from_xyz(-9.5, 0.0, 11.5);
        let gp1_tf = Transform::from_xyz(9.5, 0.0, 11.5);

        let world = app.world_mut();
        let unit = world
            .spawn((
                unit_tf,
                GlobalTransform::from(unit_tf),
                UnitType(UnitKind::Flow),
            ))
            .id();
        let root = world.spawn((root_tf, ChildOf(unit))).id();
        let base = world
            .spawn((base_tf, ChildOf(root), PieceEmit::default()))
            .id();
        let wing1 = world
            .spawn((wing_tf, ChildOf(base), PieceEmit::default()))
            .id();
        let gp0 = world
            .spawn((gp0_tf, ChildOf(wing1), PieceEmit::default()))
            .id();
        let gp1 = world
            .spawn((gp1_tf, ChildOf(wing1), PieceEmit::default()))
            .id();
        let stub = || (Transform::default(), ChildOf(unit));
        let monolith = world.spawn(stub()).id();
        let wing2 = world.spawn(stub()).id();
        let gp2 = world.spawn(stub()).id();
        let gp3 = world.spawn(stub()).id();
        let mut rig = AnimRig::for_test(piece_names(UnitKind::Flow));
        rig.piece_entities = vec![base, monolith, wing1, wing2, gp0, gp1, gp2, gp3];
        rig.muzzle = 4;
        let mut driver = driver_for(UnitKind::Flow);
        driver.bind(&rig);
        world.entity_mut(unit).insert((
            UnitAnimator {
                rig,
                created: true,
                driver,
            },
            MuzzlePiece(4),
        ));

        let shot = SalvoShot {
            attacker: unit,
            kind: UnitKind::Flow,
            weapon: WeaponId::BUILD_LASER,
            target: None,
            impact_pos: target,
            is_traveling: true,
            projectiles: 2,
        };
        app.world_mut()
            .run_system_once(
                move |mut pieces: PieceLookup,
                      registry: Res<UnitRegistry>,
                      mut attacks: ResMut<PendingAttacks>,
                      mut damage: ResMut<DamageQueue>| {
                    fire_salvo_shot(
                        &shot,
                        &GlobalTransform::from(unit_tf),
                        false,
                        &mut pieces,
                        &registry,
                        &mut attacks,
                        &mut damage,
                    );
                },
            )
            .unwrap();

        let chain = |leaf: Transform| {
            GlobalTransform::from(unit_tf).affine()
                * root_tf.compute_affine()
                * base_tf.compute_affine()
                * wing_tf.compute_affine()
                * leaf.compute_affine()
        };
        let events = &app.world().resource::<PendingAttacks>().events;
        assert_eq!(
            events.len(),
            2,
            "projectiles=2 → two projectiles per salvo shot"
        );
        for (event, leaf) in events.iter().zip([gp0_tf, gp1_tf]) {
            let expected = chain(leaf).transform_point3(Vec3::ZERO);
            assert!(
                event.attacker_pos.distance(expected) < 1e-3,
                "muzzle {} != gunpoint {}",
                event.attacker_pos,
                expected
            );
            let dir = event.delayed_hit.as_ref().unwrap().muzzle_dir;
            let aim = (target - unit_tf.translation).normalize();
            assert!(dir.dot(aim) > 0.9999, "launch dir {dir} vs aim {aim}");
        }
        assert!(events[0].attacker_pos.distance(events[1].attacker_pos) > 15.0);
        // No muzzle flash: flow.bos emits nothing from its gunpoints.
        assert!(events[0].muzzle_ceg.is_none());
        let animator = app.world().get::<UnitAnimator>(unit).unwrap();
        assert_eq!(animator.rig.muzzle, 5, "two Shot1s: gp0 then gp1");
    }
}

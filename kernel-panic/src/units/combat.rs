use bevy::prelude::*;

use super::animation::CobAnimator;
use super::components::{Faction, Health, TeamId, UnitType};
use super::definitions::UnitKind;
use super::script_triggers::JustFired;
use super::unit_registry::UnitRegistry;
use super::weapon_fx::{AttackEvent, PendingAttacks};
use super::weapons::WeaponRegistry;
use crate::interaction::movement::{MovePath, MoveTarget};

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

/// In-progress burst fire. Weapons with `burst > 1` fire the first shot
/// through the normal combat path and attach this component for the
/// remaining shots, which are released at `interval` spacing by
/// [`tick_burst_fire`]. The aim point is frozen at trigger time so the
/// whole burst lands on the same spot regardless of target motion.
#[derive(Component)]
pub struct BurstFire {
    pub shots_remaining: u32,
    pub interval: f32,
    pub timer: f32,
    pub target: Entity,
    pub target_pos: Vec3,
    pub weapon: String,
    pub arc_height: f32,
}

/// Marks a unit that has reached 0 HP and is playing its death animation.
/// The entity will be despawned once the animation finishes or the timer expires.
#[derive(Component)]
pub struct Dying {
    pub timer: f32,
}

/// Maximum time to wait for a death animation before force-despawning (seconds).
const DEATH_ANIM_TIMEOUT: f32 = 2.0;

/// Marks a unit as infected by a Worm or Virus attack. If the unit dies
/// while this component is present, a Virus spawns at the death location
/// for the attacker's team.
#[derive(Component)]
pub struct Infected {
    /// Remaining seconds before the infection expires.
    pub timer: f32,
    /// The faction that will own the spawned Virus.
    pub attacker_faction: Faction,
    /// The team ID that will own the spawned Virus.
    pub attacker_team: u8,
}

/// How long (seconds) a Worm/Virus infection lasts before expiring.
const INFECTION_DURATION: f32 = 6.0;

/// Queued virus spawns from infected unit deaths (position, faction, team).
#[derive(Resource, Default)]
pub struct VirusSpawnQueue(pub Vec<(Vec3, Faction, u8)>);

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
}

/// Pending damage to apply after combat resolution.
#[derive(Resource, Default)]
pub struct DamageQueue(Vec<PendingDamage>);

/// Deploy cycle for units that must unfold before firing (e.g. Pointer).
/// The COB script animates the legs/gun; this component gates combat so
/// the unit can only fire while `Open`, matching upstream Kernel Panic.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeployState {
    Closed,
    Opening,
    Open,
    Closing,
}

/// Attached to units with a deploy cycle. `timer` counts down through
/// transition states; the duration is the animation length in seconds.
#[derive(Component)]
pub struct Deployable {
    pub state: DeployState,
    pub timer: f32,
}

/// Stamped by `combat_system` each frame an armed unit has picked a target
/// it wants to fire at. Read by `aim_weapons_system` to rotate the body /
/// tilt the gun before combat actually commits the shot. Removed in
/// frames where the unit has no viable target so aim systems don't keep
/// steering toward a stale position.
#[derive(Component, Clone, Copy, Debug)]
pub struct AimTarget {
    pub pos: Vec3,
    /// Arc height for ballistic weapons (passed through from the
    /// WeaponDef so the gun elevates for the lob, not the direct line).
    pub arc_height: f32,
}

/// Max heading error (radians) at which a Deployable is allowed to fire.
/// ~5° — tight enough that the gun is visibly pointed at the target, loose
/// enough that the Pointer doesn't get stuck oscillating.
pub const AIM_HEADING_TOLERANCE: f32 = 0.09;

/// Open/Close animation length in seconds, matching the upstream COB
/// script timings (legs move over 0.5s, gun extends over another 1.0s).
pub const DEPLOY_DURATION: f32 = 1.5;

impl Deployable {
    /// Freshly-spawned deployable units start stowed (`Closed`). The
    /// `tick_deploy_state` system promotes them to `Opening` as soon as
    /// they're idle (i.e. have no move order), which triggers the COB
    /// `Open()` animation.
    pub fn initial() -> Self {
        Self {
            state: DeployState::Closed,
            timer: 0.0,
        }
    }
}

/// System: drive the deploy state machine from movement state, firing
/// the unit's `Open()` / `Close()` COB scripts so the visible model
/// matches the logical deploy state. Stopping schedules `Open`; starting
/// to move schedules `Close`.
#[allow(clippy::type_complexity)]
pub fn tick_deploy_state(
    time: Res<Time>,
    mut query: Query<
        (
            &mut Deployable,
            &mut CobAnimator,
            Option<&MoveTarget>,
            Option<&MovePath>,
        ),
        Without<Dying>,
    >,
) {
    let dt = time.delta_secs();
    for (mut deployable, mut animator, move_target, move_path) in &mut query {
        let is_moving = move_target.is_some() || move_path.is_some();

        if deployable.timer > 0.0 {
            deployable.timer = (deployable.timer - dt).max(0.0);
            if deployable.timer == 0.0 {
                deployable.state = match deployable.state {
                    DeployState::Opening => DeployState::Open,
                    DeployState::Closing => DeployState::Closed,
                    other => other,
                };
            }
        }

        match (deployable.state, is_moving) {
            (DeployState::Open, true) | (DeployState::Opening, true) => {
                deployable.state = DeployState::Closing;
                deployable.timer = DEPLOY_DURATION;
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Close", &[]);
            }
            (DeployState::Closed, false) | (DeployState::Closing, false) => {
                deployable.state = DeployState::Opening;
                deployable.timer = DEPLOY_DURATION;
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Open", &[]);
            }
            _ => {}
        }
    }
}

/// System: armed units auto-attack the nearest enemy in range.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn combat_system(
    time: Res<Time>,
    mut cooldowns: Query<&mut AttackCooldown>,
    attackers: Query<
        (
            Entity,
            &UnitType,
            &Faction,
            &TeamId,
            &GlobalTransform,
            Option<&Deployable>,
        ),
        (Without<Dying>, Without<super::spawning::Emerging>),
    >,
    potential_targets: Query<
        (Entity, &Faction, &TeamId, &GlobalTransform, &Health),
        (With<UnitType>, Without<super::spawning::Emerging>),
    >,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    weapon_registry: Res<WeaponRegistry>,
    mut pending_attacks: ResMut<PendingAttacks>,
    unit_registry: Res<UnitRegistry>,
) {
    let dt = time.delta_secs();

    // Tick cooldowns.
    for mut cd in &mut cooldowns {
        cd.remaining = (cd.remaining - dt).max(0.0);
    }

    damage_queue.0.clear();

    for (entity, unit_type, attacker_faction, attacker_team, attacker_gtf, deployable) in &attackers
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

        // Find nearest living enemy in range. Shared team = ally regardless
        // of faction (showcase spawns mixed-faction units on team 0 and
        // expects them to ignore each other).
        let mut best: Option<(Entity, Vec3, f32)> = None;
        for (target_entity, target_faction, target_team, target_gtf, target_health) in
            &potential_targets
        {
            if target_team == attacker_team
                || target_faction == attacker_faction
                || target_health.current <= 0.0
            {
                continue;
            }
            let target_pos = target_gtf.translation();
            let dist_sq = attacker_pos.distance_squared(target_pos);
            if dist_sq <= range_sq && best.is_none_or(|(_, _, d)| dist_sq < d) {
                best = Some((target_entity, target_pos, dist_sq));
            }
        }

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

        damage_queue.0.push(PendingDamage {
            target: target_entity,
            attacker: entity,
            weapon: weapon_name.to_string(),
            impact_pos: target_pos,
        });
        commands.entity(entity).insert((
            AttackCooldown {
                remaining: cooldown,
            },
            JustFired {
                target_pos,
                arc_height,
            },
        ));
        if !weapon_name.is_empty() {
            pending_attacks.events.push(AttackEvent {
                attacker_pos,
                target_pos,
                weapon_name: weapon_name.to_string(),
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

/// System: steer Deployable units to face their current `AimTarget` at
/// the unit's FBI TurnRate, and tilt the `gunbase` piece by the pitch
/// required to sight the target (accounting for ballistic arc height).
/// The rotation is written directly into the CobAnimator's `piece_rotations`
/// for gunbase, bypassing the COB AimWeapon1 script — our VM doesn't
/// currently route HEADING reads/writes back to the unit transform, so
/// the upstream .bos aim loop is inert. Doing this host-side keeps the
/// animated gun lined up with whatever the unit is actually shooting at.
pub fn aim_weapons_system(
    time: Res<Time>,
    mut query: Query<(
        &mut Transform,
        &GlobalTransform,
        &UnitType,
        &AimTarget,
        &mut CobAnimator,
        &Deployable,
    )>,
    unit_registry: Res<UnitRegistry>,
) {
    let dt = time.delta_secs();
    for (mut transform, gtf, unit_type, aim, mut animator, _deploy) in &mut query {
        let attacker_pos = gtf.translation();
        let to_target = Vec3::new(aim.pos.x - attacker_pos.x, 0.0, aim.pos.z - attacker_pos.z);
        let horizontal_dist = to_target.length();
        if horizontal_dist < 1e-4 {
            continue;
        }

        // Body heading: rotate toward the target at the unit's TurnRate.
        let desired_forward = to_target / horizontal_dist;
        let forward_vec = transform.forward().as_vec3();
        let current_xz = {
            let f = Vec3::new(forward_vec.x, 0.0, forward_vec.z);
            if f.length_squared() < 1e-6 {
                Vec3::Z
            } else {
                f.normalize()
            }
        };
        let turn_rate = unit_registry.turn_rate(unit_type.0);
        let max_turn = if turn_rate > 0.0 {
            turn_rate * dt
        } else {
            std::f32::consts::TAU
        };
        let new_forward =
            crate::interaction::movement::rotate_toward_xz(current_xz, desired_forward, max_turn);
        if new_forward.length_squared() > 1e-6 {
            transform.look_to(new_forward, Vec3::Y);
        }

        // Gunbase pitch: elevate the barrel. For a ballistic lob of peak
        // height h over distance d, the launch angle above horizontal is
        // roughly atan(4h/d); add that to the direct line-of-sight pitch
        // so mortar-type shots arc onto the target.
        let dy = aim.pos.y - attacker_pos.y;
        let direct_pitch = (dy).atan2(horizontal_dist);
        let arc_pitch = if aim.arc_height > 0.0 && horizontal_dist > 1.0 {
            (4.0 * aim.arc_height / horizontal_dist).atan()
        } else {
            0.0
        };
        let pitch = direct_pitch + arc_pitch;

        // pointer.bos sets gunbase's rest rotation to x-axis π/2 in Create
        // (so the barrel folds flat). AimWeapon1 rewrites it to (π/2 − p),
        // which is the same convention: higher pitch = smaller X rotation.
        // Since our VM doesn't actually run the aim loop, mirror it here.
        let gunbase_idx = animator
            .cob
            .piece_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("gunbase"));
        if let Some(idx) = gunbase_idx
            && idx < animator.piece_rotations.len()
        {
            let target_x = std::f32::consts::FRAC_PI_2 - pitch;
            animator.target_rotations[idx][0] = target_x;
            // Reasonable pitch rate (~90°/sec) so the barrel visibly
            // swings instead of snapping. The COB script uses speed <50>
            // (50 ang-units/frame ≈ 8.2°/sec) which feels too sluggish
            // for a responsive host-driven aim; we split the difference.
            animator.turn_speeds[idx][0] = std::f32::consts::PI * 0.5;
        }
    }
}

/// System: release follow-up shots for units in the middle of a burst.
/// The initial shot fires through the regular combat path; each follow-up
/// queues another damage event and weapon-FX event at `burst_rate` spacing
/// until `shots_remaining` hits zero, then removes the component.
pub fn tick_burst_fire(
    time: Res<Time>,
    mut query: Query<(Entity, &mut BurstFire, &GlobalTransform), Without<Dying>>,
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

        damage_queue.0.push(PendingDamage {
            target: burst.target,
            attacker: entity,
            weapon: burst.weapon.clone(),
            impact_pos: burst.target_pos,
        });
        pending_attacks.events.push(AttackEvent {
            attacker_pos: gtf.translation(),
            target_pos: burst.target_pos,
            weapon_name: burst.weapon.clone(),
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
fn splash_falloff(dist: f32, radius: f32, edge_mult: f32) -> f32 {
    let t = (dist / radius).clamp(0.0, 1.0);
    1.0 - t * (1.0 - edge_mult)
}

/// System: apply queued damage and mark targets as infected when hit by
/// Worm or Virus weapons. Weapons with `area_of_effect > AOE_SPLASH_THRESHOLD`
/// also damage other units in radius, with linear falloff from the
/// weapon's `edge_effectiveness`. `avoidfriendly=1` and `noselfdamage=1`
/// filter the splash set so allies / the attacker don't eat stray AoE.
#[allow(clippy::too_many_arguments)]
pub fn apply_damage(
    mut damage_queue: ResMut<DamageQueue>,
    mut health_q: Query<&mut Health>,
    attacker_q: Query<(&UnitType, &Faction, &TeamId)>,
    target_unit_q: Query<&UnitType>,
    splash_q: Query<(Entity, &UnitType, &Faction, &TeamId, &GlobalTransform), With<Health>>,
    weapon_registry: Res<WeaponRegistry>,
    unit_registry: Res<UnitRegistry>,
    mut commands: Commands,
) {
    for pending in damage_queue.0.drain(..) {
        let Some(weapon_def) = weapon_registry.get(&pending.weapon) else {
            warn!("apply_damage: weapon {:?} not in registry", pending.weapon);
            continue;
        };

        let base = |kind: UnitKind| {
            weapon_def.damage.for_type(kind.armor_class().key())
                * unit_registry.damage_modifier(kind)
        };
        let attacker_info = attacker_q.get(pending.attacker).ok();

        let primary_damage = match target_unit_q.get(pending.target) {
            Ok(unit) => base(unit.0),
            Err(_) => weapon_def.damage.default,
        };
        if let Ok(mut health) = health_q.get_mut(pending.target) {
            health.current -= primary_damage;
        }
        commands.entity(pending.target).insert(IdleTimer(0.0));

        let aoe = weapon_def.area_of_effect;
        if aoe > AOE_SPLASH_THRESHOLD {
            let aoe_sq = aoe * aoe;
            let edge_mult = weapon_def.edge_effectiveness;
            let avoid_friendly = weapon_def.avoid_friendly;
            let no_self_damage = weapon_def.no_self_damage;
            for (entity, unit, faction, team, gtf) in &splash_q {
                if entity == pending.target {
                    continue;
                }
                if no_self_damage && entity == pending.attacker {
                    continue;
                }
                if avoid_friendly
                    && let Some((_, a_faction, a_team)) = attacker_info
                    && (team == a_team || faction == a_faction)
                {
                    continue;
                }
                let d_sq = gtf.translation().distance_squared(pending.impact_pos);
                if d_sq >= aoe_sq {
                    continue;
                }
                let splash = base(unit.0) * splash_falloff(d_sq.sqrt(), aoe, edge_mult);
                if let Ok(mut health) = health_q.get_mut(entity) {
                    health.current -= splash;
                }
                commands.entity(entity).insert(IdleTimer(0.0));
            }
        }

        // Apply infection: Worm and Virus attacks infect non-Virus targets.
        if let Some((attacker_type, attacker_faction, attacker_team)) = attacker_info {
            let is_infecting =
                attacker_type.0 == UnitKind::Worm || attacker_type.0 == UnitKind::Virus;
            let target_is_virus = target_unit_q
                .get(pending.target)
                .is_ok_and(|ut| ut.0 == UnitKind::Virus);

            if is_infecting && !target_is_virus {
                commands.entity(pending.target).insert(Infected {
                    timer: INFECTION_DURATION,
                    attacker_faction: *attacker_faction,
                    attacker_team: attacker_team.0,
                });
            }
        }
    }
}

/// Spring encodes FBI `IdleTime` in sim frames at 30 fps; convert to
/// seconds so we can compare against a `Time`-driven timer.
const IDLE_FRAMES_PER_SECOND: f32 = 30.0;

/// System: regenerate HP on units that have been idle long enough.
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

/// System: tick infection timers and remove expired infections.
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

/// System: when a unit reaches 0 HP, start the Killed() COB script and mark it
/// as `Dying`. If the unit was infected, queue a Virus spawn.
#[allow(clippy::type_complexity)]
pub fn death_system(
    query: Query<
        (Entity, &Health, &GlobalTransform, Option<&Infected>),
        (With<UnitType>, Without<Dying>),
    >,
    mut animators: Query<&mut CobAnimator>,
    mut virus_spawns: ResMut<VirusSpawnQueue>,
    mut commands: Commands,
) {
    for (entity, health, gtf, infected) in &query {
        if health.current <= 0.0 {
            // Start the COB Killed() callback if the unit has an animator.
            if let Ok(mut animator) = animators.get_mut(entity) {
                let cob = animator.cob.clone();
                animator.vm.start_script(&cob, "Killed", &[0, 0]);
            }

            // If the dying unit was infected, queue a Virus spawn for the
            // attacker's team at the death location.
            if let Some(infected) = infected {
                virus_spawns.0.push((
                    gtf.translation(),
                    infected.attacker_faction,
                    infected.attacker_team,
                ));
            }

            commands.entity(entity).remove::<Infected>().insert(Dying {
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

        let anim_done = animator.is_none_or(|a| !a.vm.has_active_threads());
        if anim_done || dying.timer <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(app.world().resource::<DamageQueue>().0.len(), 1);
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
        assert_eq!(app.world().resource::<DamageQueue>().0.len(), 1);

        // Two more intervals: remaining shots fire, component is gone.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(250));
        app.world_mut().run_system_once(tick_burst_fire).unwrap();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_millis(250));
        app.world_mut().run_system_once(tick_burst_fire).unwrap();
        assert_eq!(app.world().resource::<DamageQueue>().0.len(), 3);
        assert!(app.world().get::<BurstFire>(attacker).is_none());
    }
}

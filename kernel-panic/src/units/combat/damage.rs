//! Damage application, splash falloff, burst follow-ups, and infection.
//!
//! Combat system queues hits into [`DamageQueue`]; [`apply_damage`] drains
//! the queue, resolves primary-hit gating against each target's volumetric
//! hit radius, fans out AoE via [`splash_falloff`], and tags infected
//! targets with [`Infected`]. Burst-fire follow-ups live in
//! [`tick_burst_fire`] which pushes additional [`PendingDamage`] at
//! `burst_rate` intervals after the initial shot.

use bevy::prelude::*;

use super::{ByteOpen, Dying, IdleTimer, StunCharge, Stunned};
use crate::units::components::{Faction, Health, TeamId, UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::WeaponId;
use crate::units::content::weapons::WeaponRegistry;
use crate::units::lifecycle::script_triggers::JustFired;
use crate::units::spatial::SpatialIndex;
use crate::units::weapon_fx::PendingAttacks;

/// A pending damage event. Damage is resolved at apply-time so the
/// target's armor class can pick the right entry from the weapon's
/// `[DAMAGE]` table. When `target` is `Some`, that entity always takes
/// full primary damage. When `None` (attack-ground, area blasts with no
/// specific primary hit) only AoE splash fires. If the weapon has
/// `area_of_effect > 0`, other units within that radius of `impact_pos`
/// also take damage with linear falloff from the weapon's
/// `edge_effectiveness`.
#[derive(Debug, Clone)]
pub struct PendingDamage {
    /// Primary target. `None` for ground-targeted or pure-AoE hits.
    pub target: Option<Entity>,
    pub attacker: Entity,
    /// Interned weapon id (see [`WeaponRegistry`]) — pushed by systems
    /// that already resolved the weapon through the registry, so the
    /// apply path never string-hashes. Display names come back out via
    /// [`WeaponRegistry::name`].
    pub weapon: WeaponId,
    pub impact_pos: Vec3,
    /// Distance from attacker to primary target at the moment the hit
    /// was queued. Used by dynamic-damage weapons (BugCannon) to scale
    /// the primary hit; zero is fine for single-range weapons.
    pub attacker_distance: f32,
}

/// Pending damage to apply after combat resolution.
///
/// Lifecycle: producers `push` (combat shots, kamikaze triggers,
/// death-system `ExplodeAs` self-hits), and `apply_damage` is the sole
/// consumer, draining the queue empty each time it runs. There is
/// deliberately **no `clear()`**: producers fire on both sides of the
/// drain within a frame, so any eager clear would silently drop damage
/// (that was a real bug — kamikaze splash and every death-AoE were
/// wiped before delivery).
#[derive(Resource, Default)]
pub struct DamageQueue(Vec<PendingDamage>);

impl DamageQueue {
    pub fn push(&mut self, damage: PendingDamage) {
        self.0.push(damage);
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

    /// Borrow the pending damage slice for assertions. Test-only so the
    /// production code can't accidentally peek at queued items out of
    /// order.
    #[cfg(test)]
    pub fn iter_snapshot_for_test(&self) -> impl Iterator<Item = &PendingDamage> {
        self.0.iter()
    }
}

/// In-progress salvo (upstream `salvoLeft` / `nextSalvo`). Weapons with
/// `burst > 1` fire the first shot where the salvo opens; this component
/// releases the rest `salvo_delay` whole sim frames apart
/// (`int(burstRate · 30)`, `WeaponLoader.cpp:171` — FlowMissile's 0.3 s
/// is 9 frames, MegaBeam's 0.25 s is 7), with the aim point frozen so the
/// whole burst lands on `shot.impact_pos`. `shot.target == None` means an
/// [`AttackGroundOrder`] burst — apply_damage falls through to AoE splash.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct BurstFire {
    pub shot: super::SalvoShot,
    pub shots_remaining: u32,
    /// Sim frame of the last shot — never two salvo shots in one frame
    /// (`UpdateSalvo` runs once per frame even with `salvoDelay == 0`).
    pub last_frame: u64,
    /// Sim frame the next shot is due (`nextSalvo`).
    pub next_frame: u64,
    pub salvo_delay: u64,
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

pub use crate::units::content::weapons::weapon_infection_duration;

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

/// Pure form of the Byte armor rule, decoupled from Bevy queries so
/// it can be unit-tested without a live world. See
/// [`byte_closed_damage_multiplier`] for the Query wrapper.
///
/// `is_sigterm`: upstream SIGTERM bomb weaponID is `168` in
/// `retroweapons.tdf` — byte.bos's `HitByWeaponId` checks that literal
/// ID to bypass the closed-state damage reduction. Our weapons are
/// identified by section name rather than numeric ID; the `[SigTerm]`
/// section's id is resolved once at registry load
/// (`WeaponRegistry::known().sigterm`) rather than compared by name
/// per hit.
fn byte_armor_multiplier(
    target_kind: UnitKind,
    is_sigterm: bool,
    is_open: bool,
    is_stunned: bool,
) -> f32 {
    if target_kind != UnitKind::Byte {
        return 1.0;
    }
    // Why: SigTerm bypasses the armor bonus (upstream `id == 168`);
    // paralysis forces the Byte open (`!get LUA2` branch returns 100%).
    if is_sigterm || is_stunned || is_open {
        1.0
    } else {
        0.3
    }
}

/// Upstream `byte.bos HitByWeaponId`: a closed Byte takes 30% damage
/// from anything except the SIGTERM bomb, and only while not paralyzed.
fn byte_closed_damage_multiplier(
    target: Entity,
    is_sigterm: bool,
    target_unit_q: &Query<&UnitType>,
    byte_open_q: &Query<&ByteOpen>,
    stunned_q: &Query<&Stunned>,
) -> f32 {
    let Ok(unit) = target_unit_q.get(target) else {
        return 1.0;
    };
    byte_armor_multiplier(
        unit.0,
        is_sigterm,
        byte_open_q.get(target).is_ok(),
        stunned_q.get(target).is_ok(),
    )
}

/// The per-victim state one hit touches, bundled so `apply_damage`
/// stays under Bevy's system-param limit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct HitQueries<'w, 's> {
    health: Query<'w, 's, &'static mut Health>,
    stun: Query<'w, 's, &'static mut StunCharge>,
    shield: Query<'w, 's, &'static mut crate::units::mechanics::shield::ShieldState>,
    protected: Query<'w, 's, (), With<crate::units::mechanics::command_fire::Protected>>,
    idle: Query<'w, 's, &'static mut IdleTimer>,
}

impl HitQueries<'_, '_> {
    /// Reset the victim's idle clock (auto-heal) in place. Every spawned
    /// unit carries an [`IdleTimer`]; the command fallback covers a
    /// target that lacks one (and, via `try_insert`, one despawned by
    /// an earlier command in the same flush).
    fn reset_idle(&mut self, target: Entity, commands: &mut Commands) {
        match self.idle.get_mut(target) {
            Ok(mut idle) => idle.0 = 0.0,
            Err(_) => {
                commands.entity(target).try_insert(IdleTimer(0.0));
            }
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
pub(crate) fn splash_falloff(dist: f32, radius: f32, edge_mult: f32) -> f32 {
    let t = (dist / radius).clamp(0.0, 1.0);
    1.0 - t * (1.0 - edge_mult)
}

/// Release follow-up shots for units in the middle of a burst.
/// The first shot fires where the salvo opens; each follow-up runs one
/// `UpdateSalvo` step ([`super::fire_salvo_shot`]: per projectile
/// `Shot1` → `QueryWeapon1` → fire) once its frame is due, and the last
/// one also calls `EndBurst1`.
pub fn tick_burst_fire(
    time: Res<Time>,
    mut query: Query<(Entity, &mut BurstFire, &GlobalTransform), Without<Dying>>,
    mut pieces: super::PieceLookup,
    unit_registry: Res<UnitRegistry>,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    let frame = super::sim_frame(&time);
    for (entity, mut burst, gtf) in &mut query {
        if frame < burst.next_frame || frame <= burst.last_frame {
            continue;
        }
        let last = burst.shots_remaining <= 1;
        super::fire_salvo_shot(
            &burst.shot,
            gtf,
            last,
            &mut pieces,
            &unit_registry,
            &mut pending_attacks,
            &mut damage_queue,
        );
        commands.entity(entity).try_insert(JustFired);

        burst.shots_remaining = burst.shots_remaining.saturating_sub(1);
        if burst.shots_remaining == 0 {
            commands.entity(entity).remove::<BurstFire>();
        } else {
            burst.last_frame = frame;
            burst.next_frame = frame.saturating_add(burst.salvo_delay);
        }
    }
}

/// Apply a damage hit to `target`. A shield (if any) soaks damage
/// first; a Firewall-protected target then takes only
/// `FIREWALL_DAMAGE_TAKEN` of the leak and reflects the rest back to
/// the attacker; paralyzer weapons accumulate the final amount on the
/// stun charge, promoting to `Stunned` once it exceeds max HP;
/// non-paralyzer leak subtracts from `Health`.
fn apply_hit(
    target: Entity,
    attacker: Entity,
    amount: f32,
    paralyzer: bool,
    paralyze_time: f32,
    hit: &mut HitQueries,
    commands: &mut Commands,
) {
    let HitQueries {
        health: health_q,
        stun: stun_q,
        shield: shield_q,
        protected: protected_q,
        ..
    } = hit;
    let leak = shield_q
        .get_mut(target)
        .map(|mut shield| shield.absorb(amount))
        .unwrap_or(amount);
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
                commands.entity(target).try_insert(Stunned {
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
    mut victims: HitQueries,
    attacker_q: Query<(&UnitType, &Faction, &TeamId)>,
    target_unit_q: Query<&UnitType>,
    target_pos_q: Query<(&GlobalTransform, &UnitStats), With<UnitType>>,
    byte_open_q: Query<&ByteOpen>,
    stunned_q: Query<&Stunned>,
    weapon_registry: Res<WeaponRegistry>,
    unit_registry: Res<UnitRegistry>,
    spatial: Res<SpatialIndex>,
    mut commands: Commands,
    mut splash_hits: Local<Vec<(Entity, f32, bool)>>,
    mut hex_farm: Option<ResMut<crate::map_events::hex_farm::HexFarmInbox>>,
) {
    for pending in damage_queue.drain() {
        // Ids are interned through this same registry — infallible.
        let weapon_def = weapon_registry.by_id(pending.weapon);
        // Every impact is an engine `Explosion` event; Hex Farm's
        // gadget watches them all to damage the ground.
        if let Some(inbox) = hex_farm.as_deref_mut() {
            inbox.explosion(pending.impact_pos, weapon_def.damage.default);
        }
        let infection_window = weapon_registry.infection_duration(pending.weapon);
        let is_sigterm = weapon_registry.known().sigterm == Some(pending.weapon);

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
        let target_hit = match pending.target {
            None => false, // ground-only hit: no primary target
            Some(target) => {
                let kind = target_unit_q.get(target).ok().map(|ut| ut.0);
                let hit = if weapon_def.spray_angle > 0.0 {
                    if let Ok((tgt_xform, tgt_stats)) = target_pos_q.get(target) {
                        tgt_xform.translation().distance(pending.impact_pos) <= tgt_stats.hit_radius
                    } else {
                        false
                    }
                } else {
                    true
                };
                if hit {
                    let raw_damage = kind
                        .map(|k| base(k) * dyn_mult)
                        .unwrap_or(weapon_def.damage.default * dyn_mult);
                    let primary_damage = raw_damage
                        * byte_closed_damage_multiplier(
                            target,
                            is_sigterm,
                            &target_unit_q,
                            &byte_open_q,
                            &stunned_q,
                        );
                    apply_hit(
                        target,
                        pending.attacker,
                        primary_damage,
                        paralyzer,
                        paralyze_time,
                        &mut victims,
                        &mut commands,
                    );
                    victims.reset_idle(target, &mut commands);
                }
                hit
            }
        };

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
                if pending.target == Some(candidate.entity) {
                    return;
                }
                if no_self_damage && candidate.entity == pending.attacker {
                    return;
                }
                if avoid_friendly
                    && let Some((_, _, a_team)) = attacker_info
                    && crate::units::components::is_friendly(candidate.team, a_team.0)
                {
                    return;
                }
                let d_sq = candidate.pos.distance_squared(pending.impact_pos);
                if d_sq >= aoe_sq {
                    return;
                }
                // The spatial snapshot already carries the target's kind —
                // no ECS re-fetch needed per splash candidate.
                let kind = candidate.kind;
                let splash = base(kind) * splash_falloff(d_sq.sqrt(), aoe, edge_mult);
                // infection.lua keys on `UnitDamaged`, which fires for
                // every unit an explosion touches — splash victims of an
                // infector (Wormsplash, VirusDeath) are infected too, as
                // long as they're on another team and not a Virus.
                let infect = infection_window.is_some()
                    && kind != UnitKind::Virus
                    && attacker_info.is_some_and(|(_, _, a_team)| candidate.team != a_team.0);
                splash_hits.push((candidate.entity, splash, infect));
            });
            for (entity, splash, infect) in splash_hits.drain(..) {
                let amount = splash
                    * byte_closed_damage_multiplier(
                        entity,
                        is_sigterm,
                        &target_unit_q,
                        &byte_open_q,
                        &stunned_q,
                    );
                apply_hit(
                    entity,
                    pending.attacker,
                    amount,
                    paralyzer,
                    paralyze_time,
                    &mut victims,
                    &mut commands,
                );
                victims.reset_idle(entity, &mut commands);
                // Why `try_insert` on hit markers: a victim can be
                // despawned by an earlier command in the same flush
                // (death cleanup, Bug/Exploit morph), and a plain
                // insert on a dead entity panics the app.
                if infect
                    && let (Some(duration), Some((_, attacker_faction, attacker_team))) =
                        (infection_window, attacker_info)
                {
                    commands.entity(entity).try_insert(Infected {
                        timer: duration,
                        attacker_faction: *attacker_faction,
                        attacker_team: attacker_team.0,
                    });
                }
            }
        }

        // Apply infection: keyed on the weapon (not the attacker kind)
        // to match upstream LuaRules/Gadgets/infection.lua. VirusBeam,
        // VirusDeath, Wormsplash, and Obelisk Infection each have their
        // own infection window in seconds. Only fires when the primary
        // hit actually landed — a shot that misses on spray angle
        // shouldn't infect the intended target.
        if target_hit
            && let Some(target) = pending.target
            && let Some(duration) = infection_window
            && let Some((_, attacker_faction, attacker_team)) = attacker_info
        {
            let target_is_virus = target_unit_q
                .get(target)
                .is_ok_and(|ut| ut.0 == UnitKind::Virus);
            if !target_is_virus {
                commands.entity(target).try_insert(Infected {
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
    fn byte_closed_takes_30_percent_from_normal_weapons() {
        let m = byte_armor_multiplier(UnitKind::Byte, false, false, false);
        assert!((m - 0.3).abs() < 1e-5);
    }

    #[test]
    fn byte_open_takes_full_damage() {
        let m = byte_armor_multiplier(UnitKind::Byte, false, true, false);
        assert!((m - 1.0).abs() < 1e-5);
    }

    /// Upstream: paralyzed bytes lose the armor bonus (forced open).
    #[test]
    fn byte_closed_but_stunned_takes_full_damage() {
        let m = byte_armor_multiplier(UnitKind::Byte, false, false, true);
        assert!((m - 1.0).abs() < 1e-5);
    }

    /// Upstream: `id == 168` (SIGTERM bomb) bypasses the 30% gate.
    #[test]
    fn byte_closed_takes_full_damage_from_sigterm() {
        let m = byte_armor_multiplier(UnitKind::Byte, true, false, false);
        assert!((m - 1.0).abs() < 1e-5);
    }

    /// Non-Byte targets are unaffected by the armor rule.
    #[test]
    fn non_byte_targets_take_full_damage() {
        let m = byte_armor_multiplier(UnitKind::Bit, false, false, false);
        assert!((m - 1.0).abs() < 1e-5);
    }

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

    /// Upstream salvo cadence in whole sim frames: MegaBeam
    /// (burst=4, burstrate=0.25) → `salvoDelay = int(7.5) = 7`, so the
    /// follow-ups land on frames 7, 14, 21 after the opening shot —
    /// not every 0.25 s of wall time (7.5 frames, which rounded to 8).
    #[test]
    fn megabeam_burst_follow_ups_every_7_frames() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>()
            .insert_resource(UnitRegistry::empty())
            .init_resource::<WeaponRegistry>();

        let target = app.world_mut().spawn_empty().id();
        let delay = super::super::salvo_delay_frames(0.25);
        assert_eq!(delay, 7);
        // Opening shot at frame 0, as `open_salvo` leaves it.
        let attacker = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(attacker).insert((
            GlobalTransform::default(),
            UnitType(UnitKind::Byte),
            BurstFire {
                shot: super::super::SalvoShot {
                    attacker,
                    kind: UnitKind::Byte,
                    weapon: WeaponId::BUILD_LASER,
                    target: Some(target),
                    impact_pos: Vec3::ZERO,
                    is_traveling: false,
                    projectiles: 1,
                },
                shots_remaining: 3,
                last_frame: 0,
                next_frame: delay,
                salvo_delay: delay,
            },
        ));

        let mut fired_on = Vec::new();
        for frame in 0..=30u64 {
            if frame > 0 {
                app.world_mut()
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_secs_f64(1.0 / 30.0));
            }
            let before = app.world().resource::<DamageQueue>().len();
            app.world_mut().run_system_once(tick_burst_fire).unwrap();
            if app.world().resource::<DamageQueue>().len() > before {
                fired_on.push(frame);
            }
        }
        assert_eq!(fired_on, vec![7, 14, 21]);
        assert!(app.world().get::<BurstFire>(attacker).is_none());
    }

    fn burst(attacker: Entity, target: Option<Entity>, kind: UnitKind) -> BurstFire {
        BurstFire {
            shot: super::super::SalvoShot {
                attacker,
                kind,
                weapon: WeaponId::BUILD_LASER,
                target,
                impact_pos: Vec3::ZERO,
                is_traveling: false,
                projectiles: 1,
            },
            shots_remaining: 3,
            last_frame: 0,
            next_frame: 7,
            salvo_delay: 7,
        }
    }

    /// Why: ground-attack bursts (`shot.target == None`) used
    /// to fire one shot per reload because `attack_ground_system`
    /// queued the cooldown but not the burst. Pins parity with the
    /// auto-target path.
    #[test]
    fn ground_attack_burst_fires_follow_ups_with_none_target() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>()
            .insert_resource(UnitRegistry::empty())
            .init_resource::<WeaponRegistry>();

        let attacker = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(attacker).insert((
            GlobalTransform::default(),
            UnitType(UnitKind::Byte),
            burst(attacker, None, UnitKind::Byte),
        ));

        // Advance past the entire burst (a second of sim frames).
        for _ in 0..30 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f64(1.0 / 30.0));
            app.world_mut().run_system_once(tick_burst_fire).unwrap();
        }
        // All 3 follow-up shots must fire. Each pushes a PendingDamage
        // with `target = None` → the splash path in `apply_damage`
        // still reaches everything in AoE around `impact_pos`.
        let dq = app.world().resource::<DamageQueue>();
        assert_eq!(
            dq.len(),
            3,
            "ground-attack burst must still fire 3 follow-ups"
        );
        for entry in dq.iter_snapshot_for_test() {
            assert!(
                entry.target.is_none(),
                "ground-attack burst must keep target=None on every follow-up",
            );
        }
    }

    /// Follow-ups wait for their frame, never double up in one frame,
    /// and each pushes one hitscan damage + one fx event.
    #[test]
    fn burst_fire_releases_shots_at_salvo_delay() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>()
            .insert_resource(UnitRegistry::empty())
            .init_resource::<WeaponRegistry>();

        let target = app.world_mut().spawn_empty().id();
        let attacker = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(attacker).insert((
            GlobalTransform::default(),
            UnitType(UnitKind::Bit),
            burst(attacker, Some(target), UnitKind::Bit),
        ));
        let step = |app: &mut App, frames: u32| {
            for _ in 0..frames {
                app.world_mut()
                    .resource_mut::<Time>()
                    .advance_by(std::time::Duration::from_secs_f64(1.0 / 30.0));
                app.world_mut().run_system_once(tick_burst_fire).unwrap();
            }
        };

        step(&mut app, 6);
        assert_eq!(
            app.world().resource::<DamageQueue>().len(),
            0,
            "not due before frame 7"
        );
        step(&mut app, 1);
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);
        assert_eq!(app.world().resource::<PendingAttacks>().events.len(), 1);
        assert_eq!(
            app.world()
                .get::<BurstFire>(attacker)
                .unwrap()
                .shots_remaining,
            2
        );
        step(&mut app, 14);
        assert_eq!(app.world().resource::<DamageQueue>().len(), 3);
        assert!(app.world().get::<BurstFire>(attacker).is_none());
    }

    fn death_boom_weapon() -> (WeaponRegistry, WeaponId) {
        let mut weapons = WeaponRegistry::default();
        let id = weapons.insert_for_test(
            "RegressionDeathBoom",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 25.0,
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        (weapons, id)
    }

    /// Why: pins the [`DamageQueue`] lifecycle. `apply_damage` must
    /// fully drain the queue, and entries pushed *after* a drain — the
    /// death system's `ExplodeAs` self-hits are queued in Resolve behind
    /// `apply_damage` — must survive until the next drain. The old
    /// `combat_system` `clear()` at the top of the next frame destroyed
    /// exactly those entries, silently dropping every unit's death-AoE
    /// (RetroDeath crowd damage, Virus infection chains).
    #[test]
    fn damage_queue_drains_fully_and_late_pushes_survive() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<DamageQueue>()
            .init_resource::<SpatialIndex>()
            .insert_resource(UnitRegistry::empty());
        let (weapons, boom_id) = death_boom_weapon();
        app.insert_resource(weapons);

        let target = app.world_mut().spawn(Health::full(100.0)).id();
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(PendingDamage {
                target: Some(target),
                attacker: target,
                weapon: boom_id,
                impact_pos: Vec3::ZERO,
                attacker_distance: 0.0,
            });

        // First drain: the hit lands and the queue empties.
        app.world_mut().run_system_once(apply_damage).unwrap();
        let health = app.world().get::<Health>(target).unwrap().current;
        assert!(
            (health - 75.0).abs() < 1e-4,
            "first hit must land, got {health}"
        );
        assert_eq!(app.world().resource::<DamageQueue>().len(), 0);

        // A push after the drain (what `death_system` does in Resolve,
        // behind `apply_damage`) must still be sitting in the queue.
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(PendingDamage {
                target: Some(target),
                attacker: target,
                weapon: boom_id,
                impact_pos: Vec3::ZERO,
                attacker_distance: 0.0,
            });
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);

        // Next frame's drain delivers it; no clear() in between wipes it.
        app.world_mut().run_system_once(apply_damage).unwrap();
        let health = app.world().get::<Health>(target).unwrap().current;
        assert!(
            (health - 50.0).abs() < 1e-4,
            "second hit must land, got {health}"
        );
        assert_eq!(app.world().resource::<DamageQueue>().len(), 0);
    }

    /// Why: `combat_system` used to `damage_queue.clear()` itself on
    /// entry, which destroyed `tick_kamikaze`'s push (it runs earlier in
    /// the Simulate chain, before this system) before any drain saw it.
    /// The system must treat the queue as append-only and leave
    /// consumption to `apply_damage`.
    #[test]
    fn combat_system_preserves_preexisting_queue_entries() {
        use crate::units::combat::combat_system;
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<DamageQueue>()
            .init_resource::<PendingAttacks>()
            .init_resource::<SpatialIndex>()
            .insert_resource(UnitRegistry::empty())
            .insert_resource(WeaponRegistry::default());

        // What `tick_kamikaze` pushes earlier in the same Simulate frame.
        let kamikaze = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(PendingDamage {
                target: Some(kamikaze),
                attacker: kamikaze,
                weapon: WeaponId::BUILD_LASER,
                impact_pos: Vec3::ZERO,
                attacker_distance: 0.0,
            });

        app.world_mut().run_system_once(combat_system).unwrap();

        assert_eq!(
            app.world().resource::<DamageQueue>().len(),
            1,
            "combat_system must not clear pre-existing queue entries — \
             they are delivered by the next apply_damage drain",
        );
    }
    /// Why: upstream infection.lua infects on every `UnitDamaged` from an
    /// infector weapon, so a target-less Wormsplash detonation (worm.bos
    /// `emit-sfx 4097`) must tag enemy splash victims with `Infected` —
    /// that is what turns Worm kills into Viruses. Friendlies are
    /// skipped by `avoidfriendly=1` and never infected.
    #[test]
    fn wormsplash_splash_infects_enemy_victims() {
        use crate::units::spatial::SpatialEntry;
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<DamageQueue>()
            .init_resource::<SpatialIndex>()
            .insert_resource(UnitRegistry::empty());
        let mut weapons = WeaponRegistry::default();
        let splash = weapons.insert_for_test(
            "Wormsplash",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 800.0,
                    ..Default::default()
                },
                area_of_effect: 210.0,
                edge_effectiveness: 1.0,
                avoid_friendly: true,
                ..Default::default()
            },
        );
        app.insert_resource(weapons);

        let worm = app
            .world_mut()
            .spawn((UnitType(UnitKind::Worm), Faction::Hacker, TeamId(1)))
            .id();
        let enemy = app.world_mut().spawn(Health::full(1000.0)).id();
        let friend = app.world_mut().spawn(Health::full(1000.0)).id();
        for (entity, team, x) in [(enemy, 0u8, 50.0), (friend, 1u8, -50.0)] {
            app.world_mut()
                .resource_mut::<SpatialIndex>()
                .insert_for_test(SpatialEntry {
                    entity,
                    pos: Vec3::new(x, 0.0, 0.0),
                    team,
                    kind: UnitKind::Bit,
                    hp_positive: true,
                    is_flying: false,
                    cloaked: false,
                    detected_by: 0,
                });
        }
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(PendingDamage {
                target: None,
                attacker: worm,
                weapon: splash,
                impact_pos: Vec3::ZERO,
                attacker_distance: 0.0,
            });

        app.world_mut().run_system_once(apply_damage).unwrap();

        // edgeeffectiveness=1: full 800 anywhere inside the 210 radius.
        let hp = app.world().get::<Health>(enemy).unwrap().current;
        assert!((hp - 200.0).abs() < 1e-3, "enemy took {hp}");
        let infected = app.world().get::<Infected>(enemy).expect("enemy infected");
        assert_eq!(infected.attacker_team, 1);
        assert!((infected.timer - 200.0 / 30.0).abs() < 1e-4);

        assert!((app.world().get::<Health>(friend).unwrap().current - 1000.0).abs() < 1e-3);
        assert!(app.world().get::<Infected>(friend).is_none());
    }
}

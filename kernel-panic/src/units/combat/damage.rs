//! Damage application, splash falloff, burst follow-ups, and infection.
//!
//! Every point of damage goes through one per-victim step,
//! [`DamageResolver::deliver`] — the port of `CUnit::DoDamage` with the
//! Lua gadgets and script hooks it calls (`ARMORED`, the Firewall,
//! byte.bos, infection.lua). It is fed from two places:
//!
//! - [`DamageQueue`]: weapon impacts. [`apply_damage`] (and
//!   [`apply_death_blasts`] for `ExplodeAs`) turn each into an engine
//!   `Explosion`: the struck unit plus every unit within
//!   `areaOfEffect / 2`, whatever its team, falloff per
//!   [`splash_falloff`].
//! - [`DelayedDamage`]: single-unit [`Hit`]s — far explosion fronts,
//!   Firewall reflections, and the `Spring.AddUnitDamage` calls of the
//!   Lua abilities (area denial, the Mine Launcher's HP cost).
//!
//! Burst-fire follow-ups live in [`tick_burst_fire`] which pushes
//! additional [`PendingDamage`] at `burst_rate` intervals after the
//! initial shot.

use bevy::prelude::*;

use super::{ByteOpen, Dying, IdleTimer, StunCharge, Stunned};
use crate::net::NetId;
use crate::sim::{DetFloat, RngStream, SimRng, SimTick};
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::WeaponId;
use crate::units::content::weapons::WeaponRegistry;
use crate::units::lifecycle::script_triggers::JustFired;
use crate::units::lifecycle::spawning::Emerging;
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

impl PendingDamage {
    /// An explosion of `weapon` at `impact_pos` that no living unit owns
    /// — the SIGTERM bomb, whose Signal has already self-destructed
    /// (signal.bos `FireWeapon1` → `lua_Destroy`), so the engine
    /// credits the blast to nobody.
    pub fn unowned_explosion(weapon: WeaponId, impact_pos: Vec3) -> Self {
        Self {
            target: None,
            attacker: Entity::PLACEHOLDER,
            weapon,
            impact_pos,
            attacker_distance: 0.0,
        }
    }
}

/// Pending damage to apply after combat resolution.
///
/// Lifecycle: producers `push` (hitscan shots and projectile impacts,
/// Wormsplash bites, the Debug's Minekiller blast,
/// death-system `ExplodeAs` self-hits, the SIGTERM bomb), and the two
/// drains — [`apply_damage`], and [`apply_death_blasts`] right behind
/// `death_system` — are the only consumers, each emptying the queue. There is
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
#[derive(Component, Clone)]
#[component(storage = "SparseSet")]
pub struct Infected {
    /// Remaining seconds before the infection expires.
    pub timer: f32,
    /// The faction that will own the spawned Virus.
    pub attacker_faction: Faction,
    /// The team ID that will own the spawned Virus.
    pub attacker_team: u8,
}

#[cfg(test)]
use crate::units::content::weapons::weapon_infection_duration;

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

/// The scripts' `HitByWeaponId` return value (percent of the hit the
/// unit takes), as a factor. Pure, so it can be unit-tested without a
/// live world.
///
/// - kernel.bos / hole.bos / carrier.bos: `if (id==168) return 10` —
///   homebases take 10% from the SIGTERM bomb.
/// - byte.bos: a closed Byte takes 30% from anything but the SIGTERM
///   bomb, unless paralyzed (`!get LUA2`; paralysis forces it open).
///
/// `is_sigterm`: `168` is the `id=` the `[SigTerm]` section of
/// `retroweapons.tdf` declares. Current Recoil ignores that tag and
/// passes its own load-order weapon-def id, so upstream these
/// comparisons no longer match; the port keeps the authors' intent. The
/// `[SigTerm]` id is resolved once at registry load
/// (`WeaponRegistry::known().sigterm`) rather than compared by name per
/// hit. (virus.bos also tests ids 234 and 538, which no Kernel Panic
/// weapon declares, so they never applied and are not ported.)
fn script_hit_multiplier(
    target_kind: UnitKind,
    is_sigterm: bool,
    is_open: bool,
    is_stunned: bool,
) -> f32 {
    match target_kind {
        UnitKind::Kernel | UnitKind::Hole | UnitKind::Carrier if is_sigterm => 0.1,
        UnitKind::Byte if !(is_sigterm || is_stunned || is_open) => 0.3,
        _ => 1.0,
    }
}

/// The unit's COB `ARMORED` flag (`curArmorMultiple`): while present,
/// every hit is multiplied by the FBI `DamageModifier`
/// (`armoredMultiple`, [`UnitRegistry::armored_multiple`]) in
/// `CUnit::DoDamage`. Kernel Panic's scripts `set ARMORED to 1` in
/// `Create()` and clear it once `BUILD_PERCENT_LEFT` reaches zero —
/// immediately for the datavent buildings (whose `DamageModifier=4`
/// makes nanoframes fragile), after a short grace for mobile units
/// (whose `DamageModifier=0.000001` is spawn protection). See
/// [`armored_script`].
#[derive(Component, Debug, Clone, Copy)]
#[component(storage = "SparseSet")]
pub struct Armored {
    /// Seconds the flag stays up once the unit is no longer under
    /// construction (the script's `sleep` after the build loop).
    pub after_build: f32,
}

/// How a kind's `Create()` drives `ARMORED`, from its `.bos`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ArmoredScript {
    /// The `sleep` between the end of the `BUILD_PERCENT_LEFT` loop and
    /// `set ARMORED to 0`, in seconds.
    after_build: f32,
    /// bug.bos only arms when the unit is actually built
    /// (`if (get BUILD_PERCENT_LEFT)` after its first-frame sleep) — a
    /// Bug packed up from an Exploit is never armored.
    only_if_built: bool,
}

/// Per-kind `ARMORED` choreography (scripts/*.bos `Create()`). Kinds
/// whose script never sets the flag (homebases, Exploit, Virus, Logic
/// Bomb, Debug, Bad Block, Signal) return `None`.
fn armored_script(kind: UnitKind) -> Option<ArmoredScript> {
    let after_build = match kind {
        // Buildings drop the flag the moment they finish.
        UnitKind::Socket
        | UnitKind::Window
        | UnitKind::Port
        | UnitKind::Firewall
        | UnitKind::Terminal
        | UnitKind::Obelisk => 0.0,
        UnitKind::Bit => 2.5,
        UnitKind::Bug => 3.0,
        UnitKind::Pointer | UnitKind::Dos => 4.0,
        UnitKind::Assembler
        | UnitKind::Trojan
        | UnitKind::Gateway
        | UnitKind::Packet
        | UnitKind::Connection
        | UnitKind::Worm
        | UnitKind::Flow => 5.0,
        UnitKind::Byte => 6.0,
        _ => return None,
    };
    Some(ArmoredScript {
        after_build,
        only_if_built: kind == UnitKind::Bug,
    })
}

/// Raise `ARMORED` on every new unit whose script sets it, and lower it
/// once the unit has been out of construction (no [`Emerging`]) for its
/// script's grace period. Freshly created units that were never built
/// (map start, Virus spawns) run the same choreography from their first
/// frame, as `Spring.CreateUnit`-made units do upstream.
pub fn update_armored(
    time: Res<Time>,
    added: Query<(Entity, &UnitType, Has<Emerging>), Added<UnitType>>,
    mut armored: Query<(Entity, &mut Armored), Without<Emerging>>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut flag) in &mut armored {
        flag.after_build -= dt;
        if flag.after_build <= 0.0 {
            // `try_remove`: the unit may be despawned earlier in the
            // same flush (a Bug packing into an Exploit, a Packet
            // entering a teleporter).
            commands.entity(entity).try_remove::<Armored>();
        }
    }
    for (entity, unit, being_built) in &added {
        if let Some(script) = armored_script(unit.0)
            && (being_built || !script.only_if_built)
        {
            commands.entity(entity).try_insert(Armored {
                after_build: script.after_build,
            });
        }
    }
}

/// The unit a hit is credited to — `DoDamage`'s `attacker` — captured
/// when the hit is created, so a hit that lands after its source died
/// (an explosion front, a reflection) still knows whose it was.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attacker {
    pub entity: Entity,
    pub team: u8,
    pub faction: Faction,
}

/// One `CUnit::DoDamage` call against a single unit, already resolved
/// to that unit: the weapon's `[DAMAGE]` entry for its armor class,
/// scaled by the explosion's distance falloff — or, for
/// `Spring.AddUnitDamage` callers (area denial, Firewall reflection,
/// the Mine Launcher's HP cost), the flat amount. [`DamageResolver::deliver`]
/// applies everything the victim itself contributes.
#[derive(Debug, Clone)]
pub struct Hit {
    pub target: Entity,
    /// `None` when no living unit owns the hit (the SIGTERM bomb's
    /// Signal has self-destructed; `AddUnitDamage(u, d, 0, -1, …)`).
    pub attacker: Option<Attacker>,
    /// `damages.Get(armorType)` before `ARMORED`, Firewall and script
    /// modifiers.
    pub damage: f32,
    /// The weapon the engine reports to `UnitPreDamaged` / script /
    /// `UnitDamaged` (infection, the Byte's SIGTERM exception).
    pub weapon: Option<WeaponId>,
    /// `paralyzeDamageTime` (seconds) for paralyzer weapons.
    pub paralyze_time: Option<f32>,
    pub impulse: Vec3,
}

/// The per-victim state one hit touches, bundled so the damage systems
/// stay under Bevy's system-param limit.
#[derive(bevy::ecs::system::SystemParam)]
pub struct HitQueries<'w, 's> {
    health: Query<'w, 's, &'static mut Health>,
    stun: Query<'w, 's, &'static mut StunCharge>,
    shield: Query<'w, 's, &'static mut crate::units::mechanics::shield::ShieldState>,
    victim: Query<'w, 's, VictimData>,
    idle: Query<'w, 's, &'static mut IdleTimer>,
}

type VictimData = (
    &'static UnitType,
    &'static TeamId,
    &'static Faction,
    &'static GlobalTransform,
    Has<crate::units::mechanics::command_fire::Protected>,
    Has<Armored>,
    Has<ByteOpen>,
    Has<Stunned>,
    Has<Dying>,
);

/// Everything `DoDamage` reads and writes besides the victim's own
/// components: the per-hit half of the damage pipeline, shared by
/// [`apply_damage`] / [`apply_death_blasts`] (explosions and their
/// fronts) and by every `AddUnitDamage`-style producer that queues a
/// [`Hit`] into [`DelayedDamage`].
#[derive(bevy::ecs::system::SystemParam)]
pub struct DamageResolver<'w, 's> {
    tick: Res<'w, SimTick>,
    /// Unit ids: the canonical order of the due hits.
    ids: Query<'w, 's, &'static NetId>,
    delayed: ResMut<'w, DelayedDamage>,
    hits: HitQueries<'w, 's>,
    weapons: Res<'w, WeaponRegistry>,
    units: Res<'w, UnitRegistry>,
    impulses: ResMut<'w, crate::interaction::ground_move::PendingImpulses>,
    damage_log: Option<ResMut<'w, crate::units::ai::TeamDamageLog>>,
    alliances: Option<Res<'w, crate::units::teams::Alliances>>,
    commands: Commands<'w, 's>,
}

impl DamageResolver<'_, '_> {
    /// `CUnit::DoDamage` (Unit.cpp:1308) for one [`Hit`], in the
    /// engine's order: `curArmorMultiple` (the `ARMORED` flag) and the
    /// idle-heal reset, the ONS shield soak (a port extension standing
    /// in for shield interception), `UnitPreDamaged` (the Firewall's
    /// `network_reflectorshield.lua`), the script's `HitByWeaponId`
    /// ([`script_hit_multiplier`]), `ApplyDamage`, then `UnitDamaged` (`infection.lua`,
    /// KPAI's `lastAllyDamage`). Dead or dying units take nothing
    /// (`if (isDead) return`).
    pub fn deliver(&mut self, hit: Hit) {
        let Ok((unit, team, faction, gtf, protected, armored, open, stunned, dying)) =
            self.hits.victim.get(hit.target)
        else {
            return;
        };
        if dying {
            return;
        }
        let (kind, team, faction, pos) = (unit.0, team.0, *faction, gtf.translation());
        let paralyzer = hit.paralyze_time.is_some();

        let mut damage = hit.damage;
        if damage > 0.0 {
            if armored {
                damage *= self.units.armored_multiple(kind);
            }
            // `restTime = 0`: bleeding != resting.
            match self.hits.idle.get_mut(hit.target) {
                Ok(mut idle) => idle.0 = 0.0,
                Err(_) => {
                    self.commands.entity(hit.target).try_insert(IdleTimer(0.0));
                }
            }
        }
        if damage > 0.0
            && let Ok(mut shield) = self.hits.shield.get_mut(hit.target)
        {
            damage = shield.absorb(damage);
        }

        // network_reflectorshield.lua `UnitPreDamaged`: a protected unit
        // takes half of any non-paralyzer damage and, when the attacker
        // is a living unit of another team, deals the same half back to
        // it next frame (`AddUnitDamage(au, damage*.5, 0, u, weapon)`).
        if damage > 0.0001 && protected && !paralyzer {
            use crate::units::mechanics::command_fire::FIREWALL_DAMAGE_TAKEN;
            if let Some(attacker) = hit.attacker
                && attacker.team != team
                && self.hits.victim.get(attacker.entity).is_ok_and(|v| !v.8)
            {
                let frame = self.tick.frame();
                self.delayed.push(
                    frame + 1,
                    Hit {
                        target: attacker.entity,
                        attacker: Some(Attacker {
                            entity: hit.target,
                            team,
                            faction,
                        }),
                        damage: damage * (1.0 - FIREWALL_DAMAGE_TAKEN),
                        weapon: hit.weapon,
                        paralyze_time: None,
                        impulse: Vec3::ZERO,
                    },
                );
            }
            damage *= FIREWALL_DAMAGE_TAKEN;
        }

        // Script `HitByWeaponId`: homebases vs SIGTERM, closed Bytes.
        let is_sigterm = hit.weapon.is_some() && hit.weapon == self.weapons.known().sigterm;
        damage *= script_hit_multiplier(kind, is_sigterm, open, stunned);

        if damage > 0.0 {
            if let Some(paralyze_time) = hit.paralyze_time {
                if let Ok(max_hp) = self.hits.health.get(hit.target).map(|h| h.max)
                    && let Ok(mut charge) = self.hits.stun.get_mut(hit.target)
                {
                    charge.0 += damage;
                    if charge.0 >= max_hp {
                        self.commands.entity(hit.target).try_insert(Stunned {
                            remaining: paralyze_time,
                        });
                    }
                }
            } else if let Ok(mut health) = self.hits.health.get_mut(hit.target) {
                health.current -= damage;
            }
        }
        if hit.impulse != Vec3::ZERO {
            self.impulses.0.push((hit.target, hit.impulse));
        }

        // infection.lua `UnitDamaged`: an infector weapon from a unit
        // of another team tags anything but a Virus, whatever the
        // damage. Why `try_insert`: a victim can be despawned by an
        // earlier command in the same flush (death cleanup, Bug/Exploit
        // morph), and a plain insert on a dead entity panics the app.
        if let Some(weapon) = hit.weapon
            && let Some(duration) = self.weapons.infection_duration(weapon)
            && let Some(attacker) = hit.attacker
            && attacker.team != team
            && kind != UnitKind::Virus
        {
            self.commands.entity(hit.target).try_insert(Infected {
                timer: duration,
                attacker_faction: attacker.faction,
                attacker_team: attacker.team,
            });
        }
        // KPAI_Fair `UnitDamaged`: every AI team allied with the victim
        // (`AreTeamsAllied(team, t)`, itself included) remembers where
        // its side last bled (`lastAllyDamage`).
        if damage >= 0.5
            && let Some(log) = self.damage_log.as_deref_mut()
        {
            let now = self.tick.secs();
            match self.alliances.as_deref() {
                Some(alliances) if alliances.is_seated(team) => {
                    for ally in alliances.allies_of(team) {
                        log.record(ally, now, pos);
                    }
                }
                _ => log.record(team, now, pos),
            }
        }
    }

    /// Deliver every queued hit that is due this frame.
    fn deliver_due(&mut self) {
        let frame = self.tick.frame();
        let mut due = self.delayed.take_due(frame);
        // A canonical order whoever queued them (`sim`): health sums
        // the hits in it. Stable, so exact duplicates keep theirs.
        let id = |e: Entity| self.ids.get(e).map_or(0, |n| n.0);
        due.sort_by_cached_key(|h| {
            (
                id(h.target),
                h.attacker.as_ref().map_or(0, |a| id(a.entity)),
                h.weapon,
                h.damage.to_bits(),
                h.paralyze_time.map(f32::to_bits),
            )
        });
        for hit in due {
            self.deliver(hit);
        }
    }
}

/// `CGameHelper::DoExplosionDamage`'s distance modifier
/// (GameHelper.cpp:107-172): `dist` is the distance from the explosion
/// to the victim's collision-volume *surface*, `radius` the damage
/// radius (`areaOfEffect / 2`), `edge` the weapon's `edgeEffectiveness`.
/// `(R + 0.001 − d) / (R + 0.001 − d·edge)`: linear at `edge = 0`,
/// flatter toward the centre as `edge` rises, full damage at `edge = 1`
/// or within one elmo. Callers must ensure `dist <= radius`.
pub(crate) fn splash_falloff(dist: f32, radius: f32, edge: f32) -> f32 {
    if edge == 1.0 || dist < 1.0 {
        return 1.0;
    }
    (radius + 0.001 - dist) / (radius + 0.001 - dist * edge)
}

/// `MAX_EXPLOSION_IMPULSE` (GlobalConstants.h:136).
const MAX_EXPLOSION_IMPULSE: f32 = 1e4;

/// One splash victim collected inside the spatial query, applied after.
pub struct SplashHit {
    entity: Entity,
    damage: f32,
    pos: Vec3,
    /// The distance modifier, reused for the impulse.
    falloff: f32,
    /// Distance from the explosion to the victim's collision sphere.
    dist: f32,
}

/// Single-unit hits waiting for their frame: explosion fronts that have
/// not reached a far victim yet (`CGameHelper::waitingDamages`: victims
/// further than `4 · explosionSpeed` take their damage
/// `int(d / explosionSpeed) − 3` frames later), the Firewall's
/// reflections (next frame), and the `Spring.AddUnitDamage` calls of
/// the Lua abilities (area-denial ticks, the Mine Launcher's HP cost),
/// which are due at once. Drained by the damage systems through
/// [`DamageResolver::deliver`]; a victim that died meanwhile is skipped.
#[derive(Resource, Default)]
pub struct DelayedDamage(Vec<(u64, Hit)>);

impl DelayedDamage {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Queue `hit` for sim frame `due`.
    pub fn push(&mut self, due: u64, hit: Hit) {
        self.0.push((due, hit));
    }

    /// Queue `hit` for the next damage drain (`Spring.AddUnitDamage`).
    pub fn push_now(&mut self, hit: Hit) {
        self.push(0, hit);
    }

    fn take_due(&mut self, frame: u64) -> Vec<Hit> {
        let mut due = Vec::new();
        self.0.retain(|(at, hit)| {
            if *at <= frame {
                due.push(hit.clone());
                false
            } else {
                true
            }
        });
        due
    }

    #[cfg(test)]
    pub fn iter_for_test(&self) -> impl Iterator<Item = &(u64, Hit)> {
        self.0.iter()
    }
}

/// [`tick_burst_fire`]'s random stream (`predictSpeedMod` re-rolls).
pub struct BurstRng;
impl RngStream for BurstRng {
    const SALT: u64 = 0xB0B5_7A7E;
}

/// Release follow-up shots for units in the middle of a burst.
/// The first shot fires where the salvo opens; each follow-up runs one
/// `UpdateSalvo` step ([`super::fire_salvo_shot`]: per projectile
/// `Shot1` → `QueryWeapon1` → fire) once its frame is due, and the last
/// one also calls `EndBurst1`.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn tick_burst_fire(
    tick: Res<SimTick>,
    // `CUnit::CanUpdateWeapons`: a paralyzed unit's salvo pauses (its
    // `nextSalvo` frame passes; the rest fires once the stun ends).
    mut query: Query<
        (Entity, &mut BurstFire, &GlobalTransform, &NetId),
        (Without<Dying>, Without<Stunned>),
    >,
    mut pieces: super::PieceLookup,
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    mut commands: Commands,
    mut damage_queue: ResMut<DamageQueue>,
    mut pending_attacks: ResMut<PendingAttacks>,
    mut sounds: Option<ResMut<crate::audio::SoundQueue>>,
    mut rng: ResMut<SimRng<BurstRng>>,
) {
    let mut scratch_sounds = crate::audio::SoundQueue::default();
    let sounds = sounds.as_deref_mut().unwrap_or(&mut scratch_sounds);
    let frame = tick.frame();
    let slow_rate = crate::sim::SLOW_UPDATE_RATE as u64;
    // Unit-id order: the dice and the damage queue follow it (`sim`).
    for (entity, mut burst, gtf, _) in query.iter_mut().sort::<&NetId>() {
        if frame < burst.next_frame || frame <= burst.last_frame {
            continue;
        }
        // `CWeapon::SlowUpdate` re-rolls `predictSpeedMod` every 15
        // frames, so a 21-frame burst leads with one or two guesses.
        if frame / slow_rate != burst.shot.predict_frame / slow_rate {
            burst.shot.predict_mult = super::roll_predict_mult(&mut rng);
            burst.shot.predict_frame = frame;
        }
        let last = burst.shots_remaining <= 1;
        super::fire_salvo_shot(
            &burst.shot,
            weapon_registry.by_id(burst.shot.weapon),
            gtf,
            last,
            &mut pieces,
            &unit_registry,
            &mut pending_attacks,
            &mut damage_queue,
            sounds,
            &mut rng,
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

/// The explosion half of the damage pipeline: drains [`DamageQueue`]
/// (every weapon impact is an engine `Explosion`) and the due
/// [`DelayedDamage`] hits, resolving each victim through
/// [`DamageResolver::deliver`]. Bundled as one param so the two drain
/// points ([`apply_damage`], [`apply_death_blasts`]) share it.
#[derive(bevy::ecs::system::SystemParam)]
pub struct DamagePass<'w, 's> {
    queue: ResMut<'w, DamageQueue>,
    resolver: DamageResolver<'w, 's>,
    /// A second handle on the registry the resolver holds, so a weapon
    /// def can stay borrowed while victims are resolved.
    weapons: Res<'w, WeaponRegistry>,
    attacker_q: Query<'w, 's, (&'static Faction, &'static TeamId), With<UnitType>>,
    target_unit_q: Query<'w, 's, &'static UnitType>,
    target_pos_q: Query<'w, 's, &'static GlobalTransform, With<UnitType>>,
    spatial: Res<'w, SpatialIndex>,
    splash_hits: Local<'s, Vec<SplashHit>>,
    hex_farm: Option<ResMut<'w, crate::map_events::hex_farm::HexFarmInbox>>,
}

impl DamagePass<'_, '_> {
    fn run(&mut self) {
        // Explosion fronts, reflections and `AddUnitDamage` hits due now.
        self.resolver.deliver_due();
        let frame = self.resolver.tick.frame();

        let mut pending: Vec<PendingDamage> = self.queue.drain().collect();
        // The queue fills in whatever order its producers ran — some
        // iterate projectiles or zones, which have no unit id. Resolve
        // it in a canonical order (`sim`): victims' health sums the
        // hits, and the impulses and delayed fronts queue, in it.
        let id = |e: Entity| self.resolver.ids.get(e).map_or(0, |n| n.0);
        pending.sort_by_cached_key(|p| {
            (
                id(p.attacker),
                p.weapon,
                p.impact_pos.to_array().map(f32::to_bits),
                p.attacker_distance.to_bits(),
                p.target.map_or(0, id),
            )
        });
        for pending in pending {
            self.explode(pending, frame);
        }
    }

    /// One `CGameHelper::Explosion` (or, for `Melee`, the single
    /// `DoDamage` of `CMeleeWeapon::FireImpl`).
    fn explode(&mut self, pending: PendingDamage, frame: u64) {
        // Ids are interned through this same registry — infallible.
        let weapons = &*self.weapons;
        let weapon_def = weapons.by_id(pending.weapon);
        // `CMeleeWeapon::FireImpl` damages its target unit and nothing
        // else: no explosion, no splash, no hit without a unit target.
        let melee = weapon_def.category() == spring_tdf::WeaponCategory::Melee;
        // Every other impact is an engine `Explosion` event; Hex Farm's
        // gadget watches them all to damage the ground.
        if !melee && let Some(inbox) = self.hex_farm.as_deref_mut() {
            inbox.explosion(pending.impact_pos, weapon_def.damage.default);
        }
        let attacker = self
            .attacker_q
            .get(pending.attacker)
            .ok()
            .map(|(faction, team)| Attacker {
                entity: pending.attacker,
                team: team.0,
                faction: *faction,
            });
        let paralyze_time = weapon_def.paralyzer.then_some(weapon_def.paralyze_time);
        // `GetDynamicDamages` scales the projectile's whole damage array,
        // so the falloff reaches splash victims too.
        let dyn_mult = weapon_def.dyn_damage_multiplier(pending.attacker_distance);
        let base = |kind: UnitKind| weapon_def.damage.for_type(kind.armor_class().key()) * dyn_mult;

        // `CalcImpulseScale`: the knock-back uses the *default* damage,
        // scaled by the distance modifier and clamped. KP weapons set
        // `impulseFactor=0`, so only hero-modoption weapons push.
        let attacker_pos = self
            .target_pos_q
            .get(pending.attacker)
            .map(|g| g.translation())
            .ok();
        let impulse_of = |victim_pos: Vec3, falloff: f32| -> Vec3 {
            if weapon_def.impulse_factor == 0.0 {
                return Vec3::ZERO;
            }
            let scale = (weapon_def.impulse_factor
                * falloff
                * (weapon_def.damage.default + weapon_def.impulse_boost))
                .det_clamp(-MAX_EXPLOSION_IMPULSE, MAX_EXPLOSION_IMPULSE);
            // `volPos - expPos`; a direct hit's impact point is the
            // victim's own position here, where the engine's lies on the
            // collision surface facing the shooter: use the shot's
            // direction then.
            let mut dir = (victim_pos - pending.impact_pos).normalize_or_zero();
            if dir == Vec3::ZERO
                && let Some(from) = attacker_pos
            {
                dir = (victim_pos - from).normalize_or_zero();
            }
            dir * scale
        };
        let hit = |target: Entity, damage: f32, impulse: Vec3| Hit {
            target,
            attacker,
            damage,
            weapon: Some(pending.weapon),
            paralyze_time,
            impulse,
        };

        // `target` is the unit the shot actually struck (a projectile's
        // collision, or a hitscan beam's target): the explosion sits on
        // its collision surface, so it takes the full weapon damage
        // (`DoExplosionDamage`: `expDist < 1 → mod 1`). A bolt that
        // missed arrives with `target == None` and only splashes.
        if let Some(target) = pending.target {
            let damage = self
                .target_unit_q
                .get(target)
                .map_or(weapon_def.damage.default * dyn_mult, |ut| base(ut.0));
            let impulse = self
                .target_pos_q
                .get(target)
                .map_or(Vec3::ZERO, |g| impulse_of(g.translation(), 1.0));
            let primary = hit(target, damage, impulse);
            self.resolver.deliver(primary);
        }
        if melee {
            return;
        }

        // `damageAreaOfEffect = areaOfEffect · 0.5` (WeaponDef.cpp:71), at
        // least 1 (`CGameHelper::Explosion`).
        let aoe = (weapon_def.area_of_effect * 0.5).det_max(1.0);
        let edge_mult = weapon_def.edge_effectiveness;
        let no_self_damage = weapon_def.no_self_damage;
        // Collect first, then deliver — delivery borrows the victim
        // queries mutably, so it can't run inside the spatial callback.
        // Re-uses a Local buffer across calls to avoid per-hit allocation.
        let splash_hits = &mut *self.splash_hits;
        splash_hits.clear();
        let reach = aoe + self.spatial.max_hit_radius();
        self.spatial
            .query_radius(pending.impact_pos, reach, |candidate| {
                if pending.target == Some(candidate.entity) {
                    return;
                }
                // `DoExplosionDamage` (GameHelper.cpp:122) skips only the
                // owner, and only for `ignoreOwner` (`noSelfDamage`; also
                // `noExplode`, set only by the unused FakeBugCannon).
                // There is no team filter: allies in the blast take it
                // like anyone else. `avoidfriendly` is a firing rule
                // ([`super::free_line_of_fire`]), not a damage one.
                if no_self_damage && candidate.entity == pending.attacker {
                    return;
                }
                // Distance to the collision sphere's surface
                // (`GetPointSurfaceDistance`), 0 inside it.
                let d = candidate.surface_distance(pending.impact_pos);
                if d > aoe {
                    return;
                }
                let falloff = splash_falloff(d, aoe, edge_mult);
                // The spatial snapshot already carries the target's kind —
                // no ECS re-fetch needed per splash candidate.
                splash_hits.push(SplashHit {
                    entity: candidate.entity,
                    damage: base(candidate.kind) * falloff,
                    pos: candidate.pos,
                    falloff,
                    dist: d,
                });
            });
        // `DoExplosionDamage`: the front reaches far victims later.
        let exp_speed = weapon_def.explosion_speed();
        let splash: Vec<(Hit, Option<u64>)> = splash_hits
            .drain(..)
            .map(|s| {
                let later = (s.dist >= exp_speed * 4.0)
                    .then(|| frame + ((s.dist / exp_speed) as u64).saturating_sub(3));
                (hit(s.entity, s.damage, impulse_of(s.pos, s.falloff)), later)
            })
            .collect();
        for (splash_hit, later) in splash {
            match later {
                Some(due) => self.resolver.delayed.push(due, splash_hit),
                None => self.resolver.deliver(splash_hit),
            }
        }
    }
}

/// Resolve the frame's weapon impacts: every [`PendingDamage`] is an
/// engine `Explosion` whose victims — the struck unit, then everything
/// whose collision sphere lies within `areaOfEffect / 2` of the impact
/// (falloff per [`splash_falloff`]) — each take one `DoDamage`
/// ([`DamageResolver::deliver`]). Allies are hit like enemies, as in the
/// engine; only `noselfdamage=1` spares the attacker itself. The
/// friendly-fire rules Kernel Panic's gadgets add live in `deliver`
/// (infection and Firewall reflection skip the attacker's own team) and
/// in the area-denial zones (`damageFriendly`).
pub fn apply_damage(mut pass: DamagePass) {
    pass.run();
}

/// The same drain, run right behind `death_system`: a dying unit's
/// `ExplodeAs` goes off in `KillUnit` the frame it dies, while the
/// corpse is still a unit — so its blast is credited to it
/// (VirusDeath's infection team, `noselfdamage`) before
/// `cleanup_dying` can despawn it.
pub fn apply_death_blasts(mut pass: DamagePass) {
    pass.run();
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
    use crate::sim::test_support::InitSim;

    #[test]
    fn byte_closed_takes_30_percent_from_normal_weapons() {
        let m = script_hit_multiplier(UnitKind::Byte, false, false, false);
        assert!((m - 0.3).abs() < 1e-5);
    }

    #[test]
    fn byte_open_takes_full_damage() {
        let m = script_hit_multiplier(UnitKind::Byte, false, true, false);
        assert!((m - 1.0).abs() < 1e-5);
    }

    /// Upstream: paralyzed bytes lose the armor bonus (forced open).
    #[test]
    fn byte_closed_but_stunned_takes_full_damage() {
        let m = script_hit_multiplier(UnitKind::Byte, false, false, true);
        assert!((m - 1.0).abs() < 1e-5);
    }

    /// Upstream: `id == 168` (SIGTERM bomb) bypasses the 30% gate.
    #[test]
    fn byte_closed_takes_full_damage_from_sigterm() {
        let m = script_hit_multiplier(UnitKind::Byte, true, false, false);
        assert!((m - 1.0).abs() < 1e-5);
    }

    /// Non-Byte targets are unaffected by the armor rule.
    #[test]
    fn non_byte_targets_take_full_damage() {
        let m = script_hit_multiplier(UnitKind::Bit, false, false, false);
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

    /// The engine's `expMod`: `edge` shapes the curve rather than
    /// setting the damage at the rim — the rim always tends to zero
    /// unless `edge = 1`.
    #[test]
    fn splash_falloff_matches_the_engine() {
        // Linear at edge 0: half way out → half damage, rim → ~0.
        assert!((splash_falloff(256.0, 512.0, 0.0) - 0.5).abs() < 1e-4);
        assert!(splash_falloff(512.0, 512.0, 0.0).abs() < 1e-5);
        // Quarter out at edge 0.4: (512.001 − 128) / (512.001 − 51.2).
        let expected = (512.001 - 128.0) / (512.001 - 128.0 * 0.4);
        assert!((splash_falloff(128.0, 512.0, 0.4) - expected).abs() < 1e-5);
        // Edge 0.8 at the rim is nearly nothing, not 80%.
        assert!(splash_falloff(512.0, 512.0, 0.8) < 0.01);
        // Edge 1 is flat; so is anything within an elmo.
        assert!((splash_falloff(256.0, 512.0, 1.0) - 1.0).abs() < 1e-5);
        assert!((splash_falloff(0.5, 512.0, 0.0) - 1.0).abs() < 1e-5);
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
            .init_sim()
            .init_resource::<DamageQueue>()
            .init_resource::<DelayedDamage>()
            .init_resource::<Time>()
            .init_sim()
            .init_resource::<crate::interaction::ground_move::PendingImpulses>()
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
                    aim_pos: Vec3::ZERO,
                    is_traveling: false,
                    projectiles: 1,
                    predict_mult: 1.0,
                    predict_frame: 0,
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
                crate::sim::test_support::next_frame(app.world_mut());
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
                aim_pos: Vec3::ZERO,
                is_traveling: false,
                projectiles: 1,
                predict_mult: 1.0,
                predict_frame: 0,
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
            .init_sim()
            .init_resource::<DamageQueue>()
            .init_resource::<DelayedDamage>()
            .init_resource::<Time>()
            .init_sim()
            .init_resource::<crate::interaction::ground_move::PendingImpulses>()
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
            crate::sim::test_support::next_frame(app.world_mut());
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
            .init_sim()
            .init_resource::<DamageQueue>()
            .init_resource::<DelayedDamage>()
            .init_resource::<Time>()
            .init_sim()
            .init_resource::<crate::interaction::ground_move::PendingImpulses>()
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
                crate::sim::test_support::next_frame(app.world_mut());
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

    /// A world with everything the damage systems read.
    fn damage_app(weapons: WeaponRegistry, units: UnitRegistry) -> App {
        let mut app = App::new();
        app.init_resource::<DamageQueue>()
            .init_resource::<DelayedDamage>()
            .init_resource::<Time>()
            .init_sim()
            .init_resource::<crate::interaction::ground_move::PendingImpulses>()
            .init_resource::<SpatialIndex>()
            .init_resource::<crate::units::teams::Alliances>()
            .init_resource::<crate::units::ai::TeamDamageLog>()
            .insert_resource(units)
            .insert_resource(weapons);
        app
    }

    /// A unit as the damage pipeline sees it, also entered in the
    /// spatial index (as `rebuild_spatial_index` would).
    fn unit(app: &mut App, kind: UnitKind, team: u8, pos: Vec3, hp: f32) -> Entity {
        use crate::units::spatial::SpatialEntry;
        let entity = app
            .world_mut()
            .spawn((
                UnitType(kind),
                TeamId(team),
                Faction::System,
                GlobalTransform::from_translation(pos),
                Health::full(hp),
                StunCharge::default(),
                IdleTimer(3.0),
            ))
            .id();
        app.world_mut()
            .resource_mut::<SpatialIndex>()
            .insert_for_test(SpatialEntry {
                entity,
                pos,
                hit_radius: 0.0,
                mid_y: 0.0,
                team,
                kind,
                hp_positive: true,
                is_flying: false,
                cloaked: false,
                detected_by: 0,
            });
        entity
    }

    fn hp(app: &App, entity: Entity) -> f32 {
        app.world().get::<Health>(entity).unwrap().current
    }

    fn shot(target: Option<Entity>, attacker: Entity, weapon: WeaponId, at: Vec3) -> PendingDamage {
        PendingDamage {
            target,
            attacker,
            weapon,
            impact_pos: at,
            attacker_distance: 0.0,
        }
    }

    /// Advance the sim clock by one 30 Hz frame.
    fn next_frame(app: &mut App) {
        crate::sim::test_support::next_frame(app.world_mut());
    }

    fn run_damage(app: &mut App) {
        use bevy::ecs::system::RunSystemOnce;
        app.world_mut().run_system_once(apply_damage).unwrap();
    }

    /// Why: pins the [`DamageQueue`] lifecycle. `apply_damage` must
    /// fully drain the queue, and entries pushed *after* a drain must
    /// survive until the next one. The old `combat_system` `clear()` at
    /// the top of the next frame destroyed exactly those entries,
    /// silently dropping every unit's death-AoE (RetroDeath crowd
    /// damage, Virus infection chains).
    #[test]
    fn damage_queue_drains_fully_and_late_pushes_survive() {
        let (weapons, boom_id) = death_boom_weapon();
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let target = unit(&mut app, UnitKind::Bit, 0, Vec3::ZERO, 100.0);
        let source = unit(&mut app, UnitKind::Bit, 1, Vec3::X * 500.0, 100.0);
        let hit = shot(Some(target), source, boom_id, Vec3::ZERO);
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(hit.clone());

        // First drain: the hit lands and the queue empties.
        run_damage(&mut app);
        assert!(
            (hp(&app, target) - 75.0).abs() < 1e-4,
            "first hit must land"
        );
        assert_eq!(app.world().resource::<DamageQueue>().len(), 0);

        // A push after the drain must still be sitting in the queue.
        app.world_mut().resource_mut::<DamageQueue>().push(hit);
        assert_eq!(app.world().resource::<DamageQueue>().len(), 1);

        // Next frame's drain delivers it; no clear() in between wipes it.
        run_damage(&mut app);
        assert!(
            (hp(&app, target) - 50.0).abs() < 1e-4,
            "second hit must land"
        );
        assert_eq!(app.world().resource::<DamageQueue>().len(), 0);
    }

    /// Why: `combat_system` used to `damage_queue.clear()` itself on
    /// entry, which destroyed anything queued earlier in the same
    /// Simulate frame before any drain saw it. The system must treat
    /// the queue as append-only and leave consumption to the drains.
    #[test]
    fn combat_system_preserves_preexisting_queue_entries() {
        use crate::units::combat::combat_system;
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_sim()
            .init_resource::<DamageQueue>()
            .init_resource::<DelayedDamage>()
            .init_resource::<Time>()
            .init_sim()
            .init_resource::<crate::interaction::ground_move::PendingImpulses>()
            .init_resource::<PendingAttacks>()
            .init_resource::<SpatialIndex>()
            .init_resource::<crate::units::teams::Alliances>()
            .init_resource::<crate::units::weapon_fx::PendingExplosions>()
            .insert_resource(UnitRegistry::empty())
            .insert_resource(WeaponRegistry::default());

        // Something queued earlier in the same Simulate frame.
        let earlier = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(PendingDamage {
                target: Some(earlier),
                attacker: earlier,
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
    /// that is what turns Worm kills into Viruses. The Worm's allies in
    /// the blast take the damage too (`DoExplosionDamage` has no team
    /// filter; `avoidfriendly=1` only governs firing) but are never
    /// infected (`unitTeam ~= attackerTeam`).
    #[test]
    fn wormsplash_splash_infects_enemy_victims() {
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
                // `explosionspeed=65535`: the front is instant.
                explosion_speed_raw: 65535.0,
                avoid_friendly: true,
                ..Default::default()
            },
        );
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let worm = app
            .world_mut()
            .spawn((UnitType(UnitKind::Worm), Faction::Hacker, TeamId(1)))
            .id();
        let enemy = unit(&mut app, UnitKind::Bit, 0, Vec3::X * 50.0, 1000.0);
        let friend = unit(&mut app, UnitKind::Bit, 1, Vec3::X * -50.0, 1000.0);
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(shot(None, worm, splash, Vec3::ZERO));

        run_damage(&mut app);

        // edgeeffectiveness=1: full 800 anywhere inside the 210 radius.
        assert!((hp(&app, enemy) - 200.0).abs() < 1e-3);
        let infected = app.world().get::<Infected>(enemy).expect("enemy infected");
        assert_eq!(infected.attacker_team, 1);
        assert_eq!(infected.attacker_faction, Faction::Hacker);
        assert!((infected.timer - 200.0 / 30.0).abs() < 1e-4);

        assert!(
            (hp(&app, friend) - 200.0).abs() < 1e-3,
            "splash hurts allies like the engine"
        );
        assert!(app.world().get::<Infected>(friend).is_none());
    }

    fn flat_weapon(weapons: &mut WeaponRegistry, name: &str, damage: f32) -> WeaponId {
        weapons.insert_for_test(
            name,
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: damage,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
    }

    /// network_reflectorshield.lua: a protected unit takes half and an
    /// attacker of another team gets the other half next frame — through
    /// the same pipeline, so a protected attacker would halve it again.
    #[test]
    fn firewall_halves_and_reflects_onto_the_attacker_next_frame() {
        use crate::units::mechanics::command_fire::Protected;
        let mut weapons = WeaponRegistry::default();
        let gun = flat_weapon(&mut weapons, "Gun", 100.0);
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let victim = unit(&mut app, UnitKind::Bit, 0, Vec3::ZERO, 1000.0);
        let shooter = unit(&mut app, UnitKind::Bit, 1, Vec3::X * 300.0, 1000.0);
        app.world_mut()
            .entity_mut(victim)
            .insert(Protected { remaining: 20.0 });
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            Some(victim),
            shooter,
            gun,
            Vec3::ZERO,
        ));

        run_damage(&mut app);
        assert!((hp(&app, victim) - 950.0).abs() < 1e-3);
        assert!(
            (hp(&app, shooter) - 1000.0).abs() < 1e-3,
            "the reflection waits a frame (`damageList` → `GameFrame`)"
        );

        next_frame(&mut app);
        run_damage(&mut app);
        assert!((hp(&app, shooter) - 950.0).abs() < 1e-3);
        assert!(app.world().resource::<DelayedDamage>().is_empty());
    }

    /// The Firewall ignores paralyzer damage (`not para`), and never
    /// reflects onto its own team (`ateam ~= team`) — though the halving
    /// still applies to friendly fire.
    #[test]
    fn firewall_skips_paralyzers_and_same_team_reflection() {
        use crate::units::mechanics::command_fire::Protected;
        let mut weapons = WeaponRegistry::default();
        let stun = weapons.insert_for_test(
            "Stun",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 100.0,
                    ..Default::default()
                },
                paralyzer: true,
                paralyze_time: 5.0,
                ..Default::default()
            },
        );
        let gun = flat_weapon(&mut weapons, "Gun", 100.0);
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let victim = unit(&mut app, UnitKind::Bit, 0, Vec3::ZERO, 1000.0);
        let enemy = unit(&mut app, UnitKind::Bit, 1, Vec3::X * 300.0, 1000.0);
        let ally = unit(&mut app, UnitKind::Bit, 0, Vec3::X * -300.0, 1000.0);
        app.world_mut()
            .entity_mut(victim)
            .insert(Protected { remaining: 20.0 });
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            Some(victim),
            enemy,
            stun,
            Vec3::ZERO,
        ));
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            Some(victim),
            ally,
            gun,
            Vec3::ZERO,
        ));

        run_damage(&mut app);
        let charge = app.world().get::<StunCharge>(victim).unwrap().0;
        assert!(
            (charge - 100.0).abs() < 1e-3,
            "paralyzer not halved: {charge}"
        );
        assert!(
            (hp(&app, victim) - 950.0).abs() < 1e-3,
            "friendly fire halved"
        );
        assert!(
            app.world().resource::<DelayedDamage>().is_empty(),
            "nothing reflected"
        );
    }

    /// `DamageModifier` (`armoredMultiple`) applies only while the unit
    /// is `ARMORED`: a finished Socket takes 1×, its nanoframe 4×.
    #[test]
    fn damage_modifier_applies_only_while_armored() {
        let mut defs = spring_tdf::UnitDefs::default();
        defs.units.insert(
            "socket".into(),
            spring_tdf::UnitDef {
                damage_modifier: 4.0,
                ..Default::default()
            },
        );
        let mut weapons = WeaponRegistry::default();
        let gun = flat_weapon(&mut weapons, "Gun", 100.0);
        let mut app = damage_app(weapons, UnitRegistry::for_test(defs));
        let shooter = unit(&mut app, UnitKind::Bit, 1, Vec3::X * 300.0, 1000.0);
        let finished = unit(&mut app, UnitKind::Socket, 0, Vec3::ZERO, 1000.0);
        let nanoframe = unit(&mut app, UnitKind::Socket, 0, Vec3::Z * 900.0, 1000.0);
        app.world_mut()
            .entity_mut(nanoframe)
            .insert(Armored { after_build: 0.0 });
        for (target, at) in [(finished, Vec3::ZERO), (nanoframe, Vec3::Z * 900.0)] {
            app.world_mut().resource_mut::<DamageQueue>().push(shot(
                Some(target),
                shooter,
                gun,
                at,
            ));
        }
        run_damage(&mut app);
        assert!((hp(&app, finished) - 900.0).abs() < 1e-3);
        assert!((hp(&app, nanoframe) - 600.0).abs() < 1e-3);
    }

    /// The `ARMORED` choreography: raised on a new unit whose script
    /// sets it, held while it is being built, then dropped after the
    /// script's grace; a Bug that was never built (Exploit pack-up) is
    /// never armored, kinds whose script ignores the flag neither.
    #[test]
    fn armored_flag_follows_the_scripts() {
        let mut app = App::new();
        app.init_resource::<Time>().init_sim();
        let bit = app.world_mut().spawn(UnitType(UnitKind::Bit)).id();
        let built_bug = app
            .world_mut()
            .spawn((
                UnitType(UnitKind::Bug),
                Emerging {
                    target_y: 0.0,
                    remaining: 1.0,
                    total: 1.0,
                    rally_point: None,
                    rally_then: None,
                    style: crate::units::lifecycle::spawning::EmergeStyle::Fade,
                },
            ))
            .id();
        let packed_bug = app.world_mut().spawn(UnitType(UnitKind::Bug)).id();
        let kernel = app.world_mut().spawn(UnitType(UnitKind::Kernel)).id();
        // One registered system, so `Added` is judged against its last run.
        let system = app.world_mut().register_system(update_armored);
        app.world_mut().run_system(system).unwrap();
        assert_eq!(
            app.world().get::<Armored>(bit).map(|a| a.after_build),
            Some(2.5)
        );
        assert!(app.world().get::<Armored>(built_bug).is_some());
        assert!(app.world().get::<Armored>(packed_bug).is_none());
        assert!(app.world().get::<Armored>(kernel).is_none());

        // 3 s later the Bit's 2.5 s grace is over; the Bug is still
        // under construction, so its grace hasn't started.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(3));
        app.world_mut().run_system(system).unwrap();
        assert!(app.world().get::<Armored>(bit).is_none());
        assert_eq!(
            app.world().get::<Armored>(built_bug).map(|a| a.after_build),
            Some(3.0)
        );
    }

    /// `CMeleeWeapon::FireImpl` damages its target only: the Wormbite's
    /// `areaofeffect=140` must not splash, and a target-less bite does
    /// nothing.
    #[test]
    fn melee_hits_only_its_target() {
        let mut weapons = WeaponRegistry::default();
        let bite = weapons.insert_for_test(
            "Wormbite",
            spring_tdf::WeaponDef {
                weapon_type: "Melee".into(),
                damage: spring_tdf::DamageMap {
                    default: 3200.0,
                    ..Default::default()
                },
                area_of_effect: 140.0,
                ..Default::default()
            },
        );
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let worm = unit(&mut app, UnitKind::Worm, 1, Vec3::X * 100.0, 5000.0);
        let target = unit(&mut app, UnitKind::Byte, 0, Vec3::ZERO, 10_000.0);
        let bystander = unit(&mut app, UnitKind::Bit, 0, Vec3::Z * 30.0, 1000.0);
        app.world_mut().entity_mut(target).insert(ByteOpen);
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            Some(target),
            worm,
            bite,
            Vec3::ZERO,
        ));
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(shot(None, worm, bite, Vec3::Z * 30.0));
        run_damage(&mut app);
        assert!((hp(&app, target) - 6800.0).abs() < 1e-3);
        assert!(
            (hp(&app, bystander) - 1000.0).abs() < 1e-3,
            "no melee splash"
        );
    }

    /// Why: a dying Virus's `VirusDeath` used to be drained the tick
    /// after `cleanup_dying` despawned it, so the blast had no attacker
    /// and infected nobody. Drained right behind
    /// `death_system` (`apply_death_blasts`) the corpse still credits it;
    /// the corpse itself (`Dying`, upstream `isDead`) takes nothing.
    #[test]
    fn virus_death_blast_infects_while_the_corpse_is_still_a_unit() {
        use bevy::ecs::system::RunSystemOnce;
        let mut weapons = WeaponRegistry::default();
        let blast = weapons.insert_for_test(
            "VirusDeath",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 100.0,
                    ..Default::default()
                },
                area_of_effect: 90.0,
                explosion_speed_raw: 65535.0,
                avoid_friendly: true,
                no_self_damage: true,
                ..Default::default()
            },
        );
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let virus = unit(&mut app, UnitKind::Virus, 1, Vec3::ZERO, 0.0);
        app.world_mut()
            .entity_mut(virus)
            .insert((Faction::Hacker, Dying { timer: 2.0 }));
        let enemy = unit(&mut app, UnitKind::Bit, 0, Vec3::X * 20.0, 1000.0);
        let enemy_virus = unit(&mut app, UnitKind::Virus, 0, Vec3::Z * 20.0, 1000.0);
        let friend = unit(&mut app, UnitKind::Bit, 1, Vec3::X * -20.0, 1000.0);
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            Some(virus),
            virus,
            blast,
            Vec3::ZERO,
        ));
        app.world_mut().run_system_once(apply_death_blasts).unwrap();

        let infected = app.world().get::<Infected>(enemy).expect("enemy infected");
        assert_eq!(infected.attacker_team, 1);
        assert_eq!(infected.attacker_faction, Faction::Hacker);
        assert!(hp(&app, enemy) < 1000.0);
        assert!(
            app.world().get::<Infected>(enemy_virus).is_none(),
            "Viruses never"
        );
        assert!(hp(&app, friend) < 1000.0, "allies in the blast are hit");
        assert!(
            app.world().get::<Infected>(friend).is_none(),
            "but never infected by their own side"
        );
        assert_eq!(hp(&app, virus), 0.0, "the corpse takes nothing");
    }

    /// Homebases take 10% of the SIGTERM bomb (kernel/hole/carrier.bos
    /// `HitByWeaponId`), nothing else changes for them.
    #[test]
    fn homebases_take_a_tenth_of_sigterm() {
        for kind in [UnitKind::Kernel, UnitKind::Hole, UnitKind::Carrier] {
            assert!((script_hit_multiplier(kind, true, false, false) - 0.1).abs() < 1e-6);
            assert_eq!(script_hit_multiplier(kind, false, false, false), 1.0);
        }
        assert_eq!(
            script_hit_multiplier(UnitKind::Socket, true, false, false),
            1.0
        );
    }

    fn sigterm_weapon(weapons: &mut WeaponRegistry) -> WeaponId {
        // retroweapons.tdf [SigTerm].
        weapons.insert_for_test(
            "SigTerm",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 10_000.0,
                    ..Default::default()
                },
                area_of_effect: 900.0,
                edge_effectiveness: 0.8,
                explosion_speed_raw: 65535.0,
                ..Default::default()
            },
        )
    }

    /// The SIGTERM bomb is an ordinary explosion of `[SigTerm]`: damage
    /// radius `areaofeffect / 2 = 450` (not 900), `edge .8` falloff,
    /// owned by nobody (friend and foe alike), a closed Byte takes it
    /// in full and a homebase a tenth.
    #[test]
    fn sigterm_blast_is_a_450_elmo_explosion() {
        let mut weapons = WeaponRegistry::default();
        let sigterm = sigterm_weapon(&mut weapons);
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let near = unit(&mut app, UnitKind::Bit, 0, Vec3::X * 200.0, 20_000.0);
        let outside = unit(&mut app, UnitKind::Bit, 1, Vec3::X * -600.0, 20_000.0);
        let byte = unit(&mut app, UnitKind::Byte, 1, Vec3::Z * 0.5, 20_000.0);
        let kernel = unit(&mut app, UnitKind::Kernel, 1, Vec3::Z * -0.5, 20_000.0);
        app.world_mut()
            .resource_mut::<DamageQueue>()
            .push(PendingDamage::unowned_explosion(sigterm, Vec3::ZERO));
        run_damage(&mut app);

        let falloff = splash_falloff(200.0, 450.0, 0.8);
        assert!((hp(&app, near) - (20_000.0 - 10_000.0 * falloff)).abs() < 1e-2);
        assert_eq!(hp(&app, outside), 20_000.0, "600 elmos is outside 450");
        assert!(
            (hp(&app, byte) - 10_000.0).abs() < 1e-2,
            "closed Byte, full damage"
        );
        assert!((hp(&app, kernel) - 19_000.0).abs() < 1e-2, "homebase, 10%");
    }

    /// A shell whose shooter died in flight still explodes as an
    /// ordinary `Explosion`: friend and foe in the radius are hit, and
    /// infection — which needs a living attacker upstream (a dead owner
    /// is a nil `attackerID`) — does not fire.
    #[test]
    fn orphaned_shell_splashes_everyone_without_infecting() {
        let mut weapons = WeaponRegistry::default();
        let shell = weapons.insert_for_test(
            "VirusBeam",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 100.0,
                    ..Default::default()
                },
                area_of_effect: 200.0,
                edge_effectiveness: 1.0,
                explosion_speed_raw: 65535.0,
                ..Default::default()
            },
        );
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let dead_shooter = app.world_mut().spawn_empty().id();
        app.world_mut().despawn(dead_shooter);
        let friend = unit(&mut app, UnitKind::Bit, 1, Vec3::X * 20.0, 1000.0);
        let enemy = unit(&mut app, UnitKind::Bit, 0, Vec3::X * -20.0, 1000.0);
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            None,
            dead_shooter,
            shell,
            Vec3::ZERO,
        ));
        run_damage(&mut app);
        assert_eq!(hp(&app, friend), 900.0);
        assert_eq!(hp(&app, enemy), 900.0);
        assert!(app.world().get::<Infected>(enemy).is_none());
    }

    /// Why: the port filtered `avoidfriendly=1` splash by team, so a
    /// Bug's or Dos's blast spared every ally. Engine explosions
    /// (`CGameHelper::DoExplosionDamage`) hit every unit in the radius;
    /// only `noselfdamage=1` spares the shooter itself.
    #[test]
    fn avoid_friendly_splash_still_hits_allies_but_not_a_noselfdamage_shooter() {
        let mut weapons = WeaponRegistry::default();
        let beam = weapons.insert_for_test(
            "DOS_Beam",
            spring_tdf::WeaponDef {
                damage: spring_tdf::DamageMap {
                    default: 100.0,
                    ..Default::default()
                },
                area_of_effect: 200.0,
                edge_effectiveness: 1.0,
                explosion_speed_raw: 65535.0,
                avoid_friendly: true,
                no_self_damage: true,
                ..Default::default()
            },
        );
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let shooter = unit(&mut app, UnitKind::Dos, 1, Vec3::X * 60.0, 1000.0);
        let friend = unit(&mut app, UnitKind::Bit, 1, Vec3::X * 20.0, 1000.0);
        let target = unit(&mut app, UnitKind::Bit, 0, Vec3::ZERO, 1000.0);
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            Some(target),
            shooter,
            beam,
            Vec3::ZERO,
        ));
        run_damage(&mut app);
        assert_eq!(hp(&app, target), 900.0);
        assert_eq!(hp(&app, friend), 900.0, "the ally next to the target");
        assert_eq!(hp(&app, shooter), 1000.0, "noselfdamage");
    }

    /// Every hit of half a point or more tells the AI where the team
    /// bled (KPAI_Fair `lastAllyDamage`).
    #[test]
    fn landed_hits_feed_the_ai_damage_log() {
        let mut weapons = WeaponRegistry::default();
        let gun = flat_weapon(&mut weapons, "Gun", 100.0);
        let mut app = damage_app(weapons, UnitRegistry::empty());
        let victim = unit(
            &mut app,
            UnitKind::Bit,
            2,
            Vec3::new(10.0, 0.0, 20.0),
            1000.0,
        );
        let shooter = unit(&mut app, UnitKind::Bit, 1, Vec3::X * 300.0, 1000.0);
        app.world_mut().resource_mut::<DamageQueue>().push(shot(
            Some(victim),
            shooter,
            gun,
            Vec3::ZERO,
        ));
        run_damage(&mut app);
        let log = app.world().resource::<crate::units::ai::TeamDamageLog>();
        assert_eq!(
            log.latest(2).map(|(_, p)| p),
            Some(Vec3::new(10.0, 0.0, 20.0))
        );
        assert!(log.latest(1).is_none());
        // And the victim's idle-heal clock restarted.
        assert_eq!(app.world().get::<IdleTimer>(victim).unwrap().0, 0.0);
    }
}

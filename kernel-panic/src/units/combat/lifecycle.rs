//! Unit lifecycle: stun, kamikaze trigger, death detection/teardown,
//! auto-heal.
//!
//! Each system reacts to a different terminal state: [`tick_stun`] decays
//! the [`Stunned`] marker + [`StunCharge`] pool; [`tick_kamikaze`] kills
//! proximity bombs; [`death_system`] watches for zero-HP units, fires
//! their death explosion and promotes them to [`Dying`]; [`cleanup_dying`]
//! despawns once the death anim finishes; [`auto_heal`] regenerates
//! idle units.

use bevy::ecs::lifecycle::HookContext;
use bevy::ecs::world::DeferredWorld;
use bevy::prelude::*;

use super::damage::{DamageQueue, Infected, PendingDamage, VirusSpawn, VirusSpawnQueue};
use super::{IdleTimer, StunCharge};
use crate::units::assets::animation::UnitAnimator;
use crate::units::components::{Faction, Health, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::{DeathBlast, WeaponRegistry};
use crate::units::lifecycle::bookkeeping::{IdleAutoHeal, Kamikaze};
use crate::units::lifecycle::spawning::{BuildDriven, Emerging};
use crate::units::spatial::SpatialIndex;
use crate::units::weapon_fx::{ExplosionEvent, PendingExplosions};

/// Marks a unit that has reached 0 HP and is playing its death animation.
/// The entity will be despawned once the animation finishes or the timer expires.
///
/// Inserting it is the kill — upstream `CUnit::ForcedKillUnit`'s
/// `UnitDestroyed` call-in, which fires once even though the corpse may
/// linger for its death script. Its `on_add` hook ends every
/// [`AliveOnly`] registration right there, at the command flush, in
/// whatever schedule inserted it; each marker's own `on_remove` hook
/// does the bookkeeping, which a despawn (with or without this stage)
/// runs as well. No system has to catch the insertion: a building is
/// usually despawned in the very tick it gets `Dying`.
#[derive(Component)]
#[component(storage = "SparseSet", on_add = Dying::on_add)]
pub struct Dying {
    pub timer: f32,
}

/// Registrations only a living unit holds, dropped by [`Dying`]:
/// - its structure-layer stamp (`CSolidObject::UnBlock`),
/// - the finished small-building tally (`kernelboost.lua::UnitDestroyed`)
///   and the pending one of a nanoframe,
/// - its claim on a Hex Farm tower (`SendOwnerShipChangeToUnsynced`),
/// - its place in the player's selection (`CSelectedUnitsHandler::
///   DependentDied`), so nothing that acts on the selection — orders,
///   build placement, the command panel — has to skip corpses.
///   Control groups (`CGroup::RemoveUnit`) skip dying units on recall.
///
/// The per-team unit count is not among them: upstream counts a unit
/// until it is deleted, so that marker goes with the despawn.
pub type AliveOnly = (
    crate::interaction::structures::StructureStamped,
    crate::units::lifecycle::bookkeeping::CountedSmallBuilding,
    crate::units::lifecycle::bookkeeping::UncountedSmallBuilding,
    crate::map_events::hex_farm::TowerClaim,
    crate::interaction::selection::Selected,
);

impl Dying {
    fn on_add(mut world: DeferredWorld, ctx: HookContext) {
        world
            .commands()
            .entity(ctx.entity)
            .try_remove::<AliveOnly>();
    }
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

impl SelfDestructCountdown {
    /// A fresh self-destruct order for a `kind` unit: its FBI
    /// `SelfDestructCountdown` ([`UnitRegistry::self_destruct_delay`]),
    /// or [`SELF_DESTRUCT_DELAY`] without a registry.
    pub fn ordered(kind: UnitKind, unit_registry: Option<&UnitRegistry>) -> Self {
        Self {
            remaining: unit_registry.map_or(SELF_DESTRUCT_DELAY, |r| r.self_destruct_delay(kind)),
        }
    }
}

/// The countdown of a unit without an FBI entry: the engine default
/// `selfDestructCountdown=5` armed as `2·5 + 1` slow updates.
pub const SELF_DESTRUCT_DELAY: f32 = 5.5;

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

/// Regenerate HP: FBI `AutoHeal` always, `IdleAutoHeal` once the unit
/// has rested past its `IdleTime` (`CUnit::SlowUpdate`:
/// `health += idleAutoHeal * (restTime > idleTime)`, Unit.cpp:1097).
/// "Resting" means not bleeding: `restTime` counts every frame
/// (Unit.cpp:689) and only taking damage resets it
/// (`DamageResolver::deliver`, Unit.cpp:1336 `bleeding != resting`; the
/// other reset, reclaim at Unit.cpp:2068, has no Kernel Panic
/// counterpart). Moving, aiming and shooting don't, so a Flow circling
/// unhurt, or a Bit marching across the map, heals all the way.
/// A paralyzed unit neither heals nor rests (`Update` returns before the
/// `restTime` tick, `SlowUpdate` before the heal), nor does one still
/// being built ([`Emerging`]: both return early for `beingBuilt`).
/// Units whose FBI lacks `IdleAutoHeal` (value 0) carry no
/// [`IdleAutoHeal`] marker (`bookkeeping::tag_unit_kinds`) and opt out.
///
/// A unit at 0 HP is already dead — `death_system` just hasn't seen it
/// yet (self-destruct, kamikaze, game-over `KillTeam` and AI suicides
/// zero HP directly) — and must not be healed back above zero: upstream
/// kills the moment HP reaches 0, so there is never a dead unit to
/// heal. Without the guard an idle unit regenerated `rate × dt` between
/// the zeroing and `death_system` every tick, and never died.
#[allow(clippy::type_complexity)]
pub fn auto_heal(
    time: Res<Time>,
    mut query: Query<
        (&IdleAutoHeal, &mut Health, &mut IdleTimer),
        (Without<Dying>, Without<Stunned>, Without<Emerging>),
    >,
) {
    let dt = time.delta_secs();
    for (heal, mut health, mut idle) in &mut query {
        if health.current <= 0.0 {
            continue;
        }
        idle.0 += dt;
        if health.current < health.max {
            let mut rate = heal.always;
            if idle.0 > heal.threshold {
                rate += heal.rate;
            }
            health.current = (health.current + rate * dt).min(health.max);
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
/// their proximity radius. The bomb only forces its own HP to zero:
/// `death_system` then fires its death explosion — exactly one, as
/// upstream `SlowUpdateKamikaze` → `KillUnit(.., selfDestruct=true)` →
/// `ForcedKillUnit` fires the single `SelfDestructAs` blast. Every KP
/// FBI sets `SelfDestructAs` equal to `ExplodeAs` (logic_bomb.fbi:
/// both `logic_bomb`), so the `ExplodeAs` blast `death_system` queues
/// *is* that one; queueing the weapon here as well detonated it twice.
#[allow(clippy::type_complexity)]
pub fn tick_kamikaze(
    bombs: Query<(Entity, &Kamikaze, &TeamId, &GlobalTransform), Without<Dying>>,
    mut health_q: Query<&mut Health>,
    spatial: Res<SpatialIndex>,
) {
    for (entity, bomb, team, gtf) in &bombs {
        let trigger_radius = bomb.trigger_radius;
        let trigger_sq = trigger_radius * trigger_radius;
        let self_pos = gtf.translation();
        let mut triggered = false;
        spatial.query_radius(self_pos, trigger_radius, |candidate| {
            if triggered || !candidate.hp_positive {
                return;
            }
            let enemy = candidate.team != team.0;
            if enemy && candidate.pos.distance_squared(self_pos) <= trigger_sq {
                triggered = true;
            }
        });
        if !triggered {
            continue;
        }
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
/// and gives big units their signature death boom. A unit still being
/// built ([`BuildDriven`]) dies without blast or Killed() script and is
/// despawned at once, like upstream's `beingBuilt` kill.
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
            Has<BuildDriven>,
            &crate::net::NetId,
        ),
        (Without<Dying>, Changed<Health>),
    >,
    mut animators: Query<&mut UnitAnimator>,
    mut virus_spawns: ResMut<VirusSpawnQueue>,
    mut damage_queue: ResMut<DamageQueue>,
    mut explosions: ResMut<PendingExplosions>,
    unit_registry: Res<UnitRegistry>,
    weapon_registry: Res<WeaponRegistry>,
    mut sounds: Option<ResMut<crate::audio::SoundQueue>>,
    mut commands: Commands,
) {
    // Unit-id order: the death blasts and Virus spawns queue in it
    // (`sim`).
    for (entity, unit, health, faction, gtf, infected, being_built, _) in
        query.iter().sort::<&crate::net::NetId>()
    {
        if health.current <= 0.0 {
            // If the dying unit was infected, queue a Virus spawn for the
            // attacker's team at the death location (infection.lua's
            // `UnitDestroyed` fires for nanoframes too).
            if let Some(infected) = infected {
                virus_spawns.push(VirusSpawn {
                    position: gtf.translation(),
                    faction: infected.attacker_faction,
                    team: infected.attacker_team,
                });
            }

            // `ForcedKillUnit` (Unit.cpp:492): `deathScriptFinished =
            // (reclaimed || beingBuilt)` returns before the death
            // explosion and `script->Killed()` — a half-built unit (a
            // nanoframe, a factory product still on the pad) just goes,
            // as if reclaimed. A game-over `KillTeam` must not detonate
            // an eliminated team's half-built Logic Bomb.
            if being_built {
                commands
                    .entity(entity)
                    .remove::<Infected>()
                    .insert(Dying { timer: 0.0 });
                continue;
            }

            // Drive the Killed() animation if the unit has an animator.
            if let Ok(mut animator) = animators.get_mut(entity) {
                let crate::units::assets::animation::UnitAnimator { rig, driver, .. } =
                    &mut *animator;
                driver.killed(rig, crate::units::assets::animation::AnimCtx::minimal());
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
            //
            // The resolution is cached per kind (`WeaponRegistry::
            // bind_units`, run at startup); a registry that was never
            // bound (tests) resolves it here, per death, as before.
            let pos = gtf.translation();
            let resolved;
            let blast: Option<&DeathBlast> = match weapon_registry.death_blast(unit.0) {
                Some(cached) => cached,
                None => {
                    resolved = DeathBlast::resolve(unit.0, &unit_registry, &weapon_registry);
                    resolved.as_ref()
                }
            };
            if let Some(blast) = blast {
                // `DoExplosion` -> `Explosion`: the ExplodeAs weapon's `soundhit`.
                if let Some(sounds) = sounds.as_deref_mut() {
                    sounds.explosion(weapon_registry.by_id(blast.weapon), pos);
                }
                damage_queue.push(PendingDamage {
                    target: Some(entity),
                    attacker: entity,
                    weapon: blast.weapon,
                    impact_pos: pos,
                    attacker_distance: 0.0,
                });

                explosions.events.push(ExplosionEvent {
                    pos,
                    rgb: blast.rgb.unwrap_or_else(|| faction.rgb_f32()),
                    radius: blast.radius,
                    ceg_name: blast.ceg_name.clone(),
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
    mut query: Query<(Entity, &mut Dying, Option<&UnitAnimator>)>,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut dying, animator) in &mut query {
        dying.timer -= dt;

        let anim_done = animator.is_none_or(|a| !a.driver.busy());
        if anim_done || dying.timer <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::assets::animation::{AnimRig, UnitAnim};
    use crate::units::content::definitions::UnitKind;
    use bevy::ecs::system::RunSystemOnce;

    /// A driver whose death choreography is still playing.
    #[derive(Default)]
    struct BusyDeath;
    impl UnitAnim for BusyDeath {
        fn busy(&self) -> bool {
            true
        }
    }

    fn animator(busy: bool) -> UnitAnimator {
        UnitAnimator {
            rig: AnimRig {
                piece_names: &[],
                piece_entities: Vec::new(),
                piece_base_offsets: Vec::new(),
                piece_rotations: Vec::new(),
                piece_translations: Vec::new(),
                target_rotations: Vec::new(),
                turn_speeds: Vec::new(),
                target_translations: Vec::new(),
                move_speeds: Vec::new(),
                spin_speeds: Vec::new(),
                muzzle: 0,
                move_gate: 1.0,
                outbox: Vec::new(),
                dirty: false,
            },
            created: true,
            driver: if busy {
                Box::new(BusyDeath)
            } else {
                Box::new(crate::units::assets::animation::units::NoAnim)
            },
        }
    }

    /// The kill drops the unit from the selection at once, whatever
    /// inserted `Dying` (`CSelectedUnitsHandler::DependentDied`).
    #[test]
    fn the_kill_deselects() {
        use crate::interaction::selection::Selected;
        let mut world = World::new();
        let unit = world.spawn(Selected).id();
        world.entity_mut(unit).insert(Dying { timer: 1.0 });
        world.flush();
        assert!(world.get::<Selected>(unit).is_none());
    }

    /// Why: `busy()` used to be a dormant default — every dying unit
    /// despawned on its first Cleanup tick regardless of choreography.
    /// Drivers with death choreography now hold the corpse until the
    /// window elapses (or the timeout backstop fires).
    #[test]
    fn busy_driver_delays_despawn() {
        let mut app = App::new();
        app.init_resource::<Time>();
        let busy_unit = app
            .world_mut()
            .spawn((Dying { timer: 2.0 }, animator(true)))
            .id();
        let idle_unit = app
            .world_mut()
            .spawn((Dying { timer: 2.0 }, animator(false)))
            .id();

        app.world_mut().run_system_once(cleanup_dying).unwrap();

        // The idle driver reports done → despawned this tick.
        assert!(app.world().get_entity(idle_unit).is_err());
        // The busy driver holds the unit despite timer > 0.
        assert!(app.world().get_entity(busy_unit).is_ok());

        // Once the timeout backstop expires, even a busy driver lets go.
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(2.5));
        app.world_mut().run_system_once(cleanup_dying).unwrap();
        assert!(app.world().get_entity(busy_unit).is_err());
    }

    /// The Resolve-set slice from `tick_self_destruct` to `death_system`,
    /// in schedule order.
    fn resolve_world(unit_registry: UnitRegistry, weapon_registry: WeaponRegistry) -> World {
        let mut world = World::new();
        let mut time = Time::<()>::default();
        time.advance_by(std::time::Duration::from_secs_f32(1.0 / 30.0));
        world.insert_resource(time);
        world.init_resource::<DamageQueue>();
        world.init_resource::<VirusSpawnQueue>();
        world.init_resource::<PendingExplosions>();
        world.insert_resource(unit_registry);
        world.insert_resource(weapon_registry);
        world
    }

    fn run_resolve(world: &mut World) {
        world.run_system_once(tick_self_destruct).unwrap();
        world.run_system_once(auto_heal).unwrap();
        world.run_system_once(death_system).unwrap();
    }

    /// Why: `auto_heal` runs between `tick_self_destruct` and
    /// `death_system` with no HP guard, so an idle self-healing unit
    /// whose countdown expired (Ctrl+D, AI `SuicideIfAlone`, or a
    /// game-over `KillTeam`, which zero HP the same way) was healed back
    /// above zero before `death_system` looked — every tick, forever.
    #[test]
    fn zeroed_idle_unit_dies_instead_of_healing() {
        let mut world = resolve_world(UnitRegistry::empty(), WeaponRegistry::default());
        let unit = world
            .spawn((
                UnitType(UnitKind::Bit),
                Faction::System,
                GlobalTransform::default(),
                Health::full(100.0),
                IdleAutoHeal {
                    rate: 50.0,
                    threshold: 0.0,
                    always: 0.0,
                },
                IdleTimer(10.0),
                SelfDestructCountdown { remaining: 0.0 },
            ))
            .id();
        run_resolve(&mut world);
        assert!(world.get::<Dying>(unit).is_some(), "the countdown kills it");
        assert_eq!(world.get::<Health>(unit).unwrap().current, 0.0);

        // A unit zeroed from outside (game over) dies on the next pass.
        let other = world
            .spawn((
                UnitType(UnitKind::Bit),
                Faction::System,
                GlobalTransform::default(),
                Health::full(100.0),
                IdleAutoHeal {
                    rate: 50.0,
                    threshold: 0.0,
                    always: 0.0,
                },
                IdleTimer(10.0),
            ))
            .id();
        world.get_mut::<Health>(other).unwrap().current = 0.0;
        run_resolve(&mut world);
        assert!(world.get::<Dying>(other).is_some());
    }

    /// Why: a triggered Logic Bomb queued its `logic_bomb` weapon itself
    /// and zeroed its HP, then `death_system` queued its `ExplodeAs`
    /// (also `logic_bomb`) — two blasts. Upstream `ForcedKillUnit` fires
    /// exactly one.
    #[test]
    fn kamikaze_detonates_once() {
        use crate::units::spatial::SpatialEntry;
        let weapons = WeaponRegistry::load();
        let logic_bomb = weapons.known().logic_bomb.expect("logic_bomb weapon");
        let mut world = resolve_world(UnitRegistry::load(), weapons);
        let bomb = world
            .spawn((
                UnitType(UnitKind::LogicBomb),
                Faction::Hacker,
                TeamId(0),
                GlobalTransform::default(),
                Health::full(100.0),
                Kamikaze {
                    trigger_radius: 64.0,
                },
            ))
            .id();
        let enemy = world.spawn_empty().id();
        let mut spatial = SpatialIndex::default();
        spatial.insert_for_test(SpatialEntry {
            entity: enemy,
            pos: Vec3::new(30.0, 0.0, 0.0),
            hit_radius: 8.0,
            mid_y: 0.0,
            team: 1,
            kind: UnitKind::Bit,
            hp_positive: true,
            is_flying: false,
            cloaked: false,
            detected_by: 0,
        });
        world.insert_resource(spatial);

        world.run_system_once(tick_kamikaze).unwrap();
        assert_eq!(world.get::<Health>(bomb).unwrap().current, 0.0);
        world.run_system_once(death_system).unwrap();
        let blasts: Vec<_> = world
            .resource::<DamageQueue>()
            .iter_snapshot_for_test()
            .map(|d| d.weapon)
            .collect();
        assert_eq!(blasts, vec![logic_bomb], "exactly one logic_bomb blast");
        assert!(world.get::<Dying>(bomb).is_some());
    }

    /// Why: a killed nanoframe fired its `ExplodeAs` — a game-over
    /// `KillTeam` detonated an eliminated team's half-built Logic Bomb.
    /// `ForcedKillUnit` returns before the explosion and `Killed()` for
    /// a `beingBuilt` unit (Unit.cpp:492); an infection still spawns its
    /// Virus (`UnitDestroyed` fires first).
    #[test]
    fn killed_nanoframe_neither_explodes_nor_lingers() {
        let weapons = WeaponRegistry::load();
        let mut world = resolve_world(UnitRegistry::load(), weapons);
        let frame = world
            .spawn((
                UnitType(UnitKind::LogicBomb),
                Faction::Hacker,
                TeamId(0),
                GlobalTransform::default(),
                Health::full(100.0),
                BuildDriven::default(),
                Infected {
                    timer: 3.0,
                    attacker_faction: Faction::System,
                    attacker_team: 1,
                },
            ))
            .id();
        world.get_mut::<Health>(frame).unwrap().current = 0.0;
        world.run_system_once(death_system).unwrap();
        assert!(world.resource::<DamageQueue>().is_empty(), "no blast");
        assert!(world.resource::<PendingExplosions>().events.is_empty());
        assert!(!world.resource::<VirusSpawnQueue>().is_empty());
        assert!(world.get::<Dying>(frame).is_some());
        world.run_system_once(cleanup_dying).unwrap();
        assert!(world.get_entity(frame).is_err(), "gone at once");
    }

    fn healer(world: &mut World, hp: f32, rested: f32) -> Entity {
        let mut health = Health::full(100.0);
        health.current = hp;
        world
            .spawn((
                health,
                IdleAutoHeal {
                    rate: 30.0,
                    threshold: 20.0,
                    always: 0.0,
                },
                IdleTimer(rested),
            ))
            .id()
    }

    /// Why: `auto_heal` restarted the rest clock on every move order,
    /// path or aim target, so a moving Flow or a marching Bit never
    /// idle-healed. Upstream `restTime` ticks every frame and only
    /// damage resets it (Unit.cpp:689, 1336); the heal needs
    /// `restTime > idleTime` (Unit.cpp:1097). Paralysis and being built
    /// freeze both.
    #[test]
    fn idle_heal_counts_only_time_since_damage() {
        let mut world = resolve_world(UnitRegistry::empty(), WeaponRegistry::default());
        let marching = healer(&mut world, 50.0, 25.0);
        world.entity_mut(marching).insert((
            crate::interaction::movement::MoveTarget(Vec3::X * 500.0),
            super::super::AimTarget {
                pos: Vec3::X * 100.0,
                launch: super::super::AimLaunch::Direct,
            },
        ));
        let fresh = healer(&mut world, 50.0, 0.0);
        let stunned = healer(&mut world, 50.0, 25.0);
        world.entity_mut(stunned).insert(Stunned { remaining: 5.0 });
        world.run_system_once(auto_heal).unwrap();

        assert!((world.get::<Health>(marching).unwrap().current - 51.0).abs() < 1e-3);
        assert!(world.get::<IdleTimer>(marching).unwrap().0 > 25.0);
        assert_eq!(world.get::<Health>(fresh).unwrap().current, 50.0);
        assert!(world.get::<IdleTimer>(fresh).unwrap().0 > 0.0, "rests on");
        assert_eq!(world.get::<Health>(stunned).unwrap().current, 50.0);
        assert_eq!(world.get::<IdleTimer>(stunned).unwrap().0, 25.0);
    }
}

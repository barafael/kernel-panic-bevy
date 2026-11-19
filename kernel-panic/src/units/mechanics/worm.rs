//! Worm ambush mechanics: AutoHold, surfacing (decloak-to-bite), and the
//! Wormsplash detonation that makes Worm kills spawn Viruses.
//!
//! Upstream splits this across three places, all ported here:
//!
//! - `LuaRules/Gadgets/autohold.lua` — the per-unit AutoHold toggle,
//!   initialised ON for human teams and OFF for AI (`init = {human=true,
//!   bot=false}`), pushed into the script via `AutoHold(state)`.
//! - `scripts/worm.bos` — `AutoHold` / `Create` set `STANDINGFIREORDERS`
//!   to 0 (hold fire) while cloaked with autohold on, 2 (fire at will)
//!   otherwise. `AimWeapon1` decloaks (`CLOAKED=FALSE`), halves
//!   `MAX_SPEED` and raises the head; `ResetAim` runs 2000 ms after the
//!   last aim, lowers the head, restores the speed and re-cloaks
//!   (`WANT_CLOAK`). `FireWeapon1` detonates Weapon2 (Wormsplash) with
//!   `emit-sfx 4097 from head` and `from end`.
//! - `weapons/corruptionweapons.tdf` `[Wormsplash]` — AoE 210, 800
//!   damage (≈0 vs subterranean), edge effectiveness 1; listed in
//!   `infection.lua` with a 200-frame infection window.
//!
//! The hold-fire gate itself lives in `combat_system` (it reads
//! [`AutoHold`] + `Cloaked`); this module owns the state transitions.

use bevy::prelude::*;

use crate::net::NetId;
use crate::sim::{SimTick, cob_sleep_frames, countdown_secs, slow_update_due};
use crate::units::ai::AiSeats;
use crate::units::combat::{AimTarget, DamageQueue, Dying, PendingDamage};
use crate::units::components::{TeamId, UnitStats, UnitType};
use crate::units::content::weapons::WeaponKind;
use crate::units::lifecycle::bookkeeping::{Detector, SeismicDetector};
use crate::units::mechanics::cloak::Cloaked;

/// Upstream `ResetAim`: `sleep 2000` (61 frames), then `move head to
/// y-axis [-16] speed [12]` + `wait-for-move` (16 elmos at 0.4 a frame:
/// 40 frames) before `MAX_SPEED` is restored and `WANT_CLOAK` set.
/// Seconds after the last aim request until the worm wants to burrow
/// again; the cloak itself waits for its slow update ([`PendingCloak`]).
pub const RESET_AIM_DELAY: f32 = countdown_secs(cob_sleep_frames(2000) + 40);

/// Upstream `AimWeapon1`: `set MAX_SPEED to maxspeed/2` while surfaced.
const SURFACED_SPEED_FACTOR: f32 = 0.5;

/// The AutoHold toggle (upstream `CustomToggle1=AutoHold`). While `true`
/// and the unit is cloaked, it holds fire: no auto-acquisition, only
/// explicit attack orders make it surface. While `false` it fires at
/// will even from under cover (and surfaces to bite).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoHold(pub bool);

impl AutoHold {
    /// autohold.lua's `init` (`AddAutoHoldUnit`): ON for a human
    /// player's team, OFF for an AI team (`Spring.GetTeamInfo`'s
    /// `isAiTeam`, synced: the seat, never who is watching).
    pub fn initial(ai_team: bool) -> Self {
        Self(!ai_team)
    }
}

/// worm.bos's fire state: `STANDINGFIREORDERS` 0 (hold fire) or 2
/// (fire at will), as the script sets it. Three places write it:
///
/// - `AutoHold(state)` (autohold.lua, on every toggle): hold iff the
///   toggle is on and the Worm is cloaked, at once;
/// - `AimWeapon1`: fire at will, at once, with the toggle on;
/// - `Create()`'s loop, which samples `CLOAKED` every `sleep 300` (10
///   frames) from the moment it sets `WANT_CLOAK`, and only when the
///   sample differs from the last one sets hold to match (toggle on).
///
/// So a Worm whose cloak comes on holds fire up to 10 frames later.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WormFireState {
    /// `STANDINGFIREORDERS == 0`.
    pub hold: bool,
    /// The loop's `cloak` variable: `CLOAKED` at its last sample.
    seen_cloaked: bool,
    /// The toggle as the script last heard it (`AutoHold(state)`).
    autohold: bool,
    /// The loop's next sample; `None` until `Create()` reaches it.
    next_poll: Option<u64>,
}

impl WormFireState {
    /// A new Worm: AutoHold(state) runs at creation, uncloaked: fire at
    /// will.
    pub fn new(autohold: bool) -> Self {
        Self {
            autohold,
            ..Self::default()
        }
    }

    /// A Worm whose script has caught up with its toggle and cloak.
    #[cfg(test)]
    pub fn settled(autohold: bool, cloaked: bool) -> Self {
        Self {
            hold: autohold && cloaked,
            seen_cloaked: cloaked,
            autohold,
            next_poll: None,
        }
    }

    /// `AimWeapon1`: `if(autoholdstate) set STANDINGFIREORDERS to 2`.
    fn aim(&mut self) {
        if self.autohold {
            self.hold = false;
        }
    }
}

/// worm.bos `Create()` loop period: `sleep 300`.
const FIRE_STATE_POLL: u64 = cob_sleep_frames(300) as u64;

/// Run the script side of [`WormFireState`]: an AutoHold toggle the
/// script hasn't heard yet (`AutoHold(state)`) and the `Create()` loop's
/// `CLOAKED` samples.
pub fn tick_worm_fire_state(
    tick: Res<SimTick>,
    mut worms: Query<(&mut WormFireState, &AutoHold, Has<Cloaked>), Without<Dying>>,
) {
    let frame = tick.frame();
    for (mut state, autohold, cloaked) in &mut worms {
        if state.autohold != autohold.0 {
            state.autohold = autohold.0;
            state.hold = autohold.0 && cloaked;
        }
        if state.next_poll.is_some_and(|at| at <= frame) {
            state.next_poll = Some(frame + FIRE_STATE_POLL);
            if state.seen_cloaked != cloaked {
                state.seen_cloaked = cloaked;
                if state.autohold {
                    state.hold = cloaked;
                }
            }
        }
    }
}

/// Present while a Worm is surfaced (decloaked by an aim request).
/// `base_speed` is the pre-surface `MAX_SPEED` restored on burrow.
#[derive(Component, Clone, Copy, Debug)]
pub struct Surfaced {
    pub reset_in: f32,
    pub base_speed: f32,
}

/// A unit that cloaks once it is finished and `delay` seconds have run:
/// `CUnit` never cloaks a unit that is still being built
/// (`Unit.cpp:1049-1055`: `isCloaked = wantCloak` only when not
/// `beingBuilt`), so a Logic Bomb (`Init_Cloaked`) cloaks the moment it
/// is done, and a Worm — which has none — when worm.bos `Create` gets to
/// `set WANT_CLOAK`.
#[derive(Component, Clone, Copy, Debug)]
pub struct CloakWhenBuilt {
    pub delay: f32,
}

/// worm.bos `Create` after the build: `sleep 5000` (a `sleep` lasts
/// `5000/33 + 1` frames), the head retracts for `sleep 1333` (41
/// frames), then `set WANT_CLOAK`: 193 frames in all.
pub const WORM_CLOAK_DELAY: f32 = (5000 / 33 + 1 + 1333 / 33 + 1) as f32 / 30.0;

/// worm.bos `Create` after the build: `sleep 5000` (152 frames), then
/// `set ACTIVATION to 1`.
pub const WORM_ACTIVATION_DELAY: f32 = (5000 / 33 + 1) as f32 / 30.0;

/// A unit whose radar and seismic sensors are still off: the engine
/// blocks every sensor of a deactivated unit (`LosHandler.cpp:182`), and
/// the Worm's script activates 5 s after it is finished. Turns into
/// [`Detector`] / [`SeismicDetector`] then.
#[derive(Component, Clone, Copy, Debug)]
pub struct SensorsPending {
    pub delay: f32,
    pub radar: f32,
    pub seismic: f32,
}

/// Switch the sensors on for units whose activation delay ran out.
#[allow(clippy::type_complexity)]
pub fn activate_pending_sensors(
    time: Res<Time>,
    mut waiting: Query<
        (Entity, &mut SensorsPending),
        (
            Without<crate::units::lifecycle::spawning::Emerging>,
            Without<Dying>,
        ),
    >,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut wait) in &mut waiting {
        wait.delay -= dt;
        if wait.delay > 0.0 {
            continue;
        }
        let mut unit = commands.entity(entity);
        unit.try_remove::<SensorsPending>();
        if wait.radar > 0.0 {
            unit.try_insert(Detector { radius: wait.radar });
        }
        if wait.seismic > 0.0 {
            unit.try_insert(SeismicDetector {
                radius: wait.seismic,
            });
        }
    }
}

/// A script changed the unit's `wantCloak` (`set WANT_CLOAK`, `set
/// CLOAKED`, both only write `wantCloak`: `UnitScript.cpp:1673-1679`) and
/// the unit's [`Cloaked`] state has yet to follow. The engine switches
/// `isCloaked` in one place only, `CUnit::SlowUpdateCloak`, on the unit's
/// slow update (stunned or not; never while it is being built), and
/// without a Lua `AllowUnitCloak` (Kernel Panic has none) nothing stops
/// it. So a Worm stays hidden for up to 14 frames after it starts to
/// surface, and burrows up to 14 frames after its `ResetAim`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[component(storage = "SparseSet")]
pub struct PendingCloak(pub bool);

/// `CUnit::SlowUpdateCloak`: apply each [`PendingCloak`] on its unit's
/// slow update.
#[allow(clippy::type_complexity)]
pub fn apply_pending_cloak(
    tick: Res<SimTick>,
    units: Query<
        (Entity, &NetId, &PendingCloak, Has<Cloaked>),
        (
            Without<crate::units::lifecycle::spawning::Emerging>,
            Without<Dying>,
        ),
    >,
    mut commands: Commands,
) {
    let frame = tick.frame();
    for (entity, id, pending, cloaked) in &units {
        if !slow_update_due(frame, id.0) {
            continue;
        }
        let mut unit = commands.entity(entity);
        unit.remove::<PendingCloak>();
        match (pending.0, cloaked) {
            (true, false) => {
                unit.insert(Cloaked);
            }
            (false, true) => {
                unit.remove::<Cloaked>();
            }
            _ => {}
        }
    }
}

/// Set the finished units that were waiting for it to want their cloak
/// (it comes on at their next slow update, [`PendingCloak`]).
#[allow(clippy::type_complexity)]
pub fn promote_cloak_when_built(
    time: Res<Time>,
    tick: Res<SimTick>,
    mut waiting: Query<
        (Entity, &mut CloakWhenBuilt, Option<&mut WormFireState>),
        (
            Without<crate::units::lifecycle::spawning::Emerging>,
            Without<Dying>,
        ),
    >,
    mut commands: Commands,
) {
    let dt = time.delta_secs();
    for (entity, mut wait, fire_state) in &mut waiting {
        wait.delay -= dt;
        if wait.delay <= 0.0 {
            // worm.bos `Create()` enters its `CLOAKED` loop here, with
            // `cloak` sampled uncloaked; its first look is at once.
            if let Some(mut fire_state) = fire_state {
                fire_state.next_poll = Some(tick.frame());
            }
            commands
                .entity(entity)
                .remove::<CloakWhenBuilt>()
                .try_insert(PendingCloak(true));
        }
    }
}

/// The weapon a Worm's bite detonates (FBI `Weapon2=Wormsplash`),
/// resolved at spawn so the fire path doesn't hash names per bite.
#[derive(Component, Clone, Copy, Debug)]
pub struct WormSplash(pub WeaponKind);

/// Give freshly spawned AutoHold-capable units their initial toggle
/// state from their team (human vs AI) — unless their factory already
/// passed its own on (a Hole's Worms take the Hole's) — and a Worm its
/// script's fire state, which starts from that toggle.
// Three components + one filter on the fresh-spawn pass.
#[allow(clippy::type_complexity)]
pub fn init_autohold(
    mut commands: Commands,
    ai_seats: Res<AiSeats>,
    new_units: Query<(Entity, &UnitType, &TeamId, Option<&AutoHold>), Added<UnitType>>,
) {
    for (entity, unit_type, team, inherited) in &new_units {
        if !unit_type.0.has_autohold() {
            continue;
        }
        let autohold = inherited
            .copied()
            .unwrap_or_else(|| AutoHold::initial(ai_seats.contains(team.0)));
        let mut unit = commands.entity(entity);
        if inherited.is_none() {
            unit.insert(autohold);
        }
        if unit_type.0 == crate::units::content::definitions::UnitKind::Worm {
            unit.insert(WormFireState::new(autohold.0));
        }
    }
}

/// Surface / burrow state machine. An aim request ([`AimTarget`], stamped
/// by `combat_system` / `attack_ground_system` once a target is picked)
/// is the port's `AimWeapon1`: give up the cloak (`CLOAKED=FALSE`, which
/// takes effect on the slow update, [`PendingCloak`]), lift the hold
/// fire, halve the speed, and (re)arm the reset countdown. With no aim
/// request for [`RESET_AIM_DELAY`] the worm restores its speed and wants
/// its cloak back (`ResetAim`). The head choreography runs in the worm
/// animation driver off the same aim calls.
#[allow(clippy::type_complexity)]
pub fn tick_worm_surfacing(
    time: Res<Time>,
    mut commands: Commands,
    mut worms: Query<
        (
            Entity,
            &mut UnitStats,
            Option<&mut Surfaced>,
            Has<AimTarget>,
            Has<Cloaked>,
            Option<&PendingCloak>,
            Option<&mut WormFireState>,
        ),
        // The Hole carries the toggle for its Worms' sake, not a cycle.
        (
            With<AutoHold>,
            Without<Dying>,
            Without<crate::units::components::Homebase>,
        ),
    >,
) {
    let dt = time.delta_secs();
    for (entity, mut stats, surfaced, aiming, cloaked, pending, fire_state) in &mut worms {
        if aiming && let Some(mut fire_state) = fire_state {
            fire_state.aim();
        }
        // `set CLOAKED to FALSE; set WANT_CLOAK to FALSE`.
        let uncloak = cloaked || pending.is_some_and(|p| p.0);
        match (surfaced, aiming) {
            (Some(mut s), true) => {
                s.reset_in = RESET_AIM_DELAY;
                if uncloak {
                    commands.entity(entity).insert(PendingCloak(false));
                }
            }
            (None, true) => {
                let base_speed = stats.speed;
                stats.speed = base_speed * SURFACED_SPEED_FACTOR;
                let mut e = commands.entity(entity);
                e.insert(Surfaced {
                    reset_in: RESET_AIM_DELAY,
                    base_speed,
                });
                if uncloak {
                    e.insert(PendingCloak(false));
                }
            }
            (Some(mut s), false) => {
                s.reset_in -= dt;
                if s.reset_in <= 0.0 {
                    stats.speed = s.base_speed;
                    commands
                        .entity(entity)
                        .remove::<Surfaced>()
                        .insert(PendingCloak(true));
                }
            }
            (None, false) => {}
        }
    }
}

/// Queue the Wormsplash detonations for one bite: upstream `FireWeapon1`
/// emits Weapon2 from both the telescoped `head` (the bite point) and
/// the `end` tail segment (≈ the worm's own origin). Detonations, not
/// melee fire, so `apply_damage` runs the splash + armor + infection
/// path although Wormsplash is a `Melee` weapon.
pub fn queue_wormsplash(
    damage_queue: &mut DamageQueue,
    attacker: Entity,
    splash: WeaponKind,
    bite_pos: Vec3,
    tail_pos: Vec3,
) {
    for impact_pos in [bite_pos, tail_pos] {
        damage_queue.push(PendingDamage::detonation(attacker, splash, impact_pos));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn stats(speed: f32) -> UnitStats {
        UnitStats {
            radius: 12.0,
            hit_radius: 20.0,
            mid_y: 0.0,
            speed,
            acc_rate: 0.0067,
            dec_rate: 0.01,
            turn_rate: 3.0,
            can_fly: false,
        }
    }

    /// worm.bos `ACTIVATION`: the radar and the seismic sense switch on
    /// only once the 5 s after the build ran out.
    #[test]
    fn sensors_wake_five_seconds_after_the_build() {
        let mut app = App::new();
        app.init_resource::<Time>();
        let pending = |delay| SensorsPending {
            delay,
            radar: 256.0,
            seismic: 1024.0,
        };
        let early = app.world_mut().spawn(pending(2.0)).id();
        let due = app.world_mut().spawn(pending(0.0)).id();
        app.world_mut()
            .run_system_once(activate_pending_sensors)
            .unwrap();
        assert!(app.world().get::<SensorsPending>(early).is_some());
        assert!(app.world().get::<Detector>(early).is_none());
        assert!(app.world().get::<SensorsPending>(due).is_none());
        assert_eq!(
            app.world().get::<Detector>(due).map(|d| d.radius),
            Some(256.0)
        );
        assert_eq!(
            app.world().get::<SeismicDetector>(due).map(|d| d.radius),
            Some(1024.0)
        );
    }

    #[test]
    fn autohold_defaults_on_for_human_off_for_ai() {
        assert_eq!(AutoHold::initial(false), AutoHold(true));
        assert_eq!(AutoHold::initial(true), AutoHold(false));
    }

    /// worm.bos: hold fire follows `CLOAKED` only at the `Create()`
    /// loop's samples, 10 frames apart; `AutoHold(state)` and
    /// `AimWeapon1` act at once.
    #[test]
    fn the_hold_fire_follows_the_scripts_samples() {
        let mut world = World::new();
        let mut state = WormFireState::new(true);
        state.next_poll = Some(5);
        let worm = world.spawn((state, AutoHold(true), Cloaked)).id();
        let system = world.register_system(tick_worm_fire_state);
        let hold = |world: &mut World, frame| {
            world.insert_resource(SimTick(frame));
            world.run_system(system).unwrap();
            world.get::<WormFireState>(worm).unwrap().hold
        };
        assert!(!hold(&mut world, 4), "cloaked, but no sample yet");
        assert!(hold(&mut world, 5));
        // Off and on again: `AutoHold(state)` sets it at once.
        world.entity_mut(worm).insert(AutoHold(false));
        assert!(!hold(&mut world, 6));
        world.entity_mut(worm).insert(AutoHold(true));
        assert!(hold(&mut world, 7));
        // Surfacing: `AimWeapon1` lifts it; the decloak is seen at the
        // next sample, frame 15, and the re-cloak at the one after it.
        world.get_mut::<WormFireState>(worm).unwrap().aim();
        world.entity_mut(worm).remove::<Cloaked>();
        assert!(!hold(&mut world, 15));
        world.entity_mut(worm).insert(Cloaked);
        assert!(!hold(&mut world, 24));
        assert!(hold(&mut world, 25));
    }

    /// `CUnit::SlowUpdateCloak`: a wanted cloak (or uncloak) waits for
    /// the unit's slow update, `(frame + id) % 15 == 0`.
    #[test]
    fn a_cloak_change_waits_for_the_slow_update() {
        let mut world = World::new();
        let worm = world.spawn((NetId(4), PendingCloak(true))).id();
        let system = world.register_system(apply_pending_cloak);
        for frame in 1..=10 {
            world.insert_resource(SimTick(frame));
            world.run_system(system).unwrap();
            assert!(world.get::<Cloaked>(worm).is_none(), "frame {frame}");
        }
        world.insert_resource(SimTick(11));
        world.run_system(system).unwrap();
        assert!(world.get::<Cloaked>(worm).is_some());
        assert!(world.get::<PendingCloak>(worm).is_none());

        world.entity_mut(worm).insert(PendingCloak(false));
        world.insert_resource(SimTick(20));
        world.run_system(system).unwrap();
        assert!(world.get::<Cloaked>(worm).is_some());
        world.insert_resource(SimTick(26));
        world.run_system(system).unwrap();
        assert!(world.get::<Cloaked>(worm).is_none());
    }

    /// AimWeapon1 decloaks + halves speed; ResetAim restores both once
    /// the aim requests stop for `RESET_AIM_DELAY`.
    #[test]
    fn aiming_surfaces_and_idle_reburrows() {
        let mut app = App::new();
        app.init_resource::<Time>();
        let worm = app
            .world_mut()
            .spawn((
                AutoHold(true),
                Cloaked,
                stats(63.0),
                AimTarget {
                    pos: Vec3::X * 100.0,
                    launch: crate::units::combat::AimLaunch::Direct,
                },
            ))
            .id();

        app.world_mut()
            .run_system_once(tick_worm_surfacing)
            .unwrap();
        // Still hidden: the cloak goes on its slow update.
        assert!(app.world().get::<Cloaked>(worm).is_some());
        assert_eq!(
            app.world().get::<PendingCloak>(worm),
            Some(&PendingCloak(false))
        );
        assert!(app.world().get::<Surfaced>(worm).is_some());
        assert!((app.world().get::<UnitStats>(worm).unwrap().speed - 31.5).abs() < 1e-4);

        // Aim ends; burn the reset countdown.
        app.world_mut().entity_mut(worm).remove::<AimTarget>();
        app.world_mut().get_mut::<Surfaced>(worm).unwrap().reset_in = 0.0;
        app.world_mut()
            .run_system_once(tick_worm_surfacing)
            .unwrap();
        assert_eq!(
            app.world().get::<PendingCloak>(worm),
            Some(&PendingCloak(true))
        );
        assert!(app.world().get::<Surfaced>(worm).is_none());
        assert!((app.world().get::<UnitStats>(worm).unwrap().speed - 63.0).abs() < 1e-4);
    }
}

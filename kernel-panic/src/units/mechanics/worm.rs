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

use crate::units::ai::AiSeats;
use crate::units::combat::{AimTarget, DamageQueue, Dying, PendingDamage};
use crate::units::components::{TeamId, UnitStats, UnitType};
use crate::units::content::weapons::WeaponKind;
use crate::units::lifecycle::bookkeeping::{Detector, SeismicDetector};
use crate::units::mechanics::cloak::Cloaked;

/// Upstream `ResetAim`: `sleep 2000`, then `move head to y-axis [-16]
/// speed [12]` + `wait-for-move` (16/12 s) before `MAX_SPEED` is
/// restored and `WANT_CLOAK` set. Seconds after the last aim request
/// until the worm burrows again.
pub const RESET_AIM_DELAY: f32 = 2.0 + 16.0 / 12.0;

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

/// Cloak the finished units that were waiting for it.
#[allow(clippy::type_complexity)]
pub fn promote_cloak_when_built(
    time: Res<Time>,
    mut waiting: Query<
        (Entity, &mut CloakWhenBuilt),
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
        if wait.delay <= 0.0 {
            commands
                .entity(entity)
                .remove::<CloakWhenBuilt>()
                .try_insert(Cloaked);
        }
    }
}

/// The weapon a Worm's bite detonates (FBI `Weapon2=Wormsplash`),
/// resolved at spawn so the fire path doesn't hash names per bite.
#[derive(Component, Clone, Copy, Debug)]
pub struct WormSplash(pub WeaponKind);

/// Give freshly spawned AutoHold-capable units their initial toggle
/// state from their team (human vs AI).
// Three components + one filter on the fresh-spawn pass.
#[allow(clippy::type_complexity)]
pub fn init_autohold(
    mut commands: Commands,
    ai_seats: Res<AiSeats>,
    new_units: Query<(Entity, &UnitType, &TeamId), (Added<UnitType>, Without<AutoHold>)>,
) {
    for (entity, unit_type, team) in &new_units {
        if unit_type.0.has_autohold() {
            commands
                .entity(entity)
                .insert(AutoHold::initial(ai_seats.contains(team.0)));
        }
    }
}

/// Surface / burrow state machine. An aim request ([`AimTarget`], stamped
/// by `combat_system` / `attack_ground_system` once a target is picked)
/// is the port's `AimWeapon1`: decloak, halve the speed, and (re)arm the
/// reset countdown. With no aim request for [`RESET_AIM_DELAY`] the worm
/// restores its speed and re-cloaks (`ResetAim`). The head choreography
/// runs in the worm animation driver off the same aim calls.
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
    for (entity, mut stats, surfaced, aiming, cloaked) in &mut worms {
        match (surfaced, aiming) {
            (Some(mut s), true) => {
                s.reset_in = RESET_AIM_DELAY;
                if cloaked {
                    commands.entity(entity).remove::<Cloaked>();
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
                if cloaked {
                    e.remove::<Cloaked>();
                }
            }
            (Some(mut s), false) => {
                s.reset_in -= dt;
                if s.reset_in <= 0.0 {
                    stats.speed = s.base_speed;
                    commands.entity(entity).remove::<Surfaced>().insert(Cloaked);
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
        assert_eq!(app.world().get::<Detector>(due).map(|d| d.radius), Some(256.0));
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
        assert!(app.world().get::<Cloaked>(worm).is_none());
        assert!(app.world().get::<Surfaced>(worm).is_some());
        assert!((app.world().get::<UnitStats>(worm).unwrap().speed - 31.5).abs() < 1e-4);

        // Aim ends; burn the reset countdown.
        app.world_mut().entity_mut(worm).remove::<AimTarget>();
        app.world_mut().get_mut::<Surfaced>(worm).unwrap().reset_in = 0.0;
        app.world_mut()
            .run_system_once(tick_worm_surfacing)
            .unwrap();
        assert!(app.world().get::<Cloaked>(worm).is_some());
        assert!(app.world().get::<Surfaced>(worm).is_none());
        assert!((app.world().get::<UnitStats>(worm).unwrap().speed - 63.0).abs() < 1e-4);
    }
}

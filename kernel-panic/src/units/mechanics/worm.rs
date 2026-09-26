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

use crate::units::combat::{AimTarget, DamageQueue, Dying, PendingDamage};
use crate::units::components::{TeamId, UnitStats, UnitType};
use crate::units::content::weapons::WeaponId;
use crate::units::mechanics::cloak::Cloaked;
use crate::units::player::LocalTeam;

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
    /// autohold.lua's `init`: ON for the human player's team, OFF for
    /// AI-driven teams.
    pub fn initial(team: u8, local: u8) -> Self {
        Self(team == local)
    }
}

/// Present while a Worm is surfaced (decloaked by an aim request).
/// `base_speed` is the pre-surface `MAX_SPEED` restored on burrow.
#[derive(Component, Clone, Copy, Debug)]
pub struct Surfaced {
    pub reset_in: f32,
    pub base_speed: f32,
}

/// The weapon a Worm's bite detonates (FBI `Weapon2=Wormsplash`),
/// interned at spawn so the fire path doesn't hash names per bite.
#[derive(Component, Clone, Copy, Debug)]
pub struct WormSplash(pub WeaponId);

/// Give freshly spawned AutoHold-capable units their initial toggle
/// state from their team (human vs AI).
pub fn init_autohold(
    mut commands: Commands,
    local: Res<LocalTeam>,
    new_units: Query<(Entity, &UnitType, &TeamId), (Added<UnitType>, Without<AutoHold>)>,
) {
    for (entity, unit_type, team) in &new_units {
        if unit_type.0.has_autohold() {
            commands
                .entity(entity)
                .insert(AutoHold::initial(team.0, local.0));
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
        (With<AutoHold>, Without<Dying>),
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
/// the `end` tail segment (≈ the worm's own origin). Target-less AoE
/// hits so `apply_damage` runs the splash + armor + infection path.
pub fn queue_wormsplash(
    damage_queue: &mut DamageQueue,
    attacker: Entity,
    splash: WeaponId,
    bite_pos: Vec3,
    tail_pos: Vec3,
) {
    for impact_pos in [bite_pos, tail_pos] {
        damage_queue.push(PendingDamage {
            target: None,
            attacker,
            weapon: splash,
            impact_pos,
            attacker_distance: 0.0,
        });
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
            speed,
            accel: 6.0,
            brake: 9.0,
            turn_rate: 3.0,
            can_fly: false,
            no_chase_vtol: true,
        }
    }

    #[test]
    fn autohold_defaults_on_for_human_off_for_ai() {
        assert_eq!(AutoHold::initial(0, 0), AutoHold(true));
        assert_eq!(AutoHold::initial(1, 0), AutoHold(false));
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
                    arc_height: 0.0,
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

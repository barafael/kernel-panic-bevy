//! Poisonous water: the original's `WATER.WaterDamage` on the two maps
//! that set it (`DigitalDivide_PT2` 50, `Speed_Balls_16_Way` 100).
//!
//! The engine reads the value per second and keeps it per slow update
//! (`MapInfo.cpp:236`: `damage * UNIT_SLOWUPDATE_RATE / GAME_SPEED`), so
//! every 15 frames a unit standing in water (`IsInWater`: its base at or
//! below the water level, 0, `SolidObject.cpp:125`) takes half the
//! per-second figure through `DoDamage` with no attacker and no weapon,
//! every armour class alike (`CUnit::DoWaterDamage`, `Unit.cpp:1210`,
//! called first thing in `SlowUpdate`).
//!
//! The figures live here, keyed by map name, rather than in the
//! `.kpmap`: the format has no water section, and adding one would
//! rewrite every map file and change every map's hash (and with it every
//! recorded replay), for a setting two frozen maps carry.

use bevy::prelude::*;

use crate::interaction::movement::GroundLift;
use crate::net::NetId;
use crate::sim::{GAME_SPEED, SLOW_UPDATE_RATE, SimTick, slow_update_due};
use crate::units::combat::{DelayedDamage, Dying, Hit};
use crate::units::components::Health;

/// `WATER.WaterDamage` (per second) of the shipped maps that set it.
const WATER_DAMAGE: &[(&str, f32)] = &[("DigitalDivide_PT2", 50.0), ("Speed_Balls_16_Way", 100.0)];

/// The water level (`CGround::GetWaterLevel`): the engine's default,
/// which neither map changes.
const WATER_LEVEL: f32 = 0.0;

/// The loaded map's water damage per slow update.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct WaterDamage(pub f32);

impl WaterDamage {
    /// The damage map `name`'s water deals, if any.
    pub fn for_map(name: &str) -> Option<Self> {
        WATER_DAMAGE
            .iter()
            .find(|(map, _)| map.eq_ignore_ascii_case(name))
            .map(|&(_, per_second)| Self(per_second * SLOW_UPDATE_RATE as f32 / GAME_SPEED))
    }
}

/// Whether a unit whose root is at `root_y`, lifted `lift` above its
/// base, stands in the water.
fn in_water(root_y: f32, lift: f32) -> bool {
    root_y - lift <= WATER_LEVEL
}

/// What the water reads of a unit: who, and how high its base is.
type Bather = (
    Entity,
    &'static NetId,
    &'static Transform,
    Option<&'static GroundLift>,
);

/// Queue this frame's water damage: every living unit in the water
/// whose slow update is due takes a hit with no attacker, delivered by
/// the damage drain in `Resolve` like any other `DoDamage`.
pub fn water_damage(
    water: Res<WaterDamage>,
    tick: Res<SimTick>,
    units: Query<Bather, (With<Health>, Without<Dying>)>,
    mut delayed: ResMut<DelayedDamage>,
) {
    let frame = tick.frame();
    for (entity, id, tf, lift) in &units {
        if !slow_update_due(frame, id.0) || !in_water(tf.translation.y, lift.map_or(0.0, |l| l.0)) {
            continue;
        }
        delayed.push_now(Hit {
            target: entity,
            attacker: None,
            damage: water.0,
            weapon: None,
            paralyze_time: None,
            impulse: Vec3::ZERO,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_the_per_second_figure_every_slow_update() {
        assert_eq!(
            WaterDamage::for_map("DigitalDivide_PT2"),
            Some(WaterDamage(25.0))
        );
        assert_eq!(
            WaterDamage::for_map("speed_balls_16_way"),
            Some(WaterDamage(50.0))
        );
        assert_eq!(WaterDamage::for_map("Major_Madness3.0"), None);
        assert_eq!(WaterDamage::for_map("Dual_Core"), None);
    }

    #[test]
    fn the_base_decides_and_the_surface_counts() {
        assert!(in_water(-8.0, 0.0));
        assert!(in_water(0.0, 0.0), "pos.y <= waterLevel");
        assert!(!in_water(1.0, 0.0));
        // A model lifted 10 above a base 5 under water is in it.
        assert!(in_water(5.0, 10.0));
    }

    #[test]
    fn each_unit_once_per_fifteen_frames() {
        for id in [1u32, 7, 15, 31] {
            let due: Vec<u64> = (0..45).filter(|&f| slow_update_due(f, id)).collect();
            assert_eq!(due.len(), 3, "id {id}: {due:?}");
            assert!(due.windows(2).all(|w| w[1] - w[0] == 15));
        }
    }

    /// A unit in the water loses the map's damage on its slow update and
    /// nothing in between; one on dry ground loses nothing.
    #[test]
    fn units_in_the_water_take_damage() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.insert_resource(WaterDamage(25.0));
        world.init_resource::<DelayedDamage>();
        let wet = world
            .spawn((
                NetId(3),
                Transform::from_xyz(0.0, -8.0, 0.0),
                Health::full(100.0),
            ))
            .id();
        world.spawn((
            NetId(3),
            Transform::from_xyz(0.0, 40.0, 0.0),
            Health::full(100.0),
        ));
        let mut hits = 0;
        for frame in 0..30 {
            world.insert_resource(SimTick(frame));
            world.run_system_once(water_damage).unwrap();
            let due = world
                .resource_mut::<DelayedDamage>()
                .take_due_for_test(frame);
            for hit in &due {
                assert_eq!(hit.target, wet);
                assert_eq!(hit.damage, 25.0);
                assert!(hit.attacker.is_none() && hit.weapon.is_none());
            }
            hits += due.len();
        }
        assert_eq!(hits, 2, "frames 12 and 27");
    }
}

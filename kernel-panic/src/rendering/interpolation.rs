//! Render interpolation of the 30 Hz simulation.
//!
//! The sim (unit movement, piece animation, projectiles, particles)
//! runs in `FixedUpdate` at Spring's `GAME_SPEED`; the camera and UI run
//! every rendered frame. Drawing the raw sim pose makes objects step
//! every other frame at 60 Hz and freeze for several frames at 144 Hz.
//! Spring draws every object at `GetDrawPos(timeOffset)` =
//! `mix(preFrameTra, pos, timeOffset)` (`Sim/Objects/WorldObject.h:67`,
//! used by `Rendering/Units/UnitDrawerData.cpp:366-378`) — the pose
//! blended between the previous and the current sim frame by how far
//! the renderer is into the next one. [`SimPose`] does the same for any
//! entity whose `Transform` the fixed sim writes:
//!
//! - `FixedFirst`: put the true sim pose back into `Transform` (and the
//!   root's `GlobalTransform`), so sim systems never see a blended one.
//! - `FixedLast`: `prev = curr; curr = Transform`.
//! - `PostUpdate` (before transform propagation): write
//!   `lerp/slerp(prev, curr, Time<Fixed>::overstep_fraction())`.
//!
//! A `Transform` changed outside the sim (spawn, teleport, a system in
//! `Update`) is detected because it no longer equals the value this
//! module last wrote; the pose then snaps (`prev = curr = Transform`)
//! instead of sliding in from the old position — Spring's
//! `preFrameTra` is likewise reset on teleports.

use bevy::prelude::*;
use bevy::transform::TransformSystems;

/// Previous / current sim pose of an entity whose `Transform` is
/// written in `FixedUpdate`. Added as a required component of every
/// sim-moved marker (see [`SimInterpolationPlugin`]).
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct SimPose {
    prev: Transform,
    curr: Transform,
    /// The `Transform` value this module last left in the component;
    /// `None` until first seen.
    written: Option<Transform>,
}

impl SimPose {
    fn snap(&mut self, tf: Transform) {
        self.prev = tf;
        self.curr = tf;
        self.written = Some(tf);
    }
}

pub struct SimInterpolationPlugin;

impl Plugin for SimInterpolationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(FixedFirst, restore_sim_pose)
            .add_systems(FixedLast, record_sim_pose)
            .add_systems(
                PostUpdate,
                interpolate_sim_pose.before(TransformSystems::Propagate),
            );
    }
}

/// `FixedFirst`: undo the render blend so the tick starts from the
/// true sim pose.
pub fn restore_sim_pose(
    mut q: Query<(&mut Transform, &mut SimPose, Option<&mut GlobalTransform>, Has<ChildOf>)>,
) {
    for (mut tf, mut pose, gtf, is_child) in &mut q {
        match pose.written {
            Some(w) if w == *tf => {
                if *tf != pose.curr {
                    *tf = pose.curr;
                }
                pose.written = Some(pose.curr);
            }
            // Changed outside the sim (or never seen): that is the pose.
            _ => pose.snap(*tf),
        }
        // Roots: global == local, so sim systems reading
        // `GlobalTransform` see the sim pose too.
        if !is_child && let Some(mut g) = gtf {
            *g = GlobalTransform::from(*tf);
        }
    }
}

/// `FixedLast`: this tick's result becomes `curr`.
pub fn record_sim_pose(mut q: Query<(&Transform, &mut SimPose)>) {
    for (tf, mut pose) in &mut q {
        if pose.written.is_none() {
            pose.snap(*tf);
            continue;
        }
        pose.prev = pose.curr;
        pose.curr = *tf;
        pose.written = Some(*tf);
    }
}

/// `PostUpdate`: blend by how far the render clock is into the next tick.
pub fn interpolate_sim_pose(fixed: Res<Time<Fixed>>, mut q: Query<(&mut Transform, &mut SimPose)>) {
    let alpha = fixed.overstep_fraction().clamp(0.0, 1.0);
    for (mut tf, mut pose) in &mut q {
        match pose.written {
            Some(w) if w == *tf => {}
            _ => {
                pose.snap(*tf);
                continue;
            }
        }
        let blended = blend(&pose.prev, &pose.curr, alpha);
        if blended != *tf {
            *tf = blended;
        }
        pose.written = Some(blended);
    }
}

fn blend(a: &Transform, b: &Transform, t: f32) -> Transform {
    Transform {
        translation: a.translation.lerp(b.translation, t),
        rotation: a.rotation.slerp(b.rotation, t),
        scale: a.scale.lerp(b.scale, t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::time::Duration;

    fn world_with(tf: Transform) -> (World, Entity) {
        let mut world = World::new();
        world.insert_resource(Time::<Fixed>::from_hz(crate::sim::SIMULATION_HZ));
        let e = world.spawn((tf, GlobalTransform::from(tf), SimPose::default())).id();
        (world, e)
    }

    fn sim_tick(world: &mut World, e: Entity, to: Vec3) {
        world.run_system_once(restore_sim_pose).unwrap();
        world.get_mut::<Transform>(e).unwrap().translation = to;
        world.run_system_once(record_sim_pose).unwrap();
    }

    fn render(world: &mut World, e: Entity, overstep_ms: u64) -> Vec3 {
        let mut fixed = world.resource_mut::<Time<Fixed>>();
        let o = fixed.overstep();
        fixed.discard_overstep(o);
        fixed.accumulate_overstep(Duration::from_millis(overstep_ms));
        world.run_system_once(interpolate_sim_pose).unwrap();
        world.get::<Transform>(e).unwrap().translation
    }

    /// Between two sim ticks the drawn pose is the blend of the last two
    /// sim poses; the next tick starts from the true sim pose again.
    #[test]
    fn blends_between_ticks_and_restores_for_the_sim() {
        let (mut world, e) = world_with(Transform::from_xyz(0.0, 0.0, 0.0));
        sim_tick(&mut world, e, Vec3::new(0.0, 0.0, 0.0));
        sim_tick(&mut world, e, Vec3::new(3.0, 0.0, 0.0));
        // Half a tick (16.7 ms of 33.3) into the next frame.
        let drawn = render(&mut world, e, 17);
        assert!((drawn.x - 1.53).abs() < 0.05, "drawn {drawn}");

        world.run_system_once(restore_sim_pose).unwrap();
        assert_eq!(world.get::<Transform>(e).unwrap().translation.x, 3.0);
        assert_eq!(world.get::<GlobalTransform>(e).unwrap().translation().x, 3.0);
    }

    /// A pose written outside the sim (teleport / spawn placement) is
    /// taken as-is instead of sliding in from the old position.
    #[test]
    fn external_writes_snap() {
        let (mut world, e) = world_with(Transform::from_xyz(0.0, 0.0, 0.0));
        sim_tick(&mut world, e, Vec3::ZERO);
        sim_tick(&mut world, e, Vec3::new(3.0, 0.0, 0.0));
        render(&mut world, e, 10);
        world.get_mut::<Transform>(e).unwrap().translation = Vec3::new(500.0, 0.0, 0.0);
        assert_eq!(render(&mut world, e, 20).x, 500.0);
        world.run_system_once(restore_sim_pose).unwrap();
        assert_eq!(world.get::<Transform>(e).unwrap().translation.x, 500.0);
    }
}

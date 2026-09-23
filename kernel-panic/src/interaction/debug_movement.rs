//! Movement debug visualization (F3 toggle).
//!
//! Investigation infrastructure for the pathfinding/tilt stack:
//!
//! - remaining **path polyline** per unit, ground-sampled, with the
//!   current waypoint highlighted — shows what the grid A\* actually
//!   produced and which leg the follower thinks it is on;
//! - per-unit **heading arrow** (red, `transform.forward()`) and
//!   **up-axis** (cyan, `transform.up()`) — surface-tilt anomalies
//!   (sideways lean on slopes, stale tilt after turns) are directly
//!   visible as a cyan axis not matching the terrain normal.
//!
//! Toggle with F3. Gizmos draw at zero depth bias so lines stay visible
//! against terrain.

use bevy::math::Isometry3d;
use bevy::prelude::*;

use crate::interaction::movement::MovePath;
use crate::terrain::heightmap::Heightmap;

/// Debug overlay state. Toggled by `toggle_movement_debug` (F3).
#[derive(Resource, Default)]
pub struct MovementDebug {
    pub enabled: bool,
}

/// Gizmo group for the overlay (separate from command lines so both can
/// be styled independently).
#[derive(Default, Reflect)]
pub struct DebugMovementGizmos;

impl GizmoConfigGroup for DebugMovementGizmos {}

fn toggle_movement_debug(keyboard: Res<ButtonInput<KeyCode>>, mut state: ResMut<MovementDebug>) {
    if keyboard.just_pressed(KeyCode::F3) {
        state.enabled = !state.enabled;
        info!(
            "movement debug overlay: {}",
            if state.enabled { "on" } else { "off" }
        );
    }
}

fn draw_movement_debug(
    debug: Res<MovementDebug>,
    heightmap: Option<Res<Heightmap>>,
    mut gizmos: Gizmos<DebugMovementGizmos>,
    units: Query<(&Transform, Option<&MovePath>)>,
) {
    if !debug.enabled {
        return;
    }
    let Some(heightmap) = heightmap else {
        return;
    };
    let ground = |x: f32, z: f32| heightmap.sample(x, z);

    const LIFT: f32 = 4.0;
    const HEADING_LEN: f32 = 20.0;
    const UP_LEN: f32 = 14.0;

    for (transform, move_path) in &units {
        let pos = transform.translation;

        // Heading (red) and up (cyan) axes — the tilt inspector.
        let fwd = transform.forward().as_vec3();
        gizmos.line(pos, pos + fwd * HEADING_LEN, Color::srgb(1.0, 0.25, 0.1));
        gizmos.line(
            pos,
            pos + transform.up() * UP_LEN,
            Color::srgb(0.1, 0.9, 1.0),
        );

        let Some(path) = move_path else {
            continue;
        };
        if path.current >= path.waypoints.len() {
            continue;
        }

        // Remaining path polyline, ground-sampled per segment so the
        // line hugs the terrain instead of tunneling through ridges.
        let mut prev = [pos.x, ground(pos.x, pos.z) + LIFT, pos.z];
        for wp in &path.waypoints[path.current..] {
            let next = [wp.x, ground(wp.x, wp.z) + LIFT, wp.z];
            gizmos.line(prev.into(), next.into(), Color::srgb(0.2, 1.0, 0.3));
            prev = next;
        }

        // Current waypoint marker (yellow cube), destination marker (white).
        let cur = &path.waypoints[path.current];
        let cur_y = ground(cur.x, cur.z) + LIFT;
        gizmos.cube(
            Transform::from_xyz(cur.x, cur_y, cur.z).with_scale(Vec3::splat(3.0)),
            Color::srgb(1.0, 0.9, 0.2),
        );
        if let Some(last) = path.waypoints.last()
            && path.current + 1 < path.waypoints.len()
        {
            let last_y = ground(last.x, last.z) + LIFT;
            gizmos.sphere(
                Isometry3d::from_translation(Vec3::new(last.x, last_y, last.z)),
                4.0,
                Color::srgb(1.0, 1.0, 1.0),
            );
        }
    }
}

/// Plugin wiring: resource, F3 toggle, and the draw pass (after
/// `movement_system` so the drawn heading reflects this frame's turn).
pub struct DebugMovementPlugin;

impl Plugin for DebugMovementPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MovementDebug>()
            .init_gizmo_group::<DebugMovementGizmos>()
            .add_systems(Update, (toggle_movement_debug, draw_movement_debug).chain());
    }
}

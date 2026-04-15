use bevy::prelude::*;

use spring_pathfinding::{NodeLayer, Path, find_path};

use crate::units::components::UnitType;
use crate::units::definitions::stats;

/// When present, the unit will move toward this world position.
#[derive(Component)]
pub struct MoveTarget(pub Vec3);

/// A computed path the unit follows waypoint-by-waypoint.
#[derive(Component)]
pub struct MovePath {
    pub waypoints: Vec<Vec3>,
    /// Index of the next waypoint to reach.
    pub current: usize,
}

/// The pathfinding grid resource, built from the loaded map.
#[derive(Resource)]
pub struct NavGrid(pub NodeLayer);

#[allow(clippy::too_many_arguments)]
pub fn movement_system(
    mut commands: Commands,
    time: Res<Time>,
    mut nav_grid: Option<ResMut<NavGrid>>,
    mut query: Query<(
        Entity,
        &UnitType,
        &mut Transform,
        Option<&MoveTarget>,
        Option<&mut MovePath>,
    )>,
) {
    for (entity, unit_type, mut transform, move_target, move_path) in &mut query {
        let unit_stats = stats(unit_type.0);
        if unit_stats.speed == 0.0 {
            // Buildings can't move — remove any movement components.
            commands.entity(entity).remove::<MoveTarget>();
            commands.entity(entity).remove::<MovePath>();
            continue;
        }

        // If we have a MoveTarget but no MovePath, compute the path.
        if let Some(target) = move_target {
            if move_path.is_none() {
                let path = compute_path(nav_grid.as_deref_mut(), transform.translation, target.0);
                commands.entity(entity).insert(MovePath {
                    waypoints: path,
                    current: 0,
                });
            }
        }

        // Follow the path waypoint by waypoint.
        let Some(mut path) = move_path else {
            continue;
        };

        if path.current >= path.waypoints.len() {
            // Path complete.
            commands.entity(entity).remove::<MoveTarget>();
            commands.entity(entity).remove::<MovePath>();
            continue;
        }

        let current = transform.translation;
        let waypoint = path.waypoints[path.current];
        let goal = Vec3::new(waypoint.x, current.y, waypoint.z);
        let diff = goal - current;
        let distance = diff.length();

        let arrival_threshold = 8.0;
        if distance < arrival_threshold {
            path.current += 1;
            continue;
        }

        let direction = diff / distance;
        let step = unit_stats.speed * time.delta_secs();
        let movement = direction * step.min(distance);

        transform.translation += movement;
        transform.look_to(Vec3::new(direction.x, 0.0, direction.z), Vec3::Y);
    }
}

/// Compute a path using the QTPFS nav grid, or fall back to straight-line.
fn compute_path(nav_grid: Option<&mut NavGrid>, from: Vec3, to: Vec3) -> Vec<Vec3> {
    if let Some(nav) = nav_grid {
        let src = [from.x, from.z];
        let dst = [to.x, to.z];
        let path = find_path(&mut nav.0, src, dst);

        if !path.is_empty() {
            return path
                .points
                .iter()
                .map(|p| Vec3::new(p[0], 0.0, p[1]))
                .collect();
        }
    }

    // Fallback: straight-line.
    vec![to]
}

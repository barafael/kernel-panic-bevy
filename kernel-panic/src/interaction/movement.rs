use bevy::prelude::*;

use crate::units::components::UnitType;
use crate::units::definitions::stats;

/// When present, the unit will move toward this world position.
#[derive(Component)]
pub struct MoveTarget(pub Vec3);

pub fn movement_system(
    mut commands: Commands,
    time: Res<Time>,
    mut query: Query<(Entity, &UnitType, &mut Transform, &MoveTarget)>,
) {
    for (entity, unit_type, mut transform, target) in &mut query {
        let unit_stats = stats(unit_type.0);
        if unit_stats.speed == 0.0 {
            // Buildings can't move.
            commands.entity(entity).remove::<MoveTarget>();
            continue;
        }

        let current = transform.translation;
        let goal = Vec3::new(target.0.x, current.y, target.0.z);
        let diff = goal - current;
        let distance = diff.length();

        let arrival_threshold = 5.0;
        if distance < arrival_threshold {
            commands.entity(entity).remove::<MoveTarget>();
            continue;
        }

        let direction = diff / distance;
        let step = unit_stats.speed * time.delta_secs();
        let movement = direction * step.min(distance);

        transform.translation += movement;

        // Face the movement direction.
        transform.look_to(Vec3::new(direction.x, 0.0, direction.z), Vec3::Y);
    }
}

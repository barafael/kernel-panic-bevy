use bevy::prelude::*;

use super::animation::CobFileCache;
use super::components::{Faction, TeamId, UnitType};
use super::definitions::UnitKind;
use super::meshes::S3OModelCache;
use super::spawning::{SelectionVolumeMaterial, spawn_unit};

/// Attached to factories/homebases. Continuously produces units.
#[derive(Component)]
pub struct Producer {
    produces: UnitKind,
    build_time: f32,
    progress: f32,
}

impl Producer {
    pub fn new(produces: UnitKind, build_time: f32) -> Self {
        Self {
            produces,
            build_time,
            progress: 0.0,
        }
    }
}

pub fn default_production(kind: UnitKind) -> Option<Producer> {
    match kind {
        UnitKind::Kernel => Some(Producer::new(UnitKind::Bit, 2.0)),
        UnitKind::Hole => Some(Producer::new(UnitKind::Bug, 2.2)),
        UnitKind::Connection => Some(Producer::new(UnitKind::Packet, 2.0)),
        UnitKind::Socket => Some(Producer::new(UnitKind::Bit, 2.0)),
        UnitKind::Window => Some(Producer::new(UnitKind::Bug, 2.2)),
        UnitKind::Port => Some(Producer::new(UnitKind::Packet, 2.0)),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn production_system(
    time: Res<Time>,
    mut producers: Query<(&mut Producer, &Faction, &TeamId, &GlobalTransform)>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    invisible_mat: Option<Res<SelectionVolumeMaterial>>,
    existing_units: Query<(), With<UnitType>>,
) {
    let unit_count = existing_units.iter().count();
    if unit_count > 500 {
        return;
    }

    let Some(invisible_mat) = invisible_mat else {
        return;
    };

    let dt = time.delta_secs();
    let mut spawns: Vec<(UnitKind, Faction, u8, Vec3)> = Vec::new();

    for (mut producer, faction, team, global_tf) in &mut producers {
        producer.progress += dt;

        if producer.progress >= producer.build_time {
            producer.progress -= producer.build_time;

            let factory_pos = global_tf.translation();
            let offset = Vec3::new(40.0, 0.0, 40.0);
            let spawn_pos = factory_pos + offset;

            spawns.push((producer.produces, *faction, team.0, spawn_pos));
        }
    }

    let invisible_mat_ref = SelectionVolumeMaterial(invisible_mat.0.clone());
    for (kind, faction, team, position) in spawns {
        spawn_unit(
            kind,
            faction,
            team,
            position,
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut model_cache,
            &mut cob_cache,
            &invisible_mat_ref,
        );
    }
}

use bevy::prelude::*;

use super::animation::CobFileCache;
use super::components::{Faction, TeamId, UnitType};
use super::definitions::{self, UnitKind};
use super::meshes::S3OModelCache;
use super::spawning::{SelectionVolumeMaterial, spawn_unit};

/// Attached to factories/homebases. Continuously produces units.
///
/// The factory always builds the front of its `queue`. When the queue is empty
/// it falls back to its default `produces` kind (infinite auto-production).
#[derive(Component)]
pub struct Producer {
    /// Default unit produced when the queue is empty.
    produces: UnitKind,
    /// Seconds to build one unit of the *current* type.
    build_time: f32,
    /// Seconds accumulated toward the current unit.
    progress: f32,
    /// Player-enqueued build orders (FIFO). Takes priority over `produces`.
    queue: Vec<UnitKind>,
}

impl Producer {
    pub fn new(produces: UnitKind, build_time: f32) -> Self {
        Self {
            produces,
            build_time,
            progress: 0.0,
            queue: Vec::new(),
        }
    }

    /// What is currently being built.
    pub fn current_production(&self) -> UnitKind {
        self.queue.first().copied().unwrap_or(self.produces)
    }

    /// Build progress as a fraction 0.0..1.0.
    pub fn progress_fraction(&self) -> f32 {
        if self.build_time > 0.0 {
            (self.progress / self.build_time).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The queued build orders (not including the auto-produced default).
    pub fn queue(&self) -> &[UnitKind] {
        &self.queue
    }

    /// Enqueue a unit to be built.
    pub fn enqueue(&mut self, kind: UnitKind) {
        self.queue.push(kind);
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
        let current_kind = producer.current_production();
        let current_build_time = definitions::stats(current_kind)
            .build_time
            .max(producer.build_time);

        producer.build_time = current_build_time;
        producer.progress += dt;

        if producer.progress >= current_build_time {
            producer.progress -= current_build_time;

            let factory_pos = global_tf.translation();
            let offset = Vec3::new(40.0, 0.0, 40.0);
            let spawn_pos = factory_pos + offset;

            spawns.push((current_kind, *faction, team.0, spawn_pos));

            // Pop from queue if this was a queued order.
            if !producer.queue.is_empty() {
                producer.queue.remove(0);
            }
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

use std::collections::VecDeque;

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
    /// Seconds accumulated toward the current unit.
    progress: f32,
    /// Player-enqueued build orders (FIFO). Takes priority over `produces`.
    queue: VecDeque<UnitKind>,
}

impl Producer {
    pub fn new(produces: UnitKind) -> Self {
        Self {
            produces,
            progress: 0.0,
            queue: VecDeque::new(),
        }
    }

    /// What is currently being built.
    pub fn current_production(&self) -> UnitKind {
        self.queue.front().copied().unwrap_or(self.produces)
    }

    /// Build time for the current production, derived from unit stats.
    fn current_build_time(&self) -> f32 {
        definitions::stats(self.current_production()).build_time
    }

    /// Build progress as a fraction 0.0..1.0.
    pub fn progress_fraction(&self) -> f32 {
        let bt = self.current_build_time();
        if bt > 0.0 {
            (self.progress / bt).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The queued build orders (not including the auto-produced default).
    pub fn queue(&self) -> &VecDeque<UnitKind> {
        &self.queue
    }

    /// Enqueue a unit to be built (max 20 items).
    pub fn enqueue(&mut self, kind: UnitKind) {
        if self.queue.len() < 20 {
            self.queue.push_back(kind);
        }
    }
}

pub fn default_production(kind: UnitKind) -> Option<Producer> {
    match kind {
        UnitKind::Kernel => Some(Producer::new(UnitKind::Bit)),
        UnitKind::Hole => Some(Producer::new(UnitKind::Bug)),
        UnitKind::Connection => Some(Producer::new(UnitKind::Packet)),
        UnitKind::Socket => Some(Producer::new(UnitKind::Bit)),
        UnitKind::Window => Some(Producer::new(UnitKind::Bug)),
        UnitKind::Port => Some(Producer::new(UnitKind::Packet)),
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
    let Some(invisible_mat) = invisible_mat else {
        return;
    };

    let dt = time.delta_secs();
    let mut spawns: Vec<(UnitKind, Faction, u8, Vec3)> = Vec::new();

    for (mut producer, faction, team, global_tf) in &mut producers {
        let build_time = producer.current_build_time();
        producer.progress += dt;

        if producer.progress >= build_time {
            producer.progress -= build_time;

            // Defer the cap check until a unit is actually about to spawn.
            if existing_units.iter().count() > 500 {
                producer.progress = build_time; // undo, try again next frame
                continue;
            }

            let factory_pos = global_tf.translation();
            let spawn_pos = factory_pos + Vec3::new(40.0, 0.0, 40.0);

            spawns.push((producer.current_production(), *faction, team.0, spawn_pos));
            producer.queue.pop_front();
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

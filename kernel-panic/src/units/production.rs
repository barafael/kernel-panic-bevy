use std::collections::VecDeque;

use bevy::prelude::*;

use super::animation::{CobAnimator, CobFileCache};
use super::components::{Faction, TeamId, UnitType};
use super::definitions::UnitKind;
use super::meshes::S3OModelCache;
use super::spawning::{
    EMERGE_DEPTH, EMERGE_DURATION, Emerging, FactoryPieces, SelectionVolumeMaterial, spawn_unit,
};
use super::unit_registry::UnitRegistry;
use super::weapon_fx::{AttackEvent, PendingAttacks};

/// Attached to factories/homebases. Builds units from its queue.
///
/// The factory builds the front of its `queue`. When the queue is empty
/// production is idle — nothing is built until the player enqueues something.
#[derive(Component)]
pub struct Producer {
    /// Seconds accumulated toward the current unit.
    progress: f32,
    /// Player-enqueued build orders (FIFO).
    queue: VecDeque<UnitKind>,
}

impl Producer {
    pub fn new(_default: UnitKind) -> Self {
        Self {
            progress: 0.0,
            queue: VecDeque::new(),
        }
    }

    /// What is currently being built, if anything.
    pub fn current_production(&self) -> Option<UnitKind> {
        self.queue.front().copied()
    }

    /// Build time for the current production using the unit registry.
    fn current_build_time(&self, registry: &UnitRegistry) -> Option<f32> {
        self.current_production()
            .map(|kind| registry.build_time(kind))
    }

    /// Build progress as a fraction 0.0..1.0.
    pub fn progress_fraction(&self, registry: &UnitRegistry) -> f32 {
        let Some(bt) = self.current_build_time(registry) else {
            return 0.0;
        };
        if bt > 0.0 {
            (self.progress / bt).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// The queued build orders.
    pub fn queue(&self) -> &VecDeque<UnitKind> {
        &self.queue
    }

    /// Enqueue a unit to be built (max 100 items).
    pub fn enqueue(&mut self, kind: UnitKind) {
        if self.queue.len() < 100 {
            self.queue.push_back(kind);
        }
    }
}

/// Which units are factories and what they produce by default.
/// Hardcoded from upstream sidedata.lua — acceptable for KP's fixed unit roster.
///
/// Mobile builders (Assembler / Trojan / Gateway) are *not* listed here —
/// they use the `construction` pipeline (walk to datavent, erect on site)
/// rather than the factory-style progress-and-emerge flow.
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

/// Push one frame's worth of a build-laser strand from the factory's
/// `nanoemitter` piece to its `pad` piece. The COB script (`Activate` in
/// `hole.bos`/`kernel.bos`/etc) drives `nanoemitter` in a circular pattern
/// around the `pad` over time, so just reading the live world position of
/// those two pieces every frame gives us the exact same "rays slowly
/// circling the build hole" look the original game has.
///
/// `factory_root` is the producer entity's translation, used as a fallback
/// when either piece is missing from the model.
fn emit_build_ray(
    nanoemitter_world: Vec3,
    pad_world: Vec3,
    factory_root: Vec3,
    pending: &mut PendingAttacks,
) {
    // Defensive: zero-length rays produce no visible effect and would
    // generate a NaN normal in spawn_beam — skip them.
    let length_sq = (pad_world - nanoemitter_world).length_squared();
    let (start, end) = if length_sq < 1.0 {
        // Either piece collapsed to the root; synthesise a short downward
        // strand from a small offset above the factory so something still
        // shows during the rare frames before the COB has had time to run.
        (factory_root + Vec3::new(8.0, 24.0, 0.0), factory_root)
    } else {
        (nanoemitter_world, pad_world)
    };

    pending.events.push(AttackEvent {
        attacker_pos: start,
        target_pos: end,
        weapon_name: "BuildLaser".to_string(),
    });
}

/// Look up the world position of an animated piece on a factory by index.
/// Returns `None` if the piece doesn't exist or its global transform isn't
/// available yet (e.g. the same frame the unit was spawned).
fn piece_world_pos(
    piece_idx: Option<usize>,
    animator: Option<&CobAnimator>,
    piece_transforms: &Query<&GlobalTransform, With<super::animation::PieceIndex>>,
) -> Option<Vec3> {
    let idx = piece_idx?;
    let animator = animator?;
    let entity = *animator.piece_entities.get(idx)?;
    piece_transforms.get(entity).ok().map(|gt| gt.translation())
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn production_system(
    time: Res<Time>,
    mut producers: Query<(
        &mut Producer,
        &Faction,
        &TeamId,
        &GlobalTransform,
        Option<&FactoryPieces>,
        Option<&CobAnimator>,
    )>,
    emerging_q: Query<(&GlobalTransform, &Emerging)>,
    piece_transforms: Query<&GlobalTransform, With<super::animation::PieceIndex>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut model_cache: ResMut<S3OModelCache>,
    mut cob_cache: ResMut<CobFileCache>,
    invisible_mat: Option<Res<SelectionVolumeMaterial>>,
    existing_units: Query<(), With<UnitType>>,
    unit_registry: Res<UnitRegistry>,
    mut pending_attacks: ResMut<PendingAttacks>,
) {
    let Some(invisible_mat) = invisible_mat else {
        return;
    };

    let dt = time.delta_secs();
    // Each spawn carries: unit kind, faction, team, the underground spawn
    // position (where the model first appears), the surface y it should
    // emerge to, and an optional rally point to walk to once emerged.
    let mut spawns: Vec<(UnitKind, Faction, u8, Vec3, f32, Option<Vec3>)> = Vec::new();

    for (mut producer, faction, team, global_tf, factory_pieces, animator) in &mut producers {
        let Some(build_time) = producer.current_build_time(&unit_registry) else {
            // Queue is empty — idle.
            producer.progress = 0.0;
            continue;
        };

        producer.progress += dt;

        let factory_pos = global_tf.translation();
        let nanoemitter_pos = factory_pieces
            .and_then(|fp| piece_world_pos(fp.nanoemitter, animator, &piece_transforms))
            .unwrap_or(factory_pos + Vec3::new(0.0, 24.0, 16.0));
        let pad_pos = factory_pieces
            .and_then(|fp| piece_world_pos(fp.pad, animator, &piece_transforms))
            .unwrap_or(factory_pos);

        emit_build_ray(nanoemitter_pos, pad_pos, factory_pos, &mut pending_attacks);

        if producer.progress >= build_time {
            producer.progress -= build_time;

            if existing_units.iter().count() > 10_000 {
                producer.progress = build_time; // undo, try again next frame
                continue;
            }

            // Compute a rally point that's offset from the factory in its
            // forward direction so the new unit walks clear of the hole
            // once it has finished emerging. Stationary units (speed == 0)
            // get no rally point.
            let kind = producer.current_production().unwrap();
            let rally_point = if unit_registry.speed(kind) > 0.0 {
                let forward = global_tf.forward().as_vec3();
                let exit_offset = if forward.length_squared() > 0.01 {
                    forward.normalize() * 60.0
                } else {
                    Vec3::new(0.0, 0.0, 60.0)
                };
                Some(pad_pos + exit_offset)
            } else {
                None
            };

            // Underground spawn position: the same XZ as the pad but
            // `EMERGE_DEPTH` below it. The Emerging system lifts the unit
            // back to `pad_pos.y` so it visibly comes up through the hole.
            let underground = Vec3::new(pad_pos.x, pad_pos.y - EMERGE_DEPTH, pad_pos.z);
            spawns.push((kind, *faction, team.0, underground, pad_pos.y, rally_point));
            producer.queue.pop_front();
        }
    }

    // Keep the rays firing on units that are still rising through the hole
    // so the visual transitions smoothly from "building" to "emerging".
    // We don't have a back-reference from the emerging unit to the factory
    // that built it, so use a fixed rim offset above the surface point;
    // that's close enough while the unit is briefly underground.
    for (emerging_tf, emerging) in &emerging_q {
        let pos = emerging_tf.translation();
        let surface = Vec3::new(pos.x, emerging.target_y, pos.z);
        let rim = surface + Vec3::new(0.0, 24.0, 16.0);
        emit_build_ray(rim, surface, surface, &mut pending_attacks);
    }

    let invisible_mat_ref = SelectionVolumeMaterial(invisible_mat.0.clone());
    for (kind, faction, team, underground_pos, target_y, rally_point) in spawns {
        let entity = spawn_unit(
            kind,
            faction,
            team,
            underground_pos,
            &mut commands,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut model_cache,
            &mut cob_cache,
            &invisible_mat_ref,
            &unit_registry,
        );
        commands.entity(entity).insert(Emerging {
            target_y,
            remaining: EMERGE_DURATION,
            total: EMERGE_DURATION,
            rally_point,
        });
    }
}

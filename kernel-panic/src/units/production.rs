use std::collections::VecDeque;

use bevy::prelude::*;

use super::animation::PieceIndex;
use super::animation::{CobAnimator, CobFileCache};
use super::components::{Faction, TeamId, UnitType};
use super::definitions::UnitKind;
use super::meshes::S3OModelCache;
use super::spawning::{
    EMERGE_DEPTH, EMERGE_LEAD_TIME, EmergeStyle, Emerging, FactoryPieces, FadeMaterials,
    SelectionVolumeMaterial, spawn_unit,
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
    /// True once this build cycle has spawned its unit underground (still
    /// rising). Reset to false when the cycle completes and the next item
    /// in the queue starts. Prevents the production system from spawning
    /// the same unit twice while progress ramps from "spawn moment" to
    /// "build_time" (the rising-out-of-the-pad window).
    unit_spawned: bool,
}

impl Producer {
    pub fn new(_default: UnitKind) -> Self {
        Self {
            progress: 0.0,
            queue: VecDeque::new(),
            unit_spawned: false,
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
/// Homebase production-speed bonus per small building the team owns.
/// Matches upstream `kernelboost.lua::bonusPerFac = 0.2`.
pub const KERNEL_BOOST_PER_BUILDING: f32 = 0.2;

pub fn default_production(kind: UnitKind) -> Option<Producer> {
    match kind {
        UnitKind::Kernel => Some(Producer::new(UnitKind::Bit)),
        UnitKind::Hole => Some(Producer::new(UnitKind::Bug)),
        UnitKind::Connection => Some(Producer::new(UnitKind::Packet)),
        UnitKind::Socket => Some(Producer::new(UnitKind::Bit)),
        UnitKind::Window => Some(Producer::new(UnitKind::Bug)),
        // Port is a teleporter, not a factory — it tops up its team's
        // PacketBuffer every 5.5s rather than spawning units directly.
        _ => None,
    }
}

/// Push one frame's worth of a build-laser strand from `start` to `end`,
/// or — if either is too close to the factory root — synthesise a short
/// downward strand so something still shows during the first frame after
/// spawn (before the COB script has had time to position its pieces).
fn emit_build_ray(start: Vec3, end: Vec3, factory_root: Vec3, pending: &mut PendingAttacks) {
    // Defensive: zero-length rays produce no visible effect and would
    // generate a NaN normal in spawn_beam — skip them.
    let length_sq = (end - start).length_squared();
    let (start, end) = if length_sq < 1.0 {
        (factory_root + Vec3::new(8.0, 24.0, 0.0), factory_root)
    } else {
        (start, end)
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
        Option<&super::components::Homebase>,
    )>,
    small_buildings: Query<(&UnitType, &TeamId)>,
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

    // Kernel Boost: count small buildings per team so homebase producers
    // can apply a +20% speed bonus per friendly small building.
    let mut small_count: std::collections::HashMap<u8, u32> = std::collections::HashMap::new();
    for (unit, team) in &small_buildings {
        if super::network_buffer::is_small_building(unit.0) {
            *small_count.entry(team.0).or_default() += 1;
        }
    }

    // Each spawn carries: unit kind, faction, team, the underground spawn
    // position (where the model first appears), the surface y it should
    // emerge to, and an optional rally point to walk to once emerged.
    // Each spawn carries: unit kind, faction, team, spawn position,
    // surface y to emerge to, optional rally point, the emerge duration
    // (= the time remaining in the build cycle when the unit was spawned,
    // so the rise/fade finishes exactly at build complete), and the
    // visual emerge style (rise vs fade).
    let mut spawns: Vec<(
        UnitKind,
        Faction,
        u8,
        Vec3,
        f32,
        Option<Vec3>,
        f32,
        EmergeStyle,
    )> = Vec::new();

    for (mut producer, faction, team, global_tf, factory_pieces, animator, homebase) in
        &mut producers
    {
        let Some(build_time) = producer.current_build_time(&unit_registry) else {
            // Queue is empty — idle.
            producer.progress = 0.0;
            continue;
        };

        let speed_mult = if homebase.is_some() {
            let buildings = small_count.get(&team.0).copied().unwrap_or(0) as f32;
            1.0 + buildings * KERNEL_BOOST_PER_BUILDING
        } else {
            1.0
        };
        producer.progress += dt * speed_mult;

        let factory_pos = global_tf.translation();
        let pad_pos = factory_pieces
            .and_then(|fp| piece_world_pos(fp.pad, animator, &piece_transforms))
            .unwrap_or(factory_pos);

        // One ray per emitter piece. Kernel has 4 (one per pillar tip),
        // socket has 2 (the orbiting blasers), most others have 1
        // (`nanoemitter`). When there are no resolvable emitters at all
        // — Connection has none — fall back to the synthetic above-root
        // offset so the player still sees *something* indicating
        // construction is happening.
        let mut emitted_any = false;
        if let Some(fp) = factory_pieces {
            for &emitter_idx in &fp.emitters {
                if let Some(pos) = piece_world_pos(Some(emitter_idx), animator, &piece_transforms) {
                    emit_build_ray(pos, pad_pos, factory_pos, &mut pending_attacks);
                    emitted_any = true;
                }
            }
        }
        if !emitted_any {
            let synthetic = factory_pos + Vec3::new(0.0, 24.0, 16.0);
            emit_build_ray(synthetic, pad_pos, factory_pos, &mut pending_attacks);
        }

        // Two-phase spawn: when the producer's progress reaches the
        // spawn threshold (build_time - EMERGE_LEAD_TIME), drop the
        // unit underground and let the emerge system lift it. The
        // *queue* doesn't pop until the full build_time has elapsed,
        // which gives the rising unit something to ride on (and keeps
        // the build laser firing on its emitters until completion).
        let emerge_lead = EMERGE_LEAD_TIME.min(build_time);
        let spawn_threshold = (build_time - emerge_lead).max(0.0);

        if !producer.unit_spawned && producer.progress >= spawn_threshold {
            if existing_units.iter().count() > 10_000 {
                // Don't busy-loop; pin progress at the threshold and
                // try again next frame.
                producer.progress = spawn_threshold;
                continue;
            }

            // Compute a rally point that's offset from the factory in
            // its forward direction so the new unit walks clear of the
            // hole once it has finished emerging. Stationary units
            // (speed == 0) get no rally point.
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

            // System units rise out of the ground (start underground at
            // `pad_y - EMERGE_DEPTH`); Hacker / Network units materialize
            // at-surface with an alpha ramp. Style picks both behaviors.
            let style = match faction {
                Faction::System => EmergeStyle::Rise,
                Faction::Hacker | Faction::Network => EmergeStyle::Fade,
            };
            let spawn_pos = match style {
                EmergeStyle::Rise => Vec3::new(pad_pos.x, pad_pos.y - EMERGE_DEPTH, pad_pos.z),
                EmergeStyle::Fade => Vec3::new(pad_pos.x, pad_pos.y, pad_pos.z),
            };
            spawns.push((
                kind,
                *faction,
                team.0,
                spawn_pos,
                pad_pos.y,
                rally_point,
                emerge_lead,
                style,
            ));
            producer.unit_spawned = true;
        }

        if producer.progress >= build_time {
            producer.progress -= build_time;
            producer.unit_spawned = false;
            producer.queue.pop_front();
        }
    }

    let invisible_mat_ref = SelectionVolumeMaterial(invisible_mat.0.clone());
    for (kind, faction, team, spawn_pos, target_y, rally_point, emerge_duration, style) in spawns {
        let entity = spawn_unit(
            kind,
            faction,
            team,
            spawn_pos,
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
            remaining: emerge_duration,
            total: emerge_duration,
            rally_point,
            style,
        });
        // Fade-style emergence needs per-unit cloned materials so the
        // alpha ramp doesn't leak onto every other unit sharing the
        // shared faction texture. We can't read the freshly-spawned
        // piece children here (they were queued via Commands and won't
        // exist until the next schedule sync), so a follow-up system
        // (`install_fade_materials`) does the clone next frame.
        if matches!(style, EmergeStyle::Fade) {
            commands.entity(entity).insert(PendingFadeInstall);
        }
    }
}

/// Marker placed on a freshly-spawned `Fade`-style emerging unit so the
/// next-frame `install_fade_materials` system can swap each piece's
/// MeshMaterial3d for a per-unit clone before the alpha ramp starts.
#[derive(Component)]
pub struct PendingFadeInstall;

/// System: host-animate Connection's `body` piece as a "hatch" — lifts
/// up by 16 elmos while the Connection is producing, drops back down
/// when idle. The upstream Network homebase (Carrier) does this in its
/// .bos via Activate/Deactivate moving a `mover` piece, but our
/// Connection's .bos has no such handler — so we drive it from the
/// host the same way `aim_weapons_system` host-drives the Pointer's
/// gunbase.
pub fn animate_connection_hatch(mut query: Query<(&UnitType, &Producer, &mut CobAnimator)>) {
    const HATCH_LIFT_ELMOS: f32 = 16.0;
    const HATCH_SPEED_ELMOS_PER_SEC: f32 = 24.0;

    for (unit_type, producer, mut animator) in &mut query {
        if unit_type.0 != UnitKind::Connection {
            continue;
        }
        let body_idx = animator
            .cob
            .piece_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case("body"));
        let Some(idx) = body_idx else {
            continue;
        };
        if idx >= animator.target_translations.len() {
            continue;
        }
        let target_y = if producer.current_production().is_some() {
            HATCH_LIFT_ELMOS
        } else {
            0.0
        };
        animator.target_translations[idx][1] = target_y;
        animator.move_speeds[idx][1] = HATCH_SPEED_ELMOS_PER_SEC;
    }
}

/// Run after `production_system`: for each entity tagged
/// `PendingFadeInstall`, walk its piece children, clone each shared
/// StandardMaterial into a per-unit handle (with `AlphaMode::Blend` and
/// alpha=0), and install a `FadeMaterials` component holding the swap
/// records so `emerge_system` can both fade them in and revert them
/// when emergence completes.
pub fn install_fade_materials(
    mut commands: Commands,
    pending: Query<(Entity, &Children), With<PendingFadeInstall>>,
    piece_q: Query<&Children, With<PieceIndex>>,
    leaf_q: Query<&MeshMaterial3d<StandardMaterial>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (entity, children) in &pending {
        let mut overrides = Vec::new();
        let mut stack: Vec<Entity> = children.iter().collect();
        while let Some(node) = stack.pop() {
            // Recurse into nested piece children first so deep s3o
            // hierarchies (gunbase → gun → gunpoint, etc) all get
            // their own per-unit alpha.
            if let Ok(grand) = piece_q.get(node) {
                stack.extend(grand.iter());
            }
            let Ok(mat_handle) = leaf_q.get(node) else {
                continue;
            };
            let original = mat_handle.0.clone();
            let Some(src) = materials.get(&original).cloned() else {
                continue;
            };
            let faded = materials.add(StandardMaterial {
                base_color: src.base_color.with_alpha(0.0),
                base_color_texture: src.base_color_texture.clone(),
                emissive: src.emissive,
                alpha_mode: AlphaMode::Blend,
                unlit: src.unlit,
                ..default()
            });
            commands.entity(node).insert(MeshMaterial3d(faded.clone()));
            overrides.push((node, faded, original));
        }
        commands
            .entity(entity)
            .insert(FadeMaterials { overrides })
            .remove::<PendingFadeInstall>();
    }
}

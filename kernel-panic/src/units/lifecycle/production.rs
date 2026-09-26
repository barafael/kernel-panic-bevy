use std::collections::VecDeque;

use bevy::prelude::*;

use super::spawning::{
    EMERGE_DEPTH, EMERGE_LEAD_TIME, EmergeStyle, Emerging, FactoryPieces, FadeMaterials,
    SpawnContext, spawn_unit,
};
use crate::units::assets::animation::{PieceIndex, UnitAnimator};
use crate::units::components::{Faction, TeamId, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::content::weapons::WeaponId;
use crate::units::weapon_fx::{AttackEvent, PendingAttacks};

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
    /// Monotonic count of units this factory has spawned. Used to spread
    /// rally points deterministically across a grid in front of the
    /// factory so a stream of Packets doesn't pile onto the same spot.
    spawn_count: u32,
    /// Spring `CMD.REPEAT`: when a queued unit finishes it is re-appended
    /// to the back of the queue instead of dropped, so the factory cycles
    /// its queue forever. Minifacs start with it on (upstream
    /// `kp_autospam.lua`); homebases start with it off.
    repeat: bool,
}

impl Producer {
    pub fn new() -> Self {
        Self {
            progress: 0.0,
            queue: VecDeque::new(),
            unit_spawned: false,
            spawn_count: 0,
            repeat: false,
        }
    }

    /// A factory on repeat with `kind` queued — the upstream
    /// `kp_autospam.lua` start state for Sockets / Windows, applied to
    /// every team (the widget runs for every human player and KPAI's
    /// `AddMiniFac` issues the same orders for AI teams).
    pub fn spamming(kind: UnitKind) -> Self {
        let mut producer = Self::new();
        producer.repeat = true;
        producer.queue.push_back(kind);
        producer
    }

    /// Whether finished units are re-appended to the queue.
    #[cfg(test)]
    pub fn repeat(&self) -> bool {
        self.repeat
    }

    /// Toggle repeat. The fair AI switches its minifacs' repeat off
    /// whenever its spam budget runs out (upstream `KPAI_Fair.lua` slow
    /// update issues `CMD.REPEAT {0}` on every MiniFac).
    pub fn set_repeat(&mut self, on: bool) {
        self.repeat = on;
    }

    /// Pop the finished front unit, re-appending it when on repeat.
    fn complete_front(&mut self) {
        if let Some(done) = self.queue.pop_front()
            && self.repeat
        {
            self.queue.push_back(done);
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

    /// The queued build orders. Read by the build menu's queue-summary
    /// UI and the AI's refill check.
    pub fn queue(&self) -> &VecDeque<UnitKind> {
        &self.queue
    }

    /// Enqueue a unit to be built. The queue is unbounded; the player
    /// can stack as many orders as they want (build menu, AI).
    pub fn enqueue(&mut self, kind: UnitKind) {
        self.queue.push_back(kind);
    }

    /// Pop and return the next unit to build, if any. Used by tests to
    /// assert on the AI's build sequencing.
    #[allow(dead_code)]
    pub fn dequeue_front(&mut self) -> Option<UnitKind> {
        self.queue.pop_front()
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
        UnitKind::Kernel | UnitKind::Hole | UnitKind::Carrier => Some(Producer::new()),
        // Minifacs autospam from the moment they finish building:
        // upstream `kp_autospam.lua` gives every socket `REPEAT` + `bit`
        // and every window `REPEAT` + `bug`. The player can still add to
        // or toggle the queue; production waits for `Emerging` to end,
        // mirroring the widget's `UnitFinished` hook.
        UnitKind::Socket => Some(Producer::spamming(UnitKind::Bit)),
        UnitKind::Window => Some(Producer::spamming(UnitKind::Bug)),
        // Port is a teleporter, not a factory — it tops up its team's
        // PacketBuffer every 5.5s rather than spawning units directly.
        // Connection (mobile) is likewise a teleporter — it dispatches
        // Packets from the buffer but does not build new units.
        _ => None,
    }
}

/// Push one frame's worth of a build-laser strand from `start` to `end`,
/// or — if either is too close to the factory root — synthesise a short
/// downward strand so something still shows during the first frame after
/// spawn (before the COB script has had time to position its pieces).
fn emit_build_ray(
    start: Vec3,
    end: Vec3,
    factory_root: Vec3,
    pending: &mut PendingAttacks,
    build_arc: bool,
) {
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
        weapon_id: WeaponId::BUILD_LASER,
        // BuildLaser pulses don't drive a muzzle CEG — the sparkle at
        // the target end is the primary fx; strobing the builder every
        // frame would drown out the rest of the scene.
        muzzle_ceg: None,
        delayed_hit: None,
        build_arc,
    });
}

/// Look up the world position of an animated piece on a factory by index.
/// Returns `None` if the piece doesn't exist or its global transform isn't
/// available yet (e.g. the same frame the unit was spawned).
fn piece_world_pos(
    piece_idx: Option<usize>,
    animator: Option<&UnitAnimator>,
    piece_transforms: &Query<&GlobalTransform, With<PieceIndex>>,
) -> Option<Vec3> {
    let idx = piece_idx?;
    let animator = animator?;
    let entity = *animator.rig.piece_entities.get(idx)?;
    piece_transforms.get(entity).ok().map(|gt| gt.translation())
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn production_system(
    time: Res<Time>,
    mut producers: Query<(
        &mut Producer,
        &UnitType,
        &Faction,
        &TeamId,
        &GlobalTransform,
        Option<&FactoryPieces>,
        Option<&UnitAnimator>,
        Option<&crate::units::components::Homebase>,
        Has<Emerging>,
    )>,
    small_building_counts: Res<super::bookkeeping::SmallBuildingCounts>,
    piece_transforms: Query<&GlobalTransform, With<PieceIndex>>,
    unit_count: Res<super::bookkeeping::TotalUnitCount>,
    mut pending_attacks: ResMut<PendingAttacks>,
    mut ctx: SpawnContext,
    mut hex_farm: Option<ResMut<crate::map_events::hex_farm::HexFarmInbox>>,
    // `Local` so the allocation is reused across frames — production
    // completions are sparse (most ticks push nothing), but fresh Vecs on
    // every frame cost allocator churn for no gain.
    mut spawns: Local<
        Vec<(
            UnitKind,
            Faction,
            u8,
            Vec3,
            f32,
            Option<Vec3>,
            f32,
            EmergeStyle,
        )>,
    >,
) {
    let dt = time.delta_secs();
    spawns.clear();

    for (
        mut producer,
        factory_type,
        faction,
        team,
        global_tf,
        factory_pieces,
        animator,
        homebase,
        emerging,
    ) in &mut producers
    {
        // A factory still rising out of its construction site is not
        // finished yet — upstream only hands out orders on
        // `UnitFinished`, so a half-built Socket must not spam Bits.
        if emerging {
            continue;
        }
        let Some(build_time) = producer.current_build_time(&ctx.unit_registry) else {
            // Queue is empty — idle.
            producer.progress = 0.0;
            continue;
        };

        // A fold-capable homebase (the Kernel) must finish its unfold
        // before production may begin: hold progress at zero until the
        // driver reports it fully open. Units without a fold cycle
        // return `None` and pass straight through.
        if animator.is_some_and(|a| a.driver.is_open() == Some(false)) {
            continue;
        }

        let speed_mult = if homebase.is_some() {
            let buildings = small_building_counts.get(team.0) as f32;
            1.0 + buildings * KERNEL_BOOST_PER_BUILDING
        } else {
            1.0
        };
        producer.progress += dt * speed_mult;

        let factory_pos = global_tf.translation();
        // Spring's factory build step goes through `AllowUnitBuildStep`
        // with the buildee (on the factory pad) — Hex Farm rebuilds
        // sunk neighbours with it.
        if let (Some(inbox), Some(kind)) = (hex_farm.as_deref_mut(), producer.current_production()) {
            inbox.build_step(
                factory_pos,
                dt * speed_mult / build_time * ctx.unit_registry.raw_build_time(kind),
            );
        }
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
                    emit_build_ray(
                        pos,
                        pad_pos,
                        factory_pos,
                        &mut pending_attacks,
                        factory_type.0 == UnitKind::Gateway,
                    );
                    emitted_any = true;
                }
            }
        }
        if !emitted_any {
            let synthetic = factory_pos + Vec3::new(0.0, 24.0, 16.0);
            emit_build_ray(
                synthetic,
                pad_pos,
                factory_pos,
                &mut pending_attacks,
                factory_type.0 == UnitKind::Gateway,
            );
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
            // O(1) via the bookkeeping-maintained counter — the old
            // `iter().count()` scanned every unit each time a factory
            // sat at the spawn threshold.
            if unit_count.0 > 10_000 {
                // Don't busy-loop; pin progress at the threshold and
                // try again next frame.
                producer.progress = spawn_threshold;
                continue;
            }

            // Compute a rally point that's offset from the factory in
            // its forward direction so the new unit walks clear of the
            // hole once it has finished emerging. Stationary units
            // (speed == 0) get no rally point.
            //
            // Per-unit spread: a single rally point makes every Packet
            // from the same Carrier converge on the same spot and pile
            // up. Lay out points on a grid in front of the factory —
            // rows of seven, each row further away, lateral slot picked
            // from `spawn_count`. Produces a deterministic, non-piling
            // rally cloud without any RNG or allocation.
            let kind = producer.current_production().unwrap();
            let rally_point = if ctx.unit_registry.speed(kind) > 0.0 {
                let raw_forward = global_tf.forward().as_vec3();
                let forward = if raw_forward.length_squared() > 0.01 {
                    Vec3::new(raw_forward.x, 0.0, raw_forward.z).normalize_or(Vec3::Z)
                } else {
                    Vec3::Z
                };
                // Right-hand perpendicular on XZ (Y-up): (fz, 0, -fx).
                let right = Vec3::new(forward.z, 0.0, -forward.x);
                const SLOTS_PER_ROW: u32 = 7;
                const LATERAL_STEP: f32 = 20.0;
                const ROW_STEP: f32 = 40.0;
                // The first row must land beyond the factory's visual
                // silhouette, not just its gameplay footprint: the
                // Kernel's model is 128 elmos wide while its footprint
                // radius is only 32, so a fixed 60-elmo rally left
                // freshly-built units standing "inside" the base.
                let factory_radius = crate::units::assets::meshes::unit_radius(
                    factory_type.0,
                    &mut *ctx.model_cache,
                    &ctx.unit_registry,
                );
                let unit_radius = ctx.unit_registry.collision_radius(kind);
                const EXIT_MARGIN: f32 = 24.0;
                let row0_distance = (factory_radius + unit_radius + EXIT_MARGIN).max(60.0);
                let n = producer.spawn_count;
                let slot = (n % SLOTS_PER_ROW) as f32 - (SLOTS_PER_ROW as f32 - 1.0) * 0.5;
                let ring = (n / SLOTS_PER_ROW) as f32;
                let offset =
                    forward * (row0_distance + ring * ROW_STEP) + right * (slot * LATERAL_STEP);
                Some(pad_pos + offset)
            } else {
                None
            };
            producer.spawn_count = producer.spawn_count.wrapping_add(1);

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
            producer.complete_front();
        }
    }

    for (kind, faction, team, spawn_pos, target_y, rally_point, emerge_duration, style) in
        spawns.drain(..)
    {
        let entity = spawn_unit(kind, faction, team, spawn_pos, &mut ctx);
        ctx.commands.entity(entity).insert(Emerging {
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
            ctx.commands.entity(entity).insert(PendingFadeInstall);
        }
    }
}

/// Marker placed on a freshly-spawned `Fade`-style emerging unit so the
/// next-frame `install_fade_materials` system can swap each piece's
/// MeshMaterial3d for a per-unit clone before the alpha ramp starts.
#[derive(Component)]
#[component(storage = "SparseSet")]
pub struct PendingFadeInstall;

/// Host-animate Connection's `body` piece as a "hatch" — lifts
/// up by 16 elmos while the Connection is producing, drops back down
/// when idle. The upstream Network homebase (Carrier) does this in its
/// .bos via Activate/Deactivate moving a `mover` piece, but our
/// Connection's .bos has no such handler — so we drive it from the
/// host the same way `aim_weapons_system` host-drives the Pointer's
/// gunbase.
pub fn animate_connection_hatch(
    mut query: Query<(
        &Producer,
        &mut UnitAnimator,
        &crate::units::assets::animation::HatchPiece,
    )>,
) {
    const HATCH_LIFT_ELMOS: f32 = 16.0;
    const HATCH_SPEED_ELMOS_PER_SEC: f32 = 24.0;

    for (producer, mut animator, hatch) in &mut query {
        let idx = hatch.0;
        if idx >= animator.rig.target_translations.len() {
            continue;
        }
        let target_y = if producer.current_production().is_some() {
            HATCH_LIFT_ELMOS
        } else {
            0.0
        };
        let rig = &mut animator.rig;
        rig.target_translations[idx][1] = target_y;
        rig.move_speeds[idx][1] = HATCH_SPEED_ELMOS_PER_SEC;
        // Bypassing the rig primitives means no automatic dirty flag —
        // mark the rig so apply_and_drain actually pushes the hatch.
        rig.dirty = true;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Minifacs start spamming their faction's swarm unit on repeat
    /// (upstream `kp_autospam.lua`); Ports stay teleporters and
    /// homebases start idle with repeat off.
    #[test]
    fn minifacs_default_to_autospam() {
        let socket = default_production(UnitKind::Socket).unwrap();
        assert!(socket.repeat());
        assert_eq!(socket.current_production(), Some(UnitKind::Bit));
        let window = default_production(UnitKind::Window).unwrap();
        assert!(window.repeat());
        assert_eq!(window.current_production(), Some(UnitKind::Bug));
        assert!(default_production(UnitKind::Port).is_none());
        let kernel = default_production(UnitKind::Kernel).unwrap();
        assert!(!kernel.repeat());
        assert!(kernel.queue().is_empty());
    }

    /// Repeat re-appends the finished unit behind anything the player
    /// queued meanwhile, so the queue cycles instead of growing or
    /// draining; with repeat off the finished unit is dropped.
    #[test]
    fn repeat_cycles_queue() {
        let mut p = Producer::spamming(UnitKind::Bit);
        p.enqueue(UnitKind::Bit);
        p.complete_front();
        assert_eq!(p.queue().len(), 2);
        p.set_repeat(false);
        p.complete_front();
        p.complete_front();
        assert!(p.queue().is_empty());
    }
}

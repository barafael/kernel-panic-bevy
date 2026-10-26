use crate::sim::{DetFloat, sq};
use bevy::math::ops;
use std::collections::VecDeque;

use bevy::prelude::*;

use super::spawning::{
    BuildDriven, EmergeStyle, Emerging, FactoryPieces, FadeMaterials, SpawnContext,
    spawn_unit_facing,
};
use crate::interaction::movement::QueuedCommand;
use crate::net::NetId;
use crate::sim::{RngStream, SQUARE_SIZE, SimRng, SimTick};
use crate::terrain::heightmap::Heightmap;
use crate::units::assets::animation::{PieceIndex, UnitAnimator};
use crate::units::components::{Faction, TeamId, UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::spatial::{SpatialIndex, flat_dist_sq};

/// Attached to factories/homebases. Builds units from its queue.
///
/// The factory builds the front of its `queue`. When the queue is empty
/// production is idle — nothing is built until the player enqueues something.
#[derive(Component)]
#[require(FactoryOrders)]
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
    /// A build step ran this frame (`CFactory::UpdateBuild`): the window
    /// between the script's `StartBuilding` and `StopBuilding`.
    pub building: bool,
    /// Seconds since the last build step; the yard closes after
    /// `script_triggers::DEACTIVATE_DELAY` of it.
    pub idle_time: f32,
    /// The unit under construction on the pad (`CFactory::curBuild`),
    /// from its spawn until the queue pops.
    buildee: Option<Entity>,
}

/// The orders a factory gives each unit it builds (Spring's
/// `CFactoryCAI::newUnitCommands`): the move, fight, patrol and guard
/// orders the factory itself was given (`net::apply`; only factories
/// that [`UnitKind::takes_rally_orders`]). A new unit first leaves by
/// the factory's front, then carries them out in order. A patrol is
/// stored as its point; the unit patrols between where it left the
/// factory and there.
#[derive(Component, Default, Clone, Debug)]
pub struct FactoryOrders {
    pub commands: Vec<QueuedCommand>,
}

/// A unit's copy of its factory's [`FactoryOrders`], inserted with its
/// `Emerging` and handed over when it has emerged (`emerge_system`).
#[derive(Component, Clone, Debug)]
pub struct NewUnitOrders(pub Vec<QueuedCommand>);

impl Producer {
    pub fn new() -> Self {
        Self {
            progress: 0.0,
            queue: VecDeque::new(),
            unit_spawned: false,
            spawn_count: 0,
            repeat: false,
            building: false,
            idle_time: 0.0,
            buildee: None,
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

    /// Whether finished units are re-appended to the queue (Spring's
    /// `CMD_REPEAT` state, shown on the command panel's Repeat button).
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

    /// Fraction of the current build done, `0..=1` (the build bar's
    /// progress pie).
    pub fn progress_fraction(&self, registry: &UnitRegistry, factory: UnitKind) -> Option<f32> {
        self.current_build_time(registry, factory).map(|t| {
            if t > 0.0 {
                (self.progress / t).det_clamp(0.0, 1.0)
            } else {
                0.0
            }
        })
    }

    /// What is currently being built, if anything.
    pub fn current_production(&self) -> Option<UnitKind> {
        self.queue.front().copied()
    }

    /// Build time of the current production in seconds: the buildee's
    /// FBI `BuildTime` over this factory's `WorkerTime`
    /// (`CFactory::UpdateBuild` adds `workerTime / GAME_SPEED` a frame).
    fn current_build_time(&self, registry: &UnitRegistry, factory: UnitKind) -> Option<f32> {
        self.current_production()
            .map(|kind| registry.raw_build_time(kind) / registry.worker_time(factory))
    }

    /// The queued build orders. Read by the build bar's queue counts,
    /// the command panel and the AI's refill check.
    pub fn queue(&self) -> &VecDeque<UnitKind> {
        &self.queue
    }

    /// Enqueue a unit to be built. The queue is unbounded; the player
    /// can stack as many orders as they want (build bar, command panel, AI).
    pub fn enqueue(&mut self, kind: UnitKind) {
        self.queue.push_back(kind);
    }

    /// Spring `CFactoryCAI` Alt+click: put `count` × `kind` at the front
    /// of the queue. A unit already in progress keeps building (it is
    /// never swapped out mid-build), so the new orders go right behind it.
    pub fn enqueue_front(&mut self, kind: UnitKind, count: u32) {
        let at = usize::from(self.progress > 0.0 || self.unit_spawned).min(self.queue.len());
        for _ in 0..count {
            self.queue.insert(at, kind);
        }
    }

    /// Cancel every order and repeat (an owner left play: its factories
    /// stand idle for an ally to take). A unit already rising out of the
    /// pad finishes, as a right-click couldn't cancel it either.
    pub fn cancel_all(&mut self) {
        self.repeat = false;
        let mut kinds: Vec<UnitKind> = Vec::new();
        for &kind in &self.queue {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        for kind in kinds {
            self.remove_kind(kind, u32::MAX, false);
        }
    }

    /// Spring `CFactoryCAI` right-click: drop up to `count` queued `kind`
    /// orders, newest first — or oldest first with `from_front` (Alt).
    /// Cancelling the order in progress resets its build progress; one
    /// whose unit is already rising out of the pad can't be cancelled.
    pub fn remove_kind(&mut self, kind: UnitKind, count: u32, from_front: bool) {
        let locked_front = self.unit_spawned;
        let hits: Vec<usize> = self
            .queue
            .iter()
            .enumerate()
            .filter(|&(i, k)| *k == kind && !(i == 0 && locked_front))
            .map(|(i, _)| i)
            .collect();
        let mut picked: Vec<usize> = if from_front {
            hits.into_iter().take(count as usize).collect()
        } else {
            hits.into_iter().rev().take(count as usize).collect()
        };
        // Remove back to front so earlier indices stay valid.
        picked.sort_unstable_by(|a, b| b.cmp(a));
        for i in picked {
            self.queue.remove(i);
            if i == 0 {
                self.progress = 0.0;
            }
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

/// Units a factory can produce, in upstream `SIDEDATA.TDF [CANBUILD]`
/// `canbuildN` order — the order Spring lists them on the command panel.
/// Minifacs build their faction's swarm unit; Ports build nothing (they
/// fill the packet buffer). Mobile builders use
/// [`super::construction::buildings_for`] instead.
pub fn factory_roster(factory: UnitKind) -> &'static [UnitKind] {
    match factory {
        UnitKind::Kernel => &[
            UnitKind::Bit,
            UnitKind::Pointer,
            UnitKind::Byte,
            UnitKind::Assembler,
        ],
        UnitKind::Hole => &[
            UnitKind::Bug,
            UnitKind::Dos,
            UnitKind::Worm,
            UnitKind::Trojan,
        ],
        // `[carrier]` canbuild1..4. No Signal: that's the SIGTERM
        // bomber, which only a Terminal's airstrike ever creates.
        UnitKind::Carrier => &[
            UnitKind::Packet,
            UnitKind::Connection,
            UnitKind::Flow,
            UnitKind::Gateway,
        ],
        UnitKind::Socket => &[UnitKind::Bit],
        UnitKind::Window => &[UnitKind::Bug],
        _ => &[],
    }
}

/// The swarm unit a minifac spams — its single `SIDEDATA.TDF` build
/// option (upstream `AddMiniFac` / `kp_autospam.lua`). Ports are
/// teleporters: they tick the packet buffer instead of producing, so
/// they have no spam unit.
pub fn minifac_spam(minifac: UnitKind) -> Option<UnitKind> {
    if minifac.is_minifac() {
        factory_roster(minifac).first().copied()
    } else {
        None
    }
}

pub fn default_production(kind: UnitKind) -> Option<Producer> {
    if kind.is_homebase() {
        return Some(Producer::new());
    }
    // Minifacs autospam from the moment they finish building:
    // upstream `kp_autospam.lua` gives every socket `REPEAT` + `bit`
    // and every window `REPEAT` + `bug`. The player can still add to
    // or toggle the queue; production waits for `Emerging` to end,
    // mirroring the widget's `UnitFinished` hook.
    //
    // Port is a teleporter, not a factory — it tops up its team's
    // PacketBuffer every 5.5s rather than spawning units directly.
    // Connection (mobile) is likewise a teleporter — it dispatches
    // Packets from the buffer but does not build new units.
    minifac_spam(kind).map(Producer::spamming)
}

/// One unit queued for spawning this frame: its factory, kind,
/// faction, team, pad position, facing, `(exit, free spot)` waypoints,
/// build time, style, and whether the factory drives its build.
type PendingSpawn = (
    Entity,
    UnitKind,
    Faction,
    u8,
    Vec3,
    crate::sim::Heading,
    Option<(Vec3, Vec3)>,
    f32,
    EmergeStyle,
    bool,
);

/// Extra reach of the pad-blocked query beyond the product's own radius:
/// the largest footprint radius a unit standing on the pad can have.
const PAD_BLOCK_REACH: f32 = 48.0;

/// `CFactory::SendToEmptySpot`: the first free spot on a half-circle of
/// radius `4·R + 4·r` in front of the factory, scanning 100 steps from
/// straight ahead toward the right; failing that, a random spot on the
/// same arc so units don't pile up. "Free" means no unit within `1.5·r`.
fn empty_exit_spot(
    pos: Vec3,
    forward: Vec3,
    factory_radius: f32,
    unit_radius: f32,
    spatial: &SpatialIndex,
    heightmap: Option<&Heightmap>,
    rng: &mut u32,
) -> Vec3 {
    const STEPS: usize = 100;
    let search_radius = factory_radius * 4.0 + unit_radius * 4.0;
    let step = std::f32::consts::PI / (STEPS as f32 * 0.5);
    // `rightdir = frontdir × updir`: facing +Z, right is −X.
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    let (world_w, world_d) =
        heightmap.map_or((f32::INFINITY, f32::INFINITY), Heightmap::world_size);
    let in_bounds = |p: Vec3| p.x >= 0.0 && p.z >= 0.0 && p.x < world_w && p.z < world_d;
    let on_arc = |a: f32| {
        pos + forward * (search_radius * ops::cos(a)) + right * (search_radius * ops::sin(a))
    };
    // `NoSolidsExact(testPos, unit->radius · 1.5)`: nothing's collision
    // sphere within that reach. The spatial query only trims by cell,
    // so the distance test here is the real one.
    let clearance = unit_radius * 1.5;
    let free = |p: Vec3| {
        let mut taken = false;
        spatial.query_radius(p, clearance, |other| {
            let reach = clearance + other.hit_radius;
            if flat_dist_sq(other.pos, p) < reach * reach {
                taken = true;
            }
        });
        !taken
    };
    let place = |p: Vec3| heightmap.map_or(p, |hm| hm.place(p.x, p.z));
    let candidate = (0..STEPS)
        .map(|i| on_arc(i as f32 * step))
        .find(|p| in_bounds(*p) && (*p - pos).dot(forward) >= 0.0 && free(*p));
    if let Some(found) = candidate {
        return place(found);
    }
    for _ in 0..STEPS {
        let p = on_arc(crate::rng::next_f32(rng) * STEPS as f32 * step);
        if in_bounds(p) && (p - pos).dot(forward) >= 0.0 {
            return place(p);
        }
    }
    place(pos + forward * search_radius)
}

/// XZ-flatten and normalise a forward vector, +Z when degenerate.
fn flat_forward(forward: Vec3) -> Vec3 {
    let f = Vec3::new(forward.x, 0.0, forward.z);
    if f.length_squared() < 1e-6 {
        Vec3::Z
    } else {
        f.normalize()
    }
}

/// Farthest a factory's pad piece can be from its root (elmos); a piece
/// reported further away has an unpropagated (identity) transform.
const MAX_PAD_OFFSET: f32 = 256.0;

/// World position of an animated piece on a factory, by index.
///
/// Composed from the pieces' *local* `Transform`s up to the factory
/// root (whose `Transform` is world space), never from the pieces'
/// `GlobalTransform`s: those are propagated once per render frame, so a
/// frame that runs two sim ticks would read a stale pad (and a factory
/// spawned this frame an identity one) — the sim then depends on the
/// frame rate (`headless` determinism experiments).
fn piece_world_pos(
    piece_idx: Option<usize>,
    animator: Option<&UnitAnimator>,
    factory: Entity,
    factory_tf: &Transform,
    piece_transforms: &Query<(&Transform, &ChildOf), With<PieceIndex>>,
) -> Option<Vec3> {
    let idx = piece_idx?;
    let animator = animator?;
    let piece = *animator.rig.piece_entities.get(idx)?;
    let mut local = bevy::math::Affine3A::IDENTITY;
    let mut cur = piece;
    for _ in 0..64 {
        if cur == factory {
            return Some((factory_tf.compute_affine() * local).translation.into());
        }
        let (tf, parent) = piece_transforms.get(cur).ok()?;
        local = tf.compute_affine() * local;
        cur = parent.parent();
    }
    None
}

/// [`production_system`]'s random stream (the factory exit scatter).
pub struct FactoryExitRng;
impl RngStream for FactoryExitRng {
    const SALT: u64 = 0x5EED_FAC7;
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn production_system(
    time: Res<Time>,
    // A dead factory builds nothing (its buildee went with it,
    // [`kill_orphaned_buildee`]); a dead buildee ends the build
    // (`CFactory::DependentDied`).
    mut producers: Query<
        (
            Entity,
            &mut Producer,
            &UnitType,
            &Faction,
            &TeamId,
            &Transform,
            &UnitStats,
            Option<&FactoryPieces>,
            Option<&UnitAnimator>,
            Option<&crate::units::components::Homebase>,
            Has<Emerging>,
            Has<crate::units::combat::Stunned>,
            &NetId,
        ),
        Without<crate::units::combat::Dying>,
    >,
    // The buildees on the pads, whose build clock their factory drives.
    // (`Has<Emerging>` above takes no component access.)
    mut buildees: Query<(&mut Emerging, &mut BuildDriven), Without<crate::units::combat::Dying>>,
    mut bugger_off: ResMut<BuggerOffRequests>,
    small_building_counts: Res<super::bookkeeping::SmallBuildingCounts>,
    piece_transforms: Query<(&Transform, &ChildOf), With<PieceIndex>>,
    unit_counts: Res<super::bookkeeping::TeamUnitCounts>,
    spatial: Res<SpatialIndex>,
    heightmap: Option<Res<Heightmap>>,
    mut ctx: SpawnContext,
    mut hex_farm: Option<ResMut<crate::map_events::hex_farm::HexFarmInbox>>,
    // `Local` so the allocation is reused across frames — production
    // completions are sparse (most ticks push nothing), but fresh Vecs on
    // every frame cost allocator churn for no gain.
    mut spawns: Local<Vec<PendingSpawn>>,
    mut exit_rng: ResMut<SimRng<FactoryExitRng>>,
    // The factory's AutoHold, handed to the units it builds
    // (`autohold.lua`'s `inherited`: the Hole's Worms).
    autoholds: Query<&crate::units::mechanics::worm::AutoHold>,
    tick: Res<SimTick>,
    factory_orders: Query<&FactoryOrders>,
) {
    let dt = time.delta_secs();
    spawns.clear();
    // `(gs->frameNum & (UNIT_SLOWUPDATE_RATE >> 1)) == 0`: eight frames
    // on, eight off.
    let bugger_off_frame = tick.frame() & 8 == 0;

    // Unit-id order: spawned units draw their `NetId`s, and the exit
    // dice roll, factory by factory (`sim`).
    for (
        factory_entity,
        mut producer,
        factory_type,
        faction,
        team,
        factory_tf,
        stats,
        factory_pieces,
        animator,
        homebase,
        emerging,
        stunned,
        _,
    ) in producers.iter_mut().sort::<&NetId>()
    {
        // A factory still rising out of its construction site is not
        // finished yet — upstream only hands out orders on
        // `UnitFinished`, so a half-built Socket must not spam Bits.
        producer.building = false;
        if emerging {
            continue;
        }
        let Some(build_time) = producer.current_build_time(&ctx.unit_registry, factory_type.0)
        else {
            // Queue is empty — idle.
            producer.progress = 0.0;
            producer.idle_time += dt;
            continue;
        };

        // `CFactory::Update`: while the next build waits to start, the
        // factory periodically asks its own idle units to clear the
        // exit — even stunned, and whatever its yard is doing.
        let forward = flat_forward(factory_tf.forward().as_vec3());
        if !producer.unit_spawned && bugger_off_frame {
            bugger_off.push(BuggerOff::factory(
                factory_tf.translation,
                forward,
                stats.hit_radius,
                team.0,
                factory_entity,
            ));
        }

        // The buildee's clock is the build's progress (`buildProgress`
        // lives on the unit): pick up what it lost to decay while the
        // factory was stunned. A buildee that is gone — decayed away or
        // destroyed — ends the build, and the queue front starts over
        // (`CFactory::DependentDied`).
        if producer.unit_spawned
            && let Some(buildee) = producer.buildee
        {
            match buildees.get(buildee) {
                Ok((clock, _)) => {
                    producer.progress =
                        (1.0 - clock.remaining / clock.total.det_max(f32::EPSILON)) * build_time;
                }
                Err(_) => {
                    producer.unit_spawned = false;
                    producer.buildee = None;
                    producer.progress = 0.0;
                }
            }
        }

        // `CFactory::Update` starts a build only once the yard is open
        // and the script has set `INBUILDSTANCE` (the Kernel's pillars
        // unfolded, the Hole's 2.3 s fade-in, the Window's flap, the
        // Carrier's pad lift). Drivers without such a gate return `None`.
        // A build under way continues whatever the yard does
        // (`UpdateBuild` has no yard test).
        if !producer.unit_spawned && animator.is_some_and(|a| a.driver.is_open() == Some(false)) {
            producer.idle_time = 0.0;
            continue;
        }

        // A stunned factory neither starts nor advances a build
        // (`CFactory::Update` / `UpdateBuild` bail while stunned), so its
        // buildee gets no build power and decays. The yard stays open
        // (`wantClose` needs `!IsStunned()`).
        if stunned {
            producer.idle_time = 0.0;
            continue;
        }

        // The factory's own `Transform` (a root entity, so world space):
        // its `GlobalTransform` is still identity on the frame it
        // spawned, which placed a freshly spawned Socket's first Bit at
        // the map origin.
        let factory_pos = factory_tf.translation;
        // A piece's `GlobalTransform` is likewise unpropagated on that
        // frame: a pad that is nowhere near its factory is ignored.
        let pad_pos = factory_pieces
            .and_then(|fp| {
                piece_world_pos(
                    fp.pad,
                    animator,
                    factory_entity,
                    factory_tf,
                    &piece_transforms,
                )
            })
            .filter(|p| p.distance_squared(factory_pos) < MAX_PAD_OFFSET * MAX_PAD_OFFSET)
            .unwrap_or(factory_pos);
        let kind = producer.current_production().unwrap();

        // `CFactory::StartBuild` returns early while the pad is
        // `GroundBlocked`: the next unit waits until the previous one has
        // walked clear instead of spawning inside it. Only mobile units
        // can stand on the pad squares — the factory itself, and any
        // structure placed near it, never block.
        if !producer.unit_spawned {
            let product_radius = ctx.unit_registry.collision_radius(kind);
            let mut blocked = false;
            spatial.query_radius(pad_pos, product_radius + PAD_BLOCK_REACH, |other| {
                if other.entity == factory_entity
                    || other.is_flying
                    || blocked
                    || ctx.unit_registry.speed(other.kind) <= 0.0
                {
                    return;
                }
                let reach = product_radius + ctx.unit_registry.collision_radius(other.kind);
                if flat_dist_sq(other.pos, pad_pos) < reach * reach {
                    blocked = true;
                }
            });
            if blocked {
                // The periodic call above covers the front half of the
                // factory; the pad squares the port tests are cleared
                // the same way, so a parked unit can't stall the queue.
                if bugger_off_frame {
                    bugger_off.push(BuggerOff {
                        center: pad_pos,
                        radius: product_radius + SQUARE_SIZE,
                        add_footprint: false,
                        team: team.0,
                        exclude: Some(factory_entity),
                    });
                }
                producer.idle_time = 0.0;
                continue;
            }
        }

        let speed_mult = if homebase.is_some() {
            let buildings = small_building_counts.get(team.0) as f32;
            1.0 + buildings * KERNEL_BOOST_PER_BUILDING
        } else {
            1.0
        };
        producer.progress += dt * speed_mult;
        producer.building = true;
        producer.idle_time = 0.0;

        // Spring's factory build step goes through `AllowUnitBuildStep`
        // with the buildee (on the factory pad) — Hex Farm rebuilds
        // sunk neighbours with it.
        if let Some(inbox) = hex_farm.as_deref_mut() {
            inbox.build_step(
                factory_pos,
                dt * speed_mult / build_time * ctx.unit_registry.raw_build_time(kind),
            );
        }

        // The unit exists for its whole build, as the engine's buildee
        // does: it is spawned the moment its build starts, standing on
        // the pad at full height (Kernel Panic draws no nanoframe), and
        // its script's `Create()` loop tracks `BUILD_PERCENT_LEFT`. The
        // *queue* only pops once the full build_time has elapsed.
        //
        // Divergence: upstream buildees can be shot mid-build; here the
        // whole `Emerging` window doubles as spawn protection —
        // `rebuild_spatial_index` excludes `Emerging` entities. See the
        // note there before changing either side.
        if !producer.unit_spawned {
            // `CFactory::QueueBuild`: a team at its unit limit keeps the
            // order but builds nothing (`CTeam::AtUnitLimit`). O(1) via
            // the bookkeeping-maintained per-team count.
            if unit_counts.at_limit(team.0) {
                // Don't busy-loop; pin progress at zero and try again.
                producer.progress = 0.0;
                continue;
            }

            // `CFactory::SendToEmptySpot`: a waypoint just outside the
            // factory, then the first free spot on the exit arc.
            let heading = crate::sim::Heading::from_vector(forward.x, forward.z);
            let exit = if ctx.unit_registry.speed(kind) > 0.0 {
                let factory_radius = stats.hit_radius;
                let unit_radius = crate::units::assets::meshes::unit_radius(
                    kind,
                    &mut ctx.model_cache,
                    &ctx.unit_registry,
                );
                let found = empty_exit_spot(
                    factory_pos,
                    forward,
                    factory_radius,
                    unit_radius,
                    &spatial,
                    heightmap.as_deref(),
                    &mut exit_rng,
                );
                let exit = factory_pos + forward * (factory_radius + unit_radius);
                Some((
                    heightmap
                        .as_deref()
                        .map_or(exit, |hm| hm.place(exit.x, exit.z)),
                    found,
                ))
            } else {
                None
            };
            producer.spawn_count = producer.spawn_count.wrapping_add(1);

            // System units stand on the pad from the first frame; Hacker /
            // Network units materialize at-surface with an alpha ramp. The
            // buildee's build clock runs at the factory's boosted rate, so
            // its `BUILD_PERCENT_LEFT` and spawn protection end exactly
            // when the queue pops and the pad check can see it.
            //
            // From the next tick on the factory drives the buildee's
            // clock itself (`BuildDriven`, below): a changed boost keeps
            // both in step, and a stunned factory's buildee decays. A
            // build that completes on its spawn tick runs its own clock.
            let style = faction.emerge_style();
            spawns.push((
                factory_entity,
                kind,
                *faction,
                team.0,
                pad_pos,
                heading,
                exit,
                build_time / speed_mult,
                style,
                producer.progress < build_time,
            ));
            producer.unit_spawned = true;
        } else if let Some((mut clock, mut driven)) =
            producer.buildee.and_then(|b| buildees.get_mut(b).ok())
        {
            // In the buildee's own seconds at the current rate.
            clock.total = build_time / speed_mult;
            clock.remaining = (build_time - producer.progress).det_max(0.0) / speed_mult;
            driven.unpowered = 0.0;
        }

        if producer.progress >= build_time {
            producer.progress -= build_time;
            producer.unit_spawned = false;
            producer.buildee = None;
            producer.complete_front();
        }
    }

    for (factory, kind, faction, team, spawn_pos, heading, exit, emerge_duration, style, driven) in
        spawns.drain(..)
    {
        let entity = spawn_unit_facing(kind, faction, team, spawn_pos, heading, &mut ctx);
        // Inserted before `init_autohold`'s `Without<AutoHold>` pass
        // sees the unit, so the factory's state wins over the default.
        if kind.has_autohold()
            && let Ok(hold) = autoholds.get(factory)
        {
            ctx.commands.entity(entity).insert(*hold);
        }
        ctx.commands.entity(entity).insert(Emerging {
            target_y: spawn_pos.y,
            remaining: emerge_duration,
            total: emerge_duration,
            rally_point: exit.map(|(e, _)| e),
            rally_then: exit.map(|(_, found)| found),
            style,
        });
        // The factory's orders ride along until the unit has emerged
        // (`CFactory::AssignBuildeeOrders`); a unit that doesn't leave
        // the factory (no exit) takes none.
        if exit.is_some()
            && let Ok(orders) = factory_orders.get(factory)
            && !orders.commands.is_empty()
        {
            ctx.commands
                .entity(entity)
                .insert(NewUnitOrders(orders.commands.clone()));
        }
        if driven {
            ctx.commands.entity(entity).insert(BuildDriven::default());
            if let Ok((_, mut producer, ..)) = producers.get_mut(factory) {
                producer.buildee = Some(entity);
            }
        }
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

/// One `CGameHelper::BuggerOff` call: allied idle mobile units near
/// `center` are asked to step clear of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BuggerOff {
    pub center: Vec3,
    /// Clearance radius around `center` (elmos), as the CAI receives it.
    pub radius: f32,
    /// `BuggerOffRectangle`: each unit's own footprint radius is added
    /// to `radius`.
    pub add_footprint: bool,
    pub team: u8,
    /// The unit the call comes from, which is never moved by it.
    pub exclude: Option<Entity>,
}

impl BuggerOff {
    /// `CFactory::Update`'s periodic call: a circle of half the
    /// factory's radius, centred half a radius out of its front
    /// (`boOffset = boRadius = radius / 2`), widened by a square in
    /// `BuggerOffImpl`.
    pub fn factory(pos: Vec3, forward: Vec3, radius: f32, team: u8, factory: Entity) -> Self {
        Self {
            center: pos + forward * (radius * 0.5),
            radius: radius * 0.5 + SQUARE_SIZE,
            add_footprint: false,
            team,
            exclude: Some(factory),
        }
    }

    /// `CGameHelper::BuggerOffRectangle` over a build site of `half`
    /// extents: the rectangle's half-diagonal plus a square, plus each
    /// unit's footprint.
    pub fn rectangle(site: Vec3, half: Vec2, team: u8, builder: Entity) -> Self {
        Self {
            center: site,
            radius: half.length() + SQUARE_SIZE,
            add_footprint: true,
            team,
            exclude: Some(builder),
        }
    }
}

/// This tick's [`BuggerOff`] calls, drained by [`apply_bugger_off`].
#[derive(Resource, Default, Debug)]
pub struct BuggerOffRequests(Vec<BuggerOff>);

impl BuggerOffRequests {
    pub fn push(&mut self, request: BuggerOff) {
        self.0.push(request);
    }
}

/// Units that run their CAI at all: not being built, dying or stunned.
/// Of those, `CMobileCAI::NonMoving` only runs for the ones with no
/// order (`!Orders::busy`).
type RunsCai = (
    Without<Emerging>,
    Without<crate::units::combat::Dying>,
    Without<crate::units::combat::Stunned>,
);

/// Reach of the candidate scan beyond a request's radius: the largest
/// unit's `1.4·r` plus its footprint.
const BUGGER_OFF_SCAN_MARGIN: f32 = 96.0;

/// `CMobileCAI::BuggerOff` + `NonMoving`: every allied idle mobile unit
/// within `radius + 1.4·r` of a request's centre gets a move order to
/// `radius + r` straight out from it — the nearest way out of the
/// circle — turning in 45° steps when that point is off the map.
/// Spring pushes an internal move onto the empty queue; here the idle
/// unit just gets a plain move.
#[allow(clippy::too_many_arguments)]
pub fn apply_bugger_off(
    mut requests: ResMut<BuggerOffRequests>,
    spatial: Res<SpatialIndex>,
    units: Query<&Transform, RunsCai>,
    orders: crate::interaction::movement::Orders,
    registry: Res<UnitRegistry>,
    heightmap: Option<Res<Heightmap>>,
    mut commands: Commands,
    mut ordered: Local<Vec<Entity>>,
    alliances: Res<crate::units::teams::Alliances>,
) {
    if requests.0.is_empty() {
        return;
    }
    ordered.clear();
    let (world_w, world_d) = heightmap
        .as_deref()
        .map_or((f32::INFINITY, f32::INFINITY), Heightmap::world_size);
    let in_map = |p: Vec3| p.x >= 0.0 && p.z >= 0.0 && p.x < world_w && p.z < world_d;
    for request in requests.0.drain(..) {
        let mut candidates: Vec<(Entity, UnitKind, f32)> = Vec::new();
        spatial.query_radius(
            request.center,
            request.radius + BUGGER_OFF_SCAN_MARGIN,
            |other| {
                // `BuggerOffImpl`: allied units only, never enemies.
                if alliances.friendly(other.team, request.team)
                    && Some(other.entity) != request.exclude
                    && !other.is_flying
                    && other.hp_positive
                    && registry.speed(other.kind) > 0.0
                {
                    candidates.push((other.entity, other.kind, other.hit_radius));
                }
            },
        );
        for (entity, kind, radius) in candidates {
            if ordered.contains(&entity) {
                continue;
            }
            let Ok(tf) = units.get(entity) else {
                continue;
            };
            if orders.busy(entity) {
                continue;
            }
            let clearance = if request.add_footprint {
                let half = registry.footprint_elmos(kind) * 0.5 + Vec2::splat(SQUARE_SIZE);
                request.radius + half.length()
            } else {
                request.radius
            };
            let pos = tf.translation;
            if flat_dist_sq(pos, request.center) >= sq(clearance + radius * 1.4) {
                continue;
            }
            let mut dir = (pos - request.center)
                .with_y(0.0)
                .try_normalize()
                .unwrap_or(Vec3::X);
            let mut target = request.center + dir * (clearance + radius);
            for _ in 0..8 {
                if in_map(target) {
                    break;
                }
                dir = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4) * dir;
                target = request.center + dir * (clearance + radius);
            }
            if !in_map(target) {
                continue;
            }
            let target = heightmap
                .as_deref()
                .map_or(target, |hm| hm.place(target.x, target.z));
            crate::interaction::replace_order(
                &mut commands.entity(entity),
                crate::interaction::movement::QueuedCommand::Move(target),
            );
            ordered.push(entity);
        }
    }
}

/// `CFactory::KillUnit`: a factory's death takes its unfinished buildee
/// with it, as a silent (reclaimed) kill — no death blast. An observer,
/// so a factory despawned on the tick it dies still counts.
pub fn kill_orphaned_buildee(
    add: On<Add<crate::units::combat::Dying>>,
    producers: Query<&Producer>,
    building: Query<(), With<Emerging>>,
    mut commands: Commands,
) {
    if let Ok(producer) = producers.get(add.entity)
        && let Some(buildee) = producer.buildee
        && building.contains(buildee)
    {
        commands.entity(buildee).try_despawn();
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
        // Only (re)issue the move when the target flips: `tick_rig`
        // carries an in-flight move to its target and flags the rig
        // dirty itself while it moves. Re-arming the speed every tick
        // made the resting hatch re-dirty the whole rig each frame.
        if animator.rig.target_translations[idx][1] == target_y {
            continue;
        }
        let rig = &mut animator.rig;
        rig.target_translations[idx][1] = target_y;
        rig.move_speeds[idx][1] = HATCH_SPEED_ELMOS_PER_SEC;
        // Bypassing the rig primitives means no automatic dirty flag —
        // mark the rig so apply_and_drain actually pushes the hatch.
        rig.dirty = true;
    }
}

/// Run after `production_system`: for each entity tagged
/// `PendingFadeInstall`, dress its model pieces in the alpha=0 variant
/// of their shared material ([`ModelMaterials::fade`](crate::units::assets::model_fade::ModelMaterials::fade),
/// shared by every unit fading at that step) and install a
/// `FadeMaterials` component holding the swap records so
/// `emerge_system` can both fade them in and revert them when emergence
/// completes.
pub fn install_fade_materials(
    mut commands: Commands,
    pending: Query<(Entity, &Children), With<PendingFadeInstall>>,
    mut model: crate::units::assets::model_fade::ModelMaterials,
) {
    for (entity, children) in &pending {
        let overrides = model.fade(children, 0.0, &mut commands);
        commands
            .entity(entity)
            .insert(FadeMaterials { step: 0, overrides })
            .remove::<PendingFadeInstall>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::spatial::SpatialEntry;

    fn standing(entity: Entity, pos: Vec3) -> SpatialEntry {
        SpatialEntry {
            entity,
            pos,
            hit_radius: 16.0,
            mid_y: 0.0,
            up: Vec3::Y,
            team: 0,
            kind: UnitKind::Bit,
            hp_positive: true,
            is_flying: false,
            cloaked: false,
            detected_by: 0,
            feature: false,
        }
    }

    /// `SendToEmptySpot`: straight ahead on the 4R+4r arc when nothing
    /// stands there; the next step sweeps toward the factory's right (−X
    /// for a factory facing +Z) when a unit does — a unit in the same
    /// 256-elmo spatial cell but 60 elmos away must not count.
    #[test]
    fn exit_spot_sweeps_right_past_an_occupied_spot_only() {
        let pos = Vec3::new(1000.0, 0.0, 1000.0);
        let forward = Vec3::Z;
        let (big_r, small_r) = (64.0, 16.0);
        let arc = 4.0 * big_r + 4.0 * small_r;
        let mut rng = 1u32;

        let mut empty = SpatialIndex::default();
        let spot = empty_exit_spot(pos, forward, big_r, small_r, &empty, None, &mut rng);
        assert!((spot - (pos + forward * arc)).length() < 1e-3, "{spot}");

        // A unit 60 elmos beside the straight-ahead spot shares its cell
        // but does not block it.
        let e = Entity::from_raw_u32(7).unwrap();
        empty.insert_for_test(standing(e, pos + forward * arc + Vec3::X * 60.0));
        let spot = empty_exit_spot(pos, forward, big_r, small_r, &empty, None, &mut rng);
        assert!((spot - (pos + forward * arc)).length() < 1e-3, "{spot}");

        // Standing on it: the scan moves one step toward the right.
        let mut taken = SpatialIndex::default();
        taken.insert_for_test(standing(e, pos + forward * arc));
        let spot = empty_exit_spot(pos, forward, big_r, small_r, &taken, None, &mut rng);
        assert!(spot.x < pos.x, "swept toward −X: {spot}");
        assert!((spot - pos).dot(forward) > 0.0);
        assert!(((spot - pos).length() - arc).abs() < 1e-2);
    }

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

    /// Upstream `[CANBUILD]` order; the Signal bomber is never buildable.
    #[test]
    fn factory_rosters_follow_sidedata() {
        assert_eq!(
            factory_roster(UnitKind::Kernel),
            &[
                UnitKind::Bit,
                UnitKind::Pointer,
                UnitKind::Byte,
                UnitKind::Assembler
            ]
        );
        assert_eq!(
            factory_roster(UnitKind::Hole),
            &[
                UnitKind::Bug,
                UnitKind::Dos,
                UnitKind::Worm,
                UnitKind::Trojan
            ]
        );
        assert_eq!(
            factory_roster(UnitKind::Carrier),
            &[
                UnitKind::Packet,
                UnitKind::Connection,
                UnitKind::Flow,
                UnitKind::Gateway
            ]
        );
        assert!(factory_roster(UnitKind::Port).is_empty());
        for f in [UnitKind::Kernel, UnitKind::Hole, UnitKind::Carrier] {
            assert!(!factory_roster(f).contains(&UnitKind::Signal));
        }
    }

    /// Right-click removes newest-first (Alt: oldest-first); cancelling
    /// the order in progress resets its progress; Alt+click queues at the
    /// front but behind a build already under way.
    #[test]
    fn spring_queue_edits() {
        let mut p = Producer::new();
        for k in [UnitKind::Bit, UnitKind::Byte, UnitKind::Bit, UnitKind::Bit] {
            p.enqueue(k);
        }
        p.progress = 1.0;
        p.remove_kind(UnitKind::Bit, 1, false);
        assert_eq!(
            p.queue().iter().copied().collect::<Vec<_>>(),
            vec![UnitKind::Bit, UnitKind::Byte, UnitKind::Bit]
        );
        assert_eq!(p.progress, 1.0);
        p.remove_kind(UnitKind::Bit, 1, true);
        assert_eq!(p.current_production(), Some(UnitKind::Byte));
        assert_eq!(p.progress, 0.0, "cancelling the front resets progress");
        p.progress = 0.5;
        p.enqueue_front(UnitKind::Pointer, 2);
        assert_eq!(
            p.queue().iter().copied().collect::<Vec<_>>(),
            vec![
                UnitKind::Byte,
                UnitKind::Pointer,
                UnitKind::Pointer,
                UnitKind::Bit
            ]
        );
        let mut idle = Producer::new();
        idle.enqueue(UnitKind::Bit);
        idle.enqueue_front(UnitKind::Byte, 1);
        assert_eq!(idle.current_production(), Some(UnitKind::Byte));
        // A unit already rising out of the pad can't be cancelled.
        let mut rising = Producer::new();
        rising.enqueue(UnitKind::Bit);
        rising.unit_spawned = true;
        rising.remove_kind(UnitKind::Bit, 5, false);
        assert_eq!(rising.queue().len(), 1);
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

    // --- factory build power, BuggerOff, buildee death ---------------------

    use crate::interaction::movement::{CommandQueue, MoveTarget, QueuedCommand};
    use crate::units::combat::{Dying, Stunned};
    use bevy::ecs::system::RunSystemOnce;
    use spring_tdf::{UnitDef, UnitDefs};
    use std::time::Duration;

    /// Kernel (`WorkerTime=128`) building Bits (`BuildTime=240`).
    fn registry() -> UnitRegistry {
        let mut defs = UnitDefs::default();
        defs.units.insert(
            "kernel".into(),
            UnitDef {
                worker_time: 128.0,
                footprint_x: 8.0,
                footprint_z: 8.0,
                ..UnitDef::default()
            },
        );
        defs.units.insert(
            "bit".into(),
            UnitDef {
                build_time: 240.0,
                footprint_x: 1.0,
                footprint_z: 1.0,
                max_velocity: 3.0,
                can_move: true,
                ..UnitDef::default()
            },
        );
        UnitRegistry::for_test(defs)
    }

    fn world() -> World {
        let mut world = World::new();
        world.insert_resource(registry());
        world.init_resource::<SpatialIndex>();
        world.init_resource::<crate::units::teams::Alliances>();
        world.init_resource::<BuggerOffRequests>();
        world.init_resource::<super::super::bookkeeping::TeamUnitCounts>();
        world.init_resource::<super::super::bookkeeping::SmallBuildingCounts>();
        world.init_resource::<Assets<Mesh>>();
        world.init_resource::<Assets<StandardMaterial>>();
        world.init_resource::<Assets<Image>>();
        world.init_resource::<crate::units::assets::meshes::S3OModelCache>();
        world.init_resource::<crate::units::content::weapons::WeaponRegistry>();
        world.insert_resource(Time::<()>::default());
        crate::sim::test_support::init(&mut world);
        world
    }

    fn bit_entry(entity: Entity, pos: Vec3, team: u8) -> SpatialEntry {
        SpatialEntry {
            team,
            ..standing(entity, pos)
        }
    }

    /// #57 / #27 for factories: the buildee's clock is the build; a
    /// stunned factory adds no build power (`CFactory::UpdateBuild`) and
    /// its buildee decays, and the factory picks the loss back up.
    #[test]
    fn stunned_factory_stalls_and_its_buildee_decays() {
        let mut world = world();
        // 240 points at 128 a second.
        let build_time = 240.0 / 128.0;
        let buildee = world
            .spawn((
                UnitType(UnitKind::Bit),
                Emerging {
                    target_y: 0.0,
                    remaining: build_time,
                    total: build_time,
                    rally_point: None,
                    rally_then: None,
                    style: EmergeStyle::Rise,
                },
                BuildDriven::default(),
            ))
            .id();
        let mut producer = Producer::new();
        producer.enqueue(UnitKind::Bit);
        producer.unit_spawned = true;
        producer.buildee = Some(buildee);
        let registry = registry();
        let factory = world
            .spawn((
                producer,
                UnitType(UnitKind::Kernel),
                Faction::System,
                TeamId(0),
                Transform::from_xyz(500.0, 0.0, 500.0),
                UnitStats::from_registry(UnitKind::Kernel, &registry, 64.0, 0.0),
            ))
            .id();
        let remaining = |world: &World| world.get::<Emerging>(buildee).unwrap().remaining;
        // Six sim ticks (0.2 s).
        let ticks = |world: &mut World| {
            for _ in 0..6 {
                world
                    .resource_mut::<Time>()
                    .advance_by(Duration::from_secs_f64(1.0 / 30.0));
                world.run_system_once(production_system).unwrap();
                world
                    .run_system_once(super::super::construction::decay_nanoframes)
                    .unwrap();
            }
        };

        for _ in 0..3 {
            ticks(&mut world);
        }
        assert!((remaining(&world) - (build_time - 0.6)).abs() < 1e-3);

        world.entity_mut(factory).insert(Stunned);
        let before = remaining(&world);
        ticks(&mut world);
        assert!(remaining(&world) > before, "the buildee decays");
        let decayed = remaining(&world);

        world.entity_mut(factory).remove::<Stunned>();
        ticks(&mut world);
        assert!(
            (remaining(&world) - (decayed - 0.2)).abs() < 1e-3,
            "resumes from the decayed progress"
        );
    }

    /// `autohold.lua`'s `inherited`: a Worm leaves its Security Hole
    /// with the Hole's AutoHold setting.
    #[test]
    fn a_worm_inherits_the_holes_autohold() {
        use crate::units::mechanics::worm::AutoHold;
        for setting in [false, true] {
            let mut world = world();
            let registry = registry();
            let mut producer = Producer::new();
            producer.enqueue(UnitKind::Worm);
            world.spawn((
                producer,
                UnitType(UnitKind::Hole),
                Faction::Hacker,
                TeamId(0),
                Transform::from_xyz(500.0, 0.0, 500.0),
                UnitStats::from_registry(UnitKind::Hole, &registry, 64.0, 0.0),
                AutoHold(setting),
            ));
            for _ in 0..4 {
                world
                    .resource_mut::<Time>()
                    .advance_by(Duration::from_secs_f64(1.0 / 30.0));
                world.run_system_once(production_system).unwrap();
            }
            let worm = world
                .query_filtered::<(Entity, &UnitType), With<Emerging>>()
                .iter(&world)
                .find(|(_, u)| u.0 == UnitKind::Worm)
                .map(|(e, _)| e)
                .expect("the Hole builds the Worm");
            assert_eq!(world.get::<AutoHold>(worm), Some(&AutoHold(setting)));
        }
    }

    /// A dead factory starts nothing from its corpse, and a dead
    /// buildee ends its factory's build (`CFactory::DependentDied`).
    #[test]
    fn dead_factories_and_buildees_build_nothing() {
        let mut world = world();
        let registry = registry();
        let factory = |world: &mut World, producer: Producer| {
            world
                .spawn((
                    producer,
                    UnitType(UnitKind::Kernel),
                    Faction::System,
                    TeamId(0),
                    Transform::from_xyz(500.0, 0.0, 500.0),
                    UnitStats::from_registry(UnitKind::Kernel, &registry, 64.0, 0.0),
                ))
                .id()
        };
        let mut queued = Producer::new();
        queued.enqueue(UnitKind::Bit);
        let corpse = factory(&mut world, queued);
        world.entity_mut(corpse).insert(Dying { timer: 1.0 });

        let dead_buildee = world
            .spawn((
                UnitType(UnitKind::Bit),
                Emerging {
                    target_y: 0.0,
                    remaining: 1.0,
                    total: 1.875,
                    rally_point: None,
                    rally_then: None,
                    style: EmergeStyle::Rise,
                },
                BuildDriven::default(),
                Dying { timer: 1.0 },
            ))
            .id();
        let mut building = Producer::new();
        building.enqueue(UnitKind::Bit);
        building.unit_spawned = true;
        building.buildee = Some(dead_buildee);
        let live = factory(&mut world, building);

        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f64(1.0 / 30.0));
        world.run_system_once(production_system).unwrap();
        let corpse_producer = world.get::<Producer>(corpse).unwrap();
        assert!(!corpse_producer.unit_spawned && corpse_producer.buildee.is_none());
        assert_eq!(world.get::<Emerging>(dead_buildee).unwrap().remaining, 1.0);
        assert_ne!(
            world.get::<Producer>(live).unwrap().buildee,
            Some(dead_buildee)
        );
    }

    /// `CFactory::KillUnit`: the factory's death takes its buildee.
    #[test]
    fn factory_death_kills_its_buildee() {
        let mut world = World::new();
        world.add_observer(kill_orphaned_buildee);
        let buildee = world
            .spawn(Emerging {
                target_y: 0.0,
                remaining: 1.0,
                total: 1.0,
                rally_point: None,
                rally_then: None,
                style: EmergeStyle::Rise,
            })
            .id();
        let mut producer = Producer::new();
        producer.buildee = Some(buildee);
        let factory = world.spawn(producer).id();
        world.entity_mut(factory).insert(Dying { timer: 0.0 });
        world.flush();
        assert!(world.get_entity(buildee).is_err());
    }

    /// #14: a factory waiting to start asks its idle units off the exit
    /// (`CFactory::Update` → `BuggerOff`) — not busy ones, not enemies.
    #[test]
    fn factory_buggers_off_idle_allies_only() {
        let mut world = world();
        let pos = Vec3::new(500.0, 0.0, 500.0);
        let center = pos + Vec3::Z * 32.0;
        let idle = world.spawn(Transform::from_translation(center)).id();
        let busy = world
            .spawn((Transform::from_translation(center), MoveTarget(Vec3::ZERO)))
            .id();
        let queued = world
            .spawn((
                Transform::from_translation(center),
                CommandQueue {
                    commands: vec![QueuedCommand::Move(Vec3::ZERO)],
                },
            ))
            .id();
        let enemy = world.spawn(Transform::from_translation(center)).id();
        {
            let mut spatial = world.resource_mut::<SpatialIndex>();
            for (e, team) in [(idle, 0), (busy, 0), (queued, 0), (enemy, 1)] {
                spatial.insert_for_test(bit_entry(e, center, team));
            }
        }
        let factory = world.spawn_empty().id();
        world
            .resource_mut::<BuggerOffRequests>()
            .push(BuggerOff::factory(pos, Vec3::Z, 64.0, 0, factory));
        world.run_system_once(apply_bugger_off).unwrap();

        let goal = world
            .get::<MoveTarget>(idle)
            .expect("idle unit moves off")
            .0;
        assert!(goal.xz().distance(center.xz()) >= 32.0 + SQUARE_SIZE);
        assert_eq!(world.get::<MoveTarget>(busy).unwrap().0, Vec3::ZERO);
        assert!(world.get::<MoveTarget>(queued).is_none());
        assert!(world.get::<MoveTarget>(enemy).is_none());
        assert!(world.resource::<BuggerOffRequests>().0.is_empty());
    }
}

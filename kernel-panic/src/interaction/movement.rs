use std::collections::HashMap;

use bevy::prelude::*;

use spring_pathfinding::{
    BlockMask, ComponentLabels, Path, PathSearch, SearchScratch, SearchStatus, SpeedMap,
};

use super::selection::Selected;
use crate::sim::SQUARE_SIZE;
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{
    AttackGroundOrder, AttackTargetOrder, CHASE_REPATH_DISTANCE, Dying, ForcedTarget,
};
use crate::units::components::{UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

pub use super::ground_move::{GroundMover, ground_collision_system, movement_system};

/// Dedicated gizmo config for command-line overlays so the dashed path
/// renders thinner than the default 2-px gizmo width used elsewhere.
#[derive(Default, Reflect, GizmoConfigGroup)]
pub struct CommandLineGizmos;

/// When present, the unit will move toward this world position.
#[derive(Component)]
pub struct MoveTarget(pub Vec3);

/// The per-unit movement-state components every spawned unit carries,
/// seeded from its spawn pose. Shared by `spawn_unit` and the headless
/// movement harness so both build movers identically.
pub(crate) fn ground_mover_components(
    kind: UnitKind,
    registry: &UnitRegistry,
    entity: Entity,
    transform: &Transform,
) -> impl Bundle {
    GroundMover::new(
        kind,
        registry,
        &UnitStats::from_registry(kind, registry, 0.0),
    )
    .seeded(entity, transform)
}

/// Marks an active attack-move order. While it is present AND the unit
/// has an `AimTarget` (an in-range hostile), movement halts so the unit
/// stops and fights; it resumes marching when the threat clears.
#[derive(Component)]
pub struct AttackMoveActive;

/// Player-issued order to guard (follow and protect) another unit.
/// [`guard_follow_system`] trails the target at `GUARD_DISTANCE`; while
/// close the guard holds position and the normal auto-attack path engages
/// anything that comes into weapon range.
#[derive(Component, Clone, Copy)]
pub struct GuardTarget(pub Entity);

/// How far behind the guarded unit a guard trails (elmos, beyond the
/// target's own footprint).
const GUARD_DISTANCE: f32 = 48.0;

/// Height of a unit's root above the ground: `spawn_unit` lifts models
/// authored around their centre so they rest on the terrain; every
/// ground-following write keeps that offset.
#[derive(Component, Default, Clone, Copy, Debug)]
pub struct GroundLift(pub f32);

/// A computed path the unit follows waypoint-by-waypoint.
#[derive(Component, Clone, Debug)]
pub struct MovePath {
    pub waypoints: Vec<Vec3>,
    /// Index of the current waypoint (`currWayPoint`); `>= len` means
    /// the leg is complete.
    pub current: usize,
    /// The goal this path was searched for. A `MoveTarget` that differs
    /// makes the movement system search a new path, following this one
    /// until it arrives (Spring's `nextPathId` swap).
    pub goal: Vec3,
    /// `false` when the goal is unreachable and the path ends at the
    /// closest reachable point (QTPFS partial path): reaching that end
    /// fails the order instead of arriving.
    pub reached_goal: bool,
    /// [`NavGridSet::revision`] the path was checked against.
    pub revision: u64,
}

impl MovePath {
    /// A path through `waypoints` toward `goal`.
    pub fn new(waypoints: Vec<Vec3>, goal: Vec3) -> Self {
        Self {
            waypoints,
            current: 0,
            goal,
            reached_goal: true,
            revision: 0,
        }
    }

    /// An empty path: tells the movement system the leg is over (it
    /// promotes the next queued command, or stops).
    pub fn finished() -> Self {
        Self::new(Vec::new(), Vec3::ZERO)
    }
}

/// A queued command waiting to become the unit's active order.
/// Consumed in FIFO order when the current order completes.
#[derive(Clone, Copy, Debug)]
pub enum QueuedCommand {
    Move(Vec3),
    /// Walk to `site`, then erect a building of `kind` there. Issued by
    /// the placement flow: a command-panel build button arms
    /// `PlacementMode`, the placing click commits a `BuildAt` to every
    /// selected constructor.
    BuildAt {
        kind: UnitKind,
        site: Vec3,
    },
    /// Walk to `target`, then return to the origin, repeating. Issued by
    /// the patrol (P) order.
    Patrol(Vec3),
    /// Walk to `target`, then fight anything hostile on the way (Spring
    /// `CMD.FIGHT`). Issued by the AI; no player keybind.
    AttackMove(Vec3),
    /// Walk to `pos`, then attack the unit `target` (shift-chained
    /// right-click attack). `pos` is the target's position at enqueue
    /// time, used only for command-line drawing; the live chase targets
    /// the unit itself via `AttackTargetOrder`.
    AttackUnit {
        target: Entity,
        pos: Vec3,
    },
    /// Walk to `pos`, then fire at the ground there (shift-chained
    /// attack-ground, A + click). Promotion inserts an
    /// `AttackGroundOrder`; `attack_ground_system` owns the approach
    /// (it issues its own range-boundary `MoveTarget`), so like
    /// [`QueuedCommand::AttackUnit`] this leg has no path of its own.
    AttackGround(Vec3),
}

impl QueuedCommand {
    pub fn position(&self) -> Vec3 {
        match self {
            QueuedCommand::Move(p) | QueuedCommand::Patrol(p) | QueuedCommand::AttackMove(p) => *p,
            QueuedCommand::AttackUnit { pos, .. } => *pos,
            QueuedCommand::AttackGround(pos) => *pos,
            QueuedCommand::BuildAt { site, .. } => *site,
        }
    }
}

/// FIFO queue of follow-up commands. When the unit finishes its current
/// order (e.g. reaches its `MoveTarget`), the next command is popped and
/// promoted to the active order.
#[derive(Component, Default)]
pub struct CommandQueue {
    pub commands: Vec<QueuedCommand>,
}

impl CommandQueue {
    pub fn push(&mut self, cmd: QueuedCommand) {
        self.commands.push(cmd);
    }
}

/// One pathfinding grid plus the max-slope cap (dy/dx ratio) it was
/// built for. Grids with smaller caps flag more cells as impassable.
pub struct NavBucket {
    pub max_slope: f32,
    pub speed_map: SpeedMap,
    /// Connected components of the bare terrain (no structure mask),
    /// built with the map off the main thread so the first path search
    /// on a large map doesn't label a million cells in one tick. The
    /// path service starts each mover class from a copy and brings it
    /// up to date through the structure change ring.
    pub terrain_labels: Option<std::sync::Arc<ComponentLabels>>,
}

/// A set of pathfinding grids, one per distinct `MaxSlope` in the unit
/// roster. Units with a tighter slope cap pick a grid whose cells they
/// can actually traverse; units with looser caps share the most permissive
/// grid. Sorted ascending by `max_slope`.
///
/// Upstream Spring does the same thing via per-`MoveDef` grids
/// (`rts/Sim/MoveTypes/MoveDefHandler.cpp`); see plan.md §Gameplay Bugs
/// "Movement ignores per-unit `MaxSlope`" for the full motivation.
#[derive(Resource)]
pub struct NavGridSet {
    /// Identity of this grid set: a new map gets a new value, so
    /// per-map caches (a mover's component labels, searches in flight)
    /// can tell a replaced grid from a revised one.
    pub epoch: u64,
    pub buckets: Vec<NavBucket>,
    /// Bumped whenever the structure layer changes, so paths made
    /// before can be re-checked (QTPFS `PathUpdated`).
    pub revision: u64,
    /// The squares each recent revision changed, `[x0, z0, x1, z1]`
    /// inclusive and already dilated by the widest mover footprint,
    /// oldest first. A path only re-checks the segments crossing the
    /// area changed since its own revision (QTPFS `PathUpdated` tests
    /// paths against the dirty node rectangle, not the whole map).
    pub changes: std::collections::VecDeque<(u64, [i32; 4])>,
    /// Squares blocked by buildings, and per-mover-class path masks.
    pub structures: super::structures::StructureLayer,
}

/// Revisions remembered for partial re-checks; older paths re-check
/// in full.
const REMEMBERED_CHANGES: usize = 64;

impl Default for NavGridSet {
    fn default() -> Self {
        static NEXT_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            epoch: NEXT_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            buckets: Vec::new(),
            revision: 0,
            changes: Default::default(),
            structures: Default::default(),
        }
    }
}

impl NavGridSet {
    /// A new revision that changed the squares in `bbox` (see
    /// [`Self::changed_since`]).
    pub fn bump(&mut self, bbox: [i32; 4]) {
        self.revision += 1;
        self.changes.push_back((self.revision, bbox));
        if self.changes.len() > REMEMBERED_CHANGES {
            self.changes.pop_front();
        }
    }

    /// The square rectangle changed since revision `since`, or `None`
    /// when that reaches further back than remembered (treat all
    /// squares as changed).
    pub fn changed_since(&self, since: u64) -> Option<[i32; 4]> {
        let (oldest, _) = *self.changes.front()?;
        if oldest > since + 1 {
            return None;
        }
        let mut area: Option<[i32; 4]> = None;
        for &(rev, b) in self.changes.iter().rev() {
            if rev <= since {
                break;
            }
            area = Some(area.map_or(b, |a| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            }));
        }
        area
    }

    /// The square rectangles changed by each revision after `since`,
    /// oldest first, or `None` when that reaches further back than
    /// remembered.
    pub fn changes_since_each(&self, since: u64) -> Option<impl Iterator<Item = [i32; 4]> + '_> {
        let (oldest, _) = *self.changes.front()?;
        if oldest > since + 1 {
            return None;
        }
        Some(
            self.changes
                .iter()
                .filter(move |(rev, _)| *rev > since)
                .map(|(_, b)| *b),
        )
    }

    /// Does the segment `a → b` (world XZ) touch the square rectangle?
    pub fn segment_touches(bbox: [i32; 4], a: Vec2, b: Vec2) -> bool {
        let sq = |v: f32| (v / SQUARE_SIZE).floor() as i32;
        sq(a.x.min(b.x)) <= bbox[2]
            && sq(a.x.max(b.x)) >= bbox[0]
            && sq(a.y.min(b.y)) <= bbox[3]
            && sq(a.y.max(b.y)) >= bbox[1]
    }

    /// Pick the tightest bucket whose cap ≥ `cap`. If none qualifies
    /// (the unit needs a looser grid than any we built), return the
    /// loosest bucket. `None` only with no grids built (no map loaded —
    /// the map loader always pushes at least the default 45° bucket).
    pub fn bucket(&self, cap: f32) -> Option<&NavBucket> {
        self.bucket_index(cap).map(|i| &self.buckets[i])
    }

    /// Slack in the cap comparison: a bucket built for this unit's
    /// cap must never be skipped over a rounding difference (that once
    /// sent every mobile unit to the all-passable building grid).
    const CAP_TOLERANCE: f32 = 1e-4;

    /// Can a ground unit with slope cap `cap` stand on the heightmap
    /// square under `(x, z)`? Off-map squares are not. With no grids
    /// built everything is.
    pub fn passable(&self, cap: f32, x: f32, z: f32) -> bool {
        let Some(map) = self.speed_map(cap) else {
            return true;
        };
        let (cx, cz) = ((x / SQUARE_SIZE).floor(), (z / SQUARE_SIZE).floor());
        cx >= 0.0 && cz >= 0.0 && map.get(cx as u32, cz as u32) > 0.0
    }

    /// Index into `buckets` of the bucket for slope cap `cap` (see
    /// [`Self::bucket`]).
    pub fn bucket_index(&self, cap: f32) -> Option<usize> {
        self.buckets
            .iter()
            .position(|b| b.max_slope >= cap - Self::CAP_TOLERANCE)
            .or_else(|| self.buckets.len().checked_sub(1))
    }

    /// The grid of the bucket for slope cap `cap`.
    fn speed_map(&self, cap: f32) -> Option<&SpeedMap> {
        self.bucket(cap).map(|b| &b.speed_map)
    }

    /// Does a structure block any square of a `xsizeh`-footprint mover
    /// centred at `(x, z)` (`MoveDef::TestMovePositionForObjects`)?
    pub fn footprint_blocked(&self, x: f32, z: f32, xsizeh: i32, crush_strength: f32) -> bool {
        self.structures.footprint_blocked(
            (x / SQUARE_SIZE).floor() as i32,
            (z / SQUARE_SIZE).floor() as i32,
            xsizeh,
            super::structures::crushes_features(crush_strength),
        )
    }

    /// `SquareIsBlocked(..) & BLOCK_STRUCTURE` for one square.
    pub fn structure_square(&self, x: i32, z: i32, crush_strength: f32) -> bool {
        self.structures
            .square_blocked(x, z, super::structures::crushes_features(crush_strength))
    }

    /// The structure mask a mover class paths against, if any.
    pub fn block_mask(&self, xsizeh: i32, crush_strength: f32) -> Option<&BlockMask> {
        self.structures
            .mask(xsizeh, super::structures::crushes_features(crush_strength))
    }

    /// `MoveDef::DoRawSearch`: can a mover drive straight `a → b`?
    /// Every square the segment crosses must be passable terrain and
    /// open for the mover's footprint.
    pub fn line_clear(&self, cap: f32, xsizeh: i32, crush_strength: f32, a: Vec2, b: Vec2) -> bool {
        let Some(map) = self.speed_map(cap) else {
            return true;
        };
        spring_pathfinding::line_clear(
            a.to_array(),
            b.to_array(),
            map,
            self.block_mask(xsizeh, crush_strength),
            None,
        )
    }

    /// Spring's slope encoding (`1 - cos(angle)`) of the square under
    /// `pos` as the bucket for cap `cap` rated it, or `None` where that
    /// bucket has it impassable. Recovered from the stored speed
    /// `1/(1 + slope·slopeMod)`.
    pub fn square_slope(&self, cap: f32, pos: Vec2) -> Option<f32> {
        let b = self.bucket(cap)?;
        if pos.x < 0.0 || pos.y < 0.0 {
            return None;
        }
        let s = b
            .speed_map
            .get((pos.x / SQUARE_SIZE) as u32, (pos.y / SQUARE_SIZE) as u32);
        (s > 0.0).then(|| {
            (1.0 / s - 1.0).max(0.0) / spring_pathfinding::slope_mod_from_max_slope(b.max_slope)
        })
    }

    /// `slopeMod` of the bucket for cap `cap`.
    pub fn slope_mod(&self, cap: f32) -> f32 {
        self.bucket(cap).map_or(0.0, |b| {
            spring_pathfinding::slope_mod_from_max_slope(b.max_slope)
        })
    }
}

/// Spring `MOVEINFO.TDF` HeatMapping: a shared congestion grid the
/// pathfinder reads as extra cost. Walking ground units deposit heat on
/// the cell they stand in; later paths bend around hot cells, so
/// columns marching the same line fan out instead of braiding into a
/// single rut. Upstream keeps per-class decay rates; a single shared
/// grid decays at the most persistent class (LIGHT, `HeatMod=0.10`)
/// while MEDIUM/HEAVY keep their much higher deposits.
///
/// Fidelity note: only Spring's legacy HAPFS pathfinder reads heat
/// (`Path/HAPFS/PathHeatMap`); KP runs QTPFS (`pathFinderSystem=1`),
/// which ignores it. It is kept as KP's authored data because it does
/// not fight the Spring movement port: it only biases *where* new paths
/// run (never blocks), the corner-cutting look-ahead
/// (`CanSetNextWayPoint`'s raw search) ignores it, and the headless
/// harness measures identical group-move numbers with and without it.
#[derive(Resource)]
pub struct PathHeat(pub spring_pathfinding::HeatMap);

/// Full-grid decay runs in steps of this many seconds (a plain
/// multiply at `retention^period` — same curve, memcpy cost).
const HEAT_DECAY_PERIOD: f32 = 0.5;

/// Deposit + decay pass for [`PathHeat`]. Runs before
/// `movement_system` so freshly-issued paths see this tick's
/// congestion. No-op when no map (and therefore no grid) is loaded.
#[allow(clippy::type_complexity)]
pub fn update_path_heat(
    time: Res<Time>,
    movers: Query<(&GlobalTransform, &UnitType, &UnitStats), With<MovePath>>,
    registry: Res<UnitRegistry>,
    mut heat: Option<ResMut<PathHeat>>,
    mut decay_timer: Local<f32>,
) {
    let Some(heat) = heat.as_deref_mut() else {
        return;
    };
    let dt = time.delta_secs();
    for (gtf, unit, stats) in &movers {
        // Flyers never touch the nav grid — they leave no trail.
        if stats.can_fly || stats.speed <= 0.0 {
            continue;
        }
        let produced = registry.heat_produced(unit.0);
        let pos = gtf.translation();
        heat.0.add_heat([pos.x, pos.z], produced * dt);
    }

    *decay_timer += dt;
    if *decay_timer >= HEAT_DECAY_PERIOD {
        *decay_timer = 0.0;
        heat.0
            .decay(registry.shared_heat_retention().powf(HEAT_DECAY_PERIOD));
    }
}

/// How far above the sampled ground height to draw command-line gizmos
/// so they don't z-fight with the terrain.
const GIZMO_LIFT: f32 = 1.5;

/// An order leg is done: promote the next queued command to the active
/// order, or — queue empty — drop the order entirely. Shared by the
/// ground walker ([`movement_system`]) and the aircraft
/// (`air_movement::hover_air_system`). `translation` is where the unit
/// is now (the patrol return point).
pub(crate) fn promote_next_command(
    commands: &mut Commands,
    entity: Entity,
    translation: Vec3,
    mut queue: Option<&mut CommandQueue>,
) -> Option<Vec3> {
    let next = queue
        .as_deref_mut()
        .filter(|q| !q.commands.is_empty())
        .map(|q| q.commands.remove(0));
    let goal = next.and_then(|c| match c {
        QueuedCommand::AttackUnit { .. } | QueuedCommand::AttackGround(_) => None,
        c => Some(c.position()),
    });
    let mut ec = commands.entity(entity);
    match next {
        Some(QueuedCommand::Patrol(pos)) => {
            // Patrol shuttles between two points forever: the unit
            // just arrived at `pos`'s predecessor, so record where it
            // is now and re-queue that as the opposing waypoint.
            // Mirrors upstream CommandAI.cpp pushing `owner->pos` as
            // the first patrol point when none is queued.
            super::install_command(&mut ec, QueuedCommand::Patrol(pos));
            if let Some(queue) = queue {
                let origin = Vec3::new(translation.x, 0.0, translation.z);
                queue.commands.push(QueuedCommand::Patrol(origin));
            }
        }
        Some(cmd) => super::install_command(&mut ec, cmd),
        None => {
            ec.remove::<MoveTarget>()
                .remove::<CommandQueue>()
                .remove::<crate::units::lifecycle::construction::PendingBuild>()
                .remove::<AttackMoveActive>();
        }
    }
    goal
}

/// Guard orders: trail [`GuardTarget`] at `GUARD_DISTANCE` beyond the
/// target's footprint. Farther than that, chase (repathing only when the
/// current path endpoint lags the target, sharing [`CHASE_REPATH_DISTANCE`]
/// with the attack-chase); inside it, settle and let the auto-attack path
/// engage anything in range. When the target dies the guard stands down.
pub fn guard_follow_system(
    mut commands: Commands,
    guards: Query<(Entity, &GlobalTransform, &UnitStats, &GuardTarget), Without<Dying>>,
    targets: Query<(&GlobalTransform, &UnitStats), Without<Dying>>,
    move_path_q: Query<&MovePath>,
) {
    for (entity, gtf, stats, guard) in &guards {
        // Buildings have nobody to trail.
        if stats.speed <= 0.0 {
            continue;
        }
        let Ok((target_gtf, target_stats)) = targets.get(guard.0) else {
            // Target gone: stand down and clear movement.
            commands
                .entity(entity)
                .remove::<GuardTarget>()
                .remove::<MoveTarget>()
                .remove::<MovePath>();
            continue;
        };

        let target_pos = target_gtf.translation();
        let keep = target_stats.radius + GUARD_DISTANCE;
        let dist = gtf.translation().distance(target_pos);

        if dist > keep {
            // Trail the target; repath when there's no path yet or the
            // path endpoint lags the target's current position.
            let stale = move_path_q.get(entity).map_or(true, |p| {
                p.waypoints
                    .last()
                    .is_none_or(|w| w.distance(target_pos) > CHASE_REPATH_DISTANCE)
            });
            if stale {
                // The old path is followed until the new one is ready.
                commands.entity(entity).insert(MoveTarget(target_pos));
            }
        } else {
            commands
                .entity(entity)
                .remove::<MoveTarget>()
                .remove::<MovePath>();
        }
    }
}

/// Rotate `from` (normalized, XZ plane) toward `to` by at most `max_turn`
/// radians. If `to` is nearly zero we keep the current heading.
pub fn rotate_toward_xz(from: Vec3, to: Vec3, max_turn: f32) -> Vec3 {
    let to_len_sq = to.length_squared();
    if to_len_sq < 1e-6 {
        return from;
    }
    let to_n = to / to_len_sq.sqrt();
    let dot = from.dot(to_n).clamp(-1.0, 1.0);
    let angle = dot.acos();
    if angle <= max_turn || angle < 1e-4 {
        return to_n;
    }
    // Signed turn direction: y component of `from × to_n` gives us the
    // left/right sense in the XZ plane (Y is up, right-handed).
    let cross_y = from.x * to_n.z - from.z * to_n.x;
    let sign = if cross_y >= 0.0 { -1.0 } else { 1.0 };
    let rot = Quat::from_axis_angle(Vec3::Y, sign * max_turn);
    (rot * from).normalize()
}

/// Keep every ground unit on the terrain surface (plus its
/// [`GroundLift`]) once per sim frame, in both directions: mobile units
/// follow the ground down too (Hex Farm sinking terrain used to leave
/// idle units hovering). Structures are only pushed up — Spring blocks
/// terrain changes under them (`blockHeightChanges`). Flyers
/// (`air_movement`) and units still emerging from a factory are left
/// alone.
#[allow(clippy::type_complexity)]
pub fn ground_clamp_system(
    heightmap: Option<Res<Heightmap>>,
    mut units: Query<
        (&UnitStats, &mut Transform, Option<&GroundLift>),
        Without<crate::units::lifecycle::spawning::Emerging>,
    >,
) {
    let Some(heightmap) = heightmap else {
        return;
    };
    for (stats, mut transform, lift) in &mut units {
        if stats.can_fly {
            continue;
        }
        let ground = heightmap.sample(transform.translation.x, transform.translation.z)
            + lift.map_or(0.0, |l| l.0);
        let y = transform.translation.y;
        if y < ground || (stats.speed > 0.0 && y != ground) {
            transform.translation.y = ground;
        }
    }
}

/// Structures stand upright, keeping their yaw: Spring sets `upright`
/// for every unit without a `MoveDef` that doesn't fly
/// (`UnitDef.cpp:608`). Ground units are oriented by the movement
/// system every frame, moving or idle; flyers by `air_movement`.
#[allow(clippy::type_complexity)]
pub fn orient_stationary_to_terrain(
    mut q: Query<(&mut Transform, &UnitStats), Changed<Transform>>,
) {
    for (mut transform, stats) in &mut q {
        if stats.can_fly || stats.speed > 0.0 {
            continue;
        }
        let forward = transform.forward().as_vec3();
        let f = Vec3::new(forward.x, 0.0, forward.z);
        let f = if f.length_squared() < 1e-6 {
            -Vec3::Z
        } else {
            f.normalize()
        };
        let target = Transform::default().looking_to(f, Vec3::Y).rotation;
        if transform.rotation != target {
            transform.rotation = target;
        }
    }
}

/// Whether a unit's orders have it moving, as its script and deploy
/// state see it: a move order that is not paused by a fight order
/// engaging (`CMobileCAI::ExecuteFight` stops the unit to shoot, which
/// is the script's `StopMoving`; `movement_system` holds it on the same
/// condition).
pub fn moving_for_script(has_order: bool, attack_move: bool, aiming: bool) -> bool {
    has_order && !(attack_move && aiming)
}

/// Outcome of one path search.
pub(crate) enum PathOutcome {
    /// Follow this path. When the goal is unreachable it ends at the
    /// closest reachable point (`reached_goal == false`) — QTPFS hands
    /// out such partial paths and the unit fails the order on arrival
    /// there (`CGroundMoveType::CanSetNextWayPoint`'s `lastWaypoint`).
    Route(MovePath),
    /// No route from here at all (source enclosed).
    Unreachable,
}

/// One mover's path request: the search inputs it was issued with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PathRequest {
    pub kind: UnitKind,
    pub xsizeh: i32,
    pub crush_strength: f32,
    pub from: Vec3,
    pub to: Vec3,
}

/// Start a search through the nav bucket matching the unit's `MaxSlope`,
/// against the structure mask of its MoveDef footprint and crush
/// strength, with that grid's [`ComponentLabels`] if the caller has
/// them (see [`nav_component_labels`]). `Err` carries an outcome that
/// needed no search: `None` when nothing could be decided (no nav grid
/// yet).
pub(crate) fn begin_path_search(
    scratch: &mut SearchScratch,
    nav_set: Option<&NavGridSet>,
    unit_registry: &UnitRegistry,
    labels: Option<&ComponentLabels>,
    req: &PathRequest,
) -> Result<PathSearch, Option<PathOutcome>> {
    let Some(nav) = nav_set else {
        return Err(None);
    };
    let Some(speed_map) = nav.speed_map(unit_registry.max_slope_ratio(req.kind)) else {
        return Err(None);
    };
    let mask = nav.block_mask(req.xsizeh, req.crush_strength);
    let (src, dst) = ([req.from.x, req.from.z], [req.to.x, req.to.z]);
    match scratch.begin_search_labelled(speed_map, mask, labels, src, dst) {
        Ok(search) => Ok(search),
        Err(path) => Err(Some(path_outcome(path, req.to, nav.revision))),
    }
}

/// Expand up to `max_pops` nodes of a search begun by
/// [`begin_path_search`]; `None` while it is still running.
pub(crate) fn step_path_search(
    scratch: &mut SearchScratch,
    search: &mut PathSearch,
    nav_set: Option<&NavGridSet>,
    unit_registry: &UnitRegistry,
    req: &PathRequest,
    heat: Option<&spring_pathfinding::HeatMap>,
    max_pops: usize,
) -> Option<Option<PathOutcome>> {
    let nav = nav_set?;
    let speed_map = nav.speed_map(unit_registry.max_slope_ratio(req.kind))?;
    let mask = nav.block_mask(req.xsizeh, req.crush_strength);
    match scratch.step(search, speed_map, mask, heat, max_pops) {
        SearchStatus::Running => None,
        SearchStatus::Done(path) => Some(Some(path_outcome(path, req.to, nav.revision))),
    }
}

/// The connectivity a request is searched under: its nav bucket and
/// mask class, keyed for a labels cache, and the labels' content.
pub(crate) fn nav_component_labels(
    nav: &NavGridSet,
    unit_registry: &UnitRegistry,
    req: &PathRequest,
) -> Option<((usize, i32, bool), ComponentLabels)> {
    let (key, speed_map, mask) = nav_class(nav, unit_registry, req)?;
    Some((key, ComponentLabels::build(speed_map, mask)))
}

/// The nav bucket and mask a request searches under, keyed as
/// `(bucket, xsizeh, crushes)`.
// The triple is a nav-bucket cache key plus its two maps, used once.
#[allow(clippy::type_complexity)]
pub(crate) fn nav_class<'a>(
    nav: &'a NavGridSet,
    unit_registry: &UnitRegistry,
    req: &PathRequest,
) -> Option<((usize, i32, bool), &'a SpeedMap, Option<&'a BlockMask>)> {
    let cap = unit_registry.max_slope_ratio(req.kind);
    let bucket = nav.bucket_index(cap)?;
    let crushes = super::structures::crushes_features(req.crush_strength);
    let mask = nav.structures.mask(req.xsizeh, crushes);
    Some((
        (bucket, req.xsizeh, crushes),
        &nav.buckets[bucket].speed_map,
        mask,
    ))
}

fn path_outcome(path: Option<Path>, to: Vec3, revision: u64) -> PathOutcome {
    let Some(path) = path else {
        return PathOutcome::Unreachable;
    };
    let waypoints: Vec<Vec3> = path
        .points
        .iter()
        .map(|p| Vec3::new(p[0], 0.0, p[1]))
        .collect();
    PathOutcome::Route(MovePath {
        // Point 0 is the start position itself.
        current: 1.min(waypoints.len().saturating_sub(1)),
        waypoints,
        goal: to,
        reached_goal: path.reached_goal,
        revision,
    })
}

/// Dash-pattern segment lengths (long dash, gap, short dot, gap), in elmos.
/// Drawn back-to-back they form a repeating `-.-.` run.
const DASH_PATTERN: [(f32, bool); 4] = [(16.0, true), (6.0, false), (4.0, true), (6.0, false)];

fn sample_at_ground(x: f32, z: f32, heightmap: Option<&Heightmap>) -> Vec3 {
    heightmap.map_or(Vec3::new(x, 0.0, z), |h| h.place(x, z)) + Vec3::Y * GIZMO_LIFT
}

/// Orientation that lays a gizmo circle flat on the ground:
/// `Quat::from_rotation_arc(Vec3::Z, Vec3::Y)`, a −90° turn about X.
const GROUND_RING: Quat = Quat::from_xyzw(
    -std::f32::consts::FRAC_1_SQRT_2,
    0.0,
    0.0,
    std::f32::consts::FRAC_1_SQRT_2,
);

/// Segments per order ring. A 6-elmo disc needs no more than this to
/// read as round at any zoom; the default 32 costs 5× the lines.
const RING_RESOLUTION: u32 = 12;

/// One selected unit's dashed order line from its current waypoint on,
/// kept between frames. Walking the dash pattern samples the heightmap
/// four times per 32 elmos of path, so for a long path this is hundreds
/// of samples and lines per unit per frame; the path itself only changes
/// on a repath or when a waypoint is reached, while the unit's own
/// movement only affects the lead-in segment (unit → current waypoint),
/// which is walked live every frame.
pub(crate) struct CachedDashes {
    key: DashKey,
    /// The current waypoint on the ground (where the lead-in ends).
    first: Vec3,
    /// The last waypoint on the ground (where the ring goes).
    end: Vec3,
    dashes: Vec<(Vec3, Vec3)>,
    /// Touched this frame; stale entries (deselected / arrived units)
    /// are dropped at the end of the frame.
    seen: bool,
}

/// What the cached dashes depend on: the waypoint index, count, nav
/// revision, and the first and last remaining waypoints.
pub(crate) type DashKey = (usize, usize, u64, Vec3, Vec3);

/// Draw each selected unit's order overlay in Spring's per-command colors:
/// the active path polyline plus a disc at the destination, follow-up queue
/// segments, and — for unit-targeted orders — a line and ring on the
/// attack / guard target themselves. Mimics Spring's command-overlay
/// palette: move green, fight/attack red, patrol blue, guard white.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn draw_selected_command_lines(
    mut gizmos: Gizmos<CommandLineGizmos>,
    query: Query<
        (
            Entity,
            &Transform,
            Option<&MoveTarget>,
            Option<&MovePath>,
            Option<&CommandQueue>,
            Option<&AttackMoveActive>,
            Option<&AttackGroundOrder>,
            Option<&AttackTargetOrder>,
            Option<&GuardTarget>,
            Option<&ForcedTarget>,
        ),
        With<Selected>,
    >,
    targets: Query<&GlobalTransform>,
    heightmap: Option<Res<Heightmap>>,
    mut cache: Local<HashMap<Entity, CachedDashes>>,
    mut points: Local<Vec<Vec3>>,
) {
    const MOVE_COLOR: Color = Color::srgb(0.2, 1.0, 0.3);
    const BUILD_COLOR: Color = Color::srgb(1.0, 0.8, 0.2);
    const PATROL_COLOR: Color = Color::srgb(0.4, 0.7, 1.0);
    const FIGHT_COLOR: Color = Color::srgb(1.0, 0.3, 0.25);
    const GUARD_COLOR: Color = Color::srgb(0.85, 0.95, 1.0);
    const TARGET_COLOR: Color = Color::srgb(1.0, 0.65, 0.2);
    const DISC_RADIUS: f32 = 6.0;

    // The cached dashes hug the terrain: a new or reshaped heightmap
    // (Hex Farm) invalidates them all.
    if heightmap.as_ref().is_some_and(|h| h.is_changed()) {
        cache.clear();
    }
    let hm = heightmap.as_deref();

    for (
        entity,
        transform,
        target,
        path,
        queue,
        attack_move,
        attack_ground,
        attack_target,
        guard,
        forced,
    ) in &query
    {
        let unit_pt = sample_at_ground(transform.translation.x, transform.translation.z, hm);

        // Unit-targeted orders: line + ring on the target unit itself,
        // drawn regardless of whether the unit is currently moving.
        if let Some(atk) = attack_target
            && let Ok(t_gtf) = targets.get(atk.target)
        {
            let tp = sample_at_ground(t_gtf.translation().x, t_gtf.translation().z, hm);
            draw_dashed_polyline(&mut gizmos, &[unit_pt, tp], FIGHT_COLOR, hm);
            gizmos
                .circle(Isometry3d::new(tp, GROUND_RING), DISC_RADIUS, FIGHT_COLOR)
                .resolution(RING_RESOLUTION);
        }
        if let Some(guard) = guard
            && let Ok(t_gtf) = targets.get(guard.0)
        {
            let tp = sample_at_ground(t_gtf.translation().x, t_gtf.translation().z, hm);
            draw_dashed_polyline(&mut gizmos, &[unit_pt, tp], GUARD_COLOR, hm);
            gizmos
                .circle(Isometry3d::new(tp, GROUND_RING), DISC_RADIUS, GUARD_COLOR)
                .resolution(RING_RESOLUTION);
        }

        // Manual target designation (T): amber line + ring on the forced
        // target, independent of any movement order.
        if let Some(forced) = forced
            && let Ok(t_gtf) = targets.get(forced.0)
        {
            let tp = sample_at_ground(t_gtf.translation().x, t_gtf.translation().z, hm);
            draw_dashed_polyline(&mut gizmos, &[unit_pt, tp], TARGET_COLOR, hm);
            gizmos
                .circle(Isometry3d::new(tp, GROUND_RING), DISC_RADIUS, TARGET_COLOR)
                .resolution(RING_RESOLUTION);
        }

        let Some(current) = target else {
            continue;
        };

        // Active-order line color: fight/attack red, patrol blue, guard
        // white — detected from the order markers riding along with the
        // plain `MoveTarget`.
        let has_patrol_queued = queue.is_some_and(|q| {
            q.commands
                .iter()
                .any(|c| matches!(c, QueuedCommand::Patrol(_)))
        });
        let active_color = if attack_move.is_some() || attack_ground.is_some() {
            FIGHT_COLOR
        } else if has_patrol_queued {
            PATROL_COLOR
        } else if guard.is_some() {
            GUARD_COLOR
        } else {
            MOVE_COLOR
        };

        // The polyline runs unit → remaining waypoints. If no path exists
        // yet (freshly-issued order), fall back to unit → current target
        // so the player sees something.
        let end = if let Some(path) = path
            && path.current < path.waypoints.len()
        {
            let remaining = &path.waypoints[path.current..];
            let key = (
                path.current,
                path.waypoints.len(),
                path.revision,
                remaining[0],
                remaining[remaining.len() - 1],
            );
            if !cache.get(&entity).is_some_and(|c| c.key == key) {
                points.clear();
                points.extend(remaining.iter().map(|wp| sample_at_ground(wp.x, wp.z, hm)));
                let mut dashes = cache.remove(&entity).map_or_else(Vec::new, |c| c.dashes);
                dashes.clear();
                walk_dashes(&points, hm, |a, b| dashes.push((a, b)));
                cache.insert(
                    entity,
                    CachedDashes {
                        key,
                        first: points[0],
                        end: points[points.len() - 1],
                        dashes,
                        seen: false,
                    },
                );
            }
            let c = cache.get_mut(&entity).expect("inserted above");
            c.seen = true;
            // Lead-in from the unit to its current waypoint: the only
            // part that moves with the unit, so it is walked live.
            draw_dashed_polyline(&mut gizmos, &[unit_pt, c.first], active_color, hm);
            for &(a, b) in &c.dashes {
                gizmos.line(a, b, active_color);
            }
            c.end
        } else {
            let to = sample_at_ground(current.0.x, current.0.z, hm);
            draw_dashed_polyline(&mut gizmos, &[unit_pt, to], active_color, hm);
            to
        };

        // Ring at the final point of the active order.
        gizmos
            .circle(Isometry3d::new(end, GROUND_RING), DISC_RADIUS, active_color)
            .resolution(RING_RESOLUTION);

        // Queued follow-ups: straight dashed segments between successive
        // targets plus a ring at each.
        if let Some(queue) = queue {
            let mut prev = end;
            for cmd in &queue.commands {
                let pos = cmd.position();
                let to = sample_at_ground(pos.x, pos.z, hm);
                let color = match cmd {
                    QueuedCommand::Move(_) => MOVE_COLOR,
                    QueuedCommand::BuildAt { .. } => BUILD_COLOR,
                    QueuedCommand::Patrol(_) => PATROL_COLOR,
                    QueuedCommand::AttackMove(_) => FIGHT_COLOR,
                    QueuedCommand::AttackUnit { .. } => FIGHT_COLOR,
                    QueuedCommand::AttackGround(_) => FIGHT_COLOR,
                };
                draw_dashed_polyline(&mut gizmos, &[prev, to], color, hm);
                gizmos
                    .circle(Isometry3d::new(to, GROUND_RING), DISC_RADIUS, color)
                    .resolution(RING_RESOLUTION);
                prev = to;
            }
        }
    }

    // Drop the dashes of units that are no longer selected or moving.
    cache.retain(|_, c| std::mem::take(&mut c.seen));
}

/// Draw a polyline in world space as a repeating `-.-.` dash pattern,
/// re-sampling Y from the terrain at each dash endpoint so the line hugs
/// the ground instead of cutting straight through hills.
fn draw_dashed_polyline(
    gizmos: &mut Gizmos<CommandLineGizmos>,
    points: &[Vec3],
    color: Color,
    heightmap: Option<&Heightmap>,
) {
    walk_dashes(points, heightmap, |a, b| gizmos.line(a, b, color));
}

/// Walk `points` in the `-.-.` dash pattern, handing every visible dash
/// (ground-sampled at both ends) to `emit`.
fn walk_dashes(points: &[Vec3], heightmap: Option<&Heightmap>, mut emit: impl FnMut(Vec3, Vec3)) {
    // Pattern walker: `cursor` is how far into the current pattern entry
    // we've consumed. Persisting across segments keeps the `-.-.` rhythm
    // continuous through waypoint corners.
    let mut pattern_idx = 0usize;
    let mut cursor = 0.0f32;

    for pair in points.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let dx = end.x - start.x;
        let dz = end.z - start.z;
        let seg_len = (dx * dx + dz * dz).sqrt();
        if seg_len < 1e-4 {
            continue;
        }
        let step_x = dx / seg_len;
        let step_z = dz / seg_len;

        let mut t = 0.0f32;
        while t < seg_len {
            let (entry_len, visible) = DASH_PATTERN[pattern_idx];
            let remaining = entry_len - cursor;
            let advance = remaining.min(seg_len - t);

            if visible {
                let a = sample_at_ground(start.x + step_x * t, start.z + step_z * t, heightmap);
                let b = sample_at_ground(
                    start.x + step_x * (t + advance),
                    start.z + step_z * (t + advance),
                    heightmap,
                );
                emit(a, b);
            }

            t += advance;
            cursor += advance;
            if cursor >= entry_len - 1e-4 {
                cursor = 0.0;
                pattern_idx = (pattern_idx + 1) % DASH_PATTERN.len();
            }
        }
    }
}

#[cfg(test)]
mod tilt_tests {
    use super::super::ground_move::attitude;
    use crate::sim::heading_of;
    use bevy::prelude::*;

    const EPS: f32 = 1e-4;

    /// 30° slope descending along +Z: surface normal tilts toward +Z.
    fn downhill_normal() -> Vec3 {
        Vec3::new(
            0.0,
            30.0_f32.to_radians().cos(),
            30.0_f32.to_radians().sin(),
        )
    }

    #[test]
    fn flat_terrain_is_pure_yaw() {
        let rot = attitude(heading_of(Vec2::Y), Vec3::Y);
        assert!((rot * -Vec3::Z - Vec3::Z).length() < EPS);
        assert!((rot * Vec3::Y - Vec3::Y).length() < EPS);
    }

    /// `UpdateDirVectors`: up is the ground normal, and the front stays
    /// in the surface plane.
    #[test]
    fn up_matches_normal_and_front_lies_in_the_plane() {
        let n = downhill_normal();
        for dir in [Vec2::Y, Vec2::X, Vec2::new(1.0, 1.0).normalize(), -Vec2::Y] {
            let rot = attitude(heading_of(dir), n);
            assert!((rot * Vec3::Y - n).length() < EPS);
            assert!((rot * -Vec3::Z).dot(n).abs() < EPS);
        }
    }

    /// Heading down the fall line: pitched down, no roll.
    #[test]
    fn fall_line_pitches_without_roll() {
        let n = downhill_normal();
        let rot = attitude(heading_of(Vec2::Y), n);
        assert!((rot * -Vec3::Z).y < 0.0);
        assert!((rot * Vec3::X).y.abs() < EPS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_area_unions_recent_revisions_and_forgets_old_ones() {
        let mut nav = NavGridSet::default();
        assert_eq!(nav.changed_since(0), None, "no changes remembered");
        nav.bump([10, 10, 12, 12]);
        nav.bump([20, 5, 21, 6]);
        assert_eq!(nav.changed_since(2), None, "nothing since the newest");
        assert_eq!(nav.changed_since(1), Some([20, 5, 21, 6]));
        assert_eq!(nav.changed_since(0), Some([10, 5, 21, 12]));
        for i in 0..REMEMBERED_CHANGES as i32 {
            nav.bump([i, i, i, i]);
        }
        assert_eq!(nav.changed_since(1), None, "older than the ring");
        assert!(NavGridSet::segment_touches(
            [10, 10, 12, 12],
            Vec2::new(0.0, 88.0),
            Vec2::new(200.0, 88.0)
        ));
        assert!(!NavGridSet::segment_touches(
            [10, 10, 12, 12],
            Vec2::new(0.0, 60.0),
            Vec2::new(200.0, 60.0)
        ));
    }
    use spring_pathfinding::SpeedMap;

    fn bucket_with(cap: f32) -> NavBucket {
        // Tiny 2×2 speed map — we only care about the `max_slope` value
        // for selection tests, not the grid contents.
        NavBucket {
            max_slope: cap,
            speed_map: SpeedMap::uniform(2, 2, 1.0),
            terrain_labels: None,
        }
    }

    #[test]
    fn bucket_picks_first_cap_at_or_above_unit_cap() {
        let mut set = NavGridSet::default();
        assert!(set.bucket(0.5).is_none());
        set.buckets
            .extend([bucket_with(0.2), bucket_with(0.5), bucket_with(1.0)]);
        let cap_of = |cap| set.bucket(cap).unwrap().max_slope;

        // Bit with MaxSlope=21° (tan ≈ 0.384) gets the 0.5 bucket —
        // tightest grid whose cap still covers what the unit can climb.
        assert_eq!(cap_of(0.384), 0.5);
        // Exact match picks that bucket.
        assert_eq!(cap_of(0.5), 0.5);
        // Byte with MaxSlope=60° (tan ≈ 1.73) exceeds every cap; fall
        // back to the loosest so it's not falsely blocked.
        assert_eq!(cap_of(1.73), 1.0);
        // A cap below the tightest still resolves to the tightest.
        assert_eq!(cap_of(0.1), 0.2);
    }

    /// The bucket built for a unit's own cap is picked even when the
    /// stored cap differs by float noise (the map loader once rounded
    /// caps to four decimals, which rounded LIGHT's 0.412215 *down* and
    /// sent every mobile unit to the all-passable 1.0 grid).
    #[test]
    fn bucket_tolerates_rounding_of_its_own_cap() {
        let mut set = NavGridSet::default();
        let light = spring_pathfinding::max_slope_from_degrees(36.0);
        set.buckets
            .extend([bucket_with((light * 1e4).round() / 1e4), bucket_with(1.0)]);
        assert!((set.bucket(light).unwrap().max_slope - light).abs() < 1e-3);
    }

    /// A shift-queued attack-ground promotes to a live
    /// `AttackGroundOrder` (not a plain move), with no path goal of
    /// its own — `attack_ground_system` steers the approach.
    #[test]
    fn promoted_attack_ground_inserts_the_order() {
        let mut world = World::new();
        let entity = world.spawn_empty().id();
        let mut queue = CommandQueue::default();
        queue.push(QueuedCommand::AttackGround(Vec3::new(40.0, 0.0, 40.0)));
        world.entity_mut(entity).insert(queue);

        let mut deferred = bevy::ecs::world::CommandQueue::default();
        let mut taken = world
            .entity_mut(entity)
            .take::<CommandQueue>()
            .expect("queue present");
        let goal = {
            let mut commands = Commands::new(&mut deferred, &world);
            promote_next_command(&mut commands, entity, Vec3::ZERO, Some(&mut taken))
        };
        deferred.apply(&mut world);

        assert_eq!(goal, None, "attack-ground legs have no path goal");
        let order = world
            .get::<crate::units::combat::AttackGroundOrder>(entity)
            .expect("order promoted");
        assert_eq!(order.pos, Vec3::new(40.0, 0.0, 40.0));
        assert!(world.get::<MoveTarget>(entity).is_none());
        assert!(
            world.get::<CommandQueue>(entity).is_none(),
            "empty queue is dropped after promotion"
        );
    }
}

#[cfg(test)]
mod heat_tests {
    use super::*;
    use crate::units::components::TeamId;
    use bevy::ecs::system::RunSystemOnce;
    use spring_pathfinding::HeatMap;
    use std::time::Duration;

    /// Walking ground units deposit heat at their cell; a full decay
    /// step multiplies the grid down by `retention^period`. Flyers
    /// leave no trail.
    #[test]
    fn walkers_deposit_heat_and_decay_shrinks_it() {
        let mut world = World::new();
        world.init_resource::<Time>();
        world.insert_resource(UnitRegistry::empty());
        world.insert_resource(PathHeat(HeatMap::new(8, 8)));

        let walker = world
            .spawn((
                UnitType(UnitKind::Bit),
                UnitStats {
                    radius: 12.0,
                    hit_radius: 20.0,
                    speed: 90.0,
                    acc_rate: 0.03,
                    dec_rate: 0.067,
                    turn_rate: 3.0,
                    can_fly: false,
                    no_chase_vtol: true,
                },
                TeamId(0),
                GlobalTransform::from_xyz(32.0, 0.0, 8.0),
                MovePath::new(vec![Vec3::new(500.0, 0.0, 8.0)], Vec3::new(500.0, 0.0, 8.0)),
            ))
            .id();
        let flyer = world
            .spawn((
                UnitType(UnitKind::Flow),
                UnitStats {
                    radius: 12.0,
                    hit_radius: 20.0,
                    speed: 30.0,
                    acc_rate: 0.01,
                    dec_rate: 0.03,
                    turn_rate: 3.0,
                    can_fly: true,
                    no_chase_vtol: false,
                },
                TeamId(0),
                GlobalTransform::from_xyz(40.0, 0.0, 8.0),
                MovePath::new(vec![Vec3::new(500.0, 0.0, 8.0)], Vec3::new(500.0, 0.0, 8.0)),
            ))
            .id();

        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_millis(500));
        world.run_system_once(update_path_heat).unwrap();

        let walker_heat = world.resource::<PathHeat>().0.get([32.0, 8.0]);
        assert!(
            walker_heat > 0.0,
            "walker must deposit heat at its cell, got {walker_heat}"
        );
        assert_eq!(
            world.resource::<PathHeat>().0.get([40.0, 8.0]),
            0.0,
            "flyers must leave no heat"
        );
        let deposited = walker_heat;

        // Despawn the walkers so the next call is decay-only: a
        // standing unit would keep re-depositing, and the test wants
        // the isolated decay step.
        world.despawn(walker);
        world.despawn(flyer);
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(HEAT_DECAY_PERIOD));
        world.run_system_once(update_path_heat).unwrap();

        let decayed = world.resource::<PathHeat>().0.get([32.0, 8.0]);
        let expected_factor = world
            .resource::<UnitRegistry>()
            .shared_heat_retention()
            .powf(HEAT_DECAY_PERIOD);
        assert!(
            (decayed - deposited * expected_factor).abs() < deposited * 0.05,
            "decay step must multiply by retention^{HEAT_DECAY_PERIOD}: {deposited} → {decayed}"
        );
    }

    /// The real FBI + MOVEINFO.TDF pair drives per-class heat: Bits
    /// (LIGHT) deposit 10/s, Bytes (HEAVY) 500/s.
    #[test]
    fn fbi_movement_class_selects_heat_params() {
        let registry = crate::units::content::unit_registry::UnitRegistry::load();
        assert!((registry.heat_produced(UnitKind::Bit) - 10.0).abs() < 1e-4);
        assert!((registry.heat_retention(UnitKind::Bit) - 0.10).abs() < 1e-4);
        assert!((registry.heat_produced(UnitKind::Byte) - 500.0).abs() < 1e-4);
        assert!((registry.heat_retention(UnitKind::Byte) - 0.002).abs() < 1e-4);
    }
}

#[cfg(test)]
mod cross_map_tests {
    use super::*;
    use crate::units::components::TeamId;
    use bevy::ecs::system::RunSystemOnce;
    use spring_pathfinding::HeatMap;
    use std::time::Duration;

    /// Ground-truth diagnostic: load a real shipped map, build the nav
    /// grid the game builds, and drive a real Bit through the real
    /// `movement_system` to targets all over the map — including up
    /// plateau ramps. Reports which targets are reachable.
    #[test]
    #[ignore = "manual diagnostic — run with -- --ignored --nocapture"]
    fn drive_bit_across_real_maps() {
        for map_name in ["Central_Hub", "DigitalDivide_PT2"] {
            let path = format!("assets/maps/{map_name}.kpmap");
            let Ok(bytes) = std::fs::read(&path) else {
                eprintln!("skip {map_name}: no {path}");
                continue;
            };
            let baked = spring_map::baked::read_baked_map(&bytes).expect("baked map");
            let start_positions = baked
                .map_info
                .as_ref()
                .map(|info| info.start_positions.clone())
                .unwrap_or_default();
            let parsed = baked.parsed;

            let cap = spring_pathfinding::max_slope_from_degrees(36.0);
            let speed_map = spring_pathfinding::SpeedMap::from_heightmap(
                &parsed.heights,
                parsed.header.heightmap_width() as u32,
                parsed.header.heightmap_height() as u32,
                cap,
                spring_pathfinding::slope_mod_from_max_slope(cap),
            );
            let heightmap = Heightmap::from_raw(
                parsed.heights,
                parsed.header.heightmap_width(),
                parsed.header.heightmap_height(),
            );

            let mut nav = NavGridSet::default();
            nav.buckets.push(NavBucket {
                max_slope: cap,
                speed_map,
                terrain_labels: None,
            });

            let mut world = World::new();
            world.init_resource::<Time>();
            world.insert_resource(UnitRegistry::load());
            world.insert_resource(nav);
            world.insert_resource(heightmap);
            let heat_dims = (
                parsed.header.heightmap_width() as u32 - 1,
                parsed.header.heightmap_height() as u32 - 1,
            );
            world.insert_resource(PathHeat(HeatMap::new(heat_dims.0, heat_dims.1)));

            // Start/target: the map's authored start positions — the
            // places the game actually spawns homebases. If units can't
            // march between those, the map is unplayable.
            let start = start_positions
                .first()
                .map(|sp| Vec3::new(sp.x, 0.0, sp.z))
                .unwrap_or(Vec3::new(2048.0, 0.0, 64.0));
            let enemy_spawn = start_positions.get(1).map(|sp| Vec3::new(sp.x, 0.0, sp.z));
            println!(
                "  {map_name}: start at ({:.0},{:.0}), enemy spawn {:?}",
                start.x,
                start.z,
                enemy_spawn.map(|e| (e.x, e.z)),
            );

            let registry = UnitRegistry::load();
            let transform = Transform::from_translation(start);
            let unit = world
                .spawn((
                    UnitType(UnitKind::Bit),
                    UnitStats {
                        radius: 12.0,
                        hit_radius: 20.0,
                        speed: 90.0,
                        acc_rate: 0.03,
                        dec_rate: 0.067,
                        turn_rate: 6.0,
                        can_fly: false,
                        no_chase_vtol: true,
                    },
                    TeamId(0),
                    transform,
                ))
                .id();
            world.entity_mut(unit).insert(ground_mover_components(
                UnitKind::Bit,
                &registry,
                unit,
                &transform,
            ));

            let max_cell = (heat_dims.0 as usize).min(504);
            let mut reached = 0usize;
            let mut total = 0usize;
            for (tx, tz) in [
                (max_cell, max_cell),
                (max_cell, 8),
                (8, max_cell),
                (256, 256),
                (256, 8),
                (8, 256),
            ] {
                let (tx, tz) = (tx.min(max_cell), tz.min(max_cell));
                let target = Vec3::new(tx as f32 * 8.0, 0.0, tz as f32 * 8.0);
                total += 1;

                // Reset the unit for this leg.
                world
                    .entity_mut(unit)
                    .insert(MoveTarget(target))
                    .insert(Transform::from_translation(start));
                world
                    .resource_mut::<Time>()
                    .advance_by(Duration::from_secs(1));

                let mut arrived_at = None;
                for tick in 0..2400 {
                    world
                        .resource_mut::<Time>()
                        .advance_by(Duration::from_millis(33));
                    world.run_system_once(movement_system).unwrap();
                    let pos = world.get::<Transform>(unit).unwrap().translation;
                    let dxz = ((pos.x - target.x).powi(2) + (pos.z - target.z).powi(2)).sqrt();
                    if dxz < 16.0 {
                        arrived_at = Some(tick);
                        break;
                    }
                }
                let pos = world
                    .get::<Transform>(unit)
                    .map(|t| t.translation)
                    .unwrap_or_default();
                let hm = world.resource::<Heightmap>();
                let h_start = hm.sample(start.x, start.z);
                let h_target = hm.sample(target.x, target.z);
                match arrived_at {
                    Some(tick) => {
                        reached += 1;
                        println!(
                            "  {map_name} -> cell {tx},{tz} (dh={:+.0}): ARRIVED {} ticks ({:.0}s) y={:.1}",
                            h_target - h_start,
                            tick,
                            tick as f32 * 0.033,
                            pos.y,
                        );
                    }
                    None => {
                        let mt = world.get::<MoveTarget>(unit).is_some();
                        let mp = world
                            .get::<MovePath>(unit)
                            .map(|p| (p.current, p.waypoints.len()));
                        let dxz = ((pos.x - target.x).powi(2) + (pos.z - target.z).powi(2)).sqrt();
                        println!(
                            "  {map_name} -> cell {tx},{tz} (dh={:+.0}): STUCK at ({:.0},{:.0}) dxz {:.0} y={:.1} order_alive={mt} path={mp:?}",
                            h_target - h_start,
                            pos.x,
                            pos.z,
                            dxz,
                            pos.y,
                        );
                    }
                }
            }
            // The leg that matters: spawn-to-spawn.
            if let Some(espawn) = enemy_spawn {
                total += 1;
                world
                    .entity_mut(unit)
                    .insert(MoveTarget(espawn))
                    .insert(Transform::from_translation(start));
                world
                    .resource_mut::<Time>()
                    .advance_by(Duration::from_secs(1));
                let mut arrived_at = None;
                for tick in 0..3600 {
                    world
                        .resource_mut::<Time>()
                        .advance_by(Duration::from_millis(33));
                    world.run_system_once(movement_system).unwrap();
                    let pos = world.get::<Transform>(unit).unwrap().translation;
                    if ((pos.x - espawn.x).powi(2) + (pos.z - espawn.z).powi(2)).sqrt() < 20.0 {
                        arrived_at = Some(tick);
                        break;
                    }
                }
                match arrived_at {
                    Some(tick) => {
                        reached += 1;
                        println!(
                            "  {map_name} -> ENEMY SPAWN: ARRIVED {} ticks ({:.0}s)",
                            tick,
                            tick as f32 * 0.033,
                        );
                    }
                    None => println!("  {map_name} -> ENEMY SPAWN: FAILED TO ARRIVE"),
                }
            }
            println!("{map_name}: {reached}/{total} targets reached");
        }
    }
}

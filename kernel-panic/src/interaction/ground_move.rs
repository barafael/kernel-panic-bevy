//! Port of Spring/Recoil's `CGroundMoveType`
//! (`rts/Sim/MoveTypes/GroundMoveType.cpp`) — how a ground unit follows
//! its path: acceleration and braking, turning with inertia, the
//! turn-limited target speed, waypoint advance with corner cutting,
//! unit/structure collision response and the idle/stuck bookkeeping
//! that repaths or gives up.
//!
//! Everything here runs once per 30 Hz sim frame and works in Spring's
//! per-frame units: speeds in elmos/frame, accelerations in
//! elmos/frame², headings and turn rates in 16-bit heading units
//! ([`Heading`], 65536 per turn) per frame. [`UnitStats`] keeps speed
//! and turn rate per second for the rest of the port (acceleration is
//! already per frame); [`FrameStats`] converts.
//!
//! The per-frame order mirrors `GroundMoveSystem::Update`
//! (`Sim/MoveTypes/Systems/GroundMoveSystem.cpp`):
//! 1. `UpdateTraversalPlan` — swap in a new path, `FollowPath` (goal and
//!    waypoint bookkeeping, wanted heading) — [`movement_system`];
//! 2. `ChangeHeading`;
//! 3. `UpdateUnitPosition` + `UpdatePreCollisions` — `ChangeSpeed`,
//!    `UpdateOwnerPos` (a step along the heading, gated by `UpdatePos`),
//!    `Arrived` / `Fail`;
//! 4. `HandleObjectCollisions` for every ground unit, moving or not —
//!    [`ground_collision_system`];
//! 5. `Update` — apply the collision push, `OwnerMoved` idle test; and
//!    every 15th frame per unit `SlowUpdate` (repath / give up).

use std::f32::consts::{PI, TAU};

use bevy::platform::collections::HashMap;
use bevy::prelude::*;

use super::movement::{
    AttackMoveActive, CommandQueue, GroundLift, LayerKey, MovePath, MoveTarget, NavGridSet,
    PathOutcome, PathRequest, nav_class, promote_next_command, qt_path_outcome,
};
use super::structures::crushes_features;
use crate::map_events::CircularFlow;
use crate::sim::{
    GAME_SPEED, Heading, MAP_GRAVITY_PER_FRAME2, SHORT_ANGLE_TO_RAD, SLOW_UPDATE_RATE,
    SPRING_CIRCLE_DIVS, SPRING_MAX_HEADING, SQUARE_SIZE,
};
use crate::terrain::heightmap::Heightmap;
use crate::units::combat::{AimTarget, DeployState, Deployable, Dying, Stunned};
use crate::units::components::{TeamId, UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::construction::PendingBuild;
use spring_pathfinding::qtpfs::{NodeLayer, QtScratch, QtSearch, Rect as QtRect};
use spring_pathfinding::{BlockMask, SpeedMap};

/// Goal radius of a plain move order (`CMobileCAI::SetGoal`'s default
/// `goalRadius = SQUARE_SIZE`, MobileCAI.h:25).
pub const MOVE_GOAL_RADIUS: f32 = SQUARE_SIZE;
/// `MAX_IDLING_SLOWUPDATES` (GroundMoveType.cpp:84).
pub const MAX_IDLING_SLOWUPDATES: u32 = 16;
/// `modInfo.pfRepathDelayInFrames` / `pfRepathMaxRateInFrames` defaults
/// (ModInfo.cpp:133-134).
pub const REPATH_DELAY_FRAMES: u32 = 60;
pub const REPATH_MAX_RATE_FRAMES: u32 = 150;
/// Frames a dirtied path waits before its re-search
/// (`QueueDeadPathSearches`, PathManager.cpp:895).
pub const DEAD_PATH_REFRESH_FRAMES: u32 = 30;
/// `SPRING_MAX_HEADING` (half a turn) as the float the engine compares
/// idle counts against.
const MAX_HEADING: f32 = SPRING_MAX_HEADING as f32;
/// Turn inertia: `turnAccel = turnRate · 0.333` for non-ships
/// (GroundMoveType.cpp:516).
const TURN_ACCEL_FRACTION: f32 = 0.333;
/// `float3::cmp_eps()` — "didn't move" tolerance in `OwnerMoved`.
const CMP_EPS: f32 = 1e-4;
/// `sqSkidSpeedMult` (GroundMoveType.h:251): a velocity more than
/// ~12.9° off the facing starts a skid.
const SQ_SKID_SPEED_MULT: f32 = 0.95;
/// `speedReduction` in `UpdateSkid`: elmos/frame lost per frame on
/// level ground.
const SKID_SPEED_REDUCTION: f32 = 0.35;
/// `UnitDef` drag defaults (UnitDef.cpp:434-436) and the map
/// atmosphere's `fluidDensity` default (MapInfo.cpp:164).
const ATMOSPHERIC_DRAG_COEFF: f32 = 1.0;
const GROUND_FRICTION_COEFF: f32 = 0.01;
const ROLLING_RESISTANCE_COEFF: f32 = 0.05;
const ATMOSPHERE_DENSITY: f32 = 1.2 * 0.25;

/// Impulses from this frame's explosions, `(unit, impulse)` in
/// mass-scaled units not yet divided by the unit's mass
/// (`CUnit::DoDamage` → `ApplyImpulse(impulse / mass)`); the movement
/// system applies them at the start of the next sim frame.
#[derive(Resource, Default)]
pub struct PendingImpulses(pub Vec<(Entity, Vec3)>);
/// A\* nodes expanded per sim frame across all path searches. Spring's
/// QTPFS queues `RequestPath` calls and executes them at the head of
/// the next frame while units drive toward a temporary waypoint; the
/// port does the same, but bounds the work per frame by node count
/// (deterministic where a wall-clock budget is not) and resumes an
/// unfinished search next frame — one flooding search for an
/// unreachable goal no longer stalls a whole frame. ~0.5 ms of work
/// natively; searches usually finish in a few hundred nodes.
const PATH_SEARCH_NODES_PER_FRAME: usize = 6000;

/// Overrides [`PATH_SEARCH_NODES_PER_FRAME`] (the headless harness makes
/// runs reproducible by never running out).
#[derive(Resource, Clone, Copy)]
pub struct PathSearchBudget(pub usize);

/// The path service: requests answered across sim frames (QTPFS
/// `QueueSearch` / `ExecuteQueuedSearches`). A request is answered in
/// the mover's own update while the frame's node budget lasts; once it
/// is spent the search stays in flight and later requests queue, served
/// in order at the head of the following frames, one search at a time.
/// A finished path waits here until its mover picks it up.
/// Path-service counters for the `KP_PROFILE` report: what the searches
/// cost and how they ended.
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq)]
pub struct PathStats {
    pub searches: u64,
    /// Searches that did not fit the frame budget and were resumed.
    pub deferred: u64,
    /// Requests that found the service busy and queued.
    pub queued: u64,
    /// Nodes expanded (budget spent).
    pub nodes: u64,
    /// Outcomes: goal reached / partial (closest cell) / no start.
    pub reached: u64,
    pub partial: u64,
    pub unreachable: u64,
    /// Node layers built from scratch, and brought up to date from the
    /// changed squares of a revision instead.
    pub labellings: u64,
    pub label_updates: u64,
}

impl PathStats {
    fn outcome(&mut self, outcome: &Option<PathOutcome>) {
        match outcome {
            Some(PathOutcome::Route(p)) if p.reached_goal => self.reached += 1,
            Some(PathOutcome::Route(_)) => self.partial += 1,
            _ => self.unreachable += 1,
        }
    }
}

/// Held as a resource (not a system `Local`): the path service is
/// game state — inspectable by the profile census and replaceable in
/// tests — not scratch private to one system.
#[derive(Resource, Default)]
pub struct PathQueue {
    pub stats: PathStats,
    /// [`NavGridSet::epoch`] everything below belongs to; a new map
    /// drops it all (layers, the search in flight and its scratch are
    /// sized to the old grid).
    epoch: u64,
    /// One QTPFS [`NodeLayer`] per mover class with the nav revision it
    /// is current for; built on the first request of a class and
    /// brought up to date through the change ring.
    layers: HashMap<LayerKey, (u64, NodeLayer)>,
    pending: std::collections::VecDeque<Entity>,
    queued: bevy::platform::collections::HashSet<Entity>,
    /// The search in flight with the nav revision its layer had when it
    /// began: the result is stamped with that revision, so a change that
    /// lands mid-search still dirties the path afterwards.
    in_flight: Option<(Entity, PathRequest, LayerKey, u64, QtSearch)>,
    scratch: QtScratch,
    /// Finished searches, keyed by mover: the goal searched for and
    /// the outcome (`None`: nothing could be decided, no nav grid).
    results: HashMap<Entity, (Vec2, Option<PathOutcome>)>,
}

impl PathQueue {
    /// The node layer of `req`'s class, current for `nav`'s revision
    /// (`PathManager::UpdateNodeLayer` on every changed rectangle
    /// since), built from scratch when missing or when the change ring
    /// no longer reaches back far enough.
    fn layer<'a>(
        &mut self,
        nav: &'a NavGridSet,
        registry: &UnitRegistry,
        req: &PathRequest,
    ) -> Option<(LayerKey, &'a SpeedMap, Option<&'a BlockMask>)> {
        let (key, speed_map, mask) = nav_class(nav, registry, req)?;
        // Missing: start from the map's bare-terrain layer (built with
        // the map) at revision 0 and replay the changes since.
        if !self.layers.contains_key(&key)
            && let Some(terrain) = nav.terrain_layers.get(&key)
        {
            self.layers.insert(key, (0, terrain.clone()));
        }
        let changes = match self.layers.get(&key) {
            Some((rev, _)) if *rev == nav.revision => return Some((key, speed_map, mask)),
            Some((rev, _)) => nav.changes_since_each(*rev),
            None => None,
        };
        match changes {
            Some(changes) => {
                let (rev, layer) = self.layers.get_mut(&key).expect("checked above");
                layer.update(
                    speed_map,
                    mask,
                    changes.map(|b| QtRect {
                        x0: b[0],
                        z0: b[1],
                        x1: b[2] + 1,
                        z1: b[3] + 1,
                    }),
                );
                *rev = nav.revision;
                self.stats.label_updates += 1;
            }
            None => {
                // The change ring no longer reaches back: tesselate anew.
                self.layers
                    .insert(key, (nav.revision, NodeLayer::new(speed_map, mask)));
                self.stats.labellings += 1;
            }
        }
        Some((key, speed_map, mask))
    }

    /// Begin `req`'s search on its class's layer. `Err` carries an
    /// outcome that needed no search (a clear straight line, source and
    /// goal in one leaf, or no nav grid at all: `None`).
    fn begin(
        &mut self,
        nav: Option<&NavGridSet>,
        registry: &UnitRegistry,
        req: &PathRequest,
    ) -> Result<(LayerKey, QtSearch), Option<PathOutcome>> {
        let Some(nav) = nav else { return Err(None) };
        let Some((key, speed_map, mask)) = self.layer(nav, registry, req) else {
            return Err(None);
        };
        let (_, layer) = self.layers.get_mut(&key).expect("layer exists");
        let src = [req.from.x, req.from.z];
        let dst = [req.to.x, req.to.z];
        match QtSearch::begin(
            layer,
            &mut self.scratch,
            speed_map,
            mask,
            src,
            dst,
            req.goal_radius,
        ) {
            Ok(search) => Ok((key, search)),
            Err(path) => Err(Some(qt_path_outcome(Some(path), req, nav.revision))),
        }
    }

    /// Queue a search for `entity` unless one is already pending.
    fn request(&mut self, entity: Entity) {
        if self.queued.insert(entity) {
            self.pending.push_back(entity);
            self.stats.queued += 1;
        }
    }

    /// Advance the search in flight by up to `pops` iterations.
    fn step_in_flight(&mut self, pops: usize) -> Option<Option<PathOutcome>> {
        let (_, req, key, revision, search) = self.in_flight.as_mut().expect("search in flight");
        let (_, layer) = self.layers.get(key).expect("layer exists");
        search
            .step(layer, &mut self.scratch, pops)
            .map(|path| Some(qt_path_outcome(path, req, *revision)))
    }

    /// Run queued searches until `budget` nodes are spent; returns the
    /// nodes left. `describe` gives a mover's current request (`None`:
    /// it no longer wants a path — despawned, order gone — so the
    /// request is dropped).
    fn service(
        &mut self,
        mut budget: usize,
        nav: Option<&NavGridSet>,
        registry: &UnitRegistry,
        describe: impl Fn(Entity) -> Option<PathRequest>,
    ) -> usize {
        if let Some(n) = nav
            && n.epoch != self.epoch
        {
            self.epoch = n.epoch;
            self.pending.clear();
            self.queued.clear();
            self.in_flight = None;
            self.results.clear();
            self.layers.clear();
            self.scratch = Default::default();
        }
        loop {
            if self.in_flight.is_none() {
                let Some(entity) = self.pending.pop_front() else {
                    return budget;
                };
                self.queued.remove(&entity);
                let Some(req) = describe(entity) else {
                    // Order cancelled or mover gone: an earlier answer
                    // for it would never be collected.
                    self.results.remove(&entity);
                    continue;
                };
                self.stats.searches += 1;
                let revision = nav.map_or(0, |n| n.revision);
                match self.begin(nav, registry, &req) {
                    Ok((key, search)) => {
                        self.in_flight = Some((entity, req, key, revision, search));
                    }
                    Err(outcome) => {
                        self.stats.outcome(&outcome);
                        self.results.insert(entity, (req.to.xz(), outcome));
                        continue;
                    }
                }
            }
            if budget == 0 {
                return 0;
            }
            let pops = budget.min(PATH_SEARCH_STEP);
            let done = self.step_in_flight(pops);
            budget -= pops;
            self.stats.nodes += pops as u64;
            if let Some(outcome) = done {
                let (entity, req, ..) = self.in_flight.take().expect("search in flight");
                self.stats.outcome(&outcome);
                self.results.insert(entity, (req.to.xz(), outcome));
            }
        }
    }

    /// Answer `req` for `entity` right now if the service is idle and
    /// the frame's `budget` covers the whole search — the common case,
    /// and what a synchronous `GetNewPath` did. Otherwise the search is
    /// queued (or left in flight) and answered on a later frame.
    /// Returns the outcome and the nodes left.
    fn request_now(
        &mut self,
        entity: Entity,
        req: PathRequest,
        budget: usize,
        nav: Option<&NavGridSet>,
        registry: &UnitRegistry,
    ) -> (Option<Option<PathOutcome>>, usize) {
        if self.in_flight.is_some() || self.queued.contains(&entity) || budget == 0 {
            self.request(entity);
            return (None, budget);
        }
        self.stats.searches += 1;
        let revision = nav.map_or(0, |n| n.revision);
        let (key, search) = match self.begin(nav, registry, &req) {
            Ok(s) => s,
            Err(outcome) => {
                self.stats.outcome(&outcome);
                return (Some(outcome), budget);
            }
        };
        self.in_flight = Some((entity, req, key, revision, search));
        let mut left = budget;
        while left > 0 {
            let pops = left.min(PATH_SEARCH_STEP);
            let done = self.step_in_flight(pops);
            left -= pops;
            self.stats.nodes += pops as u64;
            if let Some(outcome) = done {
                self.in_flight = None;
                self.stats.outcome(&outcome);
                return (Some(outcome), left);
            }
        }
        self.stats.deferred += 1;
        (None, 0)
    }
}

/// Search iterations per [`PathQueue::service`] step call; only bounds
/// how often the budget is re-checked.
const PATH_SEARCH_STEP: usize = 512;

/// `AMoveType::ProgressState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Progress {
    #[default]
    Done,
    Active,
    Failed,
}

/// Per-unit ground move state (`CGroundMoveType`'s members) plus the
/// unit's `MoveDef` geometry.
#[derive(Component, Clone, Debug)]
pub struct GroundMover {
    // --- MoveDef / UnitDef (static) ---
    /// Footprint half-size in heightmap squares (`MoveDef::xsizeh`).
    pub xsizeh: i32,
    /// `CalcFootPrintMaxInteriorRadius` — collision radius.
    pub collision_radius: f32,
    /// `CalcFootPrintMinExteriorRadius` — `ownerRadius`.
    pub owner_radius: f32,
    /// `CSolidObject::mass`.
    pub mass: f32,
    /// MOVEINFO `CrushStrength`.
    pub crush_strength: f32,
    /// `UnitDef::upright`: never tilt with the terrain.
    pub upright: bool,

    // --- dynamic state ---
    /// `owner->heading`: 16-bit, 0 faces +Z.
    pub heading: Heading,
    /// `turnSpeed` (heading units/frame) — turn inertia.
    pub turn_speed: f32,
    /// `currentSpeed` (elmos/frame, ≥ 0).
    pub current_speed: f32,
    pub progress: Progress,
    /// `goalPos` (XZ) of the order this state belongs to.
    pub goal: Option<Vec2>,
    pub goal_radius: f32,
    pub extra_radius: f32,
    pub waypoint_dir: Vec2,
    pub curr_wp_dist: f32,
    pub prev_wp_dist: f32,
    pub at_goal: bool,
    pub at_end_of_path: bool,
    pub last_waypoint: bool,
    /// `TriggerSkipWayPoint` (collision with a unit on our waypoint).
    pub skip_waypoint: bool,
    pub pathing_arrived: bool,
    pub pathing_failed: bool,
    pub idling: bool,
    pub num_idling_updates: u32,
    pub num_idling_slow_updates: u32,
    /// `ReRequestPath(true)` issued: search a new path.
    pub path_requested: bool,
    /// `ReRequestPath(false)`: a repath is wanted if no progress follows.
    pub want_repath: bool,
    pub want_repath_frame: u32,
    pub last_repath_frame: u32,
    pub best_last_waypoint_dist: f32,
    pub best_reattempted_last_waypoint_dist: f32,
    pub limit_speed_for_turning: u8,
    /// `lastAvoidanceDir` (obstacle avoidance).
    pub last_avoidance_dir: Vec2,
    /// `avoidingUnits`: steered round someone at the last evaluation.
    pub avoiding_units: bool,
    pub position_stuck: bool,
    /// `owner->speed`: this frame's velocity vector along the tilted
    /// front direction (elmos/frame), zero when the unit did not move.
    pub velocity: Vec3,
    /// `PSTATE_BIT_SKIDDING` / `PSTATE_BIT_FLYING`: knocked loose by an
    /// impulse; `UpdateSkid` runs instead of path following until the
    /// velocity is back along the facing and the unit is on the ground.
    pub skidding: bool,
    pub flying: bool,
    /// `skidRotVector`, `skidRotSpeed`, `skidRotAccel`: the tumble the
    /// model shows while skidding (`CalcSkidRot`).
    pub skid_rot_vector: Vec3,
    pub skid_rot_speed: f32,
    pub skid_rot_accel: f32,
    /// `forceStaticObjectCheck`: test next collision pass whether the
    /// unit stands on a closed square / inside a structure footprint
    /// (`positionStuck`). Set by the constructor, an idle repath, and
    /// the trap check when a structure appears over the unit.
    pub force_static_object_check: bool,
    /// `oldPos` at the last `OwnerMoved`.
    pub old_pos: Vec3,
    /// Frame counter; `SlowUpdate` runs when it is a multiple of
    /// `SLOW_UPDATE_RATE` (seeded per unit so they stagger).
    pub frame: u32,
}

impl GroundMover {
    /// A mover with `kind`'s MoveDef (or a fallback from its stats).
    pub fn new(kind: UnitKind, registry: &UnitRegistry, stats: &UnitStats) -> Self {
        let md = registry.move_def(kind);
        let upright = registry.def(kind).is_some_and(|d| d.upright);
        let r = md.map_or(stats.radius, |m| m.collision_radius);
        Self {
            xsizeh: md.map_or(((r / SQUARE_SIZE) as i32).max(1), |m| m.xsizeh),
            collision_radius: r,
            owner_radius: md.map_or(r * std::f32::consts::SQRT_2, |m| m.owner_radius),
            mass: registry.mass(kind),
            crush_strength: md.map_or(0.0, |m| m.crush_strength),
            upright,
            heading: Heading::default(),
            turn_speed: 0.0,
            current_speed: 0.0,
            progress: Progress::Done,
            goal: None,
            goal_radius: MOVE_GOAL_RADIUS,
            extra_radius: 0.0,
            waypoint_dir: Vec2::ZERO,
            curr_wp_dist: 0.0,
            prev_wp_dist: 0.0,
            at_goal: true,
            at_end_of_path: true,
            last_waypoint: false,
            skip_waypoint: false,
            pathing_arrived: false,
            pathing_failed: false,
            idling: false,
            num_idling_updates: 0,
            num_idling_slow_updates: 0,
            path_requested: false,
            want_repath: false,
            want_repath_frame: 0,
            last_repath_frame: 0,
            best_last_waypoint_dist: f32::INFINITY,
            best_reattempted_last_waypoint_dist: f32::INFINITY,
            limit_speed_for_turning: 0,
            last_avoidance_dir: Vec2::ZERO,
            avoiding_units: false,
            position_stuck: false,
            velocity: Vec3::ZERO,
            skidding: false,
            flying: false,
            skid_rot_vector: Vec3::ZERO,
            skid_rot_speed: 0.0,
            skid_rot_accel: 0.0,
            force_static_object_check: true,
            old_pos: Vec3::ZERO,
            frame: 0,
        }
    }

    /// Seed the dynamic state from the unit's spawn pose: heading from
    /// the `Transform`'s facing, `oldPos`, and the `SlowUpdate` phase
    /// from the entity index so units stagger.
    pub fn seeded(mut self, entity: Entity, tf: &Transform) -> Self {
        let f = tf.forward().as_vec3();
        if f.xz().length_squared() > 1e-6 {
            self.heading = Heading::from_vector(f.x, f.z);
        }
        self.old_pos = tf.translation;
        self.frame = entity.index_u32() % SLOW_UPDATE_RATE;
        self
    }

    /// `flatFrontDir` (`GetVectorFromHeading`).
    pub fn front(&self) -> Vec2 {
        self.heading.to_vector()
    }

    /// `rightdir` (XZ): `frontdir × updir`, i.e. facing +Z → −X.
    pub fn right(&self) -> Vec2 {
        let f = self.front();
        Vec2::new(-f.y, f.x)
    }

    /// `IsMoving()`: `PSTATE_BIT_MOVING`, set while `|speed| > 0.01`.
    pub fn is_moving(&self) -> bool {
        self.current_speed > 0.01
    }

    /// `StartMoving` (GroundMoveType.cpp:938): a new goal.
    pub fn start_moving(&mut self, goal: Vec2, goal_radius: f32, pos: Vec3, goal_blocked: bool) {
        self.goal = Some(goal);
        self.goal_radius = goal_radius;
        // Add the footprint radius if standing on the goal would overlap
        // blocked squares (else units jitter against it forever).
        self.extra_radius = if goal_blocked {
            (self.owner_radius - goal_radius).max(0.0)
        } else {
            0.0
        };
        self.at_goal =
            goal.distance_squared(pos.xz()) < (self.goal_radius + self.extra_radius).powi(2);
        self.at_end_of_path = false;
        self.last_waypoint = false;
        self.progress = Progress::Active;
        self.num_idling_updates = 0;
        self.num_idling_slow_updates = 0;
        self.curr_wp_dist = 0.0;
        self.prev_wp_dist = 0.0;
        self.pathing_arrived = false;
        self.pathing_failed = false;
        self.skip_waypoint = false;
        self.best_reattempted_last_waypoint_dist = f32::INFINITY;
        if !self.at_goal {
            self.re_request_path(true);
        }
    }

    /// `StopEngine` + `progressState = Done|Failed`: the order is over.
    /// Momentum is kept; `WantToStop` brakes it off at `decRate`.
    pub fn stop_engine(&mut self, progress: Progress) {
        self.progress = progress;
        self.goal = None;
        self.at_goal = true;
        self.at_end_of_path = true;
        self.path_requested = false;
        self.want_repath = false;
        self.pathing_arrived = false;
        self.pathing_failed = false;
        self.limit_speed_for_turning = 0;
        self.best_reattempted_last_waypoint_dist = f32::INFINITY;
    }

    /// `ReRequestPath` (GroundMoveType.cpp:2104).
    pub fn re_request_path(&mut self, force: bool) {
        if force {
            self.path_requested = true;
            self.want_repath = false;
            self.last_repath_frame = self.frame;
            return;
        }
        if !self.want_repath {
            self.want_repath = true;
            self.want_repath_frame = self.frame;
            self.best_last_waypoint_dist = f32::INFINITY;
        }
    }

    /// `TriggerCallArrived`.
    pub fn trigger_call_arrived(&mut self) {
        self.at_end_of_path = true;
        self.at_goal = true;
        self.pathing_arrived = true;
    }

    /// `CUnit::ApplyImpulse` + `CGroundMoveType::CanApplyImpulse`
    /// (Unit.cpp:1392, GroundMoveType.cpp:1454). `impulse` is already
    /// divided by the mass; its component into the ground is dropped,
    /// the rest becomes velocity and may start a skid (velocity far off
    /// the facing) or a flight (velocity up out of the ground).
    /// `rand` ∈ [0, 1) seeds the tumble of a launched unit.
    pub fn apply_impulse(&mut self, impulse: Vec3, ground_normal: Vec3, rand: f32) {
        let into_ground = if self.flying {
            Vec3::ZERO
        } else {
            ground_normal * impulse.dot(ground_normal).min(0.0)
        };
        let impulse = impulse - into_ground;
        if impulse.length_squared() <= 0.01 {
            return;
        }
        self.skid_rot_speed = 0.0;
        self.skid_rot_accel = 0.0;
        let new_speed = self.velocity + impulse;
        let mut skid_dir = self.heading.to_vector3();
        let start_skidding = signed_square(new_speed.dot(skid_dir)) + 0.01
            < new_speed.length_squared() * SQ_SKID_SPEED_MULT;
        let start_flying = new_speed.dot(ground_normal) > 0.2;
        if new_speed.xz().length_squared() >= 0.01 {
            skid_dir = new_speed.with_y(0.0).normalize();
        }
        self.skid_rot_vector = if start_skidding {
            skid_dir.cross(Vec3::Y)
        } else {
            Vec3::ZERO
        };
        self.skid_rot_accel = if start_flying {
            (rand - 0.5) * 0.04
        } else {
            0.0
        };
        self.skidding |= start_skidding || start_flying;
        self.flying |= start_flying;
        self.velocity = new_speed;
        self.current_speed = new_speed.length();
    }

    /// `CGroundMoveType::UpdateSkid` (GroundMoveType.cpp:1504) for one
    /// frame: drag, the per-frame speed loss, gravity while airborne,
    /// the ground bounce, and the skid rotation bookkeeping
    /// (`CalcSkidRot`). `ground_height` / `ground_normal` are taken at
    /// the current `pos`; the caller re-seats a grounded unit on the
    /// terrain afterwards (`AdjustPosToWaterLine`). `OnSlope` is never
    /// true in KP (`slideTolerance` 0), so there is no slope sliding.
    pub fn update_skid(&mut self, pos: &mut Vec3, ground_height: f32, ground_normal: Vec3) {
        let gravity = MAP_GRAVITY_PER_FRAME2;
        let mut v = self.velocity
            + drag_acceleration(
                self.velocity,
                self.mass,
                !self.flying,
                GROUND_FRICTION_COEFF,
            );
        let neg_altitude = ground_height - pos.y;
        if self.flying {
            if neg_altitude > 0.0 {
                // Landed (collision damage is off in KP).
                self.flying = false;
                pos.y += neg_altitude;
                self.skid_rot_speed = 0.0;
            } else {
                v.y += gravity;
            }
        } else {
            let front = self.heading.to_vector3();
            let stop =
                signed_square(v.dot(front)) + 0.01 >= v.length_squared() * SQ_SKID_SPEED_MULT;
            if stop {
                let rot_speed = (self.skid_rot_speed + self.skid_rot_accel + 0.5).floor();
                self.skid_rot_accel = (rot_speed - self.skid_rot_speed) * 0.5 * DEG_TO_RAD;
                self.skidding = false;
            } else {
                let speed_scale = v.length();
                let rot_rem_time = (speed_scale / SKID_SPEED_REDUCTION).max(1.0);
                v *= 1.0 - (SKID_SPEED_REDUCTION / speed_scale).min(1.0);
                let rot_speed =
                    (self.skid_rot_speed + self.skid_rot_accel * (rot_rem_time - 1.0) + 0.5)
                        .floor();
                self.skid_rot_accel = (rot_speed - self.skid_rot_speed) / rot_rem_time * DEG_TO_RAD;
                if self.skid_rot_speed.floor()
                    != (self.skid_rot_speed + self.skid_rot_accel).floor()
                {
                    self.skid_rot_speed = 0.0;
                    self.skid_rot_accel = 0.0;
                }
            }
            if neg_altitude < v.y + gravity {
                // Leaving the ground (off a ledge).
                v.y += gravity;
                self.flying = true;
                self.skidding = true;
            } else if neg_altitude > v.y {
                let proj = v.dot(ground_normal);
                if proj > 0.0 {
                    v *= 0.95;
                } else {
                    v += ground_normal * (proj.abs() + 0.1);
                    v *= 0.8;
                }
            }
        }
        self.velocity = v;
        *pos += v;
        if self.skidding {
            // `CalcSkidRot`.
            self.skid_rot_speed += self.skid_rot_accel;
            self.skid_rot_speed *= 0.999;
            self.skid_rot_accel *= 0.95;
        } else {
            // Back under `ChangeHeading` / `ChangeSpeed`, from the speed
            // the skid left (`UpdateOwnerSpeed` picks `|speed|` up).
            self.current_speed = v.length();
        }
    }

    /// The tumble `CalcSkidRot` applies on top of the heading frame:
    /// `skidRotSpeed / GAME_SPEED · 2π` about `skidRotVector`.
    pub fn skid_rotation(&self) -> Quat {
        if self.skid_rot_vector == Vec3::ZERO {
            return Quat::IDENTITY;
        }
        Quat::from_axis_angle(self.skid_rot_vector, self.skid_rot_speed / GAME_SPEED * TAU)
    }

    /// `(goalRadius + extraRadius) · (numIdlingSlowUpdates + 1)` for
    /// move commands (`UNIT_HAS_MOVE_CMD`), else unscaled.
    fn goal_tolerance(&self, has_move_cmd: bool) -> f32 {
        let r = self.goal_radius + self.extra_radius;
        if has_move_cmd {
            r * (self.num_idling_slow_updates + 1) as f32
        } else {
            r
        }
    }

    fn is_slow_update(&self) -> bool {
        self.frame.is_multiple_of(SLOW_UPDATE_RATE)
    }
}

/// A unit's per-frame kinematic limits (Spring's per-frame units).
#[derive(Clone, Copy, Debug)]
pub struct FrameStats {
    /// `maxSpeed` (elmos/frame).
    pub max_speed: f32,
    /// `accRate` / `decRate` (elmos/frame²).
    pub acc: f32,
    pub dec: f32,
    /// `turnRate`: heading units per frame, `clamp(FBI TurnRate, 1,
    /// 32767)` (GroundMoveType.cpp:515).
    pub turn_rate: f32,
}

impl FrameStats {
    pub fn new(stats: &UnitStats, speed: f32) -> Self {
        Self {
            max_speed: speed / GAME_SPEED,
            // `accRate = max(0.01, maxAcc)` (GroundMoveType.cpp:518-519).
            acc: stats.acc_rate.max(0.01),
            dec: stats.dec_rate.max(0.01),
            // `UnitStats` keeps the FBI value as rad/s; back to units
            // per frame (the FBI values are integers, so `round` is exact).
            turn_rate: (stats.turn_rate / (SHORT_ANGLE_TO_RAD * GAME_SPEED))
                .round()
                .clamp(1.0, 32767.0),
        }
    }

    /// Frames for a full revolution (`SPRING_CIRCLE_DIVS / turnRate`).
    pub fn frames_to_turn(&self) -> f32 {
        SPRING_CIRCLE_DIVS as f32 / self.turn_rate
    }
}

/// Spring's `Sign` (≥ 0 → +1).
fn sign(x: f32) -> f32 {
    if x >= 0.0 { 1.0 } else { -1.0 }
}

/// `SignedSquare`.
fn signed_square(x: f32) -> f32 {
    x * x.abs()
}

const DEG_TO_RAD: f32 = PI / 180.0;

/// `CSolidObject::GetDragAccelerationVec` (SolidObject.cpp:328) for a
/// unit on the ground or in the air (never in water): air drag
/// `½ρ·Cd·A·v²` on a sphere of the unit's mass at 8000 kg/m³, ground
/// friction `μ·m·|v|` while grounded, divided by the mass and clamped
/// per component so it never reverses the velocity. Below 0.5 elmos/s
/// the velocity is cancelled outright.
pub fn drag_acceleration(velocity: Vec3, mass: f32, on_ground: bool, friction: f32) -> Vec3 {
    if velocity.length() * GAME_SPEED < 0.5 {
        return -velocity;
    }
    let radius = (3.0 * mass / (4.0 * PI * 8000.0)).cbrt();
    let air = 0.5 * ATMOSPHERE_DENSITY * ATMOSPHERIC_DRAG_COEFF * PI * radius * radius;
    let ground = if on_ground { friction * mass } else { 0.0 };
    let per_axis = |v: f32| {
        let a = (v * v * air + v.abs() * ground) * -sign(v) / mass;
        a.clamp(-v.abs(), v.abs())
    };
    Vec3::new(
        per_axis(velocity.x),
        per_axis(velocity.y),
        per_axis(velocity.z),
    )
}

/// `AMoveType::BrakingDistance` (MoveType.h:80).
pub fn braking_distance(speed: f32, rate: f32) -> f32 {
    let time = speed / rate.max(0.001);
    0.5 * rate * time * time
}

/// `GMTDefaultPathController::GetDeltaSpeed` (IPathController.cpp:6),
/// forward-only (KP units cannot reverse): the speed moves toward
/// `target` by at most `acc` (speeding up) or `dec` (slowing down).
pub fn delta_speed(target: f32, current: f32, acc: f32, dec: f32) -> f32 {
    let diff = target - current;
    if diff < 0.0 {
        -(-diff).min(dec)
    } else {
        diff.min(acc)
    }
}

/// `GMTDefaultPathController::GetDeltaHeading` with rotational inertia
/// (IPathController.cpp:66, `MODEL_TURN_INERTIA`): the turn speed
/// changes by at most `turn_accel` per frame toward the wanted heading,
/// braking early enough not to overshoot, clamped to `max_turn`. All in
/// heading units; the engine's `short` casts are kept, so the heading
/// change is a whole number of units. Returns this frame's change.
pub fn delta_heading(
    wanted: Heading,
    heading: Heading,
    max_turn: f32,
    turn_accel: f32,
    turn_speed: &mut f32,
) -> i16 {
    let cur = *turn_speed;
    let braking = cur.abs() >= turn_accel;
    let brake_dist = braking_distance(cur.abs(), turn_accel);
    // `short stopTurnHeading = oldHeading + brakeDist · Sign · factor`.
    let lookahead = if braking { brake_dist * sign(cur) } else { 0.0 };
    let stop_heading = heading.wrapping_add(lookahead as i32 as i16);
    let cur_delta = wanted.wrapping_sub(stop_heading) as f32;
    let step = sign(cur_delta) * cur_delta.abs().min(turn_accel);
    let next = if braking { cur + step } else { step };
    *turn_speed = next.clamp(-max_turn, max_turn);
    *turn_speed as i32 as i16
}

/// Everything `ChangeSpeed` (GroundMoveType.cpp:1283) needs beyond the
/// mover's own state.
pub struct SpeedInputs {
    pub pos: Vec2,
    /// `wantedHeading` of this frame's `ChangeHeading`.
    pub wanted_heading: Heading,
    /// `UNIT_CMD_QUE_SIZE(owner) <= 1`: nothing queued behind the order.
    pub last_command: bool,
    /// Terrain speed mod at the unit's square in its facing
    /// (`CMoveMath::GetPosSpeedMod`).
    pub ground_speed_mod: f32,
}

/// `CGroundMoveType::ChangeSpeed`: returns this frame's speed delta.
/// `wanted_speed` is `maxWantedSpeed` while following a path, 0 when
/// stopping.
pub fn change_speed(m: &GroundMover, fs: &FrameStats, wanted_speed: f32, inp: &SpeedInputs) -> f32 {
    if wanted_speed <= 0.0 && m.current_speed < 0.01 {
        return -m.current_speed;
    }
    let mut target = fs.max_speed;
    if wanted_speed > 0.0 {
        let goal = m.goal.unwrap_or(inp.pos);
        let cur_goal_dist_sq = inp.pos.distance_squared(goal);
        let min_goal_dist_sq = braking_distance(m.current_speed, fs.dec).powi(2);
        // Brake for the goal only when this is the last command.
        let start_braking = inp.last_command && cur_goal_dist_sq <= min_goal_dist_sq;
        let mut max_speed_to_make_turn = f32::INFINITY;

        let turn_delta = m
            .heading
            .wrapping_sub(Heading::from_vector(m.waypoint_dir.x, m.waypoint_dir.y));
        if turn_delta != 0 {
            // Remaining turn vs the per-frame turn, in degrees: the unit
            // slows to `maxSpeed · clamp(maxTurn/reqTurn, 0.1, 1)`
            // (never below 10%), so it arcs through turns instead of
            // pivoting then lurching.
            let req_turn =
                (180.0 * m.heading.wrapping_sub(inp.wanted_heading) as f32 / MAX_HEADING).abs();
            let max_turn = fs.turn_rate / SPRING_CIRCLE_DIVS as f32 * 360.0;
            let mut turn_mod_speed = fs.max_speed;
            if req_turn != 0.0 {
                turn_mod_speed *= (max_turn / req_turn).clamp(0.1, 1.0);
            }
            // `turnInPlace=1` (the FBI default) with
            // `turnInPlaceAngleLimit=0`: any remaining turn applies it.
            if m.waypoint_dir.length_squared() > 0.1 && req_turn > 0.0 {
                target = turn_mod_speed;
            }
            if m.at_end_of_path {
                // No waypoint left to cut to: slow to a turn circle
                // through the goal so it is not orbited.
                target = target.min(m.curr_wp_dist * PI / fs.frames_to_turn());
            }
        }
        if m.limit_speed_for_turning > 0 {
            // Deflected off a structure: slow enough to make the turn
            // back to the waypoint without hitting it again.
            let offset = Heading::from_vector(m.waypoint_dir.x, m.waypoint_dir.y)
                .wrapping_sub(m.heading) as f32;
            let frames = (offset / fs.turn_rate).abs().max(1e-4);
            max_speed_to_make_turn = (m.curr_wp_dist / frames * 0.95).max(0.01);
        }
        // GroundMoveType.cpp:1397 — `wantedSpeed *= max(groundSpeedMod,
        // 1.0)`. With our 0..1 modifiers this is a no-op, exactly as the
        // engine's own NOTE explains ("raise wantedSpeed iff the
        // terrain-modifier is larger than 1"); kept verbatim so a future
        // >1 speed modifier behaves like Spring's.
        let wanted = wanted_speed * inp.ground_speed_mod.max(1.0);
        target *= inp.ground_speed_mod;
        if start_braking {
            target = 0.0;
        }
        target = target.min(wanted).min(max_speed_to_make_turn);
    } else {
        target = 0.0;
    }
    delta_speed(target, m.current_speed, fs.acc, fs.dec)
}

/// A mover's view of the map for `UpdatePos` and `DoRawSearch`.
#[derive(Clone, Copy)]
pub struct MoveMap<'a> {
    pub nav: Option<&'a NavGridSet>,
    pub max_slope: f32,
    pub xsizeh: i32,
    pub crush_strength: f32,
}

impl MoveMap<'_> {
    /// `isSquareOpen` in `UpdatePos`: the centre square's terrain is
    /// passable and no structure blocks the footprint
    /// (`TestMoveSquare` + `TestMovePositionForObjects`).
    pub fn square_open(&self, p: Vec2) -> bool {
        let Some(nav) = self.nav else { return true };
        nav.passable(self.max_slope, p.x, p.y)
            && !nav.footprint_blocked(p.x, p.y, self.xsizeh, self.crush_strength)
    }

    /// `SquareIsBlocked(..) & BLOCK_STRUCTURE` for the square under `p`.
    pub fn structure_at(&self, p: Vec2) -> bool {
        self.nav.is_some_and(|nav| {
            nav.structure_square(
                (p.x / SQUARE_SIZE).floor() as i32,
                (p.y / SQUARE_SIZE).floor() as i32,
                self.crush_strength,
            )
        })
    }

    /// `MoveDef::DoRawSearch`: can the unit drive straight `a → b`?
    pub fn raw_search(&self, a: Vec2, b: Vec2) -> bool {
        let Some(nav) = self.nav else { return true };
        nav.line_clear(self.max_slope, self.xsizeh, self.crush_strength, a, b)
    }
}

/// `CGroundMoveType::UpdatePos` (GroundMoveType.cpp:3330): the part of
/// `step` the unit may take without entering a closed square. A step
/// into one tries sideways offsets (up to a square either way) before
/// giving up; a diagonal step may not squeeze between two closed
/// squares. `position_stuck` (already inside closed squares) makes
/// every step re-checked so the unit can get out.
pub fn update_pos(
    map: &MoveMap,
    pos: Vec2,
    step: Vec2,
    right: Vec2,
    facing: u32,
    position_stuck: bool,
) -> Vec2 {
    let sq = |p: Vec2| {
        (
            (p.x / SQUARE_SIZE).floor() as i32,
            (p.y / SQUARE_SIZE).floor() as i32,
        )
    };
    let new_pos = pos + step;
    let (prev_sq, new_sq) = (sq(pos), sq(new_pos));
    if !position_stuck && prev_sq == new_sq {
        return step;
    }
    let to_pos = |s: (i32, i32)| {
        Vec2::new(
            s.0 as f32 * SQUARE_SIZE + 1.0,
            s.1 as f32 * SQUARE_SIZE + 1.0,
        )
    };
    if map.square_open(new_pos) {
        let diff = (new_sq.0 - prev_sq.0, new_sq.1 - prev_sq.1);
        if diff.0 != 0 && diff.1 != 0 {
            // Diagonal: may not press through a corner.
            let (sx, sz) = (diff.0.signum(), diff.1.signum());
            let open_x = map.square_open(to_pos((new_sq.0 - sx, new_sq.1)));
            let open_z = map.square_open(to_pos((new_sq.0, new_sq.1 - sz)));
            if !open_x && !open_z {
                return Vec2::ZERO;
            }
        }
        return step;
    }
    // Blocked: try sliding sideways by up to a square.
    let speed = step.length();
    let try_move = |target: Vec2, offset: Vec2, max_disp: f32| -> Option<Vec2> {
        let mut off = (target + offset) - pos;
        if max_disp > 0.0 && off.length_squared() > max_disp * max_disp {
            off = off.normalize_or_zero() * max_disp;
        }
        let test = pos + off;
        (sq(test) != new_sq && map.square_open(test)).then_some(off)
    };
    let mut result = None;
    for n in 1..=SQUARE_SIZE as i32 {
        let n = n as f32;
        result = try_move(new_pos, right * n, 0.0).or_else(|| try_move(new_pos, -right * n, 0.0));
        if result.is_some() {
            break;
        }
    }
    let Some(moved) = result else {
        return Vec2::ZERO;
    };
    let open_sq = sq(pos + moved);
    if open_sq.0 != prev_sq.0 && open_sq.1 != prev_sq.1 {
        // Axis-aligned slide so the unit cannot clip a corner into a
        // trap: along the axis perpendicular to the facing
        // (`vecs[(facing - 1) % 2]` — facing east/west slides along Z).
        let along_z = facing % 2 == 1;
        let displacement = if along_z { moved.y } else { moved.x };
        let side = sign(displacement);
        let amount = (displacement * side).min(speed) * side;
        let offset = if along_z {
            Vec2::new(0.0, amount)
        } else {
            Vec2::new(amount, 0.0)
        };
        return if map.square_open(pos + offset) {
            offset
        } else {
            Vec2::ZERO
        };
    }
    if moved.length_squared() > speed * speed {
        return try_move(pos, moved, speed).unwrap_or(Vec2::ZERO);
    }
    moved
}

/// The order a unit is following, as the movement step sees it.
pub struct OrderView {
    /// `UNIT_HAS_MOVE_CMD`: the front command is a plain move (not a
    /// build order), which widens the goal tolerance while idling.
    pub has_move_cmd: bool,
    /// Nothing queued behind the order (`UNIT_CMD_QUE_SIZE <= 1`).
    pub last_command: bool,
    /// The unit may not drive this frame (stunned, deploying,
    /// attack-move holding): `HEADING_CHANGED_STUN` → `ChangeSpeed(0)`.
    pub hold: bool,
}

/// What a frame of [`step_mover`] produced.
pub struct StepResult {
    /// XZ displacement to apply (already gated by `UpdatePos`).
    pub step: Vec2,
    /// `Some(true)` arrived, `Some(false)` failed: the order is over.
    pub finished: Option<bool>,
}

/// `(currWayPoint, nextWayPoint)` of `path`, or QTPFS's temporary pair
/// while the path is pending (a square toward the goal,
/// `PathManager.cpp:1738`).
pub fn current_waypoints(path: Option<&MovePath>, pos: Vec2, goal: Vec2) -> (Vec2, Vec2) {
    match path {
        Some(p) if p.current < p.waypoints.len() => {
            let c = p.waypoints[p.current].xz();
            let n = p.waypoints[(p.current + 1).min(p.waypoints.len() - 1)].xz();
            (c, n)
        }
        Some(_) => (goal, goal),
        None => {
            let t = pos + (goal - pos).normalize_or_zero() * SQUARE_SIZE;
            (t, t)
        }
    }
}

/// One mover's frame, steps 1–3 of the module docs.
#[allow(clippy::too_many_arguments)]
pub fn step_mover(
    m: &mut GroundMover,
    fs: &FrameStats,
    pos: Vec3,
    order: &OrderView,
    mut path: Option<&mut MovePath>,
    map: &MoveMap,
    up: Vec3,
    avoidance: impl FnOnce(&mut GroundMover, Vec2) -> Vec2,
    ground_speed_mod: impl Fn(Vec2, Vec2) -> f32,
) -> StepResult {
    let pos2 = pos.xz();
    // `WantToStop()`: no path (the order is over).
    let want_to_stop = m.progress != Progress::Active || m.goal.is_none();
    let steering = !want_to_stop && !order.hold;

    // --- 1. UpdateObstacleAvoidance + FollowPath (GroundMoveType.cpp:
    // 1081, 1095) ---
    // Avoidance runs first, on the *previous* frame's `waypointDir` and
    // `atGoal` (`desired = atGoal ? flatFrontDir : waypointDir`); the
    // heading `FollowPath` then asks for is `lastAvoidanceDir`.
    let mut wanted_heading = m.heading;
    if steering {
        let desired = if m.at_goal { m.front() } else { m.waypoint_dir };
        avoidance(m, desired);
    }
    if steering {
        let goal = m.goal.unwrap_or(pos2);
        let (cwp, _) = current_waypoints(path.as_deref(), pos2, goal);
        m.prev_wp_dist = m.curr_wp_dist;
        m.curr_wp_dist = cwp.distance(pos2);

        let cur_goal_dist_sq = pos2.distance_squared(goal);
        let min_goal_dist = m.goal_tolerance(order.has_move_cmd);
        let spd_goal_dist_sq = (m.current_speed * 1.05).powi(2);
        let ffd = m.front();
        let vel = ffd * m.current_speed;
        m.at_goal |= cur_goal_dist_sq <= min_goal_dist * min_goal_dist;
        // Would overshoot the goal within this frame.
        m.at_goal |= cur_goal_dist_sq <= spd_goal_dist_sq
            && ffd.dot(goal - pos2) > 0.0
            && ffd.dot(goal - (pos2 + vel)) <= 0.0;
        m.at_end_of_path |= m.at_goal;

        if !m.at_goal {
            if m.idling {
                m.num_idling_updates = (m.num_idling_updates + 1).min(32768);
            } else {
                m.num_idling_updates = m.num_idling_updates.saturating_sub(1);
            }
        }
        if !m.at_end_of_path {
            if let Some(p) = path.as_deref_mut() {
                set_next_waypoint(m, fs, pos2, goal, p, order.has_move_cmd, map);
            }
        } else if m.at_goal {
            m.pathing_arrived = true;
        }
        let cwp = if m.at_end_of_path {
            goal
        } else {
            current_waypoints(path.as_deref(), pos2, goal).0
        };
        let d = cwp - pos2;
        if d.length_squared() > CMP_EPS * CMP_EPS {
            m.waypoint_dir = d.normalize();
        }
        let dir = m.last_avoidance_dir;
        if dir.length_squared() > 1e-8 {
            wanted_heading = Heading::from_vector(dir.x, dir.y);
        }
    } else {
        m.last_avoidance_dir = m.front();
    }

    // --- 2. ChangeHeading --- (not called at all while stopping or
    // stunned, and a no-op when already on the wanted heading, so the
    // turn speed keeps its last value in both cases, as in the engine)
    if steering && m.heading != wanted_heading {
        let d = delta_heading(
            wanted_heading,
            m.heading,
            fs.turn_rate,
            fs.turn_rate * TURN_ACCEL_FRACTION,
            &mut m.turn_speed,
        );
        m.heading = m.heading.wrapping_add(d);
    }

    // --- 3. ChangeSpeed + UpdateOwnerPos ---
    let inputs = SpeedInputs {
        pos: pos2,
        wanted_heading,
        last_command: order.last_command,
        ground_speed_mod: if steering {
            ground_speed_mod(pos2, m.front())
        } else {
            1.0
        },
    };
    let wanted_speed = if steering { fs.max_speed } else { 0.0 };
    let delta = change_speed(m, fs, wanted_speed, &inputs);
    // `CalcSpeedVectorExclGravity`: a speed above the maximum (an
    // impulse along the facing) decays through rolling resistance.
    let base_speed = if m.current_speed > fs.max_speed {
        let drag = drag_acceleration(m.velocity, m.mass, true, ROLLING_RESISTANCE_COEFF);
        fs.max_speed.max((m.velocity + drag).length())
    } else {
        m.current_speed
    };
    let new_speed = (base_speed + delta).max(0.0);
    // `owner->frontdir * speed`: the heading tilted onto the ground,
    // so slopes shorten the horizontal step by the pitch's cosine.
    m.velocity = attitude(m.heading, up).mul_vec3(Vec3::NEG_Z) * new_speed;
    let request = m.velocity.xz();
    let mut step = Vec2::ZERO;
    if request.length_squared() > 0.0 {
        let gated = update_pos(
            map,
            pos2,
            request,
            m.right(),
            m.heading.facing(),
            m.position_stuck,
        );
        if gated != request {
            // `UpdatePos` bent the move: get a new path if this persists.
            m.re_request_path(false);
        }
        if gated != Vec2::ZERO {
            step = gated;
            m.position_stuck = false;
        } else if m.position_stuck {
            // Stuck inside closed squares with nowhere open: move anyway
            // so the unit eventually gets out.
            step = request;
        }
    }
    m.current_speed = new_speed;

    // --- UpdatePreCollisions: Arrived / Fail ---
    let finished = if m.pathing_arrived {
        m.stop_engine(Progress::Done);
        Some(true)
    } else if m.pathing_failed {
        m.stop_engine(Progress::Failed);
        Some(false)
    } else {
        None
    };
    StepResult { step, finished }
}

/// `CanSetNextWayPoint` + `SetNextWayPoint` (GroundMoveType.cpp:2124,
/// 2310).
fn set_next_waypoint(
    m: &mut GroundMover,
    fs: &FrameStats,
    pos: Vec2,
    goal: Vec2,
    path: &mut MovePath,
    has_move_cmd: bool,
    map: &MoveMap,
) {
    if path.current >= path.waypoints.len() {
        return;
    }
    let (cwp, nwp) = current_waypoints(Some(path), pos, goal);
    let cwp_dist_sq = cwp.distance_squared(pos);
    // Within a square: always allowed to switch.
    let allow_skip = cwp_dist_sq < (SQUARE_SIZE - 1.0).powi(2);
    if !allow_skip {
        if !m.skip_waypoint {
            // Turn-radius check: a waypoint outside our turning circle
            // can be steered at and passed without slowing — keep it.
            // The circle's DIAMETER is used so paths don't snake.
            let turn_radius =
                (m.current_speed * fs.frames_to_turn() / TAU).max(m.current_speed * 1.05) * 2.0;
            let waypoint_dot = m.waypoint_dir.dot(m.front()).clamp(-1.0, 1.0);
            if m.curr_wp_dist > turn_radius {
                return;
            }
            // Inside, but straight ahead and not reached within a frame.
            if m.curr_wp_dist > SQUARE_SIZE.max(m.current_speed * 1.05) && waypoint_dot >= 0.995 {
                return;
            }
        }
        // Cut the corner only with a clear line to the NEXT waypoint.
        if !map.raw_search(pos, nwp) {
            return;
        }
    }
    let min_goal = m.goal_tolerance(has_move_cmd);
    m.at_end_of_path |= cwp.distance_squared(goal) <= min_goal * min_goal;
    if !m.at_end_of_path {
        let is_last = path.current + 1 >= path.waypoints.len();
        m.last_waypoint |= is_last && !path.reached_goal && cwp_dist_sq <= min_goal * min_goal;
        if m.last_waypoint {
            // Incomplete path: its last reachable point is reached.
            m.pathing_failed = true;
            return;
        }
    }
    if m.at_end_of_path {
        path.current = path.waypoints.len();
        return;
    }
    path.current = (path.current + 1).min(path.waypoints.len() - 1);
    m.skip_waypoint = false;
    m.limit_speed_for_turning = m.limit_speed_for_turning.saturating_sub(1);
    m.want_repath = false;
    // A structure on the new current or next waypoint's square (built
    // since the path was made): repath (GroundMoveType.cpp:2363-2383).
    let (cwp, nwp) = current_waypoints(Some(path), pos, goal);
    if map.structure_at(cwp) || map.structure_at(nwp) {
        m.re_request_path(false);
    }
}

/// The per-unit data [`movement_system`] reads and writes.
#[derive(bevy::ecs::query::QueryData)]
#[query_data(mutable)]
pub struct MoverData {
    entity: Entity,
    kind: &'static UnitType,
    stats: &'static UnitStats,
    transform: &'static mut Transform,
    mover: &'static mut GroundMover,
    target: Option<&'static MoveTarget>,
    path: Option<&'static mut MovePath>,
    queue: Option<&'static mut CommandQueue>,
    deployable: Option<&'static Deployable>,
    stunned: Has<Stunned>,
    boost: Option<&'static crate::units::mechanics::network_buffer::SpeedBoost>,
    attack_move: Has<AttackMoveActive>,
    aim: Has<AimTarget>,
    animator: Option<&'static crate::units::assets::animation::UnitAnimator>,
    pending_build: Has<PendingBuild>,
    emerging: Has<crate::units::lifecycle::spawning::Emerging>,
    lift: Option<&'static GroundLift>,
    team: Option<&'static TeamId>,
}

/// Steps 1–3 for every ground unit (see the module docs); flyers are
/// flown by `air_movement`.
#[allow(clippy::too_many_arguments)]
pub fn movement_system(
    mut commands: Commands,
    nav_set: Option<Res<NavGridSet>>,
    heightmap: Option<Res<Heightmap>>,
    circular_flow: Option<Res<CircularFlow>>,
    registry: Res<UnitRegistry>,
    budget: Option<Res<PathSearchBudget>>,
    mut query: Query<MoverData, Without<Dying>>,
    mut avoidees: Local<Vec<Avoidee>>,
    mut avoid_grid: Local<HashMap<(i32, i32), Vec<usize>>>,
    mut path_queue: ResMut<PathQueue>,
    mut impulses: ResMut<PendingImpulses>,
    mut rng_state: Local<u32>,
) {
    let nav = nav_set.as_deref();
    // Last frame's explosion impulses (`CUnit::ApplyImpulse`), before
    // anyone moves. Buildings and aircraft take none here
    // (`CStaticMoveType::CanApplyImpulse` is false; hover air has its own).
    if *rng_state == 0 {
        *rng_state = 0x9E37_79B9;
    }
    for (entity, impulse) in impulses.0.drain(..) {
        let Ok(mut u) = query.get_mut(entity) else {
            continue;
        };
        // `CanApplyImpulse` is false while being built.
        if u.stats.can_fly || u.stats.speed <= 0.0 || u.emerging {
            continue;
        }
        let pos = u.transform.translation;
        let normal = heightmap
            .as_deref()
            .map_or(Vec3::Y, |hm| hm.normal(pos.x, pos.z));
        let mass = u.mover.mass;
        let rand = crate::rng::next_f32(&mut rng_state);
        u.mover.apply_impulse(impulse / mass, normal, rand);
    }
    // Avoidance reads everyone's pose from before this frame's moves
    // (Spring evaluates it in the parallel traversal-plan pass).
    avoidees.clear();
    for bucket in avoid_grid.values_mut() {
        bucket.clear();
    }
    for u in &query {
        let m = u.mover;
        if u.stats.can_fly || u.stats.speed <= 0.0 {
            continue;
        }
        let pos = u.transform.translation.xz();
        avoid_grid
            .entry(cell_of(pos))
            .or_default()
            .push(avoidees.len());
        avoidees.push(Avoidee {
            entity: u.entity,
            pos,
            pos3: u.transform.translation,
            vel: m.velocity,
            front: m.front(),
            right: m.right(),
            owner_radius: m.owner_radius,
            mass: m.mass,
            team: u.team.map_or(0, |t| t.0),
            moving: m.is_moving(),
            crushable: false,
            flying: m.flying,
        });
    }
    let avoid = AvoidanceView {
        entries: &avoidees,
        cells: &avoid_grid,
    };
    let map_max = heightmap.as_deref().map(|hm| {
        let (w, d) = hm.world_size();
        Vec2::new(w - SQUARE_SIZE, d - SQUARE_SIZE)
    });
    // Path requests from the previous frame are answered now, before
    // anyone moves (QTPFS `PathManager::Update` precedes the unit
    // updates in `CGame::SimFrame`), from where each mover stands.
    let node_budget = budget.map_or(PATH_SEARCH_NODES_PER_FRAME, |b| b.0);
    let mut nodes_left = path_queue.service(node_budget, nav, &registry, |entity| {
        let u = query.get(entity).ok()?;
        let goal = u.mover.goal?;
        (u.mover.progress == Progress::Active && !u.stats.can_fly).then_some(PathRequest {
            kind: u.kind.0,
            xsizeh: u.mover.xsizeh,
            crush_strength: u.mover.crush_strength,
            from: u.transform.translation,
            to: Vec3::new(goal.x, 0.0, goal.y),
            goal_radius: u.mover.goal_radius + u.mover.extra_radius,
        })
    });

    for mut u in &mut query {
        if u.stats.can_fly {
            continue;
        }
        let speed = u.stats.speed + u.boost.map_or(0.0, |b| b.0);
        if u.stats.speed <= 0.0 {
            // Buildings can't move — drop any movement order.
            if u.target.is_some() || u.path.is_some() || u.queue.is_some() {
                commands
                    .entity(u.entity)
                    .remove::<(MoveTarget, MovePath, CommandQueue)>();
            }
            continue;
        }
        let m = &mut *u.mover;
        let pos = u.transform.translation;
        m.frame = m.frame.wrapping_add(1);
        let fs = FrameStats::new(u.stats, speed);
        let max_slope = registry.max_slope_ratio(u.kind.0);
        let map = MoveMap {
            nav,
            max_slope,
            xsizeh: m.xsizeh,
            crush_strength: m.crush_strength,
        };

        if m.skidding {
            // `UpdatePreCollisions`: a skidding unit only skids — no
            // path following, no collision response of its own.
            let hm = heightmap.as_deref();
            let lift = u.lift.map_or(0.0, |l| l.0);
            let (ground, normal) = hm.map_or((pos.y, Vec3::Y), |hm| {
                (hm.sample(pos.x, pos.z) + lift, hm.normal(pos.x, pos.z))
            });
            let mut new_pos = pos;
            m.update_skid(&mut new_pos, ground, normal);
            if !m.flying
                && let Some(hm) = hm
            {
                // `AdjustPosToWaterLine`.
                new_pos.y = hm.sample(new_pos.x, new_pos.z) + lift;
            }
            let rotation = m.skid_rotation() * attitude(m.heading, up_dir(m, hm, new_pos));
            let tf = &mut *u.transform;
            tf.translation = new_pos;
            tf.rotation = rotation;
            m.old_pos = new_pos;
            continue;
        }

        let more_moves = u.queue.as_deref().is_some_and(|q| !q.commands.is_empty());

        // --- Orders → StartMoving / StopMoving ---
        let clamp = |p: Vec3| {
            let g = p.xz();
            map_max.map_or(g, |mx| g.clamp(Vec2::ZERO, mx))
        };
        let goal = u.target.map(|t| clamp(t.0));
        match goal {
            Some(g) if m.goal != Some(g) => {
                let blocked = !map.square_open(g);
                m.start_moving(g, MOVE_GOAL_RADIUS, pos, blocked);
            }
            None if m.goal.is_some() => {
                // The order was taken away (stop, non-move order).
                m.stop_engine(Progress::Done);
            }
            _ => {}
        }

        // Legs that end before this frame's step: already at the goal
        // (`GetNewPath` refuses, the CAI finishes the command), an
        // emptied path (a system ending the leg), or — with more moves
        // queued — inside `cancelDistance` at the CAI's SlowUpdate
        // (`CMobileCAI::ExecuteMove`, MobileCAI.cpp:423), which chains
        // legs at speed.
        let path_emptied = u.path.as_deref().is_some_and(|p| p.waypoints.is_empty());
        let early = m.progress == Progress::Active
            && (m.at_goal
                || path_emptied
                || (m.is_slow_update()
                    && more_moves
                    && !u.pending_build
                    && m.goal.is_some_and(|g| {
                        g.distance_squared(pos.xz())
                            < cancel_distance_sq(fs.max_speed, fs.turn_rate)
                    })));
        if early {
            m.stop_engine(Progress::Done);
            finish_leg(
                &mut commands,
                u.entity,
                u.queue.as_deref_mut(),
                m,
                &map,
                pos,
                clamp,
            );
        }

        // --- Path requests (QTPFS `RequestPath`, answered in-frame
        // within the search budget; until then the unit keeps its old
        // path or steers at a temporary waypoint) ---
        if m.progress == Progress::Active
            && let Some(g) = m.goal
        {
            if let Some(p) = u.path.as_deref_mut() {
                if let Some(n) = nav
                    && p.revision != n.revision
                {
                    // Terrain or structures changed since the path was
                    // made: a path whose remaining nodes (from two
                    // before the current waypoint) touch the changed
                    // squares is dirty and re-searched a second later
                    // (`PathCache::MarkDeadPaths`, `QueueDeadPathSearches`).
                    let area = n.changed_since(p.revision);
                    p.revision = n.revision;
                    let from = p.current.max(2) - 2;
                    let dirty = match area {
                        None => true,
                        Some(b) => {
                            p.node_rects[from.min(p.node_rects.len())..]
                                .iter()
                                .any(|r| r.x0 <= b[2] && r.x1 > b[0] && r.z0 <= b[3] && r.z1 > b[1])
                                || p.waypoints[from.min(p.waypoints.len())..]
                                    // A raw path's two points share one leaf:
                                    // its segment is tested too
                                    // (`MarkDeadPaths`' `intersectsPath`).
                                    .windows(2)
                                    .any(|w| NavGridSet::segment_touches(b, w[0].xz(), w[1].xz()))
                        }
                    };
                    if dirty && p.dirty_at.is_none() {
                        p.dirty_at = Some(m.frame + DEAD_PATH_REFRESH_FRAMES);
                    }
                }
                if p.dirty_at.is_some_and(|f| m.frame >= f) {
                    p.dirty_at = None;
                    m.re_request_path(true);
                }
                // The repath trigger of a long partial path.
                if p.repath_at.is_some_and(|i| p.current >= i) {
                    p.repath_at = None;
                    m.re_request_path(true);
                }
            }
            let stale = u.path.as_deref().is_none_or(|p| p.goal.xz() != g);
            if (m.path_requested || stale) && nav.is_some() {
                // An answer from the service for the goal still wanted,
                // else search now while the frame's budget lasts.
                let outcome = match path_queue.results.remove(&u.entity) {
                    Some((searched_for, outcome)) if searched_for == g => Some(outcome),
                    _ => {
                        let req = PathRequest {
                            kind: u.kind.0,
                            xsizeh: m.xsizeh,
                            crush_strength: m.crush_strength,
                            from: pos,
                            to: Vec3::new(g.x, 0.0, g.y),
                            goal_radius: m.goal_radius + m.extra_radius,
                        };
                        let (outcome, left) =
                            path_queue.request_now(u.entity, req, nodes_left, nav, &registry);
                        nodes_left = left;
                        outcome
                    }
                };
                if let Some(outcome) = outcome {
                    m.path_requested = false;
                    match outcome {
                        Some(PathOutcome::Route(new_path)) => {
                            // `GetNewPath` / the path swap in
                            // `UpdateTraversalPlan`.
                            m.at_goal = false;
                            m.at_end_of_path = false;
                            m.last_waypoint = false;
                            m.want_repath = false;
                            match u.path.as_deref_mut() {
                                Some(p) => *p = new_path,
                                None => {
                                    commands.entity(u.entity).insert(new_path);
                                }
                            }
                        }
                        Some(PathOutcome::Unreachable) | None => {
                            // No path from here at all: `Fail`.
                            m.stop_engine(Progress::Failed);
                            finish_leg(
                                &mut commands,
                                u.entity,
                                u.queue.as_deref_mut(),
                                m,
                                &map,
                                pos,
                                clamp,
                            );
                        }
                    }
                }
            }
        }

        let hold = u.stunned
            || u.deployable.is_some_and(|d| d.state != DeployState::Closed)
            || (u.attack_move && u.aim);
        let order = OrderView {
            has_move_cmd: !u.pending_build,
            last_command: !u.queue.as_deref().is_some_and(|q| !q.commands.is_empty()),
            hold,
        };
        let up = up_dir(m, heightmap.as_deref(), pos);
        // A path belongs to the order only once it has been searched
        // for that goal; a stale one is still followed meanwhile.
        let result = step_mover(
            m,
            &fs,
            pos,
            &order,
            u.path.as_deref_mut().filter(|p| !p.waypoints.is_empty()),
            &map,
            up,
            |m, d| {
                let me = AvoiderInfo {
                    entity: u.entity,
                    pos: pos.xz(),
                    pos3: pos,
                    team: u.team.map_or(0, |t| t.0),
                    model_radius: u.stats.hit_radius,
                };
                obstacle_avoidance_dir(m, d, &me, &avoid)
            },
            |p, dir| ground_speed_mod(nav, heightmap.as_deref(), max_slope, p, dir),
        );

        let mut step = result.step;
        // Animation gate: a driver holds its unit while a fold
        // choreography must finish first (Byte: fold-to-move).
        step *= u.animator.map_or(1.0, |a| a.rig.move_gate);
        if let Some(flow) = circular_flow.as_deref() {
            step *= flow.step_multiplier(pos, Vec3::new(m.front().x, 0.0, m.front().y));
        }
        let mut new_pos = pos;
        new_pos.x += step.x;
        new_pos.z += step.y;
        if let Some(hm) = heightmap.as_deref() {
            new_pos.y = hm.sample(new_pos.x, new_pos.z) + u.lift.map_or(0.0, |l| l.0);
        }
        // An idle unit keeps its square: reuse the slope normal.
        let new_up = if new_pos.xz() == pos.xz() {
            up
        } else {
            up_dir(m, heightmap.as_deref(), new_pos)
        };
        let rotation = attitude(m.heading, new_up);
        // Only touch the `Transform` when the pose changed, so idle
        // units don't trigger change detection / propagation.
        if u.transform.translation != new_pos || u.transform.rotation != rotation {
            let tf = &mut *u.transform;
            tf.translation = new_pos;
            tf.rotation = rotation;
        }

        if result.finished.is_some() {
            // `Arrived` / `Fail` run the CAI's SlowUpdate right away.
            finish_leg(
                &mut commands,
                u.entity,
                u.queue.as_deref_mut(),
                m,
                &map,
                new_pos,
                clamp,
            );
        }
    }
}

/// Another ground mover as obstacle avoidance sees it.
#[derive(Clone, Copy)]
pub struct Avoidee {
    entity: Entity,
    pos: Vec2,
    /// `avoidee->pos`: the engine measures avoidee distances in 3D.
    pos3: Vec3,
    vel: Vec3,
    front: Vec2,
    right: Vec2,
    owner_radius: f32,
    mass: f32,
    team: u8,
    moving: bool,
    /// Something this mover could crush (never true for units in KP:
    /// `crushable` defaults to false).
    crushable: bool,
    /// `IsInAir() || IsFlying()`: launched by an impulse — not avoided.
    flying: bool,
}

/// The avoider's own identity for [`obstacle_avoidance_dir`].
pub struct AvoiderInfo {
    pub entity: Entity,
    pub pos: Vec2,
    pub pos3: Vec3,
    pub team: u8,
    /// `CSolidObject::radius` (the model radius).
    pub model_radius: f32,
}

/// The pre-move snapshot of ground movers, bucketed by cell.
pub struct AvoidanceView<'a> {
    entries: &'a [Avoidee],
    cells: &'a HashMap<(i32, i32), Vec<usize>>,
}

/// `groundUnitCollisionAvoidanceUpdateRate` default (ModInfo.cpp:44).
const AVOIDANCE_UPDATE_RATE: u32 = 3;

/// `CGroundMoveType::GetObstacleAvoidanceDir` (GroundMoveType.cpp:1837):
/// steer sideways away from moving units (and idle enemies) ahead —
/// within ±120° of the facing, closer than a second's travel and the
/// goal — weighted by their mass share, how head-on the two are and
/// how close, then blended with the wanted direction. Idle allies are
/// not avoided: collision response pushes them aside. Evaluated every
/// third frame per unit, reusing the last direction in between.
pub fn obstacle_avoidance_dir(
    m: &mut GroundMover,
    desired: Vec2,
    me: &AvoiderInfo,
    view: &AvoidanceView,
) -> Vec2 {
    const AVOIDER_DIR_WEIGHT: f32 = 1.0;
    const DESIRED_DIR_WEIGHT: f32 = 0.5;
    const LAST_DIR_MIX_ALPHA: f32 = 0.7;
    let max_avoidee_cosine = 120.0_f32.to_radians().cos();

    if !m.frame.is_multiple_of(AVOIDANCE_UPDATE_RATE) {
        if !m.avoiding_units {
            m.last_avoidance_dir = desired;
        }
        return m.last_avoidance_dir;
    }
    m.avoiding_units = false;
    m.last_avoidance_dir = desired;
    let front = m.front();
    // Facing away from where we want to go: normal steering first.
    if front.dot(desired) < 0.0 {
        return m.last_avoidance_dir;
    }
    let goal = m.goal.unwrap_or(me.pos);
    let right = m.right();
    let vel = m.velocity;
    let avoidance_radius = m.current_speed.max(1.0) * (me.model_radius * 2.0);
    let mut avoidance_vec = Vec2::ZERO;

    let (cx, cz) = cell_of(me.pos);
    let reach = (avoidance_radius / COLLISION_CELL).ceil() as i32;
    for dz in -reach..=reach {
        for dx in -reach..=reach {
            let Some(bucket) = view.cells.get(&(cx + dx, cz + dz)) else {
                continue;
            };
            for &i in bucket {
                let o = &view.entries[i];
                if o.entity == me.entity || o.crushable || o.flying {
                    continue;
                }
                if o.pos.distance_squared(me.pos) > avoidance_radius * avoidance_radius {
                    continue;
                }
                // Idle movable allies get pushed, not avoided.
                if !o.moving && o.team == me.team {
                    continue;
                }
                // `(avoider->pos + avoider->speed) - (avoidee->pos +
                // avoidee->speed)`: a 3D separation with the full
                // velocity vectors, so units on different terrain
                // heights count as further apart.
                let vector3 = (me.pos3 + vel) - (o.pos3 + o.vel);
                let vector = vector3.xz();
                let radius_sum = m.owner_radius + o.owner_radius;
                let mass_scale = o.mass / (m.mass + o.mass);
                let dist_sq = vector3.length_squared();
                let dist = dist_sq.sqrt() + 0.01;
                if front.dot(-(vector / dist)) < max_avoidee_cosine {
                    continue;
                }
                if dist_sq >= (m.current_speed.max(1.0) * GAME_SPEED + radius_sum).powi(2) {
                    continue;
                }
                if dist_sq >= me.pos.distance_squared(goal) {
                    continue;
                }
                let mut avoider_turn_sign = -sign(o.pos.dot(right) - me.pos.dot(right));
                let avoidee_turn_sign = -sign(me.pos.dot(o.right) - o.pos.dot(o.right));
                // Maximal when anti-parallel; both turn the same local
                // way then.
                let cos_angle = front.dot(o.front).clamp(-1.0, 1.0);
                let response = (1.0 - cos_angle) + 0.1;
                let fall_off = 1.0 - (dist / (5.0 * radius_sum)).min(1.0);
                if cos_angle < 0.0 {
                    avoider_turn_sign = avoider_turn_sign.max(avoidee_turn_sign);
                }
                avoidance_vec += right
                    * AVOIDER_DIR_WEIGHT
                    * avoider_turn_sign
                    * response
                    * fall_off
                    * mass_scale;
                m.avoiding_units = true;
            }
        }
    }
    let dir = desired
        .lerp(avoidance_vec, DESIRED_DIR_WEIGHT)
        .normalize_or_zero();
    let dir = dir
        .lerp(m.last_avoidance_dir, LAST_DIR_MIX_ALPHA)
        .normalize_or_zero();
    m.last_avoidance_dir = if dir == Vec2::ZERO { desired } else { dir };
    m.last_avoidance_dir
}

/// `CMoveMath::GetPosSpeedMod` for a tank/kbot (`MoveMath.cpp:107-139`,
/// `GroundMoveMath.cpp:31-50`): `1 / (1 + max(0, slope · dirSlopeMod)
/// · slopeMod)` where `dirSlopeMod = -dir · centerNormal2D` is +1 when
/// heading straight up the fall line — only climbing slows, downhill is
/// full speed. The slope is the half-resolution slope map's value for
/// the square (see `spring_pathfinding::slope_map`), the direction the
/// square's own centre normal. KP's MaxSlope 36 gives `slopeMod ≈ 9.7`, so a 30° climb
/// runs at ~0.43×. Impassable squares give 0; `ChangeSpeed` then looks
/// one square ahead so a unit on a closed square can drive out.
pub fn ground_speed_mod(
    nav: Option<&NavGridSet>,
    hm: Option<&Heightmap>,
    cap: f32,
    pos: Vec2,
    dir: Vec2,
) -> f32 {
    let (Some(nav), Some(hm)) = (nav, hm) else {
        return 1.0;
    };
    let at = |p: Vec2| -> f32 {
        let Some(slope) = nav.square_slope(cap, p) else {
            return 0.0;
        };
        let sq = (
            (p.x / SQUARE_SIZE).floor() as i32,
            (p.y / SQUARE_SIZE).floor() as i32,
        );
        let Some(n2) = hm.center_normal_2d(sq.0, sq.1) else {
            return 1.0;
        };
        // `dirSlopeMod = -moveDir.dot(sqrNormal)` (MoveMath.cpp:130):
        // positive heading up the fall line.
        let dir_slope = -dir.dot(n2);
        1.0 / (1.0 + (slope * dir_slope).max(0.0) * nav.slope_mod(cap))
    };
    let m = at(pos);
    if m == 0.0 {
        at(pos + dir * SQUARE_SIZE)
    } else {
        m
    }
}

/// The CAI side of a finished leg (`CMobileCAI::ExecuteMove` →
/// `FinishCommand`): promote the next queued command and, when it is a
/// move, start it now so the unit keeps its momentum; with nothing
/// queued the unit's path is dropped and it brakes to a stop.
fn finish_leg(
    commands: &mut Commands,
    entity: Entity,
    queue: Option<&mut CommandQueue>,
    m: &mut GroundMover,
    map: &MoveMap,
    pos: Vec3,
    clamp: impl Fn(Vec3) -> Vec2,
) {
    match promote_next_command(commands, entity, pos, queue) {
        Some(next) => {
            let g = clamp(next);
            m.start_moving(g, MOVE_GOAL_RADIUS, pos, !map.square_open(g));
        }
        None => {
            commands.entity(entity).remove::<MovePath>();
        }
    }
}

/// `GetWantedUpDir`: the smoothed ground normal, or straight up for
/// `upright` units (KP units have `upDirSmoothing = 0`: applied as is).
fn up_dir(m: &GroundMover, heightmap: Option<&Heightmap>, pos: Vec3) -> Vec3 {
    match heightmap {
        Some(hm) if !m.upright => hm.smooth_normal(pos.x, pos.z),
        _ => Vec3::Y,
    }
}

/// `CSolidObject::UpdateDirVectors` (SolidObject.cpp:435): the flat
/// heading rotated by the shortest arc from straight up to `up`.
pub fn attitude(heading: Heading, up: Vec3) -> Quat {
    let yaw = Transform::default()
        .looking_to(heading.to_vector3(), Vec3::Y)
        .rotation;
    Quat::from_rotation_arc(Vec3::Y, up.normalize_or(Vec3::Y)) * yaw
}

/// Squared `CMobileCAI::cancelDistance` (MobileCAI.cpp:1316): the
/// static turn radius (`AMoveType::CalcStaticTurnRadius`, MoveType.cpp:
/// 125 — `maxSpeed · (65536/turnRate) / 2π`) plus two squares, squared
/// and clamped to `[1024, 2048]` (32–45 elmos). Per-frame inputs,
/// `turn_rate` in heading units.
pub fn cancel_distance_sq(max_speed: f32, turn_rate: f32) -> f32 {
    let turn_radius = max_speed * (SPRING_CIRCLE_DIVS as f32 / turn_rate.max(1.0)) / TAU;
    (turn_radius + 2.0 * SQUARE_SIZE)
        .powi(2)
        .clamp(1024.0, 2048.0)
}

/// One ground unit, as the collision pass sees every other.
#[derive(Clone, Copy)]
pub struct CollisionEntry {
    entity: Entity,
    pos: Vec2,
    /// Full position: `CheckCollisionExclSAT` and the push vector
    /// measure separation in 3D.
    pos3: Vec3,
    radius: f32,
    owner_radius: f32,
    mobile: bool,
    /// A feature movers with enough crush strength drive over (Bad
    /// Block).
    crushable: bool,
    mass: f32,
    /// `speed.w` (elmos/frame).
    speed: f32,
    front: Vec2,
    progress: Progress,
    /// Has an order / queued commands (`UNIT_CMD_QUE_SIZE != 0`).
    has_commands: bool,
    curr_waypoint: Option<Vec2>,
    /// `beingBuilt`: still rising out of its factory. Pushes others
    /// but is never pushed itself, and runs no collision pass of its
    /// own (`HandleObjectCollisions` returns early for it).
    being_built: bool,
    /// `IsSkidding()`: skipped by the normal collision pass; met only
    /// through `CheckCollisionSkid`.
    skidding: bool,
    /// `owner->speed` and the model radius, for `CheckCollisionSkid`.
    vel: Vec3,
    hit_radius: f32,
}

/// Cell size of the collision broad-phase grid.
const COLLISION_CELL: f32 = 64.0;
/// Slack around a static collider's circle when filing it under every
/// cell it reaches: covers `CheckCollisionExclSAT`'s `0.01` tolerance
/// (≤ 0.1 elmo).
const STATIC_CELL_PAD: f32 = 1.0;

/// The collision pass's broad phase: movers filed under the cell of
/// their centre, static colliders (structures, features) under every
/// cell their circle reaches — so a mover's search only has to span the
/// largest *mobile* radius, not the largest building's.
#[derive(Default)]
pub struct CollisionGrid {
    mobile: HashMap<(i32, i32), Vec<usize>>,
    fixed: HashMap<(i32, i32), Vec<usize>>,
}

fn cell_of(p: Vec2) -> (i32, i32) {
    (
        (p.x / COLLISION_CELL).floor() as i32,
        (p.y / COLLISION_CELL).floor() as i32,
    )
}

/// `CGroundMoveType::CalculatePushVector` (GroundMoveType.cpp:2957)
/// with `allowUnitCollisionOverlap=1` (KP modrules): the push this
/// collider takes from one overlapping mobile collidee. Heavier and
/// faster parties (build-cost mass × speed in elmos/frame, boosted up
/// to 6× when hit side-on) push lighter/slower ones; the overlap
/// counts from `r1²/(r1+r2) + r2²/(r1+r2)`, so units may interpenetrate
/// down to that distance before pushing hard. A small sideways slide
/// (`1/penetration`) lets crowds flow past each other.
#[allow(clippy::too_many_arguments)]
pub fn push_vector(
    r1: f32,
    r2: f32,
    sep: Vec3,
    m1: f32,
    m2: f32,
    speed1: f32,
    speed2: f32,
    front1: Vec2,
    front2: Vec2,
    right1: Vec2,
) -> Vec2 {
    let rel1 = r1 / (r1 + r2);
    let rel2 = r2 / (r1 + r2);
    let radius_sum = r1 * rel1 + r2 * rel2;
    // `sepDistance = separationVector.Length() + 0.1` — 3D, so units
    // standing at different heights overlap less; the response itself
    // is applied in the plane (`sepDirection * XZVector`).
    let sep_distance = sep.length() + 0.1;
    let pen = (radius_sum - sep_distance).max(1.0);
    let response = (SQUARE_SIZE * 2.0).min(pen * 0.5);
    let sep_dir = (sep / sep_distance).xz();
    let v1 = speed1.max(1.0);
    let v2 = speed2.max(1.0);
    let c1 = 1.0 + (1.0 - front1.dot(-sep_dir).abs()) * 5.0;
    let c2 = 1.0 + (1.0 - front2.dot(sep_dir).abs()) * 5.0;
    let s1 = m1 * v1 * c1;
    let s2 = m2 * v2 * c2;
    let q1 = s1 / (s1 + s2 + 1.0);
    let q2 = s2 / (s1 + s2 + 1.0);
    let mass_scale = (1.0 - q1).clamp(0.01, 0.99) / rel1;
    let slide_sign = sign(sep.xz().dot(right1));
    sep_dir * response * mass_scale + right1 * slide_sign * (1.0 / pen) * q2
}

/// Steps 4–5 for every ground unit: collision response
/// (`HandleObjectCollisions` → `HandleUnitCollisions`,
/// `HandleStaticObjectCollision`), the `Update` that applies it,
/// `OwnerMoved`'s idle test and the per-unit `SlowUpdate`.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn ground_collision_system(
    mut commands: Commands,
    nav_set: Option<Res<NavGridSet>>,
    heightmap: Option<Res<Heightmap>>,
    registry: Res<UnitRegistry>,
    mut movers: Query<(
        Entity,
        &UnitType,
        &UnitStats,
        &mut Transform,
        &mut GroundMover,
        Option<&MovePath>,
        Has<MoveTarget>,
        Option<&mut CommandQueue>,
        Has<Dying>,
        Option<&GroundLift>,
        Has<crate::units::lifecycle::spawning::Emerging>,
    )>,
    mut entries: Local<Vec<CollisionEntry>>,
    mut grid: Local<CollisionGrid>,
    mut candidates: Local<Vec<(i32, i32, usize)>>,
    mut seen_revision: Local<u64>,
) {
    let nav = nav_set.as_deref();
    // Squares a structure change touched since the last pass (the trap
    // check's area), or everything when that is no longer remembered.
    let trapped_area = nav.and_then(|n| {
        if n.revision == *seen_revision {
            return None;
        }
        let area = n.changed_since(*seen_revision).or(Some([
            i32::MIN / 2,
            i32::MIN / 2,
            i32::MAX / 2,
            i32::MAX / 2,
        ]));
        *seen_revision = n.revision;
        area
    });
    entries.clear();
    let grid = &mut *grid;
    for bucket in grid.mobile.values_mut().chain(grid.fixed.values_mut()) {
        bucket.clear();
    }
    let mut max_mobile_radius = 0.0_f32;
    for (entity, kind, stats, tf, m, path, has_target, queue, dying, _, emerging) in &movers {
        if stats.can_fly || dying {
            continue;
        }
        let mobile = stats.speed > 0.0;
        let pos = tf.translation.xz();
        let radius = m.collision_radius;
        // A pending `TriggerSkipWayPoint` marks `currWayPoint.y = -2`,
        // so that waypoint no longer compares equal to anyone's in the
        // traffic-jam test (at the end of a path the mark stays).
        let curr_waypoint = if m.skip_waypoint {
            None
        } else {
            m.goal.map(|g| current_waypoints(path, pos, g).0)
        };
        let idx = entries.len();
        entries.push(CollisionEntry {
            entity,
            pos,
            pos3: tf.translation,
            radius,
            owner_radius: m.owner_radius,
            mobile,
            crushable: !mobile && registry.is_feature(kind.0),
            mass: m.mass,
            speed: m.current_speed,
            front: m.front(),
            progress: m.progress,
            has_commands: has_target || queue.is_some_and(|q| !q.commands.is_empty()),
            curr_waypoint,
            being_built: emerging,
            skidding: m.skidding,
            vel: m.velocity,
            hit_radius: stats.hit_radius,
        });
        if mobile {
            grid.mobile.entry(cell_of(pos)).or_default().push(idx);
            max_mobile_radius = max_mobile_radius.max(radius);
        } else {
            let (x0, z0) = cell_of(pos - Vec2::splat(radius + STATIC_CELL_PAD));
            let (x1, z1) = cell_of(pos + Vec2::splat(radius + STATIC_CELL_PAD));
            for cz in z0..=z1 {
                for cx in x0..=x1 {
                    grid.fixed.entry((cx, cz)).or_default().push(idx);
                }
            }
        }
    }
    let mut crushed: Vec<Entity> = Vec::new();
    // `CheckCollisionSkid` moves the units a skidding unit hits too;
    // applied after the pass (position and velocity deltas).
    let mut knocked: Vec<(Entity, Vec3)> = Vec::new();

    for (entity, kind, stats, mut tf, mut m, path, _, mut queue, dying, lift, emerging) in
        &mut movers
    {
        if stats.can_fly || stats.speed <= 0.0 || dying || emerging {
            continue;
        }
        if m.skidding {
            // `CheckCollisionSkid` (GroundMoveType.cpp:1677): sphere
            // overlap on the model radii. Statics (and features) throw
            // the skidder back at 1.8× the impact speed; mobile units
            // split the impact by mass and are shoved themselves.
            let r1 = stats.hit_radius;
            let mut pos3 = tf.translation;
            for o in entries.iter() {
                let sep = pos3 - o.pos3;
                let dist_sq = sep.length_squared();
                let total = r1 + o.hit_radius;
                if o.entity == entity || dist_sq >= total * total || dist_sq <= 0.01 {
                    continue;
                }
                let dir = sep / dist_sq.sqrt();
                if !o.mobile {
                    let impact = -m.velocity.dot(dir);
                    if impact <= 0.0 {
                        continue;
                    }
                    pos3 += dir * impact;
                    m.velocity += dir * impact * 1.8;
                } else {
                    let impact = (o.vel - m.velocity).dot(dir) * 0.5;
                    if impact <= 0.0 {
                        continue;
                    }
                    let rel_mass = m.mass / (m.mass + o.mass);
                    let collider_impulse = dir * (impact * (1.0 - rel_mass));
                    pos3 += collider_impulse;
                    m.velocity += collider_impulse;
                    knocked.push((o.entity, -dir * (impact * rel_mass)));
                }
            }
            tf.translation = pos3;
            continue;
        }
        let pos3 = tf.translation;
        let pos = pos3.xz();
        let map = MoveMap {
            nav,
            max_slope: registry.max_slope_ratio(kind.0),
            xsizeh: m.xsizeh,
            crush_strength: m.crush_strength,
        };
        let (cwp, nwp) = match m.goal {
            Some(g) => current_waypoints(path, pos, g),
            None => (pos, pos),
        };
        let right = m.right();
        let front = m.front();
        let mut force_moving = Vec2::ZERO;
        let mut force_static = Vec2::ZERO;
        let r1 = m.collision_radius;
        let (cx, cz) = cell_of(pos);
        // Every overlapping collider (`CheckCollisionExclSAT`: overlap
        // of the footprint circles — square footprints never need SAT),
        // keyed by its centre cell (row-major) then snapshot index: the
        // order a single centre-bucketed grid scan would visit them.
        candidates.clear();
        let mut gather = |cells: &HashMap<(i32, i32), Vec<usize>>, search: f32| {
            let reach = (search / COLLISION_CELL).ceil() as i32;
            for dz in -reach..=reach {
                for dx in -reach..=reach {
                    let Some(bucket) = cells.get(&(cx + dx, cz + dz)) else {
                        continue;
                    };
                    for &i in bucket {
                        let o = &entries[i];
                        let r2 = o.radius;
                        // `CheckCollisionExclSAT`: `|p1 - p2|² - (r1 + r2)²
                        // <= 0.01` on the full 3D positions. Skidding
                        // collidees are met only through their own skid.
                        if o.entity == entity
                            || o.skidding
                            || (pos3 - o.pos3).length_squared() - (r1 + r2) * (r1 + r2) > 0.01
                        {
                            continue;
                        }
                        let (ox, oz) = cell_of(o.pos);
                        candidates.push((oz, ox, i));
                    }
                }
            }
        };
        gather(&grid.mobile, m.current_speed + r1 + max_mobile_radius);
        gather(&grid.fixed, m.current_speed + r1);
        candidates.sort_unstable();
        candidates.dedup();
        let mut request_path = false;
        for &(_, _, i) in candidates.iter() {
            let o = entries[i];
            let sep = pos3 - o.pos3;
            let r2 = o.radius;
            if o.mobile {
                collision_aux(&mut m, pos, cwp, nwp, &o);
                // A collidee still being built is never pushed, so the
                // collider takes the whole response (`moveCollider =
                // pushCollider || !pushCollidee`).
                force_moving += push_vector(
                    r1,
                    r2,
                    sep,
                    m.mass,
                    o.mass,
                    m.current_speed,
                    if o.being_built { 0.0 } else { o.speed },
                    front,
                    o.front,
                    right,
                );
            } else if o.crushable && crushes_features(m.crush_strength) {
                // `HandleFeatureCollisions`: a feature we are
                // not crush-resistant against is crushed
                // (`FeatureCrushEvents` → `Kill`); its squares
                // never blocked us.
                crushed.push(o.entity);
            } else {
                // Structure (always static): strafe round its
                // blocked yardmap squares.
                let f = static_square_push(&m, pos, map.nav, fs_max_speed(stats));
                force_static += f;
                if f != Vec2::ZERO {
                    m.limit_speed_for_turning = 2;
                    if !m.at_end_of_path && !m.at_goal {
                        request_path = true;
                    }
                }
            }
        }
        if request_path {
            m.re_request_path(false);
        }

        // `UnitTrapCheckSystem`: a structure appeared since the last
        // pass — units whose footprint it covers re-test their square.
        if let Some(bbox) = trapped_area {
            let (sx, sz) = (
                (pos.x / SQUARE_SIZE).floor() as i32,
                (pos.y / SQUARE_SIZE).floor() as i32,
            );
            let h = m.xsizeh;
            if sx + h >= bbox[0] && sx - h <= bbox[2] && sz + h >= bbox[1] && sz - h <= bbox[3] {
                m.force_static_object_check = true;
            }
        }
        // `forceStaticObjectCheck` (HandleObjectCollisions, GroundMoveType
        // .cpp:2540): standing on a closed square or inside a structure's
        // footprint makes the unit `positionStuck`, so every step is
        // re-checked and — with no open square beside it — taken anyway
        // until it is out.
        if m.force_static_object_check {
            if let Some(n) = map.nav {
                m.position_stuck |= !n.passable(map.max_slope, pos.x, pos.y)
                    || n.footprint_blocked(pos.x, pos.y, m.xsizeh, m.crush_strength);
            }
            m.force_static_object_check = false;
        }

        // `Update`: apply what `UpdatePos` lets through.
        let try_force = force_static + force_moving;
        let mut applied = Vec2::ZERO;
        if try_force != Vec2::ZERO {
            applied = update_pos(
                &map,
                pos,
                try_force,
                right,
                m.heading.facing(),
                m.position_stuck,
            );
            if applied == Vec2::ZERO && m.position_stuck {
                applied = force_static;
            }
        }
        if applied != Vec2::ZERO {
            tf.translation.x += applied.x;
            tf.translation.z += applied.y;
            if let Some(hm) = heightmap.as_deref() {
                tf.translation.y =
                    hm.sample(tf.translation.x, tf.translation.z) + lift.map_or(0.0, |l| l.0);
            }
        }

        // `OwnerMoved` (GroundMoveType.cpp:578).
        let new_pos = tf.translation;
        let pos_dif = new_pos - m.old_pos;
        // `cmpEps = (cmp_eps, cmp_eps · 0.01, cmp_eps)`.
        let eps_y = CMP_EPS * 0.01;
        if pos_dif.x.abs() <= CMP_EPS && pos_dif.y.abs() <= eps_y && pos_dif.z.abs() <= CMP_EPS {
            // Didn't move: speed is lost, and not moving toward an
            // unreached goal counts as idling.
            m.current_speed = 0.0;
            m.velocity = Vec3::ZERO;
            m.idling = !m.at_goal;
        } else {
            m.old_pos = new_pos;
            let dif_sq = pos_dif.length_squared();
            let ffd = m.front() * dif_sq * 0.5;
            // The engine's height test (`|posDif.y| < |cmpEps.y · pos.y|`)
            // means a unit climbing or descending never counts as idle.
            m.idling = pos_dif.y.abs() < (eps_y * new_pos.y).abs()
                && (m.curr_wp_dist - m.prev_wp_dist).powi(2) < ffd.dot(m.waypoint_dir)
                && dif_sq < (m.current_speed * 0.5).powi(2);
        }

        // `SlowUpdate` (GroundMoveType.cpp:767).
        if m.is_slow_update() && m.progress == Progress::Active {
            if m.idling {
                m.num_idling_slow_updates =
                    (m.num_idling_slow_updates + 1).min(MAX_IDLING_SLOWUPDATES);
            } else {
                m.num_idling_slow_updates = m.num_idling_slow_updates.saturating_sub(1);
            }
            let fs = FrameStats::new(stats, stats.speed);
            let mut fail = false;
            if m.num_idling_updates as f32 > MAX_HEADING / fs.turn_rate {
                // Has a path but isn't getting anywhere.
                if m.num_idling_slow_updates < MAX_IDLING_SLOWUPDATES {
                    if m.idling {
                        m.force_static_object_check = true;
                        m.re_request_path(true);
                    }
                } else {
                    // Stuck on a closed square / in a crowd: give up.
                    fail = true;
                }
            }
            if !fail && m.want_repath {
                // Give the unit a chance to make progress before paying
                // for a path (resolution: a tenth of an elmo).
                let cur_dist = (cwp.distance(new_pos.xz()) * 10.0).floor() / 10.0;
                if cur_dist < m.best_last_waypoint_dist {
                    m.best_last_waypoint_dist = cur_dist;
                    m.want_repath_frame = m.frame;
                }
                let time_for_repath = m.frame >= m.want_repath_frame + REPATH_DELAY_FRAMES
                    && (m.frame >= m.last_repath_frame + REPATH_MAX_RATE_FRAMES || m.last_waypoint);
                if time_for_repath {
                    if m.last_waypoint {
                        m.best_last_waypoint_dist /= SQUARE_SIZE;
                        if m.best_last_waypoint_dist < m.best_reattempted_last_waypoint_dist {
                            m.last_waypoint = false;
                            m.best_reattempted_last_waypoint_dist = m.best_last_waypoint_dist;
                        } else {
                            m.best_reattempted_last_waypoint_dist = f32::INFINITY;
                        }
                    }
                    if !m.last_waypoint {
                        m.re_request_path(true);
                    } else {
                        fail = true;
                    }
                }
            }
            if fail {
                m.stop_engine(Progress::Failed);
                commands.entity(entity).remove::<MovePath>();
                promote_next_command(&mut commands, entity, new_pos, queue.as_deref_mut());
            }
        }
    }
    for (entity, impulse) in knocked {
        if let Ok((_, _, _, mut tf, mut m, ..)) = movers.get_mut(entity) {
            // `collidee->Move(-impulse); SetVelocityAndSpeed(speed - impulse)`.
            tf.translation += impulse;
            m.velocity += impulse;
            m.current_speed = m.velocity.length();
        }
    }
    crushed.sort();
    crushed.dedup();
    for e in crushed {
        // A crushed feature just vanishes (no unit death explosion).
        // `try_insert`: weapon damage may have killed and cleaned it
        // up in this same tick.
        commands.entity(e).try_insert(Dying { timer: 0.0 });
    }
}

fn fs_max_speed(stats: &UnitStats) -> f32 {
    stats.speed / GAME_SPEED
}

/// `HandleStaticObjectCollision`'s yardmap branch (GroundMoveType.cpp:
/// 2616-2735) against a colliding structure: every `BLOCK_STRUCTURE`
/// square in the collider's footprint window around `pos + speed`
/// (at least 3×3) that isn't behind it adds a sideways "bounce", and
/// the average square pulls a "strafe" away from it — both along
/// `rightdir`, each `max(0.1, -pen/2)` capped by `maxSpeed`. (The
/// engine's push-out term for squares inside the footprint compares
/// loop offsets with absolute squares and never fires, so it is not
/// ported.)
fn static_square_push(
    m: &GroundMover,
    pos: Vec2,
    nav: Option<&NavGridSet>,
    max_speed: f32,
) -> Vec2 {
    let Some(nav) = nav else { return Vec2::ZERO };
    // Radius of a square: sqrt(2·4²).
    const SQUARE_RADIUS: f32 = 5.656_854;
    let right = m.right();
    let vel = m.front() * m.current_speed;
    let xmid = ((pos.x + vel.x) / SQUARE_SIZE).floor() as i32;
    let zmid = ((pos.y + vel.y) / SQUARE_SIZE).floor() as i32;
    let ext = m.xsizeh.max(1);
    let mut bounce = Vec2::ZERO;
    let mut pen_sum = 0.0;
    let mut count = 0.0;
    let mut pos_sum = Vec2::ZERO;
    for dz in -ext..=ext {
        for dx in -ext..=ext {
            let (x, z) = (xmid + dx, zmid + dz);
            if !nav.structure_square(x, z, m.crush_strength) {
                continue;
            }
            let square = Vec2::new(
                x as f32 * SQUARE_SIZE + SQUARE_SIZE * 0.5,
                z as f32 * SQUARE_SIZE + SQUARE_SIZE * 0.5,
            );
            let sv = pos - square;
            let sep = sv.length() + 0.1;
            let pen = (sep - (m.collision_radius + SQUARE_RADIUS)).min(0.0);
            // Ignore squares behind us (relative to the velocity).
            if sv.dot(vel) > 0.0 {
                continue;
            }
            bounce += right * right.dot(sv / sep);
            pen_sum += pen;
            count += 1.0;
            pos_sum += square;
        }
    }
    if count == 0.0 {
        return Vec2::ZERO;
    }
    let avg_pos = pos_sum / count;
    let avg_pen = pen_sum / count;
    let strafe_sign = -sign(avg_pos.dot(right) - pos.dot(right));
    let bounce_sign = sign(right.dot(bounce));
    let scale = max_speed.min((-avg_pen * 0.5).max(0.1));
    right * strafe_sign * scale + right * bounce_sign * scale
}

/// `HandleUnitCollisionsAux` (GroundMoveType.cpp:282): colliding with
/// a unit that already sits on our goal or waypoint counts as reaching
/// it, which ends long pushing contests in crowds.
fn collision_aux(m: &mut GroundMover, pos: Vec2, cwp: Vec2, nwp: Vec2, o: &CollisionEntry) {
    if !m.is_moving() || m.progress != Progress::Active {
        return;
    }
    let Some(goal) = m.goal else { return };
    let at_goal_pos = |p: Vec2, r: f32| p.distance_squared(goal) < r * r;
    match o.progress {
        Progress::Done => {
            if o.speed > 0.01 || o.has_commands {
                return;
            }
            if at_goal_pos(o.pos, o.owner_radius) {
                m.trigger_call_arrived();
            } else if cwp.distance_squared(o.pos) <= o.owner_radius * o.owner_radius {
                m.skip_waypoint = true;
            }
        }
        Progress::Active => {
            // Traffic jam: both following the same waypoints.
            if o.curr_waypoint == Some(nwp) {
                m.skip_waypoint = true;
                return;
            }
            // Large units can surround a waypoint diagonally without
            // any of them touching it.
            const ADJUST_FOR_DIAGONAL: f32 = 1.45;
            if at_goal_pos(pos, m.owner_radius * ADJUST_FOR_DIAGONAL)
                || at_goal_pos(o.pos, o.owner_radius * ADJUST_FOR_DIAGONAL)
            {
                m.trigger_call_arrived();
            } else if cwp.distance_squared(o.pos) <= (o.owner_radius * ADJUST_FOR_DIAGONAL).powi(2)
            {
                m.skip_waypoint = true;
            }
        }
        Progress::Failed => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::movement_harness::Harness;

    fn bit_frame_stats() -> FrameStats {
        let reg = UnitRegistry::load();
        let stats = UnitStats::from_registry(UnitKind::Bit, &reg, 20.0);
        FrameStats::new(&stats, stats.speed)
    }

    fn mover_speed(h: &Harness, e: Entity) -> f32 {
        h.world.get::<GroundMover>(e).unwrap().current_speed * GAME_SPEED
    }

    /// A Bit (`Acceleration=0.9` elmo/frame², `MaxVelocity=3`) reaches
    /// top speed in 4 frames (`GetDeltaSpeed` adds `min(Δ, accRate)`).
    #[test]
    fn bit_reaches_top_speed_in_four_frames() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(400.0, 0.0, 400.0));
        h.step();
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(1400.0, 0.0, 400.0)));
        let speeds: Vec<f32> = (0..5)
            .map(|_| {
                h.step();
                mover_speed(&h, e)
            })
            .collect();
        assert!(
            (speeds[0] - 27.0).abs() < 0.1,
            "0.9 elmo/frame after one frame: {speeds:?}"
        );
        assert!(
            speeds[2] < 90.0 && (speeds[3] - 90.0).abs() < 1e-3,
            "{speeds:?}"
        );
    }

    /// With nothing queued, Spring brakes inside the braking distance
    /// and stops at the goal without overshooting it.
    #[test]
    fn last_command_brakes_to_a_stop_at_the_goal() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(400.0, 0.0, 400.0));
        h.step();
        let goal = Vec3::new(600.0, 0.0, 400.0);
        h.world.entity_mut(e).insert(MoveTarget(goal));
        let mut max_x = 0.0f32;
        for _ in 0..150 {
            h.step();
            max_x = max_x.max(h.pos(e).x);
        }
        assert!(!h.has_order(e), "order finished");
        assert_eq!(mover_speed(&h, e), 0.0);
        assert!(
            (h.pos(e).x - goal.x).abs() <= MOVE_GOAL_RADIUS + 4.0,
            "stops at the goal: {}",
            h.pos(e)
        );
        assert!(max_x <= goal.x + 4.0, "no overshoot: {max_x}");
    }

    /// With another move queued the unit neither brakes for the goal
    /// (`startBraking` needs `UNIT_CMD_QUE_SIZE <= 1`) nor stops between
    /// legs: MobileCAI finishes the leg inside `cancelDistance`.
    #[test]
    fn queued_legs_chain_at_full_speed() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(400.0, 0.0, 400.0));
        h.step();
        h.queue_move(e, Vec3::new(600.0, 0.0, 400.0));
        h.queue_move(e, Vec3::new(800.0, 0.0, 400.0));
        for _ in 0..8 {
            h.step();
        }
        let mut min_speed = f32::MAX;
        let mut second_leg = false;
        for _ in 0..60 {
            h.step();
            min_speed = min_speed.min(mover_speed(&h, e));
            if h.world.get::<MoveTarget>(e).is_some_and(|t| t.0.x == 800.0) {
                second_leg = true;
                break;
            }
        }
        assert!(second_leg, "second leg promoted");
        assert!(min_speed > 89.0, "no braking between legs: {min_speed}");
        let x = h.pos(e).x;
        assert!(
            (600.0 - 46.0..=600.0).contains(&x),
            "promoted inside cancelDistance: x={x}"
        );
    }

    /// Turning slows to `maxSpeed · clamp(maxTurn/reqTurn, 0.1, 1)`: a
    /// Bit ordered straight behind itself keeps ≥10% speed and arcs
    /// round instead of pivoting at a standstill, and the turn takes
    /// Spring's time (TurnRate 480 → ~2.6°/frame, not 3× that).
    #[test]
    fn reversing_arcs_at_reduced_speed() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(800.0, 0.0, 800.0));
        h.step();
        // Get up to speed heading +X.
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(1800.0, 0.0, 800.0)));
        for _ in 0..10 {
            h.step();
        }
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(100.0, 0.0, 800.0)));
        let fs = bit_frame_stats();
        let mut frames_turning = 0;
        let mut min_speed = f32::MAX;
        for _ in 0..200 {
            h.step();
            let m = h.world.get::<GroundMover>(e).unwrap();
            let err = (m.heading.wrapping_sub(Heading::from_vector(-1.0, 0.0)) as f32
                * SHORT_ANGLE_TO_RAD)
                .abs();
            if err < 0.05 {
                break;
            }
            frames_turning += 1;
            min_speed = min_speed.min(m.current_speed);
        }
        // bit.fbi TurnRate=480 heading units/frame.
        assert_eq!(fs.turn_rate, 480.0);
        let expected = MAX_HEADING / fs.turn_rate;
        assert!(
            (frames_turning as f32) > expected * 0.9 && (frames_turning as f32) < expected * 1.6,
            "half turn takes ~{expected:.0} frames, took {frames_turning}"
        );
        assert!(
            min_speed >= fs.max_speed * 0.1 - 1e-3,
            "keeps ≥10% speed: {min_speed}"
        );
    }

    /// `CanSetNextWayPoint`: inside the turning circle with a clear line
    /// to the next waypoint, the current one is skipped (corner cut);
    /// with the line blocked it is not.
    #[test]
    fn waypoint_skipped_only_with_line_of_sight() {
        let fs = bit_frame_stats();
        let reg = UnitRegistry::load();
        let stats = UnitStats::from_registry(UnitKind::Bit, &reg, 20.0);
        let run = |block: bool| {
            let mut speed_map = spring_pathfinding::SpeedMap::uniform(64, 64, 1.0);
            if block {
                // Wall between the unit and the next waypoint.
                for z in 0..40 {
                    speed_map.speeds[(z * 64 + 20) as usize] = 0.0;
                }
            }
            let nav = NavGridSet {
                buckets: vec![super::super::movement::NavBucket {
                    max_slope: 1.0,
                    speed_map,
                }],
                ..Default::default()
            };
            let mut m = GroundMover::new(UnitKind::Bit, &reg, &stats);
            m.current_speed = fs.max_speed;
            m.heading = Heading::from_vector(1.0, 0.0);
            let pos = Vec3::new(100.0, 0.0, 200.0);
            let goal = Vec3::new(300.0, 0.0, 100.0);
            m.start_moving(goal.xz(), MOVE_GOAL_RADIUS, pos, false);
            let mut path = MovePath::new(
                vec![
                    pos,
                    Vec3::new(140.0, 0.0, 200.0),
                    Vec3::new(260.0, 0.0, 320.0),
                    goal,
                ],
                goal,
            );
            path.current = 1;
            let map = MoveMap {
                nav: Some(&nav),
                max_slope: 1.0,
                xsizeh: 1,
                crush_strength: 0.0,
            };
            let order = OrderView {
                has_move_cmd: true,
                last_command: true,
                hold: false,
            };
            step_mover(
                &mut m,
                &fs,
                pos,
                &order,
                Some(&mut path),
                &map,
                Vec3::Y,
                |_, d| d,
                |_, _| 1.0,
            );
            path.current
        };
        assert_eq!(
            run(false),
            2,
            "40 elmos ahead, inside the turn circle, clear LOS: skip"
        );
        assert_eq!(run(true), 1, "LOS to the next waypoint blocked: keep");
    }

    /// `CalculatePushVector` with `allowUnitCollisionOverlap`: two Bits
    /// only push hard once closer than `r1²/(r1+r2)+r2²/(r1+r2)` = 12
    /// elmos (below that the push is the half-elmo minimum); a heavy fast
    /// collider hardly yields to a light idle one, which takes most of
    /// the push.
    #[test]
    fn push_vector_weights_mass_speed_and_overlap() {
        let (f, r) = (Vec2::X, Vec2::new(0.0, 1.0));
        // Separation along the facing, so the sideways slide term
        // (along `right`) doesn't enter the measured component.
        let push = |sep: f32, m1, m2, v1, v2| {
            push_vector(
                12.0,
                12.0,
                Vec3::new(sep, 0.0, 0.0),
                m1,
                m2,
                v1,
                v2,
                f,
                f,
                r,
            )
            .x
        };
        // Equal idle Bits: minimum-penetration push above 12 elmos apart,
        // growing below.
        let far = push(20.0, 10.0, 10.0, 0.0, 0.0);
        let near = push(6.0, 10.0, 10.0, 0.0, 0.0);
        assert!((far - 0.5).abs() < 0.05, "min push at 20 elmos: {far}");
        assert!(near > 2.5, "overlap below 12 elmos pushes harder: {near}");
        // Byte (mass 100) at speed 1.5/frame vs an idle Bit (mass 10).
        let byte_yields = push(6.0, 100.0, 10.0, 1.5, 0.0);
        let bit_yields = push(-6.0, 10.0, 100.0, 0.0, 1.5).abs();
        assert!(
            bit_yields > 5.0 * byte_yields,
            "bit {bit_yields} vs byte {byte_yields}"
        );
    }

    /// `update_pos` slides along a closed square instead of freezing,
    /// and refuses a diagonal squeeze between two closed squares.
    #[test]
    fn update_pos_slides_and_refuses_corner_squeeze() {
        let mut speed_map = spring_pathfinding::SpeedMap::uniform(8, 8, 1.0);
        speed_map.speeds[(2 * 8 + 3) as usize] = 0.0; // (3,2)
        let nav = NavGridSet {
            buckets: vec![super::super::movement::NavBucket {
                max_slope: 1.0,
                speed_map,
            }],
            ..Default::default()
        };
        let map = MoveMap {
            nav: Some(&nav),
            max_slope: 1.0,
            xsizeh: 1,
            crush_strength: 0.0,
        };
        // Heading +X into (3,2) from (2,2): slides to an open neighbour.
        let pos = Vec2::new(23.0, 20.0);
        let step = Vec2::new(2.0, 0.0);
        let right = Vec2::new(0.0, 1.0);
        let out = update_pos(&map, pos, step, right, 1, false);
        assert_ne!(out, Vec2::ZERO, "slides instead of freezing");
        let end = pos + out;
        assert!(map.square_open(end));
        // Diagonal between (3,2) closed and (2,3) closed.
        let mut speed_map = spring_pathfinding::SpeedMap::uniform(8, 8, 1.0);
        speed_map.speeds[(2 * 8 + 3) as usize] = 0.0;
        speed_map.speeds[(3 * 8 + 2) as usize] = 0.0;
        let nav = NavGridSet {
            buckets: vec![super::super::movement::NavBucket {
                max_slope: 1.0,
                speed_map,
            }],
            ..Default::default()
        };
        let map = MoveMap {
            nav: Some(&nav),
            max_slope: 1.0,
            xsizeh: 1,
            crush_strength: 0.0,
        };
        let d = Vec2::new(2.0, 2.0);
        assert_eq!(
            update_pos(
                &map,
                Vec2::new(23.0, 23.0),
                d,
                right,
                Heading::from_vector(d.x, d.y).facing(),
                false
            ),
            Vec2::ZERO
        );
    }

    /// A unit pressing against terrain it cannot pass (a path made stale
    /// under it) counts as idling, re-requests a path, walks the new one
    /// and arrives.
    #[test]
    fn stuck_unit_repaths_around_the_obstacle() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(400.0, 0.0, 400.0));
        h.step();
        let goal = Vec3::new(600.0, 0.0, 400.0);
        h.world.entity_mut(e).insert(MoveTarget(goal));
        h.step();
        // Close a wall across the straight path after it was planned.
        {
            let mut nav = h.world.resource_mut::<NavGridSet>();
            let map = &mut nav.buckets[0].speed_map;
            for z in 40..62 {
                map.speeds[(z * map.width + 62) as usize] = 0.0;
            }
            // Terrain edits announce their squares (Hex Farm does).
            nav.bump([62, 40, 62, 61]);
        }
        let mut repathed = false;
        for _ in 0..900 {
            h.step();
            let p = h.world.get::<MovePath>(e);
            if p.is_some_and(|p| p.waypoints.len() > 2) {
                repathed = true;
            }
            if !h.has_order(e) {
                break;
            }
        }
        assert!(repathed, "a detour path was searched");
        assert!(!h.has_order(e), "arrived after repathing");
        assert!(
            h.pos(e).xz().distance(goal.xz()) < 30.0,
            "at the goal: {}",
            h.pos(e)
        );
    }

    /// `owner->speed` is the velocity along the tilted front direction:
    /// on a slope it has a vertical component, so the avoidance
    /// prediction `(pos + speed)` sees a climbing unit higher up than
    /// its flat speed would place it.
    #[test]
    fn velocity_follows_the_slope() {
        let mut h = Harness::flat();
        let verts = 257usize;
        let rise = 8.0 * 30.0_f32.to_radians().tan();
        let heights: Vec<f32> = (0..verts * verts)
            .map(|i| (i % verts) as f32 * rise)
            .collect();
        h.world
            .insert_resource(Heightmap::from_raw(heights, verts, verts));
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(600.0, 0.0, 1000.0));
        h.step();
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(1400.0, 0.0, 1000.0)));
        for _ in 0..30 {
            h.step();
        }
        let m = h.world.get::<GroundMover>(e).unwrap();
        let v = m.velocity;
        assert!(v.x > 0.0 && v.y > 0.0, "climbing +X: {v}");
        assert!(
            (v.y / v.x - 30.0_f32.to_radians().tan()).abs() < 0.05,
            "{v}"
        );
        assert!((v.length() - m.current_speed).abs() < 1e-3, "{v}");
    }

    /// `CanApplyImpulse`: an impulse along the facing only adds speed,
    /// which then decays back to the maximum (`CalcSpeedVectorExclGravity`
    /// drag); a sideways one starts a skid that `UpdateSkid` brakes at
    /// 0.35 elmos/frame² and ends once the velocity is back along the
    /// facing (here: when it has died down), after which normal driving
    /// resumes.
    #[test]
    fn impulses_add_speed_along_the_facing_and_skid_sideways() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(400.0, 0.0, 400.0));
        h.step();
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(1400.0, 0.0, 400.0)));
        for _ in 0..10 {
            h.step();
        }
        let mass = h.world.get::<GroundMover>(e).unwrap().mass;
        // Along +X (the facing): a shove of 2 elmos/frame.
        h.world
            .resource_mut::<PendingImpulses>()
            .0
            .push((e, Vec3::new(2.0 * mass, 0.0, 0.0)));
        h.step();
        let m = h.world.get::<GroundMover>(e).unwrap();
        assert!(!m.skidding, "no skid along the facing");
        assert!(
            m.current_speed > 3.0 && m.current_speed < 5.0,
            "{}",
            m.current_speed
        );
        for _ in 0..60 {
            h.step();
        }
        assert!(
            (mover_speed(&h, e) - 90.0).abs() < 0.5,
            "decayed back to max"
        );

        // Sideways (+Z): a skid.
        let z0 = h.pos(e).z;
        h.world
            .resource_mut::<PendingImpulses>()
            .0
            .push((e, Vec3::new(0.0, 0.0, 3.0 * mass)));
        h.step();
        assert!(h.world.get::<GroundMover>(e).unwrap().skidding);
        let mut frames = 0;
        while h.world.get::<GroundMover>(e).unwrap().skidding && frames < 60 {
            h.step();
            frames += 1;
        }
        assert!(frames < 20, "3 elmos/frame at 0.35/frame braking: {frames}");
        let dz = h.pos(e).z - z0;
        assert!(dz > 8.0 && dz < 25.0, "slid sideways {dz}");
        let x0 = h.pos(e).x;
        for _ in 0..60 {
            h.step();
        }
        assert!(
            h.pos(e).x - x0 > 150.0,
            "drives on afterwards: {}",
            h.pos(e)
        );
    }

    /// `forceStaticObjectCheck` → `positionStuck`: a unit standing inside
    /// the edge of a structure's footprint (a building erected against
    /// it, a yard closing) walks out — `UpdatePos` lets a stuck unit step
    /// even when no square beside it is open — and then reaches its
    /// goal. (A unit buried deep inside a closed block has no passable
    /// neighbour leaf and fails its path, as in the engine.)
    #[test]
    fn unit_inside_a_structure_footprint_walks_out() {
        let mut h = Harness::flat();
        let wall = Vec3::new(640.0, 0.0, 640.0);
        h.spawn_structure(UnitKind::Firewall, 0, wall);
        h.step();
        // Just past the footprint's east edge: inside the squares the
        // Bit's own footprint closes around the structure.
        let e = h.spawn(UnitKind::Bit, 0, wall + Vec3::new(36.0, 0.0, 0.0));
        h.step();
        assert!(
            h.world.get::<GroundMover>(e).unwrap().position_stuck,
            "spawned inside the footprint: stuck"
        );
        let goal = Vec3::new(640.0, 0.0, 900.0);
        h.world.entity_mut(e).insert(MoveTarget(goal));
        let mut unstuck_at = None;
        for tick in 0..900 {
            h.step();
            if unstuck_at.is_none() && !h.world.get::<GroundMover>(e).unwrap().position_stuck {
                unstuck_at = Some(tick);
            }
            if !h.has_order(e) {
                break;
            }
        }
        assert!(
            unstuck_at.is_some_and(|t| t < 120),
            "walks out of the closed squares within 4 s: {unstuck_at:?}"
        );
        assert!(!h.has_order(e), "order finished");
        assert!(
            h.pos(e).xz().distance(goal.xz()) < 40.0,
            "reached the goal: {}",
            h.pos(e)
        );
    }

    /// A unit that cannot get anywhere any more (sealed in after its
    /// path was planned) idles, re-requests a path at a SlowUpdate once
    /// A new map replaces the nav grid with one of another size: the
    /// path service must drop its labels, queue and search in flight
    /// (all sized to the old grid) instead of indexing the new grid
    /// with them.
    #[test]
    fn path_service_survives_a_map_change() {
        use super::super::movement::{NavBucket, NavGridSet};
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(404.0, 0.0, 404.0));
        h.step();
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(1500.0, 0.0, 1500.0)));
        h.step();
        assert!(
            h.world.get::<MovePath>(e).is_some(),
            "path on the first map"
        );
        // A quarter-size map with a wall, so the component labels and
        // scratch of the old grid are all wrong for it.
        let mut small = spring_pathfinding::SpeedMap::uniform(48, 48, 1.0);
        for z in 0..40 {
            small.speeds[(z * 48 + 24) as usize] = 0.0;
        }
        small.refresh_max_speed();
        let mut nav = NavGridSet::default();
        nav.buckets.push(NavBucket {
            max_slope: 1.0,
            speed_map: small,
        });
        h.world.insert_resource(nav);
        for _ in 0..10 {
            h.world
                .entity_mut(e)
                .insert(MoveTarget(Vec3::new(300.0, 0.0, 100.0)));
            h.step();
            h.world
                .entity_mut(e)
                .insert(MoveTarget(Vec3::new(20.0, 0.0, 300.0)));
            h.step();
        }
    }

    /// `numIdlingUpdates` passes `SPRING_MAX_HEADING / turnRate`, gets
    /// none, and gives the order up (`Fail`) instead of pushing forever.
    #[test]
    fn hopelessly_stuck_unit_repaths_then_gives_up() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(404.0, 0.0, 404.0));
        h.step();
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(700.0, 0.0, 404.0)));
        h.step();
        assert!(h.world.get::<MovePath>(e).is_some(), "path planned");
        let start = h.pos(e);
        {
            // Seal the unit's 3×3-square neighbourhood.
            let (cx, cz) = ((start.x / 8.0) as u32, (start.z / 8.0) as u32);
            let mut nav = h.world.resource_mut::<NavGridSet>();
            let map = &mut nav.buckets[0].speed_map;
            let w = map.width;
            for z in cz - 2..=cz + 2 {
                for x in cx - 2..=cx + 2 {
                    if x.abs_diff(cx) == 2 || z.abs_diff(cz) == 2 {
                        map.speeds[(z * w + x) as usize] = 0.0;
                    }
                }
            }
        }
        let first_request = h.world.get::<GroundMover>(e).unwrap().last_repath_frame;
        let mut ticks = 0;
        let mut requested = false;
        while h.has_order(e) && ticks < 1200 {
            h.step();
            ticks += 1;
            requested |= h
                .world
                .get::<GroundMover>(e)
                .is_some_and(|m| m.last_repath_frame != first_request);
        }
        assert!(!h.has_order(e), "gave up");
        assert!(requested, "re-requested a path before giving up");
        let fs = bit_frame_stats();
        assert!(
            ticks as f32 > MAX_HEADING / fs.turn_rate,
            "not before the idle limit: {ticks}"
        );
        assert!(h.pos(e).xz().distance(start.xz()) < 16.0);
    }

    /// An unreachable goal (inside a sealed box) yields a partial path:
    /// the unit walks to the closest reachable point and the order fails
    /// there, instead of being refused on the spot.
    #[test]
    fn unreachable_goal_walks_to_closest_point_then_fails() {
        let mut h = Harness::flat();
        {
            let mut nav = h.world.resource_mut::<NavGridSet>();
            let map = &mut nav.buckets[0].speed_map;
            let w = map.width;
            for z in 70..90 {
                for x in 100..120 {
                    if x == 100 || x == 119 || z == 70 || z == 89 {
                        map.speeds[(z * w + x) as usize] = 0.0;
                    }
                }
            }
        }
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(500.0, 0.0, 640.0));
        h.step();
        h.world
            .entity_mut(e)
            .insert(MoveTarget(Vec3::new(880.0, 0.0, 640.0)));
        h.step();
        assert!(
            !h.world.get::<MovePath>(e).unwrap().reached_goal,
            "partial path"
        );
        for _ in 0..600 {
            h.step();
            if !h.has_order(e) {
                break;
            }
        }
        assert!(!h.has_order(e), "order ended");
        assert_eq!(
            h.world.get::<GroundMover>(e).unwrap().progress,
            Progress::Failed
        );
        let p = h.pos(e);
        let inside = (800.0..960.0).contains(&p.x) && (560.0..720.0).contains(&p.z);
        assert!(!inside, "never entered the box: {p}");
        // The closest reachable cell is just outside the box, 10 cells
        // from the goal.
        assert!(
            p.xz().distance(Vec2::new(880.0, 640.0)) < 100.0,
            "stopped at the box: {p}"
        );
    }

    /// `GetPosSpeedMod`: climbing a 30° ramp runs at
    /// `1/(1 + (1-cos 30°)·slopeMod)` ≈ 0.43× (slopeMod ≈ 9.7 for KP's
    /// MaxSlope 36); descending it is full speed.
    #[test]
    fn uphill_slope_slows_downhill_does_not() {
        let mut h = Harness::flat();
        let verts = 257usize;
        let rise = 8.0 * 30.0_f32.to_radians().tan();
        let heights: Vec<f32> = (0..verts * verts)
            .map(|i| (i % verts) as f32 * rise)
            .collect();
        let cap = h.world.resource::<NavGridSet>().buckets[0].max_slope;
        let map = spring_pathfinding::SpeedMap::from_heightmap(
            &heights,
            verts as u32,
            verts as u32,
            cap,
            spring_pathfinding::slope_mod_from_max_slope(cap),
        );
        h.world.resource_mut::<NavGridSet>().buckets[0].speed_map = map;
        h.world
            .insert_resource(Heightmap::from_raw(heights, verts, verts));
        let steady = |h: &mut Harness, from: f32, to: f32| {
            let e = h.spawn(UnitKind::Bit, 0, Vec3::new(from, 0.0, 1000.0));
            h.step();
            h.world
                .entity_mut(e)
                .insert(MoveTarget(Vec3::new(to, 0.0, 1000.0)));
            // Long enough to turn round (the harness spawns facing +X).
            for _ in 0..150 {
                h.step();
            }
            let s = mover_speed(h, e);
            h.world.despawn(e);
            s
        };
        let up = steady(&mut h, 600.0, 1400.0);
        let down = steady(&mut h, 1400.0, 600.0);
        let slope = 1.0 - 30.0_f32.to_radians().cos();
        let expected = 90.0 / (1.0 + slope * spring_pathfinding::slope_mod_from_max_slope(cap));
        assert!((up - expected).abs() < 2.0, "uphill {up} vs {expected}");
        assert!((down - 90.0).abs() < 0.5, "downhill full speed: {down}");
    }

    /// Idle units follow sinking terrain down (Hex Farm) instead of
    /// hovering, keeping their spawn lift; structures stay upright.
    #[test]
    fn idle_units_follow_the_ground_down_and_structures_stay_upright() {
        let mut h = Harness::flat();
        let e = h.spawn(UnitKind::Bit, 0, Vec3::new(400.0, 0.0, 400.0));
        h.world
            .entity_mut(e)
            .insert(crate::interaction::movement::GroundLift(3.0));
        let kernel = h.spawn_structure(UnitKind::Kernel, 0, Vec3::new(800.0, 0.0, 800.0));
        for hgt in h.world.resource_mut::<Heightmap>().heights_mut() {
            *hgt = -20.0;
        }
        h.step();
        assert!(
            (h.pos(e).y - (-17.0)).abs() < 1e-4,
            "on the lowered ground + lift: {}",
            h.pos(e)
        );
        let up = h.world.get::<Transform>(kernel).unwrap().rotation * Vec3::Y;
        assert!((up - Vec3::Y).length() < 1e-5);
    }

    /// Obstacle avoidance: two Bits walking straight at each other peel
    /// apart early and pass without ever overlapping.
    #[test]
    fn head_on_units_steer_past_each_other() {
        let mut h = Harness::flat();
        let a = h.spawn(UnitKind::Bit, 0, Vec3::new(600.0, 0.0, 600.0));
        let b = h.spawn(UnitKind::Bit, 1, Vec3::new(1000.0, 0.0, 600.0));
        h.step();
        // Face each other first.
        h.world.get_mut::<GroundMover>(b).unwrap().heading = Heading::from_vector(-1.0, 0.0);
        h.world
            .entity_mut(a)
            .insert(MoveTarget(Vec3::new(1000.0, 0.0, 600.0)));
        h.world
            .entity_mut(b)
            .insert(MoveTarget(Vec3::new(600.0, 0.0, 600.0)));
        let mut min_d = f32::MAX;
        for _ in 0..400 {
            h.step();
            min_d = min_d.min(h.pos(a).xz().distance(h.pos(b).xz()));
        }
        assert!(!h.has_order(a) && !h.has_order(b), "both arrived");
        assert!(min_d > 20.0, "passed without overlapping: {min_d}");
    }

    /// Nine Bytes sent to one point settle round it instead of orbiting
    /// it forever: the traffic-jam skip marks the waypoint, so the
    /// arrival rules of `HandleUnitCollisionsAux` get their turn.
    #[test]
    fn crowd_on_one_point_settles() {
        let mut h = Harness::flat();
        let units: Vec<Entity> = (0..9)
            .map(|i| {
                let (x, z) = ((i % 3) as f32, (i / 3) as f32);
                h.spawn(
                    UnitKind::Byte,
                    0,
                    Vec3::new(560.0 + x * 40.0, 0.0, 560.0 + z * 40.0),
                )
            })
            .collect();
        h.step();
        h.group_move(&units, Vec3::new(1000.0, 0.0, 600.0));
        for _ in 0..900 {
            h.step();
        }
        for &e in &units {
            assert!(!h.has_order(e), "{e:?} still circling at {}", h.pos(e));
        }
    }
}

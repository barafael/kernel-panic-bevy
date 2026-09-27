//! Aircraft movement: Spring's `CHoverAirMoveType`
//! (`rts/Sim/MoveTypes/HoverAirMoveType.cpp`, with the `AAirMoveType`
//! helpers it uses), which flies every `HoverAttack=1` unit — in Kernel
//! Panic, the Flow.
//!
//! One call of [`hover_air_system`] is one 30 Hz sim frame, so every
//! FBI rate is used in its native per-frame unit: `MaxVelocity` elmos
//! per frame, `Acceleration`/`BrakeRate` elmos per frame², `TurnRate`
//! heading units (65536 per turn) per frame, `verticalSpeed` elmos per
//! frame.
//!
//! What the Flow does, as in Spring:
//! - **Altitude**: holds `cruiseAlt` (+ a random 0..5) above
//!   `max(smoothGround(pos), smoothGround(brakePos))` — the smoothed
//!   [`SmoothGround`] mesh sampled at its position and four braking
//!   distances ahead — with `UpdateVerticalSpeed`'s climb/sink rule
//!   (`altitudeRate` capped, `accRate × 0.7` / `× 1.5` ramps, 5 %
//!   damping inside ±2 elmos). Taking off (after being built) uses the
//!   real ground until it passes 80 % of cruise altitude.
//! - **Horizontal**: the velocity is steered as a vector toward the goal
//!   at `accRate` (`decRate` when the change opposes the motion), full
//!   speed until inside the braking distance — heading only follows, at
//!   `TurnRate`, so it can side-slip. `UpdateBanking` rolls it into its
//!   lateral acceleration.
//! - **Orders**: a move order is finished once within
//!   `GetGoalRadius() = 64` elmos (2D) at a `SlowUpdate` (every 16
//!   frames); with nothing queued the unit stops (`ExecuteStop`) and,
//!   since `AirHoverFactor=0` means it never lands, hovers — easing back
//!   to where it stopped (`UpdateHovering`, no drift at hover factor 0).
//! - **Stun** (`IsStunned`): no steering; the altitude rule keeps
//!   running (Recoil zeroes the wanted height only after picking the
//!   climb/sink direction, so a stunned Flow bobs at cruise height).
//! - **Collisions** between aircraft: `CheckForCollision` (a flyer ahead
//!   bumps the wanted altitude −30/+50) and `HandleCollisions` (overlaps
//!   pushed apart, mass-weighted); the out-of-map nudge.
//!
//! Not ported (no Kernel Panic unit needs them): landing / landed states
//! (every KP aircraft has `AirHoverFactor >= 0`, i.e. `dontLand`),
//! transports, `airStrafe` circling, attack pitch, crashing, first-person
//! control, and `allowAircraftToHitGround` (KP's modrules disables it).

use bevy::prelude::*;

use super::movement::{CommandQueue, MovePath, MoveTarget, QueuedCommand, promote_next_command};
use crate::rng::next_f32;
use crate::sim::{GAME_SPEED, SLOW_UPDATE_RATE, SQUARE_SIZE, dir3_of, heading_of};
use crate::terrain::heightmap::Heightmap;
use crate::terrain::smooth_ground::SmoothGround;
use crate::units::combat::{AimTarget, AttackGroundOrder, AttackTargetOrder, Dying, Stunned};
use crate::units::components::{UnitStats, UnitType};
use crate::units::content::unit_registry::UnitRegistry;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::mechanics::network_buffer::SpeedBoost;

/// `CHoverAirMoveType::GetGoalRadius()` (`SQUARE_SIZE * SQUARE_SIZE`):
/// a move order counts as done within this 2D distance.
pub const GOAL_RADIUS: f32 = SQUARE_SIZE * SQUARE_SIZE;
/// `CSolidObject::DEFAULT_MASS`: at or above it a unit is pushed out of,
/// never shoved.
const DEFAULT_MASS: f32 = 1e5;

/// Per-kind constants of `CHoverAirMoveType`, in Spring's per-frame units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoverAirParams {
    /// `accRate = max(0.01, maxAcc)` (FBI `Acceleration`).
    pub acc_rate: f32,
    /// `decRate = max(0.01, maxDec)` (FBI `BrakeRate`).
    pub dec_rate: f32,
    /// `altitudeRate = max(0.01, verticalSpeed)`.
    pub altitude_rate: f32,
    /// `turnRate`, in radians per frame.
    pub turn_rate: f32,
    /// `unitDef->wantedHeight` (FBI `cruiseAlt`).
    pub cruise_alt: f32,
    /// `dlHoverFactor` (FBI `AirHoverFactor`); `>= 0` → never lands.
    pub hover_factor: f32,
    pub banking_allowed: bool,
    /// `unitDef->mass` (defaults to the metal cost).
    pub mass: f32,
}

impl HoverAirParams {
    /// `DontLand()`.
    pub fn dont_land(&self) -> bool {
        self.hover_factor >= 0.0
    }
}

/// `AAirMoveType::AircraftState` (the ones a never-landing hover
/// aircraft uses).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AircraftState {
    Takeoff,
    Flying,
    Hovering,
}

/// `CHoverAirMoveType`'s per-unit state.
#[derive(Component, Debug, Clone)]
pub struct HoverAir {
    pub params: HoverAirParams,
    pub state: AircraftState,
    /// `owner->speed` (xyz), elmos per frame.
    pub speed: Vec3,
    pub wanted_speed: Vec3,
    /// `deltaSpeed`: this frame's horizontal speed change (banking).
    delta_speed: Vec3,
    pub goal: Vec3,
    /// The command position the goal was last set from
    /// (`IsMovingTowards`), `None` when no move is in progress.
    moving_to: Option<Vec3>,
    pub wanted_height: f32,
    pub org_wanted_height: f32,
    max_drift: f32,
    want_to_stop: bool,
    /// Yaw in radians; forward is `(sin, 0, cos)`.
    pub heading: f32,
    pub wanted_heading: f32,
    pub current_bank: f32,
    random_wind: Vec3,
    /// `lastCollidee` when `collisionState == COLLISION_DIRECT`.
    collidee: Option<Entity>,
    /// Per-unit phase of the `SlowUpdate` / collision-check cadences
    /// (Spring staggers by unit id).
    phase: u32,
    rng: u32,
}

impl HoverAir {
    /// `CHoverAirMoveType(owner)`: `wantedHeight = cruiseAlt + rand × 5`,
    /// starting in take-off (a fresh aircraft leaves its factory pad;
    /// one spawned already at altitude finishes take-off at once).
    pub fn new(params: HoverAirParams, pos: Vec3, heading: f32, seed: u32) -> Self {
        let mut rng = seed.wrapping_mul(0x9E37_79B9) | 1;
        let wanted_height = params.cruise_alt + next_f32(&mut rng) * 5.0;
        Self {
            params,
            state: AircraftState::Takeoff,
            speed: Vec3::ZERO,
            wanted_speed: Vec3::ZERO,
            delta_speed: Vec3::ZERO,
            // `AMoveType::goalPos = owner->pos`.
            goal: pos,
            moving_to: None,
            wanted_height,
            org_wanted_height: wanted_height,
            max_drift: 1.0,
            want_to_stop: false,
            heading,
            wanted_heading: heading,
            current_bank: 0.0,
            random_wind: Vec3::ZERO,
            collidee: None,
            phase: seed,
            rng,
        }
    }

    fn forward(&self) -> Vec3 {
        dir3_of(self.heading)
    }

    /// `SetGoal(pos, distance)`.
    fn set_goal(&mut self, pos: Vec3, distance: f32) {
        self.goal = pos;
        self.max_drift = distance.max(16.0);
        self.wanted_height = self.org_wanted_height;
    }

    /// `SetState(newState)`, for the states used here.
    fn set_state(&mut self, state: AircraftState) {
        if state == self.state {
            return;
        }
        self.state = state;
        if state == AircraftState::Hovering {
            // `mix(orgWantedHeight, wantedHeight, forceHeading)`
            self.wanted_height = self.org_wanted_height;
            self.wanted_speed = Vec3::ZERO;
        }
    }

    /// `StartMoving(pos, goalRadius)` — the command AI's `SetGoal` with
    /// its default `SQUARE_SIZE` radius.
    fn start_moving(&mut self, pos: Vec3) {
        self.want_to_stop = false;
        match self.state {
            AircraftState::Takeoff | AircraftState::Flying => {}
            AircraftState::Hovering => self.set_state(AircraftState::Flying),
        }
        self.set_goal(pos, SQUARE_SIZE);
        self.moving_to = Some(pos);
    }

    /// `StopMoving()`.
    fn stop_moving(&mut self) {
        self.want_to_stop = true;
        self.wanted_height = self.org_wanted_height;
        self.moving_to = None;
    }

    /// `ExecuteStop()` for an aircraft that cannot land.
    fn execute_stop(&mut self, pos: Vec3) {
        self.want_to_stop = false;
        self.wanted_speed = Vec3::ZERO;
        self.set_goal(pos, 0.0);
        match self.state {
            AircraftState::Takeoff | AircraftState::Flying => self.set_state(AircraftState::Hovering),
            AircraftState::Hovering => {}
        }
    }

    /// `UseSmoothMesh()`: flying or hovering (no transport orders here).
    fn use_smooth_mesh(&self) -> bool {
        matches!(self.state, AircraftState::Flying | AircraftState::Hovering)
    }
}

/// Spring's `smoothstep(e0, e1, x)`.
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The ground an aircraft measures its altitude against.
pub struct Ground<'a> {
    pub smooth: Option<&'a SmoothGround>,
    pub heightmap: Option<&'a Heightmap>,
    pub world_size: Vec2,
}

impl Ground<'_> {
    fn real(&self, x: f32, z: f32) -> f32 {
        self.heightmap.map_or(0.0, |hm| hm.sample(x, z))
    }

    /// `CGround::GetHeightAboveWater`.
    fn real_aw(&self, x: f32, z: f32) -> f32 {
        self.real(x, z).max(0.0)
    }

    fn smooth(&self, x: f32, z: f32) -> f32 {
        match self.smooth {
            Some(s) => s.get_height(x, z),
            None => self.real(x, z),
        }
    }

    /// `amtGetGroundHeightFuncs[UseSmoothMesh() * 2 + canSubmerge]`
    /// (either smooth index is the above-water variant).
    fn altitude_base(&self, smooth: bool, x: f32, z: f32) -> f32 {
        if smooth {
            self.smooth(x, z).max(0.0)
        } else {
            self.real_aw(x, z)
        }
    }

    /// `amtGetGroundHeightFuncs[4 * UseSmoothMesh()]` (`UpdateFlying`):
    /// `HAMTGetMaxGroundHeight` = max(smooth, real) when on the mesh.
    fn flying_base(&self, smooth: bool, x: f32, z: f32) -> f32 {
        if smooth {
            self.smooth(x, z).max(self.real(x, z))
        } else {
            self.real_aw(x, z)
        }
    }
}

/// What the rest of the game asks of one aircraft this frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Orders {
    /// Active move order (`MoveTarget`).
    pub target: Option<Vec3>,
    /// More move-like commands queued behind it (`HasMoreMoveCommands`).
    pub more_moves: bool,
    /// Any command at all is active (`UnitIsBusy`).
    pub busy: bool,
    /// `KeepPointingTo`: face this while attacking.
    pub point_at: Option<Vec3>,
    pub stunned: bool,
    /// Effective `maxSpeed` (FBI + Flow's territory bonus), elmos/frame.
    pub max_speed: f32,
}

/// One aircraft in the frame's working set.
pub struct Flyer {
    entity: Entity,
    pos: Vec3,
    radius: f32,
    air: HoverAir,
    orders: Orders,
    /// Set when the command AI finished the move order this frame.
    finished_leg: bool,
}

/// `CheckForCollision()`: the closest aircraft in the way ahead.
fn check_for_collision(me: usize, flyers: &[Flyer]) -> Option<Entity> {
    let f = &flyers[me];
    let forward = f.air.forward();
    let probe = f.pos + forward * 121.0;
    let mut dist = 200.0f32;
    let mut hit = None;
    for (k, o) in flyers.iter().enumerate() {
        if k == me || (o.pos - probe).length() > dist + o.radius {
            continue;
        }
        let dif = o.pos - f.pos;
        let forward_dif = forward * forward.dot(dif);
        if forward_dif.length_squared() >= dist * dist {
            continue;
        }
        let orto = dif - forward_dif;
        let front = forward_dif.length();
        let min_orto = (o.radius + f.radius) * 2.0 + front * 0.1 + 10.0;
        if orto.length_squared() < min_orto * min_orto {
            dist = front;
            hit = Some(o.entity);
        }
    }
    hit
}

/// `UpdateVerticalSpeed(spd, curRelHeight, curVertSpeed)`.
fn update_vertical_speed(f: &mut Flyer, others: &[(Entity, Vec3, Vec3)], cur_rel_height: f32, cur_vert_speed: f32) {
    let air = &mut f.air;
    let p = air.params;
    let mut wh = air.wanted_height;
    // First restore the original vertical speed.
    air.speed.y = cur_vert_speed;
    let spd = air.speed;

    if air.state != AircraftState::Takeoff
        && let Some(c) = air.collidee
        && let Some(&(_, cpos, cspd)) = others.iter().find(|(e, _, _)| *e == c)
    {
        let dir = cpos - f.pos;
        let sdir = cspd - spd;
        if spd.dot(dir + sdir * 20.0) < 0.0 {
            if cpos.y > f.pos.y {
                wh -= 30.0;
            } else {
                wh += 50.0;
            }
        }
    }

    let mut ws;
    if cur_rel_height < wh {
        ws = p.altitude_rate;
        if spd.y > 0.0001 && ((wh - cur_rel_height) / spd.y) * p.acc_rate * 1.5 < spd.y {
            ws = 0.0;
        }
    } else {
        ws = -p.altitude_rate;
        if spd.y < -0.0001 && ((wh - cur_rel_height) / spd.y) * p.acc_rate * 0.7 < -spd.y {
            ws = 0.0;
        }
    }
    if f.orders.stunned {
        wh = 0.0;
    }

    air.speed.y = if (wh - cur_rel_height).abs() > 2.0 {
        if spd.y > ws {
            ws.max(spd.y - p.acc_rate * 1.5)
        } else {
            // Accelerate upward faster if close to the ground.
            let up = if cur_rel_height < 20.0 { 2.0 } else { 0.7 };
            ws.min(spd.y + p.acc_rate * up)
        }
    } else {
        spd.y * 0.95
    };
}

/// `UpdateAirPhysics()` (terrain contact disabled, as KP's modrules
/// `allowAircraftToHitGround=0`; leaving the map allowed).
fn update_air_physics(f: &mut Flyer, ground: &Ground, others: &[(Entity, Vec3, Vec3)]) {
    let p = f.air.params;
    let spd = f.air.speed;
    let brake_pos = f.pos + spd * ((0.5 * spd.length()) / p.dec_rate) * 4.0;
    let vertical_speed = spd.y;

    // Acceleration and braking happen in the xz-plane.
    let mut spd = Vec3::new(spd.x, 0.0, spd.z);
    let delta = f.air.wanted_speed - spd;
    let dsq = delta.length_squared();
    let rate = if delta.dot(spd) < 0.0 { p.dec_rate } else { p.acc_rate };
    if dsq < rate * rate {
        spd = f.air.wanted_speed;
    } else {
        spd += delta / dsq.sqrt() * rate;
    }
    f.air.speed = spd;

    let smooth = f.air.use_smooth_mesh();
    let cp = ground.altitude_base(smooth, f.pos.x, f.pos.z);
    let bp = ground.altitude_base(smooth, brake_pos.x, brake_pos.z);
    // Sampling the ground a few braking distances ahead lets the
    // aircraft climb in time for cliffs.
    let rel = f.pos.y - cp.max(bp);
    update_vertical_speed(f, others, rel, vertical_speed);
    f.pos += f.air.speed;
}

/// `UpdateTakeoff()`.
fn update_takeoff(f: &mut Flyer, ground: &Ground, others: &[(Entity, Vec3, Vec3)]) {
    f.air.wanted_speed = Vec3::ZERO;
    f.air.wanted_height = f.air.org_wanted_height;
    update_air_physics(f, ground, others);
    let altitude = f.pos.y - ground.real_aw(f.pos.x, f.pos.z);
    if altitude > f.air.org_wanted_height * 0.8 {
        f.air.set_state(AircraftState::Flying);
    }
}

/// `UpdateHovering()`: drift (by `AirHoverFactor`) around the goal and
/// ease back onto it.
fn update_hovering(f: &mut Flyer, ground: &Ground, others: &[(Entity, Vec3, Vec3)]) {
    let air = &mut f.air;
    let cur_sq = (air.goal - f.pos).xz().length_squared();
    let abs_hover = air.params.hover_factor.abs() * 0.5;
    air.random_wind.x = air.random_wind.x * 0.9 + (next_f32(&mut air.rng) - 0.5) * 0.5;
    air.random_wind.z = air.random_wind.z * 0.9 + (next_f32(&mut air.rng) - 0.5) * 0.5;
    let drift = air.params.dont_land() || cur_sq > GOAL_RADIUS * GOAL_RADIUS;
    let mut wanted = if drift { air.random_wind * abs_hover } else { Vec3::ZERO };
    let d = air.goal - f.pos;
    wanted += Vec3::new(
        smoothstep(0.0, 400.0, d.x.abs()),
        smoothstep(0.0, 400.0, d.y.abs()),
        smoothstep(0.0, 400.0, d.z.abs()),
    ) * d;
    let m = f.orders.max_speed;
    air.wanted_speed = wanted.abs().min(Vec3::splat(m)) * wanted.signum();
    update_air_physics(f, ground, others);
}

/// `UpdateFlying()` for a cruising (or pointing) hover aircraft.
fn update_flying(f: &mut Flyer, ground: &Ground, others: &[(Entity, Vec3, Vec3)]) {
    let pos = f.pos;
    let mut goal_vec = f.air.goal - pos;
    // Don't turn back for a waypoint just flown over by a hair.
    if f.orders.more_moves && goal_vec != Vec3::ZERO && goal_vec.length() < 1.0 {
        goal_vec = f.air.forward();
    }
    let goal_dist_sq_2d = goal_vec.xz().length_squared();
    let smooth = f.air.use_smooth_mesh();
    let ground_height = ground.flying_base(smooth, pos.x, pos.z);
    let md = f.air.max_drift;
    let close = goal_dist_sq_2d < md * md && (ground_height + f.air.wanted_height - pos.y).abs() < md;
    if close && f.orders.point_at.is_none() {
        // FLY_CRUISING, can't land, not a transport.
        f.air.wanted_speed = Vec3::ZERO;
        if !f.orders.busy {
            f.air.want_to_stop = true;
            f.air.set_state(AircraftState::Hovering);
        }
    }

    goal_vec.y = 0.0;
    let p = f.air.params;
    let cur_speed = f.air.speed.xz().length();
    let brake_dist = (0.5 * cur_speed * cur_speed) / p.dec_rate;
    let goal_dist = goal_vec.length() + 0.1;
    let goal_speed = if goal_dist > brake_dist {
        f.orders.max_speed
    } else {
        cur_speed - p.dec_rate
    };
    if goal_dist > goal_speed {
        f.air.wanted_heading = heading_of(goal_vec.xz());
        // `maxWantedSpeed` follows `maxSpeed` here: Kernel Panic's Flow
        // bonus (`network_flowspeed.lua`, COB `MAX_SPEED`) raised the
        // speed limit in the engines it was written for.
        f.air.wanted_speed = goal_vec / goal_dist * goal_speed.min(f.orders.max_speed);
    } else if !f.orders.busy {
        f.air.execute_stop(pos);
    } else {
        f.air.wanted_speed = Vec3::ZERO;
    }

    update_air_physics(f, ground, others);

    let face = match f.orders.point_at {
        Some(t) => t - f.pos,
        None if f.orders.more_moves && goal_dist < brake_dist && goal_dist > 1.0 => f.air.forward(),
        None => f.air.goal - f.pos,
    };
    if face.xz().length_squared() > 1.0 {
        f.air.wanted_heading = heading_of(face.xz());
    }
}

/// `UpdateHeading()`: turn toward the wanted heading at `turnRate`.
fn update_heading(air: &mut HoverAir) {
    if air.state == AircraftState::Takeoff {
        return; // !factoryHeadingTakeoff: keep the factory's heading
    }
    let tau = std::f32::consts::TAU;
    let delta = (air.wanted_heading - air.heading + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
    air.heading += delta.clamp(-air.params.turn_rate, air.params.turn_rate);
    air.heading = (air.heading + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
}

/// `UpdateBanking(noBanking)`: roll into the lateral acceleration.
fn update_banking(air: &mut HoverAir, pos: Vec3, no_banking: bool) {
    let bank_limit = ((air.goal - pos).xz().length_squared() * 0.15 * 0.15).min(1.0);
    let front = air.forward();
    let right = front.cross(Vec3::Y);
    let mut wanted_bank = 0.0;
    if !no_banking && air.params.banking_allowed {
        wanted_bank = right.dot(air.delta_speed) / air.params.acc_rate * 0.5;
    }
    if wanted_bank * wanted_bank > bank_limit {
        wanted_bank = bank_limit.sqrt();
    }
    if air.current_bank > wanted_bank {
        air.current_bank -= (air.current_bank - wanted_bank).min(0.03);
    } else {
        air.current_bank += (wanted_bank - air.current_bank).min(0.03);
    }
}

/// `HandleCollisions()`: aircraft overlapping another are pushed apart
/// (mass-weighted, velocities exchanged along the contact normal), and
/// pulled back when off the map.
fn handle_collisions(flyers: &mut [Flyer], me: usize, world: Vec2) {
    let (m_radius, m_mass) = (flyers[me].radius, flyers[me].air.params.mass);
    if flyers[me].air.state != AircraftState::Takeoff {
        for k in 0..flyers.len() {
            if k == me {
                continue;
            }
            let pos = flyers[me].pos;
            let o = &flyers[k];
            let sq = (pos - o.pos).length_squared();
            let tot = m_radius + o.radius;
            if sq <= 0.1 || sq >= tot * tot {
                continue;
            }
            let dist = sq.sqrt();
            let dif = (pos - o.pos) / dist;
            let o_mass = o.air.params.mass;
            if o_mass >= DEFAULT_MASS {
                flyers[me].pos -= dif * (dist - tot);
                flyers[me].air.speed *= 0.99;
            } else {
                let part = m_mass / (m_mass + o_mass);
                let col_speed = -flyers[me].air.speed.dot(dif) + flyers[k].air.speed.dot(dif);
                flyers[me].pos -= dif * (dist - tot) * (1.0 - part);
                flyers[me].air.speed += dif * col_speed * (1.0 - part);
                flyers[k].air.speed -= dif * col_speed * part;
                flyers[k].pos += dif * (dist - tot) * part;
            }
        }
    }
    let f = &mut flyers[me];
    if f.pos.x < 0.0 {
        f.pos.x += 0.6;
    } else if f.pos.x > world.x {
        f.pos.x -= 0.6;
    }
    if f.pos.z < 0.0 {
        f.pos.z += 0.6;
    } else if f.pos.z > world.y {
        f.pos.z -= 0.6;
    }
}

/// `CHoverAirMoveType::Update()` plus the command AI's per-frame part,
/// for aircraft `me` at sim frame `frame`.
fn update_one(flyers: &mut [Flyer], others: &[(Entity, Vec3, Vec3)], me: usize, frame: u32, ground: &Ground) {
    if (frame.wrapping_add(flyers[me].air.phase)) & 3 == 0 {
        flyers[me].air.collidee = check_for_collision(me, flyers);
    }

    {
        let f = &mut flyers[me];
        // `CMobileCAI::ExecuteMove` / `SetGoal`.
        match f.orders.target {
            Some(t) => {
                if f.air.moving_to != Some(t) {
                    f.air.start_moving(t);
                }
                if (frame.wrapping_add(f.air.phase)) % SLOW_UPDATE_RATE == 0
                    && (t - f.pos).xz().length_squared() < GOAL_RADIUS * GOAL_RADIUS
                {
                    if !f.orders.more_moves {
                        f.air.stop_moving();
                    }
                    f.finished_leg = true;
                }
            }
            None => {
                if f.air.moving_to.is_some() {
                    f.air.stop_moving();
                }
            }
        }
    }

    let last_speed = flyers[me].air.speed;
    let f = &mut flyers[me];
    if f.orders.stunned {
        f.air.wanted_speed = Vec3::ZERO;
        update_air_physics(f, ground, others);
    } else {
        if f.air.want_to_stop {
            f.air.execute_stop(f.pos);
        }
        match f.air.state {
            AircraftState::Takeoff => update_takeoff(f, ground, others),
            AircraftState::Flying => update_flying(f, ground, others),
            AircraftState::Hovering => update_hovering(f, ground, others),
        }
        let mut delta = f.air.speed - last_speed;
        delta.y = 0.0;
        f.air.delta_speed = delta;
        update_heading(&mut f.air);
        let no_banking = f.air.state == AircraftState::Hovering;
        if f.air.state != AircraftState::Takeoff {
            update_banking(&mut f.air, f.pos, no_banking);
        }
    }
    handle_collisions(flyers, me, ground.world_size);
}

/// Advance a set of aircraft by one sim frame. Exposed for tests; the
/// ECS wrapper is [`hover_air_system`].
fn step(flyers: &mut [Flyer], frame: u32, ground: &Ground) {
    // Positions/speeds the collision-avoidance altitude bump reads.
    let others: Vec<(Entity, Vec3, Vec3)> = flyers.iter().map(|f| (f.entity, f.pos, f.air.speed)).collect();
    for me in 0..flyers.len() {
        update_one(flyers, &others, me, frame, ground);
    }
}

/// Orientation for a heading and bank angle: roll the level up-vector
/// toward the right side by `bank`.
pub fn attitude(heading: f32, bank: f32) -> Quat {
    let front = dir3_of(heading);
    let right = front.cross(Vec3::Y);
    let up = (Vec3::Y * bank.cos() + right * bank.sin()).normalize();
    Transform::default().looking_to(front, up).rotation
}

/// Builds the per-kind [`HoverAirParams`] for a flyer.
pub fn params_for(registry: &UnitRegistry, kind: crate::units::content::definitions::UnitKind) -> HoverAirParams {
    registry.hover_air_params(kind)
}

/// Is `cmd` a move-like command (`HasMoreMoveCommands`)?
fn is_move_command(cmd: &QueuedCommand) -> bool {
    matches!(
        cmd,
        QueuedCommand::Move(_) | QueuedCommand::Patrol(_) | QueuedCommand::AttackMove(_) | QueuedCommand::Guard(_)
    )
}

/// Flies every aircraft one sim frame (`CHoverAirMoveType::Update`).
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn hover_air_system(
    mut commands: Commands,
    mut frame: Local<u32>,
    registry: Res<UnitRegistry>,
    heightmap: Option<Res<Heightmap>>,
    smooth: Option<Res<SmoothGround>>,
    uninit: Query<(Entity, &UnitType, &UnitStats, &Transform), (Without<HoverAir>, Without<Emerging>, Without<Dying>)>,
    mut q: Query<
        (
            Entity,
            &UnitStats,
            &mut Transform,
            &mut HoverAir,
            Option<&MoveTarget>,
            Option<&mut MovePath>,
            Option<&mut CommandQueue>,
            Option<&SpeedBoost>,
            Has<Stunned>,
            Option<&AimTarget>,
            Has<AttackTargetOrder>,
            Has<AttackGroundOrder>,
        ),
        (Without<Emerging>, Without<Dying>),
    >,
    mut flyers: Local<Vec<Flyer>>,
) {
    *frame = frame.wrapping_add(1);
    for (entity, kind, stats, tf) in &uninit {
        if !stats.can_fly {
            continue;
        }
        let fwd = tf.forward().as_vec3();
        let heading = if fwd.xz().length_squared() > 1e-6 { heading_of(fwd.xz()) } else { 0.0 };
        let air = HoverAir::new(params_for(&registry, kind.0), tf.translation, heading, entity.to_bits() as u32);
        commands.entity(entity).insert(air);
    }

    flyers.clear();
    for (entity, stats, tf, air, target, _, queue, boost, stunned, aim, attack_unit, attack_ground) in &q {
        let more_moves = queue.is_some_and(|q| q.commands.iter().any(is_move_command));
        let attacking = attack_unit || attack_ground;
        flyers.push(Flyer {
            entity,
            pos: tf.translation,
            radius: stats.radius,
            air: air.clone(),
            orders: Orders {
                target: target.map(|t| t.0),
                more_moves,
                busy: target.is_some() || queue.is_some_and(|q| !q.commands.is_empty()) || attacking,
                point_at: if attacking && target.is_none() { aim.map(|a| a.pos) } else { None },
                stunned,
                max_speed: (stats.speed + boost.map_or(0.0, |b| b.0)) / GAME_SPEED,
            },
            finished_leg: false,
        });
    }
    if flyers.is_empty() {
        return;
    }

    let world_size = heightmap.as_deref().map_or(Vec2::splat(f32::MAX), |hm| {
        let (w, d) = hm.world_size();
        Vec2::new(w, d)
    });
    let ground = Ground {
        smooth: smooth.as_deref(),
        heightmap: heightmap.as_deref(),
        world_size,
    };
    step(&mut flyers, *frame, &ground);

    for f in flyers.iter() {
        let Ok((entity, _, mut tf, mut air, target, path, mut queue, ..)) = q.get_mut(f.entity) else {
            continue;
        };
        tf.translation = f.pos;
        tf.rotation = attitude(f.air.heading, f.air.current_bank);
        *air = f.air.clone();
        if f.finished_leg {
            commands.entity(entity).remove::<MovePath>();
            promote_next_command(&mut commands, entity, f.pos, queue.as_deref_mut());
            continue;
        }
        // Keep a one-waypoint path for everything that reads orders off
        // `MovePath` (chase re-path checks, command lines, animation).
        match (target, path) {
            (Some(t), Some(mut p)) => {
                let wp = Vec3::new(t.0.x, 0.0, t.0.z);
                if p.waypoints.len() != 1 || p.waypoints[0] != wp {
                    *p = MovePath::new(vec![wp], wp);
                }
            }
            (Some(t), None) => {
                let wp = Vec3::new(t.0.x, 0.0, t.0.z);
                commands.entity(entity).insert(MovePath::new(vec![wp], wp));
            }
            (None, Some(_)) => {
                commands.entity(entity).remove::<MovePath>();
            }
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spring_map::smooth_mesh::SmoothHeightMesh;

    /// Flow's `CHoverAirMoveType` constants.
    fn flow() -> HoverAirParams {
        HoverAirParams {
            acc_rate: 0.3,
            dec_rate: 0.9,
            altitude_rate: 3.0,
            turn_rate: 1280.0 * crate::sim::SHORT_ANGLE_TO_RAD,
            cruise_alt: 140.0,
            hover_factor: 0.0,
            banking_allowed: true,
            mass: 120.0,
        }
    }

    /// 1024×1024-elmo map: flat at 100 with a 300-high ridge across
    /// z ≈ 600.
    fn terrain() -> (Heightmap, SmoothGround) {
        let n = 129;
        let mut h = vec![100.0f32; n * n];
        for z in 70..80 {
            for x in 0..n {
                h[z * n + x] = 400.0;
            }
        }
        let hm = Heightmap::from_raw(h, n, n);
        let sg = SmoothGround::new(SmoothHeightMesh::new(hm.heights(), n, n));
        (hm, sg)
    }

    fn flyer(pos: Vec3, orders: Orders) -> Flyer {
        Flyer {
            entity: Entity::from_raw_u32(1).unwrap(),
            pos,
            radius: 16.0,
            air: HoverAir::new(flow(), pos, 0.0, 7),
            orders,
            finished_leg: false,
        }
    }

    fn orders(target: Option<Vec3>) -> Orders {
        Orders {
            target,
            busy: target.is_some(),
            max_speed: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn takes_off_and_settles_at_cruise_altitude_over_the_smooth_mesh() {
        let (hm, sg) = terrain();
        let ground = Ground {
            smooth: Some(&sg),
            heightmap: Some(&hm),
            world_size: Vec2::splat(1024.0),
        };
        let mut fl = vec![flyer(Vec3::new(300.0, 100.0, 200.0), orders(None))];
        for frame in 0..600 {
            step(&mut fl, frame, &ground);
        }
        let f = &fl[0];
        assert_eq!(f.air.state, AircraftState::Hovering);
        let target = sg.get_height(f.pos.x, f.pos.z) + f.air.wanted_height;
        assert!((f.pos.y - target).abs() < 2.5, "y {} vs {target}", f.pos.y);
        // Held in place: no horizontal drift at AirHoverFactor=0.
        assert!((f.pos.xz() - Vec2::new(300.0, 200.0)).length() < 2.0);
        assert!(f.air.speed.length() < 0.05);
    }

    #[test]
    fn climbs_ahead_of_a_ridge_and_never_dips_below_the_mesh() {
        let (hm, sg) = terrain();
        let ground = Ground {
            smooth: Some(&sg),
            heightmap: Some(&hm),
            world_size: Vec2::splat(1024.0),
        };
        let goal = Vec3::new(500.0, 100.0, 950.0);
        let mut fl = vec![flyer(Vec3::new(500.0, 100.0, 100.0), orders(Some(goal)))];
        let mut min_clearance = f32::INFINITY;
        let mut max_h_speed = 0.0f32;
        for frame in 0..1500 {
            step(&mut fl, frame, &ground);
            let f = &fl[0];
            if f.air.state != AircraftState::Takeoff {
                min_clearance = min_clearance.min(f.pos.y - hm.sample(f.pos.x, f.pos.z));
            }
            max_h_speed = max_h_speed.max(f.air.speed.xz().length());
            if f.finished_leg {
                fl[0].orders = orders(None);
                fl[0].finished_leg = false;
            }
        }
        let f = &fl[0];
        // Stopped within the goal radius, hovering at cruise height.
        assert!((f.pos.xz() - goal.xz()).length() < GOAL_RADIUS + 2.0, "{:?}", f.pos);
        assert_eq!(f.air.state, AircraftState::Hovering);
        let target = sg.get_height(f.pos.x, f.pos.z) + f.air.wanted_height;
        assert!((f.pos.y - target).abs() < 2.5);
        // Max speed respected; the ridge (300 above the plain) passed
        // with real clearance thanks to the look-ahead + smooth mesh.
        assert!(max_h_speed <= 1.0 + 1e-4, "{max_h_speed}");
        assert!(min_clearance > 60.0, "clearance {min_clearance}");
        // It faces where it flew.
        assert!((f.air.heading - 0.0).abs() < 0.05, "heading {}", f.air.heading);
    }

    /// `wh *= (1 - IsStunned())` only runs after the climb/sink target
    /// `ws` was chosen from the real wanted height, so it merely drops
    /// the ±2-elmo damping band: a stunned aircraft stops steering and
    /// bobs around its cruise altitude instead of settling.
    #[test]
    fn stunned_aircraft_stops_and_bobs_at_altitude() {
        let (hm, sg) = terrain();
        let ground = Ground {
            smooth: Some(&sg),
            heightmap: Some(&hm),
            world_size: Vec2::splat(1024.0),
        };
        let goal = Vec3::new(300.0, 100.0, 900.0);
        let mut fl = vec![flyer(Vec3::new(300.0, 100.0, 100.0), orders(Some(goal)))];
        for frame in 0..300 {
            step(&mut fl, frame, &ground);
        }
        assert!(fl[0].air.speed.xz().length() > 0.9);
        fl[0].orders.stunned = true;
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for frame in 300..600 {
            step(&mut fl, frame, &ground);
            if frame > 400 {
                let f = &fl[0];
                let rel = f.pos.y - sg.get_height(f.pos.x, f.pos.z) - f.air.wanted_height;
                lo = lo.min(rel);
                hi = hi.max(rel);
            }
        }
        assert!(fl[0].air.speed.xz().length() < 1e-3);
        assert!(lo > -15.0 && hi < 15.0, "altitude band {lo}..{hi}");
        assert!(hi - lo > 0.5, "no damping band while stunned: {lo}..{hi}");
    }

    #[test]
    fn overlapping_aircraft_are_pushed_apart() {
        let (hm, sg) = terrain();
        let ground = Ground {
            smooth: Some(&sg),
            heightmap: Some(&hm),
            world_size: Vec2::splat(1024.0),
        };
        let a = Vec3::new(300.0, 250.0, 300.0);
        let mut fl = vec![flyer(a, orders(None)), flyer(a + Vec3::X * 10.0, orders(None))];
        fl[1].entity = Entity::from_raw_u32(2).unwrap();
        for f in &mut fl {
            f.air.state = AircraftState::Hovering;
        }
        for frame in 0..60 {
            step(&mut fl, frame, &ground);
        }
        let d = (fl[0].pos - fl[1].pos).length();
        assert!(d >= 31.0, "still overlapping: {d}");
    }

    /// The ECS wrapper: a Flow with a move order gets its move type,
    /// a one-waypoint `MovePath` while travelling, and gives the order
    /// up (queue promoted) inside the goal radius.
    #[test]
    fn system_flies_a_move_order_to_completion() {
        use crate::units::content::definitions::UnitKind;
        let mut world = World::new();
        let (hm, sg) = terrain();
        world.insert_resource(hm);
        world.insert_resource(sg);
        let mut defs = spring_tdf::UnitDefs::default();
        defs.units.insert(
            "flow".into(),
            spring_tdf::UnitDef {
                id: "flow".into(),
                can_fly: true,
                hover_attack: true,
                cruise_alt: 140.0,
                acceleration: 0.3,
                brake_rate: 0.9,
                turn_rate: 1280.0,
                air_hover_factor: 0.0,
                max_velocity: 1.0,
                ..Default::default()
            },
        );
        world.insert_resource(UnitRegistry::for_test(defs));
        let first = Vec3::new(200.0, 0.0, 500.0);
        let second = Vec3::new(600.0, 0.0, 500.0);
        let e = world
            .spawn((
                UnitType(UnitKind::Flow),
                UnitStats {
                    radius: 16.0,
                    hit_radius: 20.0,
                    speed: 30.0,
                    accel: 9.0,
                    brake: 27.0,
                    turn_rate: 3.0,
                    can_fly: true,
                    no_chase_vtol: false,
                },
                Transform::from_xyz(200.0, 100.0, 100.0),
                MoveTarget(first),
                CommandQueue {
                    commands: vec![QueuedCommand::Move(second)],
                },
            ))
            .id();
        let sys = world.register_system(hover_air_system);
        world.run_system(sys).unwrap();
        assert!(world.get::<HoverAir>(e).is_some());
        let mut saw_second = false;
        for _ in 0..2000 {
            world.run_system(sys).unwrap();
            if let Some(t) = world.get::<MoveTarget>(e) {
                saw_second |= t.0 == second;
                let p = world.get::<MovePath>(e).map(|p| p.waypoints.clone());
                if let Some(p) = p {
                    assert_eq!(p, vec![Vec3::new(t.0.x, 0.0, t.0.z)]);
                }
            }
        }
        assert!(saw_second, "queued leg promoted");
        assert!(world.get::<MoveTarget>(e).is_none());
        assert!(world.get::<MovePath>(e).is_none());
        let tf = world.get::<Transform>(e).unwrap();
        assert!((tf.translation.xz() - second.xz()).length() < GOAL_RADIUS + 1.0);
        let air = world.get::<HoverAir>(e).unwrap();
        assert_eq!(air.state, AircraftState::Hovering);
        assert!(air.org_wanted_height >= 140.0 && air.org_wanted_height <= 145.0);
    }

    #[test]
    fn attitude_banks_toward_the_right() {
        let q = attitude(0.0, 0.3);
        let up = q * Vec3::Y;
        let fwd = q * Vec3::NEG_Z;
        assert!((fwd - Vec3::Z).length() < 1e-4);
        // Right of +Z (Y up) is −X.
        assert!(up.x < -0.25 && up.y > 0.9);
    }
}

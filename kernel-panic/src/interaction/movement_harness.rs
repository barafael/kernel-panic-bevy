//! Headless behavioural harness for ground movement.
//!
//! Spins up a bare `World` with the real FBI registry, a flat nav grid
//! and the fixed-tick movement chain, spawns groups of real unit kinds,
//! issues right-click style group moves and measures what a player sees:
//! how long the group takes to arrive, whether anyone gets stuck, how
//! close units get to each other, how much they wiggle on the way and
//! how much already-arrived units get shoved around.
//!
//! `report` (ignored) prints every scenario's numbers — run it with
//! `cargo test -p kernel-panic movement_harness -- --ignored --nocapture`.
//! The non-ignored tests assert the behaviour Spring exhibits.

use std::time::Duration;

use bevy::prelude::*;
use spring_pathfinding::{HeatMap, SpeedMap};

use super::movement::{
    CommandQueue, MovePath, MoveTarget, NavBucket, NavGridSet, PathHeat, QueuedCommand,
};
use crate::terrain::heightmap::Heightmap;
use crate::units::components::{TeamId, UnitStats, UnitType};
use crate::units::content::definitions::UnitKind;
use crate::units::content::unit_registry::UnitRegistry;

/// Map edge in heightmap squares (8 elmos each).
const MAP_SQUARES: u32 = 256;
const DT: f64 = 1.0 / 30.0;

pub(crate) struct Harness {
    pub world: World,
    schedule: Schedule,
    pub tick: u32,
    /// Spring footprints (`FootprintX × 16` elmos square) of the
    /// structures spawned, as (centre, half extent).
    pub structures: Vec<(Vec3, Vec2)>,
}

impl Harness {
    /// Flat 2048×2048-elmo map with a single nav bucket.
    pub fn flat() -> Self {
        let mut world = World::new();
        world.init_resource::<Time>();
        world.insert_resource(UnitRegistry::load());
        let registry = world.resource::<UnitRegistry>();
        let cap = registry.max_slope_ratio(UnitKind::Bit);
        let mut nav = NavGridSet::default();
        nav.buckets.push(NavBucket {
            max_slope: cap,
            speed_map: SpeedMap::uniform(MAP_SQUARES, MAP_SQUARES, 1.0),
        });
        world.insert_resource(nav);
        let verts = (MAP_SQUARES + 1) as usize;
        world.insert_resource(Heightmap::from_raw(vec![0.0; verts * verts], verts, verts));
        world.insert_resource(PathHeat(HeatMap::new(MAP_SQUARES, MAP_SQUARES)));
        // Never run out of search budget: reproducible runs.
        world.insert_resource(super::ground_move::PathSearchBudget(f64::INFINITY));

        let mut schedule = Schedule::default();
        super::movement::add_ground_sim_systems(&mut schedule);
        Self {
            world,
            schedule,
            tick: 0,
            structures: Vec::new(),
        }
    }

    /// Spawn a unit with the movement-relevant components `spawn_unit`
    /// gives it, facing +X.
    pub fn spawn(&mut self, kind: UnitKind, team: u8, pos: Vec3) -> Entity {
        let registry = self.world.resource::<UnitRegistry>();
        let stats = UnitStats::from_registry(kind, registry, 20.0);
        let mover = super::movement::ground_mover_components(kind, registry);
        self.world
            .spawn((
                UnitType(kind),
                TeamId(team),
                stats,
                mover,
                Transform::from_translation(pos)
                    .with_rotation(Quat::from_rotation_arc(-Vec3::Z, Vec3::X)),
            ))
            .id()
    }

    /// Spawn a finished structure of `kind` at `pos`.
    pub fn spawn_structure(&mut self, kind: UnitKind, team: u8, pos: Vec3) -> Entity {
        let fp = self
            .world
            .resource::<UnitRegistry>()
            .def(kind)
            .map_or(Vec2::splat(2.0), |d| Vec2::new(d.footprint_x, d.footprint_z));
        self.structures.push((pos, fp * 8.0));
        self.spawn(kind, team, pos)
    }

    /// Is `p` inside any spawned structure's footprint?
    pub fn inside_structure(&self, p: Vec3) -> bool {
        self.structures.iter().any(|(c, half)| {
            (p.x - c.x).abs() < half.x && (p.z - c.z).abs() < half.y
        })
    }

    /// Plain right-click on `target` for `units` (the game's group-move
    /// slot assignment).
    pub fn group_move(&mut self, units: &[Entity], target: Vec3) {
        let snapshot: Vec<(Entity, Vec3, f32)> = units
            .iter()
            .map(|&e| {
                (
                    e,
                    self.world.get::<Transform>(e).unwrap().translation,
                    self.world.get::<UnitStats>(e).unwrap().radius,
                )
            })
            .collect();
        let orders = crate::interaction::selection::right_click::group_move_slots(&snapshot, target);
        for (e, slot) in orders {
            self.world
                .entity_mut(e)
                .insert((MoveTarget(slot), CommandQueue::default()))
                .remove::<MovePath>();
        }
    }

    /// Shift-queue a move behind the unit's current order.
    pub fn queue_move(&mut self, unit: Entity, target: Vec3) {
        let mut ec = self.world.entity_mut(unit);
        if ec.get::<MoveTarget>().is_none() {
            ec.insert((MoveTarget(target), CommandQueue::default()));
        } else {
            ec.get_mut::<CommandQueue>()
                .unwrap()
                .push(QueuedCommand::Move(target));
        }
    }

    pub fn step(&mut self) {
        self.world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f64(DT));
        self.schedule.run(&mut self.world);
        self.tick += 1;
    }

    pub fn pos(&self, e: Entity) -> Vec3 {
        self.world.get::<Transform>(e).unwrap().translation
    }

    pub fn has_order(&self, e: Entity) -> bool {
        self.world.get::<MoveTarget>(e).is_some() || self.world.get::<MovePath>(e).is_some()
    }

    fn yaw(&self, e: Entity) -> f32 {
        let f = self.world.get::<Transform>(e).unwrap().forward().as_vec3();
        f.x.atan2(f.z)
    }
}

/// What a scenario measured.
#[derive(Debug, Default, Clone)]
pub(crate) struct Metrics {
    pub units: usize,
    /// Seconds until the last unit's order ended (`None`: timed out).
    pub last_arrival_s: Option<f32>,
    pub first_arrival_s: Option<f32>,
    /// Units still holding an order at the timeout.
    pub stuck: usize,
    /// Smallest centre distance between two ground units seen after the
    /// first second (elmos).
    pub min_pair_dist: f32,
    /// Distance walked / straight-line start→final distance, averaged.
    pub path_ratio: f32,
    /// Heading-rate sign flips per moving unit-second (wiggle).
    pub wiggle_per_s: f32,
    /// Mean |heading change| per moving unit-second, degrees.
    pub turn_deg_per_s: f32,
    /// Elmos arrived units got shoved in total.
    pub idle_shove: f32,
    /// Ticks where a unit with an order stood still after having started.
    pub stall_ticks: u32,
    /// Final distance from each unit to its commanded goal, max.
    pub max_goal_miss: f32,
    /// Unit-ticks spent with the centre inside a structure footprint.
    pub inside_structure_ticks: u32,
}

pub(crate) fn run(h: &mut Harness, units: &[Entity], goals: &[Vec3], timeout_s: f32) -> Metrics {
    let n = units.len();
    let start: Vec<Vec3> = units.iter().map(|&e| h.pos(e)).collect();
    let mut prev_pos = start.clone();
    let mut prev_yaw: Vec<f32> = units.iter().map(|&e| h.yaw(e)).collect();
    let mut prev_rate = vec![0.0f32; n];
    let mut walked = vec![0.0f32; n];
    let mut arrived_at: Vec<Option<u32>> = vec![None; n];
    let mut started = vec![false; n];
    let mut flips = 0u32;
    let mut turned = 0.0f32;
    let mut moving_ticks = 0u32;
    let mut m = Metrics {
        units: n,
        min_pair_dist: f32::MAX,
        ..default()
    };
    let max_ticks = (timeout_s / DT as f32) as u32;
    for t in 0..max_ticks {
        h.step();
        let pos: Vec<Vec3> = units.iter().map(|&e| h.pos(e)).collect();
        for i in 0..n {
            let e = units[i];
            if h.inside_structure(pos[i]) {
                m.inside_structure_ticks += 1;
            }
            let d = pos[i].xz().distance(prev_pos[i].xz());
            let ordered = h.has_order(e);
            if arrived_at[i].is_some() {
                m.idle_shove += d;
            } else {
                walked[i] += d;
                if d > 1e-3 {
                    started[i] = true;
                } else if ordered && started[i] {
                    m.stall_ticks += 1;
                }
            }
            if !ordered && arrived_at[i].is_none() {
                arrived_at[i] = Some(t);
            }
            if ordered && d > 1e-3 {
                moving_ticks += 1;
                let yaw = h.yaw(e);
                let mut rate = yaw - prev_yaw[i];
                if rate > std::f32::consts::PI {
                    rate -= std::f32::consts::TAU;
                } else if rate < -std::f32::consts::PI {
                    rate += std::f32::consts::TAU;
                }
                turned += rate.abs();
                if rate.abs() > 1e-3 && prev_rate[i].abs() > 1e-3 && rate.signum() != prev_rate[i].signum() {
                    flips += 1;
                }
                if rate.abs() > 1e-3 {
                    prev_rate[i] = rate;
                }
            }
            prev_yaw[i] = h.yaw(e);
        }
        if t as f64 * DT >= 1.0 {
            for i in 0..n {
                for j in i + 1..n {
                    m.min_pair_dist = m.min_pair_dist.min(pos[i].xz().distance(pos[j].xz()));
                }
            }
        }
        prev_pos = pos;
        if arrived_at.iter().all(|a| a.is_some()) {
            // Let pushes settle for another second before scoring shoves.
            for _ in 0..30 {
                h.step();
                for i in 0..n {
                    let p = h.pos(units[i]);
                    m.idle_shove += p.xz().distance(prev_pos[i].xz());
                    prev_pos[i] = p;
                }
            }
            break;
        }
    }
    let secs = |t: u32| t as f32 * DT as f32;
    m.stuck = arrived_at.iter().filter(|a| a.is_none()).count();
    if m.stuck == 0 {
        m.last_arrival_s = arrived_at.iter().flatten().max().map(|&t| secs(t));
    }
    m.first_arrival_s = arrived_at.iter().flatten().min().map(|&t| secs(t));
    let mut ratio = 0.0;
    for i in 0..n {
        let end = h.pos(units[i]);
        let straight = start[i].xz().distance(end.xz()).max(1.0);
        ratio += walked[i] / straight;
        m.max_goal_miss = m.max_goal_miss.max(end.xz().distance(goals[i].xz()));
    }
    m.path_ratio = ratio / n as f32;
    m.wiggle_per_s = flips as f32 / (moving_ticks as f32 * DT as f32).max(1e-3);
    m.turn_deg_per_s = turned.to_degrees() / (moving_ticks as f32 * DT as f32).max(1e-3);
    m
}

/// Deterministic scatter (LCG) so every run sees the same blob.
fn scatter(n: usize, center: Vec3, spread: f32, seed: u32) -> Vec<Vec3> {
    let mut s = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    let mut next = || {
        s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (s >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    };
    (0..n)
        .map(|_| center + Vec3::new(next() * spread, 0.0, next() * spread))
        .collect()
}

fn goals_of(h: &Harness, units: &[Entity]) -> Vec<Vec3> {
    units
        .iter()
        .map(|&e| h.world.get::<MoveTarget>(e).map_or(h.pos(e), |t| t.0))
        .collect()
}

/// `n` units in a loose blob, right-click 400 elmos east.
pub(crate) fn scenario_blob(kind: UnitKind, n: usize, seed: u32) -> Metrics {
    let mut h = Harness::flat();
    let units: Vec<Entity> = scatter(n, Vec3::new(600.0, 0.0, 600.0), 40.0, seed)
        .into_iter()
        .map(|p| h.spawn(kind, 0, p))
        .collect();
    h.step();
    h.group_move(&units, Vec3::new(1000.0, 0.0, 600.0));
    let goals = goals_of(&h, &units);
    run(&mut h, &units, &goals, 60.0)
}

/// Two 8-Bit columns walking through each other head-on.
pub(crate) fn scenario_crossing() -> Metrics {
    let mut h = Harness::flat();
    let mut units = Vec::new();
    let mut goals = Vec::new();
    for i in 0..8 {
        let z = 560.0 + i as f32 * 12.0;
        let a = h.spawn(UnitKind::Bit, 0, Vec3::new(600.0, 0.0, z));
        let b = h.spawn(UnitKind::Bit, 1, Vec3::new(1000.0, 0.0, z));
        units.extend([a, b]);
        goals.extend([Vec3::new(1000.0, 0.0, z), Vec3::new(600.0, 0.0, z)]);
    }
    h.step();
    for (&e, &g) in units.iter().zip(&goals) {
        h.world
            .entity_mut(e)
            .insert((MoveTarget(g), CommandQueue::default()));
    }
    run(&mut h, &units, &goals, 60.0)
}

/// 8 Bits ordered straight through a wall of three Terminals
/// (fully-blocked `gggg` yardmaps, 64 elmos each).
pub(crate) fn scenario_building() -> Metrics {
    let mut h = Harness::flat();
    for z in [536.0, 600.0, 664.0] {
        h.spawn_structure(UnitKind::Terminal, 1, Vec3::new(800.0, 0.0, z));
    }
    let units: Vec<Entity> = scatter(8, Vec3::new(620.0, 0.0, 600.0), 24.0, 7)
        .into_iter()
        .map(|p| h.spawn(UnitKind::Bit, 0, p))
        .collect();
    h.step();
    h.group_move(&units, Vec3::new(1000.0, 0.0, 600.0));
    let goals = goals_of(&h, &units);
    run(&mut h, &units, &goals, 60.0)
}

/// Two Bits walking straight at each other on the same line.
pub(crate) fn scenario_head_on(kind: UnitKind) -> Metrics {
    let mut h = Harness::flat();
    let a = h.spawn(kind, 0, Vec3::new(600.0, 0.0, 600.0));
    let b = h.spawn(kind, 1, Vec3::new(1000.0, 0.0, 600.0));
    h.step();
    let goals = [Vec3::new(1000.0, 0.0, 600.0), Vec3::new(600.0, 0.0, 600.0)];
    h.world.entity_mut(a).insert(MoveTarget(goals[0]));
    h.world.entity_mut(b).insert(MoveTarget(goals[1]));
    run(&mut h, &[a, b], &goals, 60.0)
}

/// One Bit walking a queued zig-zag of four legs.
pub(crate) fn scenario_chain() -> Metrics {
    let mut h = Harness::flat();
    let e = h.spawn(UnitKind::Bit, 0, Vec3::new(600.0, 0.0, 600.0));
    h.step();
    let legs = [
        Vec3::new(750.0, 0.0, 650.0),
        Vec3::new(900.0, 0.0, 600.0),
        Vec3::new(1050.0, 0.0, 650.0),
        Vec3::new(1200.0, 0.0, 600.0),
    ];
    for l in legs {
        h.queue_move(e, l);
    }
    run(&mut h, &[e], &[legs[3]], 60.0)
}

/// Mean of several runs (arrival times over the runs that finished).
pub(crate) fn average(runs: &[Metrics]) -> Metrics {
    let n = runs.len() as f32;
    let mean = |f: &dyn Fn(&Metrics) -> f32| runs.iter().map(f).sum::<f32>() / n;
    let finished: Vec<f32> = runs.iter().filter_map(|m| m.last_arrival_s).collect();
    Metrics {
        units: runs[0].units,
        last_arrival_s: (!finished.is_empty())
            .then(|| finished.iter().sum::<f32>() / finished.len() as f32),
        first_arrival_s: Some(mean(&|m| m.first_arrival_s.unwrap_or(0.0))),
        stuck: runs.iter().map(|m| m.stuck).sum(),
        min_pair_dist: runs.iter().map(|m| m.min_pair_dist).fold(f32::MAX, f32::min),
        path_ratio: mean(&|m| m.path_ratio),
        wiggle_per_s: mean(&|m| m.wiggle_per_s),
        turn_deg_per_s: mean(&|m| m.turn_deg_per_s),
        idle_shove: mean(&|m| m.idle_shove),
        stall_ticks: (mean(&|m| m.stall_ticks as f32)).round() as u32,
        max_goal_miss: mean(&|m| m.max_goal_miss),
        inside_structure_ticks: (mean(&|m| m.inside_structure_ticks as f32)).round() as u32,
    }
}

pub(crate) fn print(name: &str, m: &Metrics) {
    println!(
        "{name:>10}: n={:2} last={:>6} first={:>6} stuck={} minpair={:5.1} path×={:.3} wiggle/s={:.2} turn°/s={:5.1} shove={:6.1} stalls={:4} miss={:5.1} in_struct={}",
        m.units,
        m.last_arrival_s.map_or("-".into(), |s| format!("{s:.2}s")),
        m.first_arrival_s.map_or("-".into(), |s| format!("{s:.2}s")),
        m.stuck,
        if m.min_pair_dist == f32::MAX { 0.0 } else { m.min_pair_dist },
        m.path_ratio,
        m.wiggle_per_s,
        m.turn_deg_per_s,
        m.idle_shove,
        m.stall_ticks,
        m.max_goal_miss,
        m.inside_structure_ticks,
    );
}

#[test]
#[ignore = "report — run with -- --ignored --nocapture"]
fn report() {
    // Blob scenarios: mean over five scatters (stuck: total).
    let seeds = 1..=5;
    print("blob16 bit", &average(&seeds.clone().map(|s| scenario_blob(UnitKind::Bit, 16, s)).collect::<Vec<_>>()));
    print("blob9 byte", &average(&seeds.map(|s| scenario_blob(UnitKind::Byte, 9, s)).collect::<Vec<_>>()));
    print("crossing", &scenario_crossing());
    print("building", &scenario_building());
    print("headon bit", &scenario_head_on(UnitKind::Bit));
    print("headon byt", &scenario_head_on(UnitKind::Byte));
    print("chain", &scenario_chain());
}



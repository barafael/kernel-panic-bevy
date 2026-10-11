//! What the harness reads out of the sim each tick: a canonical,
//! entity-id-independent state hash, an entity census, and the
//! replicated-state records the snapshot encoders size.

use std::collections::{BTreeMap, HashMap};

use bevy::ecs::component::ComponentId;
use bevy::prelude::*;

use crate::interaction::movement::{AttackMoveActive, CommandQueue, GuardTarget, MoveTarget};
use crate::rendering::interpolation::SimPose;
use crate::units::combat::{AttackGroundOrder, AttackTargetOrder, Dying, Stunned};
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::lifecycle::construction::{Constructing, PendingBuild};
use crate::units::lifecycle::production::Producer;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::mechanics::cloak::Cloaked;

/// FNV-1a, 64 bit: tiny, stable across processes and targets.
#[derive(Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    pub fn finish(self) -> u64 {
        self.0
    }
}

/// Entity categories the census counts, by component type name.
pub const CATEGORIES: &[(&str, &str)] = &[
    // (category, component type name suffix) — first match wins, in
    // this order.
    ("unit", "::UnitType"),
    ("projectile", "::ProjectileVisual"),
    ("beam", "::BeamVisual"),
    ("laser_bolt", "::LaserBolt"),
    ("arc", "::LightningArc"),
    ("trail", "::FadingTrail"),
    ("impact", "::ImpactBurst"),
    ("ground_flash", "::GroundFlash"),
    ("ceg_particle", "::CegParticle"),
    ("ceg_flame", "::CegFlame"),
    ("ceg_spike", "::CegSpike"),
    ("ceg_delayed", "::CegDelayedSpawn"),
    ("death_particle", "::DeathParticle"),
    ("piece", "::PieceIndex"),
    ("sigterm", "::SigTermSignal"),
    ("sigterm_bomb", "::SigTermBomb"),
    ("area_denial", "::AreaDenialZone"),
];

/// Categories that are sim state proper (hashed into the sim hash).
/// The others are visuals (hashed into the fx hash only).
const SIM_CATEGORIES: &[&str] = &[
    "unit",
    "projectile",
    "sigterm",
    "sigterm_bomb",
    "area_denial",
];

/// One unit as a client needs it to draw it, plus the sim-only order
/// state the hash covers.
#[derive(Clone, Copy, Debug)]
pub struct UnitRec {
    pub entity: Entity,
    pub kind: u16,
    pub team: u8,
    pub pos: Vec3,
    pub rot: Quat,
    pub health: f32,
    pub max_health: f32,
    /// bit 0 dying, 1 emerging/being built, 2 cloaked, 3 stunned,
    /// 4 moving (has a move target), 5 fight order, 6 constructing,
    /// 7 has attack / guard order.
    pub flags: u8,
    /// Build / emerge progress 0..1 (1 when done).
    pub build: f32,
}

/// A projectile (sim-moved, carries its hit).
#[derive(Clone, Copy, Debug)]
pub struct ProjRec {
    pub entity: Entity,
    pub pos: Vec3,
}

/// Everything read from one tick.
pub struct Capture {
    pub units: Vec<UnitRec>,
    pub projectiles: Vec<ProjRec>,
    /// Entity count per category ("other" for the rest).
    pub census: BTreeMap<&'static str, usize>,
    /// Entities per category (for spawn-event counting).
    pub category_entities: HashMap<&'static str, Vec<Entity>>,
    /// Hash of the sim state: units (pose, health, flags, orders,
    /// queues), projectiles, and sim-entity counts.
    pub sim_hash: u64,
    /// Hash of the visual-only entity counts.
    pub fx_hash: u64,
    /// Separate hashes of the parts of `sim_hash`, to see what diverged
    /// first: positions, health, orders.
    pub part_hashes: [u64; 3],
    pub total_entities: usize,
}

type UnitQ = (
    (Entity, &'static UnitType, &'static TeamId, &'static SimPose),
    (
        Option<&'static Health>,
        Has<Dying>,
        Option<&'static Emerging>,
        Has<Cloaked>,
        Has<Stunned>,
    ),
    (
        Option<&'static MoveTarget>,
        Has<AttackMoveActive>,
        Option<&'static GuardTarget>,
        Option<&'static AttackTargetOrder>,
        Option<&'static AttackGroundOrder>,
        Option<&'static PendingBuild>,
        Option<&'static Constructing>,
        Option<&'static CommandQueue>,
        Option<&'static Producer>,
    ),
);

fn bits3(v: Vec3) -> [u8; 12] {
    let mut out = [0; 12];
    out[0..4].copy_from_slice(&v.x.to_bits().to_le_bytes());
    out[4..8].copy_from_slice(&v.y.to_bits().to_le_bytes());
    out[8..12].copy_from_slice(&v.z.to_bits().to_le_bytes());
    out
}

/// Category of each component id whose type name matches.
fn category_ids(world: &World) -> Vec<(ComponentId, &'static str, usize)> {
    let mut out = Vec::new();
    for (id, info) in world.components().iter_registered() {
        let name = info.name().to_string();
        if !name.starts_with(concat!(env!("CARGO_CRATE_NAME"), "::")) {
            continue;
        }
        for (rank, (cat, suffix)) in CATEGORIES.iter().enumerate() {
            if name.ends_with(suffix) {
                out.push((id, *cat, rank));
            }
        }
    }
    out
}

pub fn capture(world: &mut World) -> Capture {
    // --- census by archetype -------------------------------------------
    let cats = category_ids(world);
    let mut census: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut category_entities: HashMap<&'static str, Vec<Entity>> = HashMap::new();
    let mut total_entities = 0;
    for archetype in world.archetypes().iter() {
        if archetype.is_empty() {
            continue;
        }
        total_entities += archetype.len() as usize;
        let cat = cats
            .iter()
            .filter(|(id, _, _)| archetype.contains(*id))
            .min_by_key(|(_, _, rank)| *rank)
            .map_or("other", |(_, c, _)| *c);
        *census.entry(cat).or_default() += archetype.len() as usize;
        if cat != "other" && cat != "piece" {
            category_entities
                .entry(cat)
                .or_default()
                .extend(archetype.entities().iter().map(|e| e.id()));
        }
    }

    // --- units -----------------------------------------------------------
    let mut q = world.query::<UnitQ>();
    let mut units = Vec::new();
    // Entity → (kind, pose bits) so references hash id-independently.
    let mut key_of: HashMap<Entity, [u8; 14]> = HashMap::new();
    for ((e, ut, _, pose), _, _) in q.iter(world) {
        let mut k = [0u8; 14];
        k[0..2].copy_from_slice(&(ut.0 as u16).to_le_bytes());
        k[2..14].copy_from_slice(&bits3(pose.sim().translation));
        key_of.insert(e, k);
    }
    let reference = |e: Entity| key_of.get(&e).copied().unwrap_or([0xff; 14]);
    let mut pos_recs: Vec<Vec<u8>> = Vec::new();
    let mut hp_recs: Vec<Vec<u8>> = Vec::new();
    let mut order_recs: Vec<Vec<u8>> = Vec::new();
    for (
        (e, ut, team, pose),
        (health, dying, emerging, cloaked, stunned),
        (mt, fight, guard, attack, attack_ground, pending_build, constructing, queue, producer),
    ) in q.iter(world)
    {
        let tf = pose.sim();
        let (hp, max) = health.map_or((0.0, 0.0), |h| (h.current, h.max));
        let mut flags = 0u8;
        flags |= dying as u8;
        flags |= (emerging.is_some() as u8) << 1;
        flags |= (cloaked as u8) << 2;
        flags |= (stunned as u8) << 3;
        flags |= (mt.is_some() as u8) << 4;
        flags |= (fight as u8) << 5;
        flags |= (constructing.is_some() as u8) << 6;
        flags |= ((attack.is_some() || guard.is_some() || attack_ground.is_some()) as u8) << 7;
        let build = emerging.map_or(1.0, |em| {
            if em.total > 0.0 {
                (1.0 - em.remaining / em.total).clamp(0.0, 1.0)
            } else {
                1.0
            }
        });
        units.push(UnitRec {
            entity: e,
            kind: ut.0 as u16,
            team: team.0,
            pos: tf.translation,
            rot: tf.rotation,
            health: hp,
            max_health: max,
            flags,
            build,
        });

        let me = reference(e);
        let mut p = me.to_vec();
        p.push(team.0);
        for c in tf.rotation.to_array() {
            p.extend(c.to_bits().to_le_bytes());
        }
        pos_recs.push(p);

        let mut h = me.to_vec();
        h.extend(hp.to_bits().to_le_bytes());
        h.extend(max.to_bits().to_le_bytes());
        h.push(flags);
        if let Some(em) = emerging {
            h.extend(em.remaining.to_bits().to_le_bytes());
        }
        hp_recs.push(h);

        let mut o = me.to_vec();
        if let Some(mt) = mt {
            o.push(1);
            o.extend(bits3(mt.0));
        }
        if let Some(g) = guard {
            o.push(2);
            o.extend(reference(g.0));
        }
        if let Some(a) = attack {
            o.push(3);
            o.extend(reference(a.target));
        }
        if let Some(a) = attack_ground {
            o.push(4);
            o.extend(bits3(a.pos));
        }
        if let Some(b) = pending_build {
            o.push(5);
            o.extend((b.kind as u16).to_le_bytes());
            o.extend(bits3(b.site));
        }
        if let Some(c) = constructing {
            o.push(6);
            o.extend((c.kind as u16).to_le_bytes());
            o.extend(bits3(c.site));
        }
        if let Some(q) = queue {
            o.push(7);
            o.extend((q.commands.len() as u32).to_le_bytes());
            for c in &q.commands {
                o.extend(bits3(c.position()));
            }
        }
        if let Some(p) = producer {
            o.push(8);
            o.push(p.repeat() as u8);
            for k in p.queue() {
                o.extend((*k as u16).to_le_bytes());
            }
        }
        order_recs.push(o);
    }

    // --- projectiles -----------------------------------------------------
    let mut projectiles = Vec::new();
    let mut proj_recs: Vec<Vec<u8>> = Vec::new();
    if let Some(list) = category_entities.get("projectile") {
        for &e in list {
            let pos = world
                .get::<SimPose>(e)
                .map(|p| p.sim().translation)
                .or_else(|| world.get::<Transform>(e).map(|t| t.translation))
                .unwrap_or_default();
            projectiles.push(ProjRec { entity: e, pos });
            proj_recs.push(bits3(pos).to_vec());
        }
    }

    let hash_sorted = |mut recs: Vec<Vec<u8>>| {
        recs.sort_unstable();
        let mut h = Fnv::default();
        for r in &recs {
            h.bytes(&(r.len() as u32).to_le_bytes());
            h.bytes(r);
        }
        h.finish()
    };
    let part_hashes = [
        hash_sorted(pos_recs),
        hash_sorted(hp_recs),
        hash_sorted(order_recs),
    ];
    let mut sim = Fnv::default();
    for p in part_hashes {
        sim.bytes(&p.to_le_bytes());
    }
    sim.bytes(&hash_sorted(proj_recs).to_le_bytes());
    let mut fx = Fnv::default();
    for (cat, n) in census.iter().filter(|(c, _)| **c != "other") {
        let target = if SIM_CATEGORIES.contains(cat) {
            &mut sim
        } else {
            &mut fx
        };
        target.bytes(cat.as_bytes());
        target.bytes(&(*n as u64).to_le_bytes());
    }

    Capture {
        units,
        projectiles,
        census,
        category_entities,
        sim_hash: sim.finish(),
        fx_hash: fx.finish(),
        part_hashes,
        total_entities,
    }
}

/// Readable, sorted per-unit state (for diffing two runs at the tick
/// they diverge): kind, team, pose, health, flags and orders.
pub fn describe(world: &mut World) -> Vec<String> {
    let mut q = world.query::<UnitQ>();
    let mut out: Vec<String> = q
        .iter(world)
        .map(
            |(
                (e, ut, team, pose),
                (health, dying, emerging, _, stunned),
                (mt, fight, guard, attack, attack_ground, pending_build, constructing, queue, producer),
            )| {
                let tf = pose.sim();
                format!(
                    "{:?} t{} pos {:?} rot {:?} hp {:?} dying {} emerging {:?} stunned {} | mt {:?} fight {} guard {} attack {} ag {:?} pb {:?} cons {:?} queue {:?} prod {:?} | {e:?}",
                    ut.0,
                    team.0,
                    tf.translation,
                    tf.rotation,
                    health.map(|h| h.current),
                    dying,
                    emerging.map(|e| e.remaining),
                    stunned,
                    mt.map(|m| m.0),
                    fight,
                    guard.is_some(),
                    attack.is_some(),
                    attack_ground.map(|a| a.pos),
                    pending_build.map(|p| (p.kind, p.site)),
                    constructing.map(|c| (c.kind, c.site)),
                    queue.map(|q| q.commands.iter().map(|c| c.position()).collect::<Vec<_>>()),
                    producer.map(|p| (p.repeat(), p.queue().iter().copied().collect::<Vec<_>>())),
                )
            },
        )
        .collect();
    out.sort();
    // Query iteration order, as unit serials (deterministic ids).
    let mut sq = world.query::<(Entity, &crate::units::lifecycle::spawning::SimSerial)>();
    let order: Vec<String> = sq
        .iter(world)
        .map(|(e, s)| {
            let loc = world.entities().get(e).unwrap().unwrap();
            format!("{}@a{}", s.0, loc.archetype_id.index())
        })
        .collect();
    out.push(format!("qorder {}", order.join(" ")));
    let census = capture(world).census;
    out.push(format!("census {census:?}"));
    out
}

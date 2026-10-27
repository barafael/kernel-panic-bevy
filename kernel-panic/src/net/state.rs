//! What the headless harness and the lockstep desync check read out of
//! the sim: a canonical state hash keyed by [`NetId`] (never by
//! `Entity`, whose index differs between machines), an entity census,
//! and readable per-unit dumps. The golden-replay trails are these
//! hashes per tick; a networked peer reports one every
//! [`super::session::HASH_INTERVAL`] frames and dumps
//! [`describe`] when the host finds a mismatch.

use std::collections::{BTreeMap, HashMap};

use bevy::ecs::component::ComponentId;
use bevy::prelude::*;

use crate::interaction::ground_move::{GroundMover, PathQueue};
use crate::interaction::movement::{
    AttackMoveActive, CommandQueue, GuardTarget, MovePath, MoveTarget,
};
use crate::net::NetId;
use crate::rendering::interpolation::SimPose;
use crate::units::combat::{AttackGroundOrder, AttackTargetOrder, Dying, Stunned};
use crate::units::components::{Health, TeamId, UnitType};
use crate::units::features::Feature;
use crate::units::lifecycle::construction::{
    BuildSiteWait, ClearOrder, Constructing, PendingBuild,
};
use crate::units::lifecycle::production::Producer;
use crate::units::lifecycle::spawning::Emerging;
use crate::units::mechanics::cloak::Cloaked;
use crate::units::mechanics::deploy::BombardOrder;
use crate::units::mechanics::network_buffer::EnterOrder;

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
    ("ceg_ground_flash", "::CegGroundFlash"),
    ("ceg_spike", "::CegSpike"),
    ("ceg_delayed", "::CegDelayedSpawn"),
    ("death_particle", "::DeathParticle"),
    ("debris", "::Debris"),
    ("shard", "::Shard"),
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

/// Everything read from one tick. (The desync check reads only the sim
/// hash; the harness's measurements read the rest.)
#[allow(dead_code)]
pub struct Capture {
    /// Live units (anything with a `UnitType`).
    pub units: usize,
    /// Sim-moved projectiles.
    pub projectiles: usize,
    /// Entity count per category ("other" for the rest).
    pub census: BTreeMap<&'static str, usize>,
    /// Hash of the sim state: units (pose, health, flags, orders,
    /// queues), map features (position, health), projectiles, and
    /// sim-entity counts.
    pub sim_hash: u64,
    /// Hash of the visual-only entity counts.
    pub fx_hash: u64,
    /// Separate hashes of the parts of `sim_hash`, to see what diverged
    /// first: positions, health, orders, projectiles.
    pub part_hashes: [u64; 4],
    /// NaN floats met while hashing (hashed as one canonical NaN). A
    /// healthy sim has none.
    pub nans: usize,
    pub total_entities: usize,
    /// Hash of the bookkeeping the sim hash leaves out (every mover's
    /// full state, paths, build-site waits, the path service): not part
    /// of `sim_hash`, a trail column of its own, so a divergence that
    /// first shows in a position is caught where it was decided. The
    /// Rosetta probe.
    pub hidden_hash: u64,
}

type UnitQ = (
    (
        Entity,
        &'static NetId,
        &'static UnitType,
        &'static TeamId,
        &'static SimPose,
    ),
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
    (
        Option<&'static ClearOrder>,
        Option<&'static BombardOrder>,
        Option<&'static EnterOrder>,
    ),
);

/// The quiet NaN every NaN hashes as. A NaN's payload and sign bits are
/// not deterministic across targets (wasm engines may canonicalise or
/// keep them), so the hash never sees them; [`Capture::nans`] counts
/// the NaNs instead.
const CANONICAL_NAN: u32 = 0x7fc0_0000;

/// f32 → hashable bits, NaNs canonicalised and counted.
#[derive(Default)]
struct Bits {
    nans: usize,
}

impl Bits {
    fn f32(&mut self, x: f32) -> [u8; 4] {
        if x.is_nan() {
            self.nans += 1;
            CANONICAL_NAN.to_le_bytes()
        } else {
            x.to_bits().to_le_bytes()
        }
    }

    fn vec3(&mut self, v: Vec3) -> [u8; 12] {
        let mut out = [0; 12];
        out[0..4].copy_from_slice(&self.f32(v.x));
        out[4..8].copy_from_slice(&self.f32(v.y));
        out[8..12].copy_from_slice(&self.f32(v.z));
        out
    }
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

/// Byte records, one after another in one buffer: a hash's thousands of
/// records without an allocation each.
#[derive(Default)]
struct Records {
    buf: Vec<u8>,
    /// Where each record begins; it ends where the next does.
    starts: Vec<usize>,
}

impl Records {
    /// Begin a record with `head`; the rest goes onto the buffer.
    fn start(&mut self, head: impl AsRef<[u8]>) -> &mut Vec<u8> {
        self.starts.push(self.buf.len());
        self.buf.extend_from_slice(head.as_ref());
        &mut self.buf
    }

    /// The records as slices, in insertion order.
    fn slices(&self) -> Vec<&[u8]> {
        let ends = self.starts.iter().skip(1).copied().chain([self.buf.len()]);
        self.starts
            .iter()
            .zip(ends)
            .map(|(&a, b)| &self.buf[a..b])
            .collect()
    }

    /// The hash's exact input: the records in byte order, each after
    /// its u32 LE length, as one byte string. FNV over it is
    /// [`Records::hash_sorted`] (the Rosetta probe dumps it, so another
    /// machine can hash the very same bytes).
    fn stream(&self) -> Vec<u8> {
        let mut recs = self.slices();
        recs.sort_unstable();
        let mut out = Vec::with_capacity(self.buf.len() + 4 * recs.len());
        for r in recs {
            out.extend((r.len() as u32).to_le_bytes());
            out.extend_from_slice(r);
        }
        out
    }

    /// The records in byte order, each hashed after its length.
    fn hash_sorted(&self) -> u64 {
        let unsorted = self.slices();
        let mut recs = unsorted.clone();
        recs.sort_unstable();
        let h = fnv_records(&recs);
        // Self-check (the Rosetta probe): the same records sorted by a
        // scalar byte loop instead of libc's memcmp, and hashed again.
        // A probe dump showed every hashed value identical to Linux's
        // while this hash differed, so the hash's own machinery is
        // suspect: this says whether, and which half.
        let mut slow = unsorted;
        slow.sort_by(|a, b| {
            for (x, y) in a.iter().zip(b.iter()) {
                if x != y {
                    return x.cmp(y);
                }
            }
            a.len().cmp(&b.len())
        });
        if slow != recs {
            eprintln!(
                "state hash self-check: sort_unstable (memcmp) ordered {} records differently from the scalar sort",
                recs.len()
            );
        }
        let h2 = fnv_records(&slow);
        if h2 != h {
            eprintln!("state hash self-check: {h:016x} (memcmp order) vs {h2:016x} (scalar order)");
        }
        h
    }
}

fn fnv_records(recs: &[&[u8]]) -> u64 {
    let mut h = Fnv::default();
    for r in recs {
        h.bytes(&(r.len() as u32).to_le_bytes());
        h.bytes(r);
    }
    h.finish()
}

/// [`category_ids`], kept until a component type is registered: the
/// name scan is most of a hash's cost otherwise (`m1-runtime.md` §5).
#[derive(Resource)]
struct CategoryCache {
    registered: usize,
    ids: Vec<(ComponentId, &'static str, usize)>,
}

pub fn capture(world: &mut World) -> Capture {
    finish(&collect(world))
}

/// [`capture`], and the part hashes' exact input bytes
/// ([`Records::stream`]: positions, health, orders, projectiles, then
/// the hidden state).
pub fn capture_raw(world: &mut World) -> (Capture, Vec<Vec<u8>>) {
    let c = collect(world);
    let mut streams: Vec<Vec<u8>> = c.recs.iter().map(Records::stream).collect();
    streams.push(c.hidden.stream());
    (finish(&c), streams)
}

/// FNV of a part's stream: equal to the part hash it was dumped with,
/// unless the machine that hashed it computed wrongly.
pub fn stream_hash(stream: &[u8]) -> u64 {
    let mut h = Fnv::default();
    h.bytes(stream);
    h.finish()
}

/// A dump of the four streams, readable and diffable: per part a
/// `part <i> <hash>` line and one hex line per record.
pub fn records_text(streams: &[Vec<u8>], hashes: &[u64]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, (s, h)) in streams.iter().zip(hashes).enumerate() {
        out.push(format!("part {i} {h:016x}"));
        let mut at = 0;
        while at + 4 <= s.len() {
            let n = u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) as usize;
            at += 4;
            let r = &s[at..(at + n).min(s.len())];
            at += n;
            out.push(r.iter().map(|b| format!("{b:02x}")).collect());
        }
    }
    out
}

/// Read a [`records_text`] dump back: per part, the hash it was dumped
/// with and the hash of its records computed here.
pub fn check_records_text(text: &str) -> Vec<(usize, u64, u64)> {
    let mut out: Vec<(usize, u64, Fnv)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("part ") {
            let mut it = rest.split(' ');
            let i = it.next().and_then(|i| i.parse().ok()).unwrap_or(0);
            let h = it
                .next()
                .and_then(|h| u64::from_str_radix(h, 16).ok())
                .unwrap_or(0);
            out.push((i, h, Fnv::default()));
        } else if let Some((_, _, fnv)) = out.last_mut() {
            let bytes: Vec<u8> = (0..line.len() / 2)
                .filter_map(|k| u8::from_str_radix(&line[2 * k..2 * k + 2], 16).ok())
                .collect();
            fnv.bytes(&(bytes.len() as u32).to_le_bytes());
            fnv.bytes(&bytes);
        }
    }
    out.into_iter()
        .map(|(i, h, f)| (i, h, f.finish()))
        .collect()
}

/// One tick's state, read but not yet hashed.
struct Collected {
    /// Positions, health / flags, orders, projectiles.
    recs: [Records; 4],
    /// See [`Capture::hidden_hash`].
    hidden: Records,
    census: BTreeMap<&'static str, usize>,
    units: usize,
    projectiles: usize,
    nans: usize,
    total_entities: usize,
}

fn collect(world: &mut World) -> Collected {
    // --- census by archetype -------------------------------------------
    let registered = world.components().len();
    let cats = match world.get_resource::<CategoryCache>() {
        Some(c) if c.registered == registered => c.ids.clone(),
        _ => {
            let ids = category_ids(world);
            world.insert_resource(CategoryCache {
                registered: world.components().len(),
                ids: ids.clone(),
            });
            ids
        }
    };
    let mut census: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut projectile_entities: Vec<Entity> = Vec::new();
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
        if cat == "projectile" {
            projectile_entities.extend(archetype.entities().iter().map(|e| e.id()));
        }
    }

    // --- units -----------------------------------------------------------
    let mut q = world.query::<UnitQ>();
    let mut units = 0;
    // References to other units hash as their `NetId` (`u32::MAX`: not
    // a live unit).
    let id_of: HashMap<Entity, NetId> = q.iter(world).map(|((e, id, ..), ..)| (e, *id)).collect();
    let reference = |e: Entity| id_of.get(&e).map_or(u32::MAX, |id| id.0).to_le_bytes();
    let mut pos_recs = Records::default();
    let mut hp_recs = Records::default();
    let mut order_recs = Records::default();
    let mut bits = Bits::default();
    for (
        (_, id, ut, team, pose),
        (health, dying, emerging, cloaked, stunned),
        (mt, fight, guard, attack, attack_ground, pending_build, constructing, queue, producer),
        (clear, bombard, entering),
    ) in q.iter(world)
    {
        units += 1;
        let tf = pose.sim();
        let (hp, max) = health.map_or((0.0, 0.0), |h| (h.current, h.max));
        // bit 0 dying, 1 emerging / being built, 2 cloaked, 3 stunned,
        // 4 moving (has a move target), 5 fight order, 6 constructing,
        // 7 attack / guard order.
        let mut flags = 0u8;
        flags |= dying as u8;
        flags |= (emerging.is_some() as u8) << 1;
        flags |= (cloaked as u8) << 2;
        flags |= (stunned as u8) << 3;
        flags |= (mt.is_some() as u8) << 4;
        flags |= (fight as u8) << 5;
        flags |= (constructing.is_some() as u8) << 6;
        flags |= ((attack.is_some() || guard.is_some() || attack_ground.is_some()) as u8) << 7;
        // Every record leads with the unit's id: sorted, the records are
        // in `NetId` order, and a unit that spawned under another id
        // (a different spawn order) changes the hash.
        let me = id.0.to_le_bytes();
        let p = pos_recs.start(me);
        p.extend((ut.0 as u16).to_le_bytes());
        p.push(team.0);
        p.extend(bits.vec3(tf.translation));
        for c in tf.rotation.to_array() {
            p.extend(bits.f32(c));
        }

        let h = hp_recs.start(me);
        h.extend(bits.f32(hp));
        h.extend(bits.f32(max));
        h.push(flags);
        if let Some(em) = emerging {
            h.extend(bits.f32(em.remaining));
        }

        let o = order_recs.start(me);
        if let Some(mt) = mt {
            o.push(1);
            o.extend(bits.vec3(mt.0));
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
            o.extend(bits.vec3(a.pos));
        }
        if let Some(b) = pending_build {
            o.push(5);
            o.extend((b.kind as u16).to_le_bytes());
            o.extend(bits.vec3(b.site));
        }
        if let Some(c) = constructing {
            o.push(6);
            o.extend((c.kind as u16).to_le_bytes());
            o.extend(bits.vec3(c.site));
        }
        if let Some(q) = queue {
            o.push(7);
            o.extend((q.commands.len() as u32).to_le_bytes());
            for c in &q.commands {
                o.extend(bits.vec3(c.position()));
            }
        }
        if let Some(p) = producer {
            o.push(8);
            o.push(p.repeat() as u8);
            for k in p.queue() {
                o.extend((*k as u16).to_le_bytes());
            }
        }
        match clear {
            Some(ClearOrder::Feature { feature }) => {
                o.push(9);
                o.extend(reference(*feature));
            }
            Some(ClearOrder::Area {
                center,
                radius,
                current,
            }) => {
                o.push(10);
                o.extend(bits.vec3(*center));
                o.extend(bits.f32(*radius));
                o.extend(current.map_or([0; 4], reference));
            }
            None => {}
        }
        if let Some(b) = bombard {
            o.push(11);
            o.extend(b.target.map_or([0; 4], reference));
            o.extend(bits.vec3(b.pos));
            o.push(b.deploy_only as u8);
        }
        if let Some(e) = entering {
            o.push(12);
            o.extend(reference(e.target));
        }
    }

    // --- map features ----------------------------------------------------
    // With the units' positions and health: their ids share the counter.
    // A kind of `u16::MAX` and the feature's name mark the record.
    let mut fq = world.query::<(&NetId, &Feature, &Transform, &Health)>();
    for (id, feature, tf, health) in fq.iter(world) {
        let me = id.0.to_le_bytes();
        let p = pos_recs.start(me);
        p.extend(u16::MAX.to_le_bytes());
        p.extend(feature.def.name.as_bytes());
        p.extend(bits.vec3(tf.translation));
        hp_recs.start(me).extend(bits.f32(health.current));
    }

    // --- hidden state ------------------------------------------------------
    // Movement and construction bookkeeping, and the path service, as
    // their `Debug` text (exact floats; no entity ids, which differ
    // between machines): where a divergence that first shows in a
    // position was decided. A path's waypoints and node rectangles go
    // in as raw bytes: formatting them under Rosetta cost more than
    // the whole tick (run 40490871914: 45-minute rounds).
    let mut hidden = Records::default();
    let name = |e: Entity| id_of.get(&e).map_or(u32::MAX, |id| id.0);
    let mut hq = world.query::<(
        &NetId,
        Option<&GroundMover>,
        Option<&MovePath>,
        Option<&BuildSiteWait>,
    )>();
    for (id, mover, path, wait) in hq.iter(world) {
        if mover.is_none() && path.is_none() && wait.is_none() {
            continue;
        }
        let h = hidden.start(id.0.to_le_bytes());
        h.extend(format!("{mover:?}|").into_bytes());
        if let Some(p) = path {
            h.extend(
                format!(
                    "path {:?}|",
                    (
                        p.current,
                        p.goal,
                        p.reached_goal,
                        p.revision,
                        p.dirty_at,
                        p.repath_at,
                        p.clean_from,
                        p.waypoints.len(),
                        p.node_rects.len(),
                    )
                )
                .into_bytes(),
            );
            for w in &p.waypoints {
                h.extend(bits.vec3(*w));
            }
            for r in &p.node_rects {
                for v in [r.x0, r.z0, r.x1, r.z1] {
                    h.extend(v.to_le_bytes());
                }
            }
        } else {
            h.extend(b"path None|");
        }
        h.extend(format!("{wait:?}").into_bytes());
    }
    if let Some(pq) = world.get_resource::<PathQueue>() {
        hidden
            .start(u32::MAX.to_le_bytes())
            .extend(pq.describe(name).into_bytes());
    }

    // --- projectiles -----------------------------------------------------
    let projectiles = projectile_entities.len();
    let mut proj_recs = Records::default();
    for e in projectile_entities {
        let pos = world
            .get::<SimPose>(e)
            .map(|p| p.sim().translation)
            .or_else(|| world.get::<Transform>(e).map(|t| t.translation))
            .unwrap_or_default();
        proj_recs.start(bits.vec3(pos));
    }

    Collected {
        recs: [pos_recs, hp_recs, order_recs, proj_recs],
        hidden,
        census,
        units,
        projectiles,
        nans: bits.nans,
        total_entities,
    }
}

fn finish(c: &Collected) -> Capture {
    let part_hashes = c.recs.each_ref().map(Records::hash_sorted);
    let mut sim = Fnv::default();
    for p in part_hashes {
        sim.bytes(&p.to_le_bytes());
    }
    let mut fx = Fnv::default();
    for (cat, n) in c.census.iter().filter(|(c, _)| **c != "other") {
        let target = if SIM_CATEGORIES.contains(cat) {
            &mut sim
        } else {
            &mut fx
        };
        target.bytes(cat.as_bytes());
        target.bytes(&(*n as u64).to_le_bytes());
    }

    Capture {
        units: c.units,
        projectiles: c.projectiles,
        census: c.census.clone(),
        sim_hash: sim.finish(),
        fx_hash: fx.finish(),
        part_hashes,
        nans: c.nans,
        total_entities: c.total_entities,
        hidden_hash: c.hidden.hash_sorted(),
    }
}

/// Readable, sorted per-unit state (for diffing two runs at the tick
/// they diverge), in `NetId` order: kind, team, pose, health, flags
/// and orders.
pub fn describe(world: &mut World) -> Vec<String> {
    let mut q = world.query::<UnitQ>();
    let mut out: Vec<String> = q
        .iter(world)
        .map(
            |(
                (e, id, ut, team, pose),
                (health, dying, emerging, _, stunned),
                (mt, fight, guard, attack, attack_ground, pending_build, constructing, queue, producer),
                (clear, bombard, entering),
            )| {
                let tf = pose.sim();
                format!(
                    "#{:06} {:?} t{} pos {:?} rot {:?} hp {:?} dying {} emerging {:?} stunned {} | mt {:?} fight {} guard {} attack {} ag {:?} pb {:?} cons {:?} queue {:?} prod {:?} clear {:?} bombard {:?} enter {:?} | {e:?}",
                    id.0,
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
                    clear,
                    bombard.map(|b| (b.target, b.pos, b.deploy_only)),
                    entering.map(|e| e.target),
                )
            },
        )
        .collect();
    let mut fq = world.query::<(&NetId, &Feature, &Transform, &Health)>();
    out.extend(fq.iter(world).map(|(id, feature, tf, health)| {
        format!(
            "#{:06} feature {} pos {:?} hp {}",
            id.0, feature.def.name, tf.translation, health.current
        )
    }));
    // The movers' full state too (velocity, heading, path), one line
    // each, right after their unit's line once sorted.
    let mut mq = world.query::<(
        &NetId,
        Option<&GroundMover>,
        Option<&MovePath>,
        Option<&BuildSiteWait>,
    )>();
    for (id, mover, path, wait) in mq.iter(world) {
        if mover.is_some() || path.is_some() || wait.is_some() {
            out.push(format!(
                "#{:06} ~ mover {mover:?} path {path:?} wait {wait:?}",
                id.0
            ));
        }
    }
    // The path service, movers named by `NetId`.
    let mut iq = world.query::<(Entity, &NetId)>();
    let ids: HashMap<Entity, u32> = iq.iter(world).map(|(e, id)| (e, id.0)).collect();
    if let Some(pq) = world.get_resource::<PathQueue>() {
        out.push(format!(
            "pathqueue {}",
            pq.describe(|e| ids.get(&e).copied().unwrap_or(u32::MAX))
        ));
    }
    // The projectiles in flight, after the units once sorted.
    out.extend(crate::units::weapon_fx::describe_projectiles(world));
    out.sort();
    // Query iteration order, as unit ids (deterministic), with each
    // unit's archetype: what canonical sorts guard against.
    let mut sq = world.query::<(Entity, &NetId)>();
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
    out.extend(archetypes(world));
    out
}

/// Every archetype in creation order, with its entity count and its
/// components (short type names): diffing two machines' lists shows
/// where their archetype histories (and so their query orders) part.
pub fn archetypes(world: &World) -> Vec<String> {
    world
        .archetypes()
        .iter()
        .map(|a| {
            let mut names: Vec<String> = a
                .components()
                .iter()
                .filter_map(|id| world.components().get_name(*id))
                .map(|n| n.shortname().to_string())
                .collect();
            names.sort();
            format!(
                "archetype a{} n{} [{}]",
                a.id().index(),
                a.len(),
                names.join(" ")
            )
        })
        .collect()
}

//! Snapshot and command encoders for sizing replication traffic.
//!
//! Nothing here is a protocol; the encoders write the bytes a simple
//! but sensible one would, so their sizes are honest estimates:
//!
//! - **Replicated unit state** (what a thin client needs to draw a unit
//!   and its HUD): a server-assigned 16-bit net id, kind and team (on
//!   spawn only in deltas), position, orientation, health fraction,
//!   build progress and a flag byte (dying, being built, cloaked,
//!   stunned, moving, fight order, constructing, attacking).
//! - **Projectiles**: net id + position, every update.
//! - Not included (would travel as events or be derived client-side):
//!   COB piece poses (the client would have to run the unit scripts
//!   from replicated aim / fire state), weapon-fire and explosion
//!   events, CEG / death particles, fog-of-war filtering.

use std::collections::HashMap;

use bevy::prelude::*;

use super::state::{Capture, ProjRec, UnitRec};
use crate::netprobe::{ProbeCmd, ProbeKind, ProbeTarget};

/// Server-assigned stable ids (a client never sees `Entity`).
#[derive(Default)]
pub struct NetIds {
    ids: HashMap<Entity, u16>,
    next: u16,
}

impl NetIds {
    pub fn get(&mut self, e: Entity) -> u16 {
        *self.ids.entry(e).or_insert_with(|| {
            self.next = self.next.wrapping_add(1);
            self.next
        })
    }
    pub fn forget_missing(&mut self, live: &std::collections::HashSet<Entity>) {
        self.ids.retain(|e, _| live.contains(e));
    }
}

/// A unit quantized for the wire: 1/4-elmo positions (16 bit covers a
/// 16384-elmo map), 1/16-elmo heights, 16-bit yaw, 8-bit pitch / roll,
/// 16-bit health fraction, 8-bit build progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QUnit {
    pub id: u16,
    pub kind: u8,
    pub team: u8,
    pub x: u16,
    pub y: u16,
    pub z: u16,
    pub yaw: u16,
    pub pitch: i8,
    pub roll: i8,
    pub hp: u16,
    pub flags: u8,
    pub build: u8,
}

/// Wire size of a full [`QUnit`].
pub const QUNIT_FULL: usize = 18;

fn qpos(v: f32) -> u16 {
    (v * 4.0).round().clamp(0.0, 65535.0) as u16
}
fn qheight(v: f32) -> u16 {
    ((v + 1024.0) * 16.0).round().clamp(0.0, 65535.0) as u16
}

impl QUnit {
    pub fn from_rec(r: &UnitRec, id: u16) -> Self {
        let fwd = r.rot * Vec3::Z;
        let up = r.rot * Vec3::Y;
        let yaw = fwd.x.atan2(fwd.z);
        let pitch = fwd.y.clamp(-1.0, 1.0).asin();
        let roll = up.x.clamp(-1.0, 1.0).asin();
        let hp = if r.max_health > 0.0 {
            (r.health / r.max_health).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            id,
            kind: r.kind as u8,
            team: r.team,
            x: qpos(r.pos.x),
            y: qheight(r.pos.y),
            z: qpos(r.pos.z),
            yaw: ((yaw / std::f32::consts::TAU + 0.5) * 65535.0) as u16,
            pitch: (pitch / std::f32::consts::FRAC_PI_2 * 127.0) as i8,
            roll: (roll / std::f32::consts::FRAC_PI_2 * 127.0) as i8,
            hp: (hp * 65535.0).round() as u16,
            flags: r.flags,
            build: (r.build * 255.0).round() as u8,
        }
    }

    fn write_full(&self, out: &mut Vec<u8>) {
        out.extend(self.id.to_le_bytes());
        out.push(self.kind);
        out.push(self.team);
        out.extend(self.x.to_le_bytes());
        out.extend(self.y.to_le_bytes());
        out.extend(self.z.to_le_bytes());
        out.extend(self.yaw.to_le_bytes());
        out.push(self.pitch as u8);
        out.push(self.roll as u8);
        out.extend(self.hp.to_le_bytes());
        out.push(self.flags);
        out.push(self.build);
    }

    /// Changed fields vs `prev`: id, mask, then pos (6) / rot (4) /
    /// hp (2) / flags (1) / build (1). `None` if nothing changed.
    fn write_delta(&self, prev: &QUnit, out: &mut Vec<u8>) -> bool {
        let pos = (self.x, self.y, self.z) != (prev.x, prev.y, prev.z);
        let rot = (self.yaw, self.pitch, self.roll) != (prev.yaw, prev.pitch, prev.roll);
        let hp = self.hp != prev.hp;
        let flags = self.flags != prev.flags;
        let build = self.build != prev.build;
        let mask = pos as u8
            | (rot as u8) << 1
            | (hp as u8) << 2
            | (flags as u8) << 3
            | (build as u8) << 4;
        if mask == 0 {
            return false;
        }
        out.extend(self.id.to_le_bytes());
        out.push(mask);
        if pos {
            out.extend(self.x.to_le_bytes());
            out.extend(self.y.to_le_bytes());
            out.extend(self.z.to_le_bytes());
        }
        if rot {
            out.extend(self.yaw.to_le_bytes());
            out.push(self.pitch as u8);
            out.push(self.roll as u8);
        }
        if hp {
            out.extend(self.hp.to_le_bytes());
        }
        if flags {
            out.push(self.flags);
        }
        if build {
            out.push(self.build);
        }
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QProj {
    pub id: u16,
    pub x: u16,
    pub y: u16,
    pub z: u16,
}

impl QProj {
    fn from_rec(r: &ProjRec, id: u16) -> Self {
        Self {
            id,
            x: qpos(r.pos.x),
            y: qheight(r.pos.y),
            z: qpos(r.pos.z),
        }
    }
    fn write(&self, out: &mut Vec<u8>) {
        out.extend(self.id.to_le_bytes());
        out.extend(self.x.to_le_bytes());
        out.extend(self.y.to_le_bytes());
        out.extend(self.z.to_le_bytes());
    }
}

/// One tick's replicated state, quantized and keyed by net id.
#[derive(Clone, Default)]
pub struct QState {
    pub units: Vec<QUnit>,
    pub projs: Vec<QProj>,
}

impl QState {
    pub fn from_capture(c: &Capture, ids: &mut NetIds, proj_ids: &mut NetIds) -> Self {
        let mut units: Vec<QUnit> = c
            .units
            .iter()
            .map(|u| QUnit::from_rec(u, ids.get(u.entity)))
            .collect();
        units.sort_by_key(|u| u.id);
        let mut projs: Vec<QProj> = c
            .projectiles
            .iter()
            .map(|p| QProj::from_rec(p, proj_ids.get(p.entity)))
            .collect();
        projs.sort_by_key(|p| p.id);
        Self { units, projs }
    }
}

/// Full snapshot, raw f32 fields (the naive encoding): 44 B per unit,
/// 16 B per projectile.
pub fn full_raw(c: &Capture, with_projectiles: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + c.units.len() * 44);
    out.extend(0u32.to_le_bytes());
    out.extend((c.units.len() as u16).to_le_bytes());
    out.extend((c.projectiles.len() as u16).to_le_bytes());
    for u in &c.units {
        out.extend((u.entity.index_u32()).to_le_bytes());
        out.extend(u.kind.to_le_bytes());
        out.push(u.team);
        for f in u.pos.to_array() {
            out.extend(f.to_le_bytes());
        }
        for f in u.rot.to_array() {
            out.extend(f.to_le_bytes());
        }
        out.extend(u.health.to_le_bytes());
        out.push(u.flags);
        out.extend(u.build.to_le_bytes());
    }
    if with_projectiles {
        for p in &c.projectiles {
            out.extend(p.entity.index_u32().to_le_bytes());
            for f in p.pos.to_array() {
                out.extend(f.to_le_bytes());
            }
        }
    }
    out
}

/// Full snapshot, quantized.
pub fn full_quant(s: &QState, with_projectiles: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + s.units.len() * QUNIT_FULL);
    out.extend(0u32.to_le_bytes());
    out.extend((s.units.len() as u16).to_le_bytes());
    out.extend((s.projs.len() as u16).to_le_bytes());
    for u in &s.units {
        u.write_full(&mut out);
    }
    if with_projectiles {
        for p in &s.projs {
            p.write(&mut out);
        }
    }
    out
}

/// Delta snapshot vs `base` (both sorted by id): despawned ids, spawned
/// units in full, changed fields of the rest; projectiles: despawned
/// ids, then id + position of every live one (they all move).
pub fn delta_quant(base: &QState, s: &QState, with_projectiles: bool) -> Vec<u8> {
    let mut despawn = Vec::new();
    let mut spawn = Vec::new();
    let mut changed = Vec::new();
    let (mut n_despawn, mut n_spawn, mut n_changed) = (0u16, 0u16, 0u16);
    let (mut i, mut j) = (0, 0);
    while i < base.units.len() || j < s.units.len() {
        match (base.units.get(i), s.units.get(j)) {
            (Some(b), Some(u)) if b.id == u.id => {
                if u.write_delta(b, &mut changed) {
                    n_changed += 1;
                }
                i += 1;
                j += 1;
            }
            (Some(b), Some(u)) if b.id < u.id => {
                despawn.extend(b.id.to_le_bytes());
                n_despawn += 1;
                i += 1;
            }
            (Some(b), None) => {
                despawn.extend(b.id.to_le_bytes());
                n_despawn += 1;
                i += 1;
            }
            (_, Some(u)) => {
                u.write_full(&mut spawn);
                n_spawn += 1;
                j += 1;
            }
            (None, None) => unreachable!(),
        }
    }
    let mut out = Vec::with_capacity(16 + despawn.len() + spawn.len() + changed.len());
    out.extend(0u32.to_le_bytes()); // tick
    out.push(0); // base tick offset
    out.extend(n_despawn.to_le_bytes());
    out.extend(n_spawn.to_le_bytes());
    out.extend(n_changed.to_le_bytes());
    out.extend(despawn);
    out.extend(spawn);
    out.extend(changed);
    if with_projectiles {
        let live: std::collections::HashSet<u16> = s.projs.iter().map(|p| p.id).collect();
        let gone: Vec<u16> = base
            .projs
            .iter()
            .map(|p| p.id)
            .filter(|id| !live.contains(id))
            .collect();
        out.extend((gone.len() as u16).to_le_bytes());
        for id in gone {
            out.extend(id.to_le_bytes());
        }
        out.extend((s.projs.len() as u16).to_le_bytes());
        for p in &s.projs {
            p.write(&mut out);
        }
    }
    out
}

pub fn zstd_len(b: &[u8]) -> usize {
    zstd::bulk::compress(b, 3).map_or(b.len(), |c| c.len())
}

pub fn lz4_len(b: &[u8]) -> usize {
    lz4_flex::block::compress(b).len()
}

/// Wire size of one lockstep command: kind (1) + unit id (2) +
/// argument (position: 2 × 16-bit x/z — y follows from the heightmap;
/// unit: 2; kind + count: 2; build kind + site: 5; flag: 1).
pub fn command_size(c: &ProbeCmd) -> usize {
    3 + match c.target {
        ProbeTarget::None => 0,
        ProbeTarget::Pos(_) => 4,
        ProbeTarget::Unit(_) => 2,
        ProbeTarget::Kind(..) => 2,
        ProbeTarget::KindAt(..) => 5,
        ProbeTarget::Flag(_) => 1,
    }
}

/// Header of one player's per-turn command message: tick (4), player
/// (1), command count (1).
pub const COMMAND_HEADER: usize = 6;

pub fn kind_name(k: ProbeKind) -> &'static str {
    match k {
        ProbeKind::Move => "move",
        ProbeKind::AttackMove => "attack_move",
        ProbeKind::BuildAt => "build_at",
        ProbeKind::OtherOrder => "other_order",
        ProbeKind::FactoryEnqueue => "factory_enqueue",
        ProbeKind::FactoryRepeat => "factory_repeat",
        ProbeKind::CommandFire => "command_fire",
        ProbeKind::Deploy => "deploy",
        ProbeKind::Dispatch => "dispatch",
        ProbeKind::SelfDestruct => "self_destruct",
    }
}

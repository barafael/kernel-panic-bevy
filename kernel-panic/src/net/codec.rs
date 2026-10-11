//! Wire shapes of the command payload: quantized positions, sorted
//! delta-coded unit-id lists and delta-coded position lists, and the
//! `UnitKind` ↔ byte mapping. Everything here serializes through
//! postcard (varint integers, zigzag for signed), so a small delta costs
//! one byte.

use bevy::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use strum::VariantArray;

use super::NetId;
use crate::units::content::definitions::UnitKind;

/// Positions travel in 1/8 elmo (1/64 of a heightmap square). Far below
/// anything a click can aim at, and a power of two so the round trip to
/// `f32` is exact.
pub const POS_SCALE: f32 = 8.0;

/// A world position quantized to [`POS_SCALE`]. The issuing machine
/// applies the *quantized* value like everybody else, so the loss never
/// shows up as a desync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NetPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl NetPos {
    pub fn quantize(v: Vec3) -> Self {
        let q = |f: f32| (f * POS_SCALE).round() as i32;
        Self {
            x: q(v.x),
            y: q(v.y),
            z: q(v.z),
        }
    }

    pub fn to_vec3(self) -> Vec3 {
        Vec3::new(self.x as f32, self.y as f32, self.z as f32) / POS_SCALE
    }
}

impl From<Vec3> for NetPos {
    fn from(v: Vec3) -> Self {
        Self::quantize(v)
    }
}

/// A set of units, sorted by id and without duplicates. On the wire:
/// the first id, then the gaps (units spawned around the same time sit
/// close together, so a box selection is ~1 byte per unit).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct IdList(Vec<NetId>);

impl IdList {
    pub fn new(ids: impl IntoIterator<Item = NetId>) -> Self {
        let mut v: Vec<NetId> = ids.into_iter().filter(|id| id.0 != 0).collect();
        v.sort_unstable();
        v.dedup();
        Self(v)
    }

    pub fn ids(&self) -> &[NetId] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Serialize for IdList {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut prev = 0u32;
        s.collect_seq(self.0.iter().map(|id| {
            let d = id.0 - prev;
            prev = id.0;
            d
        }))
    }
}

impl<'de> Deserialize<'de> for IdList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let deltas = Vec::<u32>::deserialize(d)?;
        let mut acc = 0u32;
        let ids = deltas
            .into_iter()
            .map(|delta| {
                acc = acc.wrapping_add(delta);
                NetId(acc)
            })
            .collect();
        Ok(Self(ids))
    }
}

/// An ordered list of positions (formation slots, waypoints, a build
/// row). On the wire: the first position, then each one relative to its
/// predecessor — neighbouring slots are a few elmos apart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct PosList(pub Vec<NetPos>);

impl PosList {
    pub fn quantize(points: impl IntoIterator<Item = Vec3>) -> Self {
        Self(points.into_iter().map(NetPos::quantize).collect())
    }

    pub fn points(&self) -> impl Iterator<Item = Vec3> + '_ {
        self.0.iter().map(|p| p.to_vec3())
    }
}

impl Serialize for PosList {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut prev = NetPos::default();
        s.collect_seq(self.0.iter().map(|p| {
            let d = (p.x - prev.x, p.y - prev.y, p.z - prev.z);
            prev = *p;
            d
        }))
    }
}

impl<'de> Deserialize<'de> for PosList {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let deltas = Vec::<(i32, i32, i32)>::deserialize(d)?;
        let mut acc = NetPos::default();
        let points = deltas
            .into_iter()
            .map(|(dx, dy, dz)| {
                acc = NetPos {
                    x: acc.x.wrapping_add(dx),
                    y: acc.y.wrapping_add(dy),
                    z: acc.z.wrapping_add(dz),
                };
                acc
            })
            .collect();
        Ok(Self(points))
    }
}

/// `UnitKind` as its index in `UnitKind::VARIANTS` (one byte). Both ends
/// must run the same build — the handshake has to compare a content
/// hash anyway, since the sim itself is only deterministic per build.
pub mod kind_index {
    use super::*;

    pub fn serialize<S: Serializer>(kind: &UnitKind, s: S) -> Result<S::Ok, S::Error> {
        let idx = UnitKind::VARIANTS
            .iter()
            .position(|k| k == kind)
            .expect("every kind is a variant");
        s.serialize_u8(idx as u8)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<UnitKind, D::Error> {
        let idx = u8::deserialize(d)? as usize;
        UnitKind::VARIANTS
            .get(idx)
            .copied()
            .ok_or_else(|| serde::de::Error::custom("unit kind index out of range"))
    }
}

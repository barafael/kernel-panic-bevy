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

    /// The list exactly as given, sorted or not: what a malformed frame
    /// decodes to (the command fuzz builds them).
    #[cfg(any(test, all(feature = "replay", not(target_arch = "wasm32"))))]
    pub(crate) fn raw(ids: Vec<NetId>) -> Self {
        Self(ids)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{CommandFrame, DevCommand, SimCommand, Units};

    /// A frame of player 3 at tick 18 000 (ten minutes in), as
    /// `lockstep_tests::frame_sizes` measures them.
    fn frame(commands: Vec<SimCommand>) -> usize {
        CommandFrame {
            tick: 18_000,
            player: 3,
            commands,
        }
        .encode()
        .len()
    }

    /// The M1 commands' sizes (`net-experiments/m1-commands.md`):
    /// match control costs a few bytes per frame on top of the 5 of an
    /// empty one. `cargo test -p kernel-panic net::codec -- --nocapture`.
    #[test]
    fn m1_command_sizes() {
        let ten: Vec<NetId> = (0..10).map(|i| NetId(2000 + i * 17)).collect();
        let pos = NetPos::quantize(Vec3::new(3121.4, 87.2, 2650.9));
        let rows = [
            ("empty frame", frame(vec![]), 5),
            (
                "SetSpeed 150 %",
                frame(vec![SimCommand::SetSpeed { percent: 150 }]),
                8,
            ),
            (
                "SetSpeed 2000 %",
                frame(vec![SimCommand::SetSpeed { percent: 2000 }]),
                8,
            ),
            ("Pause", frame(vec![SimCommand::Pause { on: true }]), 7),
            ("Resign", frame(vec![SimCommand::Resign]), 6),
            ("Take", frame(vec![SimCommand::Take { from: 2 }]), 7),
            (
                "Dev GodMode",
                frame(vec![SimCommand::Dev(DevCommand::GodMode { on: true })]),
                8,
            ),
            (
                "Dev Spawn 10 Bits",
                frame(vec![SimCommand::Dev(DevCommand::Spawn {
                    kind: crate::units::content::definitions::UnitKind::Bit,
                    team: 1,
                    at: pos,
                    count: 10,
                    spacing: 24,
                })]),
                20,
            ),
            (
                "Dev Kill 10 units",
                frame(vec![SimCommand::Dev(DevCommand::Kill {
                    units: IdList::new(ten.iter().copied()),
                })]),
                20,
            ),
            (
                "Dev SetHealth 10 units",
                frame(vec![SimCommand::Dev(DevCommand::SetHealth {
                    units: IdList::new(ten.iter().copied()),
                    hp: 250,
                })]),
                22,
            ),
            (
                "Pause + SetSpeed + a 10-unit Stop",
                frame(vec![
                    SimCommand::Pause { on: false },
                    SimCommand::SetSpeed { percent: 300 },
                    SimCommand::Stop {
                        units: Units::of(ten.iter().copied()),
                    },
                ]),
                25,
            ),
        ];
        println!("{:<40} {:>5}", "frame", "bytes");
        for (name, size, at_most) in rows {
            println!("{name:<40} {size:>5}");
            assert!(size <= at_most, "{name}: {size} bytes > {at_most}");
        }
    }

    /// New variants append to the enum: the M0 variants keep their
    /// indices, so recorded replays still decode.
    #[test]
    fn m0_variant_indices_are_stable() {
        // The `Cast` variant (index 13) encodes its index as one byte.
        let bytes = postcard::to_allocvec(&SimCommand::Cast {
            units: Units::Prev,
            target: NetPos::default(),
            auto: false,
        })
        .unwrap();
        assert_eq!(bytes[0], 13);
        let bytes = postcard::to_allocvec(&SimCommand::SetSpeed { percent: 100 }).unwrap();
        assert_eq!(bytes[0], 14);
        let bytes =
            postcard::to_allocvec(&SimCommand::Dev(DevCommand::GodMode { on: true })).unwrap();
        assert_eq!(bytes[0], 17);
        let bytes = postcard::to_allocvec(&SimCommand::Take { from: 1 }).unwrap();
        assert_eq!(bytes[0], 18);
        let bytes = postcard::to_allocvec(&SimCommand::Dropped).unwrap();
        assert_eq!(bytes, [19], "M2: one byte");
        let bytes = postcard::to_allocvec(&SimCommand::InternalSpeed { percent: 62 }).unwrap();
        assert_eq!(bytes, [20, 62], "M3");
    }
}

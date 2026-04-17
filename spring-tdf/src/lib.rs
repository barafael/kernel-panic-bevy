//! Parser for Spring RTS engine TDF (Tag Definition Format) files.
//!
//! TDF is a simple nested configuration format used by the Spring engine
//! for weapon definitions, explosion effects, map metadata, and more.
//!
//! ```text
//! [WeaponName]
//! {
//!     key=value;
//!     [DAMAGE]
//!     {
//!         default=100;
//!     }
//! }
//! ```
//!
//! This crate provides:
//! - [`Tdf`]: a generic tree of sections and key-value pairs
//! - [`WeaponDefs`] / [`WeaponDef`]: typed weapon definitions parsed from TDF

mod explosion;
mod parse;
mod unit;
mod weapon;

pub use explosion::{
    EffectClass, EffectLayer, EffectProperties, ExplosionDef, ExplosionDefs, FlameProperties,
    GroundFlash, ParticleProperties, SpawnerProperties,
};
pub use parse::{ParseError, Section, Tdf};
pub use unit::{UnitDef, UnitDefs};
pub use weapon::{DamageMap, WeaponDef, WeaponDefs};

#[cfg(test)]
mod tests;

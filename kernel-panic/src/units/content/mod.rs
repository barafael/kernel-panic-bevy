//! Unit data: the `UnitKind` enum and the FBI/TDF-derived registries
//! (per-kind stats, per-weapon stats), parsed from the embedded
//! definitions (`defs`), plus the models and textures of the embedded
//! unit bundle (`bundle`). Pure data; no systems.

pub mod bundle;
pub mod definitions;
pub mod defs;
pub mod model;
pub mod moveinfo;
pub mod unit_registry;
pub mod weapons;

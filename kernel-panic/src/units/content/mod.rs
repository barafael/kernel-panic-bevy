//! Unit data loaded from disk: the `UnitKind` enum and the FBI/TDF-derived
//! registries (per-kind stats, per-weapon stats). Pure data; no systems.

pub mod definitions;
pub(crate) mod tdf_loader;
pub mod unit_registry;
pub mod weapons;

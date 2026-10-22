//! Unit data: the `UnitKind` enum and the FBI/TDF-derived registries
//! (per-kind stats, per-weapon stats), parsed from the embedded
//! definitions (`defs`), plus the models and textures of the embedded
//! unit bundle (`bundle`). Pure data; no systems.

pub mod bundle;
// The source CEG defs are written but not yet read: the runtime still
// evaluates the TDF path (`weapon_fx::ceg`). This attribute is deleted
// with the registry, one commit later (§5.3 of
// `docs/ceg-in-source-plan.md`).
#[allow(dead_code)]
pub mod ceg_def;
pub mod ceg_defs;
pub mod cegs;
pub mod definitions;
pub mod defs;
pub mod model;
pub mod moveinfo;
pub mod unit_registry;
pub mod weapons;

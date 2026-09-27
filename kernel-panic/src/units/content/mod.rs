//! Unit data: the `UnitKind` enum and the FBI/TDF-derived registries
//! (per-kind stats, per-weapon stats), read from the embedded unit
//! bundle (`bundle`). Pure data; no systems.

pub mod bundle;
pub mod definitions;
pub mod moveinfo;
/// The upstream TDF readers, used by the bake (`KP_BAKE_UNITS`) only.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod tdf_loader;
pub mod unit_registry;
pub mod weapons;

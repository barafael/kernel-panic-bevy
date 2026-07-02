//! Weapon registry loaded from upstream TDF files.
//!
//! At startup we read every `.tdf` file from the upstream weapons directory
//! and merge them into a single [`WeaponRegistry`] resource. The combat system
//! resolves weapon stats through this registry instead of hardcoded values.
//!
//! Weapons are addressable two ways:
//! - by name (`get(&str)`) — used at startup / Lua / tests
//! - by [`WeaponId`] (`by_id`) — `Copy`, `u16`-sized; used everywhere on the
//!   per-frame / per-shot hot path so we never hash a string in combat
//!
//! Slot 0 is reserved for the [`WeaponId::BUILD_LASER`] sentinel — a stub
//! `WeaponDef` that never makes it into damage resolution but is the
//! identity used by the visual layer for builder rays.

use bevy::prelude::*;
use spring_tdf::{WeaponDef, WeaponDefs};
use std::collections::HashMap;

use super::definitions::ALL_UNIT_KINDS;
use super::tdf_loader;

/// Compact identifier for a weapon. `Copy` so it can be cloned freely
/// in events and components without heap traffic.
#[derive(Component, Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct WeaponId(u16);

impl WeaponId {
    /// Sentinel id for the build-laser pseudo-weapon. Used by
    /// `production_system` and `start_construction` to drive a builder
    /// beam without going through TDF damage resolution.
    pub const BUILD_LASER: WeaponId = WeaponId(0);
}

const BUILD_LASER_NAME: &str = "BuildLaser";

#[derive(Resource)]
pub struct WeaponRegistry {
    /// Indexed by [`WeaponId`]; slot 0 is the build-laser stub.
    defs: Vec<WeaponDef>,
    /// Parallel `defs[i]` → name vector. Owned strings; cheap to look up.
    names: Vec<String>,
    /// Lower-case name → id, mirrors `WeaponDefs::get`'s case-insensitive
    /// lookup contract.
    index: HashMap<String, WeaponId>,
}

impl Default for WeaponRegistry {
    fn default() -> Self {
        Self::with_seed()
    }
}

impl WeaponRegistry {
    fn with_seed() -> Self {
        let mut registry = Self {
            defs: Vec::new(),
            names: Vec::new(),
            index: HashMap::new(),
        };
        // Reserve slot 0 for BuildLaser. Stub def stays at engine
        // defaults — the build-laser path special-cases this id before
        // any field is read.
        registry.insert(BUILD_LASER_NAME, WeaponDef::default());
        debug_assert_eq!(
            registry.intern(BUILD_LASER_NAME),
            Some(WeaponId::BUILD_LASER)
        );
        registry
    }

    /// Load all `.tdf` files from the upstream weapons directory.
    pub fn load() -> Self {
        let mut registry = Self::with_seed();

        let Some(dir) = tdf_loader::find_upstream_dir("weapons") else {
            warn!("Upstream weapons directory not found — using empty registry");
            return registry;
        };

        let mut total = 0usize;
        for (filename, tdf) in tdf_loader::load_all_tdf_files(&dir, "tdf") {
            let defs = WeaponDefs::from_tdf(&tdf);
            let count = defs.weapons.len();
            for (name, def) in defs.weapons {
                registry.insert(&name, def);
            }
            total += count;
            info!("  Loaded {count} weapons from {filename}");
        }

        info!("Weapon registry: {} definitions total", total);
        registry
    }

    fn insert(&mut self, name: &str, def: WeaponDef) -> WeaponId {
        let key = name.to_ascii_lowercase();
        if let Some(&existing) = self.index.get(&key) {
            // Later TDF entries with the same name win — matches the
            // previous behaviour of `extend()` overwriting on key clash.
            self.defs[existing.0 as usize] = def;
            return existing;
        }
        let id = WeaponId(self.defs.len() as u16);
        self.defs.push(def);
        self.names.push(name.to_string());
        self.index.insert(key, id);
        id
    }

    /// Look up a weapon by its TDF section name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&WeaponDef> {
        self.intern(name).map(|id| &self.defs[id.0 as usize])
    }

    /// Resolve a name to its compact id. `O(1)` after a single hash.
    pub fn intern(&self, name: &str) -> Option<WeaponId> {
        self.index.get(&name.to_ascii_lowercase()).copied()
    }

    /// Look up a weapon by id. Branchless `Vec` indexing.
    pub fn by_id(&self, id: WeaponId) -> &WeaponDef {
        &self.defs[id.0 as usize]
    }

    /// The TDF section name behind a [`WeaponId`].
    pub fn name(&self, id: WeaponId) -> &str {
        &self.names[id.0 as usize]
    }

    /// Test-only: register an ad-hoc weapon under a chosen name and
    /// return its id. Production paths go through `load()`.
    #[cfg(test)]
    pub fn insert_for_test(&mut self, name: &str, def: WeaponDef) -> WeaponId {
        self.insert(name, def)
    }

    /// Warn if any unit references a weapon name not in the registry.
    /// Call after both registries are loaded (e.g. from a startup system).
    /// Also logs each resolved (unit → weapon, reloadtime) binding at
    /// info level so `reload_time` regressions are visible in the game
    /// log — observed in-game fire rate should match these values within
    /// one frame, otherwise something downstream (double-tick, AimTarget
    /// gate, deploy state) is stealing shots.
    pub fn validate_unit_weapon_bindings(
        &self,
        unit_registry: &super::unit_registry::UnitRegistry,
    ) {
        for &kind in ALL_UNIT_KINDS {
            let weapon = unit_registry.weapon(kind);
            if weapon.is_empty() {
                continue;
            }
            match self.get(weapon) {
                None => {
                    warn!(
                        "Unit '{:?}' references weapon '{}' which is not in the TDF registry",
                        kind, weapon,
                    );
                }
                Some(def) => {
                    info!(
                        "  {:?} → {} (reload={}s, burst={}, burstrate={}s, range={})",
                        kind, weapon, def.reload_time, def.burst, def.burst_rate, def.range,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_laser_reserved_at_slot_zero() {
        let registry = WeaponRegistry::default();
        assert_eq!(registry.intern("BuildLaser"), Some(WeaponId::BUILD_LASER));
        assert_eq!(registry.intern("buildlaser"), Some(WeaponId::BUILD_LASER));
        assert_eq!(registry.name(WeaponId::BUILD_LASER), "BuildLaser");
    }

    #[test]
    fn intern_is_case_insensitive() {
        let mut registry = WeaponRegistry::default();
        let id = registry.insert_for_test("BugCannon", WeaponDef::default());
        assert_eq!(registry.intern("bugcannon"), Some(id));
        assert_eq!(registry.intern("BUGCANNON"), Some(id));
        assert!(id.0 > 0, "non-builtin weapons must follow BuildLaser");
    }

    #[test]
    fn by_id_returns_inserted_def() {
        let mut registry = WeaponRegistry::default();
        let mut def = WeaponDef::default();
        def.range = 600.0;
        let id = registry.insert_for_test("TestLaser", def);
        assert_eq!(registry.by_id(id).range, 600.0);
    }

    #[test]
    fn duplicate_insert_overwrites_in_place() {
        let mut registry = WeaponRegistry::default();
        let mut a = WeaponDef::default();
        a.range = 100.0;
        let id1 = registry.insert_for_test("Dup", a);

        let mut b = WeaponDef::default();
        b.range = 999.0;
        let id2 = registry.insert_for_test("dup", b);

        assert_eq!(id1, id2, "same name should reuse slot");
        assert_eq!(registry.by_id(id1).range, 999.0);
    }
}

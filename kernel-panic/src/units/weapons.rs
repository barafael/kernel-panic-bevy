//! Weapon registry loaded from upstream TDF files.
//!
//! At startup we read every `.tdf` file from the upstream weapons directory
//! and merge them into a single [`WeaponRegistry`] resource. The combat system
//! resolves weapon stats through this registry instead of hardcoded values.

use bevy::prelude::*;
use spring_tdf::{WeaponDef, WeaponDefs};

use super::definitions::ALL_UNIT_KINDS;
use super::tdf_loader;

/// All parsed weapon definitions, keyed by TDF section name.
#[derive(Resource, Default)]
pub struct WeaponRegistry {
    defs: WeaponDefs,
}

impl WeaponRegistry {
    /// Load all `.tdf` files from the upstream weapons directory.
    pub fn load() -> Self {
        let Some(dir) = tdf_loader::find_upstream_dir("weapons") else {
            warn!("Upstream weapons directory not found — using empty registry");
            return Self::default();
        };

        let mut merged = WeaponDefs::default();
        for (filename, tdf) in tdf_loader::load_all_tdf_files(&dir, "tdf") {
            let defs = WeaponDefs::from_tdf(&tdf);
            let count = defs.weapons.len();
            merged.weapons.extend(defs.weapons);
            info!("  Loaded {count} weapons from {filename}");
        }

        info!(
            "Weapon registry: {} definitions total",
            merged.weapons.len()
        );
        Self { defs: merged }
    }

    /// Look up a weapon by its TDF section name.
    pub fn get(&self, name: &str) -> Option<&WeaponDef> {
        self.defs.get(name)
    }

    /// Warn if any unit references a weapon name not in the registry.
    /// Call after both registries are loaded (e.g. from a startup system).
    pub fn validate_unit_weapon_bindings(
        &self,
        unit_registry: &super::unit_registry::UnitRegistry,
    ) {
        for kind in ALL_UNIT_KINDS {
            let weapon = unit_registry.weapon(kind);
            if !weapon.is_empty() && self.defs.get(weapon).is_none() {
                warn!(
                    "Unit '{:?}' references weapon '{}' which is not in the TDF registry",
                    kind, weapon,
                );
            }
        }
    }
}

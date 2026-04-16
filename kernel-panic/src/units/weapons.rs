//! Weapon registry loaded from upstream TDF files.
//!
//! At startup we read every `.tdf` file from the upstream weapons directory
//! and merge them into a single [`WeaponRegistry`] resource. The combat system
//! resolves weapon stats through this registry instead of hardcoded values.

use std::path::{Path, PathBuf};

use bevy::prelude::*;
use spring_tdf::{ParseError, Tdf, WeaponDef, WeaponDefs};
use thiserror::Error;

use super::definitions::UNIT_STATS;

#[derive(Debug, Error)]
enum WeaponLoadError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("parse error: {0}")]
    Parse(#[from] ParseError),
}

/// All parsed weapon definitions, keyed by TDF section name.
#[derive(Resource, Default)]
pub struct WeaponRegistry {
    defs: WeaponDefs,
}

impl WeaponRegistry {
    /// Load all `.tdf` files from the upstream weapons directory.
    pub fn load() -> Self {
        let Some(dir) = find_weapons_dir() else {
            warn!("Upstream weapons directory not found — using empty registry");
            return Self::default();
        };

        let mut merged = WeaponDefs::default();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            warn!("Failed to read weapons directory: {}", dir.display());
            return Self::default();
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    warn!("Failed to read directory entry: {err}");
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "tdf") {
                match load_weapon_file(&path) {
                    Ok(defs) => {
                        let count = defs.weapons.len();
                        merged.weapons.extend(defs.weapons);
                        info!("  Loaded {} weapons from {}", count, path.display());
                    }
                    Err(err) => {
                        warn!("Failed to load {}: {err}", path.display());
                    }
                }
            }
        }

        info!(
            "Weapon registry: {} definitions total",
            merged.weapons.len()
        );
        let registry = Self { defs: merged };
        registry.validate_unit_weapon_bindings();
        registry
    }

    /// Look up a weapon by its TDF section name.
    pub fn get(&self, name: &str) -> Option<&WeaponDef> {
        self.defs.get(name)
    }

    /// Warn at startup if any unit references a weapon name not in the registry.
    fn validate_unit_weapon_bindings(&self) {
        for unit in &UNIT_STATS {
            if !unit.weapon.is_empty() && self.defs.get(unit.weapon).is_none() {
                warn!(
                    "Unit '{}' references weapon '{}' which is not in the TDF registry \
                     — falling back to hardcoded stats",
                    unit.name, unit.weapon,
                );
            }
        }
    }
}

fn find_weapons_dir() -> Option<PathBuf> {
    const CANDIDATES: &[&str] = &[
        "upstream/Kernel-Panic/weapons",
        "kernel-panic/upstream/Kernel-Panic/weapons",
    ];
    CANDIDATES
        .iter()
        .map(Path::new)
        .find(|p| p.is_dir())
        .map(Path::to_path_buf)
}

fn load_weapon_file(path: &Path) -> Result<WeaponDefs, WeaponLoadError> {
    let text = std::fs::read_to_string(path)?;
    let tdf = Tdf::parse(&text)?;
    Ok(WeaponDefs::from_tdf(&tdf))
}

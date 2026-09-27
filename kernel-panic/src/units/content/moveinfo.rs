//! Upstream `gamedata/MOVEINFO.TDF` — movement-class pathing params.
//!
//! Two consumers: the heat-mapping fields (`HeatMapping`,
//! `HeatProduced`, `HeatMod`) drive
//! [`crate::interaction::movement::PathHeat`]; `FootprintX/Z` and
//! `CrushStrength` define each class's `MoveDef` — the footprint every
//! ground unit of the class collides and paths with
//! (`MoveDefHandler.cpp:314-319`), whatever its FBI footprint says.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Heat params for one movement class, verbatim from MOVEINFO.TDF.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MoveClassParams {
    /// Heat deposited per second of walking across a cell.
    pub heat_produced: f32,
    /// Fraction of heat that survives one second of decay. Upstream
    /// expresses this per decay tick; we normalize to per-second so
    /// the decay period is a free parameter.
    pub heat_retention: f32,
}

/// Upstream LIGHT class (`HeatProduced=10`, `HeatMod=0.10`) — the
/// fallback for classes missing from the file and units without a
/// movement class at all (flyers, unloaded registries).
pub const DEFAULT_HEAT_PARAMS: MoveClassParams = MoveClassParams {
    heat_produced: 10.0,
    heat_retention: 0.1,
};

/// The `MoveDef` fields of one MOVEINFO.TDF class.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MoveClassDef {
    /// `FootprintX` / `FootprintZ` in FBI footprint units (16 elmos).
    pub footprint_x: f32,
    pub footprint_z: f32,
    /// `CrushStrength`: features/units with a lower crush resistance
    /// are driven over and crushed (`CMoveMath::CrushResistant`).
    pub crush_strength: f32,
    /// `MaxSlope` in degrees (FBI encoding, before `DegreesToMaxSlope`).
    pub max_slope_deg: f32,
}

/// Baked into the unit bundle; `BTreeMap`s so the bake is reproducible.
#[derive(Resource, Debug, Clone, Default, Serialize, Deserialize)]
pub struct MoveClassTable {
    classes: BTreeMap<String, MoveClassParams>,
    defs: BTreeMap<String, MoveClassDef>,
}

impl MoveClassTable {
    /// The baked `gamedata/MOVEINFO.TDF` from the unit bundle (the
    /// registry takes it from the bundle directly; tests read it here).
    #[cfg(test)]
    pub fn load() -> Self {
        super::bundle::bundle().move_classes.clone()
    }

    /// Number of classes in the table (the bake's report).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Parse a `MOVEINFO.TDF` tree (the bake side of `bundle`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_tdf(tdf: &spring_tdf::Tdf) -> Self {
        let mut classes = BTreeMap::new();
        let mut defs = BTreeMap::new();
        for section in &tdf.sections {
            let name = section.string_clean("name").to_ascii_lowercase();
            if name.is_empty() {
                continue;
            }
            let fx = section.f32("footprintx").max(1.0);
            let fz = match section.f32("footprintz") {
                z if z > 0.0 => z,
                _ => fx,
            };
            defs.insert(
                name.clone(),
                MoveClassDef {
                    footprint_x: fx,
                    footprint_z: fz,
                    crush_strength: section.f32("crushstrength"),
                    max_slope_deg: section.f32("maxslope"),
                },
            );
            if !section.bool("heatmapping") {
                continue;
            }
            let produced = section.f32("heatproduced");
            let retention = section.f32("heatmod");
            classes.insert(
                name,
                MoveClassParams {
                    heat_produced: produced,
                    heat_retention: retention.clamp(0.001, 1.0),
                },
            );
        }
        info!(
            "MoveClassTable: {} heat-producing classes from MOVEINFO.TDF",
            classes.len(),
        );
        Self { classes, defs }
    }

    /// The `MoveDef` of class `class` (FBI `MovementClass`),
    /// case-insensitive.
    pub fn def_for(&self, class: &str) -> Option<MoveClassDef> {
        self.defs.get(&class.to_ascii_lowercase()).copied()
    }

    /// Params for a class name (FBI `MovementClass`, e.g. "LIGHT"),
    /// case-insensitive, with the LIGHT fallback.
    pub fn params_for(&self, class: &str) -> MoveClassParams {
        self.classes
            .get(&class.to_ascii_lowercase())
            .copied()
            .unwrap_or(DEFAULT_HEAT_PARAMS)
    }

    /// Per-second retention of the single shared heat grid: the most
    /// persistent class (highest `HeatMod`-derived retention), so no
    /// class's trail fades faster than upstream lets it. LIGHT's 0.10
    /// in KP's MOVEINFO; the LIGHT default when the table is empty.
    pub fn shared_heat_retention(&self) -> f32 {
        self.classes
            .values()
            .map(|p| p.heat_retention)
            .fold(None, |max: Option<f32>, r| {
                Some(max.map_or(r, |m| m.max(r)))
            })
            .unwrap_or(DEFAULT_HEAT_PARAMS.heat_retention)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real MOVEINFO.TDF parses into the three KP classes with
    /// their authored numbers.
    #[test]
    fn loads_upstream_moveinfo_classes() {
        let table = MoveClassTable::load();
        let light = table.params_for("LIGHT");
        assert!((light.heat_produced - 10.0).abs() < 1e-4);
        assert!((light.heat_retention - 0.10).abs() < 1e-4);

        let medium = table.params_for("medium");
        assert!((medium.heat_produced - 300.0).abs() < 1e-4);
        assert!((medium.heat_retention - 0.005).abs() < 1e-4);

        let heavy = table.params_for("Heavy");
        assert!((heavy.heat_produced - 500.0).abs() < 1e-4);
        assert!((heavy.heat_retention - 0.002).abs() < 1e-4);
    }

    /// The KP classes' MoveDefs: LIGHT 2×2 crush 40, MEDIUM 4×4 crush
    /// 60, HEAVY 4×4 crush 300, all MaxSlope 36.
    #[test]
    fn loads_upstream_move_defs() {
        let table = MoveClassTable::load();
        let light = table.def_for("light").unwrap();
        assert_eq!((light.footprint_x, light.crush_strength), (2.0, 40.0));
        let medium = table.def_for("MEDIUM").unwrap();
        assert_eq!((medium.footprint_x, medium.crush_strength), (4.0, 60.0));
        let heavy = table.def_for("heavy").unwrap();
        assert_eq!(
            (heavy.footprint_z, heavy.crush_strength, heavy.max_slope_deg),
            (4.0, 300.0, 36.0)
        );
    }

    /// Unknown classes fall back to the LIGHT defaults.
    #[test]
    fn unknown_class_falls_back_to_light() {
        let table = MoveClassTable::default();
        let params = table.params_for("NONEXISTENT");
        assert_eq!(params.heat_produced, DEFAULT_HEAT_PARAMS.heat_produced);
        assert_eq!(params.heat_retention, DEFAULT_HEAT_PARAMS.heat_retention);
    }
}

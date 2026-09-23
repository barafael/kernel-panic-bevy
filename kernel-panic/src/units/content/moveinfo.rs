//! Upstream `gamedata/MOVEINFO.TDF` — movement-class pathing params.
//!
//! The port only consumes the heat-mapping fields (`HeatMapping`,
//! `HeatProduced`, `HeatMod`); everything else in the file (footprints,
//! crush strength, water depth) is either read from the FBIs or unused
//! here. These params drive [`crate::interaction::movement::PathHeat`],
//! the congestion grid that makes marching columns fan out.

use bevy::prelude::*;
use spring_tdf::Tdf;
use std::collections::HashMap;

use super::tdf_loader;

/// Heat params for one movement class, verbatim from MOVEINFO.TDF.
#[derive(Debug, Clone, Copy)]
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

#[derive(Resource, Debug, Clone, Default)]
pub struct MoveClassTable {
    classes: HashMap<String, MoveClassParams>,
}

impl MoveClassTable {
    /// Parse `gamedata/MOVEINFO.TDF` from the upstream tree. Missing
    /// file or parse errors degrade to an empty table (all lookups
    /// fall back to [`DEFAULT_HEAT_PARAMS`]) so the game still boots.
    pub fn load() -> Self {
        let Some(dir) = tdf_loader::find_upstream_dir("gamedata") else {
            warn!("Upstream gamedata directory not found — heat pathing uses defaults");
            return Self::default();
        };
        let path = dir.join("MOVEINFO.TDF");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) => {
                warn!("MOVEINFO.TDF unreadable at {path:?}: {err} — heat pathing uses defaults");
                return Self::default();
            }
        };
        let tdf = match Tdf::parse(&text) {
            Ok(tdf) => tdf,
            Err(err) => {
                warn!("MOVEINFO.TDF unparsable: {err} — heat pathing uses defaults");
                return Self::default();
            }
        };

        let mut classes = HashMap::new();
        for section in &tdf.sections {
            let name = section.string_clean("name").to_ascii_lowercase();
            if name.is_empty() || !section.bool("heatmapping") {
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
        Self { classes }
    }

    /// Params for a class name (FBI `MovementClass`, e.g. "LIGHT"),
    /// case-insensitive, with the LIGHT fallback.
    pub fn params_for(&self, class: &str) -> MoveClassParams {
        self.classes
            .get(&class.to_ascii_lowercase())
            .copied()
            .unwrap_or(DEFAULT_HEAT_PARAMS)
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

    /// Unknown classes fall back to the LIGHT defaults.
    #[test]
    fn unknown_class_falls_back_to_light() {
        let table = MoveClassTable::default();
        let params = table.params_for("NONEXISTENT");
        assert_eq!(params.heat_produced, DEFAULT_HEAT_PARAMS.heat_produced);
        assert_eq!(params.heat_retention, DEFAULT_HEAT_PARAMS.heat_retention);
    }
}

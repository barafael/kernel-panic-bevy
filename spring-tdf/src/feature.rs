//! Feature definitions: the `features/**/*.tdf` sections the engine
//! reads into `FeatureDef`s (`FeatureDefHandler::CreateFeatureDef`),
//! with its defaults.

use std::collections::BTreeMap;

use spring_detmath::DetFloat;

use crate::Section;

/// One `[name]` feature section.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeatureDef {
    /// The section name, lower-cased: what a map's feature list names.
    pub name: String,
    pub description: String,
    /// `object`: the model.
    pub object: String,
    /// `footprintX` / `footprintZ`, in FBI units (×2 squares).
    pub footprint_x: u32,
    pub footprint_z: u32,
    pub height: f32,
    /// `blocking` (default on): occupies its footprint.
    pub blocking: bool,
    /// `health`, else `damage`: hit points.
    pub health: f32,
    /// `indestructible` off.
    pub destructable: bool,
    /// TDF `reclaimable` (default: `destructable`): a builder whose site
    /// it stands on clears it away first (`CBuilderCAI::ReclaimFeature`);
    /// one that isn't blocks the site.
    pub clearable: bool,
    /// TDF `reclaimTime`, else `max(1, (metal + energy) · 6)`: the build
    /// power it takes to clear, in build-time units.
    pub clear_time: f32,
    pub metal: f32,
    pub energy: f32,
    /// `mass`, else `metal·0.4 + health·0.1`, clamped to the engine's
    /// `[MINIMUM_MASS, MAXIMUM_MASS]`.
    pub mass: f32,
    /// `crushResistance`, else the mass: a mover crushes the feature
    /// when its MoveDef `crushStrength` is greater.
    pub crush_resistance: f32,
    /// `featureDead`, lower-cased: what the feature leaves when it is
    /// destroyed; empty for nothing.
    pub feature_dead: String,
}

/// `CSolidObject::MINIMUM_MASS` / `MAXIMUM_MASS`.
const MINIMUM_MASS: f32 = 1.0;
const MAXIMUM_MASS: f32 = 1e6;

impl FeatureDef {
    fn from_section(s: &Section) -> Self {
        let footprint = |key: &str| s.f32_or(key, 1.0).det_max(1.0) as u32;
        let health = s.f32_or("health", s.f32("damage"));
        let metal = s.f32("metal");
        let energy = s.f32("energy");
        let destructable = !s.bool("indestructible");
        let mass = s
            .f32_or("mass", metal * 0.4 + health * 0.1)
            .det_clamp(MINIMUM_MASS, MAXIMUM_MASS);
        Self {
            name: s.name.trim().to_ascii_lowercase(),
            description: s.string("description"),
            object: s.string("object"),
            footprint_x: footprint("footprintx"),
            footprint_z: footprint("footprintz"),
            height: s.f32("height"),
            blocking: s.get("blocking").is_none_or(|v| v.trim() == "1"),
            health,
            destructable,
            clearable: s
                .get("reclaimable")
                .map_or(destructable, |v| v.trim() == "1"),
            clear_time: s.f32_or("reclaimtime", (metal + energy) * 6.0).det_max(1.0),
            metal,
            energy,
            mass,
            crush_resistance: s.f32_or("crushresistance", mass),
            feature_dead: s.string("featuredead").trim().to_ascii_lowercase(),
        }
    }
}

/// Every feature definition, keyed by lower-cased name.
#[derive(Debug, Clone, Default)]
pub struct FeatureDefs {
    pub features: BTreeMap<String, FeatureDef>,
}

impl FeatureDefs {
    /// Every top-level section of a feature TDF.
    pub fn from_tdf(tdf: &crate::Tdf) -> Self {
        let features = tdf
            .sections
            .iter()
            .map(FeatureDef::from_section)
            .map(|def| (def.name.clone(), def))
            .collect();
        Self { features }
    }

    /// Look a feature up by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&FeatureDef> {
        self.features.get(&name.trim().to_ascii_lowercase())
    }

    /// Merge another set of definitions into this one; `other` wins.
    pub fn merge(&mut self, other: FeatureDefs) {
        self.features.extend(other.features);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defs(text: &str) -> FeatureDefs {
        FeatureDefs::from_tdf(&crate::Tdf::parse(text).unwrap())
    }

    /// `features/corpses/badblock.tdf`: no `mass`, so the engine's
    /// `metal·0.4 + health·0.1` = 65.6, which is also its crush
    /// resistance; no `featureDead`.
    #[test]
    fn badblock_takes_the_engine_defaults() {
        let defs = defs(
            "[BADBLOCK]\n{\n description=Bad Block;\n object=badblock.s3o;\n footprintx=2;\n \
             footprintz=2;\n height=22;\n blocking=1;\n hitdensity=100;\n metal=64;\n damage=400;\n \
             reclaimable=1;\n energy=0;\n}\n",
        );
        let bb = defs.get("BadBlock").unwrap();
        assert_eq!(bb.name, "badblock");
        assert_eq!((bb.footprint_x, bb.footprint_z), (2, 2));
        assert!(bb.blocking && bb.destructable);
        assert_eq!(bb.health, 400.0);
        assert!((bb.mass - 65.6).abs() < 1e-4);
        assert_eq!(bb.crush_resistance, bb.mass);
        assert!(bb.feature_dead.is_empty());
        assert!(bb.clearable);
        assert_eq!(bb.clear_time, 384.0);
    }

    /// Central Hub's `unusablesector.tdf`: an explicit `mass=500`, and
    /// each stage leaves the next.
    #[test]
    fn unusable_sectors_chain_and_weigh_500() {
        let defs = defs(
            "[unusablesector4]\n{\n object=unusablesector4.s3o;\n featuredead=unusablesector3;\n \
             footprintx=2;\n footprintz=2;\n metal=64;\n damage=400;\n mass=500;\n}\n\
             [unusablesector3]\n{\n object=unusablesector3.s3o;\n metal=32;\n damage=400;\n mass=500;\n}\n",
        );
        let s4 = defs.get("unusablesector4").unwrap();
        assert_eq!(s4.feature_dead, "unusablesector3");
        assert_eq!(s4.crush_resistance, 500.0);
        assert!(defs.get("unusablesector3").unwrap().feature_dead.is_empty());
        // `blocking` defaults on.
        assert!(s4.blocking);
    }
}

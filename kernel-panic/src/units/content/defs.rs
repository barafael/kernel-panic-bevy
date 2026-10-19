//! The unit definitions: Kernel Panic's `units/*.fbi`, `weapons/*.tdf`,
//! `gamedata/explosions/*.tdf`, `features/corpses/*.tdf` (plus the map
//! features maps ship themselves: Central Hub's `unusablesector.tdf`)
//! and `gamedata/MOVEINFO.TDF`, committed
//! as `kernel-panic/assets/defs/` and embedded as text (`build.rs`),
//! parsed once on first use.
//!
//! They stay text rather than going into the unit bundle because the
//! sim keeps porting engine behaviours that read another key: a new
//! field is a one-line change to the typed reader (`spring_tdf`) and
//! no re-bake. The models and textures, which never gain fields, are
//! in the baked bundle (`bundle`).
//!
//! Every file name is lower-cased (the original game ran on a
//! case-insensitive VFS) and the files are parsed in name order, so
//! where two files define the same name the later one wins, as the
//! bake used to resolve it.

use std::sync::LazyLock;

use bevy::prelude::*;
use spring_tdf::{ExplosionDefs, FeatureDefs, Tdf, UnitDefs, WeaponDefs};

use super::moveinfo::MoveClassTable;

include!(concat!(env!("OUT_DIR"), "/defs_catalog.rs"));

const _: () = assert!(!UNIT_FBIS.is_empty(), "no unit FBIs in assets/defs/units");

/// The parsed definitions.
pub struct UnitDefinitions {
    /// Every `units/*.fbi`, keyed by lower-cased `unitname`.
    pub units: UnitDefs,
    /// `moveinfo.tdf`.
    pub move_classes: MoveClassTable,
    /// Every `weapons/*.tdf`, keyed by section name.
    pub weapons: WeaponDefs,
    /// Every `explosions/*.tdf`, keyed by section name.
    pub explosions: ExplosionDefs,
    /// Every `features/*.tdf`, keyed by lower-cased section name.
    pub features: FeatureDefs,
}

/// The definitions, parsed on first use. Every registry `load()` reads
/// from here.
pub fn defs() -> &'static UnitDefinitions {
    static DEFS: LazyLock<UnitDefinitions> = LazyLock::new(|| {
        let start = bevy::platform::time::Instant::now();
        let defs = parse_all();
        info!(
            "Unit defs: {} units, {} move classes, {} weapons, {} CEGs parsed in {:.1} ms",
            defs.units.units.len(),
            defs.move_classes.len(),
            defs.weapons.weapons.len(),
            defs.explosions.explosions.len(),
            start.elapsed().as_secs_f64() * 1e3,
        );
        defs
    });
    &DEFS
}

/// The embedded text, file by file in catalog order (the build hash
/// peers compare, `net::compat`).
pub fn texts() -> impl Iterator<Item = &'static str> {
    [UNIT_FBIS, WEAPON_TDFS, EXPLOSION_TDFS, FEATURE_TDFS]
        .into_iter()
        .flatten()
        .map(|(_, text)| *text)
        .chain(std::iter::once(MOVEINFO_TDF))
}

fn parse_all() -> UnitDefinitions {
    let mut units = UnitDefs::default();
    for (name, text) in UNIT_FBIS {
        units.merge(UnitDefs::from_tdf(&parse(name, text)));
    }
    let mut weapons = WeaponDefs::default();
    for (name, text) in WEAPON_TDFS {
        weapons.merge(WeaponDefs::from_tdf(&parse(name, text)));
    }
    let mut explosions = ExplosionDefs::default();
    for (name, text) in EXPLOSION_TDFS {
        explosions.merge(ExplosionDefs::from_tdf(&parse(name, text)));
    }
    let mut features = FeatureDefs::default();
    for (name, text) in FEATURE_TDFS {
        features.merge(FeatureDefs::from_tdf(&parse(name, text)));
    }
    UnitDefinitions {
        units,
        move_classes: MoveClassTable::from_tdf(&parse("moveinfo.tdf", MOVEINFO_TDF)),
        weapons,
        explosions,
        features,
    }
}

/// The files are committed assets: one that does not parse is a bug
/// in the checkout, not a runtime condition.
fn parse(name: &str, text: &str) -> Tdf {
    Tdf::parse(text).unwrap_or_else(|error| panic!("assets/defs: {name}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_def_file_parses() {
        let defs = defs();
        assert_eq!(defs.units.units.len(), UNIT_FBIS.len(), "one unit per FBI");
        assert!(!defs.weapons.weapons.is_empty());
        assert!(!defs.explosions.explosions.is_empty());
        assert!(defs.move_classes.len() > 0);
    }

    #[test]
    fn file_names_are_lower_case() {
        for (name, _) in [UNIT_FBIS, WEAPON_TDFS, EXPLOSION_TDFS, FEATURE_TDFS]
            .into_iter()
            .flatten()
        {
            assert_eq!(*name, name.to_ascii_lowercase(), "{name}");
        }
    }

    /// Every feature the shipped maps place has a definition, and every
    /// `featureDead` leads to one.
    #[test]
    fn map_features_are_defined() {
        let features = &defs().features;
        for name in ["badblock", "unusablesector4"] {
            let mut def = features.get(name).unwrap_or_else(|| panic!("{name}"));
            while !def.feature_dead.is_empty() {
                def = features.get(&def.feature_dead).expect(&def.feature_dead);
            }
        }
    }
}

//! The identity of a CEG (Spring's "explosion generator", the
//! `gamedata/explosions/*.tdf` sections the engine replays as particle
//! systems, flames, spikes and ground flashes).
//!
//! Like [`UnitKind`], the set is closed: the sections are embedded at
//! compile time (`super::defs`, `build.rs`) and `net::compat` hashes the
//! embedded texts, so peers already have to agree on it. Carrying the
//! section name on the variant instead of hashing it per lookup is what
//! lets [`CegRegistry`](crate::units::weapon_fx::CegRegistry) be an array
//! index rather than a map, and lets an explosion event name its CEG in
//! a word.
//!
//! Names reach here from parsed data, always through [`CegKind::find`]:
//! a weapon's `explosiongenerator=custom:…` and `cegTag=`, a CEG
//! spawner's own `explosiongenerator`, a unit FBI's `[SFXTypes]` table.
//! Those files are data, so a name that is not a variant stays possible
//! at runtime — `find` returns `None` and the caller falls back to the
//! generic burst, exactly as the map lookup did.

use std::collections::HashMap;
use std::sync::LazyLock;
use strum::VariantArray;

/// Every `explosions/*.tdf` section Kernel Panic ships, in the order the
/// files sort in. The discriminant is the [`CegRegistry`] slot, so
/// renumbering is presentation-only: no `CegKind` crosses the wire or
/// enters the sim hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::IntoStaticStr, strum::VariantArray)]
pub enum CegKind {
    /// BugCannon's ballistic trail.
    #[strum(serialize = "corruption_BCtrail")]
    CorruptionBcTrail,
    /// The builder-arm's construction burst.
    #[strum(serialize = "corruption_builder")]
    CorruptionBuilder,
    /// Corruption-faction build-complete puff.
    #[strum(serialize = "corruption_build")]
    CorruptionBuild,
    #[strum(serialize = "corruption_burst")]
    CorruptionBurst,
    /// Obelisk's charge glow.
    #[strum(serialize = "corruption_charge")]
    CorruptionCharge,
    /// Hole's nano-emitter glow.
    #[strum(serialize = "corruption_const")]
    CorruptionConst,
    #[strum(serialize = "corruption_dust")]
    CorruptionDust,
    /// The infection cloud an infected unit leaves.
    #[strum(serialize = "corruption_infection")]
    CorruptionInfection,
    /// The smoke the infection cloud trails.
    #[strum(serialize = "corruption_infection_smoke")]
    CorruptionInfectionSmoke,
    #[strum(serialize = "corruption_shot1")]
    CorruptionShot1,
    #[strum(serialize = "corruption_stun")]
    CorruptionStun,
    #[strum(serialize = "corruption_trojan_rainbow")]
    CorruptionTrojanRainbow,
    /// Worm's WMD trail.
    #[strum(serialize = "corruption_WMDtrail")]
    CorruptionWmdTrail,
    /// The Worm's melee splash.
    #[strum(serialize = "corruption_worm_splash")]
    CorruptionWormSplash,
    #[strum(serialize = "corruption_worm_trail")]
    CorruptionWormTrail,
    /// Exploit's mobile-unit trail.
    #[strum(serialize = "experiment_trailM")]
    ExperimentTrailM,
    /// Exploit's stationary-unit trail.
    #[strum(serialize = "experiment_trailS")]
    ExperimentTrailS,
    /// The Socket↔buildee link beam.
    #[strum(serialize = "linkbeam")]
    Linkbeam,
    /// Mineclearer's sweep.
    #[strum(serialize = "mineclearer")]
    Mineclearer,
    /// MineLauncher's hexastar trail.
    #[strum(serialize = "minelauncher")]
    Minelauncher,
    /// A mine's detonation.
    #[strum(serialize = "mine")]
    Mine,
    /// Network-faction build-complete burst.
    #[strum(serialize = "network_build")]
    NetworkBuild,
    #[strum(serialize = "network_build1")]
    NetworkBuild1,
    /// Gateway's charge glow.
    #[strum(serialize = "network_charge")]
    NetworkCharge,
    /// What a dissolved unit comes apart into.
    #[strum(serialize = "network_dissolve")]
    NetworkDissolve,
    #[strum(serialize = "network_spawn")]
    NetworkSpawn,
    /// FlowMissile's impact.
    #[strum(serialize = "network_flowmissile")]
    NetworkFlowmissile,
    /// FlowMissile's starburst trail.
    #[strum(serialize = "network_flowtrail")]
    NetworkFlowtrail,
    #[strum(serialize = "network_shot")]
    NetworkShot,
    /// `[none]`: an authored *empty* CEG. Resolving to it is meaningful —
    /// it suppresses the generic burst (the GaussCannon authors it
    /// precisely so nothing is drawn). Distinct from a lookup that
    /// returns `None`, which keeps the fallback.
    #[strum(serialize = "none")]
    NoEffect,
    #[strum(serialize = "oldskool_big")]
    OldskoolBig,
    /// The build-laser's impact sparkle.
    #[strum(serialize = "oldskool_build")]
    OldskoolBuild,
    #[strum(serialize = "oldskool_death")]
    OldskoolDeath,
    #[strum(serialize = "oldskool_impact")]
    OldskoolImpact,
    /// The muzzle flash most Network units author in `[SFXTypes]`.
    #[strum(serialize = "oldskool_shot1")]
    OldskoolShot1,
    /// Bit's cyan arrowflare muzzle flash.
    #[strum(serialize = "oldskool_shot2")]
    OldskoolShot2,
    /// The stock impact burst: the fallback every weapon without an
    /// authored CEG still gets, so this is the most-used variant.
    #[strum(serialize = "oldskool")]
    Oldskool,
    #[strum(serialize = "oldskool_vbig")]
    OldskoolVBig,
    /// Rock-Paper-Scissors: Rock's spawn fountain.
    #[strum(serialize = "rock_fountain")]
    RockFountain,
    /// Rock-Paper-Scissors: Paper's spawn fountain.
    #[strum(serialize = "paper_fountain")]
    PaperFountain,
    /// Rock-Paper-Scissors: Scissors' spawn fountain.
    #[strum(serialize = "scissors_fountain")]
    ScissorsFountain,
    /// The NX Flag's denial-zone detonation, which fans out into
    /// [`Self::SystemNxFire`] on a 240-timer spawner.
    #[strum(serialize = "system_nx")]
    SystemNx,
    #[strum(serialize = "system_nx_fire")]
    SystemNxFire,
    /// The Terminal's SIGTERM bomb crater.
    #[strum(serialize = "system_sigterm")]
    SystemSigterm,
    #[strum(serialize = "system_sigterm_fire")]
    SystemSigtermFire,
    #[strum(serialize = "touhou_bullet")]
    TouhouBullet,
    #[strum(serialize = "touhou_spark")]
    TouhouSpark,
    #[strum(serialize = "touhou_buildFX")]
    TouhouBuildFx,
    #[strum(serialize = "touhou_incant")]
    TouhouIncant,
    /// Touhou bullets' bubble trail.
    #[strum(serialize = "touhou_bubbletrail")]
    TouhouBubbletrail,
}

/// All `CegKind` variants in declaration order, straight from
/// `strum::VariantArray` so a new variant needs no second edit.
pub const ALL_CEG_KINDS: &[CegKind] = CegKind::VARIANTS;

/// Number of `CegKind` variants: the length of the [`CegRegistry`] table.
pub const CEG_KIND_COUNT: usize = ALL_CEG_KINDS.len();

/// Section name → variant, keyed as-authored for the exact hit every
/// shipped reference takes. [`CegKind::find`] falls back to a scan.
static CEG_BY_NAME: LazyLock<HashMap<&'static str, CegKind>> = LazyLock::new(|| {
    ALL_CEG_KINDS
        .iter()
        .map(|&kind| (kind.name(), kind))
        .collect()
});

impl CegKind {
    /// Dense 0-based index in declaration order (== position in
    /// [`ALL_CEG_KINDS`]), for the [`CegRegistry`](crate::units::weapon_fx::CegRegistry)'s
    /// array lookup.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The TDF section name this variant was authored as.
    pub fn name(self) -> &'static str {
        self.into()
    }

    /// Resolve a name as it appears in a TDF or FBI: with or without the
    /// `custom:` prefix the engine's `explosiongenerator` values carry,
    /// in any case.
    ///
    /// `None` for an empty name and for a name that is not a CEG at all.
    /// No shipped reference takes that path — the 61 `[SFXTypes]`
    /// entries, 42 weapon `explosiongenerator=` values, 7 `cegTag=`
    /// values and 3 spawner targets all name a section that exists — so
    /// what reaches it is a name from a different vocabulary (a target
    /// category, a weapon name) or data that is not shipped. Callers
    /// treat `None` as "no scripted CEG" and fall back to the generic
    /// burst, which is what the map lookup did.
    ///
    /// The exact hit is what every shipped reference takes — the names
    /// in `weapons/*.tdf`, `explosions/*.tdf` and the FBIs all match
    /// their section's case — so the ignore-case scan only runs on a
    /// miss. Neither arm allocates.
    pub fn find(reference: &str) -> Option<Self> {
        let name = reference.trim();
        let name = name.strip_prefix("custom:").unwrap_or(name);
        if name.is_empty() {
            return None;
        }
        CEG_BY_NAME.get(name).copied().or_else(|| {
            ALL_CEG_KINDS
                .iter()
                .copied()
                .find(|&k| k.name().eq_ignore_ascii_case(name))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_round_trips_through_find() {
        for &kind in ALL_CEG_KINDS {
            assert_eq!(CegKind::find(kind.name()), Some(kind));
            assert_eq!(
                CegKind::find(&format!("custom:{}", kind.name())),
                Some(kind)
            );
        }
    }

    #[test]
    fn indices_are_dense_and_unique() {
        let mut seen = [false; CEG_KIND_COUNT];
        for &kind in ALL_CEG_KINDS {
            let i = kind.index();
            assert!(!seen[i], "{kind:?} shares a slot");
            seen[i] = true;
        }
    }

    #[test]
    fn find_ignores_case_and_accepts_the_prefix() {
        assert_eq!(CegKind::find("OLDSKOOL"), Some(CegKind::Oldskool));
        assert_eq!(
            CegKind::find("custom:Oldskool_ShOT2"),
            Some(CegKind::OldskoolShot2)
        );
    }

    #[test]
    fn the_authored_none_is_a_ceg_but_an_empty_name_is_not() {
        // `[none]` is a real (empty) section: resolving to it is how a
        // weapon suppresses the generic burst.
        assert_eq!(CegKind::find("custom:none"), Some(CegKind::NoEffect));
        assert_eq!(CegKind::find(""), None);
        assert_eq!(CegKind::find("custom:"), None);
    }

    #[test]
    fn names_that_are_not_cegs_resolve_to_nothing() {
        // None of the three is a CEG reference: `sawfunc` is commented
        // out in `retroweapons.tdf`, `VOID` is an `OnlyTargetCategory*`
        // value and `minekiller` a `[UNITINFO] Weapon1` value. They pin
        // the contract a caller relies on — a name from another
        // vocabulary resolves to `None` — which the shipped data itself
        // never exercises.
        for reference in ["custom:sawfunc", "VOID", "minekiller"] {
            assert_eq!(CegKind::find(reference), None, "{reference}");
        }
    }

    /// The other half of that claim: `find`'s `None` path is documented
    /// as unreachable for shipped data, so every CEG reference the
    /// embedded files make has to name a section. `sfx_types` is sparse
    /// (empty slots stand for missing indices), so empty entries are the
    /// absence of a reference rather than a bad one.
    #[test]
    fn every_shipped_ceg_reference_resolves() {
        use spring_tdf::EffectProperties;

        let defs = crate::units::content::defs::defs();
        let mut counts = [0; 3];

        for def in defs.weapons.weapons.values() {
            for reference in [&def.explosion_generator, &def.ceg_tag] {
                if !reference.is_empty() {
                    assert!(CegKind::find(reference).is_some(), "{reference}");
                    counts[0] += 1;
                }
            }
        }
        for def in defs.units.units.values() {
            for reference in &def.sfx_types {
                if !reference.is_empty() {
                    assert!(CegKind::find(reference).is_some(), "{reference}");
                    counts[1] += 1;
                }
            }
        }
        for def in defs.explosions.explosions.values() {
            for layer in &def.effects {
                if let EffectProperties::Spawner(props) = &layer.properties
                    && !props.explosion_generator.is_empty()
                {
                    let reference = &props.explosion_generator;
                    assert!(CegKind::find(reference).is_some(), "{reference}");
                    counts[2] += 1;
                }
            }
        }
        // Each kind has to have been walked: an empty loop would pass.
        assert!(
            counts[0] > 0 && counts[1] > 0 && counts[2] > 0,
            "{counts:?}"
        );
    }
}

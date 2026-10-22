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

use super::ceg_def::ExplosionDef;
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

    /// The definition this variant was generated into: the `[name]`
    /// section of an upstream `explosions/*.tdf`, as a `static` in
    /// [`super::ceg_defs`].
    ///
    /// Total by construction, and that is the point: `gen_ceg_defs`
    /// refuses to write a section no variant names, this match refuses to
    /// compile without one for every variant, and so the spawn path needs
    /// neither the `Option` a registry lookup returned nor the `bool` that
    /// distinguished "no such CEG" from "a CEG with nothing in it"
    /// (`docs/ceg-in-source-plan.md` §0, §3.5).
    ///
    /// Not yet read — the CEG runtime still evaluates the parsed TDFs —
    /// so the attribute goes with the registry, one commit later (§5.3).
    #[allow(dead_code)]
    pub const fn def(self) -> &'static ExplosionDef {
        match self {
            Self::CorruptionBcTrail => &super::ceg_defs::corruption_bctrail::CORRUPTION_BCTRAIL,
            Self::CorruptionBuilder => &super::ceg_defs::corruption_builder::CORRUPTION_BUILDER,
            Self::CorruptionBuild => &super::ceg_defs::corruption_build::CORRUPTION_BUILD,
            Self::CorruptionBurst => &super::ceg_defs::corruption_burst::CORRUPTION_BURST,
            Self::CorruptionCharge => &super::ceg_defs::corruption_charge::CORRUPTION_CHARGE,
            Self::CorruptionConst => &super::ceg_defs::corruption_const::CORRUPTION_CONST,
            Self::CorruptionDust => &super::ceg_defs::corruption_dust::CORRUPTION_DUST,
            Self::CorruptionInfection => {
                &super::ceg_defs::corruption_infection::CORRUPTION_INFECTION
            }
            Self::CorruptionInfectionSmoke => {
                &super::ceg_defs::corruption_infection::CORRUPTION_INFECTION_SMOKE
            }
            Self::CorruptionShot1 => &super::ceg_defs::corruption_shot1::CORRUPTION_SHOT1,
            Self::CorruptionStun => &super::ceg_defs::corruption_stun::CORRUPTION_STUN,
            Self::CorruptionTrojanRainbow => {
                &super::ceg_defs::corruption_trojan_rainbow::CORRUPTION_TROJAN_RAINBOW
            }
            Self::CorruptionWmdTrail => &super::ceg_defs::corruption_wmdtrail::CORRUPTION_WMDTRAIL,
            Self::CorruptionWormSplash => {
                &super::ceg_defs::corruption_worm_splash::CORRUPTION_WORM_SPLASH
            }
            Self::CorruptionWormTrail => {
                &super::ceg_defs::corruption_worm_trail::CORRUPTION_WORM_TRAIL
            }
            Self::ExperimentTrailM => &super::ceg_defs::experiment_trails::EXPERIMENT_TRAILM,
            Self::ExperimentTrailS => &super::ceg_defs::experiment_trails::EXPERIMENT_TRAILS,
            Self::Linkbeam => &super::ceg_defs::linkbeam::LINKBEAM,
            Self::Mineclearer => &super::ceg_defs::mineclearer::MINECLEARER,
            Self::Minelauncher => &super::ceg_defs::minelauncher::MINELAUNCHER,
            Self::Mine => &super::ceg_defs::mine::MINE,
            Self::NetworkBuild => &super::ceg_defs::network_build::NETWORK_BUILD,
            Self::NetworkBuild1 => &super::ceg_defs::network_build::NETWORK_BUILD1,
            Self::NetworkCharge => &super::ceg_defs::network_charge::NETWORK_CHARGE,
            Self::NetworkDissolve => &super::ceg_defs::network_dissolve::NETWORK_DISSOLVE,
            Self::NetworkSpawn => &super::ceg_defs::network_dissolve::NETWORK_SPAWN,
            Self::NetworkFlowmissile => &super::ceg_defs::network_flowmissile::NETWORK_FLOWMISSILE,
            Self::NetworkFlowtrail => &super::ceg_defs::network_flowtrail::NETWORK_FLOWTRAIL,
            Self::NetworkShot => &super::ceg_defs::network_shot::NETWORK_SHOT,
            Self::NoEffect => &super::ceg_defs::none::NONE,
            Self::OldskoolBig => &super::ceg_defs::oldskool_big::OLDSKOOL_BIG,
            Self::OldskoolBuild => &super::ceg_defs::oldskool_build::OLDSKOOL_BUILD,
            Self::OldskoolDeath => &super::ceg_defs::oldskool_death::OLDSKOOL_DEATH,
            Self::OldskoolImpact => &super::ceg_defs::oldskool_impact::OLDSKOOL_IMPACT,
            Self::OldskoolShot1 => &super::ceg_defs::oldskool_shot1::OLDSKOOL_SHOT1,
            Self::OldskoolShot2 => &super::ceg_defs::oldskool_shot2::OLDSKOOL_SHOT2,
            Self::Oldskool => &super::ceg_defs::oldskool::OLDSKOOL,
            Self::OldskoolVBig => &super::ceg_defs::oldskool_vbig::OLDSKOOL_VBIG,
            Self::RockFountain => &super::ceg_defs::rps_fountains::ROCK_FOUNTAIN,
            Self::PaperFountain => &super::ceg_defs::rps_fountains::PAPER_FOUNTAIN,
            Self::ScissorsFountain => &super::ceg_defs::rps_fountains::SCISSORS_FOUNTAIN,
            Self::SystemNx => &super::ceg_defs::system_nx::SYSTEM_NX,
            Self::SystemNxFire => &super::ceg_defs::system_nx::SYSTEM_NX_FIRE,
            Self::SystemSigterm => &super::ceg_defs::system_sigterm::SYSTEM_SIGTERM,
            Self::SystemSigtermFire => &super::ceg_defs::system_sigterm::SYSTEM_SIGTERM_FIRE,
            Self::TouhouBullet => &super::ceg_defs::touhou::TOUHOU_BULLET,
            Self::TouhouSpark => &super::ceg_defs::touhou::TOUHOU_SPARK,
            Self::TouhouBuildFx => &super::ceg_defs::touhou::TOUHOU_BUILDFX,
            Self::TouhouIncant => &super::ceg_defs::touhou::TOUHOU_INCANT,
            Self::TouhouBubbletrail => &super::ceg_defs::touhou::TOUHOU_BUBBLETRAIL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ceg_def as source;
    use super::*;
    use spring_tdf as parsed;

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

    /// The generated statics say what the TDFs say.
    ///
    /// This, not the generator, is the gate on `ceg_defs`. A generator
    /// that put `particle_speed` in `particle_size` would happily
    /// regenerate the same wrong file forever, so the comparison runs
    /// the other way: each `static` is turned back into the owned tree
    /// `spring_tdf` parsed from the embedded files, and the two trees
    /// have to be equal. Nothing in here reads a generated file or calls
    /// the generator.
    ///
    /// The parsed side is normalised first, four times, each one a
    /// fidelity decision of §3 rather than a different def:
    ///
    /// - a texture name is compared as the alias it spells, lowercased,
    ///   with `none` and `""` both standing for "no texture" (§3.3);
    /// - a value program whose ops are all adds is folded to their sum,
    ///   which is what [`source::CegExpr::Lit`] says, and an empty
    ///   program is the `0.0` the accumulator starts at;
    /// - a `[groundflash]` with `ttl = 0` is no flash at all (§3.4);
    /// - a spawner's target is compared as a lowercased name, because
    ///   that is how the runtime resolves it (`CegKind::find`).
    ///
    /// Everything else — including which `[properties]` block became
    /// which variant, and so the layer's `class` — is compared as it
    /// stands.
    #[test]
    fn every_source_def_matches_the_parsed_tdf() {
        let tdfs = &crate::units::content::defs::defs().explosions;
        for &kind in ALL_CEG_KINDS {
            let authored = tdfs
                .explosions
                .get(kind.name())
                .unwrap_or_else(|| panic!("[{}] is not a TDF section", kind.name()));
            assert_eq!(authored.id, kind.name(), "the map key is the section name");
            assert_eq!(
                from_source(kind.def(), kind.name()),
                normalise(authored),
                "[{}] and its static disagree",
                kind.name()
            );
        }
    }

    /// The `static` back into the parsed shape, so both sides of the
    /// comparison are one type. The two liberties are the documented
    /// ones: a texture becomes the alias it names, and a spawner target
    /// the lowercased name `CegKind::find` would resolve.
    fn from_source(def: &'static source::ExplosionDef, name: &str) -> parsed::ExplosionDef {
        parsed::ExplosionDef {
            id: name.to_string(),
            effects: def.effects.iter().map(layer_from_source).collect(),
            ground_flash: def.ground_flash.as_ref().map(|flash| parsed::GroundFlash {
                flash_size: flash.flash_size,
                flash_alpha: flash.flash_alpha,
                circle_growth: flash.circle_growth,
                circle_alpha: flash.circle_alpha,
                ttl: flash.ttl,
                color: flash.color,
            }),
        }
    }

    fn layer_from_source(layer: &source::EffectLayer) -> parsed::EffectLayer {
        let (class, properties) = match &layer.properties {
            source::EffectProperties::Particle(p) => (
                parsed::EffectClass::SimpleParticleSystem,
                parsed::EffectProperties::Particle(parsed::ParticleProperties {
                    texture: alias(p.texture),
                    color_map: parsed::ColorMap {
                        stops: p.color_map.0.to_vec(),
                    },
                    num_particles: value_from_source(&p.num_particles),
                    particle_life: value_from_source(&p.particle_life),
                    particle_life_spread: value_from_source(&p.particle_life_spread),
                    particle_speed: value_from_source(&p.particle_speed),
                    particle_speed_spread: value_from_source(&p.particle_speed_spread),
                    particle_size: value_from_source(&p.particle_size),
                    particle_size_spread: value_from_source(&p.particle_size_spread),
                    size_growth: value_from_source(&p.size_growth),
                    size_mod: value_from_source(&p.size_mod),
                    emit_rot: value_from_source(&p.emit_rot),
                    emit_rot_spread: value_from_source(&p.emit_rot_spread),
                    airdrag: value_from_source(&p.airdrag),
                    gravity: vector_from_source(&p.gravity),
                    pos: vector_from_source(&p.pos),
                    emit_vector: emit_vector_from_source(&p.emit_vector),
                    directional: p.directional,
                    always_visible: p.always_visible,
                }),
            ),
            source::EffectProperties::Flame(p) => (
                parsed::EffectClass::BitmapMuzzleFlame,
                parsed::EffectProperties::Flame(parsed::FlameProperties {
                    side_texture: alias(p.side_texture),
                    front_texture: alias(p.front_texture),
                    color_map: parsed::ColorMap {
                        stops: p.color_map.0.to_vec(),
                    },
                    size: value_from_source(&p.size),
                    length: value_from_source(&p.length),
                    size_growth: value_from_source(&p.size_growth),
                    ttl: value_from_source(&p.ttl),
                    front_offset: value_from_source(&p.front_offset),
                    speed: value_from_source(&p.speed),
                    dir: vector_from_source(&p.dir),
                    pos: vector_from_source(&p.pos),
                    always_visible: p.always_visible,
                }),
            ),
            source::EffectProperties::Spawner(p) => (
                parsed::EffectClass::ExpGenSpawner,
                parsed::EffectProperties::Spawner(parsed::SpawnerProperties {
                    explosion_generator: p.target.name().to_ascii_lowercase(),
                    delay: value_from_source(&p.delay),
                    damage: value_from_source(&p.damage),
                    dir: vector_from_source(&p.dir),
                    pos: vector_from_source(&p.pos),
                    speed: vector_from_source(&p.speed),
                    always_visible: p.always_visible,
                }),
            ),
            source::EffectProperties::Spike(p) => (
                parsed::EffectClass::ExploSpike,
                parsed::EffectProperties::Spike(parsed::SpikeProperties {
                    length: value_from_source(&p.length),
                    width: value_from_source(&p.width),
                    alpha: value_from_source(&p.alpha),
                    alpha_decay: value_from_source(&p.alpha_decay),
                    dir: vector_from_source(&p.dir),
                    pos: vector_from_source(&p.pos),
                    color: p.color.as_ref().map(vector_from_source),
                }),
            ),
        };
        parsed::EffectLayer {
            name: layer.name.to_string(),
            class,
            air: layer.air,
            ground: layer.ground,
            water: layer.water,
            count: layer.count,
            properties,
        }
    }

    fn value_from_source(expr: &source::CegExpr) -> parsed::CegExpr {
        parsed::CegExpr(match expr {
            source::CegExpr::Lit(v) => vec![parsed::CegOp::Add(*v)],
            source::CegExpr::Ops(ops) => ops.iter().map(op_from_source).collect(),
        })
    }

    fn op_from_source(op: &source::CegOp) -> parsed::CegOp {
        match *op {
            source::CegOp::Add(v) => parsed::CegOp::Add(v),
            source::CegOp::Rand(v) => parsed::CegOp::Rand(v),
            source::CegOp::Index(v) => parsed::CegOp::Index(v),
            source::CegOp::Damage(v) => parsed::CegOp::Damage(v),
            source::CegOp::Yank(n) => parsed::CegOp::Other('y', f32::from(n)),
            source::CegOp::MulBuf(n) => parsed::CegOp::Other('x', f32::from(n)),
        }
    }

    fn vector_from_source(vec: &source::CegVec3) -> parsed::CegVec3 {
        parsed::CegVec3 {
            x: value_from_source(&vec.x),
            y: value_from_source(&vec.y),
            z: value_from_source(&vec.z),
        }
    }

    fn emit_vector_from_source(vector: &source::EmitVector) -> parsed::EmitVector {
        match vector {
            source::EmitVector::Direction => parsed::EmitVector::Direction,
            source::EmitVector::Literal(vec) => {
                parsed::EmitVector::Literal(vector_from_source(vec))
            }
        }
    }

    fn alias(texture: Option<source::CegTexture>) -> String {
        match texture {
            None => String::new(),
            Some(texture) => texture.name().to_string(),
        }
    }

    /// The parsed def with the four documented normalisations applied.
    fn normalise(def: &parsed::ExplosionDef) -> parsed::ExplosionDef {
        parsed::ExplosionDef {
            id: def.id.clone(),
            effects: def.effects.iter().map(normalise_layer).collect(),
            ground_flash: def.ground_flash.clone().filter(|flash| flash.ttl > 0.0),
        }
    }

    fn normalise_layer(layer: &parsed::EffectLayer) -> parsed::EffectLayer {
        let mut layer = layer.clone();
        match &mut layer.properties {
            parsed::EffectProperties::Particle(p) => {
                p.texture = texture_name(&p.texture);
                for value in [
                    &mut p.num_particles,
                    &mut p.particle_life,
                    &mut p.particle_life_spread,
                    &mut p.particle_speed,
                    &mut p.particle_speed_spread,
                    &mut p.particle_size,
                    &mut p.particle_size_spread,
                    &mut p.size_growth,
                    &mut p.size_mod,
                    &mut p.emit_rot,
                    &mut p.emit_rot_spread,
                    &mut p.airdrag,
                ] {
                    normalise_value(value);
                }
                normalise_vector(&mut p.gravity);
                normalise_vector(&mut p.pos);
                if let parsed::EmitVector::Literal(vec) = &mut p.emit_vector {
                    normalise_vector(vec);
                }
            }
            parsed::EffectProperties::Flame(p) => {
                p.side_texture = texture_name(&p.side_texture);
                p.front_texture = texture_name(&p.front_texture);
                for value in [
                    &mut p.size,
                    &mut p.length,
                    &mut p.size_growth,
                    &mut p.ttl,
                    &mut p.front_offset,
                    &mut p.speed,
                ] {
                    normalise_value(value);
                }
                normalise_vector(&mut p.dir);
                normalise_vector(&mut p.pos);
            }
            parsed::EffectProperties::Spawner(p) => {
                p.explosion_generator = p.explosion_generator.trim().to_ascii_lowercase();
                normalise_value(&mut p.delay);
                normalise_value(&mut p.damage);
                normalise_vector(&mut p.dir);
                normalise_vector(&mut p.pos);
                normalise_vector(&mut p.speed);
            }
            parsed::EffectProperties::Spike(p) => {
                for value in [
                    &mut p.length,
                    &mut p.width,
                    &mut p.alpha,
                    &mut p.alpha_decay,
                ] {
                    normalise_value(value);
                }
                normalise_vector(&mut p.dir);
                normalise_vector(&mut p.pos);
                if let Some(vec) = &mut p.color {
                    normalise_vector(vec);
                }
            }
            parsed::EffectProperties::Raw(entries) => panic!(
                "{} is a class the source has no variant for ({} raw keys): \
                 `gen_ceg_defs` would refuse to write it too",
                layer.name,
                entries.len()
            ),
        }
        layer
    }

    fn normalise_vector(vec: &mut parsed::CegVec3) {
        normalise_value(&mut vec.x);
        normalise_value(&mut vec.y);
        normalise_value(&mut vec.z);
    }

    /// A run of adds is the sum it computes, and nothing is `0.0`.
    ///
    /// The fold adds in file order, which is the order `eval` walks the
    /// ops in, so the sum is the same number rather than merely an
    /// equal-in-spirit one.
    fn normalise_value(expr: &mut parsed::CegExpr) {
        let mut sum = 0.0;
        for op in &expr.0 {
            match op {
                parsed::CegOp::Add(v) => sum += v,
                _ => return,
            }
        }
        expr.0 = vec![parsed::CegOp::Add(sum)];
    }

    /// The alias a TDF texture name spells, in the form
    /// [`from_source`] writes one: `""` for both `none` and no name.
    fn texture_name(authored: &str) -> String {
        let name = authored.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("none") {
            String::new()
        } else {
            name.to_ascii_lowercase()
        }
    }
}

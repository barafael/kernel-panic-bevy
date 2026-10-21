//! Weapon registry loaded from the upstream TDF files (via the unit bundle).
//!
//! At startup the bundle's merged `weapons/*.tdf` definitions become a
//! single [`WeaponRegistry`] resource. The combat system resolves weapon
//! stats through this registry instead of hardcoded values.
//!
//! Weapons are addressed by [`WeaponKind`], one variant per TDF section.
//! The set is closed: the sections are embedded at compile time
//! (`super::defs`, `build.rs`) and `net::compat` hashes the embedded
//! texts, so peers already have to agree on it. That makes the identity a
//! `Copy` byte that is impossible to invent, the registry an array index
//! rather than a `HashMap`, and a shot's weapon a field compare rather
//! than a hashed name — which is what the combat hot path spends its
//! lookups on.
//!
//! Names still reach it from parsed data, always through
//! [`WeaponKind::find`]: an FBI's `Weapon1` / `Weapon2` / `ExplodeAs`, and
//! the shield and script paths that name a weapon directly. Those files
//! are data, so a name that is not a variant stays possible at runtime —
//! `find` returns `None` and the unit goes unarmed, as the map lookup did.

use bevy::prelude::*;
use spring_tdf::{WeaponDef, WeaponDefs};
use std::collections::HashMap;
use std::sync::LazyLock;
use strum::VariantArray;

use super::cegs::CegKind;
use super::definitions::{ALL_UNIT_KINDS, UnitKind};
use crate::sim::{DetFloat, frames_to_secs};

/// Every `weapons/*.tdf` section Kernel Panic ships. The discriminant is
/// the [`WeaponRegistry`] slot, so it is never sent anywhere: weapons are
/// sim-internal, and the netcode keys state by [`NetId`](crate::net::NetId).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    strum::IntoStaticStr,
    strum::VariantArray,
)]
pub enum WeaponKind {
    // Declared first so its discriminant is 0. That is the sentinel a
    // zeroed component holds and the value the visual layer uses for
    // builder rays; `production_system` and `start_construction` drive a
    // builder beam from it without going through TDF damage resolution.
    // It is also a real section, so a loaded registry holds the authored
    // def rather than a stub — as it did when slot 0 was reserved.
    /// The builder ray. 16 units author it as `Weapon1` purely to keep
    /// Spring from reordering their shield slot.
    #[strum(serialize = "BuildLaser")]
    BuildLaser,
    /// Bug's and BugOld's cannon.
    #[strum(serialize = "BugShot")]
    BugShot,
    /// Upstream's Bug bomb, which no Kernel Panic FBI fires.
    #[strum(serialize = "BugBomb")]
    BugBomb,
    /// DOS's beam.
    #[strum(serialize = "DOS_Beam")]
    DosBeam,
    /// The Worm's melee bite. Its `Weapon2` is [`Self::Wormsplash`].
    #[strum(serialize = "Wormbite")]
    Wormbite,
    /// The shockwave a Worm bite detonates (`worm.bos` `emit-sfx 4097`).
    #[strum(serialize = "Wormsplash")]
    Wormsplash,
    /// Exploit's cannon.
    #[strum(serialize = "BugCannon")]
    BugCannon,
    /// Upstream's decoy BugCannon, unfired here.
    #[strum(serialize = "FakeBugCannon")]
    FakeBugCannon,
    /// Virus's infection beam.
    #[strum(serialize = "VirusBeam")]
    VirusBeam,
    /// What a Virus explodes into, which infects.
    #[strum(serialize = "VirusDeath")]
    VirusDeath,
    /// Obelisk's Infection gas, and the CEG-less command cast.
    #[strum(serialize = "Infection")]
    Infection,
    /// WormOld's disintegrator.
    #[strum(serialize = "Swallow_DISINTEGRATOR")]
    SwallowDisintegrator,
    /// WormOld's secondary, fired by its script.
    #[strum(serialize = "SwallowDamage")]
    SwallowDamage,
    /// Trojan's build-lightning.
    #[strum(serialize = "BuildLightning")]
    BuildLightning,
    /// ExpArty's stationary bombardment.
    #[strum(serialize = "ExpStationary")]
    ExpStationary,
    /// ExpMobile's shell.
    #[strum(serialize = "ExpMobile")]
    ExpMobile,
    /// Packet's beam.
    #[strum(serialize = "PacketBeam")]
    PacketBeam,
    /// Gateway's beam.
    #[strum(serialize = "GatewayBeam")]
    GatewayBeam,
    /// Connection's arc, drawn by `network_arceffect.lua` instead of any
    /// engine visual — see [`WeaponRegistry::is_gauss_cannon`].
    #[strum(serialize = "GaussCannon")]
    GaussCannon,
    /// Flow's missile.
    #[strum(serialize = "FlowMissile")]
    FlowMissile,
    /// A homebase's shield emitter (`Weapon2`).
    #[strum(serialize = "homebaseshieldgood")]
    HomebaseShieldGood,
    /// The drained half of that shield pair, which no FBI fires.
    #[strum(serialize = "homebaseshieldbad")]
    HomebaseShieldBad,
    /// A small structure's shield emitter (`Weapon2`).
    #[strum(serialize = "minifacshieldgood")]
    MinifacShieldGood,
    /// The drained half of the minifac shield pair.
    #[strum(serialize = "minifacshieldbad")]
    MinifacShieldBad,
    /// Bit's hitscan laser.
    #[strum(serialize = "Line")]
    Line,
    /// Byte's Mega Beam.
    #[strum(serialize = "MegaBeam")]
    MegaBeam,
    /// Pointer's Geometric weapon.
    #[strum(serialize = "Geometric")]
    Geometric,
    /// Logic Bomb's own detonation, and Counter's `ExplodeAs`.
    #[strum(serialize = "logic_bomb")]
    LogicBomb,
    /// Logic Bomb's `Weapon1`: the bomb that ends the game on a kernel.
    #[strum(serialize = "end_game_logic_bomb")]
    EndGameLogicBomb,
    /// Counter's and MineBlaster's mine sweep, and MineBlaster's
    /// `ExplodeAs`. Counter's FBI spells it lowercase.
    #[strum(serialize = "Minekiller")]
    Minekiller,
    /// The small-unit death blast: 15 units' `ExplodeAs`.
    #[strum(serialize = "RetroDeath")]
    RetroDeath,
    /// Upstream's Pointer-sized death blast, unreferenced here.
    #[strum(serialize = "RetroDeath_pointer")]
    RetroDeathPointer,
    /// The factory-sized death blast.
    #[strum(serialize = "RetroDeathBig")]
    RetroDeathBig,
    /// The homebase-sized death blast.
    #[strum(serialize = "RetroDeathVBig")]
    RetroDeathVBig,
    /// Upstream's Assembler-sized death blast, unreferenced here.
    #[strum(serialize = "RetroDeathBig_assembler")]
    RetroDeathBigAssembler,
    /// Signal's `ExplodeAs`: the blast with no CEG behind it.
    #[strum(serialize = "BuildLaserNoEffect")]
    BuildLaserNoEffect,
    /// Signal's SIGTERM bomb, and the area-denial zone it leaves.
    #[strum(serialize = "SigTerm")]
    SigTerm,
    /// Pointer's NX Flag (`Weapon2`), the denial-zone cast.
    #[strum(serialize = "nx")]
    Nx,
    /// Byte's mine tubes (`Weapon2`).
    #[strum(serialize = "MineLauncher")]
    MineLauncher,
    /// Rock's melee weapon, and Rock-Paper-Scissors' first hand.
    #[strum(serialize = "Rock")]
    Rock,
    /// Paper's melee weapon.
    #[strum(serialize = "Paper")]
    Paper,
    /// Scissors' melee weapon.
    #[strum(serialize = "Scissors")]
    Scissors,
    /// Fairy's bullet, in both of its slots.
    #[strum(serialize = "FairyBullet")]
    FairyBullet,
    /// Reimu's spread shot.
    #[strum(serialize = "ScatterBullet")]
    ScatterBullet,
    /// Marisa's Spark.
    #[strum(serialize = "Spark")]
    Spark,
    /// Upstream's Marisa orb, unfired here.
    #[strum(serialize = "Orb")]
    Orb,
}

/// All `WeaponKind` variants in declaration order, straight from
/// `strum::VariantArray` so a new section needs no second edit.
pub const ALL_WEAPON_KINDS: &[WeaponKind] = WeaponKind::VARIANTS;

/// Number of `WeaponKind` variants: the length of every
/// [`WeaponRegistry`] table.
pub const WEAPON_KIND_COUNT: usize = ALL_WEAPON_KINDS.len();

/// Section name → variant, keyed as-authored for the exact hit.
/// [`WeaponKind::find`] falls back to a scan.
static WEAPON_BY_NAME: LazyLock<HashMap<&'static str, WeaponKind>> = LazyLock::new(|| {
    ALL_WEAPON_KINDS
        .iter()
        .map(|&kind| (kind.name(), kind))
        .collect()
});

impl WeaponKind {
    /// Dense 0-based index in declaration order (== position in
    /// [`ALL_WEAPON_KINDS`]), for the [`WeaponRegistry`]'s array lookups.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The TDF section name this variant was authored as.
    pub fn name(self) -> &'static str {
        self.into()
    }

    /// Resolve a name as an FBI or a script writes it, in any case — the
    /// contract `WeaponDefs::get` never had but `WeaponRegistry::intern`
    /// did, and the one the shipped data needs: `counter.fbi` and friends
    /// say `minekiller`, `rock.fbi` says `rock`, `marisa.fbi` says `spark`,
    /// none of which match their section's case.
    ///
    /// `None` for an empty name (an unarmed unit's `Weapon1`) and for a
    /// name with no section behind it. The exact hit covers every other
    /// reference; the scan runs for those three and for nothing else in
    /// the game. Neither arm allocates, which is the point: `intern` built
    /// a lowercased `String` per call.
    pub fn find(reference: &str) -> Option<Self> {
        let name = reference.trim();
        if name.is_empty() {
            return None;
        }
        WEAPON_BY_NAME.get(name).copied().or_else(|| {
            ALL_WEAPON_KINDS
                .iter()
                .copied()
                .find(|&k| k.name().eq_ignore_ascii_case(name))
        })
    }

    /// Per-weapon infection window in seconds, for the weapons that
    /// infect. Mirrors upstream `LuaRules/Gadgets/infection.lua`, which
    /// expresses the window in sim frames at 30 fps.
    pub fn infection_duration(self) -> Option<f32> {
        let frames = match self {
            Self::VirusBeam => 90.0,
            Self::VirusDeath => 180.0,
            Self::Wormsplash => 200.0,
            Self::Infection => 30.0,
            _ => return None,
        };
        Some(frames_to_secs(frames))
    }
}

/// A unit kind's FBI `ExplodeAs` resolved for `death_system`: the
/// self-hit weapon plus the visual explosion derived from its TDF.
#[derive(Clone, Debug)]
pub struct DeathBlast {
    pub weapon: WeaponKind,
    /// Visual ring radius: `area_of_effect`, at least 24 elmos.
    pub radius: f32,
    /// The weapon's `rgb_color` when it configures one; `None` → the
    /// dying unit's faction colour.
    pub rgb: Option<[f32; 3]>,
    /// The weapon's `explosiongenerator` as a [`CegKind`], resolved once
    /// when the blast was bound. `None` when the weapon authors no CEG
    /// or names one the TDFs do not define.
    pub ceg: Option<CegKind>,
}

impl DeathBlast {
    /// Resolve `kind`'s `ExplodeAs` against `weapons` the way
    /// `death_system` used to per death: absent, empty, or unregistered
    /// names yield `None`.
    pub fn resolve(
        kind: UnitKind,
        units: &super::unit_registry::UnitRegistry,
        weapons: &WeaponRegistry,
    ) -> Option<Self> {
        let name = units.def(kind).map(|d| d.explode_as.as_str())?;
        let weapon = weapons.find(name)?;
        let def = weapons.by_kind(weapon);
        Some(Self {
            weapon,
            radius: def.area_of_effect.det_max(24.0),
            rgb: Some(def.rgb_color).filter(|c| c[0] + c[1] + c[2] > 0.01),
            ceg: weapons.ceg(weapon),
        })
    }
}

/// Every parsed weapon, in a table indexed by [`WeaponKind`] rather than
/// keyed by section name: each read is an array index, and the
/// name→variant resolution happens once, at load.
///
/// Parsing stays in [`spring_tdf::WeaponDefs`]; this resolves its sections
/// into the dense table. A slot is `None` when the section is absent —
/// which is the state a registry built from a partial `WeaponDefs` (a
/// test's) leaves every weapon in.
#[derive(Resource)]
pub struct WeaponRegistry {
    /// Indexed by [`WeaponKind::index`].
    defs: Box<[Option<WeaponDef>]>,
    /// Parallel `defs[i]` → the def's `explosion_generator` resolved, so
    /// the per-shot / per-death explosion events carry a word rather than
    /// a name to look up (`DeathBlast`, `ExplosionEvent`).
    ceg_kinds: Box<[Option<CegKind>]>,
    /// Parallel `defs[i]` → `ceg_tag`, the CEG a traveling weapon replays
    /// *every frame* of its flight. Resolved at insert for the same
    /// reason, and more so: this one is read per projectile per tick.
    ceg_tags: Box<[Option<CegKind>]>,
    /// Per-`UnitKind` (indexed by discriminant) `ExplodeAs` resolution,
    /// filled by [`Self::bind_units`]; `None` until then.
    death_blasts: Option<Box<[Option<DeathBlast>]>>,
}

impl Default for WeaponRegistry {
    /// Nothing but the [`WeaponKind::BuildLaser`] sentinel, whose def is
    /// the engine default: the build-laser path special-cases the kind
    /// before reading a field, and a registry that never loaded TDFs still
    /// has to answer for it.
    fn default() -> Self {
        let mut registry = Self {
            defs: (0..WEAPON_KIND_COUNT).map(|_| None).collect(),
            ceg_kinds: (0..WEAPON_KIND_COUNT).map(|_| None).collect(),
            ceg_tags: (0..WEAPON_KIND_COUNT).map(|_| None).collect(),
            death_blasts: None,
        };
        registry.insert(WeaponKind::BuildLaser, WeaponDef::default());
        registry
    }
}

impl WeaponRegistry {
    /// The registry of the embedded `weapons/*.tdf` (`defs`).
    pub fn load() -> Self {
        let registry = Self::from_defs(&super::defs::defs().weapons);
        info!(
            "Weapon registry: {} definitions total",
            registry.defs.iter().flatten().count(),
        );
        registry.validate_bindings();
        registry
    }

    /// Build from parsed weapon TDFs.
    pub fn from_defs(defs: &WeaponDefs) -> Self {
        let mut registry = Self::default();
        for (name, def) in &defs.weapons {
            match WeaponKind::find(name) {
                Some(kind) => registry.insert(kind, def.clone()),
                // A section no variant names: a unit that says it in
                // `Weapon1` would go unarmed, and
                // `validate_unit_weapon_bindings` is what says so.
                None => warn!("weapon section '{name}' has no WeaponKind variant"),
            }
        }
        registry
    }

    fn insert(&mut self, kind: WeaponKind, def: WeaponDef) {
        let i = kind.index();
        // Resolve both CEG references from `def` before it moves, so the
        // tables cannot disagree about where a name came from.
        self.ceg_kinds[i] = CegKind::find(&def.explosion_generator);
        self.ceg_tags[i] = CegKind::find(&def.ceg_tag);
        // A repeated section name (which `WeaponDefs`' map has already
        // collapsed, but not across case) keeps its slot: the last write
        // wins, as `extend()` overwriting on key clash used to.
        self.defs[i] = Some(def);
    }

    /// Warn for every [`WeaponKind`] the parsed TDFs leave undefined, the
    /// way `UnitRegistry` reports a `UnitKind` with no FBI.
    fn validate_bindings(&self) {
        for &kind in ALL_WEAPON_KINDS {
            if self.defs[kind.index()].is_none() {
                warn!(
                    "weapon '{}' (variant {kind:?}) not found in weapons/*.tdf",
                    kind.name(),
                );
            }
        }
    }

    /// Resolve every unit kind's `ExplodeAs` once (a startup system runs
    /// this after both registries are loaded) so `death_system` reads a
    /// table slot per death instead of two name lookups and a CEG string
    /// clone. Tests that skip this keep the per-death resolution.
    pub fn bind_units(&mut self, units: &super::unit_registry::UnitRegistry) {
        self.death_blasts = Some(
            ALL_UNIT_KINDS
                .iter()
                .map(|&kind| DeathBlast::resolve(kind, units, self))
                .collect(),
        );
    }

    /// `kind`'s cached [`DeathBlast`] — outer `None` when
    /// [`Self::bind_units`] has not run, inner `None` when the kind has
    /// no (registered) `ExplodeAs`.
    pub fn death_blast(&self, kind: UnitKind) -> Option<Option<&DeathBlast>> {
        self.death_blasts
            .as_ref()
            .and_then(|blasts| blasts.get(kind.index()))
            .map(Option::as_ref)
    }

    /// The def behind `kind`, `None` when its section is absent.
    pub fn get(&self, kind: WeaponKind) -> Option<&WeaponDef> {
        self.defs[kind.index()].as_ref()
    }

    /// Resolve a name as an FBI or a script writes it to the weapon it
    /// names — `None` for an empty name, one with no section behind it,
    /// or a variant these TDFs leave undefined. The entry point for every
    /// data-driven reference (`Weapon1` / `Weapon2` / `ExplodeAs`, the
    /// script paths), and the `intern` it replaces.
    pub fn find(&self, name: &str) -> Option<WeaponKind> {
        WeaponKind::find(name).filter(|&kind| self.is_present(kind))
    }

    /// Whether the parsed TDFs define `kind` at all.
    pub fn is_present(&self, kind: WeaponKind) -> bool {
        self.defs[kind.index()].is_some()
    }

    /// The def behind `kind`.
    ///
    /// Infallible, because every `WeaponKind` a live shot can carry came
    /// from a section: a miss means a `WeaponKind` was built out of
    /// nothing, or a registry was used before it loaded. `get` is the
    /// lookup for the paths where absence is a real answer.
    pub fn by_kind(&self, kind: WeaponKind) -> &WeaponDef {
        self.defs[kind.index()].as_ref().unwrap_or_else(|| {
            panic!(
                "weapon '{}' ({kind:?}) is not in the registry; use WeaponRegistry::get \
                 where absence is expected",
                kind.name(),
            )
        })
    }

    /// The CEG behind `kind`'s `explosion_generator` — the blast a shot or
    /// a death replays. `None` means no scripted CEG, so the spawner falls
    /// back to its generic burst.
    pub fn ceg(&self, kind: WeaponKind) -> Option<CegKind> {
        self.ceg_kinds[kind.index()]
    }

    /// The CEG behind `kind`'s `ceg_tag` — what a traveling weapon replays
    /// along its flight path. `None` when the weapon authors no tag.
    pub fn ceg_tag(&self, kind: WeaponKind) -> Option<CegKind> {
        self.ceg_tags[kind.index()]
    }

    /// True for the Connection's `GaussCannon`, drawn as the gadget's
    /// lightning arc instead of any engine visual.
    pub fn is_gauss_cannon(&self, kind: WeaponKind) -> bool {
        kind == WeaponKind::GaussCannon && self.is_present(kind)
    }

    /// True for the Worm's `Wormbite` melee weapon.
    pub fn is_worm_bite(&self, kind: WeaponKind) -> bool {
        kind == WeaponKind::Wormbite && self.is_present(kind)
    }

    /// Test-only: register an ad-hoc def for a weapon. Production paths go
    /// through `load()`.
    #[cfg(test)]
    pub fn insert_for_test(&mut self, kind: WeaponKind, def: WeaponDef) -> WeaponKind {
        self.insert(kind, def);
        kind
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
            let name = unit_registry.weapon(kind);
            if name.is_empty() {
                continue;
            }
            match self.find(name) {
                None => {
                    warn!(
                        "Unit '{:?}' references weapon '{}' which is not in the TDF registry",
                        kind, name,
                    );
                }
                Some(weapon) => {
                    let def = self.by_kind(weapon);
                    info!(
                        "  {:?} → {} (reload={}s, burst={}, burstrate={}s, range={})",
                        kind, name, def.reload_time, def.burst, def.burst_rate, def.range,
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
    fn build_laser_is_the_zero_sentinel_and_is_always_present() {
        assert_eq!(WeaponKind::BuildLaser.index(), 0);
        let registry = WeaponRegistry::default();
        assert!(registry.is_present(WeaponKind::BuildLaser));
        // The rest of the table is empty until a TDF fills it.
        assert!(!registry.is_present(WeaponKind::BugShot));
        assert!(registry.get(WeaponKind::BugShot).is_none());
    }

    #[test]
    fn find_is_case_insensitive_like_the_intern_it_replaces() {
        assert_eq!(WeaponKind::find("BugCannon"), Some(WeaponKind::BugCannon));
        assert_eq!(WeaponKind::find("bugcannon"), Some(WeaponKind::BugCannon));
        assert_eq!(WeaponKind::find("BUGCANNON"), Some(WeaponKind::BugCannon));
        // The three FBI-authored spellings that only match case-insensitively.
        assert_eq!(WeaponKind::find("minekiller"), Some(WeaponKind::Minekiller));
        assert_eq!(WeaponKind::find("rock"), Some(WeaponKind::Rock));
        assert_eq!(WeaponKind::find("spark"), Some(WeaponKind::Spark));
    }

    #[test]
    fn every_variant_round_trips_through_find() {
        for &kind in ALL_WEAPON_KINDS {
            assert_eq!(WeaponKind::find(kind.name()), Some(kind));
            assert_eq!(
                WeaponKind::find(&kind.name().to_ascii_lowercase()),
                Some(kind)
            );
        }
    }

    #[test]
    fn indices_are_dense_and_unique() {
        let mut seen = [false; WEAPON_KIND_COUNT];
        for &kind in ALL_WEAPON_KINDS {
            let i = kind.index();
            assert!(!seen[i], "{kind:?} shares a slot");
            seen[i] = true;
        }
    }

    #[test]
    fn an_unarmed_or_unknown_weapon_name_resolves_to_nothing() {
        assert_eq!(WeaponKind::find(""), None);
        assert_eq!(WeaponKind::find("   "), None);
        assert_eq!(WeaponKind::find("BitShot"), None);
    }

    #[test]
    fn the_shipped_tdfs_define_every_variant() {
        // `load()` warns when this is untrue; assert it here so a renamed
        // section is a test failure rather than a silently unarmed unit.
        let registry = WeaponRegistry::load();
        for &kind in ALL_WEAPON_KINDS {
            assert!(registry.is_present(kind), "{} missing", kind.name());
        }
        // Slot 0 is pre-filled by `default()`, so `is_present` cannot
        // tell a loaded `[BuildLaser]` from the stub: check the authored
        // `range=256` (retroweapons.tdf) as well.
        assert_eq!(registry.by_kind(WeaponKind::BuildLaser).range, 256.0);
    }

    #[test]
    fn infection_durations_match_the_upstream_gadget() {
        assert_eq!(WeaponKind::VirusBeam.infection_duration(), Some(3.0));
        assert_eq!(WeaponKind::VirusDeath.infection_duration(), Some(6.0));
        assert!((WeaponKind::Wormsplash.infection_duration().unwrap() - 6.666_667).abs() < 1e-3);
        assert_eq!(WeaponKind::Infection.infection_duration(), Some(1.0));
        assert_eq!(WeaponKind::BugShot.infection_duration(), None);
        assert_eq!(WeaponKind::Wormbite.infection_duration(), None);
    }
}

//! Well-known callin function names.
//!
//! Mirrors upstream `rts/Sim/Units/Scripts/CobScriptNames.h`. The engine
//! caches `name → fn id` lookups for common entry points (`Create`,
//! `Killed`, `AimWeapon1`, ...) so dispatch doesn't go through a string
//! map every frame. The numeric identifiers are an ABI shared between
//! the engine and any tooling that introspects scripts; preserve them
//! verbatim. Aliases for the legacy "Primary/Secondary/Tertiary" weapon
//! names are kept so old `.bos` files keep working.
//!
//! Use [`CobFn::function_id`] in conjunction with
//! [`CobFile::function_id_for_callin`](crate::cob_file::CobFile::function_id_for_callin)
//! to resolve a callin to the actual bytecode offset on a given script.
//!
//! [CobScriptNames]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/Sim/Units/Scripts/CobScriptNames.h

/// Maximum weapons the engine indexes per unit. Mirrors
/// `rts/Sim/Misc/GlobalConstants.h:MAX_WEAPONS_PER_UNIT`. Recoil uses 32;
/// we follow that so `CobFn::Aim`/`Fire`/etc. can address the same range.
pub const MAX_WEAPONS_PER_UNIT: usize = 32;

/// Width (in `CobFn` slots) of one weapon's callin block.
pub const WEAPON_FUNCS: usize = 8;

/// Indices reserved for non-weapon callins. After this, the next
/// `MAX_WEAPONS_PER_UNIT * WEAPON_FUNCS` slots hold the per-weapon
/// blocks, in the same packing as upstream's enum.
pub const NUM_NON_WEAPON_CALLINS: usize = 33;

/// All possible callin slots, in upstream's enum order. Use
/// [`CobFn::weapon_callin`] to address per-weapon entries by `(kind,
/// weapon_index)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumIter)]
#[repr(usize)]
pub enum CobFn {
    Create = 0,
    Destroy,
    StartMoving,
    StopMoving,
    Activate,
    Killed,
    Deactivate,
    SetDirection,
    SetSpeed,
    RockUnit,
    HitByWeapon,
    MoveRate0,
    MoveRate1,
    MoveRate2,
    MoveRate3,
    SetSfxOccupy,
    HitByWeaponId,
    QueryLandingPadCount,
    QueryLandingPad,
    Falling,
    Landed,
    BeginTransport,
    QueryTransport,
    TransportPickup,
    StartUnload,
    EndTransport,
    TransportDrop,
    SetMaxReloadTime,
    StartBuilding,
    StopBuilding,
    QueryNanoPiece,
    QueryBuildInfo,
    Go,
}

impl CobFn {
    /// Numeric callin slot (matches upstream's `COBFN_*` integer). Stable
    /// across builds — used when round-tripping callin tables to disk.
    pub const fn id(self) -> usize {
        self as usize
    }

    /// Canonical script-function name as written in `.bos`/`.cob` for
    /// this non-weapon callin. Matches `CobScriptNames.cpp`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Create => "Create",
            Self::Destroy => "Destroy",
            Self::StartMoving => "StartMoving",
            Self::StopMoving => "StopMoving",
            Self::Activate => "Activate",
            Self::Killed => "Killed",
            Self::Deactivate => "Deactivate",
            Self::SetDirection => "SetDirection",
            Self::SetSpeed => "SetSpeed",
            Self::RockUnit => "RockUnit",
            Self::HitByWeapon => "HitByWeapon",
            Self::MoveRate0 => "MoveRate0",
            Self::MoveRate1 => "MoveRate1",
            Self::MoveRate2 => "MoveRate2",
            Self::MoveRate3 => "MoveRate3",
            // Upstream casing: lowercase 's' / 'o'. Don't "fix" it; the
            // engine looks for that exact string in the script's name table.
            Self::SetSfxOccupy => "setSFXoccupy",
            Self::HitByWeaponId => "HitByWeaponId",
            Self::QueryLandingPadCount => "QueryLandingPadCount",
            Self::QueryLandingPad => "QueryLandingPad",
            Self::Falling => "Falling",
            Self::Landed => "Landed",
            Self::BeginTransport => "BeginTransport",
            Self::QueryTransport => "QueryTransport",
            Self::TransportPickup => "TransportPickup",
            Self::StartUnload => "StartUnload",
            Self::EndTransport => "EndTransport",
            Self::TransportDrop => "TransportDrop",
            Self::SetMaxReloadTime => "SetMaxReloadTime",
            Self::StartBuilding => "StartBuilding",
            Self::StopBuilding => "StopBuilding",
            Self::QueryNanoPiece => "QueryNanoPiece",
            Self::QueryBuildInfo => "QueryBuildInfo",
            Self::Go => "Go",
        }
    }
}

/// One slot inside a weapon's per-weapon callin block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumIter)]
#[repr(usize)]
pub enum WeaponCallin {
    Query = 0,
    Aim,
    AimFrom,
    Fire,
    EndBurst,
    Shot,
    BlockShot,
    TargetWeight,
}

impl WeaponCallin {
    /// Numeric slot inside the per-weapon block (0..[`WEAPON_FUNCS`]).
    pub const fn slot(self) -> usize {
        self as usize
    }

    /// Canonical name with the 1-based weapon suffix used by upstream
    /// (e.g. `"AimWeapon1"`, `"FireWeapon2"`). `weapon_index` is 0-based.
    pub fn name(self, weapon_index: usize) -> String {
        let prefix = match self {
            Self::Query => "QueryWeapon",
            Self::Aim => "AimWeapon",
            Self::AimFrom => "AimFromWeapon",
            Self::Fire => "FireWeapon",
            Self::EndBurst => "EndBurst",
            Self::Shot => "Shot",
            Self::BlockShot => "BlockShot",
            Self::TargetWeight => "TargetWeight",
        };
        format!("{prefix}{}", weapon_index + 1)
    }

    /// Combined callin index suitable for use as the array offset into a
    /// `[Option<usize>; total_callin_slots()]` table — i.e. the upstream
    /// `COBFN_AimPrimary + COBFN_Weapon_Funcs * weapon_index` arithmetic.
    pub const fn callin_index(self, weapon_index: usize) -> usize {
        NUM_NON_WEAPON_CALLINS + weapon_index * WEAPON_FUNCS + self as usize
    }
}

/// Total number of indexable callin slots. Use this to size a
/// `[Option<usize>; total_callin_slots()]` lookup table at compile time.
pub const fn total_callin_slots() -> usize {
    NUM_NON_WEAPON_CALLINS + MAX_WEAPONS_PER_UNIT * WEAPON_FUNCS
}

/// Resolve a script-function name to its `(non-weapon CobFn)` slot, or
/// to a `(weapon-callin, weapon_index)` pair. Mirrors upstream's
/// `scriptMap` plus the legacy aliases (`"FirePrimary"` →
/// `(WeaponCallin::Fire, 0)` etc.).
pub fn resolve_callin(name: &str) -> Option<CallinSlot> {
    // Non-weapon callins: walk the enum.
    use strum::IntoEnumIterator;
    for fn_kind in CobFn::iter() {
        if fn_kind.name() == name {
            return Some(CallinSlot::Plain(fn_kind));
        }
    }

    // Per-weapon: legacy aliases first.
    let alias = match name {
        "QueryPrimary" => Some((WeaponCallin::Query, 0)),
        "QuerySecondary" => Some((WeaponCallin::Query, 1)),
        "QueryTertiary" => Some((WeaponCallin::Query, 2)),
        "AimPrimary" => Some((WeaponCallin::Aim, 0)),
        "AimSecondary" => Some((WeaponCallin::Aim, 1)),
        "AimTertiary" => Some((WeaponCallin::Aim, 2)),
        "AimFromPrimary" => Some((WeaponCallin::AimFrom, 0)),
        "AimFromSecondary" => Some((WeaponCallin::AimFrom, 1)),
        "AimFromTertiary" => Some((WeaponCallin::AimFrom, 2)),
        "FirePrimary" => Some((WeaponCallin::Fire, 0)),
        "FireSecondary" => Some((WeaponCallin::Fire, 1)),
        "FireTertiary" => Some((WeaponCallin::Fire, 2)),
        _ => None,
    };
    if let Some((kind, idx)) = alias {
        return Some(CallinSlot::Weapon(kind, idx));
    }

    // Per-weapon: numeric suffix variants. Strip the 1-based suffix off
    // the prefix; the prefix table mirrors the upstream loop.
    for (prefix, kind) in [
        ("QueryWeapon", WeaponCallin::Query),
        ("AimWeapon", WeaponCallin::Aim),
        ("AimFromWeapon", WeaponCallin::AimFrom),
        ("FireWeapon", WeaponCallin::Fire),
        ("EndBurst", WeaponCallin::EndBurst),
        ("Shot", WeaponCallin::Shot),
        ("BlockShot", WeaponCallin::BlockShot),
        ("TargetWeight", WeaponCallin::TargetWeight),
    ] {
        let Some(suffix) = name.strip_prefix(prefix) else {
            continue;
        };
        let Ok(one_based) = suffix.parse::<usize>() else {
            continue;
        };
        if one_based == 0 || one_based > MAX_WEAPONS_PER_UNIT {
            continue;
        }
        return Some(CallinSlot::Weapon(kind, one_based - 1));
    }

    None
}

/// A resolved callin reference. Either a plain non-weapon entry or a
/// weapon-indexed slot; both can be looked up against
/// [`CobFile::function_id_for_callin`](crate::cob_file::CobFile::function_id_for_callin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallinSlot {
    Plain(CobFn),
    Weapon(WeaponCallin, usize),
}

impl CallinSlot {
    /// Position in a flat `[Option<usize>; total_callin_slots()]` table.
    pub const fn callin_index(self) -> usize {
        match self {
            Self::Plain(fn_kind) => fn_kind as usize,
            Self::Weapon(kind, weapon_index) => kind.callin_index(weapon_index),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_resolves_to_plain_create() {
        assert_eq!(
            resolve_callin("Create"),
            Some(CallinSlot::Plain(CobFn::Create))
        );
    }

    #[test]
    fn aim_weapon_1_resolves_to_weapon_0() {
        assert_eq!(
            resolve_callin("AimWeapon1"),
            Some(CallinSlot::Weapon(WeaponCallin::Aim, 0))
        );
    }

    #[test]
    fn aim_primary_alias_resolves_to_weapon_0() {
        assert_eq!(
            resolve_callin("AimPrimary"),
            Some(CallinSlot::Weapon(WeaponCallin::Aim, 0))
        );
    }

    #[test]
    fn unknown_callin_is_none() {
        assert_eq!(resolve_callin("NotAThing"), None);
    }

    /// Upstream uses `setSFXoccupy` — note the camelCase. Off-by-one would
    /// silently break SetSFXOccupy callins on every unit.
    #[test]
    fn sfx_occupy_uses_upstream_camelcase() {
        assert_eq!(CobFn::SetSfxOccupy.name(), "setSFXoccupy");
        assert_eq!(
            resolve_callin("setSFXoccupy"),
            Some(CallinSlot::Plain(CobFn::SetSfxOccupy))
        );
    }

    #[test]
    fn weapon_callin_index_packs_above_non_weapon_block() {
        // Aim for weapon 0 sits right after the non-weapon block.
        assert_eq!(
            WeaponCallin::Aim.callin_index(0),
            NUM_NON_WEAPON_CALLINS + WeaponCallin::Aim.slot()
        );
        // Adjacent weapons are exactly WEAPON_FUNCS apart.
        assert_eq!(
            WeaponCallin::Aim.callin_index(1) - WeaponCallin::Aim.callin_index(0),
            WEAPON_FUNCS
        );
    }

    #[test]
    fn last_weapon_index_fits_in_table() {
        assert!(
            WeaponCallin::TargetWeight.callin_index(MAX_WEAPONS_PER_UNIT - 1)
                < total_callin_slots()
        );
    }
}

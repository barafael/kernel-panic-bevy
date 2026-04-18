//! Unit registry loaded from upstream FBI files.
//!
//! At startup we read every `.fbi` file from the upstream units directory
//! and merge them into a single [`UnitRegistry`] resource. Game systems
//! resolve unit stats through this registry instead of hardcoded values.

use bevy::prelude::*;
use spring_tdf::{UnitDef, UnitDefs};

use super::definitions::{ALL_UNIT_KINDS, UnitKind};
use super::tdf_loader;

/// Spring engine simulation runs at 30 frames per second.
/// FBI `MaxVelocity` is in elmos/frame; multiply by this to get elmos/second.
const SPRING_SIM_FPS: f32 = 30.0;

/// Spring engine `BuildTime` is in "build ticks" at 30 fps.
/// The actual build duration depends on the factory's `WorkerTime`.
/// For Kernel Panic, factories have WorkerTime = 64-128, and the
/// convention is build_time = BuildTime / WorkerTime seconds.
/// We use 128 as the standard worker speed (homebases).
const DEFAULT_WORKER_TIME: f32 = 128.0;

/// Below this cutoff, an FBI `DamageModifier` is treated as the Spring
/// engine-disable hack (`0.000001`) rather than a real gameplay value
/// and is normalised to `1.0`. Any legitimate designer-set vulnerability
/// or resistance we've seen in KP is O(1) — 0.5 through 4.0 — so the
/// threshold sits well below that range.
const DAMAGE_MODIFIER_DISABLED_THRESHOLD: f32 = 0.01;

/// Fallback max-slope ratio for units whose FBI omits `MaxSlope` (e.g.
/// buildings). Matches the global cap in `map_loading.rs` (45°).
pub const DEFAULT_MAX_SLOPE_RATIO: f32 = 1.0;

/// Upper bound clamp on per-unit max-slope so a rogue FBI
/// (`MaxSlope=90` → tan → infinity) can't produce a bucket that makes
/// every cell passable. 60° (tan ≈ 1.73) matches the steepest explicit
/// KP cap and is well above any real map slope.
pub const MAX_SLOPE_RATIO: f32 = 1.8;

/// All parsed unit definitions, accessible by `UnitKind`.
#[derive(Resource)]
pub struct UnitRegistry {
    defs: UnitDefs,
}

impl UnitRegistry {
    /// Load all `.fbi` files from the upstream units directory.
    pub fn load() -> Self {
        let Some(dir) = tdf_loader::find_upstream_dir("units") else {
            warn!("Upstream units directory not found — using empty registry");
            return Self {
                defs: UnitDefs::default(),
            };
        };

        let mut merged = UnitDefs::default();
        for (filename, tdf) in tdf_loader::load_all_tdf_files(&dir, "fbi") {
            let defs = UnitDefs::from_tdf(&tdf);
            let count = defs.units.len();
            merged.units.extend(defs.units);
            info!("  Loaded {count} units from {filename}");
        }

        info!("Unit registry: {} definitions total", merged.units.len());
        let registry = Self { defs: merged };
        registry.validate_unit_bindings();
        registry
    }

    /// Look up the raw FBI definition for a unit kind.
    ///
    /// Uses direct BTreeMap lookup since `UnitKind::unitname()` returns
    /// already-lowercase keys, avoiding the `to_ascii_lowercase()` allocation
    /// in `UnitDefs::get()`.
    pub fn def(&self, kind: UnitKind) -> Option<&UnitDef> {
        self.defs.units.get(kind.unitname())
    }

    // -- Convenience accessors that map FBI fields to game-usable values --

    /// Display name (e.g. "Bit", "Denial of Service").
    pub fn name(&self, kind: UnitKind) -> &str {
        self.def(kind).map_or(kind.unitname(), |d| &d.name)
    }

    /// Maximum health (FBI `MaxDamage`).
    pub fn max_health(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.max_health)
    }

    /// Movement speed in elmos per second.
    pub fn speed(&self, kind: UnitKind) -> f32 {
        self.def(kind)
            .map_or(0.0, |d| d.max_velocity * SPRING_SIM_FPS)
    }

    /// Maximum turn speed in radians per second. Spring's FBI `TurnRate` is
    /// in 16-bit heading units per sim frame (65536 = 360°, 30 fps), so
    /// `rad/sec = TurnRate / 65536 * 2π * 30`. A TurnRate of 0 means the
    /// unit can't rotate (buildings); our movement system treats that as
    /// "snap instantly" so the face-target step is still coherent but
    /// doesn't produce a divide-by-zero.
    ///
    /// Multiplied by `TURN_RATE_MULT` to make units feel responsive: raw
    /// Spring values assumed a 30 Hz sim tick and engine-level heading
    /// interpolation we don't reproduce, which left modest TurnRates
    /// (200-500) visibly sluggish at 60 fps.
    pub fn turn_rate(&self, kind: UnitKind) -> f32 {
        const SPRING_ANGLE_UNITS_PER_REV: f32 = 65536.0;
        const TURN_RATE_MULT: f32 = 3.0;
        self.def(kind).map_or(0.0, |d| {
            d.turn_rate / SPRING_ANGLE_UNITS_PER_REV
                * std::f32::consts::TAU
                * SPRING_SIM_FPS
                * TURN_RATE_MULT
        })
    }

    /// Whether this unit flies (FBI `canFly=1`). Flying units ignore the
    /// terrain-Y snap in the movement system and hold `cruise_alt` above
    /// the ground instead.
    pub fn can_fly(&self, kind: UnitKind) -> bool {
        self.def(kind).is_some_and(|d| d.can_fly)
    }

    /// Does this unit's `NoChaseCategory` contain `VTOL`? Most KP ground
    /// units set this so they'll ignore Flows (and other flying units)
    /// during auto-target selection. Upstream treats the field as a space-
    /// separated token list, e.g. `NoChaseCategory=VTOL FACTORY`.
    pub fn no_chase_vtol(&self, kind: UnitKind) -> bool {
        self.def(kind).is_some_and(|d| {
            d.no_chase_category
                .split_ascii_whitespace()
                .any(|tok| tok.eq_ignore_ascii_case("VTOL"))
        })
    }

    /// Max traversable slope as a `dy/dx` ratio (what
    /// `spring_pathfinding::SpeedMap` takes). Spring's FBI `MaxSlope` is in
    /// degrees (KP values: 10, 15, 20, 32, 60); we convert with
    /// `tan(degrees)`. Unknown / zero-MaxSlope units fall back to
    /// [`DEFAULT_MAX_SLOPE_RATIO`] (45°, matching the global cap in
    /// `map_loading.rs`).
    pub fn max_slope_ratio(&self, kind: UnitKind) -> f32 {
        self.def(kind)
            .map(|d| d.max_slope)
            .filter(|&deg| deg > 0.0)
            .map_or(DEFAULT_MAX_SLOPE_RATIO, |deg| {
                deg.to_radians().tan().min(MAX_SLOPE_RATIO)
            })
    }

    /// Cruise altitude in elmos above the terrain for flying units. 0 for
    /// ground units; only consulted when `can_fly` is true.
    pub fn cruise_alt(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.cruise_alt)
    }

    /// Approximate collision radius in world units (elmos) derived from the
    /// FBI footprint. Spring's `FootprintX` / `FootprintZ` are in map squares
    /// (1 square = 8 elmos), so half the larger dimension is a reasonable
    /// in-plane circular radius for both hit testing and hard collision.
    /// Returns a small minimum so an unparsed or zero-footprint unit still
    /// has a non-zero radius.
    pub fn collision_radius(&self, kind: UnitKind) -> f32 {
        const ELMOS_PER_SQUARE: f32 = 8.0;
        const MIN_RADIUS: f32 = 6.0;
        self.def(kind).map_or(MIN_RADIUS, |d| {
            let larger = d.footprint_x.max(d.footprint_z);
            (larger * ELMOS_PER_SQUARE * 0.5).max(MIN_RADIUS)
        })
    }

    /// Build time in seconds, assuming the standard worker speed.
    pub fn build_time(&self, kind: UnitKind) -> f32 {
        self.def(kind)
            .map_or(0.0, |d| d.build_time / DEFAULT_WORKER_TIME)
    }

    /// Whether this unit is a building (cannot move or has zero velocity).
    pub fn is_building(&self, kind: UnitKind) -> bool {
        self.def(kind)
            .is_some_and(|d| !d.can_move || d.max_velocity == 0.0)
    }

    /// S3O model filename (e.g. "kernel.s3o").
    pub fn model(&self, kind: UnitKind) -> &str {
        self.def(kind).map_or("", |d| &d.object_name)
    }

    /// Buildpic filename as declared in the FBI (e.g. "bit.pcx", "network_big.png").
    /// Returns `""` when the unit has no BuildPic field.
    pub fn build_pic(&self, kind: UnitKind) -> &str {
        self.def(kind).map_or("", |d| &d.build_pic)
    }

    /// Proximity trigger radius (elmos) for kamikaze units. A non-zero
    /// value implies this unit detonates when an enemy enters the
    /// circle; returns 0.0 for everything else.
    pub fn kamikaze_distance(&self, kind: UnitKind) -> f32 {
        self.def(kind)
            .map_or(0.0, |d| if d.kamikaze { d.kamikaze_distance } else { 0.0 })
    }

    /// Detector radius (elmos) — a unit can reveal cloaked enemies within
    /// this range. Maps to the FBI `RadarDistance` field; zero means this
    /// unit kind does not detect cloaked targets.
    pub fn detector_range(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.radar_distance)
    }

    /// Vision range in elmos (FBI `SightDistance`). Used by the
    /// fog-of-war MVP in `cloak::update_fog_visibility` to reveal
    /// enemies that any player-team unit can "see". Units without a
    /// declared SightDistance (Debug, LogicBomb) return `0.0` and
    /// contribute no vision.
    pub fn sight_distance(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.sight_distance)
    }

    /// HP per second regenerated once a unit has been idle for `idle_time`.
    /// Zero means the unit never auto-heals.
    pub fn idle_auto_heal(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.idle_auto_heal)
    }

    /// Sim frames (30/s) the unit must be idle before auto-heal kicks in.
    pub fn idle_time(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.idle_time)
    }

    /// Incoming-damage multiplier from the FBI `DamageModifier` field.
    ///
    /// Spring-engine trick: upstream Kernel Panic sets
    /// `DamageModifier=0.000001` on every combat unit as a way to *disable*
    /// Spring's default damage path — the real damage formula lives in
    /// KP's LuaRules gadget. Our reimplementation resolves damage
    /// directly in [`super::combat::apply_damage`], so treating the FBI
    /// near-zero value literally zeroes out every hit (a Bit takes
    /// `80 × 1e-6 ≈ 8e-5` HP per Line shot — it lives forever).
    ///
    /// Pragmatic rule: values below [`DAMAGE_MODIFIER_DISABLED_THRESHOLD`]
    /// are treated as the engine-disable hack and round to `1.0`.
    /// Explicit design values like `4.0` (Socket / Firewall: deliberately
    /// fragile) pass through unchanged. A missing field also defaults to
    /// `1.0`. If we ever want homebase / Byte near-immunity back, it
    /// should come from a dedicated per-kind multiplier table rather
    /// than the FBI engine-hack value.
    pub fn damage_modifier(&self, kind: UnitKind) -> f32 {
        let raw = self.def(kind).map_or(1.0, |d| d.damage_modifier);
        if raw < DAMAGE_MODIFIER_DISABLED_THRESHOLD {
            1.0
        } else {
            raw
        }
    }

    /// Primary weapon TDF section name, or `""` if unarmed / only has BuildLaser.
    pub fn weapon(&self, kind: UnitKind) -> &str {
        self.def(kind).map_or("", |d| {
            let w = d.weapon1.as_str();
            if w.eq_ignore_ascii_case("BuildLaser") || w.eq_ignore_ascii_case("BuildLaserNoEffect")
            {
                ""
            } else {
                w
            }
        })
    }

    fn validate_unit_bindings(&self) {
        for &kind in ALL_UNIT_KINDS {
            if self.def(kind).is_none() {
                warn!(
                    "Unit kind {:?} (unitname='{}') not found in FBI files",
                    kind,
                    kind.unitname(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spring_tdf::UnitDef;

    fn registry_with(kind: UnitKind, raw_damage_modifier: f32) -> UnitRegistry {
        let mut defs = UnitDefs::default();
        let def = UnitDef {
            damage_modifier: raw_damage_modifier,
            ..UnitDef::default()
        };
        defs.units.insert(kind.unitname().to_string(), def);
        UnitRegistry { defs }
    }

    /// Upstream KP ships every combat unit with `DamageModifier=0.000001`
    /// as a Spring engine-disable hack. If we applied that literally
    /// every hit would shave off 1e-6 of the weapon's damage and no unit
    /// would ever die (observed pre-fix). The accessor must normalise
    /// those sub-threshold values to `1.0`.
    #[test]
    fn near_zero_damage_modifier_is_treated_as_spring_engine_hack() {
        let reg = registry_with(UnitKind::Bit, 0.000_001);
        assert_eq!(reg.damage_modifier(UnitKind::Bit), 1.0);
    }

    /// Design-intent values (Socket / Firewall take 4× damage) must
    /// survive normalisation unchanged.
    #[test]
    fn designer_set_multiplier_passes_through() {
        let reg = registry_with(UnitKind::Socket, 4.0);
        assert_eq!(reg.damage_modifier(UnitKind::Socket), 4.0);
    }

    /// Missing FBI entry falls back to neutral `1.0`.
    #[test]
    fn missing_unit_defaults_to_one() {
        let reg = UnitRegistry {
            defs: UnitDefs::default(),
        };
        assert_eq!(reg.damage_modifier(UnitKind::Bit), 1.0);
    }

    fn registry_with_slope(kind: UnitKind, raw_max_slope_deg: f32) -> UnitRegistry {
        let mut defs = UnitDefs::default();
        let def = UnitDef {
            max_slope: raw_max_slope_deg,
            ..UnitDef::default()
        };
        defs.units.insert(kind.unitname().to_string(), def);
        UnitRegistry { defs }
    }

    #[test]
    fn max_slope_ratio_converts_fbi_degrees_to_rise_run() {
        // Bit's FBI: MaxSlope=20 → tan(20°) ≈ 0.364.
        let reg = registry_with_slope(UnitKind::Bit, 20.0);
        let ratio = reg.max_slope_ratio(UnitKind::Bit);
        assert!(
            (ratio - 20.0_f32.to_radians().tan()).abs() < 1e-5,
            "got {ratio}"
        );
    }

    #[test]
    fn max_slope_ratio_default_for_missing_unit() {
        let reg = UnitRegistry {
            defs: UnitDefs::default(),
        };
        // Buildings / unparsed units fall back to the 45° cap.
        assert_eq!(reg.max_slope_ratio(UnitKind::Bit), DEFAULT_MAX_SLOPE_RATIO);
    }

    #[test]
    fn max_slope_ratio_clamps_absurd_values() {
        // Paranoid FBI with MaxSlope=89 → tan(89°) ≈ 57, which would
        // produce a bucket where every cell is passable. Clamp to our
        // configured ceiling so the bucket selector still has a useful
        // "loosest" category.
        let reg = registry_with_slope(UnitKind::Bit, 89.0);
        assert_eq!(reg.max_slope_ratio(UnitKind::Bit), MAX_SLOPE_RATIO);
    }
}

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
        for kind in ALL_UNIT_KINDS {
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

//! Unit registry loaded from the upstream FBI files (via the unit bundle).
//!
//! At startup the bundle's merged `units/*.fbi` definitions become a
//! single [`UnitRegistry`] resource. Game systems
//! resolve unit stats through this registry instead of hardcoded values.

use bevy::prelude::*;
use spring_tdf::{UnitDef, UnitDefs};

use super::definitions::{ALL_UNIT_KINDS, UNIT_KIND_COUNT, UnitKind};
use super::moveinfo::MoveClassTable;
use crate::sim::{GAME_SPEED, SHORT_ANGLE_TO_RAD};

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

/// A ground unit's Spring `MoveDef` geometry (`MoveDefHandler.cpp`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveDefParams {
    /// Footprint half-size in heightmap squares: MOVEINFO `FootprintX
    /// × 2`, made odd (`xsize -= !(xsize & 1)`, `MoveDefHandler.cpp:
    /// 314-319`), halved — LIGHT 1 (3 squares), MEDIUM/HEAVY 3 (7).
    pub xsizeh: i32,
    pub zsizeh: i32,
    /// `CalcFootPrintMaxInteriorRadius`: collision radius (LIGHT 12).
    pub collision_radius: f32,
    /// `CalcFootPrintMinExteriorRadius`: `ownerRadius`, used for goal
    /// tolerance and avoidance (LIGHT ≈ 17).
    pub owner_radius: f32,
    /// MOVEINFO `CrushStrength`.
    pub crush_strength: f32,
}

impl MoveDefParams {
    pub fn from_class(class: &super::moveinfo::MoveClassDef) -> Self {
        let odd = |fp: f32| {
            let size = (fp.round() as i32).max(1) * 2;
            size - if size & 1 == 1 { 0 } else { 1 }
        };
        let (xs, zs) = (odd(class.footprint_x), odd(class.footprint_z));
        Self {
            xsizeh: xs >> 1,
            zsizeh: zs >> 1,
            collision_radius: xs.max(zs) as f32 * 0.5 * 8.0,
            owner_radius: ((xs * xs + zs * zs) as f32).sqrt() * 0.5 * 8.0,
            crush_strength: class.crush_strength,
        }
    }
}

/// Default FBI `MaxSlope` (degrees) for KP movement classes, taken
/// from upstream `gamedata/MOVEINFO.TDF` where every entry
/// (LIGHT/MEDIUM/HEAVY) sets `MaxSlope=36`. Mobile units in KP don't
/// declare `MaxSlope` directly in their FBIs — they reference a
/// MovementClass that does. Until we wire up MOVEINFO parsing, fall
/// back to this so the pathfinder's effective cap matches the engine
/// upstream uses (54° geometric, after Spring's 1.5× pre-multiplier in
/// `DegreesToMaxSlope`).
pub const DEFAULT_MAX_SLOPE_DEGREES: f32 = 36.0;

/// Do the two space-separated category token lists share a token
/// (case-insensitive)? Spring category comparisons are ASCII
/// case-insensitive, and upstream FBI authors mix `FactoRy`-style
/// casing freely.
fn categories_intersect(tokens: &str, categories: &str) -> bool {
    tokens.split_ascii_whitespace().any(|tok| {
        categories
            .split_ascii_whitespace()
            .any(|c| c.eq_ignore_ascii_case(tok))
    })
}

/// [`UnitRegistry::bad_target`] for one attacker FBI against a
/// candidate's `Category` list.
fn bad_target_gate(attacker: &UnitDef, categories: &str) -> bool {
    categories_intersect(&attacker.bad_target_category1, categories)
}

/// [`UnitRegistry::can_attack`] for one attacker FBI against a
/// candidate's `Category` list: the `OnlyTargetCategory1` half only.
fn manual_target_gate(attacker: &UnitDef, categories: &str) -> bool {
    let only = attacker.only_target_category1.as_str();
    only.is_empty() || categories_intersect(only, categories)
}

/// `UnitDef::maxAcc` (elmos/frame²) with Spring's 0.5 default — see
/// [`UnitRegistry::acc_rate`].
fn max_acc(d: Option<&UnitDef>) -> f32 {
    const DEFAULT_MAX_ACC: f32 = 0.5;
    d.map(|d| d.acceleration)
        .filter(|&a| a > 0.0)
        .unwrap_or(DEFAULT_MAX_ACC)
}

/// `UnitDef::maxDec` (elmos/frame²), defaulting to `maxAcc` — see
/// [`UnitRegistry::dec_rate`].
fn max_dec(d: Option<&UnitDef>) -> f32 {
    d.map(|d| d.brake_rate)
        .filter(|&b| b > 0.0)
        .unwrap_or_else(|| max_acc(d))
}

/// Per-kind values derived from the FBI + MOVEINFO once at load, so the
/// per-unit-per-tick callers (movement, pathing) don't re-resolve
/// the movement class or re-parse degrees each frame.
#[derive(Debug, Clone, Copy)]
struct KindData {
    /// See [`UnitRegistry::move_def`].
    move_def: Option<MoveDefParams>,
    /// See [`UnitRegistry::max_slope_ratio`].
    max_slope_ratio: f32,
}

/// All parsed unit definitions, accessible by `UnitKind`.
///
/// Everything is resolved at load into dense tables indexed by
/// [`UnitKind::index`]: [`Self::def`] is an array index, not a map
/// lookup, and the pairwise target-category gates are precomputed
/// matrices — cheap enough for per-candidate / per-tick hot paths.
#[derive(Resource)]
pub struct UnitRegistry {
    /// The FBI definition of each kind (`None` when its file is missing).
    defs: Box<[Option<UnitDef>]>,
    /// Derived per-kind data, parallel to `defs`.
    kinds: Box<[KindData]>,
    /// [`Self::auto_target_allowed`], row-major `[attacker][candidate]`.
    bad_target: Box<[bool]>,
    /// [`Self::can_attack`], row-major `[attacker][candidate]`.
    manual_target: Box<[bool]>,
    /// See [`Self::hexfarm_medians`] — over *every* loaded FBI, not just
    /// the ones bound to a `UnitKind`.
    hexfarm_medians: (f64, f64),
}

impl UnitRegistry {
    /// The registry of the baked `units/*.fbi` + `MOVEINFO.TDF` (the
    /// unit bundle).
    pub fn load() -> Self {
        let bundle = super::bundle::bundle();
        info!(
            "Unit registry: {} definitions total",
            bundle.units.units.len()
        );
        let registry = Self::from_defs(bundle.units.clone(), bundle.move_classes.clone());
        registry.validate_unit_bindings();
        registry
    }

    /// Resolve parsed FBIs (keyed by lowercase `unitname`, as
    /// `UnitDefs::from_tdf` stores them) and the MOVEINFO table into the
    /// per-kind tables.
    pub fn from_defs(defs: UnitDefs, move_classes: MoveClassTable) -> Self {
        let all = || defs.units.values();
        let hexfarm_medians = (
            spring_map::hexfarm::lua_median(all().map(|d| d.max_health as f64)),
            spring_map::hexfarm::lua_median(all().map(|d| d.build_time as f64)),
        );
        let mut units = defs.units;
        let defs: Box<[Option<UnitDef>]> = ALL_UNIT_KINDS
            .iter()
            .map(|k| units.remove(k.unitname()))
            .collect();
        let kinds = defs
            .iter()
            .map(|d| KindData {
                move_def: d
                    .as_ref()
                    .filter(|d| !d.can_fly && !d.movement_class.is_empty())
                    .and_then(|d| move_classes.def_for(&d.movement_class))
                    .map(|class| MoveDefParams::from_class(&class)),
                max_slope_ratio: spring_pathfinding::max_slope_from_degrees(
                    d.as_ref()
                        .map(|d| d.max_slope)
                        .filter(|&deg| deg > 0.0)
                        .unwrap_or(DEFAULT_MAX_SLOPE_DEGREES),
                ),
            })
            .collect();
        let pairs = |gate: fn(&UnitDef, &str) -> bool| -> Box<[bool]> {
            defs.iter()
                .flat_map(|attacker| {
                    defs.iter().map(move |candidate| {
                        // No FBI for the attacker: unfiltered, like a
                        // unit that declares no category restrictions.
                        attacker.as_ref().is_none_or(|a| {
                            gate(a, candidate.as_ref().map_or("", |c| c.category.as_str()))
                        })
                    })
                })
                .collect()
        };
        Self {
            bad_target: pairs(bad_target_gate),
            manual_target: pairs(manual_target_gate),
            defs,
            kinds,
            hexfarm_medians,
        }
    }

    /// An empty registry — tests that instantiate systems without
    /// reading upstream disk files use this to satisfy the `Res<UnitRegistry>`
    /// system-param without triggering a `warn!` log flood for every
    /// known `UnitKind`.
    #[cfg(test)]
    pub fn empty() -> Self {
        Self::for_test(UnitDefs::default())
    }

    /// Test-only: build a registry from hand-authored defs, so systems
    /// under test can exercise FBI-derived behavior (target-category
    /// gates, speed conversion) without loading disk data.
    #[cfg(test)]
    pub fn for_test(defs: UnitDefs) -> Self {
        Self::from_defs(defs, MoveClassTable::default())
    }

    /// Look up the raw FBI definition for a unit kind (O(1)).
    pub fn def(&self, kind: UnitKind) -> Option<&UnitDef> {
        self.defs[kind.index()].as_ref()
    }

    fn kind(&self, kind: UnitKind) -> &KindData {
        &self.kinds[kind.index()]
    }

    /// Index of an `(attacker, candidate)` pair in the gate matrices.
    fn pair(attacker: UnitKind, candidate: UnitKind) -> usize {
        attacker.index() * UNIT_KIND_COUNT + candidate.index()
    }

    // -- Convenience accessors that map FBI fields to game-usable values --

    /// Display name (e.g. "Bit", "Denial of Service").
    pub fn name(&self, kind: UnitKind) -> &str {
        self.def(kind).map_or(kind.unitname(), |d| &d.name)
    }

    /// Median `health` and `buildTime` over every loaded unit def, as Hex
    /// Farm's gadget computes them from `UnitDefs` to scale how much
    /// damage sinks a tower and how much building raises one
    /// (`HexFarm8.lua` l.214-245). Raw FBI values (`MaxDamage`,
    /// `BuildTime`), not the port's derived seconds.
    pub fn hexfarm_medians(&self) -> (f64, f64) {
        self.hexfarm_medians
    }

    /// Maximum health (FBI `MaxDamage`).
    pub fn max_health(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.max_health)
    }

    /// Movement speed in elmos per second.
    pub fn speed(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.max_velocity * GAME_SPEED)
    }

    /// `UnitDef::maxAcc` in elmos/frame²: FBI `Acceleration`, default
    /// 0.5 (`UnitDef.cpp:455`; the TDF parser reads a missing tag as 0).
    /// Both move types apply it once per sim frame as `accRate`
    /// (`GroundMoveType.cpp:518`, `IPathController.cpp:33`): a Bit's 0.9
    /// reaches its 3 elmos/frame top speed in ~3.3 frames — Spring units
    /// snap to speed, they don't ramp for seconds.
    pub fn acc_rate(&self, kind: UnitKind) -> f32 {
        max_acc(self.def(kind))
    }

    /// `UnitDef::maxDec` in elmos/frame²: FBI `BrakeRate`, defaulting to
    /// [`Self::acc_rate`] (`UnitDef.cpp:459`). A Bit's 1.2 stops it from
    /// full speed within `v²/2a` = 3.75 elmos.
    pub fn dec_rate(&self, kind: UnitKind) -> f32 {
        max_dec(self.def(kind))
    }

    /// Maximum turn speed in radians per second. Spring's FBI `TurnRate`
    /// is in 16-bit heading units per sim frame (65536 = 360°) —
    /// `CGroundMoveType::turnRate` (GroundMoveType.cpp:514) applies it
    /// once per frame — so `rad/s = TurnRate / 65536 · 2π · 30`. A Bit's
    /// 480 turns 2.6°/frame, a half turn in ~2.3 s. A TurnRate of 0
    /// (buildings) is treated as "snap" by the movement code.
    pub fn turn_rate(&self, kind: UnitKind) -> f32 {
        self.def(kind)
            .map_or(0.0, |d| d.turn_rate * SHORT_ANGLE_TO_RAD * GAME_SPEED)
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

    /// Auto-target gate for the primary weapon: upstream FBI
    /// `OnlyTargetCategory1` against the candidate's `Category` list.
    /// When non-empty, the weapon may only acquire candidates whose
    /// categories share at least one token; `VOID` (Byte's mine
    /// launcher, all build lasers) therefore disables auto-targeting
    /// entirely. The same gate applies to explicit orders
    /// ([`Self::can_attack`]).
    ///
    /// Missing FBI entries (tests with an empty registry, unnamed kinds)
    /// leave the weapon unfiltered, matching a unit that declares no
    /// category restrictions.
    ///
    /// Precomputed per kind pair at load: an array index.
    pub fn auto_target_allowed(&self, attacker: UnitKind, candidate: UnitKind) -> bool {
        self.manual_target[Self::pair(attacker, candidate)]
    }

    /// Upstream FBI `BadTargetCategory1`: the candidate is a *bad*
    /// target for the attacker's primary weapon. The engine does not
    /// skip such targets, it multiplies their pick priority by 100
    /// (`CGameHelper::GenerateWeaponTargets`), so a Pointer still shells
    /// a lone Bit (`FAST`) and a Bit still shoots a Socket (`FACTORY`)
    /// when nothing better is in range.
    pub fn bad_target(&self, attacker: UnitKind, candidate: UnitKind) -> bool {
        self.bad_target[Self::pair(attacker, candidate)]
    }

    /// Manual-order gate for the primary weapon. `OnlyTargetCategory1`
    /// also blocks explicit attack orders in Spring (a DOS literally
    /// cannot attack a building — the engine refuses the command), while
    /// `BadTargetCategory1` only affects auto-acquisition. This checks
    /// the OnlyTarget half only.
    pub fn can_attack(&self, attacker: UnitKind, candidate: UnitKind) -> bool {
        self.manual_target[Self::pair(attacker, candidate)]
    }

    /// Max traversable slope in **Spring's encoding**: `1 - cos(deg ×
    /// 1.5)`. Ground units in KP don't set FBI `MaxSlope` — they
    /// reference `MovementClass` which keys into `MOVEINFO.TDF` where
    /// every class has `MaxSlope=36`. Until we parse `MOVEINFO.TDF`,
    /// the default mirrors that. Buildings (which DO declare `MaxSlope`
    /// directly) still pull from the FBI value.
    ///
    /// The 1.5× pre-multiplier in [`max_slope_from_degrees`] matches
    /// upstream `Sim/MoveTypes/MoveDefHandler.cpp:DegreesToMaxSlope` —
    /// without it, the port's pathfinder rejects ramps the engine
    /// would have accepted, leaving units stuck on plateaus that have
    /// 50° ramps the original game routes them through.
    pub fn max_slope_ratio(&self, kind: UnitKind) -> f32 {
        self.kind(kind).max_slope_ratio
    }

    /// `CHoverAirMoveType` constants for a flying unit, in Spring's
    /// per-frame units (`AAirMoveType` / `CHoverAirMoveType` ctors).
    pub fn hover_air_params(
        &self,
        kind: UnitKind,
    ) -> crate::interaction::air_movement::HoverAirParams {
        let fallback;
        let d = match self.def(kind) {
            Some(d) => d,
            None => {
                fallback = UnitDef::default();
                &fallback
            }
        };
        crate::interaction::air_movement::HoverAirParams {
            acc_rate: max_acc(Some(d)).max(0.01),
            dec_rate: max_dec(Some(d)).max(0.01),
            altitude_rate: d.vertical_speed.max(0.01),
            // Non-negative and finite: `clamp(-rate, rate)` panics on
            // a negative or NaN bound.
            turn_rate: (d.turn_rate * SHORT_ANGLE_TO_RAD)
                .abs()
                .max(SHORT_ANGLE_TO_RAD),
            cruise_alt: d.cruise_alt,
            hover_factor: d.air_hover_factor,
            banking_allowed: d.banking_allowed,
            // `mass` defaults to the metal cost (flow.fbi comments its
            // `mass` out).
            mass: d.build_cost_metal.max(1.0),
        }
    }

    /// Cruise altitude in elmos above the terrain for flying units. 0 for
    /// ground units; only consulted when `can_fly` is true.
    pub fn cruise_alt(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.cruise_alt)
    }

    /// The Spring `MoveDef` of a ground unit (its FBI `MovementClass`
    /// looked up in MOVEINFO.TDF), or `None` for structures and
    /// aircraft.
    pub fn move_def(&self, kind: UnitKind) -> Option<MoveDefParams> {
        self.kind(kind).move_def
    }

    /// `UnitDef::mass` (`UnitDef.cpp:351`): FBI `Mass`, defaulting to
    /// the metal cost, clamped to `[1, 1e6]`.
    pub fn mass(&self, kind: UnitKind) -> f32 {
        self.def(kind)
            .map_or(1.0, |d| d.mass.unwrap_or(d.build_cost_metal))
            .clamp(1.0, 1e6)
    }

    /// Collision radius (elmos) in the plane — what unit-unit
    /// collision, separation and group spacing use.
    ///
    /// - Ground units: the MoveDef footprint's
    ///   `CalcFootPrintMaxInteriorRadius` (`MoveDefHandler.cpp:734`,
    ///   used by `CGroundMoveType::HandleObjectCollisions`): LIGHT 12,
    ///   MEDIUM/HEAVY 28 — MOVEINFO footprints, not FBI ones.
    /// - Structures: the FBI footprint's max interior radius,
    ///   `FootprintX × SPRING_FOOTPRINT_SCALE(2) × 8 / 2`
    ///   (`UnitDef.cpp:671`) — e.g. 64 for an 8×8 homebase.
    /// - Aircraft keep the historical half-footprint radius the air
    ///   movement port was tuned against.
    pub fn collision_radius(&self, kind: UnitKind) -> f32 {
        const MIN_RADIUS: f32 = 6.0;
        if let Some(md) = self.move_def(kind) {
            return md.collision_radius;
        }
        self.def(kind).map_or(MIN_RADIUS, |d| {
            let larger = d.footprint_x.max(d.footprint_z);
            let per_unit = if d.can_fly { 4.0 } else { 8.0 };
            (larger * per_unit).max(MIN_RADIUS)
        })
    }

    /// Footprint in world elmos: FBI `FootprintX × SPRING_FOOTPRINT_SCALE
    /// (2)` heightmap squares of 8 elmos (`UnitDef.cpp:671`), i.e. ×16.
    /// Used by the placement ghost's slope gate
    /// (`CGameHelper::TestUnitBuildSquare` tests the same squares).
    /// Falls back to a 2×2 FBI footprint when FBI data is missing.
    pub fn footprint_elmos(&self, kind: UnitKind) -> Vec2 {
        const ELMOS_PER_FOOTPRINT_UNIT: f32 = 16.0;
        self.def(kind).map_or(Vec2::splat(32.0), |d| {
            Vec2::new(
                d.footprint_x.max(1.0) * ELMOS_PER_FOOTPRINT_UNIT,
                d.footprint_z.max(1.0) * ELMOS_PER_FOOTPRINT_UNIT,
            )
        })
    }

    /// Raw FBI `BuildTime` (Spring's `UnitDefs[].buildTime`), in build
    /// points rather than seconds.
    pub fn raw_build_time(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.build_time)
    }

    /// Build time in seconds, assuming the standard worker speed.
    pub fn build_time(&self, kind: UnitKind) -> f32 {
        self.def(kind)
            .map_or(0.0, |d| d.build_time / DEFAULT_WORKER_TIME)
    }

    /// FBI `WorkerTime`: build points a factory adds per second
    /// (`CFactory::buildSpeed = workerTime / GAME_SPEED` per frame).
    /// Kernel/Hole/Carrier build at 128, Socket/Window at 64 — a Bit
    /// takes 1.9 s from a Kernel but 3.75 s from a Socket.
    pub fn worker_time(&self, factory: UnitKind) -> f32 {
        self.def(factory)
            .map(|d| d.worker_time)
            .filter(|w| *w > 0.0)
            .unwrap_or(DEFAULT_WORKER_TIME)
    }

    /// Per-team cap on live units of this kind (FBI `UnitRestricted`,
    /// Spring's `maxThisUnit`). Only `logic_bomb.fbi` declares one (64);
    /// upstream `Launcher.lua` and `byte.bos` (`lua_GetLogicBombLeft`)
    /// honour it for launched mines too.
    pub fn team_limit(&self, kind: UnitKind) -> Option<u32> {
        self.def(kind).and_then(|d| d.unit_restricted)
    }

    /// FBI `Init_Cloaked`: the unit is cloaked from the moment it spawns
    /// (the Logic Bomb; the Worm's cloak is driven by its script cycle).
    pub fn init_cloaked(&self, kind: UnitKind) -> bool {
        self.def(kind).is_some_and(|d| d.init_cloaked)
    }

    /// FBI `IsFeature`: the unit turns into a crushable feature (the Bad
    /// Block wall).
    pub fn is_feature(&self, kind: UnitKind) -> bool {
        self.def(kind).is_some_and(|d| d.is_feature)
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

    /// Detector radius (elmos) — a unit reveals cloaked enemies within
    /// this range. Maps to the FBI `RadarDistance` field; zero means
    /// this unit kind does not detect cloaked targets. Read by
    /// `update_cloak_visibility` against the [`LocalTeam`](crate::units::player::LocalTeam).
    pub fn radar_distance(&self, kind: UnitKind) -> f32 {
        self.def(kind).map_or(0.0, |d| d.radar_distance)
    }

    /// Vision range in elmos (FBI `SightDistance`).
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

    /// Indexed CEG name for one of the unit's FBI `[SFXTypes]` entries.
    ///
    /// COB scripts fire `emit-sfx 1024+i from piece` to play particle
    /// generator `i` from the unit's FBI. Combat-side fire paths
    /// (muzzle flash, weapon hit) call this with the index their
    /// corresponding COB `FireWeaponN` body emits:
    ///
    /// - Bit `FireWeapon1` → `emit-sfx 1025` → index 1 (arrowflare muzzle).
    /// - Pointer `FireWeapon1` / `FireWeapon2` → `emit-sfx 1024` → index 0
    ///   (soft-blue puff).
    /// - Byte `FireWeapon1` → `emit-sfx 1024` at bp0..bp3 → index 0.
    ///
    /// Returns `None` when the unit has no `[SFXTypes]` block or the
    /// requested index is unset, so callers can fall back to a
    /// synthesised muzzle flash.
    pub fn sfx_type(&self, kind: UnitKind, index: usize) -> Option<&str> {
        self.def(kind).and_then(|d| {
            d.sfx_types
                .get(index)
                .map(|s| s.as_str())
                .filter(|s| !s.is_empty())
        })
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

    /// Secondary weapon (FBI `Weapon2`) TDF section name, or `""`.
    /// Only script-detonated weapons use it today (worm.bos `FireWeapon1`
    /// → `emit-sfx 4097` = Wormsplash).
    pub fn weapon2(&self, kind: UnitKind) -> &str {
        self.def(kind).map_or("", |d| d.weapon2.as_str())
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
        UnitRegistry::for_test(defs)
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
        let reg = UnitRegistry::empty();
        assert_eq!(reg.damage_modifier(UnitKind::Bit), 1.0);
    }

    fn registry_with_slope(kind: UnitKind, raw_max_slope_deg: f32) -> UnitRegistry {
        let mut defs = UnitDefs::default();
        let def = UnitDef {
            max_slope: raw_max_slope_deg,
            ..UnitDef::default()
        };
        defs.units.insert(kind.unitname().to_string(), def);
        UnitRegistry::for_test(defs)
    }

    /// FBI MaxSlope=20 should produce Spring's encoded value
    /// `1 - cos(20° × 1.5) = 1 - cos(30°) ≈ 0.134`. The 1.5×
    /// pre-multiplier matches upstream `DegreesToMaxSlope`.
    #[test]
    fn max_slope_ratio_uses_spring_encoding() {
        let reg = registry_with_slope(UnitKind::Bit, 20.0);
        let ratio = reg.max_slope_ratio(UnitKind::Bit);
        let expected = 1.0 - 30.0_f32.to_radians().cos();
        assert!(
            (ratio - expected).abs() < 1e-5,
            "got {ratio}, expected {expected}",
        );
    }

    /// KP mobile units (Bit/Bug/Byte/etc.) don't declare
    /// `MaxSlope=` in their FBIs — they reference `MovementClass`
    /// which keys into `MOVEINFO.TDF`. Until we parse that file, the
    /// fallback uses the upstream LIGHT/MEDIUM/HEAVY default of
    /// `MaxSlope=36`, encoded as `1 - cos(54°) ≈ 0.412`.
    #[test]
    fn max_slope_ratio_default_is_kp_class_default() {
        let reg = UnitRegistry::empty();
        let expected = 1.0 - 54.0_f32.to_radians().cos();
        let got = reg.max_slope_ratio(UnitKind::Bit);
        assert!(
            (got - expected).abs() < 1e-5,
            "got {got}, expected {expected} (FBI MaxSlope=36 default)",
        );
    }

    /// FBI `Acceleration=0.9` / `BrakeRate=1.2` (bit.fbi) are Spring's
    /// per-frame `maxAcc` / `maxDec` as-is; `MaxVelocity` converts to
    /// elmos/s. Missing fields fall back to Spring's defaults (`maxAcc`
    /// 0.5, `maxDec = maxAcc`).
    #[test]
    fn accel_brake_conversion_matches_fbi_frame_convention() {
        let mut defs = UnitDefs::default();
        defs.units.insert(
            "bit".into(),
            UnitDef {
                max_velocity: 3.0,
                acceleration: 0.9,
                brake_rate: 1.2,
                ..UnitDef::default()
            },
        );
        let reg = UnitRegistry::for_test(defs);

        assert_eq!(reg.speed(UnitKind::Bit), 90.0);
        assert_eq!(reg.acc_rate(UnitKind::Bit), 0.9);
        assert_eq!(reg.dec_rate(UnitKind::Bit), 1.2);

        let mut defs = UnitDefs::default();
        defs.units.insert(
            "bit".into(),
            UnitDef {
                max_velocity: 2.0,
                ..UnitDef::default()
            },
        );
        let reg = UnitRegistry::for_test(defs);
        assert_eq!(reg.acc_rate(UnitKind::Bit), 0.5);
        assert_eq!(reg.dec_rate(UnitKind::Bit), 0.5);
    }

    /// `max_slope_from_degrees` clamps the FBI input to `[0, 60]`
    /// before applying the 1.5× multiplier, mirroring upstream — a
    /// degenerate `MaxSlope=89` saturates at the same value as
    /// `MaxSlope=60` (≡ 1 - cos(90°) = 1.0, i.e. the whole map is
    /// climbable). We verify the clamp rather than imposing our own.
    #[test]
    fn max_slope_ratio_saturates_at_upstream_clamp() {
        let reg = registry_with_slope(UnitKind::Bit, 89.0);
        let got = reg.max_slope_ratio(UnitKind::Bit);
        // 60° clamp: 1 - cos(60° × 1.5) = 1 - cos(90°) = 1.0.
        assert!((got - 1.0).abs() < 1e-5, "got {got}, expected 1.0");
    }

    /// Register two kinds with the category tables copied from the
    /// upstream FBIs, so the target-category gates can be exercised
    /// without loading disk data.
    fn target_registry() -> UnitRegistry {
        let mut defs = UnitDefs::default();
        let bit = UnitDef {
            category: "FAST EDIBLE UNIT NOTFACTORY TARGET".into(),
            only_target_category1: "TARGET".into(),
            bad_target_category1: "FACTORY".into(),
            ..UnitDef::default()
        };
        let socket = UnitDef {
            category: "EDIBLE FACTORY TARGET".into(),
            ..UnitDef::default()
        };
        let dos = UnitDef {
            category: "EDIBLE UNIT NOTFACTORY TARGET".into(),
            only_target_category1: "UNIT".into(),
            bad_target_category1: "FAST".into(),
            ..UnitDef::default()
        };
        defs.units.insert("bit".into(), bit);
        defs.units.insert("socket".into(), socket);
        defs.units.insert("dos".into(), dos);
        UnitRegistry::for_test(defs)
    }

    /// Per-kind flags read from the real FBIs: only `logic_bomb.fbi`
    /// declares `UnitRestricted` (64) and `Init_Cloaked`.
    #[test]
    fn team_limit_and_init_cloaked_come_from_the_logic_bomb_fbi() {
        let reg = UnitRegistry::load();
        for &kind in ALL_UNIT_KINDS {
            let bomb = kind == UnitKind::LogicBomb;
            assert_eq!(reg.team_limit(kind), bomb.then_some(64), "{kind:?}");
            assert_eq!(reg.init_cloaked(kind), bomb, "{kind:?}");
        }
    }

    /// Upstream `BadTargetCategory1=FACTORY` (bit.fbi): a Socket stays
    /// auto-targetable for a Bit — the engine demotes bad candidates
    /// behind ordinary ones rather than skipping them — while a Bit
    /// remains an ordinary (non-bad) auto-target for another Bit.
    #[test]
    fn bit_ranks_factories_as_bad_targets() {
        let reg = target_registry();
        assert!(reg.auto_target_allowed(UnitKind::Bit, UnitKind::Socket));
        assert!(reg.bad_target(UnitKind::Bit, UnitKind::Socket));
        assert!(reg.auto_target_allowed(UnitKind::Bit, UnitKind::Bit));
        assert!(!reg.bad_target(UnitKind::Bit, UnitKind::Bit));
    }

    /// Upstream `OnlyTargetCategory1=UNIT` (dos.fbi): the DOS beam can
    /// never target buildings — not even on a manual order.
    #[test]
    fn dos_cannot_attack_buildings_even_manually() {
        let reg = target_registry();
        assert!(!reg.can_attack(UnitKind::Dos, UnitKind::Socket));
        assert!(reg.can_attack(UnitKind::Dos, UnitKind::Bit));
        // BadTarget=FAST (dos.fbi) neither blocks manual orders nor
        // auto-acquisition: Bits are picked, just ranked behind any
        // non-FAST candidate in range.
        assert!(reg.auto_target_allowed(UnitKind::Dos, UnitKind::Bit));
        assert!(reg.can_attack(UnitKind::Dos, UnitKind::Bit));
        assert!(reg.bad_target(UnitKind::Dos, UnitKind::Bit));
    }

    /// `BadTargetCategory1` demotes a candidate in auto-acquisition but
    /// never blocks a manual order (bit.fbi vs a Socket).
    #[test]
    fn bad_target_still_allows_manual_orders() {
        let reg = target_registry();
        assert!(reg.can_attack(UnitKind::Bit, UnitKind::Socket));
    }

    /// Registry entries without category fields (tests, unloaded kinds)
    /// leave targeting unfiltered.
    #[test]
    fn missing_categories_keep_targeting_open() {
        let reg = target_registry();
        // Kernel is not in the fixture registry at all.
        assert!(reg.auto_target_allowed(UnitKind::Kernel, UnitKind::Bit));
        assert!(reg.can_attack(UnitKind::Kernel, UnitKind::Socket));
        // `UnitRegistry::empty()` behaves the same way for every pair.
        let empty = UnitRegistry::empty();
        assert!(empty.auto_target_allowed(UnitKind::Bit, UnitKind::Socket));
        assert!(empty.can_attack(UnitKind::Dos, UnitKind::Socket));
    }
}

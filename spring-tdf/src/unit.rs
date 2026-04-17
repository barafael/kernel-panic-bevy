//! Typed unit definitions extracted from parsed TDF (FBI) files.
//!
//! Spring's `.fbi` files are standard TDF format with a single `[UNITINFO]`
//! section containing all key-value pairs for one unit.

use std::collections::BTreeMap;

use crate::Section;

/// All unit definitions loaded from FBI files, keyed by `unitname` (lowercased).
#[derive(Debug, Clone, Default)]
pub struct UnitDefs {
    pub units: BTreeMap<String, UnitDef>,
}

/// A single unit definition parsed from an FBI file.
///
/// Fields that don't appear in the TDF default to `0.0` / `false` / `""`,
/// except `damage_modifier` which defaults to 1.0 (no scaling).
#[derive(Debug, Clone, better_default::Default)]
pub struct UnitDef {
    /// Display name from `Name=`.
    pub name: String,
    /// Internal identifier from `Unitname=` (lowercased).
    pub id: String,
    /// Text description.
    pub description: String,
    /// Faction code from `Side=` (e.g. "CPU", "ERROR", "NET").
    pub side: String,

    // --- Health & build ---
    /// Maximum hit points (`MaxDamage=`).
    pub max_health: f32,
    /// Build cost in metal (`BuildCostMetal=`).
    pub build_cost_metal: f32,
    /// Build time in Spring ticks (`BuildTime=`).
    pub build_time: f32,

    // --- Movement ---
    /// Maximum velocity in Spring units (elmos per frame, ~30fps).
    pub max_velocity: f32,
    /// Can this unit move? (`CanMove=1`).
    pub can_move: bool,
    /// Movement class name (e.g. "LIGHT", "MEDIUM", "HEAVY").
    pub movement_class: String,
    /// Acceleration rate.
    pub acceleration: f32,
    /// Brake rate.
    pub brake_rate: f32,
    /// Turn rate (degrees/sec).
    pub turn_rate: f32,

    // --- Size ---
    /// Pathfinding footprint X (in spring map squares).
    pub footprint_x: f32,
    /// Pathfinding footprint Z (in spring map squares).
    pub footprint_z: f32,

    // --- Model ---
    /// S3O model filename (`ObjectName=`).
    pub object_name: String,
    /// Build preview bitmap filename from `BuildPic=` (e.g. "bit.pcx", "network_big.png").
    /// Empty if the FBI has no `BuildPic` field.
    pub build_pic: String,

    // --- Weapons ---
    /// Primary weapon TDF section name (`Weapon1=`).
    pub weapon1: String,
    /// Secondary weapon TDF section name (`Weapon2=`).
    pub weapon2: String,
    /// Third weapon (`Weapon3=`).
    pub weapon3: String,

    // --- Abilities ---
    /// Is a builder/constructor (`Builder=1`).
    pub builder: bool,
    /// Is a commander unit (`Commander=1`).
    pub commander: bool,
    /// Can attack (`CanAttack=1`).
    pub can_attack: bool,
    /// Worker construction speed (`WorkerTime=`).
    pub worker_time: f32,
    /// Build distance for constructors.
    pub build_distance: f32,
    /// Damage modifier (multiplier on incoming damage).
    #[default(1.0)]
    pub damage_modifier: f32,
    /// Is a kamikaze / suicide unit.
    pub kamikaze: bool,
    /// Proximity trigger distance in elmos for kamikaze units.
    pub kamikaze_distance: f32,

    // --- Sight ---
    /// Line-of-sight range (`SightDistance=`).
    pub sight_distance: f32,
    /// Radar range (`RadarDistance=`).
    pub radar_distance: f32,
    /// Seismic detection range.
    pub seismic_distance: f32,

    // --- Categories ---
    /// Unit categories, space-separated (e.g. "FAST EDIBLE UNIT TARGET").
    pub category: String,

    // --- Death ---
    /// Explosion type on death (`ExplodeAs=`).
    pub explode_as: String,
    /// Explosion type on self-destruct.
    pub self_destruct_as: String,

    // --- Misc ---
    /// Can fly (`canFly=1`).
    pub can_fly: bool,
    /// Cruise altitude for flying units.
    pub cruise_alt: f32,
    /// Auto-heal rate when idle (HP/sec).
    pub idle_auto_heal: f32,
    /// Seconds before idle auto-heal kicks in.
    pub idle_time: f32,
    /// Max terrain slope for building placement.
    pub max_slope: f32,
    /// Initial cloaked state.
    pub init_cloaked: bool,
}

impl UnitDefs {
    /// Extract unit definitions from parsed TDF files.
    ///
    /// FBI files have a single `[UNITINFO]` section. This method handles both
    /// that case and multi-section files (unlikely but harmless).
    pub fn from_tdf(tdf: &crate::Tdf) -> Self {
        let mut units = BTreeMap::new();

        for section in &tdf.sections {
            let def = UnitDef::from_section(section);
            if !def.id.is_empty() {
                units.insert(def.id.clone(), def);
            }
        }

        Self { units }
    }

    /// Look up a unit by its `unitname` (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&UnitDef> {
        self.units.get(&name.to_ascii_lowercase())
    }

    /// Merge another set of definitions into this one.
    pub fn merge(&mut self, other: UnitDefs) {
        self.units.extend(other.units);
    }
}

impl UnitDef {
    fn from_section(s: &Section) -> Self {
        let id = s.string("unitname").to_ascii_lowercase();

        Self {
            name: s.string("name"),
            id,
            description: s.string("description"),
            side: s.string("side"),

            max_health: s.f32("maxdamage"),
            build_cost_metal: s.f32("buildcostmetal"),
            build_time: s.f32("buildtime"),

            max_velocity: s.f32("maxvelocity"),
            can_move: s.bool("canmove"),
            movement_class: s.string("movementclass"),
            acceleration: s.f32("acceleration"),
            brake_rate: s.f32("brakerate"),
            turn_rate: s.f32("turnrate"),

            footprint_x: s.f32("footprintx"),
            footprint_z: s.f32("footprintz"),

            object_name: s.string("objectname"),
            build_pic: s.string("buildpic"),

            weapon1: s.string_clean("weapon1"),
            weapon2: s.string_clean("weapon2"),
            weapon3: s.string_clean("weapon3"),

            builder: s.bool("builder"),
            commander: s.bool("commander"),
            can_attack: s.bool("canattack"),
            worker_time: s.f32("workertime"),
            build_distance: s.f32("builddistance"),
            damage_modifier: {
                let v = s.f32("damagemodifier");
                if v == 0.0 { 1.0 } else { v }
            },
            kamikaze: s.bool("kamikaze"),
            kamikaze_distance: s.f32("kamikazedistance"),

            sight_distance: s.f32("sightdistance"),
            radar_distance: s.f32("radardistance"),
            seismic_distance: s.f32("seismicdistance"),

            category: s.string("category"),

            explode_as: s.string("explodeas"),
            self_destruct_as: s.string("selfdestructas"),

            can_fly: s.bool("canfly"),
            cruise_alt: s.f32("cruisealt"),
            idle_auto_heal: s.f32("idleautoheal"),
            idle_time: s.f32("idletime"),
            max_slope: s.f32("maxslope"),
            init_cloaked: s.bool("init_cloaked"),
        }
    }
}

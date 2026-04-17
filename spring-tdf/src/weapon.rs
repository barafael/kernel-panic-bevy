//! Typed weapon definitions extracted from a parsed TDF tree.

use std::collections::BTreeMap;

use crate::Section;

/// All weapon definitions from one or more TDF files.
#[derive(Debug, Clone, Default)]
pub struct WeaponDefs {
    pub weapons: BTreeMap<String, WeaponDef>,
}

/// A single weapon definition with the most commonly used fields.
///
/// Fields that don't appear in the TDF default to `0.0` / `false` / `""`.
#[derive(Debug, Clone, Default)]
pub struct WeaponDef {
    /// Internal identifier (the section name, e.g. "Rock", "BugShot").
    pub id: String,
    /// Display name from `name=`.
    pub name: String,

    // --- Rendering ---
    pub render_type: f32,
    pub rgb_color: [f32; 3],
    pub thickness: f32,
    pub core_thickness: f32,
    pub intensity: f32,
    pub size: f32,
    pub texture1: String,
    pub texture2: String,
    pub model: String,

    // --- Weapon type flags ---
    pub weapon_type: String,
    pub beam_weapon: bool,
    pub beam_laser: bool,
    pub large_beam_laser: bool,
    pub beam_burst: bool,
    pub ballistic: bool,
    pub is_shield: bool,
    pub paralyzer: bool,

    // --- Beam timing ---
    pub duration: f32,
    pub beam_time: f32,
    pub beam_ttl: f32,
    pub beam_decay: f32,

    // --- Ballistics ---
    pub weapon_velocity: f32,
    pub start_velocity: f32,
    pub weapon_acceleration: f32,
    pub my_gravity: f32,
    pub trajectory_height: f32,
    pub turn_rate: f32,
    pub flight_time: f32,

    // --- Combat ---
    pub turret: bool,
    pub range: f32,
    pub reload_time: f32,
    pub area_of_effect: f32,
    pub edge_effectiveness: f32,
    pub tolerance: f32,
    pub spray_angle: f32,
    pub burst: f32,
    pub burst_rate: f32,
    pub projectiles: f32,

    // --- Behavior flags ---
    pub line_of_sight: bool,
    pub collide_friendly: bool,
    pub avoid_friendly: bool,
    pub no_self_damage: bool,
    pub command_fire: bool,
    pub smoke_trail: bool,
    pub tracks: bool,

    // --- Effects ---
    pub explosion_generator: String,
    pub ceg_tag: String,
    pub sound_start: String,
    pub sound_hit: String,

    // --- Paralyze ---
    pub paralyze_time: f32,

    // --- Dynamic damage (BugCannon: damage scales with distance) ---
    /// Exponent applied to the normalized range fraction. 0 / absent =
    /// flat damage, 1 = linear.
    pub dyn_damage_exp: f32,
    /// Range over which `dyn_damage_exp` interpolates. When zero, the
    /// weapon's own `range` is used as the denominator.
    pub dyn_damage_range: f32,
    /// When `true` (upstream `dynDamageInverted=1`), farther targets
    /// take *more* damage — the defining Exploit quirk.
    pub dyn_damage_inverted: bool,
    /// Targeting preference: negative values deprioritize close units
    /// (Exploit prefers distant targets).
    pub proximity_priority: f32,

    // --- Damage map ---
    pub damage: DamageMap,
}

/// Damage values keyed by armor type (lowercased).
///
/// `default` is the fallback used when no specific armor type matches.
#[derive(Debug, Clone, Default)]
pub struct DamageMap {
    pub default: f32,
    pub types: BTreeMap<String, f32>,
}

impl WeaponDefs {
    /// Extract weapon definitions from a parsed TDF.
    ///
    /// Each top-level section is treated as one weapon definition.
    pub fn from_tdf(tdf: &crate::Tdf) -> Self {
        let mut weapons = BTreeMap::new();
        for section in &tdf.sections {
            let def = WeaponDef::from_section(section);
            weapons.insert(section.name.clone(), def);
        }
        Self { weapons }
    }

    /// Look up a weapon by its section name (case-sensitive).
    pub fn get(&self, name: &str) -> Option<&WeaponDef> {
        self.weapons.get(name)
    }
}

impl DamageMap {
    /// Get damage for a specific armor type, falling back to `default`.
    ///
    /// `armor_type` must be lowercase; the TDF parser already lowercases
    /// keys at storage time, so callers should pass canonical keys to
    /// avoid allocating a lowercase copy on the combat hot path.
    pub fn for_type(&self, armor_type: &str) -> f32 {
        self.types.get(armor_type).copied().unwrap_or(self.default)
    }
}

impl WeaponDef {
    /// Multiplier for dynamic-damage weapons (BugCannon) given the
    /// distance from attacker to target. Returns 1.0 for weapons that
    /// don't set `dyn_damage_exp` — i.e. most weapons keep flat damage.
    ///
    /// With `dynDamageInverted=1` (BugCannon), the multiplier is
    /// `(dist / dyn_damage_range)^exp` — farther targets take more.
    /// Otherwise it's `(1 - dist / dyn_damage_range)^exp` — falls off
    /// with distance. Always clamped to [0, 1].
    pub fn dyn_damage_multiplier(&self, dist: f32) -> f32 {
        if self.dyn_damage_exp == 0.0 {
            return 1.0;
        }
        let denom = if self.dyn_damage_range > 0.0 {
            self.dyn_damage_range
        } else if self.range > 0.0 {
            self.range
        } else {
            return 1.0;
        };
        let t = (dist / denom).clamp(0.0, 1.0);
        let base = if self.dyn_damage_inverted { t } else { 1.0 - t };
        base.powf(self.dyn_damage_exp)
    }
}

impl WeaponDef {
    fn from_section(s: &Section) -> Self {
        let damage = s
            .child("DAMAGE")
            .map(|d| {
                let default = d.f32("default");
                let types = d
                    .entries
                    .iter()
                    .filter(|(k, _)| k.as_str() != "default")
                    .map(|(k, v)| (k.clone(), v.trim().parse().unwrap_or(0.0)))
                    .collect();
                DamageMap { default, types }
            })
            .unwrap_or_default();

        Self {
            id: s.name.clone(),
            name: s.string("name"),

            render_type: s.f32("rendertype"),
            rgb_color: s.color3("rgbcolor"),
            thickness: s.f32("thickness"),
            core_thickness: s.f32("corethickness"),
            intensity: s.f32("intensity"),
            size: s.f32("size"),
            texture1: s.string("texture1"),
            texture2: s.string("texture2"),
            model: s.string("model"),

            weapon_type: s.string("weapontype"),
            beam_weapon: s.bool("beamweapon"),
            beam_laser: s.bool("beamlaser"),
            large_beam_laser: s.bool("largebeamlaser"),
            beam_burst: s.bool("beamburst"),
            ballistic: s.bool("ballistic"),
            is_shield: s.bool("isshield"),
            paralyzer: s.bool("paralyzer"),

            duration: s.f32("duration"),
            beam_time: s.f32("beamtime"),
            beam_ttl: s.f32("beamttl"),
            beam_decay: s.f32("beamdecay"),

            weapon_velocity: s.f32("weaponvelocity"),
            start_velocity: s.f32("startvelocity"),
            weapon_acceleration: s.f32("weaponacceleration"),
            my_gravity: s.f32("mygravity"),
            trajectory_height: s.f32("trajectoryheight"),
            turn_rate: s.f32("turnrate"),
            flight_time: s.f32("flighttime"),

            turret: s.bool("turret"),
            range: s.f32("range"),
            reload_time: s.f32("reloadtime"),
            area_of_effect: s.f32("areaofeffect"),
            edge_effectiveness: s.f32("edgeeffectiveness"),
            tolerance: s.f32("tolerance"),
            spray_angle: s.f32("sprayangle"),
            burst: s.f32("burst"),
            burst_rate: s.f32("burstrate"),
            projectiles: s.f32("projectiles"),

            line_of_sight: s.bool("lineofsight"),
            collide_friendly: s.bool("collidefriendly"),
            avoid_friendly: s.bool("avoidfriendly"),
            no_self_damage: s.bool("noselfdamage"),
            command_fire: s.bool("commandfire"),
            smoke_trail: s.bool("smoketrail"),
            tracks: s.bool("tracks"),

            explosion_generator: s.string("explosiongenerator"),
            ceg_tag: s.string("cegtag"),
            sound_start: s.string("soundstart"),
            sound_hit: s.string("soundhit"),

            paralyze_time: s.f32("paralyzetime"),

            dyn_damage_exp: s.f32("dyndamageexp"),
            dyn_damage_range: s.f32("dyndamagerange"),
            dyn_damage_inverted: s.bool("dyndamageinverted"),
            proximity_priority: s.f32("proximitypriority"),

            damage,
        }
    }
}

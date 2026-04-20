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
    /// Legacy palette hue (0-255). When `rgb_color` is unset upstream's
    /// `weapondefs_post.lua` synthesises an `rgbColor` via
    /// [`WeaponDef::palette_rgb`] using `color` as HSV hue and forcing
    /// saturation=1.0 (its `color2` sibling is intentionally ignored —
    /// upstream has a FIXME noting the same). Zero means "palette path
    /// not taken"; for `RetroDeath` the value 40 resolves to a bright
    /// yellow.
    pub color: f32,
    /// Legacy palette saturation (0-255). Kept for round-trip but
    /// ignored by the `hs2rgb` shim per upstream.
    pub color2: f32,
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
    /// Texture-UV scroll speed in texels/second along the beam. Used by
    /// the DOS_Beam so the `dosray` 0/1 texture visibly streams along the
    /// beam instead of sitting still. 0 = no scrolling.
    pub scroll_speed: f32,
    /// Radius (in thickness-multiples) of the end-flare drawn at the
    /// beam origin for `BeamLaser` weapons. 0 = no flare.
    pub laser_flare_size: f32,
    /// When true, a beam-weapon laser that runs out of range stops at
    /// max extent rather than fading; matches upstream `hardstop=1`.
    pub hard_stop: bool,

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

    // --- Shield (when is_shield=true) ---
    pub shield_radius: f32,
    /// Max shield HP. Zero means infinite (upstream homebase convention).
    pub shield_power: f32,
    /// Shield HP regenerated per second. Only consulted when
    /// `shield_power > 0`.
    pub shield_power_regen: f32,

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

/// Normalise a legacy-format RGB triplet into 0-1. Tag authors write
/// either 0-255 or 0-1; the heuristic tests for any channel above 2.0.
fn normalise_legacy_rgb(rgb: [f32; 3]) -> [f32; 3] {
    let [r, g, b] = rgb;
    if r > 2.0 || g > 2.0 || b > 2.0 {
        [r / 255.0, g / 255.0, b / 255.0]
    } else {
        [r, g, b]
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod shim_tests {
    use super::*;
    use crate::Tdf;

    fn parse(src: &str, name: &str) -> WeaponDef {
        let tdf = Tdf::parse(src).unwrap();
        let defs = WeaponDefs::from_tdf(&tdf);
        defs.get(name).cloned().expect("section")
    }

    #[test]
    fn explicit_weapon_type_wins() {
        let w = parse("[W]\n{\nweapontype=BeamLaser;\nbeamweapon=1;\n}", "W");
        // Even though beamweapon=1 would map to LaserCannon in the
        // legacy shim, the explicit weaponType takes precedence.
        assert_eq!(w.category(), WeaponCategory::BeamLaser);
    }

    #[test]
    fn bit_line_becomes_laser_cannon() {
        // Verbatim-ish `Line` (Bit): `beamweapon=1 lineofsight=1` with
        // no literal `weaponType=`. Must resolve to LaserCannon so
        // weapon_fx spawns a traveling bolt, not a hitscan beam.
        let w = parse(
            "[Line]\n{\nbeamweapon=1;\nlineofsight=1;\nthickness=4;\n}",
            "Line",
        );
        assert_eq!(w.category(), WeaponCategory::LaserCannon);
        assert!(!w.is_projectile());
    }

    #[test]
    fn byte_megabeam_becomes_laser_cannon() {
        let w = parse(
            "[MegaBeam]\n{\nbeamweapon=1;\nlineofsight=1;\nburst=4;\n}",
            "MegaBeam",
        );
        assert_eq!(w.category(), WeaponCategory::LaserCannon);
    }

    #[test]
    fn pointer_geometric_becomes_missile_launcher() {
        // `smoketrail=1` with `lineofsight=1` → MissileLauncher,
        // regardless of whether a `model=` is set. The model-check in
        // `is_projectile` is a secondary guard.
        let w = parse(
            "[Geo]\n{\nsmoketrail=1;\nlineofsight=1;\nmodel=octashot.s3o;\ntracks=1;\n}",
            "Geo",
        );
        assert_eq!(w.category(), WeaponCategory::MissileLauncher);
        assert!(w.is_projectile());
    }

    #[test]
    fn build_laser_becomes_beam_laser() {
        let w = parse(
            "[BuildLaser]\n{\nbeamlaser=1;\nbeamtime=0.06;\n}",
            "BuildLaser",
        );
        assert_eq!(w.category(), WeaponCategory::BeamLaser);
    }

    #[test]
    fn mine_launcher_honors_explicit_laser_cannon() {
        // MineLauncher: `WeaponType=LaserCannon; ballistic=1;`. The
        // explicit weaponType keeps it a LaserCannon; ballistic makes
        // `is_projectile()` true separately (gravity-affected bolt).
        let w = parse(
            "[M]\n{\nweapontype=LaserCannon;\nballistic=1;\nmygravity=.4;\n}",
            "M",
        );
        assert_eq!(w.category(), WeaponCategory::LaserCannon);
        assert!(w.is_projectile());
    }

    #[test]
    fn sigterm_becomes_aircraft_bomb() {
        let w = parse(
            "[Sig]\n{\nweapontype=AircraftBomb;\nmodel=sigterm.s3o;\n}",
            "Sig",
        );
        assert_eq!(w.category(), WeaponCategory::AircraftBomb);
    }

    #[test]
    fn shield_flag_wins_over_everything() {
        // Weapons marked `isshield=1` must resolve to Shield even if
        // they also carry `beamweapon=1` etc — those flags describe
        // the projectile template the shield uses on collision.
        let w = parse("[S]\n{\nisshield=1;\nbeamweapon=1;\n}", "S");
        assert_eq!(w.category(), WeaponCategory::Shield);
    }

    #[test]
    fn pure_cannon_fallback() {
        // Nothing set → Cannon (upstream default in weapondefs_post).
        let w = parse("[C]\n{\n}", "C");
        assert_eq!(w.category(), WeaponCategory::Cannon);
    }

    // Palette resolution -------------------------------------------------

    fn rgb_close(a: [f32; 3], b: [f32; 3]) {
        for i in 0..3 {
            assert!(
                (a[i] - b[i]).abs() < 1e-2,
                "channel {i}: got {} expected {}",
                a[i],
                b[i],
            );
        }
    }

    #[test]
    fn retrodeath_color_40_synthesises_yellow() {
        // Upstream `RetroDeath` authors only `color=40` (hue ≈
        // 40/255 = 0.157, which is in the first 1/6 of the wheel).
        // The hs2rgb shim produces r=1, g=40/255*6 ≈ 0.94, b=0.
        let w = parse("[RD]\n{\nbeamweapon=1;\nlineofsight=1;\ncolor=40;\n}", "RD");
        let rgb = w.palette_rgb().expect("color=40 must synth");
        rgb_close(rgb, [1.0, 0.941, 0.0]);
        // `resolved_rgb` must prefer the palette over white.
        rgb_close(w.resolved_rgb(), [1.0, 0.941, 0.0]);
    }

    #[test]
    fn palette_hue_high_bumps_by_0_1() {
        // Upstream applies a `+0.1` bump when hue > 0.5. At `color=128`
        // (hue ≈ 0.5019), the bump pushes hue into the 0.6+ range,
        // which lands in the 2/3..5/6 segment → blue-heavy.
        let w = parse("[P]\n{\nbeamweapon=1;\nlineofsight=1;\ncolor=128;\n}", "P");
        let rgb = w.palette_rgb().expect("synth");
        assert!(rgb[2] > 0.5, "expected blue-dominant, got {rgb:?}");
    }

    #[test]
    fn palette_zero_color_returns_none() {
        let w = parse("[P]\n{\nbeamweapon=1;\nlineofsight=1;\n}", "P");
        assert!(w.palette_rgb().is_none());
    }

    #[test]
    fn resolved_rgb_prefers_rgb_color_over_palette() {
        let w = parse(
            "[P]\n{\nbeamweapon=1;\nlineofsight=1;\ncolor=40;\nrgbcolor=0.1 0.2 0.3;\n}",
            "P",
        );
        rgb_close(w.resolved_rgb(), [0.1, 0.2, 0.3]);
    }

    #[test]
    fn resolved_rgb_normalises_0_255_rgb() {
        let w = parse("[P]\n{\nrgbcolor=255 128 64;\n}", "P");
        rgb_close(w.resolved_rgb(), [1.0, 0.502, 0.251]);
    }

    #[test]
    fn resolved_rgb_cannon_default_is_orange() {
        // No rgbColor, no color palette → Cannon default (1.0, 0.5, 0.0).
        let w = parse("[P]\n{\n}", "P");
        rgb_close(w.resolved_rgb(), [1.0, 0.5, 0.0]);
    }
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

/// Canonical classification of a weapon's rendering / behaviour
/// category.
///
/// Upstream KP almost never sets `weaponType=` literally — the
/// category comes from the **legacy tag shim** implemented in
/// `cont/base/springcontent/gamedata/weapondefs_post.lua`, which maps
/// old-style flags (`beamweapon=1`, `smoketrail=1`, `ballistic=1`,
/// `beamlaser=1`, etc.) onto the modern weapon types. See
/// [`WeaponDef::derived_weapon_type`] for the exact rules. Two
/// specimens illustrate why the shim matters:
///
/// - `Line` (Bit's weapon) sets `beamweapon=1 lineofsight=1` and
///   *nothing else* → [`LaserCannon`] (a traveling finite-length
///   bolt, NOT a hitscan beam). Our fx side must spawn a `LaserBolt`.
/// - `Geometric` (Pointer) sets `smoketrail=1 lineofsight=1 model=…`
///   → [`MissileLauncher`]. FX side must render the `.s3o` model
///   along an arcing homing path, not a beam.
///
/// Without the shim both land in [`Other`] and fx routing picks the
/// wrong path.
///
/// [`LaserCannon`]: Self::LaserCannon
/// [`MissileLauncher`]: Self::MissileLauncher
/// [`Other`]: Self::Other
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeaponCategory {
    Melee,
    Shield,
    /// Instant hitscan beam (Spring `BeamLaser`). Drawn as a static
    /// quad from attacker to target for `beamTime` seconds.
    BeamLaser,
    /// Finite-length traveling bolt (Spring `LaserCannon`). Visual
    /// length = `duration * weaponVelocity`. Covers Bit's `Line`,
    /// Byte's `MegaBeam`, the `RetroDeath*` family, and the
    /// `MineLauncher`.
    LaserCannon,
    MissileLauncher,
    StarburstLauncher,
    /// Hitscan lightning ribbon with wobble.
    LightningCannon,
    /// Spray weapon (flamer).
    Flame,
    /// Multi-bullet plasma-like.
    EmgCannon,
    /// Ballistic sprite / model projectile that falls under gravity.
    Cannon,
    AircraftBomb,
    /// Tag present but not one we recognise — fx falls back to a
    /// flat untextured beam. Getting here for a KP weapon is a bug.
    Other,
}

impl WeaponDef {
    /// Classify this weapon.
    ///
    /// Honours a literal `weaponType=` if it was set, otherwise runs
    /// the legacy-tag shim documented on [`WeaponCategory`]. Called
    /// instead of matching raw strings so every fx/combat call site
    /// stays in sync when the shim gains a new case.
    pub fn category(&self) -> WeaponCategory {
        self.derived_weapon_type()
    }

    /// Resolve the effective Spring weapon-type, running the same
    /// compatibility mapping as
    /// `cont/base/springcontent/gamedata/weapondefs_post.lua` in the
    /// upstream engine. A literal `weaponType=` in the TDF takes
    /// precedence; otherwise the legacy flag set determines the
    /// category.
    pub fn derived_weapon_type(&self) -> WeaponCategory {
        // 1. Explicit `weaponType=...` wins.
        if !self.weapon_type.is_empty() {
            return match self.weapon_type.as_str() {
                "Melee" => WeaponCategory::Melee,
                "Shield" => WeaponCategory::Shield,
                "BeamLaser" => WeaponCategory::BeamLaser,
                "LaserCannon" => WeaponCategory::LaserCannon,
                "MissileLauncher" => WeaponCategory::MissileLauncher,
                "StarburstLauncher" => WeaponCategory::StarburstLauncher,
                "LightningCannon" => WeaponCategory::LightningCannon,
                "Flame" => WeaponCategory::Flame,
                "EmgCannon" => WeaponCategory::EmgCannon,
                "Cannon" => WeaponCategory::Cannon,
                "AircraftBomb" => WeaponCategory::AircraftBomb,
                "TorpedoLauncher" | "Rifle" | "DGun" => WeaponCategory::Other,
                _ => WeaponCategory::Other,
            };
        }

        // 2. Legacy flags — order matters; matches weapondefs_post.lua.
        if self.is_shield {
            return WeaponCategory::Shield;
        }
        if self.beam_laser {
            return WeaponCategory::BeamLaser;
        }

        if self.line_of_sight {
            // `rendertype=7` is the old lightning cannon marker;
            // we don't currently parse that field, and KP never sets
            // it anyway, so the simple fallthrough below suffices.
            if self.beam_weapon {
                return WeaponCategory::LaserCannon;
            }
            if self.smoke_trail {
                return WeaponCategory::MissileLauncher;
            }
            return WeaponCategory::Cannon;
        }

        WeaponCategory::Cannon
    }

    /// True for any weapon whose projectile class launches an actual
    /// model or ballistic shell, rather than a hitscan beam or short
    /// traveling bolt. Used by weapon-fx to decide whether to spawn a
    /// moving `ProjectileVisual` vs a beam ribbon.
    ///
    /// `LaserCannon` is NOT a projectile in this sense even though
    /// it travels — it renders as a flat stretched ribbon via
    /// [`crate::WeaponCategory::LaserCannon`], not a 3D model shell.
    pub fn is_projectile(&self) -> bool {
        self.ballistic
            || matches!(
                self.category(),
                WeaponCategory::MissileLauncher
                    | WeaponCategory::StarburstLauncher
                    | WeaponCategory::Cannon
                    | WeaponCategory::AircraftBomb,
            )
            || (!self.model.is_empty() && self.model != ";")
    }

    /// Synthesise an RGB triplet from the legacy `color=` palette
    /// field (0-255 hue) when `rgb_color` is unset.
    ///
    /// Mirrors `hs2rgb(color/255, _)` from
    /// `cont/base/springcontent/gamedata/weapondefs_post.lua` verbatim
    /// — saturation is clamped to 1.0 internally (the upstream author
    /// left a `FIXME? ignores saturation completely` comment), so
    /// `color2` never affects the result. Hue values above 0.5 get a
    /// +0.1 bump per the upstream formula.
    ///
    /// Returns `None` when `color` is zero or negative — the caller
    /// should then stay with the authored `rgb_color` (or the
    /// type-aware default).
    pub fn palette_rgb(&self) -> Option<[f32; 3]> {
        if self.color <= 0.0 {
            return None;
        }
        let mut h = self.color / 255.0;
        // Per upstream: a bump for the second-half of the wheel.
        if h > 0.5 {
            h += 0.1;
            if h > 1.0 {
                h -= 1.0;
            }
        }
        // Saturation forced to 1, as in upstream's FIXME'd shim.
        let s = 1.0_f32;
        let inv_sat = 1.0 - s;

        let mut r = inv_sat / 2.0;
        let mut g = inv_sat / 2.0;
        let mut b = inv_sat / 2.0;

        const T1: f32 = 1.0 / 6.0;
        const T2: f32 = 1.0 / 3.0;
        const T3: f32 = 1.0 / 2.0;
        const T4: f32 = 2.0 / 3.0;
        const T5: f32 = 5.0 / 6.0;

        if h < T1 {
            r += s;
            g += s * (h * 6.0);
        } else if h < T2 {
            g += s;
            r += s * ((T2 - h) * 6.0);
        } else if h < T3 {
            g += s;
            b += s * ((h - T2) * 6.0);
        } else if h < T4 {
            b += s;
            g += s * ((T4 - h) * 6.0);
        } else if h < T5 {
            b += s;
            r += s * ((h - T4) * 6.0);
        } else {
            r += s;
            b += s * ((1.0 - h) * 6.0);
        }

        Some([r, g, b])
    }

    /// Resolve the effective edge-tint RGB (0-1) for this weapon,
    /// applying the same three-tier cascade upstream does: explicit
    /// `rgbColor` → synthesised from `color=` palette → per-category
    /// default (Cannon orange, EmgCannon yellow, others white). All
    /// weapon-fx call sites should go through this helper instead of
    /// reading `rgb_color` directly.
    pub fn resolved_rgb(&self) -> [f32; 3] {
        let [r, g, b] = self.rgb_color;
        if !(r == 0.0 && g == 0.0 && b == 0.0) {
            return normalise_legacy_rgb([r, g, b]);
        }
        if let Some(rgb) = self.palette_rgb() {
            return rgb;
        }
        match self.category() {
            WeaponCategory::Cannon => [1.0, 0.5, 0.0],
            WeaponCategory::EmgCannon => [0.9, 0.9, 0.2],
            _ => [1.0, 1.0, 1.0],
        }
    }

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
            color: s.f32("color"),
            color2: s.f32("color2"),
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
            scroll_speed: s.f32("scrollspeed"),
            laser_flare_size: s.f32("laserflaresize"),
            hard_stop: s.bool("hardstop"),

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

            shield_radius: s.f32("shieldradius"),
            shield_power: s.f32("shieldpower"),
            shield_power_regen: s.f32("shieldpowerregen"),

            dyn_damage_exp: s.f32("dyndamageexp"),
            dyn_damage_range: s.f32("dyndamagerange"),
            dyn_damage_inverted: s.bool("dyndamageinverted"),
            proximity_priority: s.f32("proximitypriority"),

            damage,
        }
    }
}

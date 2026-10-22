//! Typed explosion / CEG (Custom Explosion Generator) definitions from TDF files.
//!
//! Spring weapons reference particle scripts via
//! `explosiongenerator=custom:NAME` (or `cegTag=NAME`), which resolves to a
//! section in `gamedata/explosions/*.tdf` describing one or more effect
//! layers (particle emitters, bitmap flames, ground flashes, or recursive
//! spawners).
//!
//! ```text
//! [corruption_burst]
//! {
//!     [burst]
//!     {
//!         class=CSimpleParticleSystem;
//!         [properties] { ... }
//!         air=1;
//!         ground=1;
//!     }
//!     [groundflash]
//!     {
//!         flashSize=16;
//!         ttl=8;
//!         color=1,0.6,0.6;
//!     }
//! }
//! ```
//!
//! ## CEG expression grammar
//!
//! Numeric properties are not plain floats — they are tiny stack-machine
//! expressions. `rts/Sim/Projectiles/ExplosionGenerator.cpp` parses each
//! value as a sequence of operations (see [`CegOp`]):
//!
//! | Token         | Meaning                                                    |
//! | ------------- | ---------------------------------------------------------- |
//! | `123.5`       | literal — add to accumulator                               |
//! | `rN`          | uniform random in `[0, N)` — added                         |
//! | `iN`          | per-spawn index × N (repeat counter; `count=240` ticks it) |
//! | `dN`          | damage × N — multiplied by the owning weapon's damage      |
//! | `mN kN sN pN` | sawtooth / discrete / sine / pow                           |
//! | `xN yN aN qN` | value-buffer ops                                           |
//!
//! Concatenated forms (`110d-3`, `12 d-.5`) are both legal: the parser
//! splits at every opcode character or whitespace boundary.
//!
//! This crate parses the programs and keeps them as written. Running
//! them is the game's business (`kernel-panic`'s `ceg_def`), which
//! chooses the op set it models; the last two rows reach it as
//! [`CegOp::Other`].

use spring_detmath::DetFloat;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Section;

// ── Typed values ────────────────────────────────────────────────────

/// A parsed CEG expression. See the module docs for the grammar.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CegExpr(pub Vec<CegOp>);

/// One operation in a [`CegExpr`] program.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CegOp {
    /// Literal add.
    Add(f32),
    /// Uniform random in `[0, v)`.
    Rand(f32),
    /// Per-spawn index × v — steps by `v` each particle of the burst.
    Index(f32),
    /// `damage × v` — weapon damage scales the contribution.
    Damage(f32),
    /// Every other opcode, kept as written (the `char` is the opcode
    /// letter, the `f32` its operand) for the consumer to model or
    /// refuse — the game models `y` and `x` and refuses the rest.
    Other(char, f32),
}

impl CegExpr {
    /// Parse a CEG expression from its raw string. Invalid opcodes are
    /// silently skipped, matching Spring's parser which only logs a
    /// warning and continues (see `ParseExplosionCode`).
    pub fn parse(src: &str) -> Self {
        let mut ops = Vec::new();
        let bytes = src.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i] as char;
            if c.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            // Literal number (possibly signed). No opcode prefix.
            if (c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
                && let Some((val, consumed)) = parse_float(&src[i..])
            {
                ops.push(CegOp::Add(val));
                i += consumed;
                continue;
            }
            // Opcode + operand.
            match c {
                'r' | 'i' | 'd' | 'm' | 'k' | 's' | 'p' | 'x' | 'y' | 'a' | 'q' => {
                    let tail = &src[i + 1..];
                    if let Some((val, consumed)) = parse_float(tail) {
                        let op = match c {
                            'r' => CegOp::Rand(val),
                            'i' => CegOp::Index(val),
                            'd' => CegOp::Damage(val),
                            other => CegOp::Other(other, val),
                        };
                        ops.push(op);
                        i += 1 + consumed;
                        continue;
                    }
                    // No operand consumed — skip the lone opcode char
                    // rather than looping forever.
                    i += 1;
                }
                _ => {
                    // Unknown character; mirror upstream by logging-and-
                    // continuing silently.
                    i += 1;
                }
            }
        }
        Self(ops)
    }
}

/// Read a leading float from `s` and return `(value, bytes_consumed)`.
fn parse_float(s: &str) -> Option<(f32, usize)> {
    let bytes = s.as_bytes();
    let mut end = 0;
    // Optional sign.
    if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
        end += 1;
    }
    let digits_start = end;
    while end < bytes.len()
        && (bytes[end].is_ascii_digit()
            || bytes[end] == b'.'
            || bytes[end] == b'e'
            || bytes[end] == b'E')
    {
        // Allow a sign after an exponent marker.
        if (bytes[end] == b'e' || bytes[end] == b'E')
            && end + 1 < bytes.len()
            && (bytes[end + 1] == b'-' || bytes[end + 1] == b'+')
        {
            end += 2;
            continue;
        }
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    let slice = &s[..end];
    slice.parse::<f32>().ok().map(|v| (v, end))
}

/// A [`CegExpr`] per axis (`x, y, z`). Comma-separated in the source.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CegVec3 {
    pub x: CegExpr,
    pub y: CegExpr,
    pub z: CegExpr,
}

impl CegVec3 {
    /// Parse `"x, y, z"` (whitespace-flexible). Missing axes default to
    /// the zero expression.
    pub fn parse(src: &str) -> Self {
        let mut parts = src.split(',');
        let x = CegExpr::parse(parts.next().unwrap_or("").trim());
        let y = CegExpr::parse(parts.next().unwrap_or("").trim());
        let z = CegExpr::parse(parts.next().unwrap_or("").trim());
        Self { x, y, z }
    }
}

/// Emitter orientation: either a fixed axis or the keyword `dir` which
/// the engine replaces with the owning weapon's firing direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EmitVector {
    /// Use the weapon's firing direction at spawn time.
    Direction,
    /// A literal axis (may still contain spread tokens).
    Literal(CegVec3),
}

impl Default for EmitVector {
    fn default() -> Self {
        // Upstream default: straight up.
        Self::Literal(CegVec3 {
            x: CegExpr::default(),
            y: CegExpr(vec![CegOp::Add(1.0)]),
            z: CegExpr::default(),
        })
    }
}

impl EmitVector {
    pub fn parse(src: &str) -> Self {
        if src.trim().eq_ignore_ascii_case("dir") {
            Self::Direction
        } else {
            Self::Literal(CegVec3::parse(src))
        }
    }
}

/// A gradient of RGBA stops sampled across a particle's lifetime.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColorMap {
    pub stops: Vec<[f32; 4]>,
}

impl ColorMap {
    /// Parse `"r g b a   r g b a   ..."` (whitespace-separated floats in
    /// groups of four; any trailing partial group is dropped).
    pub fn parse(src: &str) -> Self {
        let floats: Vec<f32> = src
            .split_ascii_whitespace()
            .filter_map(|tok| tok.parse::<f32>().ok())
            .collect();
        let stops = floats
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        Self { stops }
    }
}

// ── Aggregate tree ──────────────────────────────────────────────────

/// All explosion definitions from one or more TDF files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExplosionDefs {
    pub explosions: BTreeMap<String, ExplosionDef>,
}

/// A single named explosion generator, composed of effect layers.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ExplosionDef {
    /// The explosion name (e.g. "corruption_burst", "oldskool_shot1").
    pub id: String,
    /// Visual effect layers (particle systems, flames, spawners).
    pub effects: Vec<EffectLayer>,
    /// Optional ground flash.
    pub ground_flash: Option<GroundFlash>,
}

/// One visual effect layer within an explosion.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectLayer {
    /// Subsection name (e.g. "burst", "squarecloud", "tracers").
    pub name: String,
    /// Effect class determining the rendering approach.
    pub class: EffectClass,
    /// Whether this effect triggers in air / on ground / in water.
    pub air: bool,
    pub ground: bool,
    pub water: bool,
    /// How many times this effect fires (default 1).
    pub count: u32,
    /// Effect-specific properties.
    pub properties: EffectProperties,
}

/// The rendering class for an effect layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectClass {
    /// Burst of particles with physics (gravity, drag, lifetime).
    SimpleParticleSystem,
    /// Textured beam / shockwave / flame billboard.
    BitmapMuzzleFlame,
    /// Spawns another explosion generator (chaining).
    ExpGenSpawner,
    /// Animated static stars / sparkles (rarely used).
    Stars,
    /// `CExploSpikeProjectile` (`class=explspike`): a camera-facing
    /// streak that grows and fades in a few frames.
    ExploSpike,
    /// Unknown class string (preserved for forward compatibility).
    Other(String),
}

/// Properties for a `CSimpleParticleSystem` effect.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ParticleProperties {
    pub texture: String,
    pub color_map: ColorMap,
    pub num_particles: CegExpr,
    pub particle_life: CegExpr,
    pub particle_life_spread: CegExpr,
    pub particle_speed: CegExpr,
    pub particle_speed_spread: CegExpr,
    pub particle_size: CegExpr,
    pub particle_size_spread: CegExpr,
    pub size_growth: CegExpr,
    pub size_mod: CegExpr,
    pub emit_rot: CegExpr,
    pub emit_rot_spread: CegExpr,
    pub airdrag: CegExpr,
    pub gravity: CegVec3,
    pub pos: CegVec3,
    pub emit_vector: EmitVector,
    pub directional: bool,
    pub always_visible: bool,
}

/// Properties for a `CBitmapMuzzleFlame` effect.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FlameProperties {
    pub side_texture: String,
    pub front_texture: String,
    pub color_map: ColorMap,
    pub size: CegExpr,
    pub length: CegExpr,
    pub size_growth: CegExpr,
    pub ttl: CegExpr,
    pub front_offset: CegExpr,
    pub speed: CegExpr,
    pub dir: CegVec3,
    pub pos: CegVec3,
    pub always_visible: bool,
}

/// Properties for a `CExpGenSpawner` (chains to another explosion).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpawnerProperties {
    /// Name of the nested CEG, without the `custom:` prefix.
    pub explosion_generator: String,
    pub delay: CegExpr,
    pub damage: CegExpr,
    pub dir: CegVec3,
    pub pos: CegVec3,
    pub speed: CegVec3,
    pub always_visible: bool,
}

/// Properties for a `CExploSpikeProjectile` (`explspike`) effect —
/// upstream `ExploSpikeProjectile.cpp`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SpikeProperties {
    pub length: CegExpr,
    pub width: CegExpr,
    pub alpha: CegExpr,
    pub alpha_decay: CegExpr,
    /// Unnormalised streak direction; its length also sets the
    /// per-frame length growth (`Init`: `lengthGrowth = |dir|·(0.5+r·0.4)`).
    pub dir: CegVec3,
    pub pos: CegVec3,
    /// Tint; upstream default `(1, 0.8, 0.5)`.
    pub color: Option<CegVec3>,
}

/// Union of effect-specific properties.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum EffectProperties {
    Particle(ParticleProperties),
    Flame(FlameProperties),
    Spawner(SpawnerProperties),
    Spike(SpikeProperties),
    /// Unknown or unsupported class — raw entries retained for debugging.
    Raw(BTreeMap<String, String>),
}

/// Ground flash effect (simple circle/ring on the terrain).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GroundFlash {
    pub flash_size: f32,
    pub flash_alpha: f32,
    pub circle_growth: f32,
    pub circle_alpha: f32,
    pub ttl: f32,
    pub color: [f32; 3],
}

// ── Parsing ─────────────────────────────────────────────────────────

impl ExplosionDefs {
    /// Extract explosion definitions from a parsed TDF.
    ///
    /// Each top-level section is one named explosion generator.
    pub fn from_tdf(tdf: &crate::Tdf) -> Self {
        let mut explosions = BTreeMap::new();
        for section in &tdf.sections {
            let def = ExplosionDef::from_section(section);
            explosions.insert(section.name.clone(), def);
        }
        Self { explosions }
    }

    /// Look up an explosion by name (case-sensitive, matching TDF convention).
    /// Accepts both the raw name and the `custom:NAME` prefix weapons use.
    pub fn get(&self, name: &str) -> Option<&ExplosionDef> {
        let key = name.strip_prefix("custom:").unwrap_or(name);
        self.explosions.get(key)
    }

    /// Merge another set of definitions into this one.
    pub fn merge(&mut self, other: ExplosionDefs) {
        self.explosions.extend(other.explosions);
    }
}

impl ExplosionDef {
    fn from_section(s: &Section) -> Self {
        let mut effects = Vec::new();
        let mut ground_flash = None;

        for child in &s.children {
            if child.name.eq_ignore_ascii_case("groundflash") {
                ground_flash = Some(GroundFlash::from_section(child));
            } else if let Some(effect) = EffectLayer::from_section(child) {
                effects.push(effect);
            }
        }

        Self {
            id: s.name.clone(),
            effects,
            ground_flash,
        }
    }
}

impl EffectLayer {
    fn from_section(s: &Section) -> Option<Self> {
        // `class=` may live either on the child block itself or inside
        // the nested `[properties]` subsection.
        let class_raw = {
            let outer = s.string("class");
            if !outer.is_empty() {
                outer
            } else {
                s.child("properties")
                    .map(|p| p.string("class"))
                    .unwrap_or_default()
            }
        };
        let class = if class_raw.is_empty() {
            // No class → not an effect layer (likely a [groundflash]
            // subsection handled by the parent).
            return None;
        } else {
            EffectClass::parse(class_raw.trim())
        };

        let props = s.child("properties");

        let properties = match &class {
            EffectClass::SimpleParticleSystem => {
                EffectProperties::Particle(ParticleProperties::from_section(props))
            }
            EffectClass::BitmapMuzzleFlame => {
                EffectProperties::Flame(FlameProperties::from_section(props))
            }
            EffectClass::ExpGenSpawner => {
                EffectProperties::Spawner(SpawnerProperties::from_section(props))
            }
            EffectClass::ExploSpike => {
                EffectProperties::Spike(SpikeProperties::from_section(props))
            }
            EffectClass::Stars | EffectClass::Other(_) => {
                EffectProperties::Raw(props.map(|p| p.entries.clone()).unwrap_or_default())
            }
        };

        Some(Self {
            name: s.name.clone(),
            class,
            air: s.bool("air"),
            ground: s.bool("ground"),
            water: s.bool("water"),
            // `psi.count = max(0, GetInt("count", 1))`
            // (`ExplosionGenerator.cpp:978`): absent → 1, an explicit
            // `count=0` disables the layer (network_flowtrail's Circle).
            count: s.f32_or("count", 1.0).det_max(0.0) as u32,
            properties,
        })
    }
}

impl EffectClass {
    fn parse(s: &str) -> Self {
        match s {
            "CSimpleParticleSystem" => Self::SimpleParticleSystem,
            "CBitmapMuzzleFlame" => Self::BitmapMuzzleFlame,
            "CExpGenSpawner" => Self::ExpGenSpawner,
            "CStars" => Self::Stars,
            "explspike" | "CExploSpikeProjectile" => Self::ExploSpike,
            other => Self::Other(other.to_string()),
        }
    }
}

/// Pull an expression out of a section by key; absent keys parse to an
/// empty expression (which evaluates to 0.0 — matching Spring's default
/// for any unset CEG property).
fn expr(s: &Section, key: &str) -> CegExpr {
    CegExpr::parse(s.get(key).unwrap_or(""))
}

/// Like [`expr`] but with a constant default for keys that are absent —
/// used for `airdrag` (defaults to 1) and `sizemod` (defaults to 1)
/// since Spring treats an unset drag/sizemod as "no change per frame".
fn expr_or(s: &Section, key: &str, default: f32) -> CegExpr {
    match s.get(key) {
        Some(v) if !v.trim().is_empty() => CegExpr::parse(v),
        _ => CegExpr(vec![CegOp::Add(default)]),
    }
}

impl ParticleProperties {
    fn from_section(s: Option<&Section>) -> Self {
        let Some(s) = s else {
            return Self::default();
        };
        Self {
            texture: s.string("texture"),
            color_map: ColorMap::parse(&s.string("colormap")),
            num_particles: expr_or(s, "numparticles", 1.0),
            particle_life: expr(s, "particlelife"),
            particle_life_spread: expr(s, "particlelifespread"),
            particle_speed: expr(s, "particlespeed"),
            particle_speed_spread: expr(s, "particlespeedspread"),
            particle_size: expr(s, "particlesize"),
            particle_size_spread: expr(s, "particlesizespread"),
            size_growth: expr(s, "sizegrowth"),
            size_mod: expr_or(s, "sizemod", 1.0),
            emit_rot: expr(s, "emitrot"),
            emit_rot_spread: expr(s, "emitrotspread"),
            airdrag: expr_or(s, "airdrag", 1.0),
            gravity: CegVec3::parse(&s.string("gravity")),
            pos: CegVec3::parse(&s.string("pos")),
            emit_vector: EmitVector::parse(&s.string("emitvector")),
            directional: s.bool("directional"),
            always_visible: s.bool("alwaysvisible"),
        }
    }
}

impl FlameProperties {
    fn from_section(s: Option<&Section>) -> Self {
        let Some(s) = s else {
            return Self::default();
        };
        Self {
            side_texture: s.string("sidetexture"),
            front_texture: s.string("fronttexture"),
            color_map: ColorMap::parse(&s.string("colormap")),
            size: expr(s, "size"),
            length: expr(s, "length"),
            size_growth: expr(s, "sizegrowth"),
            ttl: expr(s, "ttl"),
            front_offset: expr(s, "frontoffset"),
            speed: expr(s, "speed"),
            dir: CegVec3::parse(&s.string("dir")),
            pos: CegVec3::parse(&s.string("pos")),
            always_visible: s.bool("alwaysvisible"),
        }
    }
}

impl SpikeProperties {
    fn from_section(s: Option<&Section>) -> Self {
        let Some(s) = s else {
            return Self::default();
        };
        let color = s.get("color").map(CegVec3::parse);
        Self {
            length: expr(s, "length"),
            width: expr(s, "width"),
            alpha: expr(s, "alpha"),
            alpha_decay: expr(s, "alphadecay"),
            dir: CegVec3::parse(&s.string("dir")),
            pos: CegVec3::parse(&s.string("pos")),
            color,
        }
    }
}

impl SpawnerProperties {
    fn from_section(s: Option<&Section>) -> Self {
        let Some(s) = s else {
            return Self::default();
        };
        let raw = s.string_clean("explosiongenerator");
        let explosion_generator = raw
            .strip_prefix("custom:")
            .unwrap_or(&raw)
            .trim()
            .to_string();
        Self {
            explosion_generator,
            delay: expr(s, "delay"),
            damage: expr(s, "damage"),
            dir: CegVec3::parse(&s.string("dir")),
            pos: CegVec3::parse(&s.string("pos")),
            speed: CegVec3::parse(&s.string("speed")),
            always_visible: s.bool("alwaysvisible"),
        }
    }
}

impl GroundFlash {
    fn from_section(s: &Section) -> Self {
        Self {
            flash_size: s.f32("flashsize"),
            flash_alpha: s.f32("flashalpha"),
            circle_growth: s.f32("circlegrowth"),
            circle_alpha: s.f32("circlealpha"),
            ttl: s.f32("ttl"),
            color: parse_vec3_plain(&s.string("color")),
        }
    }
}

/// Plain `r, g, b` triplet used only for `GroundFlash::color` where the
/// CEG grammar's expression tokens are not applicable.
fn parse_vec3_plain(src: &str) -> [f32; 3] {
    let parts: Vec<f32> = src
        .split([',', ' '])
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    [
        parts.first().copied().unwrap_or(0.0),
        parts.get(1).copied().unwrap_or(0.0),
        parts.get(2).copied().unwrap_or(0.0),
    ]
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_close(a: [f32; 4], b: [f32; 4]) {
        for i in 0..4 {
            assert!(
                (a[i] - b[i]).abs() < 1e-4,
                "rgba[{i}]: got {} expected {}",
                a[i],
                b[i],
            );
        }
    }

    // CegExpr -----------------------------------------------------------

    use CegOp::{Add, Damage, Index, Other, Rand};

    #[test]
    fn expr_empty_is_an_empty_program() {
        assert_eq!(CegExpr::parse("").0, vec![]);
    }

    #[test]
    fn expr_literal_add() {
        assert_eq!(CegExpr::parse("12.5").0, vec![Add(12.5)]);
    }

    #[test]
    fn expr_signed_literal() {
        assert_eq!(CegExpr::parse("-0.1").0, vec![Add(-0.1)]);
        assert_eq!(CegExpr::parse("+7").0, vec![Add(7.0)]);
    }

    #[test]
    fn expr_rand_token() {
        // `-300 r600`: a literal and a uniform draw in `[0, 600)`.
        assert_eq!(
            CegExpr::parse("-300 r600").0,
            vec![Add(-300.0), Rand(600.0)]
        );
    }

    #[test]
    fn expr_index_token() {
        // `delay=10 i10`: 10 plus 10 per spawn index.
        assert_eq!(CegExpr::parse("10 i10").0, vec![Add(10.0), Index(10.0)]);
    }

    #[test]
    fn expr_damage_token() {
        // `damage=0 i1` steps by the spawn index, not the damage: the
        // d-component needs its own token, as in a standalone `d-3`.
        assert_eq!(CegExpr::parse("0 i1").0, vec![Add(0.0), Index(1.0)]);
        assert_eq!(CegExpr::parse("d-3").0, vec![Damage(-3.0)]);
    }

    #[test]
    fn expr_concatenated_tokens_no_space() {
        // Upstream `particleLife=110d-3` — literal 110 followed by
        // damage-scaled -3 with no intervening whitespace.
        assert_eq!(CegExpr::parse("110d-3").0, vec![Add(110.0), Damage(-3.0)]);
    }

    #[test]
    fn expr_space_separated_damage_token() {
        // Upstream `particleSpeed=12 d-.5` — literal 12 then damage×-0.5.
        assert_eq!(CegExpr::parse("12 d-.5").0, vec![Add(12.0), Damage(-0.5)]);
    }

    #[test]
    fn expr_unknown_opcode_is_skipped_and_operand_reparses() {
        // `z5` has no opcode — upstream logs a warning and moves on
        // ONE character at a time. The `5` then parses as its own
        // literal, so `42 z5` is two adds. This mirrors
        // `CCustomExplosionGenerator::ParseExplosionCode` exactly.
        assert_eq!(CegExpr::parse("42 z5").0, vec![Add(42.0), Add(5.0)]);
    }

    #[test]
    fn expr_other_opcodes_are_kept_as_written() {
        // `s3 p2` — sine + pow — parsed and stored for the consumer.
        assert_eq!(
            CegExpr::parse("10 s3 p2").0,
            vec![Add(10.0), Other('s', 3.0), Other('p', 2.0)]
        );
    }

    // CegVec3 -----------------------------------------------------------

    #[test]
    fn vec3_splits_by_comma() {
        let v = CegVec3::parse("-30 r60, 1.0, -30 r60");
        assert_eq!(v.x.0, vec![Add(-30.0), Rand(60.0)]);
        assert_eq!(v.y.0, vec![Add(1.0)]);
        assert_eq!(v.z.0, vec![Add(-30.0), Rand(60.0)]);
    }

    #[test]
    fn vec3_missing_axes_are_empty_programs() {
        let v = CegVec3::parse("5");
        assert_eq!(v.x.0, vec![Add(5.0)]);
        assert_eq!(v.y, CegExpr::default());
        assert_eq!(v.z, CegExpr::default());
    }

    // EmitVector --------------------------------------------------------

    #[test]
    fn emit_vector_dir_keyword_case_insensitive() {
        assert_eq!(EmitVector::parse("dir"), EmitVector::Direction);
        assert_eq!(EmitVector::parse("DIR"), EmitVector::Direction);
        assert_eq!(EmitVector::parse(" Dir "), EmitVector::Direction);
    }

    #[test]
    fn emit_vector_literal_axis() {
        match EmitVector::parse("0, 1, 0") {
            EmitVector::Literal(v) => {
                assert_eq!(v.x.0, vec![Add(0.0)]);
                assert_eq!(v.y.0, vec![Add(1.0)]);
                assert_eq!(v.z.0, vec![Add(0.0)]);
            }
            EmitVector::Direction => panic!("should not be direction"),
        }
    }

    // ColorMap ----------------------------------------------------------

    #[test]
    fn colormap_parses_oldskool_gradient() {
        let cm = ColorMap::parse("1 1 0 .3   1 0 0 .2   0 0 0 .8   0 0 0 0");
        assert_eq!(cm.stops.len(), 4);
        rgba_close(cm.stops[0], [1.0, 1.0, 0.0, 0.3]);
        rgba_close(cm.stops[3], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn colormap_partial_quad_is_dropped() {
        // Three trailing floats can't form a stop — drop them silently.
        let cm = ColorMap::parse("1 1 1 1  0 0 0");
        assert_eq!(cm.stops.len(), 1);
    }

    // GroundFlash -------------------------------------------------------

    #[test]
    fn groundflash_color_comma_triplet() {
        assert_eq!(parse_vec3_plain("1,0.6,0.6"), [1.0, 0.6, 0.6]);
        assert_eq!(parse_vec3_plain("1 1 1"), [1.0, 1.0, 1.0]);
    }

    // End-to-end: full CEG section --------------------------------------

    #[test]
    fn oldskool_shot1_roundtrips() {
        let src = r#"
[oldskool_shot1]
{
    [circle]
    {
        class=CSimpleParticleSystem;
        [properties]
        {
            sizegrowth=0.1;
            sizemod=1;
            pos=0, 1.0, 0;
            emitVector=dir;
            gravity=0, 0, 0;
            Texture=circle;
            colorMap=1 1 1 .1   .2 .2 1 .1   0 0 0 0;
            airdrag=1;
            particleLife=12;
            particleLifeSpread=2;
            numParticles=1;
            particleSpeed=.3;
            particleSpeedSpread=.2;
            particleSize=8;
            particleSizeSpread=2;
            emitRot=0;
            emitRotSpread=0;
            directional=0;
        }
        air=1;
        ground=1;
        water=1;
    }
    [groundflash]
    {
        flashSize=16;
        color=1,0.6,0.6;
    }
}
"#;
        let tdf = crate::Tdf::parse(src).unwrap();
        let defs = ExplosionDefs::from_tdf(&tdf);
        let def = defs.get("oldskool_shot1").expect("named CEG");
        assert_eq!(def.effects.len(), 1);
        let eff = &def.effects[0];
        assert_eq!(eff.name, "circle");
        assert_eq!(eff.class, EffectClass::SimpleParticleSystem);
        assert_eq!(eff.count, 1);
        let EffectProperties::Particle(p) = &eff.properties else {
            panic!("expected Particle");
        };
        assert_eq!(p.texture, "circle");
        assert_eq!(p.color_map.stops.len(), 3);
        rgba_close(p.color_map.stops[0], [1.0, 1.0, 1.0, 0.1]);
        rgba_close(p.color_map.stops[1], [0.2, 0.2, 1.0, 0.1]);
        assert_eq!(p.emit_vector, EmitVector::Direction);
        assert_eq!(p.particle_life.0, vec![Add(12.0)]);
        assert_eq!(p.airdrag.0, vec![Add(1.0)]);
        assert!(!p.directional);

        let gf = def.ground_flash.as_ref().expect("groundflash present");
        assert_eq!(gf.flash_size, 16.0);
        assert_eq!(gf.color, [1.0, 0.6, 0.6]);
    }

    #[test]
    fn system_nx_chain_is_preserved() {
        // `system_nx` has a CExpGenSpawner that recursively fires
        // `system_nx_fire` 240 times at interval 8 frames. Verify every
        // moving part parses: count, interval expression, nested CEG ref.
        let src = r#"
[system_nx]
{
    [fire]
    {
        class=CExpGenSpawner;
        [properties]
        {
            delay=8 i8;
            dir=0,1,0;
            damage=0 i1;
            explosionGenerator=custom:system_nx_fire;
            speed=0,0,0;
            pos=0,0,0;
        }
        air=1;
        ground=1;
        water=1;
        count=240;
    }
}
"#;
        let tdf = crate::Tdf::parse(src).unwrap();
        let defs = ExplosionDefs::from_tdf(&tdf);
        let def = defs.get("system_nx").expect("named CEG");
        let eff = &def.effects[0];
        assert_eq!(eff.class, EffectClass::ExpGenSpawner);
        assert_eq!(eff.count, 240);
        let EffectProperties::Spawner(sp) = &eff.properties else {
            panic!("expected Spawner");
        };
        assert_eq!(sp.explosion_generator, "system_nx_fire");
        // `delay=8 i8` → spawn N uses 8 + N*8 frames of delay.
        assert_eq!(sp.delay.0, vec![Add(8.0), Index(8.0)]);
    }

    #[test]
    fn system_sigterm_spawner_damage_with_i_token() {
        let src = r#"
[system_sigterm]
{
    [risingfire]
    {
        class=CExpGenSpawner;
        [properties]
        {
            delay=2 i3;
            dir=0,1,0;
            damage=0 i1;
            explosionGenerator=custom:system_sigterm_fire;
            speed=0,0,0;
            pos=0,0 i23,0;
            alwaysVisible=1;
        }
        count=20;
    }
}
"#;
        let tdf = crate::Tdf::parse(src).unwrap();
        let defs = ExplosionDefs::from_tdf(&tdf);
        let def = defs.get("system_sigterm").unwrap();
        let EffectProperties::Spawner(sp) = &def.effects[0].properties else {
            panic!();
        };
        // `pos=0, 0 i23, 0` means y steps by 23 per spawn.
        assert_eq!(sp.pos.x.0, vec![Add(0.0)]);
        assert_eq!(sp.pos.y.0, vec![Add(0.0), Index(23.0)]);
        assert_eq!(sp.pos.z.0, vec![Add(0.0)]);
        assert!(sp.always_visible);
        assert_eq!(def.effects[0].count, 20);
    }

    #[test]
    fn bitmap_muzzle_flame_parses_shockwave() {
        let src = r#"
[test]
{
    [shockwave]
    {
        class=CBitmapMuzzleFlame;
        [properties]
        {
            dir=0,1,0.00001;
            pos=0, 10, 0;
            colorMap=.75 .5 .3 .1   .4 .3 .1 .1   0 0 0 0;
            size=1;
            length=1;
            sizeGrowth=120;
            ttl=6;
            frontOffset=0;
            sideTexture=none;
            frontTexture=shockwave;
        }
        air=1;
        ground=1;
    }
}
"#;
        let tdf = crate::Tdf::parse(src).unwrap();
        let defs = ExplosionDefs::from_tdf(&tdf);
        let def = defs.get("test").unwrap();
        let EffectProperties::Flame(f) = &def.effects[0].properties else {
            panic!();
        };
        assert_eq!(f.front_texture, "shockwave");
        assert_eq!(f.side_texture, "none");
        assert_eq!(f.size_growth.0, vec![Add(120.0)]);
        assert_eq!(f.ttl.0, vec![Add(6.0)]);
        assert_eq!(f.color_map.stops.len(), 3);
    }
}

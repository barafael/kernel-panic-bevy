//! Typed explosion / CEG (Custom Explosion Generator) definitions from TDF files.
//!
//! Spring's explosion TDF files define visual effects triggered on weapon impact.
//! Each file contains one or more named explosion generators, each composed of
//! multiple effect layers (particle systems, bitmap flames, ground flashes, and
//! spawners that chain to other explosions).
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

use std::collections::BTreeMap;

use crate::Section;

/// All explosion definitions from one or more TDF files.
#[derive(Debug, Clone, Default)]
pub struct ExplosionDefs {
    pub explosions: BTreeMap<String, ExplosionDef>,
}

/// A single named explosion generator, composed of effect layers.
#[derive(Debug, Clone, Default)]
pub struct ExplosionDef {
    /// The explosion name (e.g. "corruption_burst", "oldskool_shot1").
    pub id: String,
    /// Visual effect layers (particle systems, flames, spawners).
    pub effects: Vec<EffectLayer>,
    /// Optional ground flash.
    pub ground_flash: Option<GroundFlash>,
}

/// One visual effect layer within an explosion.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectClass {
    /// Burst of particles with physics (gravity, drag, lifetime).
    SimpleParticleSystem,
    /// Textured beam / shockwave / flame billboard.
    BitmapMuzzleFlame,
    /// Spawns another explosion generator (chaining).
    ExpGenSpawner,
    /// Unknown class string (preserved for forward compatibility).
    Other(String),
}

/// Properties for a `CSimpleParticleSystem` effect.
#[derive(Debug, Clone, Default)]
pub struct ParticleProperties {
    pub texture: String,
    pub color_map: String,
    pub num_particles: f32,
    pub particle_life: f32,
    pub particle_life_spread: f32,
    pub particle_speed: f32,
    pub particle_speed_spread: f32,
    pub particle_size: f32,
    pub particle_size_spread: f32,
    pub size_growth: f32,
    pub size_mod: f32,
    pub emit_rot: f32,
    pub emit_rot_spread: f32,
    pub airdrag: f32,
    pub gravity: [f32; 3],
    pub pos: String,
    pub emit_vector: String,
    pub directional: bool,
    pub always_visible: bool,
}

/// Properties for a `CBitmapMuzzleFlame` effect.
#[derive(Debug, Clone, Default)]
pub struct FlameProperties {
    pub side_texture: String,
    pub front_texture: String,
    pub color_map: String,
    pub size: f32,
    pub length: f32,
    pub size_growth: f32,
    pub ttl: f32,
    pub front_offset: f32,
    pub speed: f32,
    pub dir: String,
    pub pos: String,
}

/// Properties for a `CExpGenSpawner` (chains to another explosion).
#[derive(Debug, Clone, Default)]
pub struct SpawnerProperties {
    pub explosion_generator: String,
    pub delay: String,
    pub damage: String,
    pub dir: String,
    pub pos: String,
    pub speed: String,
}

/// Union of effect-specific properties.
#[derive(Debug, Clone)]
pub enum EffectProperties {
    Particle(ParticleProperties),
    Flame(FlameProperties),
    Spawner(SpawnerProperties),
}

/// Ground flash effect (simple circle/ring on the terrain).
#[derive(Debug, Clone, Default)]
pub struct GroundFlash {
    pub flash_size: f32,
    pub flash_alpha: f32,
    pub circle_growth: f32,
    pub circle_alpha: f32,
    pub ttl: f32,
    pub color: [f32; 3],
}

// ── Parsing ────────────────────────────────────────────────────────

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
    pub fn get(&self, name: &str) -> Option<&ExplosionDef> {
        self.explosions.get(name)
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
            } else {
                if let Some(effect) = EffectLayer::from_section(child) {
                    effects.push(effect);
                }
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
        let class_str = s.string("class");
        let class = if class_str.is_empty() {
            // Some subsections (like groundflash handled above) have no class.
            return None;
        } else {
            EffectClass::parse(&class_str)
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
            EffectClass::Other(_) => {
                EffectProperties::Particle(ParticleProperties::from_section(props))
            }
        };

        Some(Self {
            name: s.name.clone(),
            class,
            air: s.bool("air"),
            ground: s.bool("ground"),
            water: s.bool("water"),
            count: {
                let c = s.f32("count") as u32;
                if c == 0 { 1 } else { c }
            },
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
            other => Self::Other(other.to_string()),
        }
    }
}

/// Parse a comma-or-space separated 3-component vector, defaulting to [0,0,0].
fn parse_vec3(s: &str) -> [f32; 3] {
    let parts: Vec<f32> = s
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

impl ParticleProperties {
    fn from_section(s: Option<&Section>) -> Self {
        let Some(s) = s else {
            return Self::default();
        };
        Self {
            texture: s.string("texture"),
            color_map: s.string("colormap"),
            num_particles: s.f32("numparticles"),
            particle_life: s.f32("particlelife"),
            particle_life_spread: s.f32("particlelifespread"),
            particle_speed: s.f32("particlespeed"),
            particle_speed_spread: s.f32("particlespeedspread"),
            particle_size: s.f32("particlesize"),
            particle_size_spread: s.f32("particlesizespread"),
            size_growth: s.f32("sizegrowth"),
            size_mod: {
                let v = s.f32("sizemod");
                if v == 0.0 { 1.0 } else { v }
            },
            emit_rot: s.f32("emitrot"),
            emit_rot_spread: s.f32("emitrotspread"),
            airdrag: {
                let v = s.f32("airdrag");
                if v == 0.0 { 1.0 } else { v }
            },
            gravity: parse_vec3(&s.string("gravity")),
            pos: s.string("pos"),
            emit_vector: s.string("emitvector"),
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
            color_map: s.string("colormap"),
            size: s.f32("size"),
            length: s.f32("length"),
            size_growth: s.f32("sizegrowth"),
            ttl: s.f32("ttl"),
            front_offset: s.f32("frontoffset"),
            speed: s.f32("speed"),
            dir: s.string("dir"),
            pos: s.string("pos"),
        }
    }
}

impl SpawnerProperties {
    fn from_section(s: Option<&Section>) -> Self {
        let Some(s) = s else {
            return Self::default();
        };
        Self {
            explosion_generator: s.string_clean("explosiongenerator"),
            delay: s.string("delay"),
            damage: s.string("damage"),
            dir: s.string("dir"),
            pos: s.string("pos"),
            speed: s.string("speed"),
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
            color: parse_vec3(&s.string("color")),
        }
    }
}

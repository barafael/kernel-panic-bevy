//! `touhou.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/touhou.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::CegOp::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[touhou_bubbletrail]` — 1 effect layer, no ground flash.
/// Referenced by 1 weapon.
pub static TOUHOU_BUBBLETRAIL: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "bubblecloud",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Bubbles),
            color_map: ColorMap(&[[0.3, 1.0, 0.8, 1.0], [0.3, 0.6, 1.0, 0.0]]),
            num_particles: Lit(1.0),
            particle_life: Lit(8.0),
            particle_life_spread: Lit(0.0),
            particle_speed: Lit(0.0),
            particle_speed_spread: Lit(0.0),
            particle_size: Lit(10.0),
            particle_size_spread: Lit(0.0),
            size_growth: Lit(0.0),
            size_mod: Lit(0.8),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(0.0),
            airdrag: Lit(1.0),
            gravity: ZERO,
            pos: ZERO,
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: false,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

/// `[touhou_buildFX]` — 1 effect layer, no ground flash.
/// Referenced by 3 `[SFXTypes]` entries.
pub static TOUHOU_BUILDFX: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "circle",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Sparkle),
            color_map: ColorMap(&[[1.0, 0.9, 0.5, 0.1], [0.0, 0.0, 0.0, 0.0]]),
            num_particles: Lit(1.0),
            particle_life: Lit(32.0),
            particle_life_spread: Lit(6.0),
            particle_speed: Lit(3.0),
            particle_speed_spread: Lit(1.0),
            particle_size: Lit(6.0),
            particle_size_spread: Lit(2.0),
            size_growth: Lit(0.0),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(0.0),
            airdrag: Lit(1.0),
            gravity: ZERO,
            pos: CegVec3 {
                x: Ops(&[Damage(1.0), Yank(0), Add(-1.0), Rand(2.0), MulBuf(0)]),
                y: Lit(0.0),
                z: Ops(&[Add(-1.0), Rand(2.0), MulBuf(0)]),
            },
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: true,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

/// `[touhou_bullet]` — 1 effect layer, no ground flash.
/// Referenced by 2 weapons.
pub static TOUHOU_BULLET: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "circle",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Bullet),
            color_map: ColorMap(&[[1.0, 1.0, 1.0, 0.1], [0.0, 0.0, 0.0, 0.0]]),
            num_particles: Lit(1.0),
            particle_life: Lit(10.0),
            particle_life_spread: Lit(6.0),
            particle_speed: Lit(2.0),
            particle_speed_spread: Lit(1.0),
            particle_size: Lit(9.0),
            particle_size_spread: Lit(5.0),
            size_growth: Lit(-0.5),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(5.0),
            airdrag: Lit(0.8),
            gravity: ZERO,
            pos: ZERO,
            emit_vector: EmitVector::Direction,
            directional: true,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

/// `[touhou_incant]` — 1 effect layer, no ground flash.
/// Referenced by 1 `[SFXTypes]` entry.
pub static TOUHOU_INCANT: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "sevenlobed",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Flame(FlameProperties {
            side_texture: None,
            front_texture: Some(CegTexture::LobedIncantation),
            color_map: ColorMap(&[
                [0.0, 1.0, 0.0, 1.0],
                [0.5, 1.0, 0.5, 1.0],
                [0.0, 0.0, 1.0, 0.0],
            ]),
            size: Lit(30.0),
            length: Lit(1.0),
            size_growth: Lit(1.0),
            ttl: Lit(40.0),
            front_offset: Lit(0.0),
            speed: Lit(0.0),
            dir: vec3(0.0, 1.0, 1e-5),
            pos: vec3(0.0, 10.0, 0.0),
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

/// `[touhou_spark]` — 1 effect layer, no ground flash.
/// Referenced by 2 weapons.
pub static TOUHOU_SPARK: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "circle",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Circle),
            color_map: ColorMap(&[
                [0.5, 1.0, 0.5, 0.3],
                [0.0, 1.0, 0.0, 0.2],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(7.0),
            particle_life: Lit(20.0),
            particle_life_spread: Lit(12.0),
            particle_speed: Lit(1.0),
            particle_speed_spread: Lit(3.0),
            particle_size: Lit(20.0),
            particle_size_spread: Lit(1.0),
            size_growth: Lit(-0.5),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(90.0),
            airdrag: Lit(1.0),
            gravity: ZERO,
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: false,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

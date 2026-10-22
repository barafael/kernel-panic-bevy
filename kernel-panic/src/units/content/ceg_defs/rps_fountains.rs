//! `rps_fountains.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/rps_fountains.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[paper_fountain]`.
pub static PAPER_FOUNTAIN: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "paper_drops",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::MiniPaper),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 0.0],
            ]),
            num_particles: Lit(24.0),
            particle_life: Lit(96.0),
            particle_life_spread: Lit(24.0),
            particle_speed: Lit(0.4),
            particle_speed_spread: Lit(0.3),
            particle_size: Lit(5.0),
            particle_size_spread: Lit(2.0),
            size_growth: Lit(0.05),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(5.0),
            airdrag: Lit(1.0),
            gravity: vec3(0.0, -0.02, 0.0),
            pos: ZERO,
            emit_vector: EmitVector::Direction,
            directional: true,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

/// `[rock_fountain]`.
pub static ROCK_FOUNTAIN: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "rock_drops",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::MiniRock),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 0.0],
            ]),
            num_particles: Lit(24.0),
            particle_life: Lit(96.0),
            particle_life_spread: Lit(24.0),
            particle_speed: Lit(0.4),
            particle_speed_spread: Lit(0.3),
            particle_size: Lit(5.0),
            particle_size_spread: Lit(2.0),
            size_growth: Lit(0.05),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(5.0),
            airdrag: Lit(1.0),
            gravity: vec3(0.0, -0.02, 0.0),
            pos: ZERO,
            emit_vector: EmitVector::Direction,
            directional: true,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

/// `[scissors_fountain]`.
pub static SCISSORS_FOUNTAIN: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "scissors_drops",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::MiniScissors),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 0.0],
            ]),
            num_particles: Lit(24.0),
            particle_life: Lit(96.0),
            particle_life_spread: Lit(24.0),
            particle_speed: Lit(0.4),
            particle_speed_spread: Lit(0.3),
            particle_size: Lit(5.0),
            particle_size_spread: Lit(2.0),
            size_growth: Lit(0.05),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(5.0),
            airdrag: Lit(1.0),
            gravity: vec3(0.0, -0.02, 0.0),
            pos: ZERO,
            emit_vector: EmitVector::Direction,
            directional: true,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

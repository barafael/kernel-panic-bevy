//! `network_dissolve.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/network_dissolve.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[network_dissolve]`.
pub static NETWORK_DISSOLVE: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "squarecloud",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Square),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(15.0),
            particle_life: Lit(16.0),
            particle_life_spread: Lit(16.0),
            particle_speed: Lit(2.0),
            particle_speed_spread: Lit(20.0),
            particle_size: Lit(6.0),
            particle_size_spread: Lit(6.0),
            size_growth: Lit(0.0),
            size_mod: Lit(0.9),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(0.0),
            airdrag: Lit(0.9),
            gravity: ZERO,
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: false,
            always_visible: false,
        }),
    }],
    ground_flash: Some(GroundFlash {
        flash_size: 24.0,
        flash_alpha: 1.0,
        circle_growth: 0.0,
        circle_alpha: 0.0,
        ttl: 16.0,
        color: [1.0, 1.0, 1.0],
    }),
};

/// `[network_spawn]`.
pub static NETWORK_SPAWN: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "squarecloud",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Square),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(3.0),
            particle_life: Lit(16.0),
            particle_life_spread: Lit(16.0),
            particle_speed: Lit(1.0),
            particle_speed_spread: Lit(2.0),
            particle_size: Lit(6.0),
            particle_size_spread: Lit(6.0),
            size_growth: Lit(0.0),
            size_mod: Lit(0.9),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(0.0),
            airdrag: Lit(0.9),
            gravity: ZERO,
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: false,
            always_visible: false,
        }),
    }],
    ground_flash: Some(GroundFlash {
        flash_size: 24.0,
        flash_alpha: 1.0,
        circle_growth: 0.0,
        circle_alpha: 0.0,
        ttl: 16.0,
        color: [1.0, 1.0, 1.0],
    }),
};

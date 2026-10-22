//! `experiment_trails.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/experiment_trails.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[experiment_trailM]` — 1 effect layer, no ground flash.
/// Referenced by 1 weapon.
pub static EXPERIMENT_TRAILM: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "squarecloud",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Square),
            color_map: ColorMap(&[[0.8, 1.0, 0.8, 0.1], [0.2, 1.0, 0.2, 0.1]]),
            num_particles: Lit(1.0),
            particle_life: Lit(8.0),
            particle_life_spread: Lit(0.0),
            particle_speed: Lit(0.0),
            particle_speed_spread: Lit(0.0),
            particle_size: Lit(5.0),
            particle_size_spread: Lit(0.0),
            size_growth: Lit(-0.5),
            size_mod: Lit(1.0),
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

/// `[experiment_trailS]` — 1 effect layer, no ground flash.
/// Referenced by 1 weapon.
pub static EXPERIMENT_TRAILS: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "squarecloud",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Square),
            color_map: ColorMap(&[[0.8, 0.8, 1.0, 0.1], [0.2, 0.2, 1.0, 0.1]]),
            num_particles: Lit(1.0),
            particle_life: Lit(8.0),
            particle_life_spread: Lit(0.0),
            particle_speed: Lit(0.0),
            particle_speed_spread: Lit(0.0),
            particle_size: Lit(5.0),
            particle_size_spread: Lit(0.0),
            size_growth: Lit(-0.5),
            size_mod: Lit(1.0),
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

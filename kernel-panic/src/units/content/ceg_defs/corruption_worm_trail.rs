//! `corruption_worm_trail.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/corruption_worm_trail.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[corruption_worm_trail]`.
pub static CORRUPTION_WORM_TRAIL: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "dirt",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Square),
            color_map: ColorMap(&[
                [0.8, 0.8, 0.0, 1.0],
                [0.6, 0.6, 0.0, 0.8],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(2.0),
            particle_life: Lit(24.0),
            particle_life_spread: Lit(8.0),
            particle_speed: Lit(0.8),
            particle_speed_spread: Lit(1.9),
            particle_size: Lit(4.0),
            particle_size_spread: Lit(2.0),
            size_growth: Lit(1.0),
            size_mod: Lit(0.93),
            emit_rot: Lit(86.0),
            emit_rot_spread: Lit(10.0),
            airdrag: Lit(0.98),
            gravity: ZERO,
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: false,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

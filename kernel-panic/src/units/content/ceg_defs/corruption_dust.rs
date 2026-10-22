//! `corruption_dust.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/corruption_dust.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[corruption_dust]`.
pub static CORRUPTION_DUST: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "dust",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::HLine),
            color_map: ColorMap(&[
                [0.8, 0.8, 0.0, 1.0],
                [0.4, 0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0, 1.0],
            ]),
            num_particles: Lit(4.0),
            particle_life: Lit(12.0),
            particle_life_spread: Lit(8.0),
            particle_speed: Lit(6.0),
            particle_speed_spread: Lit(3.0),
            particle_size: Lit(5.0),
            particle_size_spread: Lit(1.0),
            size_growth: Lit(0.0),
            size_mod: Lit(1.0),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(30.0),
            airdrag: Lit(0.98),
            gravity: vec3(0.0, -0.5, 0.0),
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Direction,
            directional: true,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

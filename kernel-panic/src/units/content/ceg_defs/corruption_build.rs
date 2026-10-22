//! `corruption_build.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/corruption_build.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[corruption_build]` — 1 effect layer, no ground flash.
/// Referenced by 3 `[SFXTypes]` entries.
pub static CORRUPTION_BUILD: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "fog",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::Circle),
            color_map: ColorMap(&[
                [0.4, 0.0, 0.0, 1.0],
                [0.3, 0.0, 0.0, 0.8],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(1.0),
            particle_life: Lit(24.0),
            particle_life_spread: Lit(8.0),
            particle_speed: Lit(0.5),
            particle_speed_spread: Lit(1.5),
            particle_size: Lit(8.0),
            particle_size_spread: Lit(2.0),
            size_growth: Lit(0.0),
            size_mod: Lit(1.0),
            emit_rot: Lit(55.0),
            emit_rot_spread: Lit(10.0),
            airdrag: Lit(0.98),
            gravity: vec3(0.0, -0.05, 0.0),
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: false,
            always_visible: false,
        }),
    }],
    ground_flash: None,
};

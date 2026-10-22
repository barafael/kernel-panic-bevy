//! `mine.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/mine.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[mine]`.
pub static MINE: ExplosionDef = ExplosionDef {
    effects: &[EffectLayer {
        name: "squarecloud",
        air: true,
        ground: true,
        water: true,
        count: 1,
        properties: Particle(ParticleProperties {
            texture: Some(CegTexture::SquareHollow),
            color_map: ColorMap(&[
                [1.0, 1.0, 1.0, 1.0],
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 0.0, 0.0, 0.0],
            ]),
            num_particles: Lit(48.0),
            particle_life: Lit(25.0),
            particle_life_spread: Lit(25.0),
            particle_speed: Lit(2.0),
            particle_speed_spread: Lit(20.0),
            particle_size: Lit(8.0),
            particle_size_spread: Lit(4.0),
            size_growth: Lit(0.0),
            size_mod: Lit(0.98),
            emit_rot: Lit(0.0),
            emit_rot_spread: Lit(80.0),
            airdrag: Lit(0.8),
            gravity: ZERO,
            pos: vec3(0.0, 1.0, 0.0),
            emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
            directional: true,
            always_visible: false,
        }),
    }],
    ground_flash: Some(GroundFlash {
        flash_size: 64.0,
        flash_alpha: 1.0,
        circle_growth: 12.8,
        circle_alpha: 1.0,
        ttl: 16.0,
        color: [1.0, 1.0, 1.0],
    }),
};

//! `oldskool_vbig.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/oldskool_vbig.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[oldskool_vbig]` — 2 effect layers, ground flash live.
/// Referenced by 1 weapon.
pub static OLDSKOOL_VBIG: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
            name: "squarecloud",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::Square),
                color_map: ColorMap(&[
                    [1.0, 1.0, 0.0, 0.3],
                    [1.0, 0.0, 0.0, 0.2],
                    [0.0, 0.0, 0.0, 0.8],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(36.0),
                particle_life: Lit(36.0),
                particle_life_spread: Lit(48.0),
                particle_speed: Lit(2.0),
                particle_speed_spread: Lit(30.0),
                particle_size: Lit(14.0),
                particle_size_spread: Lit(10.0),
                size_growth: Lit(0.0),
                size_mod: Lit(0.98),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(80.0),
                airdrag: Lit(0.8),
                gravity: vec3(0.1, 0.0, 0.0),
                pos: vec3(0.0, 1.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: false,
                always_visible: false,
            }),
        },
        EffectLayer {
            name: "tracers",
            air: true,
            ground: true,
            water: true,
            count: 1,
            properties: Particle(ParticleProperties {
                texture: Some(CegTexture::HLine),
                color_map: ColorMap(&[
                    [1.0, 1.0, 1.0, 1.0],
                    [1.0, 1.0, 1.0, 1.0],
                    [0.0, 0.0, 0.0, 0.0],
                ]),
                num_particles: Lit(15.0),
                particle_life: Lit(5.0),
                particle_life_spread: Lit(2.0),
                particle_speed: Lit(30.0),
                particle_speed_spread: Lit(20.0),
                particle_size: Lit(10.0),
                particle_size_spread: Lit(0.0),
                size_growth: Lit(0.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(80.0),
                airdrag: Lit(0.7),
                gravity: ZERO,
                pos: vec3(0.0, 1.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: true,
                always_visible: false,
            }),
        },
    ],
    ground_flash: Some(GroundFlash {
        flash_size: 96.0,
        flash_alpha: 1.0,
        circle_growth: 6.4,
        circle_alpha: 1.0,
        ttl: 64.0,
        color: [1.0, 1.0, 1.0],
    }),
};

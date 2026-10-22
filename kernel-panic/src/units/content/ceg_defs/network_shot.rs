//! `network_shot.tdf` — the CEGs upstream ships in this file, as Rust.
//!
//! Generated from `assets/defs/explosions/network_shot.tdf` by the ignored
//! `gen_ceg_defs` test in `super::ceg_def`, and checked against it by
//! `cegs::tests::every_source_def_matches_the_parsed_tdf`. Regenerate;
//! a hand-edit that changes a value fails that test.

use crate::units::content::ceg_def::CegExpr::*;
use crate::units::content::ceg_def::EffectProperties::*;
use crate::units::content::ceg_def::*;

/// `[network_shot]` — 2 effect layers, no ground flash.
/// Referenced by 1 weapon.
pub static NETWORK_SHOT: ExplosionDef = ExplosionDef {
    effects: &[
        EffectLayer {
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
                num_particles: Lit(1.0),
                particle_life: Lit(10.0),
                particle_life_spread: Lit(6.0),
                particle_speed: Lit(0.0),
                particle_speed_spread: Lit(0.0),
                particle_size: Lit(2.0),
                particle_size_spread: Lit(5.0),
                size_growth: Lit(0.5),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(15.0),
                airdrag: Lit(0.9),
                gravity: ZERO,
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
                num_particles: Lit(8.0),
                particle_life: Lit(8.0),
                particle_life_spread: Lit(2.0),
                particle_speed: Lit(4.0),
                particle_speed_spread: Lit(4.0),
                particle_size: Lit(4.0),
                particle_size_spread: Lit(0.0),
                size_growth: Lit(0.0),
                size_mod: Lit(1.0),
                emit_rot: Lit(0.0),
                emit_rot_spread: Lit(100.0),
                airdrag: Lit(0.9),
                gravity: ZERO,
                pos: vec3(0.0, 1.0, 0.0),
                emit_vector: EmitVector::Literal(vec3(0.0, 1.0, 0.0)),
                directional: true,
                always_visible: false,
            }),
        },
    ],
    ground_flash: None,
};
